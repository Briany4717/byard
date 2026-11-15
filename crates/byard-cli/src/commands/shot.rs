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
    /// Let go of the pointer at a logical point (RFC-0063).
    Release(f32, f32),
    /// Move the pointer to a logical point, pressing nothing (RFC-0056).
    Hover(f32, f32),
    /// Press at the first point, move to the second in [`DRAG_MOVES`] steps,
    /// a frame each, and let go there: a swipe, a scroll, a sheet pulled down.
    Drag((f32, f32), (f32, f32)),
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
                    surface: byard_core::SurfaceId::MAIN,
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
            Self::Release(x, y) => vec![pointer_event(EventKind::PointerUp, *x, *y, time_ms)],
            Self::Hover(x, y) => vec![pointer_event(EventKind::PointerMove, *x, *y, time_ms)],
            // Only the press: the moves and the release are frames of their
            // own (`Scene::apply`), because the router joins the moves of one
            // frame into one and a drag would arrive as a jump.
            Self::Drag((x0, y0), _) => [EventKind::PointerMove, EventKind::PointerDown]
                .into_iter()
                .map(|kind| pointer_event(kind, *x0, *y0, time_ms))
                .collect(),
            Self::Key(Key { name, mods }) => [EventKind::KeyDown, EventKind::KeyUp]
                .into_iter()
                .map(|kind| InputEvent {
                    surface: byard_core::SurfaceId::MAIN,
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
                    surface: byard_core::SurfaceId::MAIN,
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

impl Step {
    /// This step's input, for surface `surface` (RFC-0068 §6): in a program
    /// with windows every event names the surface it is for.
    fn events_on(&self, now: u32, surface: byard_core::SurfaceId) -> Vec<InputEvent> {
        let mut events = self.events(now);
        for ev in &mut events {
            ev.surface = surface;
        }
        events
    }
}

/// How many moves a [`Step::Drag`] makes between the press and the release,
/// evenly spaced and a frame apart, so a handler that reads travel sees a
/// gesture and not a jump.
const DRAG_MOVES: u16 = 8;

/// A drag's two ends, press then release.
pub type Drag = ((f32, f32), (f32, f32));

/// A pointer event at a logical point, with no payload or modifiers.
fn pointer_event(kind: EventKind, x: f32, y: f32, time_ms: u64) -> InputEvent {
    InputEvent {
        surface: byard_core::SurfaceId::MAIN,
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms,
        mods: byard_core::KeyModifiers::default(),
    }
}

/// When set, `byard shot` draws every 16 ms frame up to `--at`, even once the
/// app is at rest. The output is the same; a test compares the two.
const SHOT_EVERY_FRAME: &str = "BYARD_SHOT_EVERY_FRAME";

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
    /// The window to draw, in a program with several (RFC-0068 §9): its `as`
    /// name, else its title. The steps are for it too.
    pub surface: Option<&'a str>,
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
    /// Off under `BYARD_SHOT_EVERY_FRAME`: see [`Scene::run_to`].
    skip_idle: bool,
    /// The windows of a program that has them (RFC-0068 §9); `None` for the
    /// one implicit window, which is `frame` above.
    windows: Option<Windows>,
}

/// What a [`Scene`] keeps of a program with windows: the last frame each was
/// drawn into, and where keys go.
struct Windows {
    frames: Vec<(byard_core::SurfaceId, RenderFrame)>,
    /// The window that has the keyboard: the one last tapped, else the first.
    focus: Option<byard_core::SurfaceId>,
}

/// One window of a [`Scene`], as the CLI needs it.
pub struct SurfaceView<'a> {
    /// Which surface it is.
    pub id: byard_core::SurfaceId,
    /// The name `--surface` finds it by: its `as` name, else its title.
    pub name: &'a str,
    /// Its title.
    pub title: &'a str,
    /// The size it is drawn at, logical pixels.
    pub size: (f32, f32),
    /// Its last frame.
    pub frame: &'a RenderFrame,
}

