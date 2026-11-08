//! `byard shot`: render a project to a PNG, headlessly.
//!
//! The same program `byard dev` and a shipped `App` run (the manifest, the
//! theme and its fonts, every `.byd`, the packages), through the same
//! interpreter and the same GPU encoder, drawn into an offscreen texture
//! instead of a window. Nothing appears on screen, so nothing else on the
//! screen can cover it, and no stray click can change it.
//!
//! This exists because looking at the result is the only check that catches
//! a wrong picture: every text-rendering defect fixed alongside it passed its
//! tests and was obvious in a screenshot. A screenshot of a live window is
//! the right tool when a person is watching; this is the one that works when
//! the machine is busy, and the one a script can repeat exactly.
//!
//! It can also press keys and type (`--key`, `--type`), in order with `--tap`,
//! so a text field's caret and selection can be drawn without a window
//! (RFC-0041 §7).

use std::path::Path;
use std::sync::Arc;

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::ast::ViewDecl;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use byard_core::platform::{EventKind, InputEvent};

use crate::deps::resolve_project;
use crate::manifest::Manifest;
use crate::style;

/// A key to press: its name as the platform reports it, and the modifiers held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// `ArrowLeft`, `Backspace`, `a`.
    pub name: String,
    /// What is held while it is pressed.
    pub mods: byard_core::KeyModifiers,
}

/// One thing `byard shot` does before the picture (RFC-0041 §7).
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Tap a logical point.
    Tap(f32, f32),
    /// Press at a logical point and keep the button down (RFC-0056).
    Hold(f32, f32),
    /// Move the pointer to a logical point, pressing nothing (RFC-0056).
    Hover(f32, f32),
    /// Press a key.
    Key(Key),
    /// Type text.
    Type(String),
    /// Let this many milliseconds of app time pass with no input (RFC-0058).
    Wait(u32),
}

impl Step {
    /// The input this step is, as one frame's events at time `now`.
    fn events(&self, now: u32) -> Vec<InputEvent> {
        let time_ms = u64::from(now);
        match self {
            Self::Wait(_) => Vec::new(),
            Self::Tap(x, y) => [EventKind::PointerDown, EventKind::PointerUp]
                .into_iter()
                .map(|kind| InputEvent {
                    kind,
                    pos: (*x, *y),
                    delta: (0.0, 0.0),
                    payload: None,
                    time_ms,
                    mods: byard_core::KeyModifiers::default(),
                })
                .collect(),
            // A pointer moves before it presses, and hover is driven by the
            // move; the button is never released, so the picture is a press
            // in progress: the morph, the ripple and `on pressed` are live.
            Self::Hold(x, y) => [EventKind::PointerMove, EventKind::PointerDown]
                .into_iter()
                .map(|kind| pointer_event(kind, *x, *y, time_ms))
                .collect(),
            Self::Hover(x, y) => vec![pointer_event(EventKind::PointerMove, *x, *y, time_ms)],
            Self::Key(Key { name, mods }) => [EventKind::KeyDown, EventKind::KeyUp]
                .into_iter()
                .map(|kind| InputEvent {
                    kind,
                    pos: (0.0, 0.0),
                    delta: (0.0, 0.0),
                    payload: Some(byard_core::platform::InputPayload::Key(name.clone())),
                    time_ms,
                    mods: *mods,
                })
                .collect(),
            // One event per character, all in the same frame, as a keyboard
            // delivers a burst of typing: it is one run to undo.
            Self::Type(text) => text
                .chars()
                .map(|c| InputEvent {
                    kind: EventKind::TextInput,
                    pos: (0.0, 0.0),
                    delta: (0.0, 0.0),
                    payload: Some(byard_core::platform::InputPayload::Key(c.to_string())),
                    time_ms,
                    mods: byard_core::KeyModifiers::default(),
                })
                .collect(),
        }
    }
}

/// A pointer event at a logical point, with no payload or modifiers.
fn pointer_event(kind: EventKind, x: f32, y: f32, time_ms: u64) -> InputEvent {
    InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms,
        mods: byard_core::KeyModifiers::default(),
    }
}

