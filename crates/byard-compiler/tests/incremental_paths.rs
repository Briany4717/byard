//! Which incremental path production takes — asserted, at the integration
//! point, on a real frame.
//!
//! # Why this file exists
//!
//! Byard built incremental machinery in three layers: `mark_dirty_all` +
//! `recompute_dirty` in layout, the dirty-target set at the atlas→frame
//! boundary, and the encoder's scissor. Each was validated **in isolation** —
//! `recompute_dirty` has benchmarks, `TargetId` has generation tests, the
//! scissor has `tests/m26_m27_incremental.rs` — and none of them had an
//! assertion that fails when production takes the slow path instead. So when
//! the interpreter was built taking the simple path, nothing protested, and
//! nothing kept protesting for several phases afterwards.
//!
//! That is the defect this file addresses, and it is a process defect rather
//! than a code one. A benchmark proves a path is fast; only an integration
//! assertion proves anyone walks it.
//!
//! # What it asserts, and why some of it is `#[ignore]`d
//!
//! The tests below fall into two groups.
//!
//! **Group 1 — the current behaviour, pinned.** These pass today. They exist so
//! that a change to the interpreter's frame path is a *deliberate* change with
//! a failing test attached, rather than a silent drift.
//!
//! **Group 2 — the acceptance criteria, `#[ignore]`d.** These describe the
//! behaviour the incremental design was built for and do **not** pass. They are
//! written now, and ignored rather than deleted, so the gap is visible in
//! `cargo test` output instead of living in nobody's head — and so that whoever
//! closes it has an acceptance criterion already waiting instead of writing
//! their own after the fact.
//!
//! # What actually blocks group 2
//!
//! Not the atlas, and not the one-line call site the surface reading suggests.
//! The interpreter stores element attributes as raw expressions on the render
//! node and re-evaluates them from scratch every frame, so there is no reactive
//! edge from a signal to a box's colour or width and therefore **no per-element
//! change signal at all**. Both group-2 behaviours need one:
//!
//! - passing a real dirty set to `populate_frame` needs to know *which nodes*
//!   changed;
//! - skipping `atlas.clear()` needs to know that nothing *layout-affecting*
//!   changed — and an animated `width` or an edited `Text` would otherwise keep
//!   the previous frame's geometry, leaving a stale rect that hit-testing still
//!   answers from. An element that looks like it moved but is tappable where it
//!   used to be is a correctness bug, not a performance regression.
//!
//! So the two findings the audit classified separately have one root cause, and
//! closing them is a change to the evaluation model rather than a call-site fix.
//! The measurements that say it is worth doing are in `cargo bench --bench
//! atlas`: 3.8–7.2× on layout time and ~72 % of the per-frame heap allocations.

#![cfg(feature = "telemetry")]

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::atlas::layout::path_counters;
use byard_core::frame::RenderFrame;

const W: f32 = 800.0;
const H: f32 = 600.0;

/// A view whose only reactive input is a **colour** — no layout consequence at
/// all. This is the cheapest possible frame: nothing moved, nothing was
/// mounted, one box is a different shade. If any incremental path is ever going
/// to be taken, it is on a frame like this one.
const SOURCE: &str = r#"
View Probe() {
    var hot = false
    Column #[padding: 16, gap: 8, width: 400, height: 300] {
        Text("steady") #[size: 16]
        Box #[width: 100, height: 40, bg: hot ? 0xFF0000 : 0x0000FF] {}
        Box #[width: 100, height: 40, bg: 0x00FF00] {}
    }
}
"#;

fn build() -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(SOURCE);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    (interp, tree)
}

/// One frame of the runner loop, with the atlas path counters reset first so
/// the snapshot describes this frame alone.
fn frame(interp: &mut Interpreter, tree: &[RenderNode]) -> (RenderFrame, path_counters::Counts) {
    path_counters::reset();
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, W, H);
    (f, path_counters::snapshot())
}

/// Flips the `hot` var, so the next frame differs from the previous one by
/// exactly one box's colour and nothing else.
fn change_one_colour(interp: &mut Interpreter) {
    let sig = interp
        .var_signal(&Symbol::intern("hot"))
        .expect("the probe view declares `hot`");
    let current = interp.peek(sig).as_bool().unwrap_or(false);
    interp.write_var(sig, Value::Bool(!current));
}

// ── Group 1: the current behaviour, pinned ─────────────────────────────────

#[test]
fn a_value_only_frame_still_tears_down_and_rebuilds_the_whole_atlas() {
    // Pins H1. A frame whose only difference is one box's colour still calls
    // `clear()` — bumping the view generation and invalidating every
    // previously-issued `TargetId` — and runs a full `compute`, never
    // `recompute_dirty`.
    //
    // If this test fails because `clears` dropped to 0, that is the *good*
    // outcome and `the_retained_layout_path_is_taken_on_a_value_only_frame`
    // below is the test to un-ignore. Do not "fix" this one by relaxing it.
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (_f, counts) = frame(&mut interp, &tree);

    assert_eq!(
        counts.clears, 1,
        "the interpreter clears the atlas once per frame"
    );
    assert!(
        counts.full_computes >= 1,
        "and runs a full layout pass over the freshly rebuilt tree"
    );
    assert_eq!(
        counts.retained_recomputes, 0,
        "the retained path is never entered from production"
    );
}

