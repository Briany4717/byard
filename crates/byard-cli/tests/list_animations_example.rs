//! Guards the committed per-instance animation example — a spring and a stagger
//! inside a `for`, a nested `for` inside each row, and rows that mount and
//! unmount — two ways: it must `byard check` clean, and it must actually *move*,
//! driven headlessly through the real interpreter with real taps.
//!
//! The second half is the one that matters. Every defect this example was
//! written to demonstrate was invisible to a check: the rows mounted, laid out
//! and painted exactly as written while the motion itself was dead.

use std::path::PathBuf;
use std::process::Command;

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::events::EventKind as CompKind;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

const LIST: &str = include_str!("../examples/list_animations/src/main.byd");
const W: f32 = 700.0;
const H: f32 = 500.0;
/// The bar inside each row, by its written height — the element whose `rotate`
/// the shuffle drives. Its *width* is per-row (`width: row.bar`), which is
/// precisely what a bar must not be identified by here.
const BAR_HEIGHT: f32 = 22.0;
/// The bar widths the rows are written with, in row order.
const BAR_WIDTHS: [f32; 7] = [120.0, 168.0, 96.0, 200.0, 144.0, 112.0, 184.0];

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/list_animations")
}

/// `byard check <project-dir>` on the list-animations example reports no errors.
#[test]
fn list_animations_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("0 errors"),
        "expected a clean check, got:\n{stdout}"
    );
}

fn pointer(kind: EventKind, pos: (f32, f32), t: u64) -> InputEvent {
    InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
    }
}

/// One runner frame at engine time `ms`: dispatch, tick, render.
fn frame(interp: &mut Interpreter, tree: &[RenderNode], inputs: &[InputEvent], ms: u32) {
    interp.dispatch_events(inputs);
    interp.tick();
    interp.set_now_ms(ms);
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, W, H);
}

/// A tap = down on one frame, up on the next (as in the real app).
fn tap(interp: &mut Interpreter, tree: &[RenderNode], center: (f32, f32), ms: u32) {
    let t = u64::from(ms);
    frame(
        interp,
        tree,
        &[pointer(EventKind::PointerDown, center, t)],
        ms,
    );
    frame(
        interp,
        tree,
        &[pointer(EventKind::PointerUp, center, t + 30)],
        ms + 16,
    );
}

/// Every bar's rotation this frame, in row order. A row whose entrance is still
/// running paints translucent (the decorated path); a settled one paints on the
/// flat instance path — the bar is the same bar either way, so read both.
fn bar_rotations(interp: &mut Interpreter, tree: &[RenderNode], ms: u32) -> Vec<f32> {
    interp.tick();
    interp.set_now_ms(ms);
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, W, H);
    let solid = f
        .instances()
        .iter()
        .filter(|b| (b.rect[3] - BAR_HEIGHT).abs() < 0.5)
        .map(|b| (b.rect[1], b.transform.rotate));
    let decorated = f
        .decorated()
        .iter()
        .filter(|d| (d.base.rect[3] - BAR_HEIGHT).abs() < 0.5)
        .map(|d| (d.base.rect[1], d.base.transform.rotate));
    let mut bars: Vec<(f32, f32)> = solid.chain(decorated).collect();
    // Top to bottom is row order; the two paints arrive in separate pools.
    bars.sort_by(|a, b| a.0.total_cmp(&b.0));
    bars.into_iter().map(|(_, rot)| rot).collect()
}

/// Every bar's laid-out width this frame, in row order.
fn bar_widths(interp: &mut Interpreter, tree: &[RenderNode], ms: u32) -> Vec<f32> {
    interp.tick();
    interp.set_now_ms(ms);
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, W, H);
    let solid = f
        .instances()
        .iter()
        .filter(|b| (b.rect[3] - BAR_HEIGHT).abs() < 0.5)
        .map(|b| (b.rect[1], b.rect[2]));
    let decorated = f
        .decorated()
        .iter()
        .filter(|d| (d.base.rect[3] - BAR_HEIGHT).abs() < 0.5)
        .map(|d| (d.base.rect[1], d.base.rect[2]));
    let mut bars: Vec<(f32, f32)> = solid.chain(decorated).collect();
    bars.sort_by(|a, b| a.0.total_cmp(&b.0));
    bars.into_iter().map(|(_, w)| w).collect()
}

/// The rows' entrance opacities this frame, in row order — what the stagger
/// cascade is visible as.
fn row_opacities(interp: &mut Interpreter, tree: &[RenderNode], ms: u32) -> Vec<f32> {
    interp.tick();
    interp.set_now_ms(ms);
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, W, H);
    let mut rows: Vec<(f32, f32)> = f
        .decorated()
        .iter()
        .filter(|d| (d.base.rect[3] - BAR_HEIGHT).abs() < 0.5)
        .map(|d| (d.base.rect[1], d.opacity))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, o)| o).collect()
}

fn load() -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(LIST);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = parsed.views[0].clone();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&view, &[]);
    interp.tick();
    (interp, tree)
}

/// All live `Tap` handler rects (center points), in registration = document
/// order: Shuffle, Add, Remove, Replay, then any row handlers.
fn tap_centers(interp: &Interpreter) -> Vec<(f32, f32)> {
    interp
        .router
        .handler_rects()
        .into_iter()
        .filter(|(_, k, _)| matches!(k, CompKind::Tap))
        .map(|(_, _, r)| (r.x + r.w / 2.0, r.y + r.h / 2.0))
        .collect()
}

