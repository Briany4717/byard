//! `byard dev [file]`, the live-reload dev runner (RFC-0006 §3).
//!
//! Thread model:
//!   Main/winit thread → `on_resume` → `start_logic_from_view` (logic thread)
//!                     → `start_watcher` (OS notify thread)
//!   Logic thread: drain channel → `dispatch_events` → tick → render / error overlay
//!   Watcher thread: file change → re-resolve the module graph → `LatestWins::publish`
//!
//! RFC-0008: the watcher covers the whole project directory plus every `path`
//! dependency (cooperative dev, D-J); a change to any `.byd` or `byard.toml`
//! re-runs the module resolver, so package edits hot-reload like local ones.
//! Fetched, lock-pinned cache packages are immutable and not watched.

use byard_compiler::CompileError;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::reload::{Gated, ViewReload};
use byard_compiler::interp::reload::{
    LatestWins, ParsedFile, ReloadKind, diff_program, gate, start_watcher,
};
use byard_compiler::parser::ast::ViewDecl;
use byard_core::frame::{BoxInstance, RenderFrame, TargetId, TextLine};
use byard_core::{
    ByardError, Engine, LogicRuntime, PlatformHost, PointerButton, PointerState, WindowSize,
};
use byard_platform::WinitHost;

use crate::statusline::StatusLine;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::deps::{cache_dir, resolve_project};
use crate::manifest::Manifest;

/// Everything `byard dev` was invoked with.
///
/// A struct rather than four positional parameters: three of them are optional
/// paths and strings, and a call site that swaps two of those compiles.
#[derive(Clone, Copy)]
pub struct Options<'a> {
    /// The `[file]` override.
    pub file: Option<&'a Path>,
    /// `--deep-link <url>` (RFC-0026).
    pub deep_link: Option<&'a str>,
    /// `--trace <path>` (RFC-0030 §V5).
    pub trace: Option<&'a Path>,
    /// `--profile` (RFC-0030 §V1).
    pub profile: bool,
}

pub fn run(opts: Options<'_>) -> Result<(), String> {
    let started = std::time::Instant::now();
    let Options {
        file,
        deep_link,
        trace,
        profile,
    } = opts;
    let manifest = Manifest::discover(file)?;

    // Initial resolve on the main thread: catch errors before opening the
    // window. This covers the whole module graph (RFC-0008), not just the
    // entry file.
    let (program, provider) = resolve_project(&manifest)?;

    // The startup header is four facts, a rule, a result and a duration
    // (RFC-0030 §"Starting the dev runner"). Two of the facts, the adapter and
    // the frame budget, do not exist until the window and device do, so the
    // *whole* header is deferred to `on_resume` rather than half-printed here
    // and finished later, which would put "watching for changes" above the
    // facts it is meant to introduce.
    let title = format!("Byard dev, {}", manifest.name);
    let header = Header {
        project: manifest.name.clone(),
        entry: manifest.entry.display().to_string(),
        packages: program.packages[1..].join(", "),
        views: program.views.len(),
        errors: program.errors.clone(),
        source_map: program.source_map,
        started,
    };

    // Watch the project source directory plus every resolved `path`
    // dependency (D-J); cache checkouts are pinned/immutable → not watched.
    let cache = cache_dir();
    let entry_dir = manifest
        .entry
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut watch_paths = vec![entry_dir];
    for root in provider.resolved_roots().values() {
        if !root.starts_with(&cache) {
            watch_paths.push(root.clone());
        }
    }

    let initial_views = program.views;
    let initial_errors = program.errors;
    let vector_cache_dir = manifest
        .project_root
        .join(".byard")
        .join("cache")
        .join("vectors");

    // Poll mode: the event loop spins continuously so file-change frames
    // appear without waiting for the next mouse event (RFC-0006 §3.2).
    // Opened before the window so a bad path fails immediately rather than
    // after the event loop has taken over the process.
    let trace = match trace {
        Some(path) => {
            crate::style::fact("Trace", &path.display().to_string());
            Some(crate::trace::TraceWriter::create(path)?)
        }
        None => None,
    };

    // RFC-0030 §P6: the counters the statusline reads. `reloads` and
    // `reload_pending` are written by the logic thread and read by the render
    // thread, so they cross as atomics, the only way anything may (INV-2).
    let reloads = Arc::new(AtomicU32::new(0));
    let reload_pending = Arc::new(AtomicBool::new(false));
    // `Mod+Shift+D` (or `[dev] hud`): written by the render thread, read by
    // the logic thread, so it crosses as an atomic (INV-2).
    let hud_visible = Arc::new(AtomicBool::new(manifest.dev.hud));

    let host = WinitHost::new(&title, 1280, 720).with_poll();
    host.run(App {
        engine: None,
        project: manifest.name.clone(),
        header: Some(header),
        dev: manifest.dev.clone(),
        want_profile: profile,
        statusline: None,
        width_bits: None,
        height_bits: None,
        animating: None,
        file_override: file.map(Path::to_path_buf),
        watch_paths,
        vector_cache_dir,
        initial_views,
        initial_errors,
        initial_theme: manifest.theme.clone(),
        deep_link: deep_link.map(str::to_string),
        last_render_telemetry: byard_core::telemetry::SampleBlock::default(),
        last_telemetry_print: std::time::Instant::now(),
        trace,
        reloads,
        reload_pending,
        hud_visible,
        hud_telemetry: None,
        last_frame: std::time::Instant::now(),
    })
    .map_err(|e| format!("event loop error: {e}"))
}

/// Re-derives the whole program for the watcher thread (RFC-0008 Pillar E):
/// re-discovers the manifest (so `byard.toml` edits apply live), re-runs the
/// module resolver, and folds any project-level failure into the same error
/// channel the overlay renders.
fn reresolve(file_override: Option<&Path>) -> ParsedFile {
    match Manifest::discover(file_override).and_then(|m| resolve_project(&m)) {
        Ok((program, _)) => ParsedFile {
            views: program.views,
            errors: program.errors,
        },
        Err(message) => ParsedFile {
            views: Vec::new(),
            errors: vec![CompileError::Project {
                span: byard_compiler::Span::new(0, 0),
                message,
            }],
        },
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() * 1000 + u64::from(d.subsec_millis()))
}

// ── The deferred-reload gate (RFC-0006 §3.5 / C1) ────────────────────────────

/// A structure-incompatible reload held until the in-flight gesture ends, and
/// the flag the statusline reads to say so.
///
/// The two live in one type because the failure they exist to prevent is the
/// two of them *disagreeing*. A developer holding a drag while a save waits
/// behind the gate sees an application that has silently stopped responding to
/// their edits, indistinguishable from a broken watcher, and the most alarming
/// thing a dev runner can do without saying anything. An indicator that can be
/// left on after the reload landed, or off while one waits, is worse than none.
///
/// Here the slot and the flag cannot drift: nothing sets one without the other,
/// because nothing can reach either directly.
struct PendingReload {
    slot: Option<(Vec<ViewDecl>, ReloadKind)>,
    /// Mirrored to the render thread as an `AtomicBool`, the only way state
    /// may cross (INV-2).
    published: Arc<AtomicBool>,
}

impl PendingReload {
    fn new(published: Arc<AtomicBool>) -> Self {
        Self {
            slot: None,
            published,
        }
    }

    /// Holds `views` behind the gate. Returns `true` on the rising edge, so the
    /// caller can wake an idle render loop for the frame that shows the
    /// indicator, the wait has no input behind it and would otherwise not be
    /// drawn until something unrelated happened.
    fn defer(&mut self, views: Vec<ViewDecl>, kind: ReloadKind) -> bool {
        let was = self.published.swap(true, Ordering::Relaxed);
        // A newer save supersedes an older deferred one: applying the stale
        // views afterwards would undo an edit the developer already made.
        self.slot = Some((views, kind));
        !was
    }

    /// Releases the held reload once the gesture is over, clearing the flag in
    /// the same step.
    fn take_if_released(&mut self, pointer_pressed: bool) -> Option<(Vec<ViewDecl>, ReloadKind)> {
        if pointer_pressed {
            return None;
        }
        let taken = self.slot.take();
        if taken.is_some() {
            self.published.store(false, Ordering::Relaxed);
        }
        taken
    }
}

