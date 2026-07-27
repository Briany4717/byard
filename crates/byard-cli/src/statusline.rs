//! The `byard dev` statusline (RFC-0030 §P5–P6).
//!
//! One line, anchored to the bottom of the terminal, redrawn in place while the
//! event log scrolls above it.
//!
//! ```text
//!  ● 60fps  work 3.4ms · idle 13.5ms  ▁▂▃▂▁▂▅▂▁  382 boxes  ↻14  retained 59/60
//! ```
//!
//! # Why a line and not a block
//!
//! `byard dev` used to `eprint!` a multi-line telemetry block once a second,
//! forever. In a five-minute session that is three hundred blocks of scrolling
//! text, and its practical effect was to **bury parse errors** under timing
//! noise. Continuous measurement and discrete events are different media and
//! must not share a scroll region.
//!
//! The fix is not a TUI. A full-screen TUI takes the alternate screen buffer,
//! which destroys scrollback — and scrollback is where parse errors live. This
//! is `cargo`'s model for the same reason: an ordinary scrolling log, plus one
//! anchored line.
//!
//! # `work` / `idle`, not `cpu | gpu | interp`
//!
//! RFC-0030's original field set predates the `present.*` scopes and cannot
//! distinguish an engine that finished early from one that overran — which is
//! the entire reading of a frame. `idle` is `present.acquire`, the wall time
//! the display made the engine wait; `work` is everything else. Once a scene is
//! vsync-bound every further engine win shows up as *more idle* rather than a
//! smaller total, so a statusline whose headline number is the total has a
//! headline number that cannot move. See the erratum in RFC-0030.
//!
//! # `retained N/M`
//!
//! The answer to *"am I on the fast path?"*, read rather than inferred. A view
//! that rebuilds its layout tree every frame becomes something a developer sees
//! and usually fixes, instead of something they deduce from a timing that never
//! got smaller.
//!
//! # The terminal is left as it was found (INV-25)
//!
//! This module writes exactly two kinds of byte sequence: SGR colour (always
//! closed by a reset within the same write) and `\r\x1b[2K` (carriage return,
//! erase line). It **never** hides the cursor, never touches the scroll region,
//! and never enters the alternate screen.
//!
//! That is a deliberate design constraint rather than an omission. A `Drop`
//! guard does not run on `SIGINT` — the process is terminated, not unwound — so
//! any terminal state this module could set is state it might fail to restore.
//! By only ever writing state that is already scoped to the line being drawn,
//! the terminal is left usable on *every* exit path, including `kill -9`, and
//! the `Drop` guard is a courtesy (it erases the stale line) rather than the
//! thing correctness depends on.
//!
//! # Streams (§Q2)
//!
//! Statusline → **stderr**. Event log → **stdout**. `byard dev > session.log`
//! therefore yields a readable, control-character-free log, and `byard dev
//! 1>/dev/null` yields the live display alone. `byard check` and `byard build`
//! keep the opposite, CI-shaped contract (diagnostics on stderr, non-zero exit)
//! — two commands, two contracts, each matching what its consumer actually is.

use std::io::{IsTerminal, Write};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use byard_core::Census;
use byard_core::telemetry::{SampleBlock, scope_name};

use crate::style::{Glyphs, Palette, display_width, glyphs, palette};

/// Frame-time history, in samples. Fixed at 24 (§Q5): it is the width that fits
/// alongside every other field inside 80 columns, and at 60 fps it is 400 ms of
/// history — long enough that a hitch is still on screen when the developer
/// looks up, short enough that the display tracks the present. A configurable
/// sparkline width is a knob with no correct setting and therefore no reason to
/// turn.
const SPARK_LEN: usize = 24;
/// The most the sparkline will ever widen to when `COLUMNS` is generous.
const SPARK_MAX: usize = 64;
/// How many frames the `retained` field looks back over. Sixty is one second at
/// 60 fps, which makes `retained 59/60` read as "one rebuild in the last
/// second" without the reader doing arithmetic.
const RETAINED_WINDOW: u32 = 64;
/// The composed width when `COLUMNS` says nothing (§Q7).
const DEFAULT_WIDTH: usize = 80;
/// Below this, nothing useful can be composed, so nothing is.
const MIN_WIDTH: usize = 24;
/// Repaint cadence. Fast enough to read an fps change, slow enough that the
/// write is invisible (≈ 120 bytes × 10/s).
const REPAINT_INTERVAL: Duration = Duration::from_millis(100);
/// The scope whose duration *is* the idle half of a frame.
const IDLE_SCOPE: &str = "present.acquire";

// ── The painter: the only thing that writes to the terminal ────────────────

/// The process-wide statusline painter.
///
/// A global, and deliberately so: the statusline is a property of the
/// *terminal*, and the terminal is a process-wide resource written to from both
/// the render thread (which repaints) and the logic thread (which logs perf
/// warnings and reloads). Funnelling every write through one mutex is what
/// makes "a log line never lands on top of the statusline" a structural
/// property instead of a thread-timing coincidence (§P5).
///
/// When no statusline is active every method is a no-op and [`log`] degrades to
/// a plain `println!`, which is exactly what a non-TTY or `[dev] statusline =
/// false` should produce.
static PAINTER: OnceLock<Mutex<Painter>> = OnceLock::new();

fn painter() -> MutexGuard<'static, Painter> {
    PAINTER
        .get_or_init(|| Mutex::new(Painter::default()))
        .lock()
        // A poisoned painter is a thread that panicked mid-log. The terminal is
        // still a terminal and the remaining threads still have things to say;
        // refusing to print from here would turn one panic into silence.
        .unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Painter {
    /// What is currently on screen: one line for the statusline, or
    /// [`crate::telemetry_overlay::PROFILE_LINES`] for the expanded block.
    /// Empty when nothing is drawn.
    drawn: String,
    /// Whether a statusline is installed at all.
    active: bool,
}