/// What `byard shot` was asked to render.
pub struct ShotArgs<'a> {
    /// The project directory, `byard.toml` or `.byd`; the current project
    /// when absent.
    pub path: Option<&'a Path>,
    /// The view to draw instead of the root (RFC-0058).
    pub view: Option<&'a str>,
    /// Where the PNG goes, or `None` to draw none (a run that only wants the
    /// accessibility tree needs no GPU).
    pub out: Option<&'a Path>,
    /// Where to write the accessibility tree as indented text (RFC-0042 §6),
    /// built from the last frame as an assistive technology would be given it.
    pub a11y: Option<&'a Path>,
    /// Logical size of the viewport.
    pub size: (u32, u32),
    /// Device pixels per logical pixel.
    pub scale: f32,
    /// Milliseconds of app time to run before the picture is taken, so
    /// entrance animations and transitions have settled.
    pub at_ms: u32,
    /// The wall clock `now()` reads, fixed at this ISO time (RFC-0049), or
    /// `None` for the system's. `at_ms` advances it.
    pub wall_clock: Option<&'a str>,
    /// Start in the dark scheme (`Some(true)`), the light one, or whichever
    /// the manifest says (`None`).
    pub dark: Option<bool>,
    /// What to do before the picture is taken, in order: taps, key presses
    /// and typed text (RFC-0041 §7), one frame each.
    pub steps: &'a [Step],
}

/// A project read and checked, ready to be run any number of times.
pub struct Project {
    manifest: Manifest,
    program: byard_compiler::resolve::ResolvedProgram,
}

impl Project {
    /// Reads the project at `path` (the current one when absent) and runs the
    /// same check as `byard check` over every view and branch, not just the
    /// root's first frame: a picture of a program `check` rejects shows
    /// something the program does not mean (an attribute that failed to type
    /// is dropped and the box is drawn without it). Warnings print and do not
    /// stop the run, as in `check`. `what` is the verb for the refusal of a
    /// library: "render", "test".
    pub fn load(path: Option<&Path>, what: &str) -> Result<Self, String> {
        let manifest = Manifest::discover(path).map_err(|e| e.to_string())?;
        manifest.require_app(what).map_err(|e| e.to_string())?;
        let (program, _) = resolve_project(&manifest).map_err(|e| e.to_string())?;
        let diagnostics =
            crate::commands::check::check_program_with_theme(&program, manifest.theme.clone());
        if !diagnostics.is_empty() {
            crate::commands::check::print_diagnostics(&diagnostics, &program.source_map, false);
        }
        let n = diagnostics.iter().filter(|d| !d.is_warning()).count();
        if n > 0 {
            style::err(&format!(
                "{n} error{}; {} (`byard check` rejects this program)",
                if n == 1 { "" } else { "s" },
                if what == "render" {
                    "no picture drawn"
                } else {
                    "no test run"
                }
            ));
            return Err(String::new());
        }
        if byard_compiler::parser::ast::root_view(&program.views).is_none() {
            return Err("the program declares no `View`".into());
        }
        Ok(Self { manifest, program })
    }

    /// The directory of the project, where `tests/` is.
    pub fn root(&self) -> &Path {
        &self.manifest.project_root
    }

    /// The view to draw: the one named, or the root. A name that is not a
    /// view, or a view that needs arguments, is an error that says what to do
    /// (RFC-0058).
    pub fn view(&self, name: Option<&str>) -> Result<&ViewDecl, String> {
        pick_view(&self.program.views, name)
    }
}

