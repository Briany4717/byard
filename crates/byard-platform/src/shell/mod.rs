//! The Wayland shell backend (RFC-0068 §8): a program with a `Panel` runs here.
//!
//! One Wayland connection, one calloop event loop, and every surface of the
//! program on them: a layer surface for each `Panel`, an `xdg_toplevel` for
//! each `Window`. Built on smithay-client-toolkit, the 0.19 winit already links.
//!
//! # How a frame gets to the screen
//!
//! The logic thread publishes frames and wakes the loop (a calloop ping, the
//! frame waker of RFC-0029). The wake applies what it queued about surfaces
//! (create, configure, destroy, input region) through [`windows::Windows`], the
//! bookkeeping the winit host shares, and finds the surfaces with a frame newer
//! than the one they last drew. A surface draws only when it has such a frame
//! and its own `wl_surface.frame` callback has fired (RFC-0068 §5): a bar on a
//! screen that is off, or behind a full-screen window, is throttled by the
//! compositor and costs nothing. A surface that has not been configured yet
//! keeps its frames until the first `configure`, so it opens with its content.
//!
//! With nothing in flight nothing is committed and the loop sleeps in
//! `poll(2)`: `WAYLAND_DEBUG=1` shows no traffic for a still bar.
//!
//! # What is not here
//!
//! Text input (IME) and compose keys: a text field in a panel takes the keys
//! the keymap gives and nothing an input method would add. Popups are the next
//! step; `Backend::create` and `Backend::configure` are where a
//! `SurfaceKind::Popup` arm goes.

#[cfg(feature = "a11y")]
mod a11y;
mod handle;
mod handlers;
mod keys;
mod layer;
mod outputs;
mod region;
mod seat;
mod xkb;

use std::sync::Arc;
use std::time::Duration;

use byard_core::relay::FrameWaker;
use byard_core::surface::InputRegion;
use byard_core::{
    ByardError, PerSurface, PlatformHost, ScreenInfo, SurfaceId, SurfaceKind,
    SurfaceSpec, WindowSize,
};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop::ping::make_ping;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{GlobalList, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{LayerShell, LayerSurface};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowDecorations};

use crate::unsupported;
use crate::windows::{Backend, Windows};

/// Runs a program of panels, and the windows beside them, on Wayland.
///
/// ```ignore
/// ShellHost::new("My shell", "dev.example.shell").run(host)?;
/// ```
pub struct ShellHost {
    title: String,
    app_id: String,
}

impl ShellHost {
    /// A host whose windows are titled `title` unless they say otherwise, and
    /// whose windows carry `app_id` (the id a desktop matches `.desktop` files
    /// and window rules to).
    pub fn new(title: impl Into<String>, app_id: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            app_id: app_id.into(),
        }
    }

    /// Connects to the compositor and runs `host` until the program is
    /// stopped. A program with panels has no last window, so it ends when it is
    /// told to (a `Window`'s `close =>` that quits, a signal), not because a
    /// surface went.
    ///
    /// # Errors
    ///
    /// [`ByardError::Platform`] saying `PanelsUnsupported` when there is no
    /// Wayland session or the compositor has no `zwlr_layer_shell_v1`, before
    /// anything is made; or whatever a host callback or the connection fails
    /// with.
    pub fn run(self, host: impl PlatformHost + 'static) -> Result<(), ByardError> {
        run(self, Box::new(host))
    }
}

/// A surface the backend made: the Wayland objects and what is known of it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one switch per stage of a surface's life"
)]
struct Surf {
    spec: SurfaceSpec,
    /// What was last sent to a layer surface (nothing for a window).
    wire: Option<layer::Wire>,
    role: Role,
    wl: WlSurface,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    /// The scale the surface is drawn at.
    scale: f64,
    /// The size the compositor configured it to, in logical pixels.
    logical: (u32, u32),
    /// Whether the compositor has configured it, which is when it may draw.
    configured: bool,
    /// Whether the engine has a wgpu surface for it.
    drawable: bool,
    /// Whether a newer frame than the last drawn is waiting for it.
    want_draw: bool,
    /// Whether the callback of the last frame it presented is still to come.
    callback_pending: bool,
    /// How many rounds in a row asked it to draw and presented nothing.
    misses: u8,
    /// What the interpreter last said takes the pointer.
    region: Option<InputRegion>,
    /// What was sent for the input region last.
    region_sent: region::Plan,
}

