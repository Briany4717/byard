//! [`App`], the shipped-application entry point (RFC-0028 §2 registration).
//!
//! An app is a crate whose `main.rs` builds its controllers, hands them to the
//! engine, and points it at the `.byd` entry:
//!
//! ```ignore
//! fn main() -> Result<(), byard::ByardError> {
//!     byard::App::new("src/main.byd")
//!         .title("Weather")
//!         .provide(WeatherApi::new())
//!         .run()
//! }
//! ```
//!
//! ## Why this is not `byard dev`
//!
//! `byard dev` exists to make a *change* visible: it watches the filesystem,
//! re-resolves the module graph on every save, paints an error overlay instead
//! of dying, and spins the event loop continuously so a file change appears
//! without the developer having to move the mouse. Every one of those is a
//! cost a shipped application should not pay, and the last one is a
//! battery-drain bug in a released app.
//!
//! `App` is the other half: read the entry once, open a window, and run the
//! loop in `Wait` mode so a static scene costs exactly zero frames. It is also
//! the only path that can register controllers at all, since the ones a given
//! app provides are Rust types the CLI binary has never heard of.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::ast::ViewDecl;
use byard_core::bridge::{Controller, ControllerRegistry};
use byard_core::frame::{RenderFrame, TargetId};
use byard_core::relay::IoResult;
use byard_core::{
    ByardError, Engine, InputEvent, LogicRuntime, PlatformHost, PointerButton, PointerState,
    WindowSize,
};

/// A Byard application: a `.byd` entry view, the controllers it may `inject`,
/// and the window it runs in.
pub struct App {
    entry: PathBuf,
    /// The stable identity this app's persistent state is filed under
    /// (RFC-0029 O5).
    ///
    /// Deliberately not the window title (presentation, and translated) and
    /// deliberately not the entry file's stem, which is `main` for almost
    /// every app and would file every shipped Byard app's `Store` in one
    /// directory. Defaults to the executable's own name, which is as stable as
    /// the binary itself, and is overridable with [`App::app_id`] for an app
    /// whose binary is renamed or shipped under several names.
    #[allow(
        clippy::struct_field_names,
        reason = "`app_id` is the domain term an app author writes; bare `id` would read as a handle"
    )]
    app_id: String,
    title: String,
    size: (u32, u32),
    registry: ControllerRegistry,
    /// Controllers rejected for taking a reserved capability name (RFC-0029
    /// §7), reported by [`App::run`].
    ///
    /// Collected rather than returned from `provide`, because `provide` is a
    /// builder step and making it fallible would put a `?` in the middle of
    /// every app's `main` for a mistake almost no app makes. The failure still
    /// has to be loud, so it is held here and fails the run.
    reserved: Vec<&'static str>,
    /// Native views refused because their name was already registered
    /// (RFC-0039), reported by [`run`](App::run) for the same reason
    /// [`reserved`](Self::reserved) is: a builder step that returns `Self`
    /// cannot fail, and a name collision is too consequential to drop.
    duplicate_views: Vec<&'static str>,
}

impl App {
    /// Starts an app rooted at the `.byd` entry file `entry`.
    #[must_use]
    pub fn new(entry: impl AsRef<Path>) -> Self {
        let entry = entry.as_ref().to_path_buf();
        let title = entry
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Byard")
            .to_string();
        let app_id = executable_name().unwrap_or_else(|| title.clone());
        Self {
            entry,
            // Seeded with the framework's own capabilities (RFC-0029 §7), so
            // `inject Http as http` works in an app that provided nothing.
            registry: byard_core::cap::default_registry(&app_id),
            app_id,
            title,
            size: (1280, 720),
            reserved: Vec::new(),
            duplicate_views: Vec::new(),
        }
    }

    /// Drops the framework's built-in capabilities, so a view can `inject`
    /// only what the app provides (RFC-0029 §7 opt-out).
    ///
    /// For an app that owns its whole I/O story, or one auditing exactly what
    /// its views can reach. It does **not** free up the reserved names:
    /// [`provide`](Self::provide) still refuses them, so an app replacing the
    /// built-in HTTP stack names its own controller something else. What this
    /// buys is that `inject Http as http` then fails loudly as unresolved,
    /// rather than silently reaching a capability the app meant to remove.
    #[must_use]
    pub fn without_default_capabilities(mut self) -> Self {
        self.registry = ControllerRegistry::new();
        self
    }