// ── The reload flash (RFC-0030 §V6) ──────────────────────────────────────────

/// A 2 px inset border that pulses once when a hot reload lands.
///
/// Green for a reactive-compatible reload, amber for a deferred
/// structure-incompatible one that has just come out from behind the gate.
///
/// # Never suppressed on a no-visible-diff reload (§Q9)
///
/// That is its *only* justifying case. A save that changed a colour announces
/// itself; a save that changed a handler, a comment, or a value that happens to
/// render identically does not, and that is precisely the situation in which a
/// developer cannot tell whether hot reload is working at all. Suppressing the
/// flash there would remove the feature from the one case it exists for and
/// leave it firing only when it is redundant.
#[derive(Default)]
struct ReloadFlash {
    /// When the flash started, on the interpreter's animation clock, or `None`.
    started_ms: Option<u32>,
    /// Amber rather than green: a deferred, structure-incompatible reload.
    deferred: bool,
}

impl ReloadFlash {
    /// How long the pulse lasts.
    const DURATION_MS: u32 = 120;
    /// Peak alpha.
    const PEAK: f32 = 0.9;
    /// Border thickness in logical pixels.
    const WIDTH: f32 = 2.0;

    fn trigger(&mut self, now_ms: u32, deferred: bool) {
        self.started_ms = Some(now_ms);
        self.deferred = deferred;
    }

    /// Whether the flash still has frames to draw.
    fn is_active(&self, now_ms: u32) -> bool {
        self.started_ms
            .is_some_and(|start| now_ms.saturating_sub(start) < Self::DURATION_MS)
    }

    /// Alpha at `now_ms`: 0 → peak → 0, triangular over the duration.
    ///
    /// A triangle rather than an ease: the whole event is 120 ms, which is
    /// about seven frames, and the difference between a curve and a ramp is not
    /// perceivable across seven frames. Spending a `Motion` on it would add
    /// state to something whose entire lifetime is shorter than a keypress.
    fn alpha(&self, now_ms: u32) -> f32 {
        let Some(start) = self.started_ms else {
            return 0.0;
        };
        let elapsed = now_ms.saturating_sub(start);
        if elapsed >= Self::DURATION_MS {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let t = elapsed as f32 / Self::DURATION_MS as f32;
        let ramp = if t < 0.5 { t * 2.0 } else { (1.0 - t) * 2.0 };
        ramp * Self::PEAK
    }

    /// Draws the inset border, if the flash is running.
    ///
    /// One `DecoratedBox`, a transparent fill with a border, on top of
    /// everything else. No new pipeline, and no state beyond the trigger.
    fn render(&mut self, frame: &mut RenderFrame, w: f32, h: f32, now_ms: u32) {
        let alpha = self.alpha(now_ms);
        if alpha <= 0.0 {
            // Released rather than left set, so `is_active` stays a cheap
            // answer to "does this still need frames?".
            self.started_ms = None;
            return;
        }
        let colour = if self.deferred {
            [0.91, 0.73, 0.19, alpha] // warn amber
        } else {
            [0.49, 0.83, 0.66, alpha] // ok green
        };
        // Its own layer, above the app *and* the HUD: the flash is a statement
        // about the session, not about anything in the scene.
        frame.begin_layer();
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: BoxInstance {
                rect: [
                    Self::WIDTH * 0.5,
                    Self::WIDTH * 0.5,
                    (w - Self::WIDTH).max(0.0),
                    (h - Self::WIDTH).max(0.0),
                ],
                color: [0.0; 4],
                radii: [0.0; 4],
                transform: byard_core::frame::Transform::IDENTITY,
                smooth: 0.0,
            },
            border_width: Self::WIDTH,
            border_color: colour,
            opacity: 1.0,
            dirty: true,
            ..Default::default()
        });
    }
}

// ── Logic runtime ─────────────────────────────────────────────────────────────

struct ByldRuntime {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    current_views: Vec<ViewDecl>,
    reload_channel: Arc<LatestWins<ParsedFile>>,
    /// Changed `.svg` paths from the file watcher: each is invalidated in the
    /// vector JIT so the field regenerates live (RFC-0009 §3, M47).
    asset_changes: crossbeam_channel::Receiver<std::path::PathBuf>,
    /// A structure-incompatible reload held during an in-flight gesture (E5),
    /// together with the indicator that says so (RFC-0006 C1).
    pending_reload: PendingReload,
    /// Parse errors from the last file save (drives error overlay, RFC-0006 §3.4).
    error_state: Option<Vec<CompileError>>,
    width_bits: Arc<AtomicU32>,
    height_bits: Arc<AtomicU32>,
    /// Epoch for the animation clock (RFC-0010): `with` animations sample
    /// against ms elapsed since the logic runtime started.
    start: std::time::Instant,
    /// Performance diagnostics already reported to the terminal (RFC-0023,
    /// e.g. overlapping blurs), so each distinct warning prints once instead
    /// of every frame.
    reported_perf: std::collections::HashSet<String>,
    /// How many hot reloads have been applied this session, the statusline's
    /// `↻N` field, and the cheapest possible confirmation that the watcher is
    /// alive at all (RFC-0030 §P6). Published to the render thread through
    /// `reloads_pub`.
    reload_count: u32,
    /// The render thread's mirror of `reload_count`.
    reloads_pub: Arc<AtomicU32>,
    /// The RFC-0010 active-animation set, published for the render thread
    /// (`AtomicBool` because that is the only way it may cross, INV-2).
    ///
    /// The logic thread writes it after every render; the event loop reads it to
    /// decide whether to keep spinning. Without this the accessor existed and
    /// nothing consulted it, so `byard dev` polled forever and a settled scene
    /// still cost a full core.
    animating: Arc<AtomicBool>,
    /// Wakes an idle (`Wait`-mode) render loop.
    ///
    /// The relay only wakes on an *input-bearing* tick, which is not enough once
    /// the loop is allowed to sleep: a hot reload and an animation that starts
    /// without input both change the frame with no input behind them. Fired on
    /// the rising edge only, so a steadily animating scene (already spinning)
    /// posts nothing.
    waker: byard_core::relay::FrameWaker,
    /// The reload flash (RFC-0030 §V6).
    flash: ReloadFlash,
    /// The in-window HUD (RFC-0030 §V3), or `None` if the embedded source
    /// failed to parse, which is a defect in this repository, reported once
    /// rather than taking a developer's session down with it.
    hud: Option<crate::hud::DevHud>,
    /// The render thread's `Mod+Shift+D` toggle.
    hud_visible: Arc<AtomicBool>,
    /// Fresh HUD readings from the render thread, latest wins.
    hud_telemetry: crossbeam_channel::Receiver<crate::hud::HudTelemetry>,
    /// Whether the error overlay was up on the previous tick, so the two
    /// frames that change the composition wholesale can ask for a full redraw
    /// and no others do.
    overlay_was_up: bool,
    /// A `--deep-link <url>` waiting to be delivered (RFC-0026), applied after
    /// the first render. It waits because a navigation container only exists
    /// once the frame that mounts it has been reconciled, a stack nested in a
    /// tab is not there to receive anything before that.
    pending_deep_link: Option<String>,
}

impl ByldRuntime {
    /// Publishes the active-animation set for the render thread, waking an idle
    /// loop on the **rising** edge (nothing moving → something moving). A loop
    /// that is already spinning needs no event, and a settled one must not be
    /// woken at all, that is the whole point of letting it sleep.
    fn publish_animating(&self, active: bool) {
        let was = self.animating.swap(active, Ordering::Relaxed);
        if active && !was {
            self.wake_render_loop();
        }
    }

    /// Asks the render loop to present the next frame, for the changes that have
    /// no input event behind them (a hot reload, a fresh error overlay, an
    /// animation that just started).
    fn wake_render_loop(&self) {
        (self.waker)();
    }

