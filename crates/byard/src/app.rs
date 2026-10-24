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
    pub(crate) entry: PathBuf,
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
    pub(crate) size: (u32, u32),
    pub(crate) registry: ControllerRegistry,
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
    /// Whether the running app follows its project's files: see
    /// [`hot_reload`](App::hot_reload).
    hot_reload: bool,
    /// Where to write a trace of every frame: see [`trace`](App::trace).
    trace: Option<PathBuf>,
}

/// The program a running app reloads into, and the watcher that sends it.
type Reloads =
    Arc<byard_compiler::interp::reload::LatestWins<byard_compiler::interp::reload::ParsedFile>>;

impl App {
    /// Starts an app rooted at `entry`: a project (its directory or its
    /// `byard.toml`), or a lone `.byd` file.
    ///
    /// A project runs exactly as `byard dev` runs it: its `[theme]` and
    /// declared fonts, every `.byd` beside the entry, and its packages. A lone
    /// file runs by itself in the built-in theme.
    ///
    /// When `entry` is not there, which is the case for an app built on one
    /// machine and run on another (its `CARGO_MANIFEST_DIR` stays behind),
    /// the project is looked for beside the executable, where `byard bundle`
    /// puts it: `project/` next to it, or `../Resources/project/` inside a
    /// macOS `.app`.
    #[must_use]
    pub fn new(entry: impl AsRef<Path>) -> Self {
        let entry = locate_project(entry.as_ref(), std::env::current_exe().ok().as_deref());
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
            hot_reload: false,
            trace: None,
        }
    }

    /// Writes a trace of every frame to `path`: the logic thread's scopes,
    /// the render thread's and the GPU's passes, as `byard dev --trace`
    /// does. Open it in Perfetto, `chrome://tracing` or speedscope. The file
    /// is valid at every instant, so a run that is killed keeps its trace.
    ///
    /// ```ignore
    /// let trace = std::env::var_os("APP_TRACE");
    /// let app = byard::App::new(env!("CARGO_MANIFEST_DIR"));
    /// match trace { Some(path) => app.trace(path), None => app }.run()
    /// ```
    #[must_use]
    pub fn trace(mut self, path: impl Into<PathBuf>) -> Self {
        self.trace = Some(path.into());
        self
    }

    /// Follows the project's files while the app runs: editing a `.byd`, the
    /// manifest or its theme applies to the open window, as under
    /// `byard dev`, with the app's own Rust (its controllers, native views
    /// and shaders) linked in. A program that stops compiling is reported on
    /// standard error and the last one that compiled keeps running.
    ///
    /// For development: a shipped app has no project beside it to follow.
    ///
    /// ```ignore
    /// let dev = std::env::args().any(|a| a == "--dev");
    /// byard::App::new(env!("CARGO_MANIFEST_DIR")).hot_reload(dev).run()
    /// ```
    #[must_use]
    pub const fn hot_reload(mut self, on: bool) -> Self {
        self.hot_reload = on;
        self
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

    /// Registers a shader of the app's own, so a native view can draw
    /// through it (RFC-0039).
    ///
    /// ```ignore
    /// byard::App::new("src/main.byd")
    ///     .shader::<MapTiles>()
    ///     .native_view::<Map>()
    ///     .run()
    /// ```
    ///
    /// The engine builds the pipeline when the window opens. A shader that
    /// does not compile is reported there, by name.
    #[must_use]
    pub fn shader<S: byard_core::render::Shader>(self) -> Self {
        byard_core::render::register_shader::<S>();
        self
    }

    /// Refuses a set of registrations [`run`](Self::run) could not honour: a
    /// controller under a reserved name, or two native views under one name.
    pub(crate) fn check_registrations(&self) -> Result<(), ByardError> {
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
        Ok(())
    }

    /// Opens the window and runs until it closes.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::Platform`] if the entry file cannot be read or
    /// does not parse, and whatever engine/platform error initialisation or
    /// the event loop produces.
    pub fn run(self) -> Result<(), ByardError> {
        self.check_registrations()?;
        let Program {
            views,
            theme,
            http,
            icon,
        } = load_program(&self.entry)?;
        let reloads = if self.hot_reload {
            Some(self.follow_project()?)
        } else {
            None
        };
        let mut registry = self.registry;
        byard_core::cap::configure_http(&mut registry, http.base_url.as_deref(), &http.hosts);
        let trace = self
            .trace
            .as_deref()
            .map(byard_core::telemetry::trace::TraceWriter::create)
            .transpose()
            .map_err(ByardError::Platform)?;

        let (width, height) = self.size;
        // Wait mode: a shipped app redraws when something changed, and the
        // frame waker is what tells it that something did, including a
        // controller reply arriving with no input behind it (RFC-0029 §2).
        #[allow(clippy::cast_precision_loss)]
        let (w0, h0) = (width as f32, height as f32);
        let mut window = byard_platform::WinitHost::new(self.title.clone(), width, height);
        if let Some((rgba, w, h)) = icon {
            window = window.with_icon(rgba, w, h);
        }
        window.run(Host {
            engine: None,
            views,
            theme: Some(theme),
            registry,
            width_bits: Arc::new(AtomicU32::new(w0.to_bits())),
            height_bits: Arc::new(AtomicU32::new(h0.to_bits())),
            reloads,
            trace,
            mods: byard_core::KeyModifiers::default(),
        })
    }

    /// Starts watching the project, and returns where its re-read program
    /// arrives. The watcher lives as long as the process, as under
    /// `byard dev`.
    fn follow_project(&self) -> Result<Reloads, ByardError> {
        let fail = |e: String| ByardError::Platform(format!("hot reload: {e}"));
        let manifest =
            byard_project::manifest::Manifest::discover(Some(&self.entry)).map_err(fail)?;
        let (_, provider) = byard_project::deps::resolve_project(&manifest).map_err(fail)?;
        let paths = byard_project::live::watch_paths(&manifest, &provider);
        let reloads: Reloads = Arc::default();
        // Images the app draws are decoded by the engine, which a running app
        // already re-reads when they change; only the program is followed here.
        let (assets, _ignored) = crossbeam_channel::unbounded();
        let entry = self.entry.clone();
        let watcher = byard_compiler::interp::reload::start_watcher(
            &paths,
            &manifest.watch_files,
            Arc::clone(&reloads),
            assets,
            move || byard_project::live::reresolve(Some(&entry)),
        )
        .map_err(|e| fail(e.to_string()))?;
        std::mem::forget(watcher);
        Ok(reloads)
    }
}