    /// Sets the identity this app's persistent state is filed under
    /// (RFC-0029 O5), defaulting to the executable's name.
    ///
    /// Set it when the binary may be renamed, or shipped under more than one
    /// name, and its saved state should follow the *app* rather than the file.
    /// Changing it points the app at a different store, so it is a decision
    /// about data, not about presentation, which is why it is separate from
    /// [`title`](Self::title).
    #[must_use]
    pub fn app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = app_id.into();
        self.registry = byard_core::cap::default_registry(&self.app_id);
        self
    }

    /// Sets the window title (defaults to the entry file's stem).
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Sets the initial window size in logical pixels.
    #[must_use]
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.size = (width, height);
        self
    }

    /// Registers `controller` as an ambient provider, so `inject T as x`
    /// inside the view resolves a handle to it (RFC-0028 §3).
    ///
    /// A controller whose `type_name()` is one of the framework's reserved
    /// capability names (`Http`, `Json`, `Store`, `Timer`, RFC-0029 §7) is
    /// **rejected**, and [`run`](Self::run) fails saying so. Allowing the
    /// shadow would mean `inject Http as http` meaning different things in
    /// different apps, and a `byld` file that reads correctly against the
    /// documentation while doing something else entirely.
    ///
    /// The rejection is unconditional, and in particular
    /// [`without_default_capabilities`](Self::without_default_capabilities)
    /// does **not** lift it: that method controls which built-ins are
    /// *registered*, not which names exist. An app that wants its own HTTP
    /// stack gives it a name of its own (`MyHttp`) and injects that.
    #[must_use]
    pub fn provide<C: Controller + 'static>(mut self, controller: C) -> Self {
        let name = controller.type_name();
        if byard_core::cap::is_reserved(name) {
            self.reserved.push(name);
            return self;
        }
        self.registry.insert(Arc::new(controller));
        self
    }

    /// Registers a native view so `byld` can name it as an element
    /// (RFC-0039).
    ///
    /// ```ignore
    /// byard::App::new("src/main.byd")
    ///     .native_view::<Sparkline>()
    ///     .run()
    /// ```
    ///
    /// The view is compiled into this binary, so the call site in `byld` is a
    /// direct one and the widget draws at intrinsic speed. Registering two
    /// views under one name is refused and [`run`](Self::run) says so: which
    /// widget appears must not depend on the order two `register` calls
    /// happened to run in.
    #[must_use]
    pub fn native_view<V: byard_core::render::NativeViewMeta>(mut self) -> Self {
        if !byard_core::render::registry::register::<V>() {
            self.duplicate_views.push(V::INFO.name);
        }
        self
    }

    /// Opens the window and runs until it closes.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::Platform`] if the entry file cannot be read or
    /// does not parse, and whatever engine/platform error initialisation or
    /// the event loop produces.
    pub fn run(self) -> Result<(), ByardError> {
        if !self.reserved.is_empty() {
            // Deduplicated, because two controllers rejected for the same name
            // is one problem stated twice, and sorted so the message does not
            // depend on registration order.
            let mut reserved = self.reserved.clone();
            reserved.sort_unstable();
            reserved.dedup();
            // The advice is only "rename". An earlier draft also offered
            // `without_default_capabilities()`, which does not help: it clears
            // the built-ins but `provide` still refuses a reserved name, by
            // design, so `inject Http as http` means the same thing in every
            // app. Pointing at a door that does not open is worse than not
            // mentioning one.
            return Err(ByardError::Platform(format!(
                "these controllers use names the framework reserves for its own \
                 capabilities (RFC-0029 §7): {}. A reserved name cannot be provided \
                 by an app, whatever the capability set; give yours a name of its \
                 own (`MyHttp`) and `inject` that.",
                reserved.join(", ")
            )));
        }
        if !self.duplicate_views.is_empty() {
            let mut names = self.duplicate_views.clone();
            names.sort_unstable();
            names.dedup();
            return Err(ByardError::Platform(format!(
                "these native views were registered under a name another view already \
                 has (RFC-0039): {}. An element name resolves to exactly one view, so \
                 rename one of them rather than letting link order decide which widget \
                 the app draws.",
                names.join(", ")
            )));
        }
        let source = std::fs::read_to_string(&self.entry).map_err(|e| {
            ByardError::Platform(format!("cannot read `{}`: {e}", self.entry.display()))
        })?;
        let parsed = byard_compiler::parser::parse(&source);
        if !parsed.errors.is_empty() {
            // A shipped app with a broken view has nothing to fall back on, so
            // it fails at startup with the diagnostics rather than opening a
            // window onto nothing. (`byard dev` is the one that keeps going.)
            let report = parsed
                .errors
                .iter()
                .map(|e| format!("  {}", e.headline()))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(ByardError::Platform(format!(
                "`{}` did not compile:\n{report}",
                self.entry.display()
            )));
        }
        if parsed.views.is_empty() {
            return Err(ByardError::Platform(format!(
                "`{}` declares no `View`",
                self.entry.display()
            )));
        }

        let (width, height) = self.size;
        // Wait mode: a shipped app redraws when something changed, and the
        // frame waker is what tells it that something did, including a
        // controller reply arriving with no input behind it (RFC-0029 §2).
        #[allow(clippy::cast_precision_loss)]
        let (w0, h0) = (width as f32, height as f32);
        byard_platform::WinitHost::new(self.title.clone(), width, height).run(Host {
            engine: None,
            views: parsed.views,
            registry: self.registry,
            width_bits: Arc::new(AtomicU32::new(w0.to_bits())),
            height_bits: Arc::new(AtomicU32::new(h0.to_bits())),
        })
    }
}