    fn apply_reload(&mut self, new_views: &[ViewDecl], kind: ReloadKind) {
        // The flash is triggered here rather than at the call sites so it
        // cannot be forgotten by one of them, and so it fires on *every*
        // applied reload, including the ones that changed nothing visible,
        // which is the only case it exists for (§Q9).
        let now = u32::try_from(self.start.elapsed().as_millis()).unwrap_or(u32::MAX);
        self.flash
            .trigger(now, kind == ReloadKind::StructureIncompatible);
        // The rendered root is the first tracked view. Editing any view it
        // transitively instantiates must re-derive its tree, so compute the
        // affected set (changed views ∪ transitive callers, RFC-0007 §5) and
        // re-lower only when the root is in it, siblings unrelated to the root
        // keep their state.
        if let (Some(old_root), Some(new_root)) = (self.current_views.first(), new_views.first()) {
            let affected =
                byard_compiler::interp::reload::affected_views(&self.current_views, new_views);
            let diff_kind = byard_compiler::interp::reload::diff_view(old_root, new_root);
            self.interp.reload(new_root, diff_kind);
            // Rebuild the user-`View` registry so reloaded sibling views resolve
            // and expand (RFC-0007 §1/§5).
            self.interp.load_views(new_views);
            if affected.contains(&new_root.name) {
                let known: Vec<&str> = new_views.iter().map(|v| v.name.as_str()).collect();
                self.tree = self.interp.lower_view(new_root, &known);
            }
        }
        self.current_views = new_views.to_vec();
        self.error_state = None;
        self.reload_count = self.reload_count.saturating_add(1);
        self.reloads_pub.store(self.reload_count, Ordering::Relaxed);
        crate::style::reload(&format!("reloaded {} view(s)", new_views.len()));
    }
}

impl LogicRuntime for ByldRuntime {
    fn apply_io_results(&mut self, results: Vec<byard_core::relay::IoResult>) -> bool {
        // Tick step 0 (RFC-0028 §6): every controller reply and timer tick the
        // pool completed, applied before input and before the pull.
        self.interp.apply_io_results(results)
    }

    fn evaluate_tick(
        &mut self,
        frame: &mut RenderFrame,
        input_events: &[byard_core::platform::InputEvent],
        _dirty: &[TargetId],
    ) {
        // ── Step 0: drain latest-wins reload channel (RFC-0006 §3.2 C3) ───────
        if let Some(parsed) = self.reload_channel.take() {
            if parsed.errors.is_empty() {
                let pointer_pressed = self.interp.router.is_pointer_pressed();
                // Classify the worst-case kind across all changed views.
                let diffs = diff_program(&self.current_views, &parsed.views);
                let worst =
                    diffs
                        .iter()
                        .fold(ReloadKind::ReactiveCompatible, |acc, (_, r)| match r {
                            ViewReload::Patch(ReloadKind::StructureIncompatible)
                            | ViewReload::Added
                            | ViewReload::Removed => ReloadKind::StructureIncompatible,
                            ViewReload::Patch(ReloadKind::ReactiveCompatible) => acc,
                        });
                match gate(worst, pointer_pressed) {
                    Gated::Apply => {
                        self.apply_reload(&parsed.views, worst);
                        // A reload changes the frame with no input behind it, so
                        // an idle loop has to be told to present it.
                        self.wake_render_loop();
                    }
                    Gated::Defer => {
                        if self.pending_reload.defer(parsed.views, worst) {
                            // The indicator appearing is itself a frame change
                            // with no input behind it.
                            self.wake_render_loop();
                        }
                    }
                }
            } else {
                self.error_state = Some(parsed.errors);
                self.wake_render_loop();
            }
        }

        // ── Step 0b: apply deferred reload once pointer released ───────────────
        let pressed = self.interp.router.is_pointer_pressed();
        if let Some((new_views, kind)) = self.pending_reload.take_if_released(pressed) {
            self.apply_reload(&new_views, kind);
            self.wake_render_loop();
        }

        // ── Step 0c: invalidate hot-reloaded vector assets (RFC-0009 §3) ──────
        // Drain every `.svg` change the watcher reported since the last tick and
        // invalidate its MSDF field; the regenerated field reuses the same atlas
        // cell, so the icon updates in place without remounting its `View`.
        while let Ok(path) = self.asset_changes.try_recv() {
            self.interp.invalidate_vector_asset(&path);
        }

        // ── Step 1: dispatch input events ─────────────────────────────────────
        self.interp.dispatch_events(input_events);

        // ── Step 2: reactive tick ─────────────────────────────────────────────
        self.interp.tick();

        // ── Step 3: render ────────────────────────────────────────────────────
        // Advance the animation clock (RFC-0010) before rendering so `with`
        // curves sample against the current elapsed time.
        let elapsed = u32::try_from(self.start.elapsed().as_millis()).unwrap_or(u32::MAX);
        self.interp.set_now_ms(elapsed);
        let w = f32::from_bits(self.width_bits.load(Ordering::Relaxed));
        let h = f32::from_bits(self.height_bits.load(Ordering::Relaxed));

        // Mounting and dismissing the overlay both change the entire
        // composition at once, and the encoder's scissor union is derived from
        // what changed *between two frames*. On the mount frame the last-good
        // view underneath is being drawn again after a gap, so a union computed
        // from a clean previous frame would leave both it and the overlay
        // partially painted. Ask for a full redraw on exactly those two frames
        //, not on every frame the overlay is up, which would hand back the
        // incremental path for as long as a file stays broken.
        let overlay_now = self.error_state.is_some();
        if overlay_now != self.overlay_was_up {
            frame.request_full_redraw();
            self.overlay_was_up = overlay_now;
        }

        if let Some(errors) = &self.error_state {
            // RFC-0006 §3.4 promised the last successfully-rendered view stays
            // as a blurred background, and the original implementation
            // deliberately painted an opaque field instead. The comment
            // explaining why was honest and correct at the time: the flat
            // four-pass encoder drew all text in one global pass after every
            // box, so the app's text bled *over* the scrim.
            //
            // RFC-0017's z-layers and RFC-0023's backdrop blur removed that
            // constraint. The app renders normally, `begin_layer` closes it,
            // and the overlay composites after it, text included, so the
            // promise can finally be kept as written.
            self.interp.render(&self.tree, frame, w, h);
            frame.begin_layer();
            render_error_overlay(frame, errors, w, h);
            // An error overlay is static: nothing to animate, so the loop may
            // sleep until the next save, unless a flash is still running.
            self.publish_animating(self.flash.is_active(elapsed));
        } else {
            self.interp.render(&self.tree, frame, w, h);
            // RFC-0010/RFC-0025: publish whether anything is still in motion, so
            // the event loop can stop requesting frames once it all settles.
            self.publish_animating(
                self.interp.has_active_animations() || self.flash.is_active(elapsed),
            );
            // RFC-0023 runtime perf diagnostics (e.g. ≥ 3 stacked frosted-glass
            // panes): surface each distinct warning once on the terminal.
            for warning in self.interp.perf_warnings() {
                let text = match warning {
                    byard_compiler::interp::eval::PerfWarning::OverlappingBlurs { count } => {
                        format!(
                            "perf: {count} overlapping backdrop-blur panes in one frame, \
                             each pane re-blurs the ones below it (RFC-0023)"
                        )
                    }
                    // RFC-0026: a navigation stack that keeps growing is almost
                    // always a push that never pops.
                    byard_compiler::interp::eval::PerfWarning::DeepNavStack { depth, path } => {
                        format!(
                            "perf: `NavStack` is {depth} deep, refused to push `{path}`; \
                             every entry below the top stays in memory (RFC-0026)"
                        )
                    }
                    // RFC-0038: a rect that keeps flipping between two sizes is
                    // an `on measure` feeding its own layout. Bounded to one
                    // fire per frame, so the app runs; named here, because a
                    // layout that twitches explains nothing by itself.
                    byard_compiler::interp::eval::PerfWarning::MeasureFeedback { span } => {
                        format!(
                            "perf: the `on measure` at byte {} alternates between two sizes, \
                             so its own layout depends on what it measured (RFC-0038)",
                            span.start
                        )
                    }
                };
                if self.reported_perf.insert(text.clone()) {
                    crate::style::warn(&text);
                }
            }
            // RFC-0026 deep linking: the host's whole job is to hand the URL
            // over, from here it is an ordinary navigation, with the same
            // push, transition and `route_change` as a tap.
            if let Some(url) = self.pending_deep_link.take() {
                if self.interp.apply_deep_link(&url) {
                    self.interp.tick();
                    self.wake_render_loop();
                } else {
                    crate::style::warn(&format!(
                        "no route matches the deep link `{url}`, ignoring it"
                    ));
                }
            }
        }

        // ── Step 4: the dev-only surfaces (RFC-0030 §V3–§V4, §V6) ────────────
        self.render_dev_surfaces(frame, w, h, elapsed);
    }
}