/// Where the project an app runs is: `entry` when it is there, or else the
/// copy `byard bundle` put beside the executable `exe`.
fn locate_project(entry: &Path, exe: Option<&Path>) -> PathBuf {
    if entry.exists() {
        return entry.to_path_buf();
    }
    let Some(dir) = exe.and_then(Path::parent) else {
        return entry.to_path_buf();
    };
    [dir.join("project"), dir.join("../Resources/project")]
        .into_iter()
        .find(|candidate| candidate.join("byard.toml").is_file())
        .unwrap_or_else(|| entry.to_path_buf())
}

/// What [`load_program`] reads from a project.
pub(crate) struct Program {
    /// The window's icon, decoded, when the project names one.
    pub(crate) icon: Option<(Vec<u8>, u32, u32)>,
    pub(crate) views: Vec<ViewDecl>,
    pub(crate) theme: byard_compiler::interp::theme::Theme,
    /// The `[http]` origins, for the built-in `Http` (RFC-0029).
    pub(crate) http: byard_project::manifest::HttpConfig,
}

/// Reads the program an app runs and the theme it runs in, the same way
/// `byard dev` does (RFC-0008, RFC-0022).
///
/// `entry` is a `.byd` file, or a project: its directory or its `byard.toml`.
/// A project brings its `[theme]`, its declared fonts, every `.byd` beside the
/// entry, and its packages; a lone file brings itself and the built-in theme,
/// exactly as it always did.
///
/// A shipped app with a broken view has nothing to fall back on, so a program
/// that does not compile fails here with its diagnostics rather than opening
/// a window onto nothing. (`byard dev` is the one that keeps going.)
pub(crate) fn load_program(entry: &Path) -> Result<Program, ByardError> {
    let manifest = byard_project::manifest::Manifest::discover(Some(entry))
        .map_err(|e| ByardError::Platform(format!("`{}`: {e}", entry.display())))?;
    let (program, _) = byard_project::deps::resolve_project(&manifest)
        .map_err(|e| ByardError::Platform(format!("`{}`: {e}", entry.display())))?;
    if !program.errors.is_empty() {
        let report = program
            .errors
            .iter()
            .map(|e| format!("  {}", e.headline()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ByardError::Platform(format!(
            "`{}` did not compile:\n{report}",
            entry.display()
        )));
    }
    if program.views.is_empty() {
        return Err(ByardError::Platform(format!(
            "`{}` declares no `View`",
            entry.display()
        )));
    }
    check_declared_views(&manifest.native_views)?;
    let icon = match &manifest.icon {
        Some(path) => {
            let image = image::open(path)
                .map_err(|e| ByardError::Platform(format!("`{}`: {e}", path.display())))?
                .to_rgba8();
            let (w, h) = image.dimensions();
            Some((image.into_raw(), w, h))
        }
        None => None,
    };
    Ok(Program {
        icon,
        views: program.views,
        theme: manifest.theme,
        http: manifest.http,
    })
}

/// The `PlatformHost` half: owns the `Engine`, forwards OS input, and starts
/// the logic thread once the surface exists.
struct Host {
    engine: Option<Engine>,
    views: Vec<ViewDecl>,
    /// The project's theme, fonts included (RFC-0022, RFC-0034): taken by the
    /// logic thread when it starts.
    theme: Option<byard_compiler::interp::theme::Theme>,
    registry: ControllerRegistry,
    width_bits: Arc<AtomicU32>,
    height_bits: Arc<AtomicU32>,
    /// Where a re-read program arrives, when the app follows its project.
    reloads: Option<Reloads>,
    /// The trace every frame is written to, when the app was asked for one.
    trace: Option<byard_core::telemetry::trace::TraceWriter>,
    /// The modifier keys held now, stamped onto the events this host creates
    /// (RFC-0041 §2).
    mods: byard_core::KeyModifiers,
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
        let theme = self.theme.take();
        let width_bits = Arc::clone(&self.width_bits);
        let height_bits = Arc::clone(&self.height_bits);
        let reloads = self.reloads.clone();

        engine.start_logic_from_view(move |_arena| {
            let mut interp = Interpreter::new();
            interp.set_dispatcher(dispatcher);
            // Before lowering, as `byard dev` does: `inject Theme` resolves
            // against the ambient chain at lower time.
            if let Some(theme) = theme {
                interp.set_theme(theme);
            }
            interp.load_views(&views);
            let known: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
            let root = byard_compiler::parser::ast::root_view(&views)
                .expect("a program with no views is rejected before this");
            let tree = interp.lower_view(root, &known);
            // The program resolved, and lowering can still refuse part of it
            // (an attribute an element does not take). A shipped app has no
            // overlay to show that in, so it says it where it will be read.
            for error in interp.errors() {
                eprintln!("  {}", error.headline());
            }
            let errors_told = interp.errors().len();
            interp.tick();
            Box::new(AppRuntime {
                interp,
                tree,
                start: std::time::Instant::now(),
                width_bits,
                height_bits,
                views,
                reloads,
                reloaded: std::cell::Cell::new(false),
                errors_told,
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
        let Some(engine) = self.engine.as_mut() else {
            return Ok(());
        };
        engine.render_latest()?;
        if let Some(trace) = self.trace.as_mut() {
            let render = byard_core::telemetry::drain_samples();
            trace.write_frame(engine.latest_cpu_telemetry().as_ref(), &render);
        }
        Ok(())
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

    fn on_system_scheme(&mut self, dark: bool) {
        self.push(
            byard_core::EventKind::SystemScheme,
            (0.0, 0.0),
            (0.0, 0.0),
            Some(byard_core::InputPayload::Bool(dark)),
        );
    }

    fn on_cursor_moved(&mut self, x: f32, y: f32) {
        self.push(byard_core::EventKind::PointerMove, (x, y), (0.0, 0.0), None);
    }

    fn on_modifiers(&mut self, mods: byard_core::KeyModifiers) {
        self.mods = mods;
    }

    fn on_window_focus(&mut self, focused: bool) {
        self.push(
            byard_core::EventKind::WindowFocus,
            (0.0, 0.0),
            (0.0, 0.0),
            Some(byard_core::InputPayload::Bool(focused)),
        );
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

    fn on_ime(&mut self, event: byard_core::ImeEvent) {
        if let (Some(engine), Some(input)) = (self.engine.as_ref(), event.into_input(now_ms())) {
            engine.push_input(input);
        }
    }

    fn text_input(&self) -> Option<byard_core::frame::TextInputState> {
        self.engine.as_ref().and_then(Engine::text_input)
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
                mods: self.mods,
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
    /// The program running now, which a reload is diffed against.
    views: Vec<ViewDecl>,
    /// Where a re-read program arrives, when the app follows its project.
    reloads: Option<Reloads>,
    /// Whether a reload changed the frame since it was last asked for, with
    /// no input behind it to ask for the next one.
    reloaded: std::cell::Cell<bool>,
    /// How many of the interpreter's errors have been printed.
    errors_told: usize,
}

impl AppRuntime {
    /// Prints what the program reported since the last frame: a reply that
    /// did not match its record type, a reload's lowering errors. A shipped
    /// app has no overlay, so standard error is where it can be read.
    fn tell_new_errors(&mut self) {
        let errors = self.interp.errors();
        for error in errors.iter().skip(self.errors_told) {
            eprintln!("  {}", error.headline());
        }
        self.errors_told = errors.len();
    }

    /// Applies a re-read program, if one arrived: the theme if it changed,
    /// then the views. A program that does not compile is reported and the
    /// running one is kept.
    fn follow(&mut self) {
        let Some(parsed) = self.reloads.as_ref().and_then(|r| r.take()) else {
            return;
        };
        if !parsed.errors.is_empty() {
            eprintln!("  the edit does not compile; the app keeps the last program that did:");
            for error in &parsed.errors {
                eprintln!("    {}", error.headline());
            }
            return;
        }
        if let Some(mut theme) = parsed.theme.filter(|t| !t.same_tokens(&self.interp.theme)) {
            theme.active_dark = self.interp.theme.active_dark;
            self.interp.set_theme(theme);
        }
        if let Some(tree) = byard_compiler::interp::reload::apply_program(
            &mut self.interp,
            &self.views,
            &parsed.views,
        ) {
            self.tree = tree;
        }
        self.views = parsed.views;
        self.reloaded.set(true);
    }
}

impl LogicRuntime for AppRuntime {
    fn evaluate_tick(
        &mut self,
        frame: &mut RenderFrame,
        input_events: &[InputEvent],
        _dirty: &[TargetId],
    ) {
        self.follow();
        self.interp.dispatch_events(input_events);
        self.interp.tick();
        let elapsed = u32::try_from(self.start.elapsed().as_millis()).unwrap_or(u32::MAX);
        self.interp.set_now_ms(elapsed);
        let w = f32::from_bits(self.width_bits.load(Ordering::Relaxed));
        let h = f32::from_bits(self.height_bits.load(Ordering::Relaxed));
        self.interp.render(&self.tree, frame, w, h);
        self.tell_new_errors();
    }

    fn apply_io_results(&mut self, results: Vec<IoResult>) -> bool {
        self.interp.apply_io_results(results)
    }

    fn wants_another_frame(&self) -> bool {
        self.interp.needs_another_frame() || self.reloaded.take()
    }
}

/// Refuses a manifest whose `[native_views]` describes an element differently
/// from the view the app registered under that name.
///
/// The declaration is what `byard check` and the editor go by, and the
/// registered view is what runs. If the two disagree, a program that checks
/// clean can fail in the app, or the other way round, so the app says which
/// property is off instead of starting.
fn check_declared_views(
    declared: &[byard_project::manifest::NativeViewDecl],
) -> Result<(), ByardError> {
    use byard_core::render::registry;
    for view in declared {
        let Some(info) = registry::info(&view.name) else {
            continue;
        };
        let registered: Vec<registry::DeclaredProp> = info
            .props
            .iter()
            .map(|p| registry::DeclaredProp {
                name: p.name.to_string(),
                ty: p.ty,
                layout: p.layout,
            })
            .collect();
        let describe = |props: &[registry::DeclaredProp]| {
            let mut all: Vec<String> = props
                .iter()
                .map(|p| {
                    format!(
                        "{}: {:?}{}",
                        p.name,
                        p.ty,
                        if p.layout { " (layout)" } else { "" }
                    )
                })
                .collect();
            all.sort();
            all
        };
        let (written, real) = (describe(&view.props), describe(&registered));
        if written != real {
            return Err(ByardError::Platform(format!(
                "byard.toml declares `{}` with the properties [{}], and the view registered \
                 under that name has [{}]. `[native_views.{}]` is what `byard check` and the \
                 editor go by, so it has to say what the view takes.",
                view.name,
                written.join(", "),
                real.join(", "),
                view.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project's icon is read and decoded when the app loads it; Aura
    /// Weather names one.
    #[test]
    fn a_projects_icon_is_decoded() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/aura_weather");
        let program = load_program(&dir).unwrap();
        let (rgba, w, h) = program.icon.expect("Aura Weather names an icon");
        assert_eq!((w, h), (256, 256));
        assert_eq!(rgba.len(), 256 * 256 * 4);
    }

    /// A built app run where its sources are not finds the project
    /// `byard bundle` put beside its executable.
    #[test]
    fn the_project_is_found_beside_the_executable_when_its_path_is_gone() {
        let root = std::env::temp_dir().join(format!("byard-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let flat = root.join("flat");
        std::fs::create_dir_all(flat.join("project")).unwrap();
        std::fs::write(flat.join("project/byard.toml"), "[project]\nname = \"x\"\n").unwrap();
        let mac = root.join("X.app/Contents");
        std::fs::create_dir_all(mac.join("MacOS")).unwrap();
        std::fs::create_dir_all(mac.join("Resources/project")).unwrap();
        std::fs::write(
            mac.join("Resources/project/byard.toml"),
            "[project]\nname = \"x\"\n",
        )
        .unwrap();
        let gone = Path::new("/a/build/machine/that/is/not/here");

        let found = locate_project(gone, Some(&flat.join("app")));
        assert_eq!(found, flat.join("project"));
        let found = locate_project(gone, Some(&mac.join("MacOS/app")));
        assert!(found.join("byard.toml").is_file(), "{}", found.display());
        // Where the sources are, they win.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(locate_project(here, Some(&flat.join("app"))), here);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project in a fresh directory: `main.byd` holding `body`.
    fn hot_project(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("byard-hot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("byard.toml"),
            "[project]\nname = \"hot\"\nentry = \"src/main.byd\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/main.byd"), body).unwrap();
        dir
    }

    /// The app's logic runtime over `dir`, as `run` builds it, following a
    /// channel the test fills.
    fn runtime(dir: &Path, reloads: &Reloads) -> AppRuntime {
        let Program { views, theme, .. } = load_program(dir).unwrap();
        let mut interp = Interpreter::new();
        interp.set_theme(theme);
        interp.load_views(&views);
        let known: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        let root = byard_compiler::parser::ast::root_view(&views).unwrap();
        let tree = interp.lower_view(root, &known);
        AppRuntime {
            interp,
            tree,
            start: std::time::Instant::now(),
            width_bits: Arc::new(AtomicU32::new(400.0_f32.to_bits())),
            height_bits: Arc::new(AtomicU32::new(300.0_f32.to_bits())),
            views,
            reloads: Some(Arc::clone(reloads)),
            reloaded: std::cell::Cell::new(false),
            errors_told: 0,
        }
    }

    fn texts(app: &mut AppRuntime) -> Vec<String> {
        let mut frame = RenderFrame::new();
        app.evaluate_tick(&mut frame, &[], &[]);
        frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    /// An edit to a view reaches the running app, and asks for the frame that
    /// shows it.
    #[test]
    fn an_edit_reaches_the_running_app() {
        let dir = hot_project("edit", "View Main() { Text(\"before\") }");
        let reloads: Reloads = Arc::default();
        let mut app = runtime(&dir, &reloads);
        assert_eq!(texts(&mut app), ["before"]);
        assert!(!app.wants_another_frame());

        std::fs::write(dir.join("src/main.byd"), "View Main() { Text(\"after\") }").unwrap();
        reloads.publish(byard_project::live::reresolve(Some(&dir)));
        assert_eq!(texts(&mut app), ["after"]);
        assert!(app.wants_another_frame(), "the reload asks for its frame");
        assert!(!app.wants_another_frame(), "once");
    }

    /// An edit that does not compile leaves the running program alone.
    #[test]
    fn an_edit_that_does_not_compile_keeps_the_last_good_program() {
        let dir = hot_project("broken", "View Main() { Text(\"good\") }");
        let reloads: Reloads = Arc::default();
        let mut app = runtime(&dir, &reloads);
        std::fs::write(dir.join("src/main.byd"), "View Main() { Text(\"good\" }").unwrap();
        reloads.publish(byard_project::live::reresolve(Some(&dir)));
        assert_eq!(texts(&mut app), ["good"]);
    }

    /// State survives a reload that only changes how it is shown.
    #[test]
    fn state_survives_an_edit_to_how_it_is_shown() {
        let dir = hot_project(
            "state",
            "View Main() { var n: Int = 0\n Button(\"add\") => n++\n Text(\"n {n}\") }",
        );
        let reloads: Reloads = Arc::default();
        let mut app = runtime(&dir, &reloads);
        assert!(texts(&mut app).contains(&"n 0".to_string()));
        app.interp.dispatch_events(&[]);
        let n = app
            .interp
            .var_signal(&byard_compiler::Symbol::intern("n"))
            .unwrap();
        app.interp
            .write_var(n, byard_compiler::interp::env::Value::Int(5));
        assert!(texts(&mut app).contains(&"n 5".to_string()));

        std::fs::write(
            dir.join("src/main.byd"),
            "View Main() { var n: Int = 0\n Button(\"add\") => n++\n Text(\"count {n}\") }",
        )
        .unwrap();
        reloads.publish(byard_project::live::reresolve(Some(&dir)));
        assert!(
            texts(&mut app).contains(&"count 5".to_string()),
            "{:?}",
            texts(&mut app)
        );
    }

    /// The watcher an app starts sends the re-read program when a file
    /// changes.
    #[test]
    fn the_app_watches_its_project() {
        let dir = hot_project("watch", "View Main() { Text(\"one\") }");
        let app = App::new(&dir).hot_reload(true);
        let reloads = app.follow_project().unwrap();
        // Give the watcher a moment to register before the edit.
        std::thread::sleep(std::time::Duration::from_millis(200));
        std::fs::write(dir.join("src/main.byd"), "View Main() { Text(\"two\") }").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let parsed = loop {
            if let Some(parsed) = reloads.take() {
                break parsed;
            }
            assert!(std::time::Instant::now() < deadline, "no reload within 5 s");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(parsed.views.len(), 1);
    }

    /// A declaration that has drifted from the view registered under its name
    /// stops the app, naming both sides.
    #[test]
    fn a_declaration_that_does_not_match_its_view_is_refused() {
        use byard_core::render::registry::{self, DeclaredProp, NativePropType};
        use byard_project::manifest::NativeViewDecl;

        // What the app registered, standing in for a `#[native_view]` type.
        registry::declare(
            "DriftGauge",
            &[DeclaredProp {
                name: "value".into(),
                ty: NativePropType::Float,
                layout: false,
            }],
        );
        let declared = |props: Vec<DeclaredProp>| {
            vec![NativeViewDecl {
                name: "DriftGauge".into(),
                props,
            }]
        };
        let prop = |name: &str, ty| DeclaredProp {
            name: name.into(),
            ty,
            layout: false,
        };

        assert!(
            check_declared_views(&declared(vec![prop("value", NativePropType::Float)])).is_ok()
        );
        let renamed = check_declared_views(&declared(vec![prop("level", NativePropType::Float)]))
            .unwrap_err()
            .to_string();
        assert!(
            renamed.contains("DriftGauge")
                && renamed.contains("level")
                && renamed.contains("value"),
            "{renamed}"
        );
        let retyped = check_declared_views(&declared(vec![prop("value", NativePropType::Color)]));
        assert!(retyped.is_err(), "a different type is a different property");
        // A name nothing registered is not this check's business.
        let other = vec![NativeViewDecl {
            name: "NobodyRegisteredThis".into(),
            props: Vec::new(),
        }];
        assert!(check_declared_views(&other).is_ok());
    }

    /// A shipped app reads its project as `byard dev` does: the theme's
    /// tokens and declared fonts arrive, and a view in a sibling file is part
    /// of the program. It used to read the entry file alone, so a project's
    /// theme, fonts, other files and packages were all silently absent from
    /// the shipped build.
    #[test]
    fn a_project_brings_its_theme_fonts_and_sibling_views() {
        let dir = std::env::temp_dir().join(format!("byard-app-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fonts")).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../byard-cli/examples/assets/fonts/Manrope-Variable.ttf"),
            dir.join("fonts/Body.ttf"),
        )
        .unwrap();
        std::fs::write(
            dir.join("byard.toml"),
            "[project]\nname = \"shipped\"\nentry = \"main.byd\"\n\
             [assets.fonts]\nBody = \"fonts/Body.ttf\"\n\
             [theme.color.light]\nprimary = \"#123456\"\n\
             [http]\nbase_url = \"https://api.example.com\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("main.byd"), "View Main() { Card() }\n").unwrap();
        std::fs::write(
            dir.join("card.byd"),
            "View Card() { Text(\"card\") #[font: \"Body\"] }\n",
        )
        .unwrap();

        let Program {
            views, theme, http, ..
        } = load_program(&dir).expect("the project loads");
        assert_eq!(http.base_url.as_deref(), Some("https://api.example.com"));
        let names: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"Card"), "the sibling view: {names:?}");
        assert_eq!(theme.color("primary", false), Some(0x0012_3456));
        assert!(theme.font("Body").is_some(), "the declared font");

        // A lone file still runs by itself, in the built-in theme.
        let Program { views, theme, .. } =
            load_program(&dir.join("card.byd")).expect("a lone file loads");
        assert_eq!(views.len(), 1);
        assert!(theme.font("Body").is_none());

        // And a program that does not compile fails with its diagnostics.
        std::fs::write(dir.join("main.byd"), "View Main( {").unwrap();
        let Err(err) = load_program(&dir) else {
            panic!("a broken program must not load");
        };
        let err = err.to_string();
        assert!(err.contains("did not compile"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
