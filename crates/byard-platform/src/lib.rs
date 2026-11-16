//! # byard-platform
//!
//! `winit`-backed implementation of [`byard_core::PlatformHost`] (RFC-0001 §6).
//!
//! This is the only crate in the workspace allowed to depend on `winit`.
//! [`WinitHost`] owns the actual window and event loop; it translates
//! `winit`'s `WindowEvent`s into [`byard_core::PlatformHost`] calls and never
//! leaks a `winit` type through its own public API (`WinitHost::run` takes a
//! generic `PlatformHost` and returns a plain `Result<(), ByardError>`), so
//! downstream crates can depend on `byard-platform` without ever importing
//! `winit` themselves.
//!
//! # Accessibility (RFC-0042)
//!
//! The window also hands the platform's screen-reader API the tree the engine
//! builds for it, through AccessKit, when the `a11y` feature is on (it is off
//! by default). With it on, it costs nothing while no assistive technology is
//! connected. See `access`.

mod clipboard;
mod windows;

pub use clipboard::SystemClipboard;

use std::sync::Arc;

use byard_core::relay::FrameWaker;
use byard_core::{
    ByardError, KeyModifiers, PlatformHost, PointerButton, PointerState, SurfaceId, WindowSize,
};
use std::sync::Mutex;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// The control flow a poll-mode loop should adopt for the next iteration:
/// `Poll` while the application has frame-driven work, `Wait` once it has none
/// (RFC-0010's active-animation set, RFC-0025 §2's power argument).
///
/// Split out as a pure function so the decision is unit-testable without a
/// window: it is the one line that decides whether a settled app costs 0 % or
/// 100 % of a core.
fn control_flow_for(wants_frames: bool) -> ControlFlow {
    if wants_frames {
        ControlFlow::Poll
    } else {
        ControlFlow::Wait
    }
}

#[cfg(feature = "a11y")]
mod access;

/// What the accessibility bridge is when the feature is off: nothing, behind
/// the same few calls, so the host reads the same either way.
#[cfg(not(feature = "a11y"))]
mod access {
    use byard_core::{PerSurface, PlatformHost};
    use winit::window::Window;

    /// One (empty) adapter per surface, so the host reads the same either way.
    pub(crate) type Bridges = PerSurface<Bridge>;

    /// A window needs no hiding when there is no adapter to build first.
    pub(crate) const NEEDS_HIDDEN_START: bool = false;

    /// Nothing to bridge.
    pub(crate) struct Bridge;

    // The same signatures as the real bridge, so the host calls it the same
    // way with the feature off; there is simply nothing for `self` to be.
    #[allow(clippy::unused_self)]
    impl Bridge {
        pub(crate) fn process_event(&mut self, _: &Window, _: &winit::event::WindowEvent) {}
        pub(crate) fn sync<H: PlatformHost>(&mut self, _: &H, _: &Window) {}
    }
}

/// User events posted to the winit loop.
#[derive(Debug)]
enum UserEvent {
    /// Posted by the logic thread's [`FrameWaker`] when it publishes a frame
    /// that changed in response to input. It carries no data; its only job is
    /// to wake a `Wait`-mode loop so it redraws the update.
    FramePublished,
    /// The screen-reader adapter has something to say (RFC-0042 §5): a reader
    /// connected or left, or asked a node to act.
    #[cfg(feature = "a11y")]
    Access(accesskit_winit::Event),
}

#[cfg(feature = "a11y")]
impl From<accesskit_winit::Event> for UserEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::Access(event)
    }
}

/// A `winit`-backed [`PlatformHost`] driver.
///
/// Construct with [`WinitHost::new`] and hand it a [`PlatformHost`]
/// implementation via [`WinitHost::run`], which blocks until the window is
/// closed (or a [`PlatformHost`] callback returns an error).
pub struct WinitHost {
    title: String,
    width: u32,
    height: u32,
    /// When `true`, uses `ControlFlow::Poll` so the event loop spins
    /// continuously and new `RenderFrame`s are presented without waiting
    /// for a user input event. Intended for `byard dev` where hot-reload
    /// must appear immediately after a file save.
    poll: bool,
    /// The window's icon, as `(rgba, width, height)`.
    icon: Option<(Vec<u8>, u32, u32)>,
    /// The id the desktop knows the window by: see [`WinitHost::with_app_id`].
    app_id: String,
}