impl ByldRuntime {
    /// Draws the in-window HUD and the reload flash, above everything the app
    /// emitted.
    ///
    /// Both are dev-only overlays; neither may change what the app's own
    /// measurement says about it (INV-24).
    fn render_dev_surfaces(&mut self, frame: &mut RenderFrame, w: f32, h: f32, elapsed: u32) {
        // Where the app's primitives end and the dev runner's begin, recorded
        // unconditionally, including on frames that emit no dev surface at
        // all, where it resolves to "the dev range is empty" rather than to
        // "unknown". The render thread reads it to charge the shaping of these
        // lines to the profiler rather than to the app (RFC-0030 §V4).
        let dev_base = frame.cursor();

        if self.hud_visible.load(Ordering::Relaxed) {
            // Captured *before* the HUD renders: the HUD runs its own
            // `LayoutAtlas` on this thread, so its layout activity lands in the
            // same thread-local counters the statusline reads. Without
            // restoring this, the HUD would report the app as rebuilding every
            // frame, the observer effect in its most embarrassing form.
            let app_paths = frame.atlas_paths();
            while let Ok(t) = self.hud_telemetry.try_recv() {
                if let Some(hud) = self.hud.as_mut() {
                    hud.accept(t);
                }
            }
            if let Some(hud) = self.hud.as_mut() {
                hud.render(frame, w, h);
            }
            frame.set_atlas_paths(app_paths);
        }

        // The flash is a dev surface too, it draws over the app and the app
        // never asked for it, so it sits inside the same partition. It emits
        // one `DecoratedBox` and no text, so in practice it contributes
        // nothing; recording it anyway keeps the rule "everything after this
        // cursor belongs to the dev runner" true without exceptions.
        self.flash.render(frame, w, h, elapsed);

        frame.set_dev_base(dev_base);
    }
}

/// Max errors listed in the overlay.
///
/// Three was a Phase-2 heuristic forced by the overlay having to fit in a
/// hand-placed column on an opaque field. It is now a real layer over a real
/// view, so the panel sizes itself to what it holds and the limit exists only
/// to stop a cascade of two hundred errors from running off the bottom of the
/// window, which is a different, much larger number.
const OVERLAY_MAX_ERRORS: usize = 12;
/// Max chars per headline before adding "…" (avoids horizontal overflow).
const OVERLAY_MAX_HEADLINE_CHARS: usize = 78;
/// Backdrop blur σ, in logical pixels (RFC-0023).
///
/// Enough that no word of the app underneath is readable, a legible
/// background competes with the error text for the same attention, and little
/// enough that the layout, the colours and *where you were* all survive, which
/// is the entire reason the last good view is shown at all.
const OVERLAY_BLUR: f32 = 18.0;

/// Renders the error overlay as an RFC-0017 layer over a blurred backdrop of
/// the last good view (RFC-0006 §3.4).
///
/// Truncates to [`OVERLAY_MAX_ERRORS`] errors and [`OVERLAY_MAX_HEADLINE_CHARS`]
/// chars per headline to keep the overlay bounded without needing Taffy layout
///, this path is deliberately independent of the interpreter, since the
/// interpreter is what just failed.
fn render_error_overlay(frame: &mut RenderFrame, errors: &[CompileError], w: f32, h: f32) {
    // The frosted pane: the whole viewport, blurred, with a dark tint over it.
    // Not an opaque fill, and not a plain translucent scrim either, a scrim
    // leaves the app's own contrast fighting the error text, while a blur
    // removes the detail and keeps the shape.
    frame.push_backdrop(byard_core::frame::BackdropInstance {
        rect: [0.0, 0.0, w, h],
        radii: [0.0; 4],
        blur: OVERLAY_BLUR,
        tint: [0.05, 0.05, 0.07, 0.82],
        saturation: 0.35,
        quality: byard_core::frame::BLUR_QUALITY_HIGH,
        opacity: 1.0,
        transform: byard_core::frame::Transform::IDENTITY,
        depth: 0.0,
        smooth: 0.0,
    });

    let padding = 32.0;
    let line_height = 22.0;
    let shown = errors.len().min(OVERLAY_MAX_ERRORS);
    let truncated = errors.len() > OVERLAY_MAX_ERRORS;

    // The panel is sized to its contents rather than filling the viewport, so
    // the blurred view is visible around it and the overlay reads as something
    // *in front of* the app rather than instead of it.
    let rows = 1.5
        + f32::from(u16::try_from(shown).unwrap_or(u16::MAX)) * 1.2
        + if truncated { 1.3 } else { 0.0 }
        + 1.6;
    let panel_h = (padding * 2.0 + line_height * rows).min(h - 32.0);
    let panel_w = (w - 64.0).clamp(240.0, 880.0);
    let panel_x = ((w - panel_w) / 2.0).max(0.0);
    let panel_y = ((h - panel_h) / 2.0).max(0.0);
    frame.push_instance(BoxInstance {
        rect: [panel_x, panel_y, panel_w, panel_h],
        color: [0.11, 0.11, 0.14, 0.96],
        radii: [14.0; 4],
        transform: byard_core::frame::Transform::IDENTITY,
        smooth: 0.0,
    });

    let x = panel_x + padding;
    let mut y = panel_y + padding + line_height;

    let title = if errors.len() == 1 {
        "Parse error".to_string()
    } else {
        format!("Parse errors ({})", errors.len())
    };
    frame.push_text(TextLine {
        x,
        y,
        text: title,
        font_size: 18.0,
        color: [1.0, 0.42, 0.42, 1.0],
        dirty: true,
    });
    y += line_height * 1.5;

    for err in &errors[..shown] {
        let headline = truncate_str(&err.headline(), OVERLAY_MAX_HEADLINE_CHARS);
        frame.push_text(TextLine {
            x,
            y,
            text: headline,
            font_size: 15.0,
            color: [1.0, 1.0, 1.0, 1.0],
            dirty: true,
        });
        y += line_height * 1.2;
    }

    if truncated {
        y += line_height * 0.3;
        frame.push_text(TextLine {
            x,
            y,
            text: format!("… and {} more error(s)", errors.len() - OVERLAY_MAX_ERRORS),
            font_size: 13.0,
            color: [0.6, 0.6, 0.6, 1.0],
            dirty: true,
        });
        y += line_height;
    }

    frame.push_text(TextLine {
        x,
        y: y + line_height * 0.6,
        text: "Fix the file and save to dismiss, the last good view is behind this.".to_string(),
        font_size: 13.0,
        color: [0.55, 0.55, 0.58, 1.0],
        dirty: true,
    });
}

/// Truncates `s` to at most `max_chars` Unicode scalar values, appending "…"
/// if truncated. Operates on chars to avoid splitting multi-byte sequences.
fn truncate_str(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let mut out = String::with_capacity(s.len().min(max_chars + 3));
    let mut count = 0;
    loop {
        match chars.next() {
            Some(c) if count < max_chars => {
                out.push(c);
                count += 1;
            }
            Some(_) => {
                out.push('…');
                break;
            }
            None => break,
        }
    }
    out
}

// ── Platform host (winit integration) ────────────────────────────────────────