/// The `PlatformHost` half: owns the `Engine`, forwards OS input, and starts
/// the logic thread once the surface exists.
struct Host {
    engine: Option<Engine>,
    views: Vec<ViewDecl>,
    registry: ControllerRegistry,
    width_bits: Arc<AtomicU32>,
    height_bits: Arc<AtomicU32>,
}

impl PlatformHost for Host {
    fn on_resume(
        &mut self,
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        size: WindowSize,
        waker: byard_core::relay::FrameWaker,
    ) -> Result<(), ByardError> {
        let mut engine = pollster::block_on(Engine::init(
            instance,
            surface,
            size.width,
            size.height,
            size.scale_factor,
        ))?;
        engine.set_frame_waker(waker);

        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let logical = (
            size.width as f32 / size.scale_factor as f32,
            size.height as f32 / size.scale_factor as f32,
        );
        self.width_bits
            .store(logical.0.to_bits(), Ordering::Relaxed);
        self.height_bits
            .store(logical.1.to_bits(), Ordering::Relaxed);

        // Built before the logic thread starts, because `inject` resolves at
        // lower time: a handle provided after the view was lowered would be
        // invisible to the view that asked for it (RFC-0028 §3).
        let dispatcher = engine.dispatcher(std::mem::take(&mut self.registry));
        let views = std::mem::take(&mut self.views);
        let width_bits = Arc::clone(&self.width_bits);
        let height_bits = Arc::clone(&self.height_bits);

        engine.start_logic_from_view(move |_arena| {
            let mut interp = Interpreter::new();
            interp.set_dispatcher(dispatcher);
            interp.load_views(&views);
            let known: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
            let tree = interp.lower_view(&views[0], &known);
            interp.tick();
            Box::new(AppRuntime {
                interp,
                tree,
                start: std::time::Instant::now(),
                width_bits,
                height_bits,
            })
        })?;

        self.engine = Some(engine);
        Ok(())
    }

    fn on_resize(&mut self, size: WindowSize) {
        if let Some(engine) = self.engine.as_mut() {
            engine.on_resize(size.width, size.height, size.scale_factor);
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let w = size.width as f32 / size.scale_factor as f32;
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let h = size.height as f32 / size.scale_factor as f32;
            self.width_bits.store(w.to_bits(), Ordering::Relaxed);
            self.height_bits.store(h.to_bits(), Ordering::Relaxed);
        }
    }