#[test]
fn the_atlas_contributes_no_dirty_rects_to_the_frame() {
    // Pins H2. `populate_frame` is called with an empty dirty set, so every
    // rect the atlas pushes is `dirty: false` and the atlas adds nothing to
    // the frame's dirty union.
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (f, counts) = frame(&mut interp, &tree);

    assert_eq!(counts.populate_calls, 1, "populate_frame runs once a frame");
    assert_eq!(
        counts.populate_dirty_targets, 0,
        "and receives no targets at all — not stale ones, none"
    );
    assert!(
        !f.rects().is_empty(),
        "the frame did receive geometry, so the empty dirty set is the finding \
         rather than an empty frame"
    );
    assert!(
        f.dirty().iter().all(|d| !d),
        "with no targets passed, nothing can be marked dirty"
    );
}

#[test]
fn the_view_generation_advances_every_frame() {
    // The mechanism `TargetId`'s generation field exists for — distinguishing
    // one *view* from another — degenerates into distinguishing one *frame*
    // from another when `clear()` runs per frame. This is what makes the dirty
    // channel unusable even if a caller did have targets to pass: they would
    // be a generation stale by the time `populate_frame` saw them.
    let (mut interp, tree) = build();
    let (_f, first) = frame(&mut interp, &tree);
    let (_f, second) = frame(&mut interp, &tree);

    assert_eq!(first.clears, 1);
    assert_eq!(second.clears, 1);
}

#[test]
fn the_counters_do_not_fire_when_nothing_renders() {
    // A negative case, so the assertions above are known to be measuring the
    // render path rather than something that happens to be true always.
    let (mut interp, _tree) = build();
    path_counters::reset();
    interp.tick();
    let counts = path_counters::snapshot();
    assert_eq!(counts.clears, 0);
    assert_eq!(counts.full_computes, 0);
    assert_eq!(counts.populate_calls, 0);
}

#[test]
fn a_structural_change_is_indistinguishable_from_a_value_change_at_the_atlas() {
    // The counters must not accidentally already encode "something structural
    // happened" — if they did, the retained-path condition would look
    // available when it is not. Mounting a whole new subtree and recolouring
    // one box produce the identical atlas call sequence.
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (_f, value_only) = frame(&mut interp, &tree);

    let mut interp2 = {
        let parsed = parse(SOURCE);
        let mut i = Interpreter::new();
        let _ = i.lower_view(&parsed.views[0], &[]);
        i.tick();
        i
    };
    let parsed = parse(SOURCE);
    let tree2 = interp2.lower_view(&parsed.views[0], &[]);
    let (_f, first_ever) = frame(&mut interp2, &tree2);

    assert_eq!(
        (value_only.clears, value_only.retained_recomputes),
        (first_ever.clears, first_ever.retained_recomputes),
        "the atlas cannot tell a recolour from a first render, which is exactly \
         why it cannot take a cheaper path on one of them"
    );
}

// ── Group 2: acceptance criteria for the retained path ─────────────────────

#[test]
#[ignore = "blocked: the interpreter has no per-element change signal — \
            attributes are raw expressions re-evaluated every frame, so it \
            cannot know whether anything layout-affecting changed. See this \
            file's module docs."]
fn the_retained_layout_path_is_taken_on_a_value_only_frame() {
    // The acceptance criterion for reactivating `recompute_dirty` in the
    // interpreter: a frame with no structural change, no resize, no hot reload
    // and no theme change must not tear the atlas down.
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (_f, counts) = frame(&mut interp, &tree);

    assert_eq!(
        counts.clears, 0,
        "a value-only frame must not clear the atlas"
    );
    assert_eq!(
        counts.retained_recomputes, 1,
        "it must take the retained path exactly once"
    );
    assert_eq!(counts.full_computes, 0, "and never the full one");
}

#[test]
#[ignore = "blocked: same missing per-element change signal — `ReactiveCtx` \
            tracks value bindings (Text content, Image src), not the attribute \
            surface, and there is no binding→atlas-node map. See this file's \
            module docs."]
fn the_atlas_marks_exactly_the_changed_node_dirty() {
    // The acceptance criterion for the atlas→frame dirty channel: one box's
    // colour changed, so exactly one rect comes back dirty — not zero (today)
    // and not all of them (which would make the channel useless in the other
    // direction).
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (f, counts) = frame(&mut interp, &tree);

    assert!(
        counts.populate_dirty_targets > 0,
        "the interpreter must pass the tick's dirty set, not an empty slice"
    );
    assert_eq!(
        f.dirty().iter().filter(|d| **d).count(),
        1,
        "exactly the recoloured node is dirty"
    );
}

#[test]
#[ignore = "blocked: depends on the retained layout path above — a structural \
            change can only be *required* to force a rebuild once a rebuild is \
            no longer unconditional. See this file's module docs."]
fn every_structural_change_still_forces_a_full_rebuild() {
    // The safety half of the retained path, and the more important half: the
    // fast path must be opt-in per condition, with rebuilding as the default.
    // A missed invalidation here does not merely look wrong — it leaves a
    // stale rect that hit-testing still answers from, i.e. an element that
    // appears to have moved but is tappable where it used to be.
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);

    // A resize is the simplest structural trigger that needs no grammar.
    path_counters::reset();
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(&tree, &mut f, W * 0.5, H);
    let counts = path_counters::snapshot();

    assert_eq!(
        counts.clears, 1,
        "a viewport change must always force the full rebuild"
    );
}