struct App {
    engine: Option<Engine>,
    /// The manifest's project name. Decides where the `Store` capability
    /// writes (RFC-0029 O5): keyed on the project rather than on the path, so
    /// a store does not move when the directory does.
    project: String,
    width_bits: Option<Arc<AtomicU32>>,
    height_bits: Option<Arc<AtomicU32>>,
    /// Mirror of the logic thread's active-animation set (RFC-0010), read by the
    /// event loop to decide whether to keep spinning.
    animating: Option<Arc<AtomicBool>>,
    /// The `byard dev [file]` override, threaded into the watcher's
    /// re-resolve closure so it re-discovers the same manifest.
    file_override: Option<PathBuf>,
    /// Directories the watcher covers: project sources + `path` deps (D-J).
    watch_paths: Vec<PathBuf>,
    /// Persistent MSDF field cache (`.byard/cache/vectors/`, RFC-0009 §5, M52),
    /// installed on the interpreter so cold starts skip regeneration.
    vector_cache_dir: PathBuf,
    initial_views: Vec<ViewDecl>,
    initial_errors: Vec<CompileError>,
    /// The resolved design-token theme (RFC-0022), installed on the interpreter
    /// so `inject Theme as t` resolves and `t.token` references paint.
    initial_theme: byard_compiler::interp::theme::Theme,
    /// A `--deep-link <url>` handed in at startup (RFC-0026): delivered to the
    /// interpreter once the tree is lowered, exactly as an OS intent would be.
    deep_link: Option<String>,
    /// This (render/main) thread's telemetry ring, its own `encode.frame`
    /// scope (RFC-0030 §I1) plus the asynchronously-resolved `gpu.*` pass
    /// samples, drained on **every** redraw, not just when about to print,
    /// so it only ever holds samples produced since the *previous redraw*.
    /// `byard dev`'s Poll-mode loop redraws far more often than once a
    /// second; draining only at print time would let a whole second's worth
    /// of samples pile up into one inflated, unreadable dump (RFC-0013's
    /// overlay is meant to be a per-frame snapshot, not an accumulator).
    last_render_telemetry: byard_core::telemetry::SampleBlock,
    /// Last time the telemetry overlay was printed (RFC-0013 "Overlay
    /// format"), throttled to roughly once a second so `byard dev` doesn't
    /// spam a line for every redraw. Printing is throttled; draining
    /// `last_render_telemetry` (above) is not.
    last_telemetry_print: std::time::Instant,
    /// The startup header, taken and printed once the device exists so the
    /// adapter and the frame budget can be part of it.
    header: Option<Header>,
    /// The `[dev]` table (RFC-0030 §V2).
    dev: crate::manifest::DevConfig,
    /// Whether `--profile` asked for the expanded block up front.
    want_profile: bool,
    /// The anchored statusline (RFC-0030 §P5–P6). Inert when stderr is not a
    /// terminal, in which case the per-second summary below takes over.
    ///
    /// Built in `on_resume`, not before: the frame budget it charts against is
    /// the display's refresh interval, and there is no display until the window
    /// exists (§Q3).
    statusline: Option<crate::statusline::StatusLine>,
    /// Hot reloads applied this session, published by the logic thread.
    reloads: Arc<AtomicU32>,
    /// A structure-incompatible reload held behind the gesture gate
    /// (RFC-0006 C1), published by the logic thread.
    reload_pending: Arc<AtomicBool>,
    /// The `Mod+Shift+D` toggle, mirrored to the logic thread.
    hud_visible: Arc<AtomicBool>,
    /// The sender half of the HUD's telemetry feed, taken in `on_resume`.
    hud_telemetry: Option<crossbeam_channel::Sender<crate::hud::HudTelemetry>>,
    /// When the previous redraw happened, so the sparkline plots the frame
    /// *period*, which is what a developer sees, rather than the sum of the
    /// scopes that happened to be instrumented.
    last_frame: std::time::Instant,
    /// The `--trace <path>` writer (RFC-0030 §V5), or `None`.
    ///
    /// Both rings are streamed into it as they are drained, on the thread that
    /// drains them, nothing is held back, and the file on disk is a complete
    /// JSON array after every frame, so a session that is `Ctrl-C`'d still
    /// leaves a trace every viewer will open. That is usually the session you
    /// most want to look at.
    trace: Option<crate::trace::TraceWriter>,
}

impl App {
    /// Starts the file watcher and returns the two channels the logic thread
    /// drains: the latest-wins reload channel and the vector-asset changes.
    ///
    /// Extracted from `on_resume` to keep it under the line-count lint, and
    /// because it is the one part of resume that has nothing to do with the
    /// GPU.
    fn start_file_watcher(
        &self,
    ) -> Result<
        (
            Arc<LatestWins<ParsedFile>>,
            crossbeam_channel::Receiver<std::path::PathBuf>,
        ),
        ByardError,
    > {
        // Hot-reload channel (RFC-0006 §3.5, D10).
        let reload_channel = Arc::new(LatestWins::<ParsedFile>::new());
        // Watcher lifetime tied to App; Arc shared with logic thread (C5).
        let watcher_channel = Arc::clone(&reload_channel);
        // Vector-asset (`.svg`) change channel: the watcher forwards changed
        // paths, the logic thread drains them each tick and invalidates the
        // matching MSDF field so it regenerates live (RFC-0009 §3, M47).
        let (asset_tx, asset_rx) = crossbeam_channel::unbounded::<std::path::PathBuf>();
        let file_override = self.file_override.clone();
        let watcher = start_watcher(&self.watch_paths, watcher_channel, asset_tx, move || {
            reresolve(file_override.as_deref())
        })
        .map_err(|e| ByardError::RenderSurface(format!("file watcher error: {e}")))?;
        // Keep the watcher alive for the entire process lifetime. Intentional:
        // file watching must persist even if the logic thread is restarted by
        // a structure-incompatible reload.
        std::mem::forget(watcher);
        Ok((reload_channel, asset_rx))
    }

    /// The non-TTY fallback for the statusline: **one plain line** per second
    /// (RFC-0030 §"The statusline").
    ///
    /// `byard dev 2>&1 | tee log` has to produce a readable log, and the
    /// multi-line block this replaces did the opposite, three hundred blocks
    /// in a five-minute session, whose practical effect was to bury the parse
    /// errors a developer actually needed to read. One line per second is
    /// greppable, diffable, and still carries the split that matters.
    ///
    /// The full per-scope breakdown is not gone; it moves behind `--profile`.
    fn print_plain_summary(
        engine: &Engine,
        logic: &byard_core::telemetry::SampleBlock,
        render: &byard_core::telemetry::SampleBlock,
        frame_ns: u64,
        last_print: &mut std::time::Instant,
    ) {
        if last_print.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        *last_print = std::time::Instant::now();
        let census = engine.latest_census();
        let atlas = engine.latest_atlas_paths();
        let idle_ns = crate::statusline::idle_ns(logic, render);
        crate::style::info(&format!(
            "frame {:.1}ms · work {:.1}ms · idle {:.1}ms · {} boxes · layout {}",
            ns_ms(frame_ns),
            ns_ms(crate::statusline::work_ns(logic, render, idle_ns)),
            ns_ms(idle_ns),
            census.instances,
            if atlas.populate_calls == 0 {
                "idle"
            } else if atlas.clears > 0 {
                "rebuild"
            } else {
                "retained"
            },
        ));
    }
}