impl WinitHost {
    /// Creates a host that will open a window with the given title and
    /// initial size (in logical pixels).
    pub fn new(title: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            title: title.into(),
            width,
            height,
            poll: false,
            icon: None,
            app_id: DEFAULT_APP_ID.to_string(),
        }
    }

    /// Names the app the window belongs to, the id a Linux desktop matches to
    /// the app's `.desktop` file, its icon and its window rules (Wayland
    /// `app_id`, X11 `WM_CLASS`). Defaults to `dev.byard.Byard`, the id of
    /// Byard's own tools. Other platforms have no such id and ignore it.
    #[must_use]
    pub fn with_app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = app_id.into();
        self
    }

    /// Gives the window an icon: tightly packed 8-bit RGBA, `width` by
    /// `height`. Shown in the title bar and the taskbar on Windows and Linux;
    /// macOS takes an app's icon from its bundle instead, and ignores this.
    #[must_use]
    pub fn with_icon(mut self, rgba: Vec<u8>, width: u32, height: u32) -> Self {
        self.icon = Some((rgba, width, height));
        self
    }

    /// Switches the event loop to `ControlFlow::Poll`, redraws are requested
    /// on every iteration instead of waiting for the next OS event.
    ///
    /// Use this for dev tools (e.g. `byard dev`) where file-change frames must
    /// appear without requiring a mouse event to unblock the event loop.
    #[must_use]
    pub fn with_poll(mut self) -> Self {
        self.poll = true;
        self
    }

    /// Runs the `winit` event loop until the window closes, dispatching
    /// every relevant OS event to `host`.
    ///
    /// This call blocks the calling thread for the lifetime of the window,
    /// per RFC-0001 §6, this is the render thread; it never shares mutable
    /// state with a logic thread, so blocking here is exactly what the
    /// concurrency model expects.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::Platform`] if the event loop or window itself
    /// fails to initialise, or whatever [`ByardError`] a [`PlatformHost`]
    /// callback returns (e.g. [`Engine::init`](byard_core::engine::Engine::init)
    /// failing inside [`PlatformHost::on_resume`]), `winit`'s
    /// `ApplicationHandler` callbacks are themselves infallible, so such
    /// errors are captured internally and surfaced here once the loop exits.
    pub fn run<H: PlatformHost>(self, host: H) -> Result<(), ByardError> {
        // A user-event loop so the logic thread can wake it via a proxy when it
        // publishes a changed frame (see `FramePublished` / the frame waker).
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .map_err(|e| ByardError::Platform(e.to_string()))?;
        // Poll mode for live-reload dev tools; Wait mode for shipped applications
        // (waits for OS events, no busy-loop, saves battery on static scenes).
        if self.poll {
            event_loop.set_control_flow(ControlFlow::Poll);
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }

        // The waker handed to the host's `Engine`: each call posts a
        // `FramePublished` to this loop, which redraws on receipt. The proxy is
        // wrapped in a `Mutex` so the closure is `Sync` regardless of whether
        // `EventLoopProxy` is, satisfying the `FrameWaker` bound; the lock is
        // only ever taken by the single logic thread, so it never contends.
        let access_proxy = event_loop.create_proxy();
        let proxy = Mutex::new(event_loop.create_proxy());
        let waker: FrameWaker = std::sync::Arc::new(move || {
            if let Ok(proxy) = proxy.lock() {
                let _ = proxy.send_event(UserEvent::FramePublished);
            }
        });

        let mut app = WinitApp {
            multi: host.is_multi_surface(),
            host,
            started: false,
            instance: None,
            slots: byard_core::PerSurface::new(),
            wins: windows::Windows::default(),
            redraws: Vec::new(),
            title: self.title,
            width: self.width,
            height: self.height,
            icon: self.icon,
            app_id: self.app_id,
            fatal: None,
            poll: self.poll,
            waker,
            modifiers: KeyModifiers::default(),
            access_proxy,
            access: access::Bridges::new(),
        };

        event_loop
            .run_app(&mut app)
            .map_err(|e| ByardError::Platform(e.to_string()))?;

        match app.fatal {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// One open window of the program: what the host keeps beside it.
struct Slot {
    window: Arc<Window>,
    /// The cursor's last position in this window, logical.
    cursor_pos: (f32, f32),
    /// The caret last handed to this window's IME, `None` while it is off
    /// (RFC-0040 §4). Kept so the window is only told when something changed.
    ime_caret: Option<byard_core::frame::Rect>,
}

impl Slot {
    fn new(window: Arc<Window>) -> Self {
        Self {
            window,
            cursor_pos: (0.0, 0.0),
            ime_caret: None,
        }
    }
}

/// The smallest or largest size a window may be resized to, from a bound per
/// axis: an axis with no bound is left open (`open`). `None` when neither
/// axis has one.
fn bound_size(bound: (Option<f32>, Option<f32>), open: f64) -> Option<LogicalSize<f64>> {
    if bound.0.is_none() && bound.1.is_none() {
        return None;
    }
    Some(LogicalSize::new(
        bound.0.map_or(open, f64::from),
        bound.1.map_or(open, f64::from),
    ))
}

/// What a window of the program is opened with, from its spec.
fn window_attributes(
    spec: &byard_core::SurfaceSpec,
    app_id: &str,
    icon: Option<winit::window::Icon>,
) -> winit::window::WindowAttributes {
    let mut attrs = Window::default_attributes()
        .with_title(&spec.title)
        .with_inner_size(LogicalSize::new(
            f64::from(spec.size.0),
            f64::from(spec.size.1),
        ))
        .with_resizable(spec.resizable)
        .with_decorations(spec.decorations)
        .with_transparent(spec.transparent)
        .with_window_icon(icon)
        .with_visible(!access::NEEDS_HIDDEN_START);
    if let Some(min) = bound_size(spec.min_size, 0.0) {
        attrs = attrs.with_min_inner_size(min);
    }
    if let Some(max) = bound_size(spec.max_size, 1.0e6) {
        attrs = attrs.with_max_inner_size(max);
    }
    with_app_id(attrs, app_id)
}

/// Adapts a [`PlatformHost`] to `winit`'s `ApplicationHandler`.
///
/// A program of one window keeps it in `slots` as [`SurfaceId::MAIN`] and
/// runs the code it always did. A program of several (RFC-0068 §2, §8) opens
/// no window at first: the logic thread is started, and a window is made for
/// each [`byard_core::SurfaceChange::Create`] it asks for, the first of which
/// is the engine's first surface.
struct WinitApp<H: PlatformHost> {
    host: H,
    /// Whether the program has several windows, asked of the host once.
    multi: bool,
    started: bool,
    /// Used to make the surface of each window after the first.
    instance: Option<wgpu::Instance>,
    /// The open windows, by surface.
    slots: byard_core::PerSurface<Slot>,
    /// What decides which windows are open (a program of several).
    wins: windows::Windows,
    /// The surfaces that asked for a redraw in this round of the event loop.
    redraws: Vec<SurfaceId>,
    title: String,
    width: u32,
    height: u32,
    /// The window's icon, if the app has one.
    icon: Option<(Vec<u8>, u32, u32)>,
    /// The id the desktop knows the window by.
    app_id: String,
    /// Set when a [`PlatformHost`] callback returns `Err`, since
    /// `ApplicationHandler`'s methods can't themselves return a `Result`.
    /// `WinitHost::run` checks this after the event loop exits and surfaces
    /// it to its own caller.
    fatal: Option<ByardError>,
    /// Mirror of [`WinitHost::poll`]: when `true`, `about_to_wait` requests
    /// a redraw on every event-loop iteration so hot-reload frames appear
    /// immediately without waiting for the next user input event.
    poll: bool,
    /// Frame waker installed on the host's `Engine` in `resumed`; fired by the
    /// logic thread to wake this loop when a changed frame is ready.
    waker: FrameWaker,
    /// Latest modifier state, tracked so a key event can be offered to the host
    /// as a chord before it reaches the router (RFC-0030 §V3).
    modifiers: KeyModifiers,
    /// The proxy the screen-reader adapter posts its events through.
    #[cfg_attr(
        not(feature = "a11y"),
        allow(dead_code, reason = "only the AccessKit bridge uses it")
    )]
    access_proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    /// The screen-reader adapter of each window, once it exists (RFC-0042).
    access: access::Bridges,
}