/// Erases a `lines`-tall block whose last line the cursor is sitting on,
/// leaving the cursor at column 0 of where the block started.
///
/// Walks *up*: clear this line, move up one, clear, repeat. This is why the
/// profile block's line count has to be constant — a block that grew between
/// repaints would leave its extra lines behind, and one that shrank would eat a
/// line of scrollback on every repaint, ten times a second.
fn erase_block(out: &mut impl Write, lines: usize) {
    if lines == 0 {
        return;
    }
    let _ = out.write_all(b"\r\x1b[2K");
    for _ in 1..lines {
        let _ = out.write_all(b"\x1b[1A\r\x1b[2K");
    }
}

/// Writes `block` with no trailing newline, leaving the cursor on its last
/// line.
///
/// Every line is preceded by its own erase, or a longer previous line shows
/// through the shorter one replacing it.
fn draw_block(out: &mut impl Write, block: &str) {
    for (i, line) in block.lines().enumerate() {
        if i > 0 {
            let _ = out.write_all(b"\n");
        }
        let _ = out.write_all(b"\r\x1b[2K");
        let _ = out.write_all(line.as_bytes());
    }
}

impl Painter {
    /// How many terminal lines are currently occupied.
    fn height(&self) -> usize {
        if self.active {
            self.drawn.lines().count()
        } else {
            0
        }
    }

    /// Erases what is on screen.
    fn erase(&self) {
        if self.height() == 0 {
            return;
        }
        let mut err = std::io::stderr().lock();
        erase_block(&mut err, self.height());
        let _ = err.flush();
    }

    /// Redraws whatever was last composed.
    fn draw(&self) {
        if self.height() == 0 {
            return;
        }
        let mut err = std::io::stderr().lock();
        draw_block(&mut err, &self.drawn);
        let _ = err.flush();
    }

    /// Replaces what is on screen, erasing the previous block first.
    ///
    /// The erase is the whole point and was missing in the first cut: without
    /// it a one-line statusline still worked — `\r\x1b[2K` clears the line the
    /// cursor is on — while the seventeen-line profile block appended sixteen
    /// fresh lines on every repaint, scrolling ten times a second and eating
    /// exactly the scrollback this design exists to protect.
    fn replace(&mut self, block: &str) {
        if !self.active {
            return;
        }
        let previous = self.height();
        self.drawn.clear();
        self.drawn.push_str(block);
        let mut err = std::io::stderr().lock();
        erase_block(&mut err, previous);
        draw_block(&mut err, &self.drawn);
        let _ = err.flush();
    }
}

/// Writes one event-log line to **stdout**, without the statusline landing on
/// top of it.
///
/// Every `style::` call routes through here, so no call site has to know a
/// statusline exists. With none installed this is a plain `println!`.
pub fn log(message: &str) {
    let p = painter();
    p.erase();
    println!("{message}");
    p.draw();
}

/// As [`log`], but for the lines whose contract is stderr — the
/// rustc-compatible diagnostic first line (RFC-0006 **C7**).
pub fn log_stderr(message: &str) {
    let p = painter();
    p.erase();
    {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{message}");
    }
    p.draw();
}

/// Erases the statusline for the rest of the process.
///
/// Called from [`StatusLine`]'s `Drop`. Not the thing correctness depends on —
/// see the module docs on INV-25 — but it is what makes a clean exit leave no
/// stale line behind.
fn retire() {
    let mut p = painter();
    p.erase();
    p.drawn.clear();
    p.active = false;
}

// ── The data the line is composed from ─────────────────────────────────────

/// One statusline's worth of numbers.
///
/// Split out from [`StatusLine`] so the composition is a pure function of its
/// inputs and can be asserted exhaustively — including at field values no real
/// session would reach, which is where a width bug actually lives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fields {
    /// Whether anything is animating (RFC-0010's active set).
    pub animating: bool,
    /// Redraws in the last measured interval.
    pub fps: u32,
    /// Frame time minus [`IDLE_SCOPE`] — what the engine actually did.
    pub work_ns: u64,
    /// [`IDLE_SCOPE`] — what the display made it wait.
    pub idle_ns: u64,
    /// Box-class draw instances in the last published frame.
    pub instances: usize,
    /// Hot reloads applied this session.
    pub reloads: u32,
    /// A structure-incompatible reload waiting behind the gesture gate
    /// (RFC-0006 C1).
    pub reload_pending: bool,
    /// Frames in the last window that took the retained layout path.
    pub retained: u32,
    /// How many frames that window actually holds yet.
    pub window: u32,
}

/// The fields that may be dropped when the terminal is narrower than the line,
/// **in the order they are dropped** (§P5).
///
/// The order is the inverse of how much each field says that nothing else says.
/// The sparkline goes first because it is the widest; the census next because a
/// box count is a slow-moving number a developer can ask for another way; the
/// reload count last among the droppable, because it is the cheapest possible
/// confirmation that the watcher is alive at all. `work`/`idle`, fps and the
/// animating dot are never dropped — a line that cannot show them is not worth
/// drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Detail {
    /// Everything.
    Full,
    /// No sparkline.
    NoSparkline,
    /// No sparkline, no census.
    NoCensus,
    /// No sparkline, no census, no reload count.
    NoReloads,
    /// The irreducible line: dot, fps, work/idle.
    Minimal,
}

impl Detail {
    const ALL: [Self; 5] = [
        Self::Full,
        Self::NoSparkline,
        Self::NoCensus,
        Self::NoReloads,
        Self::Minimal,
    ];
}

// ── Composition ────────────────────────────────────────────────────────────