/// Builds one HUD reading from the frame that just presented (RFC-0030 §V4).
///
/// # The subtraction, in one place
///
/// The HUD's cost is **not** the inclusive time of the `hud.render` scope. That
/// scope bounds the HUD's work on the logic thread and cannot bound its work on
/// the render thread, where its text is shaped, it has been dropped by then.
/// Reporting it as the HUD's cost under-reported by most of the real figure and
/// let §V4's own 5 % gate read as comfortably passed.
///
/// So the cost is `owner_total_ns(DevTools)` across **both** threads: every
/// nanosecond any scope spent on the dev runner's behalf, wherever in the scope
/// forest it landed. And, symmetrically, every figure the HUD publishes about
/// the *app* is built from `Owner::App` samples only, so the app's rows do not
/// silently absorb the profiler's second interpreter.
///
/// The reading is one frame behind, which is what §V4 specifies: the cost of
/// drawing the HUD is not known until it has been drawn.
fn hud_reading(
    logic: &byard_core::telemetry::SampleBlock,
    render: &byard_core::telemetry::SampleBlock,
    statusline: &StatusLine,
    engine: &Engine,
    frame_ns: u64,
) -> crate::hud::HudTelemetry {
    use byard_core::telemetry::Owner;
    let hud_cost_ns =
        logic.owner_total_ns(Owner::DevTools) + render.owner_total_ns(Owner::DevTools);
    let idle_ns = crate::statusline::idle_ns(logic, render);
    let census = engine.latest_census();
    let (retained, window, fps, spark) = statusline.hud_fields();
    crate::hud::HudTelemetry {
        fps,
        frame_ns,
        budget_ns: statusline.budget_ns(),
        // No `saturating_sub` anywhere below: `work_ns` and the per-scope
        // totals are already app-only, because the samples say so.
        work_ns: crate::statusline::work_ns(logic, render, idle_ns),
        interp_ns: scope_total(logic, "interp.render"),
        layout_ns: scope_total(logic, "layout.taffy"),
        encode_ns: scope_total(render, "encode.frame"),
        gpu_ns: render.sum_by_kind(byard_core::telemetry::ScopeKind::Gpu),
        boxes: census.instances,
        texts: census.texts,
        vectors: census.vectors,
        retained,
        window,
        spark,
        hud_cost_ns,
    }
}

/// Total inclusive time of every depth-0, **app-owned** sample named `scope`.
///
/// The owner filter is what stops the HUD's own `interp.render` from being
/// added to the app's when both ran on this thread this frame (RFC-0030 §V4).
fn scope_total(block: &byard_core::telemetry::SampleBlock, scope: &str) -> u64 {
    block
        .samples
        .iter()
        .filter(|s| {
            s.depth() == 0
                && s.owner() == byard_core::telemetry::Owner::App
                && byard_core::telemetry::scope_name(s.scope) == Some(scope)
        })
        .map(byard_core::telemetry::Sample::duration_ns)
        .sum()
}

/// Nanoseconds as milliseconds. Frame times never approach `f64`'s limit.
#[allow(clippy::cast_precision_loss)]
fn ns_ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

/// The frame budget when nothing better is known: 60 Hz.
const DEFAULT_FRAME_BUDGET_NS: u64 = 16_667_000;

/// Resolves the frame budget and says where it came from (RFC-0030 §Q3).
///
/// A pinned `[dev] frame_budget` wins, that is what pinning is for, and CI
/// needs a value that does not depend on the runner's panel. Otherwise the
/// display's refresh interval, because the budget answers "will this app drop
/// frames on *this* machine". The 60 Hz fallback is last and says so, so a
/// developer on a 120 Hz panel whose platform could not report the rate is not
/// quietly told their 12 ms frame is comfortable.
fn resolve_budget(
    dev: &crate::manifest::DevConfig,
    refresh_mhz: Option<u32>,
) -> (u64, &'static str) {
    if let Some(ns) = dev.frame_budget_ns {
        return (ns, "byard.toml [dev] frame_budget");
    }
    match refresh_mhz {
        // millihertz → nanoseconds per frame.
        Some(mhz) if mhz > 0 => (1_000_000_000_000 / u64::from(mhz), "display refresh"),
        _ => (DEFAULT_FRAME_BUDGET_NS, "60Hz assumed, display unknown"),
    }
}

/// Everything the startup header says, held until the device exists.
struct Header {
    project: String,
    entry: String,
    /// Comma-joined package names, empty when there are none.
    packages: String,
    views: usize,
    /// Pre-rendered diagnostic first lines (RFC-0006 **C7**), if the initial
    /// resolve failed.
    errors: Vec<CompileError>,
    /// The source map they point into, so the header can draw caret blocks.
    source_map: byard_compiler::resolve::SourceMap,
    started: std::time::Instant,
}

impl Header {
    /// Four facts, a result and a duration (RFC-0030 §"Starting the dev
    /// runner").
    ///
    /// The `gpu` fact answers up front the question a developer would otherwise
    /// ask twenty minutes in, *why is there no GPU timing?*, instead of
    /// printing "GPU timing unavailable" on every subsequent readout. The
    /// `budget` fact removes any ambiguity about which number the bars and the
    /// sparkline are drawn against. The `keys` fact makes the dev chords
    /// discoverable without documentation.
    fn print(self, budget_ns: u64, budget_source: &str, engine: &Engine) {
        crate::style::fact("project", &self.project);
        crate::style::fact("entry", &self.entry);
        if !self.packages.is_empty() {
            crate::style::fact("packages", &self.packages);
        }
        let gpu = if engine.gpu_timing_available() {
            "timestamp-query ✓".to_string()
        } else {
            "timestamp-query unavailable, gpu rows read `unknown`, never `0`".to_string()
        };
        crate::style::fact("gpu", &gpu);
        crate::style::fact(
            "budget",
            &format!("{:.1}ms  ({budget_source})", ns_ms(budget_ns)),
        );
        crate::style::fact(
            "keys",
            "Mod+Shift+D  in-window HUD  ·  Mod+Shift+P  expanded profile block",
        );

        if self.errors.is_empty() {
            crate::style::ok(
                &format!("{} view(s), 0 errors", self.views),
                Some(self.started.elapsed()),
            );
        } else {
            crate::commands::check::print_diagnostics(&self.errors, &self.source_map, false);
            crate::style::err(&format!(
                "{} error(s), see the overlay in the window",
                self.errors.len()
            ));
        }
        crate::style::action("watching for changes");
    }
}