/// The host's windows as [`windows::Backend`]: the real thing.
struct Real<'a, H: PlatformHost> {
    app: &'a mut WinitApp<H>,
    event_loop: &'a ActiveEventLoop,
}

impl<H: PlatformHost> windows::Backend for Real<'_, H> {
    fn create(&mut self, id: SurfaceId, spec: &byard_core::SurfaceSpec) -> Result<(), String> {
        let app = &mut *self.app;
        let icon = app.icon_for_window();
        let window = self
            .event_loop
            .create_window(window_attributes(spec, &app.app_id, icon))
            .map(Arc::new)
            .map_err(|e| e.to_string())?;
        app.attach_access(self.event_loop, &window, id);
        let Some(instance) = app.instance.take() else {
            return Err("the program has no graphics instance".to_string());
        };
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let mut size = to_window_size(window.inner_size(), window.scale_factor());
        size.refresh_rate_mhz = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz());
        let made = app
            .host
            .on_surface_created(&instance, id, surface, size, spec.transparent)
            .map_err(|e| e.to_string());
        app.instance = Some(instance);
        made?;
        // The appearance the program opens in, from its first window.
        if app.slots.is_empty() {
            if let Some(theme) = window.theme() {
                app.host
                    .on_system_scheme(theme == winit::window::Theme::Dark);
            }
        }
        app.slots.insert(id, Slot::new(window));
        Ok(())
    }

    fn configure(&mut self, id: SurfaceId, spec: &byard_core::SurfaceSpec) {
        let Some(slot) = self.app.slots.get(id) else {
            return;
        };
        let w = &slot.window;
        w.set_title(&spec.title);
        w.set_min_inner_size(bound_size(spec.min_size, 0.0));
        w.set_max_inner_size(bound_size(spec.max_size, 1.0e6));
        w.set_resizable(spec.resizable);
        w.set_decorations(spec.decorations);
        w.set_transparent(spec.transparent);
    }

    fn destroy(&mut self, id: SurfaceId) {
        self.app.host.on_surface_destroyed(id);
        self.app.access.remove(id);
        self.app.slots.remove(id);
        self.app.redraws.retain(|s| *s != id);
    }

    fn request_redraw(&mut self, id: SurfaceId) {
        if let Some(slot) = self.app.slots.get(id) {
            slot.window.request_redraw();
        }
    }

    fn send_close(&mut self, id: SurfaceId) {
        self.app.host.on_surface_close(id);
    }

    fn exit(&mut self) {
        self.event_loop.exit();
    }
}

impl<H: PlatformHost> WinitApp<H> {
    /// Records a fatal error and asks the event loop to exit on its next
    /// iteration.
    fn fail(&mut self, event_loop: &ActiveEventLoop, err: ByardError) {
        self.fatal = Some(err);
        event_loop.exit();
    }

    /// The icon, made again for each window.
    fn icon_for_window(&self) -> Option<winit::window::Icon> {
        let (rgba, width, height) = self.icon.clone()?;
        winit::window::Icon::from_rgba(rgba, width, height)
            .map_err(|e| eprintln!("  the window icon was not used: {e}"))
            .ok()
    }