/// Composes the line, dropping fields in the fixed order until it fits.
///
/// Never returns a string wider than `width`: an over-wide statusline wraps,
/// and a wrapped statusline breaks the in-place redraw permanently — every
/// subsequent repaint erases one line of a two-line display and the terminal
/// fills with debris.
fn compose(
    out: &mut String,
    f: &Fields,
    spark: &[u16],
    budget_ns: u64,
    width: usize,
    p: &Palette,
    g: &Glyphs,
) {
    for detail in Detail::ALL {
        out.clear();
        write_line(out, f, spark, budget_ns, width, p, g, detail);
        if display_width(out) <= width {
            return;
        }
    }
    // Even `Minimal` did not fit: truncate on a character boundary rather than
    // emit something that wraps. Colour is dropped entirely here — a truncated
    // escape sequence is the one way this module could corrupt a terminal.
    out.clear();
    write_line(
        out,
        f,
        spark,
        budget_ns,
        width,
        &Palette::plain(),
        g,
        Detail::Minimal,
    );
    truncate_to_width(out, width);
}

#[allow(clippy::too_many_arguments)]
fn write_line(
    out: &mut String,
    f: &Fields,
    spark: &[u16],
    budget_ns: u64,
    width: usize,
    p: &Palette,
    g: &Glyphs,
    detail: Detail,
) {
    use std::fmt::Write as _;

    // ● / ○ — a direct read of the flag that decides whether the event loop
    // sleeps. One relaxed load, and it says something no number beside it does.
    let (dot, dot_colour) = if f.animating {
        (g.dot_filled, p.ok)
    } else {
        (g.dot_hollow, p.dim)
    };
    let _ = write!(out, " {dot_colour}{dot}{} ", p.reset);
    let _ = write!(out, "{}{:>3}fps{}", p.metric, f.fps, p.reset);

    let _ = write!(
        out,
        "  work {}{}{} {}·{} idle {}{}{}",
        p.metric,
        fmt_ms(f.work_ns),
        p.reset,
        p.dim,
        p.reset,
        p.dim,
        fmt_ms(f.idle_ns),
        p.reset
    );

    if detail == Detail::Full {
        // The sparkline widens into whatever `COLUMNS` gave us and nothing
        // else does (§Q5). Everything left of here has a fixed width, so the
        // arithmetic is a subtraction rather than a layout pass.
        let fixed = display_width(out) + tail_width(f, detail, g);
        let room = width.saturating_sub(fixed + 2);
        let cells = room.clamp(0, SPARK_MAX).min(spark.len());
        if cells >= 4 {
            out.push_str("  ");
            write_sparkline(out, &spark[spark.len() - cells..], budget_ns, p, g);
        }
    }

    if detail <= Detail::NoSparkline {
        let _ = write!(out, "  {}{}{} boxes", p.metric, f.instances, p.reset);
    }
    if detail <= Detail::NoCensus {
        let _ = write!(out, "  {}{}{}{}", p.accent, g.reload, f.reloads, p.reset);
    }
    if detail <= Detail::NoReloads && f.window > 0 {
        // Coloured by the reading, not by the number: anything short of the
        // full window means a frame rebuilt its layout tree, which is the
        // finding worth noticing.
        let colour = if f.retained == f.window { p.ok } else { p.warn };
        let _ = write!(
            out,
            "  {colour}retained {}/{}{}",
            f.retained, f.window, p.reset
        );
    }
    // RFC-0006 C1. Always shown when set, at every detail level: a developer
    // whose edits have silently stopped applying needs to know that more than
    // they need any other field on this line.
    if f.reload_pending {
        let _ = write!(out, "  {}{} pending{}", p.warn, g.reload, p.reset);
    }
}

/// Display width of everything that follows the sparkline, so the sparkline can
/// be sized by subtraction.
fn tail_width(f: &Fields, detail: Detail, g: &Glyphs) -> usize {
    let mut w = 0;
    if detail <= Detail::NoSparkline {
        w += 2 + decimal_width(f.instances as u64) + 6; // "  382 boxes"
    }
    if detail <= Detail::NoCensus {
        w += 2 + display_width(g.reload) + decimal_width(u64::from(f.reloads));
    }
    if detail <= Detail::NoReloads && f.window > 0 {
        w += 2 + 9 + decimal_width(u64::from(f.retained)) + 1 + decimal_width(u64::from(f.window));
    }
    if f.reload_pending {
        w += 2 + display_width(g.reload) + 8;
    }
    w
}

/// Renders the per-frame **work** ring as block-eighths, **scaled against the
/// budget**.
///
/// Not against the window maximum. An auto-scaled sparkline is always full and
/// therefore always says nothing: it animates constantly, it looks alarming on
/// a perfectly healthy scene, and a hitch is invisible because the hitch *is*
/// the scale. Against a fixed budget, a fast frame is visibly empty space and a
/// dropped frame is a spike — which is the entire reason this field earns its
/// place beside three scalars.
fn write_sparkline(out: &mut String, ring: &[u16], budget_ns: u64, p: &Palette, g: &Glyphs) {
    let ceiling_us = (budget_ns / 1_000).max(1);
    out.push_str(p.metric);
    let mut over = false;
    for &us in ring {
        let level = usize::try_from((u64::from(us) * 8 / ceiling_us).min(8)).unwrap_or(8);
        if level >= 8 && !over {
            // A frame at or over budget switches the rest of the line to `err`
            // and stays there, so the eye lands on where it started rather than
            // on a single cell it has to find.
            out.push_str(p.err);
            over = true;
        }
        out.push_str(g.bars[level.max(1) - 1]);
    }
    out.push_str(p.reset);
}

/// Truncates in place to at most `width` display columns, on a `char` boundary.
fn truncate_to_width(s: &mut String, width: usize) {
    if display_width(s) <= width {
        return;
    }
    let end = s
        .char_indices()
        .nth(width)
        .map_or(s.len(), |(index, _)| index);
    s.truncate(end);
}