/// **The example's headline claim.** Pressing "Shuffle" sends every bar to a
/// different angle at the same moment, and each one arrives at *its own*.
///
/// Each row's target is written as `row.tilt` inside a `when` — the ordinary
/// shape of a filtered list. A row that cannot resolve `row` at render time
/// reads `0` for its target and simply never moves, which is what the example
/// looked like: a correct-looking list where Shuffle did nothing at all.
#[test]
fn shuffle_sends_every_row_to_its_own_angle() {
    let (mut interp, tree) = load();
    frame(&mut interp, &tree, &[], 0);
    // Let the entrance cascade play out first, so what follows is the shuffle.
    let resting = bar_rotations(&mut interp, &tree, 3_000);
    assert_eq!(resting.len(), 5, "five rows are mounted, got {resting:?}");
    assert!(
        resting.iter().all(|r| r.abs() < 0.01),
        "at rest every bar is level, got {resting:?}"
    );

    let shuffle = tap_centers(&interp)[0];
    tap(&mut interp, &tree, shuffle, 3_100);

    // Mid-flight the rows are already apart — one shared spring could only ever
    // produce one angle.
    let mid = bar_rotations(&mut interp, &tree, 3_250);
    assert!(
        mid.windows(2).any(|w| (w[0] - w[1]).abs() > 0.05),
        "the rows must animate independently, got {mid:?}"
    );

    // …and each settles on the tilt written for *its* row: -14°, 22°, -6°, 30°,
    // 10° (the first five of the example's seven).
    let settled = bar_rotations(&mut interp, &tree, 8_000);
    let want = [-14.0_f32, 22.0, -6.0, 30.0, 10.0].map(f32::to_radians);
    for (row, (got, expect)) in settled.iter().zip(want).enumerate() {
        assert!(
            (got - expect).abs() < 0.02,
            "row {row} must reach its own tilt {expect}, got {got} (all: {settled:?})"
        );
    }
}

/// Each bar is laid out at the width its own row carries (`width: row.bar`).
///
/// Layout is a separate pass over the same tree, one ahead of paint, and it has
/// to know which row it is in for the same reason paint does. When it did not,
/// `row.bar` resolved to nothing there — and a `width` that resolves to nothing
/// is a box with no width, so every bar stretched to fill the column while the
/// paint pass, which *had* the row, coloured and rotated each one correctly.
#[test]
fn each_bar_is_laid_out_at_its_own_rows_width() {
    let (mut interp, tree) = load();
    frame(&mut interp, &tree, &[], 0);
    let widths = bar_widths(&mut interp, &tree, 3_000);
    assert_eq!(widths.len(), 5, "five rows are mounted, got {widths:?}");
    for (row, (got, expect)) in widths.iter().zip(BAR_WIDTHS).enumerate() {
        assert!(
            (got - expect).abs() < 0.5,
            "row {row} must be laid out at its own width {expect}, got {got} (all: {widths:?})"
        );
    }
}

/// "Replay" restarts one written stagger, and the rows cascade in index order
/// rather than arriving as one flash — the per-row delay is `80ms × i`, so a row
/// that has forgotten which index it is has no delay at all.
#[test]
fn replay_cascades_the_rows_in_index_order() {
    let (mut interp, tree) = load();
    frame(&mut interp, &tree, &[], 0);
    bar_rotations(&mut interp, &tree, 3_000);

    let replay = tap_centers(&interp)[3];
    tap(&mut interp, &tree, replay, 3_100);

    // 120ms into the replay: row 0 is well under way, row 1 has just been
    // released (80ms delay), and rows 2+ are still waiting their turn.
    let mid = row_opacities(&mut interp, &tree, 3_220);
    assert_eq!(mid.len(), 5, "five rows, got {mid:?}");
    assert!(
        mid[0] > mid[1] && mid[1] > mid[2] && mid[4] < 0.01,
        "the entrance must cascade in row order, got {mid:?}"
    );

    // Every row still arrives.
    let done = row_opacities(&mut interp, &tree, 8_000);
    assert!(
        done.iter().all(|o| (o - 1.0).abs() < 0.05),
        "every row finishes its entrance, got {done:?}"
    );
}

/// "Add" stops at the last row it has to add, and "Remove" at the empty list.
/// Neither is a silent no-op at the limit: the button dims (its `bg`/`color` are
/// written against the same bound), so a press that cannot do anything is a
/// press the example already told you not to make.
#[test]
fn add_and_remove_stop_at_the_ends_of_the_list() {
    let (mut interp, tree) = load();
    frame(&mut interp, &tree, &[], 0);
    let buttons = tap_centers(&interp);
    let (add, remove) = (buttons[1], buttons[2]);

    // Seven rows are written; five are mounted. Press "Add" ten times.
    let mut t = 1_000;
    for _ in 0..10 {
        tap(&mut interp, &tree, add, t);
        t += 400;
    }
    let full = bar_rotations(&mut interp, &tree, t + 3_000);
    assert_eq!(
        full.len(),
        7,
        "the list grows to the rows it has and stops, got {} rows",
        full.len()
    );

    // And the same at the bottom: ten "Remove"s empty the list and leave it
    // empty, rather than driving the bound negative.
    for _ in 0..10 {
        tap(&mut interp, &tree, remove, t);
        t += 400;
    }
    let empty = bar_rotations(&mut interp, &tree, t + 3_000);
    assert!(
        empty.is_empty(),
        "the list empties and stays, got {empty:?}"
    );

    // It still comes back — the clamp is a bound, not a latch.
    tap(&mut interp, &tree, add, t);
    let again = bar_rotations(&mut interp, &tree, t + 3_000);
    assert_eq!(again.len(), 1, "one row is back, got {again:?}");
}