    #[cfg_attr(
        not(feature = "a11y"),
        allow(
            clippy::unused_self,
            reason = "there is no adapter to keep without AccessKit"
        )
    )]
    /// Creates the screen-reader adapter for `window`, then shows the window
    /// (RFC-0042 §5). With the feature off the window was never hidden and
    /// there is nothing to build.
    fn attach_access(&mut self, event_loop: &ActiveEventLoop, window: &Window, id: SurfaceId) {
        #[cfg(feature = "a11y")]
        {
            self.access.insert(
                id,
                access::Bridge::new(event_loop, window, self.access_proxy.clone(), id),
            );
            window.set_visible(true);
        }
        #[cfg(not(feature = "a11y"))]
        let _ = (event_loop, window, id);
    }

    /// Turns the IME of `surface`'s window on or off and moves its candidate
    /// window to follow the focused field's caret, as the frame just
    /// presented reports it (RFC-0040 §4). Read from the presented frame, so
    /// the rectangle is always that of what is on screen, and the logic
    /// thread never touches the window.
    fn sync_ime(&mut self, surface: SurfaceId) {
        let want = self.host.text_input(surface).map(|t| t.caret);
        let Some(slot) = self.slots.get_mut(surface) else {
            return;
        };
        if want == slot.ime_caret {
            return;
        }
        match (slot.ime_caret, want) {
            (None, Some(_)) => slot.window.set_ime_allowed(true),
            (Some(_), None) => slot.window.set_ime_allowed(false),
            _ => {}
        }
        if let Some(caret) = want {
            slot.window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(caret.x, caret.y),
                LogicalSize::new(caret.width, caret.height),
            );
        }
        slot.ime_caret = want;
    }

    /// The surface and window an event of `window_id` is about. The hidden
    /// anchor of a program of several windows is nobody's.
    fn locate(&self, window_id: WindowId) -> Option<(SurfaceId, Arc<Window>)> {
        self.slots
            .iter()
            .find(|(_, s)| s.window.id() == window_id)
            .map(|(id, s)| (id, s.window.clone()))
    }

    /// Runs `f` on the window bookkeeping with a [`Real`] backend; a window
    /// that cannot be made ends the program.
    fn with_windows<R>(
        &mut self,
        event_loop: &ActiveEventLoop,
        f: impl FnOnce(&mut windows::Windows, &mut Real<'_, H>) -> R,
    ) -> R {
        let mut wins = std::mem::take(&mut self.wins);
        let out = f(
            &mut wins,
            &mut Real {
                app: self,
                event_loop,
            },
        );
        self.wins = wins;
        out
    }

    /// Applies what the logic thread queued about surfaces, then asks for a
    /// redraw of the windows whose frame is new (a program of several).
    fn sync_surfaces(&mut self, event_loop: &ActiveEventLoop) {
        let changes = self.host.take_surface_changes();
        if !changes.is_empty() {
            if let Err(e) = self.with_windows(event_loop, |w, b| w.apply(&changes, b)) {
                self.fail(event_loop, ByardError::Platform(e));
                return;
            }
        }
        let versions: Vec<(SurfaceId, Option<u64>)> = self
            .wins
            .ids()
            .map(|id| (id, self.host.frame_version(id)))
            .collect();
        self.with_windows(event_loop, |w, b| {
            w.frames_published(
                |id| {
                    versions
                        .iter()
                        .find(|(s, _)| *s == id)
                        .and_then(|(_, v)| *v)
                },
                b,
            );
        });
    }

    /// Draws the surfaces that asked for it this round as one round, then
    /// tells each one's IME and screen reader what it now shows.
    fn flush_redraws(&mut self, event_loop: &ActiveEventLoop) {
        if self.redraws.is_empty() {
            return;
        }
        let ids = std::mem::take(&mut self.redraws);
        if let Err(e) = self.host.on_redraw_round(&ids) {
            self.fail(event_loop, e);
            return;
        }
        for id in ids {
            self.sync_ime(id);
            if let (Some(access), Some(slot)) = (self.access.get_mut(id), self.slots.get(id)) {
                access.sync(&self.host, &slot.window);
            }
        }
    }
}