impl PlatformHost for App {
    fn on_resume(
        &mut self,
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        size: WindowSize,
        waker: byard_core::relay::FrameWaker,
    ) -> Result<(), ByardError> {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let w = size.width as f32 / size.scale_factor as f32;
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let h = size.height as f32 / size.scale_factor as f32;
        let width_bits = Arc::new(AtomicU32::new(w.to_bits()));
        let height_bits = Arc::new(AtomicU32::new(h.to_bits()));
        let w_clone = Arc::clone(&width_bits);
        let h_clone = Arc::clone(&height_bits);
        // Seeded `true` so the first frames are always drawn; the logic thread
        // takes over the moment it has rendered once.
        let animating = Arc::new(AtomicBool::new(true));
        let animating_logic = Arc::clone(&animating);
        let reloads_logic = Arc::clone(&self.reloads);
        let reload_pending_logic = Arc::clone(&self.reload_pending);
        let hud_visible_logic = Arc::clone(&self.hud_visible);
        // Bounded at one, latest-wins: the HUD wants the newest reading, and a
        // queue of stale ones would only delay it.
        let (hud_tx, hud_rx) = crossbeam_channel::bounded(1);
        self.hud_telemetry = Some(hud_tx);

        let (reload_channel, asset_rx) = self.start_file_watcher()?;

        let initial_views = self.initial_views.clone();
        let initial_theme = self.initial_theme.clone();
        let initial_errors = if self.initial_errors.is_empty() {
            None
        } else {
            Some(self.initial_errors.clone())
        };
        // Seeded to match, so a session that starts broken does not spend its
        // first frame asking for a redraw it does not need, and a session that
        // starts clean still gets one the moment it breaks.
        let initial_errors_present = initial_errors.is_some();

        let mut engine = pollster::block_on(Engine::init(
            instance,
            surface,
            size.width,
            size.height,
            size.scale_factor,
        ))?;
        // `byard dev` runs in Poll mode (redraws every iteration for hot-reload),
        // so the waker is not strictly required, installing it is still correct
        // and makes input-driven redraws prompt if the mode ever changes.
        // The logic thread keeps its own handle: the relay only wakes on an
        // input-bearing tick, and a hot reload (or an animation starting from a
        // non-input source) also has to reach a sleeping loop.
        let waker_for_logic = waker.clone();
        engine.set_frame_waker(waker);

        // RFC-0009 §2-C: the render thread reports which vector-atlas
        // uploads it actually applied through this channel, so the dev JIT
        // cache (on the logic thread) knows when to stop re-sending one.
        let (vector_ack_tx, vector_ack_rx) = crossbeam_channel::unbounded();
        engine.set_vector_ack_sender(vector_ack_tx);
        let vector_cache_dir = self.vector_cache_dir.clone();
        let deep_link = self.deep_link.clone();
        // RFC-0028 §3: the capabilities a `.byd` file may `inject`, bundled
        // with the pool that runs them and the channel their replies come back
        // on. Built here, on the main thread, because it has to be `Send` into
        // the logic-thread factory below and cannot be built inside it.
        let dispatcher = engine.dispatcher(crate::capabilities::registry(&self.project));

        engine.start_logic_from_view(move |_arena| {
            let (mut interp, tree, current_views) = if initial_views.is_empty() {
                let mut interp = Interpreter::new();
                interp.set_dispatcher(dispatcher);
                interp.set_theme(initial_theme);
                (interp, vec![], vec![])
            } else {
                let mut interp = Interpreter::new();
                // Both of these provide ambient values, and both must land
                // before lowering: `inject` resolves against the ambient chain
                // at lower time, so anything provided afterwards is invisible
                // to the view that asked for it (RFC-0022, RFC-0028 §3).
                interp.set_dispatcher(dispatcher);
                interp.set_theme(initial_theme);
                interp.load_views(&initial_views);
                let known: Vec<&str> = initial_views.iter().map(|v| v.name.as_str()).collect();
                let tree = interp.lower_view(&initial_views[0], &known);
                interp.tick();
                (interp, tree, initial_views)
            };
            interp.set_vector_ack_receiver(vector_ack_rx);
            interp.set_vector_cache_dir(vector_cache_dir);

            Box::new(ByldRuntime {
                interp,
                tree,
                current_views,
                reload_channel,
                asset_changes: asset_rx,
                pending_reload: PendingReload::new(reload_pending_logic),
                error_state: initial_errors,
                width_bits: w_clone,
                height_bits: h_clone,
                start: std::time::Instant::now(),
                reported_perf: std::collections::HashSet::new(),
                reload_count: 0,
                reloads_pub: reloads_logic,
                animating: animating_logic,
                waker: waker_for_logic,
                flash: ReloadFlash::default(),
                // Built here rather than handed in: the HUD owns an
                // `Interpreter`, which is `!Send` by design (RFC-0001 §5), so
                // it is constructed on the thread that will drive it. A HUD
                // that fails to parse is a defect in *this* repository, so it
                // is reported once and the session continues without it, a
                // developer should never lose their dev runner to a broken
                // diagnostic.
                hud: match crate::hud::DevHud::new() {
                    Ok(hud) => Some(hud),
                    Err(e) => {
                        crate::style::warn(&format!("the in-window HUD is unavailable: {e}"));
                        None
                    }
                },
                hud_visible: hud_visible_logic,
                hud_telemetry: hud_rx,
                overlay_was_up: initial_errors_present,
                pending_deep_link: deep_link,
            })
        })?;

        // RFC-0030 §Q3/§V2: the budget is the display's refresh interval unless
        // `[dev] frame_budget` pinned one, and it is printed in the startup
        // header so it is never ambiguous which number a bar is drawn against.
        let (budget_ns, budget_source) = resolve_budget(&self.dev, size.refresh_rate_mhz);
        if let Some(header) = self.header.take() {
            header.print(budget_ns, budget_source, &engine);
        }

        let mut statusline = StatusLine::new(self.dev.statusline, budget_ns);
        if self.want_profile {
            statusline.set_profile(true);
        }
        self.statusline = Some(statusline);

        self.engine = Some(engine);
        self.width_bits = Some(width_bits);
        self.height_bits = Some(height_bits);
        self.animating = Some(animating);
        Ok(())
    }

    /// RFC-0010's active set, read from the logic thread's published flag: while
    /// something is animating the loop keeps requesting frames, and once
    /// everything has settled it sleeps until the next input, file save or
    /// published frame. Before the first render (`None`) it keeps spinning.
    fn wants_frames(&self) -> bool {
        self.animating
            .as_ref()
            .is_none_or(|flag| flag.load(Ordering::Relaxed))
    }

    fn on_resize(&mut self, size: WindowSize) {
        if let Some(e) = self.engine.as_mut() {
            e.on_resize(size.width, size.height, size.scale_factor);
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let w = size.width as f32 / size.scale_factor as f32;
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let h = size.height as f32 / size.scale_factor as f32;
            if let Some(b) = &self.width_bits {
                b.store(w.to_bits(), Ordering::Relaxed);
            }
            if let Some(b) = &self.height_bits {
                b.store(h.to_bits(), Ordering::Relaxed);
            }
        }
    }

    fn on_redraw(&mut self) -> Result<(), ByardError> {
        if let Some(e) = self.engine.as_mut() {
            e.render_latest()?;
            // Drained every redraw (see `last_render_telemetry`'s doc comment),
            // independent of any print throttle.
            self.last_render_telemetry = byard_core::telemetry::drain_samples();
            let logic = e.latest_cpu_telemetry().unwrap_or_default();
            // RFC-0030 §V5: stream both rings into the trace as they are
            // drained. The logic thread's block rides in on the frame it was
            // captured with, so it is written from here too, the writer is a
            // plain file handle and the render thread is the only one holding
            // it, which is what keeps this off any lock.
            if let Some(trace) = self.trace.as_mut() {
                trace.write_frame(Some(&logic), &self.last_render_telemetry);
            }

            // The frame *period*, which is what a developer perceives, not the
            // sum of whichever scopes happen to be instrumented, which would
            // quietly shrink every time a scope was removed.
            let frame_ns = u64::try_from(self.last_frame.elapsed().as_nanos()).unwrap_or(u64::MAX);
            self.last_frame = std::time::Instant::now();

            let animating = self
                .animating
                .as_ref()
                .is_none_or(|f| f.load(Ordering::Relaxed));
            if let Some(sl) = self.statusline.as_mut() {
                sl.on_frame(crate::statusline::FrameInputs {
                    frame_ns,
                    logic: &logic,
                    render: &self.last_render_telemetry,
                    census: e.latest_census(),
                    atlas: e.latest_atlas_paths(),
                    animating,
                    reloads: self.reloads.load(Ordering::Relaxed),
                    reload_pending: self.reload_pending.load(Ordering::Relaxed),
                    gpu_available: e.gpu_timing_available(),
                    text_reshapes: e.last_text_reshapes(),
                });
            }

            if self.hud_visible.load(Ordering::Relaxed) {
                if let (Some(tx), Some(sl)) =
                    (self.hud_telemetry.as_ref(), self.statusline.as_ref())
                {
                    // `try_send` on a bounded(1) channel: if the logic thread
                    // has not taken the previous reading yet, this one is
                    // dropped rather than queued. The HUD wants the newest
                    // number, and a backlog of stale ones would only delay it.
                    let _ = tx.try_send(hud_reading(
                        &logic,
                        &self.last_render_telemetry,
                        sl,
                        e,
                        frame_ns,
                    ));
                }
            }

            if !self.statusline.as_ref().is_some_and(StatusLine::is_enabled) {
                App::print_plain_summary(
                    e,
                    &logic,
                    &self.last_render_telemetry,
                    frame_ns,
                    &mut self.last_telemetry_print,
                );
            }
        }
        Ok(())
    }