/// One line of the accessibility dump, with where its node is.
pub struct AccessLine {
    /// The line, with its indent.
    pub text: String,
    /// The surface it is on.
    pub surface: byard_core::SurfaceId,
    /// Where its node is on that surface, when it is a node.
    pub bounds: Option<byard_core::frame::Rect>,
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

/// How long a frame waits for work on another thread before it draws
/// without it.
const BACKGROUND_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

impl Scene {
    /// Starts `view` of `project` and runs its first frame.
    pub fn new(
        project: &Project,
        view: &ViewDecl,
        options: &SceneOptions<'_>,
    ) -> Result<Self, String> {
        let mut interp = Interpreter::new();
        // `after` and `every` count the scene's own frames, not the real time
        // they take to draw, as `App::shot` does.
        interp.use_virtual_timers();
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
        // A program that writes `Window` is drawn by surface (RFC-0068 §9). Read
        // from the view that is drawn: `--view` can pick one that has none.
        let multi =
            byard_compiler::surfaces::surface_shape_of(&project.program.views, view).is_multi();
        if multi {
            interp.enable_surfaces();
            interp.set_default_window_title(&project.manifest.name);
        }
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
            skip_idle: std::env::var_os(SHOT_EVERY_FRAME).is_none(),
            windows: multi.then(|| Windows {
                frames: Vec::new(),
                focus: None,
            }),
        };
        scene.frame_with(&[]);
        Ok(scene)
    }

    /// Whether the program has windows, and is drawn by surface.
    pub fn has_windows(&self) -> bool {
        self.windows.is_some()
    }