impl<H: PlatformHost> ApplicationHandler<UserEvent> for WinitApp<H> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Already initialised, e.g. resumed again after a mobile suspend.
        if self.started {
            return;
        }
        self.started = true;

        // wgpu 29: InstanceDescriptor::default() removed.
        // `new_without_display_handle_from_env` is the direct equivalent:
        // reads the WGPU_BACKEND env var and works on Metal/Dx12/Vulkan.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        if self.multi {
            if let Err(e) = self.host.on_start(self.waker.clone()) {
                self.fail(event_loop, e);
            }
            self.instance = Some(instance);
            return;
        }

        let icon = self.icon_for_window();
        let attributes = with_app_id(
            Window::default_attributes()
                .with_title(&self.title)
                .with_inner_size(LogicalSize::new(self.width, self.height))
                .with_window_icon(icon)
                // AccessKit wants its adapter in place before the window is
                // first shown (RFC-0042 §5), so it opens hidden and is shown
                // as soon as the adapter exists.
                .with_visible(!access::NEEDS_HIDDEN_START),
            &self.app_id,
        );
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.fail(event_loop, ByardError::Platform(e.to_string()));
                return;
            }
        };
        self.attach_access(event_loop, &window, SurfaceId::MAIN);

        // create_surface requires the window to be Arc'd ('static bound).
        // No `unsafe` needed with wgpu 24+ + winit 0.30.
        let surface = match instance.create_surface(window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.fail(event_loop, ByardError::Platform(e.to_string()));
                return;
            }
        };

        let mut size = to_window_size(window.inner_size(), window.scale_factor());
        // The frame budget the dev runner charts against (RFC-0030 §Q3). Read
        // once here rather than per frame: it is a property of the display, and
        // a window dragged to a second monitor mid-session is a rarer event
        // than sixty redraws a second.
        size.refresh_rate_mhz = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz());

        if let Err(e) = self
            .host
            .on_resume(&instance, surface, size, self.waker.clone())
        {
            self.fail(event_loop, e);
            return;
        }

        // The appearance the window opens in, for an app whose theme
        // follows it. A platform that cannot say reports nothing.
        if let Some(theme) = window.theme() {
            self.host
                .on_system_scheme(theme == winit::window::Theme::Dark);
        }

        window.request_redraw();
        self.slots.insert(SurfaceId::MAIN, Slot::new(window));
    }

    #[allow(clippy::too_many_lines, reason = "one arm per window event")]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some((surface, window)) = self.locate(window_id) else {
            return;
        };
        let multi = self.multi;
        // A program of one window asks for a redraw after most events, as it
        // always did; a program of several draws what the logic thread
        // publishes, which `sync_surfaces` finds by version.
        let redraw = |w: &Window| {
            if !multi {
                w.request_redraw();
            }
        };
        if let Some(access) = self.access.get_mut(surface) {
            access.process_event(&window, &event);
        }

        match event {
            // on_close_requested() == false means "keep the window open";
            // there's nothing else to do, so it falls through to the `_`
            // arm below rather than getting its own empty branch here.
            WindowEvent::CloseRequested if multi => {
                self.with_windows(event_loop, |w, b| w.close_requested(surface, b));
            }
            WindowEvent::CloseRequested if self.host.on_close_requested(surface) => {
                event_loop.exit();
            }

            WindowEvent::ThemeChanged(theme) => {
                self.host
                    .on_system_scheme(theme == winit::window::Theme::Dark);
                redraw(&window);
            }

            WindowEvent::Resized(new_size) => {
                let size = to_window_size(new_size, window.scale_factor());
                self.host.on_resize(surface, size);
                window.request_redraw();
            }

            // Fired when the window moves between displays with different
            // DPI (e.g. Retina to a non-HiDPI external monitor). The new
            // physical size after the factor change is supplied by winit.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = to_window_size(window.inner_size(), scale_factor);
                self.host.on_resize(surface, size);
                window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                if multi {
                    // Drawn with the others of this round, in `about_to_wait`.
                    if !self.redraws.contains(&surface) {
                        self.redraws.push(surface);
                    }
                    return;
                }
                if let Err(e) = self.host.on_redraw(surface) {
                    self.fail(event_loop, e);
                    return;
                }
                self.sync_ime(surface);
                // RFC-0042: sends the screen reader the tree the frame just
                // presented carries, if it differs from the last one sent. A
                // window with no reader connected does nothing here.
                if let Some(access) = self.access.get_mut(surface) {
                    access.sync(&self.host, &window);
                }
            }

            WindowEvent::Ime(ime) => {
                self.host.on_ime(surface, to_ime_event(ime));
                redraw(&window);
            }

            WindowEvent::CursorMoved { position, .. } => {
                let scale_factor = window.scale_factor();
                let logical = position.to_logical::<f64>(scale_factor);
                #[allow(clippy::cast_possible_truncation)]
                let x = logical.x as f32;
                #[allow(clippy::cast_possible_truncation)]
                let y = logical.y as f32;
                if let Some(slot) = self.slots.get_mut(surface) {
                    slot.cursor_pos = (x, y);
                }
                self.host.on_cursor_moved(surface, x, y);
                redraw(&window);
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = self.slots.get(surface).map_or((0.0, 0.0), |s| s.cursor_pos);
                self.host.on_pointer_input(
                    surface,
                    to_pointer_button(button),
                    to_pointer_state(state),
                    x,
                    y,
                );
                redraw(&window);
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = self.slots.get(surface).map_or((0.0, 0.0), |s| s.cursor_pos);
                match to_scroll_origin(delta, window.scale_factor()) {
                    ScrollOrigin::Wheel(dx, dy) => self.host.on_wheel(surface, dx, dy, x, y),
                    ScrollOrigin::Scroll(dx, dy) => self.host.on_scroll(surface, dx, dy, x, y),
                }
                redraw(&window);
            }

            WindowEvent::ModifiersChanged(state) => {
                self.modifiers = to_key_modifiers(state.state());
                // The host stamps them onto the events it creates next
                // (RFC-0041 §2): a text field tells Left from Shift+Left.
                self.host.on_modifiers(self.modifiers);
            }

            // Whether anyone is typing into this window. The caret of a
            // focused field stops blinking when nobody is (RFC-0041 §6).
            WindowEvent::Focused(focused) => {
                self.wins.focus_changed(surface, focused);
                self.host.on_window_focus(surface, focused);
                redraw(&window);
            }

            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                let key_str = key_to_str(&event.logical_key);
                // The host gets first refusal with the modifier state. A dev
                // chord that reached the router would be indistinguishable from
                // the user typing, so a consumed chord stops here, no
                // `on_key`, no `on_text` (RFC-0030 §V3).
                let consumed = !key_str.is_empty()
                    && self
                        .host
                        .on_chord(surface, &key_str, pressed, self.modifiers);
                if consumed {
                    redraw(&window);
                    return;
                }
                if !key_str.is_empty() {
                    self.host.on_key(surface, &key_str, pressed);
                }
                // Typed text, on press. Read from `text` rather than the logical
                // key: it is what carries a dead-key combination on Windows.
                // Control characters (Enter's "\r", Backspace's "\u{8}") are
                // keys, not text, and already went to `on_key`. Text an IME
                // commits never arrives here, winit delivers it as
                // `Ime::Commit` instead (RFC-0040 §3).
                if pressed {
                    if let Some(text) = typed_text(&event) {
                        self.host.on_text(surface, text);
                    }
                }
                redraw(&window);
            }

            _ => {}
        }
    }

    /// The logic thread published a frame that changed in response to input.
    /// Request a redraw so a `Wait`-mode loop presents it immediately instead
    /// of showing the pre-input frame until the next unrelated OS event.
    ///
    /// Or the screen-reader adapter has something to say (RFC-0042 §5), which
    /// also ends in a redraw: a reader that just connected needs the tree
    /// sent, and one that asked a node to act needs the result shown.
    ///
    /// In a program of several windows this is also where the windows the
    /// logic thread opened, changed or closed are made to match, and where the
    /// windows with a new frame are found (RFC-0068 §5).
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::FramePublished => {}
            #[cfg(feature = "a11y")]
            UserEvent::Access(event) => {
                // The window id on the event names the surface.
                let surface = self
                    .locate(event.window_id)
                    .map_or(SurfaceId::MAIN, |(id, _)| id);
                access::handle_event(
                    &mut self.access,
                    surface,
                    &mut self.host,
                    event.window_event,
                );
                if self.multi {
                    if let Some(slot) = self.slots.get(surface) {
                        slot.window.request_redraw();
                    }
                    return;
                }
            }
        }
        if self.multi {
            self.sync_surfaces(event_loop);
            return;
        }
        if let Some(slot) = self.slots.get(SurfaceId::MAIN) {
            slot.window.request_redraw();
        }
    }

    /// Decides, once per event-loop iteration, whether to keep spinning.
    ///
    /// A poll-mode host requests a redraw after every iteration so the logic
    /// thread's latest `RenderFrame` is presented without waiting for a user
    /// event (hot-reload in `byard dev`), but only *while the application has
    /// frame-driven work*. When [`PlatformHost::wants_frames`] goes false (every
    /// animation settled, per the RFC-0010/RFC-0025 active set) the loop drops
    /// back to `Wait` and genuinely idles at zero frames; input events and the
    /// logic thread's frame waker both bring it back.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.multi {
            // Every window that asked this round is drawn together, so the
            // glyph atlas is trimmed once (RFC-0068 §5).
            self.flush_redraws(event_loop);
            if !self.poll {
                return;
            }
            event_loop.set_control_flow(control_flow_for(self.host.wants_frames()));
            if self.host.wants_frames() {
                self.sync_surfaces(event_loop);
            }
            return;
        }
        if !self.poll {
            return;
        }
        event_loop.set_control_flow(control_flow_for(self.host.wants_frames()));
        if self.host.wants_frames() {
            if let Some(slot) = self.slots.get(SurfaceId::MAIN) {
                slot.window.request_redraw();
            }
        }
    }
}