/// The role object of a surface.
enum Role {
    Layer(LayerSurface),
    Window(Window),
}

impl Role {
    fn commit(&self) {
        match self {
            Self::Layer(l) => l.commit(),
            Self::Window(w) => w.commit(),
        }
    }
}

/// A `Panel` asked for before its screen is there (it was unplugged between
/// the program reading `screens` and the surface being made, or is not yet
/// listed): made when the screen is.
struct Waiting {
    id: SurfaceId,
    spec: SurfaceSpec,
}

/// The whole state of the backend, which is also the toolkit's handler state.
pub(crate) struct Shell {
    // The host is first so that it drops first: its wgpu surfaces borrow
    // Wayland objects that must still be alive when they go.
    host: Box<dyn PlatformHost>,
    conn: Connection,
    qh: QueueHandle<Shell>,
    loop_handle: LoopHandle<'static, Shell>,
    instance: wgpu::Instance,
    registry: RegistryState,
    compositor: CompositorState,
    outputs: OutputState,
    seats: SeatState,
    layers: LayerShell,
    /// The layer-shell version bound, 1 to 4.
    layer_version: u32,
    xdg: Option<XdgShell>,
    viewporter: Option<WpViewporter>,
    fractional_manager: Option<WpFractionalScaleManagerV1>,
    cursor_shape: Option<CursorShapeManager>,
    wins: Windows,
    surfs: PerSurface<Surf>,
    waiting: Vec<Waiting>,
    /// Surfaces that may be able to draw this round.
    queued: Vec<SurfaceId>,
    /// Whether the loop must come round again at once (a surface that
    /// presented nothing and is worth another try).
    retry: bool,
    /// Whether an output was added, removed or changed since the screens were
    /// last told to the program.
    screens_dirty: bool,
    /// The screens the program was last told.
    screens_told: Vec<ScreenInfo>,
    seat: seat::Devices,
    /// The screen-reader adapter of each surface (RFC-0042).
    #[cfg(feature = "a11y")]
    a11y: a11y::Bridges,
    #[cfg(feature = "a11y")]
    a11y_tx: smithay_client_toolkit::reexports::calloop::channel::Sender<a11y::Message>,
    app_id: String,
    title: String,
    done: bool,
    fatal: Option<ByardError>,
}

fn platform_error(what: impl std::fmt::Display) -> ByardError {
    ByardError::Platform(what.to_string())
}

/// The version of the global `interface` a compositor advertises, if it does.
fn advertised(globals: &GlobalList, interface: &str) -> Option<u32> {
    globals.contents().with_list(|list| {
        list.iter()
            .find(|g| g.interface == interface)
            .map(|g| g.version)
    })
}