/// The view `name` names among `views`, or the root when there is no name.
pub fn pick_view<'a>(views: &'a [ViewDecl], name: Option<&str>) -> Result<&'a ViewDecl, String> {
    let Some(name) = name else {
        return Ok(byard_compiler::parser::ast::root_view(views).expect("a program with a view"));
    };
    let Some(view) = views.iter().find(|v| v.name.as_str() == name) else {
        let mut near: Vec<(usize, &str)> = views
            .iter()
            .map(|v| v.name.as_str())
            .map(|n| (byard_compiler::util::levenshtein(name, n), n))
            .collect();
        near.sort_unstable();
        near.dedup();
        let near: Vec<&str> = near.into_iter().take(5).map(|(_, n)| n).collect();
        return Err(format!(
            "no view `{name}` in this project; closest: {}",
            near.join(", ")
        ));
    };
    let required: Vec<&str> = view
        .params
        .iter()
        .filter(|p| p.default.is_none())
        .map(|p| p.name.as_str())
        .collect();
    if !required.is_empty() {
        return Err(format!(
            "view `{name}` has required parameters ({}), so it cannot be drawn on its own; \
             give them defaults or draw a view that has none",
            required.join(", ")
        ));
    }
    Ok(view)
}

/// A program running headlessly, one frame at a time: what `byard shot` and
/// `byard test` both drive.
pub struct Scene {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    frame: RenderFrame,
    now: u32,
    size: (f32, f32),
    access: bool,
}

/// What a [`Scene`] starts as.
pub struct SceneOptions<'a> {
    /// Logical size of the viewport.
    pub size: (u32, u32),
    /// The dark scheme, the light one, or the manifest's.
    pub dark: Option<bool>,
    /// The fixed wall clock, if any.
    pub wall_clock: Option<&'a str>,
    /// Build the accessibility tree on every frame.
    pub access: bool,
}

impl Scene {
    /// Starts `view` of `project` and runs its first frame.
    pub fn new(
        project: &Project,
        view: &ViewDecl,
        options: &SceneOptions<'_>,
    ) -> Result<Self, String> {
        let mut interp = Interpreter::new();
        if let Some(iso) = options.wall_clock {
            interp
                .set_wall_clock(iso)
                .map_err(|e| format!("now: {e}"))?;
        }
        // As `check` does: the framework's own capabilities resolve, so a view
        // that injects `Http` renders its not-yet-loaded state instead of failing.
        interp.declare_controllers(&byard_core::cap::provided_names());
        interp.set_theme(project.manifest.theme.clone());
        if let Some(dark) = options.dark {
            interp.set_theme_dark(dark);
        }
        interp.load_views(&project.program.views);
        let known: Vec<&str> = project
            .program
            .views
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        let tree = interp.lower_view(view, &known);
        #[allow(clippy::cast_precision_loss)]
        let size = (options.size.0 as f32, options.size.1 as f32);
        let mut scene = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            now: 0,
            size,
            access: options.access,
        };
        scene.frame_with(&[]);
        Ok(scene)
    }

    /// One frame: the events, a tick, a full render. 16 ms pass.
    fn frame_with(&mut self, events: &[InputEvent]) {
        self.interp.set_now_ms(self.now);
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame = RenderFrame::new();
        self.frame.request_full_redraw();
        // Asked for like the platform asks: by the frame's flag.
        self.frame.set_access_wanted(self.access);
        self.interp
            .render(&self.tree, &mut self.frame, self.size.0, self.size.1);
        self.now += 16;
    }

    /// Does one step: one frame, or for a wait, as many as the time takes.
    pub fn apply(&mut self, what: &Step) {
        if let Step::Wait(ms) = what {
            let until = self.now + ms;
            while self.now < until {
                self.frame_with(&[]);
            }
            return;
        }
        let events = what.events(self.now);
        self.frame_with(&events);
    }

    /// Runs frames until `at_ms` of app time has passed, then once more at
    /// rest, so a mesh or an image that landed a frame late is in.
    pub fn settle(&mut self, at_ms: u32) {
        while self.now < at_ms {
            self.frame_with(&[]);
        }
        self.frame_with(&[]);
    }

    /// The last frame.
    pub fn frame(&self) -> &RenderFrame {
        &self.frame
    }

    /// The accessibility tree of the last frame.
    pub fn access(&self) -> Option<&byard_core::access::AccessTree> {
        self.frame.access()
    }
}