    /// The windows that are open, in the order they opened, each with its last
    /// frame. Empty for a program with none.
    pub fn surfaces(&self) -> Vec<SurfaceView<'_>> {
        let Some(w) = &self.windows else {
            return Vec::new();
        };
        self.interp
            .surface_ids()
            .into_iter()
            .filter_map(|id| {
                let spec = self.interp.surface_spec(id)?;
                let frame = &w.frames.iter().find(|(i, _)| *i == id)?.1;
                Some(SurfaceView {
                    id,
                    name: &spec.name,
                    title: &spec.title,
                    size: self.interp.surface_size(id)?,
                    frame,
                })
            })
            .collect()
    }

    /// The window `name` names (an `as` name, else a title), or an error that
    /// lists them.
    pub fn surface_named(&self, name: &str) -> Result<byard_core::SurfaceId, String> {
        let all = self.surfaces();
        if let Some(found) = all.iter().find(|s| s.name == name) {
            return Ok(found.id);
        }
        Err(format!(
            "no window named `{name}`; the windows are {}",
            names_of(&all)
        ))
    }

    /// The accessibility dump of every window: each starts with a
    /// `Window "title"` line and has its tree indented below it (RFC-0068 §9).
    /// For a program with one implicit window, that window's tree as it is.
    pub fn access_lines(&self) -> Vec<AccessLine> {
        let main = byard_core::SurfaceId::MAIN;
        let tree_lines = |surface, tree: &byard_core::access::AccessTree, indent: &str| {
            let text = tree.to_text();
            text.lines()
                .zip(&tree.nodes)
                .map(|(line, node)| AccessLine {
                    text: format!("{indent}{line}"),
                    surface,
                    bounds: Some(node.bounds),
                })
                .collect::<Vec<_>>()
        };
        if self.windows.is_none() {
            return self
                .frame
                .access()
                .map(|t| tree_lines(main, t, ""))
                .unwrap_or_default();
        }
        let mut out = Vec::new();
        for s in self.surfaces() {
            out.push(AccessLine {
                text: format!("Window {:?}", s.title),
                surface: s.id,
                bounds: None,
            });
            if let Some(tree) = s.frame.access() {
                out.extend(tree_lines(s.id, tree, "  "));
            }
        }
        out
    }

    /// [`access_lines`](Self::access_lines) as text.
    pub fn access_text(&self) -> String {
        let mut out = String::new();
        for line in self.access_lines() {
            out.push_str(&line.text);
            out.push('\n');
        }
        out
    }

    /// Where keys go: the window last tapped, else the first.
    fn key_surface(&self) -> byard_core::SurfaceId {
        let focus = self.windows.as_ref().and_then(|w| w.focus);
        focus
            .filter(|id| self.interp.surface_ids().contains(id))
            .or_else(|| self.interp.surface_ids().first().copied())
            .unwrap_or(byard_core::SurfaceId::MAIN)
    }

    /// One frame: the events, a tick, a full render. 16 ms pass.
    fn frame_with(&mut self, events: &[InputEvent]) {
        // A field the last frame asked for lands on this one, on any machine.
        self.interp.wait_for_background_work(BACKGROUND_WAIT);
        self.interp.set_now_ms(self.now);
        self.interp.dispatch_events(events);
        self.interp.tick();
        if self.windows.is_some() {
            self.surface_frames();
            self.now += 16;
            return;
        }
        self.frame = RenderFrame::new();
        self.frame.request_full_redraw();
        // Asked for like the platform asks: by the frame's flag.
        self.frame.set_access_wanted(self.access);
        self.interp
            .render(&self.tree, &mut self.frame, self.size.0, self.size.1);
        self.now += 16;
    }

    /// Does one step: one frame; for a wait, as many as the time takes; for a
    /// drag, a press, the moves and the release, a frame each.
    pub fn apply(&mut self, what: &Step) {
        self.apply_on(None, what);
    }

    /// [`apply`](Self::apply) for a program with windows, with the window the
    /// step is for: a tap or a drag goes to it, and the keyboard follows it.
    /// `None` is the window that has the keyboard.
    pub fn apply_on(&mut self, surface: Option<byard_core::SurfaceId>, what: &Step) {
        if let Step::Wait(ms) = what {
            self.run_to(self.now + ms);
            return;
        }
        let surface = if self.windows.is_some() {
            surface.unwrap_or_else(|| self.key_surface())
        } else {
            byard_core::SurfaceId::MAIN
        };
        let mut events = Vec::new();
        if self.windows.is_some() && Self::moves_focus(what) {
            events.extend(self.focus_events(surface));
        }
        events.extend(what.events_on(self.now, surface));
        self.frame_with(&events);
        if let Step::Drag((x0, y0), (x1, y1)) = what {
            for i in 1..=DRAG_MOVES {
                let t = f32::from(i) / f32::from(DRAG_MOVES);
                let at = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                let mut moved =
                    pointer_event(EventKind::PointerMove, at.0, at.1, u64::from(self.now));
                moved.surface = surface;
                self.frame_with(&[moved]);
            }
            let mut up = pointer_event(EventKind::PointerUp, *x1, *y1, u64::from(self.now));
            up.surface = surface;
            self.frame_with(&[up]);
        }
    }

    /// Whether a step is a pointer press, which gives its window the keyboard.
    fn moves_focus(what: &Step) -> bool {
        matches!(what, Step::Tap(..) | Step::Hold(..) | Step::Drag(..))
    }

    /// The events that give `surface` the keyboard: the window that had it
    /// loses it, as the platform says when another window is clicked.
    fn focus_events(&mut self, surface: byard_core::SurfaceId) -> Vec<InputEvent> {
        let previous = self.key_surface();
        let Some(w) = self.windows.as_mut() else {
            return Vec::new();
        };
        w.focus = Some(surface);
        if previous == surface {
            return Vec::new();
        }
        let focus = |surface, on| InputEvent {
            surface,
            kind: EventKind::WindowFocus,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload: Some(byard_core::platform::InputPayload::Bool(on)),
            time_ms: u64::from(self.now),
            mods: byard_core::KeyModifiers::default(),
        };
        vec![focus(previous, false), focus(surface, true)]
    }

    /// A tick of a program with windows: each window with something new is
    /// drawn into a frame of its own, and the rest keep the one they have.
    fn surface_frames(&mut self) {
        let access = self.access;
        let mut out = Vec::new();
        self.interp.render_surfaces(
            &self.tree,
            &mut || {
                // As a shot of the one window does: a picture is drawn from
                // nothing, so the frame redraws everything.
                let mut frame = RenderFrame::new();
                frame.request_full_redraw();
                frame.set_access_wanted(access);
                frame
            },
            &mut out,
        );
        let open = self.interp.surface_ids();
        if let Some(w) = self.windows.as_mut() {
            for (id, frame) in out {
                match w.frames.iter_mut().find(|(i, _)| *i == id) {
                    Some(slot) => slot.1 = frame,
                    None => w.frames.push((id, frame)),
                }
            }
            w.frames.retain(|(id, _)| open.contains(id));
        }
        // What the program asked of the platform has nobody to ask here.
        let _ = self.interp.take_surface_changes();
    }

    /// Runs frames until `at_ms` of app time has passed, then once more at
    /// rest, so a mesh or an image that landed a frame late is in.
    pub fn settle(&mut self, at_ms: u32) {
        self.run_to(at_ms);
        self.frame_with(&[]);
    }

    /// Runs frames with no input until `until` ms of app time.
    ///
    /// A picture of an app at rest does not need 90 frames of it. Once two
    /// frames in a row ask for no more (no animation in flight, no timer or
    /// call waiting, nothing being measured), the ones that follow would draw
    /// what these drew, so the clock goes to where the loop would have ended,
    /// or to the next timer on the app clock, on the same 16 ms grid. No input arrives in between, so no skipped
    /// frame can be the one something happens in. `BYARD_SHOT_EVERY_FRAME`
    /// draws them all, which is how a test checks the two agree.
    fn run_to(&mut self, until: u32) {
        let mut still = 0_u32;
        while self.now < until {
            self.frame_with(&[]);
            // A loop that never ends is a function of the clock, so it does not
            // keep the frames in between from being skipped; anything that has
            // to be stepped through does.
            still = if self.interp.needs_another_frame_besides_loops()
                || self.interp.has_background_work()
            {
                0
            } else {
                still + 1
            };
            if self.skip_idle && still >= 2 && self.now < until {
                // Up to `until`, or to the frame a waiting timer is due on,
                // whichever comes first, so it fires when it would have.
                let target = self
                    .interp
                    .next_virtual_timer_ms()
                    .and_then(|due| u32::try_from(due).ok())
                    .map_or(until, |due| due.min(until));
                if target > self.now {
                    self.now += (target - self.now).div_ceil(16) * 16;
                }
            }
        }
    }

    /// The last frame. For a program with windows, the frame of the first
    /// (see [`surfaces`](Self::surfaces) for each).
    pub fn frame(&self) -> &RenderFrame {
        match &self.windows {
            Some(w) => w.frames.first().map_or(&self.frame, |(_, f)| f),
            None => &self.frame,
        }
    }

    /// The accessibility tree of the last frame.
    pub fn access(&self) -> Option<&byard_core::access::AccessTree> {
        self.frame().access()
    }
}