#[allow(clippy::too_many_lines, reason = "the whole start of the backend")]
fn run(cfg: ShellHost, host: Box<dyn PlatformHost>) -> Result<(), ByardError> {
    let conn = Connection::connect_to_env()
        .map_err(|e| platform_error(unsupported::no_wayland(&e.to_string())))?;
    let (globals, mut queue) = registry_queue_init::<Shell>(&conn).map_err(|e| {
        platform_error(unsupported::no_wayland(&format!(
            "the registry could not be read: {e}"
        )))
    })?;
    let qh = queue.handle();

    let layer_version = advertised(&globals, layer::INTERFACE)
        .map(layer::bound_version)
        .ok_or_else(|| platform_error(unsupported::no_layer_shell()))?;
    let layers = LayerShell::bind(&globals, &qh)
        .map_err(|_| platform_error(unsupported::no_layer_shell()))?;
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|e| platform_error(format!("this compositor has no wl_compositor: {e}")))?;

    let mut event_loop: EventLoop<'static, Shell> = EventLoop::try_new().map_err(platform_error)?;
    let loop_handle = event_loop.handle();
    let (ping, ping_source) = make_ping().map_err(platform_error)?;
    let waker: FrameWaker = Arc::new(move || ping.ping());

    #[cfg(feature = "a11y")]
    let (a11y_tx, a11y_rx) = smithay_client_toolkit::reexports::calloop::channel::channel();
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

    let mut shell = Shell {
        host,
        conn: conn.clone(),
        qh: qh.clone(),
        loop_handle: loop_handle.clone(),
        instance,
        registry: RegistryState::new(&globals),
        compositor,
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        layers,
        layer_version,
        xdg: XdgShell::bind(&globals, &qh).ok(),
        viewporter: globals.bind(&qh, 1..=1, ()).ok(),
        fractional_manager: globals.bind(&qh, 1..=1, ()).ok(),
        cursor_shape: CursorShapeManager::bind(&globals, &qh).ok(),
        wins: Windows::for_shell(),
        surfs: PerSurface::new(),
        waiting: Vec::new(),
        queued: Vec::new(),
        retry: false,
        screens_dirty: true,
        screens_told: Vec::new(),
        seat: seat::Devices::default(),
        #[cfg(feature = "a11y")]
        a11y: a11y::Bridges::default(),
        #[cfg(feature = "a11y")]
        a11y_tx,
        app_id: cfg.app_id,
        title: cfg.title,
        done: false,
        fatal: None,
    };

    // The registry's first answers: the outputs with their names and sizes,
    // the seats with what they can do, the keymaps. Two round trips, so the
    // program is told its screens before anything is drawn.
    queue.roundtrip(&mut shell).map_err(platform_error)?;
    queue.roundtrip(&mut shell).map_err(platform_error)?;

    WaylandSource::new(conn, queue)
        .insert(loop_handle.clone())
        .map_err(|e| platform_error(e.error))?;
    loop_handle
        .insert_source(ping_source, |(), _unit, shell: &mut Shell| shell.wake())
        .map_err(|e| platform_error(e.error))?;

    #[cfg(feature = "a11y")]
    loop_handle
        .insert_source(a11y_rx, |event, (), shell: &mut Shell| {
            if let smithay_client_toolkit::reexports::calloop::channel::Event::Msg(m) = event {
                shell.access_message(m);
            }
        })
        .map_err(|e| platform_error(e.error))?;

    shell.host.on_start(waker)?;
    shell.tell_screens();

    while !shell.done && shell.fatal.is_none() {
        let timeout = shell.retry.then_some(Duration::ZERO);
        shell.retry = false;
        event_loop
            .dispatch(timeout, &mut shell)
            .map_err(platform_error)?;
        shell.after_dispatch();
    }
    shell.fatal.take().map_or(Ok(()), Err)
}

impl Shell {
    /// Records a fatal error and ends the loop.
    pub(crate) fn fail(&mut self, error: ByardError) {
        self.fatal.get_or_insert(error);
    }