pub fn run(args: &ShotArgs<'_>) -> Result<(), String> {
    let started = std::time::Instant::now();
    let project = Project::load(args.path, "render")?;
    let view = project.view(args.view)?;
    let mut scene = Scene::new(
        &project,
        view,
        &SceneOptions {
            size: args.size,
            dark: args.dark,
            wall_clock: args.wall_clock,
            access: args.a11y.is_some(),
        },
    )
    .map_err(|e| format!("--{e}"))?;
    for what in args.steps {
        scene.apply(what);
    }
    scene.settle(args.at_ms);
    let frame = scene.frame();
    #[allow(clippy::cast_precision_loss)]
    let (w, h) = (args.size.0 as f32, args.size.1 as f32);

    if let Some(path) = args.a11y {
        let tree = frame
            .access()
            .ok_or("the last frame carried no accessibility tree")?;
        std::fs::write(path, tree.to_text()).map_err(|e| format!("{}: {e}", path.display()))?;
        style::ok(
            &format!("{} ({} nodes)", path.display(), tree.nodes.len()),
            Some(started.elapsed()),
        );
    }
    let Some(out) = args.out else {
        return Ok(());
    };
    let pixels = Gpu::open()?.paint(frame, args.size, args.scale)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (pw, ph) = (
        (w * args.scale).round() as u32,
        (h * args.scale).round() as u32,
    );
    image::save_buffer(out, &pixels, pw, ph, image::ColorType::Rgba8)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    style::ok(
        &format!("{} ({pw}x{ph})", out.display()),
        Some(started.elapsed()),
    );
    Ok(())
}

/// A GPU device to draw frames on, opened once and used for any number of
/// pictures.
pub struct Gpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

impl Gpu {
    /// Opens the default adapter. `Err` says there is none.
    pub fn open() -> Result<Self, String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("byard shot"),
            required_features: wgpu::Features::empty(),
            required_limits: byard_core::engine::device_limits(&adapter),
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU device: {e}"))?;
        Ok(Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
        })
    }

    /// Draws `frame` offscreen and reads it back as RGBA8 (sRGB) rows.
    pub fn paint(
        &self,
        frame: &RenderFrame,
        size: (u32, u32),
        scale: f32,
    ) -> Result<Vec<u8>, String> {
        let (device, queue) = (&self.device, &self.queue);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let (pw, ph) = (
            (size.0 as f32 * scale).round() as u32,
            (size.1 as f32 * scale).round() as u32,
        );
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut enc = pollster::block_on(EncoderSubsystem::init(
            Arc::clone(device),
            Arc::clone(queue),
            format,
            scale,
            pw,
            ph,
        ))
        .map_err(|e| e.to_string())?;
        #[allow(clippy::cast_precision_loss)]
        enc.update_viewport(
            Viewport {
                width: size.0 as f32,
                height: size.1 as f32,
            },
            pw,
            ph,
            scale,
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("byard shot target"),
            size: wgpu::Extent3d {
                width: pw,
                height: ph,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let cmd = enc
            .encode_frame_from_relay(&target, frame)
            .map_err(|e| e.to_string())?;
        queue.submit(std::iter::once(cmd));

        let mut pixels = read_back(device, queue, &target, pw, ph);
        // The window's surface is opaque, so wherever the app drew nothing it
        // shows black. The offscreen target keeps alpha, which a PNG viewer would
        // show as white or a checkerboard; flattening onto black makes the shot
        // the picture the window shows. The colour is premultiplied, so black is
        // already underneath and only the alpha changes.
        for alpha in pixels.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        Ok(pixels)
    }
}

/// Copies `target` out of the GPU as tightly packed RGBA8 rows.
fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    pw: u32,
    ph: u32,
) -> Vec<u8> {
    let bpr = 256 * (pw * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("byard shot readback"),
        size: u64::from(bpr) * u64::from(ph),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut ce = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    ce.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(ph),
            },
        },
        wgpu::Extent3d {
            width: pw,
            height: ph,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let row = (pw * 4) as usize;
    let mut out = vec![0u8; row * ph as usize];
    for y in 0..ph as usize {
        let src = y * bpr as usize;
        out[y * row..(y + 1) * row].copy_from_slice(&data[src..src + row]);
    }
    out
}