/// The free-desktop application id (Wayland `app_id` / X11 `WM_CLASS`) of a
/// window whose app named none: Byard's own tools, such as `byard dev`.
///
/// An app names its own through [`WinitHost::with_app_id`], so the desktop
/// shell attributes each window to its application instead of the generic
/// "Unknown"/"Desconocido" label a Wayland toplevel with no `app_id` gets, and
/// a `.desktop` file of that name can supply the taskbar icon and name.
const DEFAULT_APP_ID: &str = "dev.byard.Byard";

/// Applies the free-desktop application id to `attrs` on Wayland and X11.
///
/// winit exposes the app id only through its per-backend attribute extension
/// traits. Both are available on free-desktop targets and applying both is
/// correct: a Wayland session ignores the X11 `WM_CLASS` and vice versa. On
/// macOS/Windows/mobile there is no such concept, so the other definition below
/// is the identity.
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn with_app_id(
    attrs: winit::window::WindowAttributes,
    app_id: &str,
) -> winit::window::WindowAttributes {
    // Fully-qualified rather than `use`d as a trait: both extension traits
    // expose `with_name`, so bringing them into scope would be ambiguous (and
    // an unused import on the call that doesn't resolve through it).
    let attrs =
        winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, app_id, "byard");
    winit::platform::x11::WindowAttributesExtX11::with_name(attrs, app_id, "byard")
}

/// No-op on platforms without a free-desktop application id (macOS, Windows,
/// mobile): the window title is the only identifier those shells use.
#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
fn with_app_id(
    attrs: winit::window::WindowAttributes,
    _app_id: &str,
) -> winit::window::WindowAttributes {
    attrs
}

/// Converts a `winit` physical size + DPI scale factor into a
/// [`WindowSize`].
///
/// Extracted as a pure function so the conversion is testable without a real
/// `winit` event loop or display, neither of which exist in CI.
fn to_window_size(size: PhysicalSize<u32>, scale_factor: f64) -> WindowSize {
    WindowSize {
        width: size.width,
        height: size.height,
        scale_factor,
        refresh_rate_mhz: None,
    }
}

/// Converts `winit`'s modifier state into the windowing-crate-agnostic
/// [`KeyModifiers`] that crosses into `byard-core`.
fn to_key_modifiers(state: ModifiersState) -> KeyModifiers {
    KeyModifiers {
        shift: state.shift_key(),
        ctrl: state.control_key(),
        alt: state.alt_key(),
        meta: state.super_key(),
    }
}

/// Converts a `winit` mouse button into the plain, windowing-crate-agnostic
/// [`PointerButton`] that crosses into `byard-core`.
///
/// Extracted as a pure function for the same reason as [`to_window_size`],
/// testable without a real event loop.
fn to_pointer_button(button: MouseButton) -> PointerButton {
    match button {
        MouseButton::Left => PointerButton::Left,
        MouseButton::Right => PointerButton::Right,
        MouseButton::Middle => PointerButton::Middle,
        MouseButton::Back => PointerButton::Back,
        MouseButton::Forward => PointerButton::Forward,
        MouseButton::Other(code) => PointerButton::Other(code),
    }
}