/// One decimal place, always milliseconds — the same unit throughout, so the
/// two halves of `work · idle` stay comparable at a glance.
#[allow(clippy::cast_precision_loss)] // frame times never approach f64's limit
fn fmt_ms(ns: u64) -> String {
    format!("{:.1}ms", ns as f64 / 1_000_000.0)
}

fn decimal_width(mut n: u64) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

// ── The live statusline ────────────────────────────────────────────────────

/// One redraw's worth of input to [`StatusLine::on_frame`].
///
/// A struct rather than nine positional parameters: every one of them is a
/// number or a flag, and a call site that swaps two `bool`s compiles.
#[derive(Clone, Copy)]
pub struct FrameInputs<'a> {
    /// The frame *period*, as the developer perceives it.
    pub frame_ns: u64,
    /// The logic thread's block for the frame just presented.
    pub logic: &'a SampleBlock,
    /// This (render) thread's own block.
    pub render: &'a SampleBlock,
    /// The published frame's instance census.
    pub census: Census,
    /// Which layout path the frame took.
    pub atlas: byard_core::atlas::layout::path_counters::Counts,
    /// RFC-0010's active set.
    pub animating: bool,
    /// Hot reloads applied this session.
    pub reloads: u32,
    /// A reload waiting behind the gesture gate (RFC-0006 C1).
    pub reload_pending: bool,
    /// Whether the device can time GPU passes (RFC-0013 **P5**).
    pub gpu_available: bool,
}

/// The `byard dev` statusline: owns the ring buffers, decides when to repaint,
/// and erases itself on drop.
///
/// Owned by the render thread. Logging is a free function ([`log`]) rather than
/// a method precisely because the logic thread has to be able to do it too.
pub struct StatusLine {
    enabled: bool,
    /// When set, the expanded `--profile` block replaces the one-line display
    /// (§V1: it is a superset, so the two never coexist).
    profile: bool,
    budget_ns: u64,
    width: usize,

    /// The engine's **work** per frame in microseconds, oldest first — not the
    /// frame period.
    ///
    /// Plotting the period looked obviously right and is useless: under vsync
    /// the period *is* the budget by construction, so a perfectly healthy
    /// 60 fps app drew a permanently full, permanently red sparkline. That is
    /// the "always full and therefore says nothing" failure the budget scaling
    /// exists to avoid, arrived at from the other direction.
    ///
    /// Work against the budget is the reading that moves: a healthy scene shows
    /// visible headroom, and a hitch is a spike. It is the same correction as
    /// the headline's `work`/`idle` split, applied to the field beside it.
    ///
    /// `u16` saturating at 65 ms: 128 bytes for the whole history, and a frame
    /// past 65 ms is off the chart in every sense that matters.
    frames: Vec<u16>,
    /// One bit per recent frame: set if that frame took the retained layout
    /// path. Sixty-four frames in eight bytes, and `count_ones` is the field.
    retained_bits: u64,
    /// How many bits of `retained_bits` are meaningful yet.
    retained_seen: u32,

    fps_frames: u32,
    fps_since: Instant,
    last_paint: Instant,
    fields: Fields,
    /// The most recent frame's samples and context, kept so a repaint can
    /// recompose the expanded block without waiting for the next redraw.
    last: LastFrame,
    /// Reused between repaints — cleared, never reallocated.
    line: String,
}

/// What the expanded block needs that the one-line display does not.
#[derive(Default)]
struct LastFrame {
    logic: SampleBlock,
    render: SampleBlock,
    frame_ns: u64,
    gpu_available: bool,
    atlas: byard_core::atlas::layout::path_counters::Counts,
}

impl StatusLine {
    /// Creates a statusline, or an inert one when the terminal cannot host it.
    ///
    /// `enabled` is the `[dev] statusline` manifest setting; a non-TTY stderr
    /// overrides it to off regardless, because control sequences in a
    /// redirected stream are debris rather than a display.
    #[must_use]
    pub fn new(enabled: bool, budget_ns: u64) -> Self {
        let enabled = enabled && std::io::stderr().is_terminal();
        if enabled {
            painter().active = true;
        }
        Self {
            enabled,
            profile: false,
            budget_ns,
            width: resolve_width(),
            frames: vec![0; SPARK_LEN.max(SPARK_MAX)],
            retained_bits: 0,
            retained_seen: 0,
            fps_frames: 0,
            fps_since: Instant::now(),
            last_paint: Instant::now(),
            fields: Fields::default(),
            last: LastFrame::default(),
            line: String::with_capacity(2048),
        }
    }

    /// Whether anything will actually be drawn.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Turns the expanded `--profile` block on or off (`Mod+Shift+P`, §V1).
    ///
    /// Erases first: the two displays are different heights, so switching
    /// without clearing would leave the taller one's tail on screen for the
    /// rest of the session.
    pub fn set_profile(&mut self, on: bool) {
        if self.profile == on {
            return;
        }
        self.profile = on;
        // `replace` erases the previous block, whatever height it was, so
        // switching between the one-line display and the seventeen-line one
        // needs nothing special here.
        self.paint();
    }

    /// Whether the expanded block is showing.
    #[must_use]
    pub const fn is_profiling(&self) -> bool {
        self.profile
    }