    /// The frame waker fired: the logic thread published something. Applies
    /// what it queued about surfaces and finds the surfaces with a new frame.
    fn wake(&mut self) {
        let changes = self.host.take_surface_changes();
        if !changes.is_empty() {
            if let Err(e) = self.with_windows(|w, b| w.apply(&changes, b)) {
                self.fail(platform_error(e));
                return;
            }
        }
        let versions: Vec<(SurfaceId, Option<u64>)> = self
            .wins
            .ids()
            .map(|id| (id, self.host.frame_version(id)))
            .collect();
        self.with_windows(|w, b| {
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

    /// Runs `f` on the window bookkeeping with this as its backend.
    pub(crate) fn with_windows<R>(
        &mut self,
        f: impl FnOnce(&mut Windows, &mut Real<'_>) -> R,
    ) -> R {
        let mut wins = std::mem::take(&mut self.wins);
        let out = f(&mut wins, &mut Real { shell: self });
        // `take` left a default behind, and nothing in `f` made a window of
        // its own, so what `f` did is all in `wins`.
        self.wins = wins;
        out
    }

    /// After each batch of events: tell the program its screens if they
    /// changed, make the panels that were waiting for a screen, and draw the
    /// surfaces that may.
    fn after_dispatch(&mut self) {
        if self.screens_dirty {
            self.tell_screens();
            self.make_waiting();
        }
        self.flush_round();
    }

    /// Tells the program the list of screens, when it changed.
    fn tell_screens(&mut self) {
        self.screens_dirty = false;
        let facts = self.output_facts();
        let list = outputs::screens(&facts, self.fractional_manager.is_some());
        if list != self.screens_told || self.screens_told.is_empty() {
            self.screens_told.clone_from(&list);
            self.host.on_screens(list);
        }
    }

    fn output_facts(&self) -> Vec<outputs::OutputFacts> {
        self.outputs
            .outputs()
            .filter_map(|o| self.outputs.info(&o))
            .map(|info| outputs::OutputFacts {
                name: info.name.clone(),
                description: info.description.clone(),
                location: info.location,
                logical_position: info.logical_position,
                logical_size: info.logical_size,
                mode: info
                    .modes
                    .iter()
                    .find(|m| m.current)
                    .map(|m| m.dimensions),
                scale_factor: info.scale_factor,
                rotated: matches!(
                    info.transform,
                    smithay_client_toolkit::reexports::client::protocol::wl_output::Transform::_90
                        | smithay_client_toolkit::reexports::client::protocol::wl_output::Transform::_270
                        | smithay_client_toolkit::reexports::client::protocol::wl_output::Transform::Flipped90
                        | smithay_client_toolkit::reexports::client::protocol::wl_output::Transform::Flipped270
                ),
            })
            .collect()
    }

    /// The `wl_output` the screen called `name` is, by the same naming
    /// `screens` uses.
    fn output_named(&self, name: &str) -> Option<WlOutput> {
        let mut n = 0;
        for output in self.outputs.outputs() {
            let Some(info) = self.outputs.info(&output) else {
                continue;
            };
            let facts = outputs::OutputFacts {
                name: info.name.clone(),
                ..outputs::OutputFacts::default()
            };
            if outputs::name_of(n, &facts) == name {
                return Some(output);
            }
            n += 1;
        }
        None
    }

    /// The refresh rate, in millihertz, of the screen a surface is on: its
    /// `screen` when it names one, else the first.
    fn refresh_rate(&self, spec: &SurfaceSpec) -> Option<u32> {
        let wanted = spec.layer.as_ref().and_then(|l| l.screen.as_deref());
        let output = wanted
            .and_then(|n| self.output_named(n))
            .or_else(|| self.outputs.outputs().next())?;
        let info = self.outputs.info(&output)?;
        let mode = info.modes.iter().find(|m| m.current)?;
        u32::try_from(mode.refresh_rate).ok().filter(|r| *r > 0)
    }

    // ── surfaces ────────────────────────────────────────────────────────

    /// Makes the surface of `id` from `spec`, or waits for its screen.
    fn make_surface(&mut self, id: SurfaceId, spec: &SurfaceSpec) -> Result<(), String> {
        match spec.kind {
            SurfaceKind::Panel => self.make_panel(id, spec),
            SurfaceKind::Window => self.make_toplevel(id, spec),
            // A surface kind this backend has no way to make yet; a new
            // kind's arm goes above this one.
            _ => Err(format!(
                "this backend cannot make a surface of kind {:?} yet",
                spec.kind
            )),
        }
    }

    /// The `wl_surface` of a new surface, with the scaling objects it uses.
    fn new_wl_surface(
        &self,
        id: SurfaceId,
    ) -> (WlSurface, Option<WpViewport>, Option<WpFractionalScaleV1>) {
        let wl = self.compositor.create_surface(&self.qh);
        let (viewport, fractional) = match (&self.viewporter, &self.fractional_manager) {
            (Some(viewporter), Some(manager)) => (
                Some(viewporter.get_viewport(&wl, &self.qh, ())),
                Some(manager.get_fractional_scale(&wl, &self.qh, id)),
            ),
            _ => (None, None),
        };
        (wl, viewport, fractional)
    }

    fn make_panel(&mut self, id: SurfaceId, spec: &SurfaceSpec) -> Result<(), String> {
        let Some(l) = spec.layer.as_ref() else {
            return Err("a panel has no layer-shell part".to_string());
        };
        let output = l.screen.as_deref().map(|name| self.output_named(name));
        if matches!(output, Some(None)) {
            // Its screen is not here (yet): the program learns of it from
            // `screens`, and the panel is made when it is.
            self.waiting.push(Waiting {
                id,
                spec: spec.clone(),
            });
            return Ok(());
        }
        let output = output.flatten();
        let (wl, viewport, fractional) = self.new_wl_surface(id);
        let layer_surface = self.layers.create_layer_surface(
            &self.qh,
            wl.clone(),
            layer::layer(l.layer),
            Some(l.namespace.clone()),
            output.as_ref(),
        );
        let wire = layer::Wire::of(l, spec.size, self.layer_version, false);
        let changes = wire.changes_from(None);
        if changes.anchor {
            layer_surface.set_anchor(wire.anchor);
        }
        if changes.size {
            layer_surface.set_size(wire.size.0, wire.size.1);
        }
        if changes.zone {
            layer_surface.set_exclusive_zone(wire.zone);
        }
        if changes.margin {
            let (t, r, b, le) = wire.margin;
            layer_surface.set_margin(t, r, b, le);
        }
        if changes.keyboard {
            layer_surface.set_keyboard_interactivity(wire.keyboard);
        }
        // The initial commit, with no buffer: it is what asks the compositor
        // for the first `configure`.
        layer_surface.commit();
        #[cfg(feature = "a11y")]
        self.a11y.attach(id, &self.a11y_tx);
        self.surfs.insert(
            id,
            Surf::new(
                spec.clone(),
                Some(wire),
                Role::Layer(layer_surface),
                wl,
                viewport,
                fractional,
            ),
        );
        Ok(())
    }

    fn make_toplevel(&mut self, id: SurfaceId, spec: &SurfaceSpec) -> Result<(), String> {
        let Some(xdg) = self.xdg.as_ref() else {
            return Err(
                "this compositor does not offer xdg_wm_base, so it cannot open a \
                        `Window`"
                    .to_string(),
            );
        };
        let (wl, viewport, fractional) = self.new_wl_surface(id);
        let decorations = if spec.decorations {
            WindowDecorations::RequestServer
        } else {
            WindowDecorations::None
        };
        let window = xdg.create_window(wl.clone(), decorations, &self.qh);
        window.set_title(if spec.title.is_empty() {
            self.title.clone()
        } else {
            spec.title.clone()
        });
        window.set_app_id(self.app_id.clone());
        apply_window_bounds(&window, spec);
        window.commit();
        #[cfg(feature = "a11y")]
        self.a11y.attach(id, &self.a11y_tx);
        self.surfs.insert(
            id,
            Surf::new(
                spec.clone(),
                None,
                Role::Window(window),
                wl,
                viewport,
                fractional,
            ),
        );
        Ok(())
    }

    /// Makes the panels that were waiting for a screen that is there now.
    fn make_waiting(&mut self) {
        let waiting = std::mem::take(&mut self.waiting);
        for w in waiting {
            let screen_here = w
                .spec
                .layer
                .as_ref()
                .and_then(|l| l.screen.as_deref())
                .is_none_or(|n| self.output_named(n).is_some());
            if !screen_here {
                self.waiting.push(w);
                continue;
            }
            if let Err(e) = self.make_surface(w.id, &w.spec) {
                self.fail(platform_error(e));
                return;
            }
        }
    }

    /// Sends a reconfigured spec to a surface that is open.
    fn configure_surface(&mut self, id: SurfaceId, spec: &SurfaceSpec) {
        if let Some(w) = self.waiting.iter_mut().find(|w| w.id == id) {
            w.spec = spec.clone();
            return;
        }
        let focused_field = self.host.text_input(id).is_some();
        let version = self.layer_version;
        let Some(surf) = self.surfs.get_mut(id) else {
            return;
        };
        match (&surf.role, spec.layer.as_ref()) {
            (Role::Layer(ls), Some(l)) => {
                let wire = layer::Wire::of(l, spec.size, version, focused_field);
                let changes = wire.changes_from(surf.wire.as_ref());
                if changes.layer {
                    ls.set_layer(wire.layer);
                }
                if changes.anchor {
                    ls.set_anchor(wire.anchor);
                }
                if changes.size {
                    ls.set_size(wire.size.0, wire.size.1);
                }
                if changes.zone {
                    ls.set_exclusive_zone(wire.zone);
                }
                if changes.margin {
                    let (t, r, b, le) = wire.margin;
                    ls.set_margin(t, r, b, le);
                }
                if changes.keyboard {
                    ls.set_keyboard_interactivity(wire.keyboard);
                }
                if changes.any() {
                    // The requests are double-buffered: they apply on commit.
                    ls.commit();
                }
                surf.wire = Some(wire);
            }
            (Role::Window(w), _) => {
                if spec.title != surf.spec.title {
                    w.set_title(if spec.title.is_empty() {
                        self.title.clone()
                    } else {
                        spec.title.clone()
                    });
                }
                if (spec.min_size, spec.max_size, spec.resizable)
                    != (surf.spec.min_size, surf.spec.max_size, surf.spec.resizable)
                {
                    apply_window_bounds(w, spec);
                    w.commit();
                }
            }
            (Role::Layer(_), None) => {}
        }
        surf.spec = spec.clone();
    }

    /// Takes a surface down: the engine's surface first, since it holds the
    /// Wayland object, then the objects.
    fn destroy_surface(&mut self, id: SurfaceId) {
        self.waiting.retain(|w| w.id != id);
        self.queued.retain(|s| *s != id);
        let Some(surf) = self.surfs.remove(id) else {
            return;
        };
        if surf.drawable {
            self.host.on_surface_destroyed(id);
        }
        self.seat.surface_gone(id);
        #[cfg(feature = "a11y")]
        self.a11y.detach(id);
        if let Some(f) = surf.fractional {
            f.destroy();
        }
        if let Some(v) = surf.viewport {
            v.destroy();
        }
        // The role object destroys itself, and its `wl_surface`, when dropped.
        drop(surf.role);
    }

    /// The compositor configured `id` to `logical` pixels: the first one is
    /// when the surface can be drawn into, a later one resizes it.
    pub(crate) fn configured(&mut self, id: SurfaceId, logical: (u32, u32)) {
        let Some(surf) = self.surfs.get_mut(id) else {
            return;
        };
        let first = !surf.configured;
        let changed = surf.logical != logical;
        surf.logical = logical;
        surf.configured = true;
        self.apply_scale(id);
        if first {
            if let Err(e) = self.make_drawable(id) {
                self.fail(e);
                return;
            }
        } else if changed {
            let size = self.window_size(id);
            if let Some(size) = size {
                self.host.on_resize(id, size);
            }
        }
        self.apply_region(id);
        let Some(surf) = self.surfs.get_mut(id) else {
            return;
        };
        if first || changed {
            surf.want_draw = true;
            self.queued.push(id);
        } else {
            // Nothing to draw, and the compositor still expects the commit
            // that answers its configure.
            surf.role.commit();
        }
    }

    /// Makes the engine's surface for `id`, now that the compositor has said
    /// what size it is.
    fn make_drawable(&mut self, id: SurfaceId) -> Result<(), ByardError> {
        let Some(surf) = self.surfs.get(id) else {
            return Ok(());
        };
        let target = handle::WaylandTarget::new(&self.conn, &surf.wl)
            .ok_or_else(|| platform_error("the Wayland connection has no libwayland display"))?;
        let wgpu_surface = self
            .instance
            .create_surface(target)
            .map_err(|e| platform_error(format!("a Wayland surface for wgpu: {e}")))?;
        let transparent = surf.spec.transparent;
        let Some(size) = self.window_size(id) else {
            return Ok(());
        };
        self.host
            .on_surface_created(&self.instance, id, wgpu_surface, size, transparent)?;
        if let Some(surf) = self.surfs.get_mut(id) {
            surf.drawable = true;
        }
        Ok(())
    }

    /// The size the engine is given for a surface: physical pixels and the
    /// scale.
    fn window_size(&self, id: SurfaceId) -> Option<WindowSize> {
        let surf = self.surfs.get(id)?;
        let (width, height) = layer::physical_size(surf.logical, surf.scale);
        Some(WindowSize {
            width,
            height,
            scale_factor: surf.scale,
            refresh_rate_mhz: self.refresh_rate(&surf.spec),
        })
    }

    /// Tells the compositor how big a buffer pixel is: the viewport maps the
    /// buffer onto the logical size when the scale is fractional, and the
    /// buffer scale does it for an integer one.
    fn apply_scale(&self, id: SurfaceId) {
        let Some(surf) = self.surfs.get(id) else {
            return;
        };
        if let Some(viewport) = &surf.viewport {
            let w = i32::try_from(surf.logical.0).unwrap_or(i32::MAX);
            let h = i32::try_from(surf.logical.1).unwrap_or(i32::MAX);
            if w > 0 && h > 0 {
                viewport.set_destination(w, h);
            }
        } else {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an integer scale is a small positive whole number"
            )]
            let n = surf.scale.round().max(1.0) as u32;
            if surf.wl.version() >= 3 {
                surf.wl.set_buffer_scale(i32::try_from(n).unwrap_or(1));
            }
        }
    }

    /// The scale of a surface changed (`wp_fractional_scale_v1`, or the
    /// integer scale of its output).
    pub(crate) fn set_scale(&mut self, id: SurfaceId, scale: f64) {
        let Some(surf) = self.surfs.get_mut(id) else {
            return;
        };
        if (surf.scale - scale).abs() < 1.0e-6 {
            return;
        }
        surf.scale = scale;
        if !surf.configured {
            return;
        }
        self.apply_scale(id);
        if let Some(size) = self.window_size(id) {
            if self.surfs.get(id).is_some_and(|s| s.drawable) {
                self.host.on_resize(id, size);
            }
        }
        if let Some(surf) = self.surfs.get_mut(id) {
            surf.want_draw = true;
            self.queued.push(id);
        }
    }

    /// Sends the input region of `id` if it differs from the one it has.
    fn apply_region(&mut self, id: SurfaceId) {
        let Some(surf) = self.surfs.get_mut(id) else {
            return;
        };
        let Some(wanted) = surf.region.clone() else {
            return;
        };
        if !surf.configured {
            return;
        }
        let size = (
            i32::try_from(surf.logical.0).unwrap_or(i32::MAX),
            i32::try_from(surf.logical.1).unwrap_or(i32::MAX),
        );
        let plan = region::plan(&wanted, size);
        if plan == surf.region_sent {
            return;
        }
        match &plan {
            region::Plan::Whole => surf.wl.set_input_region(None),
            region::Plan::Rects(rects) => match Region::new(&self.compositor) {
                Ok(r) => {
                    for &(x, y, w, h) in rects {
                        r.add(x, y, w, h);
                    }
                    surf.wl.set_input_region(Some(r.wl_region()));
                }
                Err(_) => return,
            },
        }
        surf.region_sent = plan;
        // Applies on the next commit; the frame that follows carries it, and
        // a region that arrives with none behind it needs its own.
        surf.role.commit();
    }

    // ── drawing ─────────────────────────────────────────────────────────

    /// A surface may be able to draw: it is looked at at the end of the
    /// loop's round.
    fn queue(&mut self, id: SurfaceId) {
        if !self.queued.contains(&id) {
            self.queued.push(id);
        }
    }

    /// Draws the surfaces that have a newer frame and whose frame callback
    /// fired, as one round (so the glyph atlas is trimmed once, RFC-0068 §5).
    fn flush_round(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let mut ids = Vec::new();
        for id in std::mem::take(&mut self.queued) {
            let ready = self
                .surfs
                .get(id)
                .is_some_and(|s| s.drawable && s.configured && s.want_draw && !s.callback_pending);
            // A surface with no frame yet keeps waiting: the first one the
            // logic thread publishes wakes the loop.
            if ready && self.host.frame_version(id).is_some() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return;
        }
        // The frame request goes in before the commit that presenting makes,
        // so its callback is that frame's.
        for &id in &ids {
            if let Some(s) = self.surfs.get_mut(id) {
                s.want_draw = false;
                s.callback_pending = true;
                s.wl.frame(&self.qh, s.wl.clone());
            }
        }
        if let Err(e) = self.host.on_redraw_round(&ids) {
            self.fail(e);
            return;
        }
        let presented = self.host.take_presented().unwrap_or_else(|| ids.clone());
        for &id in &ids {
            let Some(s) = self.surfs.get_mut(id) else {
                continue;
            };
            if presented.contains(&id) {
                s.misses = 0;
                continue;
            }
            // Nothing was committed (a swapchain that had to be made again,
            // a surface the compositor is not showing), so no callback is
            // coming for this request. Try again, a few times.
            s.callback_pending = false;
            s.want_draw = true;
            s.misses = s.misses.saturating_add(1);
            if s.misses < 3 {
                self.retry = true;
                self.queue(id);
            }
        }
        self.refresh_keyboard_modes(&ids);
        #[cfg(feature = "a11y")]
        self.sync_access(&ids);
        let _ = self.conn.flush();
    }

    /// On a layer shell that has no `on_demand`, the keyboard mode of a panel
    /// follows whether it has a focused text field (RFC-0068 §3). Looked at
    /// after each frame, which is when focus moves.
    fn refresh_keyboard_modes(&mut self, ids: &[SurfaceId]) {
        if self.layer_version >= layer::ON_DEMAND_SINCE {
            return;
        }
        for &id in ids {
            let Some(spec) = self.surfs.get(id).map(|s| s.spec.clone()) else {
                continue;
            };
            if spec
                .layer
                .as_ref()
                .is_some_and(|l| l.keyboard == byard_core::surface::KeyboardMode::OnDemand)
            {
                self.configure_surface(id, &spec);
            }
        }
    }

    /// The compositor's frame callback of `wl` fired: it is a good time to
    /// draw again, if there is something to draw.
    pub(crate) fn frame_done(&mut self, wl: &WlSurface) {
        let Some(id) = self.find(wl) else {
            return;
        };
        if let Some(s) = self.surfs.get_mut(id) {
            s.callback_pending = false;
            if s.want_draw {
                self.queue(id);
            }
        }
    }

    /// The surface a `wl_surface` is of.
    pub(crate) fn find(&self, wl: &WlSurface) -> Option<SurfaceId> {
        self.surfs
            .iter()
            .find(|(_, s)| &s.wl == wl)
            .map(|(id, _)| id)
    }
}

impl Surf {
    fn new(
        spec: SurfaceSpec,
        wire: Option<layer::Wire>,
        role: Role,
        wl: WlSurface,
        viewport: Option<WpViewport>,
        fractional: Option<WpFractionalScaleV1>,
    ) -> Self {
        Self {
            spec,
            wire,
            role,
            wl,
            viewport,
            fractional,
            scale: 1.0,
            logical: (0, 0),
            configured: false,
            drawable: false,
            // A surface made late (its screen was not there) has missed the
            // request the bookkeeping made for it when it was asked for.
            want_draw: true,
            callback_pending: false,
            misses: 0,
            region: None,
            region_sent: region::Plan::Whole,
        }
    }
}

/// A window's size limits, from its spec: a window that cannot be resized is
/// one whose smallest and largest are its size.
fn apply_window_bounds(window: &Window, spec: &SurfaceSpec) {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "window sizes are small positive logical pixels"
    )]
    let whole = |v: f32| v.max(0.0).round() as u32;
    if spec.resizable {
        window.set_min_size(
            (spec.min_size.0.is_some() || spec.min_size.1.is_some()).then(|| {
                (
                    spec.min_size.0.map_or(1, whole),
                    spec.min_size.1.map_or(1, whole),
                )
            }),
        );
        window.set_max_size(
            (spec.max_size.0.is_some() || spec.max_size.1.is_some()).then(|| {
                (
                    spec.max_size.0.map_or(0, whole),
                    spec.max_size.1.map_or(0, whole),
                )
            }),
        );
    } else {
        let size = (whole(spec.size.0), whole(spec.size.1));
        window.set_min_size(Some(size));
        window.set_max_size(Some(size));
    }
}

/// The shared window bookkeeping's backend: what each of its decisions does
/// to the Wayland objects.
pub(crate) struct Real<'a> {
    shell: &'a mut Shell,
}

impl Backend for Real<'_> {
    fn create(&mut self, id: SurfaceId, spec: &SurfaceSpec) -> Result<(), String> {
        self.shell.make_surface(id, spec)
    }

    fn configure(&mut self, id: SurfaceId, spec: &SurfaceSpec) {
        self.shell.configure_surface(id, spec);
    }

    fn destroy(&mut self, id: SurfaceId) {
        self.shell.destroy_surface(id);
    }

    fn request_redraw(&mut self, id: SurfaceId) {
        if let Some(s) = self.shell.surfs.get_mut(id) {
            s.want_draw = true;
            self.shell.queue(id);
        }
    }

    fn send_close(&mut self, id: SurfaceId) {
        self.shell.host.on_surface_close(id);
    }

    fn exit(&mut self) {
        self.shell.done = true;
    }

    fn input_region(&mut self, id: SurfaceId, region: &InputRegion) {
        if let Some(s) = self.shell.surfs.get_mut(id) {
            s.region = Some(region.clone());
        }
        self.shell.apply_region(id);
    }
}
