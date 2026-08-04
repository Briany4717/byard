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
        Self {
            entry,
            // Seeded with the framework's own capabilities (RFC-0029 §7), so
            // `inject Http as http` works in an app that provided nothing.
            // Keyed on the entry's stem rather than the window title, which is
            // presentation and may be translated: a store must not move
            // because someone reworded a title bar.
            registry: byard_core::cap::default_registry(&title),
            title,
            size: (1280, 720),
            reserved: Vec::new(),
        }
    }

    /// Drops the framework's built-in capabilities, so the app provides
    /// everything itself (RFC-0029 §7 opt-out).
    ///
    /// For an app that owns its whole I/O story, or one auditing exactly what
    /// its views can reach.
    #[must_use]
    pub fn without_default_capabilities(mut self) -> Self {
        self.registry = ControllerRegistry::new();
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
    /// documentation while doing something else entirely. An app that wants
    /// its own stack calls
    /// [`without_default_capabilities`](Self::without_default_capabilities)
    /// and names it something of its own.
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

    /// Opens the window and runs until it closes.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::Platform`] if the entry file cannot be read or
    /// does not parse, and whatever engine/platform error initialisation or
    /// the event loop produces.
    pub fn run(self) -> Result<(), ByardError> {
        if !self.reserved.is_empty() {
            return Err(ByardError::Platform(format!(
                "these controllers use names the framework reserves for its own \
                 capabilities (RFC-0029): {}. Rename them, or call \
                 `without_default_capabilities()` if you mean to replace the built-ins.",
                self.reserved.join(", ")
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