/// Converts a `winit` element state into the plain [`PointerState`] that
/// crosses into `byard-core`.
fn to_pointer_state(state: ElementState) -> PointerState {
    match state {
        ElementState::Pressed => PointerState::Pressed,
        ElementState::Released => PointerState::Released,
    }
}

/// Which [`PlatformHost`] callback a `winit` scroll delta maps to
/// (RFC-0012 §A loose end): a physical mouse wheel reports discrete
/// [`MouseScrollDelta::LineDelta`]s, a trackpad reports continuous
/// [`MouseScrollDelta::PixelDelta`]s, `winit` doesn't otherwise
/// distinguish the two input devices, so the delta's own shape is the only
/// signal available for routing `wheel` vs `scroll`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ScrollOrigin {
    /// A physical mouse wheel, `on_wheel`.
    Wheel(f32, f32),
    /// A trackpad/touch scroll gesture, `on_scroll`.
    Scroll(f32, f32),
}

/// Converts a `winit` [`MouseScrollDelta`] into a [`ScrollOrigin`], resolving
/// `PixelDelta`'s physical coordinates to logical pixels via `scale_factor`.
///
/// Extracted as a pure function for the same reason as [`to_window_size`],
/// testable without a real event loop.
fn to_scroll_origin(delta: MouseScrollDelta, scale_factor: f64) -> ScrollOrigin {
    match delta {
        MouseScrollDelta::LineDelta(dx, dy) => ScrollOrigin::Wheel(dx, dy),
        MouseScrollDelta::PixelDelta(pos) => {
            let logical = pos.to_logical::<f64>(scale_factor);
            #[allow(clippy::cast_possible_truncation)]
            ScrollOrigin::Scroll(logical.x as f32, logical.y as f32)
        }
    }
}

/// The text a key press typed, if any: `None` for a key that types nothing
/// and for control characters, which are keys rather than text.
fn typed_text(event: &winit::event::KeyEvent) -> Option<&str> {
    event
        .text
        .as_deref()
        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
}

/// Maps winit's IME event onto the engine's platform-neutral one (RFC-0040
/// §3). A pure function, so the mapping is testable without a window.
fn to_ime_event(ime: winit::event::Ime) -> byard_core::ImeEvent {
    use byard_core::ImeEvent;
    use winit::event::Ime;
    match ime {
        Ime::Enabled => ImeEvent::Enabled,
        Ime::Preedit(text, cursor) => ImeEvent::Preedit { text, cursor },
        Ime::Commit(text) => ImeEvent::Commit(text),
        Ime::Disabled => ImeEvent::Disabled,
    }
}