    fn on_redraw(&mut self) -> Result<(), ByardError> {
        match self.engine.as_mut() {
            Some(engine) => engine.render_latest(),
            None => Ok(()),
        }
    }

    fn on_pointer_input(&mut self, button: PointerButton, state: PointerState, x: f32, y: f32) {
        let kind = match state {
            PointerState::Pressed => byard_core::EventKind::PointerDown,
            PointerState::Released => byard_core::EventKind::PointerUp,
        };
        let payload =
            (button == PointerButton::Right).then_some(byard_core::InputPayload::Bool(true));
        self.push(kind, (x, y), (0.0, 0.0), payload);
    }

    fn on_cursor_moved(&mut self, x: f32, y: f32) {
        self.push(byard_core::EventKind::PointerMove, (x, y), (0.0, 0.0), None);
    }

    fn on_key(&mut self, key: &str, pressed: bool) {
        let kind = if pressed {
            byard_core::EventKind::KeyDown
        } else {
            byard_core::EventKind::KeyUp
        };
        let payload = Some(byard_core::InputPayload::Key(key.to_string()));
        self.push(kind, (0.0, 0.0), (0.0, 0.0), payload);
    }

    fn on_text(&mut self, text: &str) {
        let payload = Some(byard_core::InputPayload::Key(text.to_string()));
        self.push(
            byard_core::EventKind::TextInput,
            (0.0, 0.0),
            (0.0, 0.0),
            payload,
        );
    }

    fn on_scroll(&mut self, dx: f32, dy: f32, x: f32, y: f32) {
        self.push(byard_core::EventKind::Scroll, (x, y), (dx, dy), None);
    }

    fn on_wheel(&mut self, dx: f32, dy: f32, x: f32, y: f32) {
        self.push(byard_core::EventKind::Wheel, (x, y), (dx, dy), None);
    }
}

impl Host {
    /// Forwards one OS event to the logic thread's queue.
    fn push(
        &self,
        kind: byard_core::EventKind,
        pos: (f32, f32),
        delta: (f32, f32),
        payload: Option<byard_core::InputPayload>,
    ) {
        if let Some(engine) = self.engine.as_ref() {
            engine.push_input(InputEvent {
                kind,
                pos,
                delta,
                payload,
                time_ms: now_ms(),
            });
        }
    }
}

/// The running executable's file stem, if the OS will say.
///
/// `None` under a harness that reports no path; the caller falls back to the
/// entry stem, which is worse but never nothing.
fn executable_name() -> Option<String> {
    std::env::current_exe()
        .ok()?
        .file_stem()?
        .to_str()
        .map(str::to_string)
}

/// Milliseconds since the Unix epoch, the clock the router's tap/double-tap
/// thresholds are measured against.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() * 1000 + u64::from(d.subsec_millis()))
}

/// The logic-thread half: dispatch input, tick, render, and apply whatever the
/// async pool completed.
struct AppRuntime {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    start: std::time::Instant,
    width_bits: Arc<AtomicU32>,
    height_bits: Arc<AtomicU32>,
}

impl LogicRuntime for AppRuntime {
    fn evaluate_tick(
        &mut self,
        frame: &mut RenderFrame,
        input_events: &[InputEvent],
        _dirty: &[TargetId],
    ) {
        self.interp.dispatch_events(input_events);
        self.interp.tick();
        let elapsed = u32::try_from(self.start.elapsed().as_millis()).unwrap_or(u32::MAX);
        self.interp.set_now_ms(elapsed);
        let w = f32::from_bits(self.width_bits.load(Ordering::Relaxed));
        let h = f32::from_bits(self.height_bits.load(Ordering::Relaxed));
        self.interp.render(&self.tree, frame, w, h);
    }