/// The windows' names, as an error lists them.
fn names_of(surfaces: &[SurfaceView<'_>]) -> String {
    if surfaces.is_empty() {
        return "none open".to_string();
    }
    surfaces
        .iter()
        .map(|s| format!("`{}`", s.name))
        .collect::<Vec<_>>()
        .join(", ")
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
    if args.surface.is_some() && !scene.has_windows() {
        return Err(
            "`--surface` picks one of a program's windows, and this program writes no \
                    `Window`: it is one window, drawn whole"
                .to_string(),
        );
    }
    for what in args.steps {
        // The steps are for the window `--surface` names, when it is open; until
        // then, for the one that has the keyboard, which is the first.
        let target = args.surface.and_then(|name| scene.surface_named(name).ok());
        scene.apply_on(target, what);
    }
    scene.settle(args.at_ms);

    if let Some(path) = args.a11y {
        let (text, nodes) = if scene.has_windows() {
            let lines = scene.access_lines();
            (
                scene.access_text(),
                lines.iter().filter(|l| l.bounds.is_some()).count(),
            )
        } else {
            let tree = scene
                .frame()
                .access()
                .ok_or("the last frame carried no accessibility tree")?;
            (tree.to_text(), tree.nodes.len())
        };
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        style::ok(
            &format!("{} ({nodes} nodes)", path.display()),
            Some(started.elapsed()),
        );
    }
    let Some(out) = args.out else {
        return Ok(());
    };
    // What is drawn, and at what size: the one window as it always was, or one
    // window of several at the size it opens at (RFC-0068 §9).
    let (frame, size): (&RenderFrame, (u32, u32)) = if scene.has_windows() {
        let all = scene.surfaces();
        let picked = match (args.surface, all.as_slice()) {
            (Some(name), _) => all.iter().find(|s| s.name == name).ok_or_else(|| {
                format!(
                    "no window named `{name}`; the windows are {}",
                    names_of(&all)
                )
            })?,
            (None, [only]) => only,
            (None, _) => {
                return Err(format!(
                    "this program has {} windows ({}); say which one to draw with \
                     `--surface NAME` (its `as` name, else its title). Drawing every window \
                     together on a screen is RFC-0068's headless screens, step 5",
                    all.len(),
                    names_of(&all)
                ));
            }
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size = (picked.size.0.round() as u32, picked.size.1.round() as u32);
        (picked.frame, size)
    } else {
        (scene.frame(), args.size)
    };
    #[allow(clippy::cast_precision_loss)]
    let (w, h) = (size.0 as f32, size.1 as f32);
    let pixels = Gpu::open()?.paint(frame, size, args.scale)?;
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
        let format = wgpu::TextureFormat::Rgba8Unorm;
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