    /// Records one redraw and repaints if the cadence is due.
    ///
    /// Everything here is a counter bump or a `Vec::len` read; the composition
    /// happens at most ten times a second. The per-frame cost is one `u32`
    /// increment, one `u16` store, one shift, and three `len`s (INV-24).
    pub fn on_frame(&mut self, f: FrameInputs<'_>) {
        if !self.enabled {
            return;
        }
        let FrameInputs {
            frame_ns,
            logic,
            render,
            census,
            atlas,
            animating,
            reloads,
            reload_pending,
            gpu_available,
        } = f;
        self.fps_frames = self.fps_frames.saturating_add(1);

        let idle_ns = idle_ns(logic, render);
        let work_ns = work_ns(logic, render, idle_ns);

        // The ring is a shift rather than a cursor: it keeps `frames` in
        // oldest-first order, so the sparkline is one slice rather than two.
        self.frames.rotate_left(1);
        let last = self.frames.len() - 1;
        self.frames[last] = u16::try_from(work_ns / 1_000).unwrap_or(u16::MAX);

        // A frame that cleared the layout tree rebuilt it; anything else took
        // the retained path (RFC-0032 §R7). `populate_calls == 0` means nothing
        // rendered, which is neither.
        if atlas.populate_calls > 0 {
            self.retained_bits = (self.retained_bits << 1) | u64::from(atlas.clears == 0);
            self.retained_seen = (self.retained_seen + 1).min(RETAINED_WINDOW);
        }

        self.fields = Fields {
            animating,
            fps: self.fields.fps,
            work_ns,
            idle_ns,
            instances: census.instances,
            reloads,
            reload_pending,
            retained: self.retained_window_count(),
            window: self.retained_seen,
        };

        let elapsed = self.fps_since.elapsed();
        if elapsed >= Duration::from_secs(1) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let fps = (f64::from(self.fps_frames) / elapsed.as_secs_f64()).round() as u32;
            self.fields.fps = fps;
            self.fps_frames = 0;
            self.fps_since = Instant::now();
        }

        if self.profile {
            // Cloned only in profile mode: the expanded block is composed from
            // the whole sample set, and the ring it came from is drained on the
            // next redraw. One clone per frame while a developer is explicitly
            // looking at a profiler is a cost they have asked for; paying it on
            // every frame of every session would not be (INV-24).
            self.last = LastFrame {
                logic: logic.clone(),
                render: render.clone(),
                frame_ns,
                gpu_available,
                atlas,
            };
        }

        if self.last_paint.elapsed() >= REPAINT_INTERVAL {
            self.last_paint = Instant::now();
            self.paint();
        }
    }

    /// The number of set bits inside the meaningful window.
    fn retained_window_count(&self) -> u32 {
        let mask = if self.retained_seen >= 64 {
            u64::MAX
        } else {
            (1u64 << self.retained_seen) - 1
        };
        (self.retained_bits & mask).count_ones()
    }

    fn paint(&mut self) {
        if self.profile {
            self.line.clear();
            self.line
                .push_str(&crate::telemetry_overlay::format_profile_block(
                    &self.last.logic,
                    &self.last.render,
                    crate::telemetry_overlay::ProfileContext {
                        budget_ns: self.budget_ns,
                        frame_ns: self.last.frame_ns,
                        gpu_available: self.last.gpu_available,
                        atlas: self.last.atlas,
                    },
                    palette(),
                    glyphs(),
                ));
        } else {
            let spark = &self.frames[self.frames.len() - SPARK_MAX..];
            compose(
                &mut self.line,
                &self.fields,
                spark,
                self.budget_ns,
                self.width,
                palette(),
                glyphs(),
            );
        }
        painter().replace(&self.line);
    }
}

impl Drop for StatusLine {
    fn drop(&mut self) {
        if self.enabled {
            retire();
        }
    }
}

/// The composed width: `COLUMNS` when the shell exports it, 80 otherwise (§Q7).
///
/// A stale or absent `COLUMNS` is never a correctness problem — the line only
/// has to not *exceed* the width, and 80 is the floor every terminal in
/// practical use clears.
fn resolve_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|w| *w >= MIN_WIDTH)
        .unwrap_or(DEFAULT_WIDTH)
}

/// The idle half of a frame: the wall time the display made the engine wait.
///
/// Shared with the non-TTY summary in `dev.rs` so both readouts split the frame
/// the same way. Two places computing "idle" from two definitions is how a
/// profiler ends up disagreeing with itself.
#[must_use]
pub fn idle_ns(logic: &SampleBlock, render: &SampleBlock) -> u64 {
    sum_scope(logic, IDLE_SCOPE) + sum_scope(render, IDLE_SCOPE)
}

/// The work half of a frame: the **longer** of the two threads' non-idle
/// totals, not their sum.
///
/// The logic and render threads run concurrently. Adding their totals reports
/// 20 ms of work inside a 16 ms frame — the same double count RFC-0030 §I2
/// exists to prevent between nested scopes, one level up between threads. And
/// taking only the render thread's would hide the interpreter entirely, which
/// is the number a developer is usually looking for.
///
/// The frame is late when *either* thread overruns, so the pipeline's critical
/// path is the honest single figure: it moves when the interpreter gets slower
/// and when the encoder does, and it never claims more work than the frame
/// contained.
#[must_use]
pub fn work_ns(logic: &SampleBlock, render: &SampleBlock, idle_ns: u64) -> u64 {
    logic
        .total_ns()
        .max(render.total_ns().saturating_sub(idle_ns))
}