    /// Dev chords, consumed before the router ever sees them (RFC-0030 §V3).
    ///
    /// A chord that reached `dispatch_events` would be indistinguishable from
    /// the user typing into the app under test, which is exactly why it is a
    /// `Mod+Shift` chord and not a bare function key (§Q1).
    fn on_chord(&mut self, key: &str, pressed: bool, mods: byard_core::KeyModifiers) -> bool {
        if !mods.is_dev_chord() {
            return false;
        }
        // Matched case-insensitively: `Shift` is part of the chord, so the
        // platform reports the character as `P` on some layouts and `p` on
        // others, and a chord that works on one keyboard and not another is
        // worse than no chord.
        let Some(sl) = self.statusline.as_mut() else {
            return false;
        };
        if key.eq_ignore_ascii_case("p") {
            // Only on press: acting on the release too would toggle twice and
            // land back where it started, which reads as the chord not working.
            if pressed {
                sl.set_profile(!sl.is_profiling());
            }
            return true;
        }
        if key.eq_ignore_ascii_case("d") {
            if pressed {
                let now = !self.hud_visible.load(Ordering::Relaxed);
                self.hud_visible.store(now, Ordering::Relaxed);
                // The logic thread may be parked with nothing to animate, and
                // the chord is not an input event it will ever see, so the
                // frame that mounts or dismisses the HUD has to be asked for.
                if let Some(e) = self.engine.as_ref() {
                    e.push_input(byard_core::platform::InputEvent {
                        kind: byard_core::platform::EventKind::PointerMove,
                        pos: (-1.0, -1.0),
                        delta: (0.0, 0.0),
                        payload: None,
                        time_ms: now_ms(),
                    });
                }
            }
            return true;
        }
        false
    }

    fn on_pointer_input(&mut self, button: PointerButton, state: PointerState, x: f32, y: f32) {
        if let Some(engine) = &self.engine {
            let kind = match state {
                PointerState::Pressed => byard_core::platform::EventKind::PointerDown,
                PointerState::Released => byard_core::platform::EventKind::PointerUp,
            };
            // The router only consults this on `PointerDown` (RFC-0012
            // `secondary`): a right-button press flags the whole down→up
            // gesture as `secondary` instead of `tap`. Without it every
            // button reports as a plain left-click.
            let payload = (button == PointerButton::Right)
                .then_some(byard_core::platform::InputPayload::Bool(true));
            engine.push_input(byard_core::platform::InputEvent {
                kind,
                pos: (x, y),
                delta: (0.0, 0.0),
                payload,
                time_ms: now_ms(),
            });
        }
    }

    fn on_cursor_moved(&mut self, x: f32, y: f32) {
        if let Some(engine) = &self.engine {
            engine.push_input(byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::PointerMove,
                pos: (x, y),
                delta: (0.0, 0.0),
                payload: None,
                time_ms: now_ms(),
            });
        }
    }

    fn on_key(&mut self, key: &str, pressed: bool) {
        if let Some(engine) = &self.engine {
            let kind = if pressed {
                byard_core::platform::EventKind::KeyDown
            } else {
                byard_core::platform::EventKind::KeyUp
            };
            // The router keys `Tab` traversal (M18) and `Backspace`/edit
            // handling (M17) off this payload, dropping it here silently
            // breaks both, since every key would otherwise look identical.
            engine.push_input(byard_core::platform::InputEvent {
                kind,
                pos: (0.0, 0.0),
                delta: (0.0, 0.0),
                payload: Some(byard_core::platform::InputPayload::Key(key.to_string())),
                time_ms: now_ms(),
            });
        }
    }

    fn on_text(&mut self, text: &str) {
        if let Some(engine) = &self.engine {
            engine.push_input(byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::TextInput,
                pos: (0.0, 0.0),
                delta: (0.0, 0.0),
                payload: Some(byard_core::platform::InputPayload::Key(text.to_string())),
                time_ms: now_ms(),
            });
        }
    }

    fn on_scroll(&mut self, dx: f32, dy: f32, x: f32, y: f32) {
        if let Some(engine) = &self.engine {
            engine.push_input(byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::Scroll,
                pos: (x, y),
                delta: (dx, dy),
                payload: None,
                time_ms: now_ms(),
            });
        }
    }

    fn on_wheel(&mut self, dx: f32, dy: f32, x: f32, y: f32) {
        if let Some(engine) = &self.engine {
            engine.push_input(byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::Wheel,
                pos: (x, y),
                delta: (dx, dy),
                payload: None,
                time_ms: now_ms(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn views(name: &str) -> Vec<ViewDecl> {
        vec![ViewDecl {
            name: byard_compiler::Symbol::intern(name),
            params: Vec::new(),
            body: Vec::new(),
            span: byard_compiler::Span::new(0, 0),
        }]
    }

    /// RFC-0006 **C1**. The indicator and the gate are one type precisely so
    /// they cannot disagree; these are the transitions that would let them.
    #[test]
    fn the_pending_flag_is_set_and_cleared_around_a_gated_reload() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut gate = PendingReload::new(Arc::clone(&flag));

        // Nothing waiting: no flag, nothing to take.
        assert!(!flag.load(Ordering::Relaxed));
        assert!(gate.take_if_released(false).is_none());
        assert!(!flag.load(Ordering::Relaxed));

        // A save lands mid-gesture.
        assert!(
            gate.defer(views("A"), ReloadKind::StructureIncompatible),
            "the first defer is a rising edge and must wake an idle loop"
        );
        assert!(flag.load(Ordering::Relaxed));

        // Still holding the pointer: the reload stays behind the gate and the
        // indicator stays up.
        assert!(gate.take_if_released(true).is_none());
        assert!(
            flag.load(Ordering::Relaxed),
            "the indicator must not clear while the reload is still waiting"
        );

        // Released: the reload comes out and the indicator goes down in the
        // same step.
        let taken = gate.take_if_released(false).expect("the deferred reload");
        assert_eq!(taken.0[0].name.as_str(), "A");
        assert!(
            !flag.load(Ordering::Relaxed),
            "the indicator must clear exactly where the reload is consumed"
        );
        assert!(gate.take_if_released(false).is_none());
    }

    #[test]
    fn a_second_defer_supersedes_the_first_and_is_not_a_second_rising_edge() {
        // Applying the older views afterwards would undo an edit the developer
        // has already made, the gate holds the *latest* save, like the
        // latest-wins channel feeding it.
        let flag = Arc::new(AtomicBool::new(false));
        let mut gate = PendingReload::new(Arc::clone(&flag));
        assert!(gate.defer(views("old"), ReloadKind::StructureIncompatible));
        assert!(
            !gate.defer(views("new"), ReloadKind::StructureIncompatible),
            "the indicator is already up; waking the loop again is noise"
        );
        let taken = gate.take_if_released(false).expect("the deferred reload");
        assert_eq!(taken.0[0].name.as_str(), "new");
        assert!(!flag.load(Ordering::Relaxed));
    }

    /// RFC-0030 §Q3. The budget is what every bar is drawn against, so where it
    /// came from has to be unambiguous, and a pinned value has to win, or
    /// pinning it in CI would do nothing.
    #[test]
    fn the_frame_budget_prefers_a_pin_then_the_display_then_says_it_guessed() {
        let pinned = crate::manifest::DevConfig {
            frame_budget_ns: Some(8_000_000),
            ..Default::default()
        };
        assert_eq!(
            resolve_budget(&pinned, Some(120_000)),
            (8_000_000, "byard.toml [dev] frame_budget")
        );

        // 120 Hz is 8.33ms. A tool that drew bars against 16.7ms here would
        // report a comfortable frame for one that visibly stutters.
        let (ns, source) = resolve_budget(&crate::manifest::DevConfig::default(), Some(120_000));
        assert_eq!(ns, 8_333_333);
        assert_eq!(source, "display refresh");

        let (ns, source) = resolve_budget(&crate::manifest::DevConfig::default(), Some(60_000));
        assert_eq!(ns, 16_666_666);
        assert_eq!(source, "display refresh");

        // No refresh rate reported: fall back, and *say so*, so a developer on
        // a 120 Hz panel is not quietly told their 12ms frame is comfortable.
        let (ns, source) = resolve_budget(&crate::manifest::DevConfig::default(), None);
        assert_eq!(ns, DEFAULT_FRAME_BUDGET_NS);
        assert!(source.contains("assumed"), "{source}");
        assert_eq!(
            resolve_budget(&crate::manifest::DevConfig::default(), Some(0)).0,
            DEFAULT_FRAME_BUDGET_NS,
            "a zero refresh rate is not a budget of infinity"
        );
    }
}