    fn apply_io_results(&mut self, results: Vec<IoResult>) -> bool {
        self.interp.apply_io_results(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_identity_does_not_come_from_the_entry_file_name() {
        // Almost every app's entry is `src/main.byd`, so keying persistent
        // state on its stem would file every shipped Byard app's store in one
        // directory called `main`, and two unrelated apps would share their
        // settings. The default is the executable, which is as stable as the
        // binary itself.
        let app = App::new("src/main.byd");
        assert_ne!(app.app_id, "main");
        assert_eq!(
            app.app_id,
            executable_name().expect("a test binary has a path")
        );
    }

    #[test]
    fn the_window_title_and_the_store_identity_are_separate() {
        // A title is presentation and may be translated; a store identity is
        // where data lives. Rewording one must not move the other.
        let app = App::new("src/main.byd").title("Weather, translated");
        assert_eq!(app.title, "Weather, translated");
        assert_ne!(app.app_id, "Weather, translated");
    }

    #[test]
    fn app_id_overrides_the_default_and_rebuilds_the_registry() {
        let app = App::new("src/main.byd").app_id("dev.example.weather");
        assert_eq!(app.app_id, "dev.example.weather");
        assert!(app.registry.contains("Store"));
    }

    #[test]
    fn dropping_the_built_ins_still_does_not_free_up_their_names() {
        // The message used to point at this method as the way to provide your
        // own `Http`. It is not, and a suggestion that does not work is worse
        // than none.
        struct Impostor;
        impl byard_core::bridge::Controller for Impostor {
            fn type_name(&self) -> &'static str {
                "Http"
            }
            fn invoke(
                &self,
                _method: &str,
                _args: Vec<byard_core::bridge::HostValue>,
            ) -> byard_core::bridge::BoxFuture<
                'static,
                Result<byard_core::bridge::HostValue, byard_core::bridge::HostValue>,
            > {
                Box::pin(async { Ok(byard_core::bridge::HostValue::Unit) })
            }
        }
        let app = App::new("src/main.byd")
            .without_default_capabilities()
            .provide(Impostor);
        assert_eq!(app.reserved, vec!["Http"]);
        assert!(
            !app.registry.contains("Http"),
            "the built-in really is gone"
        );
    }

    #[test]
    fn the_reserved_name_report_says_each_name_once() {
        // Two controllers rejected for one name is one problem, and the
        // message should not repeat it.
        struct A;
        struct B;
        macro_rules! http_impostor {
            ($t:ty) => {
                impl byard_core::bridge::Controller for $t {
                    fn type_name(&self) -> &'static str {
                        "Http"
                    }
                    fn invoke(
                        &self,
                        _method: &str,
                        _args: Vec<byard_core::bridge::HostValue>,
                    ) -> byard_core::bridge::BoxFuture<
                        'static,
                        Result<byard_core::bridge::HostValue, byard_core::bridge::HostValue>,
                    > {
                        Box::pin(async { Ok(byard_core::bridge::HostValue::Unit) })
                    }
                }
            };
        }
        http_impostor!(A);
        http_impostor!(B);
        let app = App::new("src/main.byd").provide(A).provide(B);
        let message = app
            .run()
            .expect_err("a reserved name fails the run")
            .to_string();
        // Count within the list itself: the advice that follows names
        // `MyHttp`, which contains `Http` and would make a whole-message count
        // meaningless.
        let list = message
            .split("§7): ")
            .nth(1)
            .and_then(|rest| rest.split('.').next())
            .expect("the message lists the offending names");
        assert_eq!(
            list.matches("Http").count(),
            1,
            "one name, said once: {list}"
        );
        assert!(
            !message.contains("without_default_capabilities"),
            "the message must not offer a door that does not open: {message}"
        );
    }

    #[test]
    fn a_reserved_name_is_rejected_rather_than_shadowing_the_built_in() {
        // RFC-0029 §7. `run()` fails naming it; the check is here because
        // opening a window in a unit test is not an option.
        struct Impostor;
        impl byard_core::bridge::Controller for Impostor {
            fn type_name(&self) -> &'static str {
                "Http"
            }
            fn invoke(
                &self,
                _method: &str,
                _args: Vec<byard_core::bridge::HostValue>,
            ) -> byard_core::bridge::BoxFuture<
                'static,
                Result<byard_core::bridge::HostValue, byard_core::bridge::HostValue>,
            > {
                Box::pin(async { Ok(byard_core::bridge::HostValue::Unit) })
            }
        }
        let app = App::new("src/main.byd").provide(Impostor);
        assert_eq!(app.reserved, vec!["Http"]);
    }
}