/// Total inclusive time of every depth-0 sample naming `scope`.
fn sum_scope(block: &SampleBlock, scope: &str) -> u64 {
    block
        .samples
        .iter()
        .filter(|s| s.depth() == 0 && scope_name(s.scope) == Some(scope))
        .map(byard_core::telemetry::Sample::duration_ns)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: u64 = 16_667_000;

    fn fields() -> Fields {
        Fields {
            animating: true,
            fps: 60,
            work_ns: 3_400_000,
            idle_ns: 13_500_000,
            instances: 382,
            reloads: 14,
            reload_pending: false,
            retained: 59,
            window: 60,
        }
    }

    fn ring(us: u16) -> Vec<u16> {
        vec![us; SPARK_MAX]
    }

    fn render(f: &Fields, spark: &[u16], width: usize, p: &Palette) -> String {
        let mut out = String::new();
        compose(&mut out, f, spark, BUDGET, width, p, &Glyphs::unicode());
        out
    }

    #[test]
    fn a_multi_line_block_redraws_in_place_instead_of_scrolling() {
        // Found by running it against a real pty: a one-line statusline worked
        // because `\r\x1b[2K` clears the line the cursor is on, so the missing
        // erase was invisible — while the seventeen-line profile block appended
        // sixteen fresh lines on every repaint, ten times a second, eating
        // exactly the scrollback this design exists to protect.
        let mut out = Vec::new();
        erase_block(&mut out, crate::telemetry_overlay::PROFILE_LINES);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.matches("\x1b[1A").count(),
            crate::telemetry_overlay::PROFILE_LINES - 1,
            "the cursor must walk up over every line of the previous block"
        );
        assert_eq!(
            text.matches("\x1b[2K").count(),
            crate::telemetry_overlay::PROFILE_LINES,
            "and clear each one"
        );

        // A one-line block moves the cursor nowhere at all.
        let mut out = Vec::new();
        erase_block(&mut out, 1);
        assert_eq!(out, b"\r\x1b[2K");

        // Nothing drawn, nothing erased — the case where an eager cursor-up
        // would climb into the log above.
        let mut out = Vec::new();
        erase_block(&mut out, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn every_drawn_line_clears_the_one_it_replaces() {
        // A shorter line drawn over a longer one leaves the longer one's tail
        // on screen unless each line is cleared individually.
        let mut out = Vec::new();
        draw_block(&mut out, "aaa\nbb\nc");
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "\r\x1b[2Kaaa\n\r\x1b[2Kbb\n\r\x1b[2Kc");
        assert!(
            !text.ends_with('\n'),
            "a trailing newline would scroll the block off its own anchor"
        );
    }

    #[test]
    fn nothing_written_hides_the_cursor_or_takes_the_alternate_screen() {
        // INV-25, structurally. A `Drop` guard does not run on `SIGINT`, so the
        // only state safe to write is state already scoped to the line being
        // drawn. A hidden cursor or an alternate screen buffer would be state
        // this module might fail to restore, on the exit path people actually
        // use.
        let mut out = Vec::new();
        erase_block(&mut out, 8);
        draw_block(&mut out, " ● 60fps\nsecond line");
        let text = String::from_utf8(out).unwrap();
        for forbidden in ["\x1b[?25l", "\x1b[?1049h", "\x1b[?47h", "\x1b[r"] {
            assert!(
                !text.contains(forbidden),
                "wrote terminal state it cannot guarantee to restore: {forbidden:?}"
            );
        }
    }

    #[test]
    fn a_non_tty_emits_no_control_characters_at_all() {
        // The `byard dev 2> log` case. A redirected stream full of `\x1b[2K`
        // is debris, not a display — and the palette is only half of it, since
        // the erase sequence is written by the painter, not by the palette.
        let out = render(&fields(), &ring(8_000), 80, &Palette::plain());
        assert!(
            !out.contains('\x1b') && !out.contains('\r'),
            "control characters in an uncoloured statusline: {out:?}"
        );
    }

    #[test]
    fn the_line_never_exceeds_the_composed_width_for_any_field_values() {
        // A property test over the values a width bug actually lives at: a
        // six-digit box count, a four-digit fps, a five-digit reload count, a
        // frame time past the `u16` ceiling. An over-wide statusline wraps, and
        // a wrapped statusline breaks the in-place redraw *permanently* —
        // every later repaint erases one line of a two-line display.
        for width in [24usize, 40, 60, 72, 79, 80, 100, 200] {
            for fps in [0u32, 60, 9999] {
                for instances in [0usize, 382, 999_999] {
                    for reloads in [0u32, 14, 99_999] {
                        for pending in [false, true] {
                            for animating in [false, true] {
                                let f = Fields {
                                    animating,
                                    fps,
                                    work_ns: 999_000_000,
                                    idle_ns: 999_000_000,
                                    instances,
                                    reloads,
                                    reload_pending: pending,
                                    retained: 0,
                                    window: 64,
                                };
                                for p in [Palette::plain(), Palette::ansi()] {
                                    let out = render(&f, &ring(u16::MAX), width, &p);
                                    assert!(
                                        display_width(&out) <= width,
                                        "composed {} columns into {width}: {out:?}",
                                        display_width(&out)
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fields_drop_in_the_fixed_order_as_the_terminal_narrows() {
        // Fixed, and asserted, because a line whose fields disappear in an
        // order that depends on their *values* changes shape while you read it.
        let f = fields();
        let full = render(&f, &ring(8_000), 100, &Palette::plain());
        assert!(full.contains("boxes") && full.contains("retained"));

        let mut seen_without = Vec::new();
        for width in [100usize, 80, 66, 52, 40, 30] {
            let out = render(&f, &ring(8_000), width, &Palette::plain());
            // work/idle and fps survive every width — a line that cannot show
            // them is not worth drawing.
            assert!(out.contains("fps"), "fps dropped at {width}: {out:?}");
            assert!(out.contains("work"), "work dropped at {width}: {out:?}");
            seen_without.push((
                width,
                out.contains('▁') || out.contains('█'),
                out.contains("boxes"),
                out.contains('↻'),
            ));
        }
        // Monotone: once a field is gone it never comes back as the terminal
        // narrows further.
        for w in [1usize, 2, 3] {
            let flags: Vec<bool> = seen_without
                .iter()
                .map(|t| match w {
                    1 => t.1,
                    2 => t.2,
                    _ => t.3,
                })
                .collect();
            let first_false = flags.iter().position(|b| !b).unwrap_or(flags.len());
            assert!(
                flags[first_false..].iter().all(|b| !*b),
                "field {w} reappeared as the terminal narrowed: {flags:?}"
            );
        }
        // And the order itself: the sparkline goes before the census, which
        // goes before the reload count.
        let spark_gone = seen_without.iter().position(|t| !t.1).unwrap();
        let census_gone = seen_without.iter().position(|t| !t.2).unwrap();
        let reload_gone = seen_without.iter().position(|t| !t.3).unwrap();
        assert!(
            spark_gone <= census_gone && census_gone <= reload_gone,
            "drop order is not sparkline → census → reloads: {seen_without:?}"
        );
    }

    #[test]
    fn the_sparkline_plots_work_not_the_vsync_locked_frame_period() {
        // Found by running it. Under vsync the frame *period* is the budget by
        // construction, so plotting it drew a permanently full, permanently red
        // sparkline on a perfectly healthy 60 fps app — the "always full and
        // therefore says nothing" failure, arrived at from the other direction.
        use byard_core::telemetry::{Sample, ScopeKind, scope_id_tagged};
        let acquire = scope_id_tagged(IDLE_SCOPE, ScopeKind::Native);
        let encode = scope_id_tagged("statusline.test.spark.encode", ScopeKind::Native);
        let mut s = StatusLine::new(false, BUDGET);
        s.enabled = true;
        // A healthy vsync-bound frame: 16.6ms period, 15.0ms of it waiting.
        let healthy = SampleBlock {
            samples: vec![
                Sample::cpu(acquire, 0, 0, 15_000_000),
                Sample::cpu(encode, 0, 15_000_000, 16_600_000),
            ],
            dropped: 0,
        };
        for _ in 0..SPARK_MAX {
            s.on_frame(FrameInputs {
                frame_ns: 16_600_000,
                logic: &SampleBlock::default(),
                render: &healthy,
                census: Census::default(),
                atlas: byard_core::atlas::layout::path_counters::Counts::default(),
                animating: true,
                reloads: 0,
                reload_pending: false,
                gpu_available: true,
            });
        }
        let spark = &s.frames[s.frames.len() - SPARK_MAX..];
        let out = render(&s.fields, spark, 120, &Palette::plain());
        assert!(
            !out.contains('█'),
            "a healthy vsync-bound frame must show headroom, not a full bar: {out:?}"
        );
    }

    #[test]
    fn the_sparkline_is_scaled_against_the_budget_not_the_window_maximum() {
        // The failure this guards is the one that looks fine in a screenshot:
        // an auto-scaled sparkline is always full, so a healthy scene looks
        // alarming and a hitch is invisible because the hitch *is* the scale.
        let f = fields();
        let calm = render(&f, &ring(1_000), 200, &Palette::plain());
        let busy = render(&f, &ring(17_000), 200, &Palette::plain());
        assert!(
            calm.contains('▁') && !calm.contains('█'),
            "a 1ms frame against a 16.7ms budget must render near-empty: {calm:?}"
        );
        assert!(
            busy.contains('█'),
            "a 17ms frame against a 16.7ms budget must render full: {busy:?}"
        );
        // And the boundary is the budget, not "nearly the budget": a 16.0ms
        // frame is *under* a 16.7ms budget and must not render as a dropped
        // one. Crying wolf at 96 % of budget is how a developer learns to stop
        // looking at the sparkline.
        let close = render(&f, &ring(16_000), 200, &Palette::plain());
        assert!(
            !close.contains('█'),
            "a frame inside the budget must not render as over it: {close:?}"
        );
        assert_ne!(calm, busy, "the sparkline rescaled itself away");
    }

    #[test]
    fn an_over_budget_frame_colours_the_rest_of_the_sparkline_err() {
        let f = fields();
        let mut spark = ring(1_000);
        let last = spark.len() - 1;
        spark[last] = 40_000;
        let out = render(&f, &spark, 200, &Palette::ansi());
        assert!(
            out.contains(Palette::ansi().err),
            "an over-budget frame must be visible without reading a number: {out:?}"
        );
    }

    #[test]
    fn retained_is_read_from_the_counters_not_inferred_from_a_timing() {
        let mut s = StatusLine::new(false, BUDGET);
        // Force the accumulation path on regardless of whether the test
        // process has a terminal.
        s.enabled = true;
        let clean = byard_core::atlas::layout::path_counters::Counts {
            clears: 0,
            populate_calls: 1,
            ..Default::default()
        };
        let rebuilt = byard_core::atlas::layout::path_counters::Counts { clears: 1, ..clean };
        let blank = SampleBlock::default();
        let feed = |s: &mut StatusLine, c| {
            s.on_frame(FrameInputs {
                frame_ns: 1_000_000,
                logic: &blank,
                render: &blank,
                census: Census::default(),
                atlas: c,
                animating: false,
                reloads: 0,
                reload_pending: false,
                gpu_available: true,
            });
        };
        for _ in 0..10 {
            feed(&mut s, clean);
        }
        assert_eq!((s.fields.retained, s.fields.window), (10, 10));
        feed(&mut s, rebuilt);
        assert_eq!(
            (s.fields.retained, s.fields.window),
            (10, 11),
            "a rebuild must be visible as a missing frame, not averaged away"
        );
    }

    #[test]
    fn a_tick_that_rendered_nothing_does_not_count_as_a_rebuild() {
        // `populate_calls == 0` means nothing rendered. Counting it as a
        // rebuild would make a parked, idle app — the healthiest state there
        // is — read as the sickest.
        let mut s = StatusLine::new(false, BUDGET);
        s.enabled = true;
        let blank = SampleBlock::default();
        for _ in 0..5 {
            s.on_frame(FrameInputs {
                frame_ns: 1_000_000,
                logic: &blank,
                render: &blank,
                census: Census::default(),
                atlas: byard_core::atlas::layout::path_counters::Counts::default(),
                animating: false,
                reloads: 0,
                reload_pending: false,
                gpu_available: true,
            });
        }
        assert_eq!((s.fields.retained, s.fields.window), (0, 0));
    }

    #[test]
    fn work_is_the_frame_minus_the_wait_the_display_imposed() {
        use byard_core::telemetry::{Sample, ScopeKind, scope_id_tagged};
        // The correction the erratum is about: a healthy frame and an
        // overrunning one print the same total and differ entirely in this
        // split.
        let acquire = scope_id_tagged(IDLE_SCOPE, ScopeKind::Native);
        let encode = scope_id_tagged("statusline.test.encode", ScopeKind::Native);
        let render_block = SampleBlock {
            samples: vec![
                Sample::cpu(acquire, 0, 0, 13_500_000),
                Sample::cpu(encode, 0, 13_500_000, 16_900_000),
            ],
            dropped: 0,
        };
        let mut s = StatusLine::new(false, BUDGET);
        s.enabled = true;
        s.on_frame(FrameInputs {
            frame_ns: 16_900_000,
            logic: &SampleBlock::default(),
            render: &render_block,
            census: Census::default(),
            atlas: byard_core::atlas::layout::path_counters::Counts::default(),
            animating: true,
            reloads: 0,
            reload_pending: false,
            gpu_available: true,
        });
        assert_eq!(s.fields.idle_ns, 13_500_000);
        assert_eq!(s.fields.work_ns, 3_400_000);
    }

    #[test]
    fn work_never_exceeds_the_frame_it_describes() {
        use byard_core::telemetry::{Sample, ScopeKind, scope_id_tagged};
        // Found by running it: summing the two threads' totals reported
        // `work 5.1ms · idle 15.0ms` inside a 16.5ms frame. They run
        // concurrently, so a sum is the §I2 double count one level up —
        // between threads instead of between nested scopes.
        let acquire = scope_id_tagged(IDLE_SCOPE, ScopeKind::Native);
        let encode = scope_id_tagged("statusline.test.pipeline.encode", ScopeKind::Native);
        let interp = scope_id_tagged("statusline.test.pipeline.interp", ScopeKind::Interpreter);
        let render_block = SampleBlock {
            samples: vec![
                Sample::cpu(acquire, 0, 0, 15_000_000),
                Sample::cpu(encode, 0, 15_000_000, 16_500_000),
            ],
            dropped: 0,
        };
        let logic_block = SampleBlock {
            samples: vec![Sample::cpu(interp, 0, 0, 3_600_000)],
            dropped: 0,
        };
        let idle = idle_ns(&logic_block, &render_block);
        let work = work_ns(&logic_block, &render_block, idle);
        assert_eq!(idle, 15_000_000);
        assert_eq!(
            work, 3_600_000,
            "the critical path is the slower thread, not the two added together"
        );
        assert!(
            work + idle <= 18_600_000,
            "work + idle must stay inside the frame"
        );

        // And it still moves when the *render* thread is the slow one, which
        // taking the logic thread alone would hide.
        let heavy = SampleBlock {
            samples: vec![
                Sample::cpu(acquire, 0, 0, 1_000_000),
                Sample::cpu(encode, 0, 1_000_000, 12_000_000),
            ],
            dropped: 0,
        };
        let idle = idle_ns(&logic_block, &heavy);
        assert_eq!(work_ns(&logic_block, &heavy, idle), 11_000_000);
    }

    #[test]
    fn a_pending_reload_is_shown_even_when_nothing_else_fits() {
        // RFC-0006 C1. A developer whose saves have silently stopped applying
        // needs this more than they need any other field on the line.
        let mut f = fields();
        f.reload_pending = true;
        // From 50 columns up. Below roughly 46 the minimal line itself does not
        // fit and is truncated rather than wrapped — which is the right failure
        // (a wrapped statusline breaks the in-place redraw permanently) but it
        // is a failure, and pretending otherwise would make this assertion a
        // claim the code cannot keep.
        for width in [50usize, 60, 80, 120] {
            let out = render(&f, &ring(8_000), width, &Palette::plain());
            assert!(
                out.contains("pending"),
                "the pending indicator was dropped at {width}: {out:?}"
            );
        }
    }

    #[test]
    fn the_repaint_buffer_stops_growing_once_it_has_seen_a_full_line() {
        // INV-24: a diagnostic that allocates per frame is competing with the
        // thing it measures.
        let mut s = StatusLine::new(false, BUDGET);
        s.enabled = true;
        s.paint();
        let capacity = s.line.capacity();
        for _ in 0..200 {
            s.paint();
        }
        assert_eq!(
            s.line.capacity(),
            capacity,
            "the repaint buffer reallocated"
        );
    }

    #[test]
    fn columns_widens_the_sparkline_and_nothing_else() {
        // §Q5: `COLUMNS` is not a general layout input. Everything but the
        // sparkline has a fixed width, so a wider terminal must produce a
        // longer sparkline and an otherwise identical line.
        let f = fields();
        let narrow = render(&f, &ring(4_000), 80, &Palette::plain());
        let wide = render(&f, &ring(4_000), 140, &Palette::plain());
        let bars = |s: &str| s.chars().filter(|c| "▁▂▃▄▅▆▇█".contains(*c)).count();
        assert!(
            bars(&wide) > bars(&narrow),
            "a wider terminal must widen the sparkline: {narrow:?} vs {wide:?}"
        );
        let strip = |s: &str| {
            s.chars()
                .filter(|c| !"▁▂▃▄▅▆▇█".contains(*c))
                .collect::<String>()
        };
        assert_eq!(
            strip(&narrow).trim_end(),
            strip(&wide).trim_end(),
            "COLUMNS changed a field other than the sparkline"
        );
    }
}