/// Converts a `winit` logical key to a string key name.
///
/// Returns an empty string for keys we don't model (so the caller can skip).
fn key_to_str(key: &Key) -> String {
    match key {
        Key::Character(s) => s.to_string(),
        Key::Named(named) => match named {
            NamedKey::Backspace => "Backspace".to_string(),
            NamedKey::Delete => "Delete".to_string(),
            NamedKey::Enter => "Enter".to_string(),
            NamedKey::Tab => "Tab".to_string(),
            // winit names the space bar rather than giving it as a character,
            // so without this a focused control never saw it as a key, only as
            // the text it also types.
            NamedKey::Space => " ".to_string(),
            NamedKey::Escape => "Escape".to_string(),
            NamedKey::ArrowLeft => "ArrowLeft".to_string(),
            NamedKey::ArrowRight => "ArrowRight".to_string(),
            NamedKey::ArrowUp => "ArrowUp".to_string(),
            NamedKey::ArrowDown => "ArrowDown".to_string(),
            NamedKey::Home => "Home".to_string(),
            NamedKey::End => "End".to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window carries the id its app names, and Byard's own tools keep
    /// `dev.byard.Byard`.
    /// A window's bounds are per axis: an axis with none is left open, and a
    /// window with no bound on either has none to set.
    #[test]
    fn a_window_bound_is_per_axis_and_absent_when_neither_is_written() {
        assert_eq!(bound_size((None, None), 0.0), None);
        let min = bound_size((Some(200.0), None), 0.0).unwrap();
        assert_eq!((min.width, min.height), (200.0, 0.0));
        let max = bound_size((None, Some(500.0)), 1.0e6).unwrap();
        assert_eq!((max.width, max.height), (1.0e6, 500.0));
    }

    #[test]
    fn a_host_carries_the_app_id_it_is_given() {
        assert_eq!(WinitHost::new("t", 1, 1).app_id, "dev.byard.Byard");
        let host = WinitHost::new("t", 1, 1).with_app_id("dev.byard.aura-weather");
        assert_eq!(host.app_id, "dev.byard.aura-weather");
        // Applying it to the window's attributes works on every platform.
        let _ = with_app_id(Window::default_attributes(), &host.app_id);
    }

    /// The space bar reaches the router as the key it is: winit gives it as a
    /// named key, and a control activated by Space (a focused button) would
    /// otherwise never be pressed from a real keyboard.
    #[test]
    fn the_space_bar_is_a_key_like_any_other() {
        assert_eq!(key_to_str(&Key::Named(NamedKey::Space)), " ");
        assert_eq!(key_to_str(&Key::Named(NamedKey::Tab)), "Tab");
        assert_eq!(key_to_str(&Key::Character("a".into())), "a");
    }

    /// Every winit IME event maps onto the engine's own, field for field
    /// (RFC-0040 §3), so the host never has to know which windowing library
    /// produced it.
    #[test]
    fn ime_events_map_one_to_one() {
        use byard_core::ImeEvent;
        use winit::event::Ime;
        assert_eq!(to_ime_event(Ime::Enabled), ImeEvent::Enabled);
        assert_eq!(
            to_ime_event(Ime::Preedit("にほん".into(), Some((3, 6)))),
            ImeEvent::Preedit {
                text: "にほん".into(),
                cursor: Some((3, 6))
            }
        );
        assert_eq!(
            to_ime_event(Ime::Preedit(String::new(), None)),
            ImeEvent::Preedit {
                text: String::new(),
                cursor: None
            }
        );
        assert_eq!(
            to_ime_event(Ime::Commit("日本".into())),
            ImeEvent::Commit("日本".into())
        );
        assert_eq!(to_ime_event(Ime::Disabled), ImeEvent::Disabled);
    }

    /// Every modifier `winit` reports reaches the engine as the one it is, and
    /// nothing else does (RFC-0041 §2): a text field tells Shift+Left from
    /// Ctrl+Left by these four.
    #[test]
    fn modifier_state_maps_each_key_to_its_own_field() {
        assert_eq!(
            to_key_modifiers(ModifiersState::empty()),
            KeyModifiers::default()
        );
        let all = to_key_modifiers(
            ModifiersState::SHIFT
                | ModifiersState::CONTROL
                | ModifiersState::ALT
                | ModifiersState::SUPER,
        );
        assert!(all.shift && all.ctrl && all.alt && all.meta);
        let shift_only = to_key_modifiers(ModifiersState::SHIFT);
        assert!(shift_only.shift && !shift_only.ctrl && !shift_only.alt && !shift_only.meta);
        let ctrl_only = to_key_modifiers(ModifiersState::CONTROL);
        assert!(ctrl_only.ctrl && !ctrl_only.shift && !ctrl_only.alt && !ctrl_only.meta);
        let alt_only = to_key_modifiers(ModifiersState::ALT);
        assert!(alt_only.alt && !alt_only.ctrl);
        let super_only = to_key_modifiers(ModifiersState::SUPER);
        assert!(super_only.meta && !super_only.ctrl);
    }

    #[test]
    fn to_window_size_carries_fields_through_unchanged() {
        let phys = PhysicalSize::new(1024_u32, 768_u32);
        let size = to_window_size(phys, 2.0);

        assert_eq!(size.width, 1024);
        assert_eq!(size.height, 768);
        assert!((size.scale_factor - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn to_window_size_handles_non_hidpi_scale() {
        let phys = PhysicalSize::new(800_u32, 600_u32);
        let size = to_window_size(phys, 1.0);

        assert_eq!(size.width, 800);
        assert_eq!(size.height, 600);
        assert!((size.scale_factor - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn to_pointer_button_maps_every_winit_variant() {
        assert_eq!(to_pointer_button(MouseButton::Left), PointerButton::Left);
        assert_eq!(to_pointer_button(MouseButton::Right), PointerButton::Right);
        assert_eq!(
            to_pointer_button(MouseButton::Middle),
            PointerButton::Middle
        );
        assert_eq!(to_pointer_button(MouseButton::Back), PointerButton::Back);
        assert_eq!(
            to_pointer_button(MouseButton::Forward),
            PointerButton::Forward
        );
        assert_eq!(
            to_pointer_button(MouseButton::Other(9)),
            PointerButton::Other(9)
        );
    }

    #[test]
    fn to_pointer_state_maps_both_winit_variants() {
        assert_eq!(
            to_pointer_state(ElementState::Pressed),
            PointerState::Pressed
        );
        assert_eq!(
            to_pointer_state(ElementState::Released),
            PointerState::Released
        );
    }

    #[test]
    fn to_scroll_origin_routes_line_delta_to_wheel() {
        // A physical mouse wheel, line deltas are already logical units,
        // `scale_factor` must not affect them.
        assert_eq!(
            to_scroll_origin(MouseScrollDelta::LineDelta(0.0, 1.0), 2.0),
            ScrollOrigin::Wheel(0.0, 1.0)
        );
    }

    #[test]
    fn to_scroll_origin_routes_pixel_delta_to_scroll_and_converts_to_logical() {
        // A trackpad, physical pixel deltas must be divided by the scale
        // factor to land in the same logical-pixel space as everything else.
        let physical = winit::dpi::PhysicalPosition::new(20.0, 40.0);
        let origin = to_scroll_origin(MouseScrollDelta::PixelDelta(physical), 2.0);
        assert_eq!(origin, ScrollOrigin::Scroll(10.0, 20.0));
    }

    #[test]
    fn to_scroll_origin_pixel_delta_at_unit_scale_is_unchanged() {
        let physical = winit::dpi::PhysicalPosition::new(5.0, -3.0);
        let origin = to_scroll_origin(MouseScrollDelta::PixelDelta(physical), 1.0);
        assert_eq!(origin, ScrollOrigin::Scroll(5.0, -3.0));
    }

    #[test]
    fn a_settled_app_stops_spinning_and_a_moving_one_keeps_polling() {
        // The whole idle contract in one line: while the RFC-0010 active set has
        // work the loop spins (smooth animation), and the moment it empties the
        // loop blocks on OS events instead of burning a core on a static scene.
        assert_eq!(control_flow_for(true), ControlFlow::Poll);
        assert_eq!(control_flow_for(false), ControlFlow::Wait);
    }
}
