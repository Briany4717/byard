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
//! # What changed
//!
//! The file used to hold two groups: the current behaviour pinned, and three
//! `#[ignore]`d acceptance criteria for behaviour that was blocked on a signal
//! the interpreter did not produce. RFC-0032 produces it, so the acceptance
//! criteria are now ordinary tests and the pins that described the *old*
//! behaviour are gone — they said "the atlas is torn down every frame", which
//! is no longer true and must not become true again.
//!
//! What the tests are pinning now, in order of how badly it hurts to get it
//! wrong:
//!
//! 1. **The retained path is taken.** A frame with no structural change, no
//!    resize, no reload, no theme flip and no overlay/route movement does not
//!    clear the atlas.
//! 2. **Every eligibility condition forces a rebuild.** This is the safety
//!    half and the more important one: the fast path is opt-in per condition,
//!    with rebuilding as the default (RFC-0032 §R4, default-deny).
//! 3. **Wrapping text still wraps.** `recompute_dirty` without a sizer falls
//!    back to a leaf's natural single-line size, which would silently un-wrap
//!    every paragraph on the frame after any retained one. RFC-0032 §R5 exists
//!    for this and it is the single most likely way the RFC ships a visible
//!    bug.
//! 4. **Hit-testing still answers from where things are.** A missed
//!    invalidation here does not merely look wrong: it leaves a rect that the
//!    spatial grid still answers from, i.e. an element that appears to have
//!    moved and is tappable where it used to be. That failure is invisible in
//!    a screenshot and invisible in a test that only checks pixels.
//! 5. **The dirty set is real.** A recolour produces one dirty primitive, not
//!    all of them and not none.

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

fn build_from(source: &str) -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    (interp, tree)
}

fn build() -> (Interpreter, Vec<RenderNode>) {
    build_from(SOURCE)
}

/// One frame of the runner loop, with the atlas path counters reset first so
/// the snapshot describes this frame alone.
fn frame(interp: &mut Interpreter, tree: &[RenderNode]) -> (RenderFrame, path_counters::Counts) {
    frame_at(interp, tree, W, H)
}

fn frame_at(
    interp: &mut Interpreter,
    tree: &[RenderNode],
    w: f32,
    h: f32,
) -> (RenderFrame, path_counters::Counts) {
    path_counters::reset();
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, w, h);
    (f, path_counters::snapshot())
}

/// Flips the `hot` var, so the next frame differs from the previous one by
/// exactly one box's colour and nothing else.
fn change_one_colour(interp: &mut Interpreter) {
    flip_bool(interp, "hot");
}

fn flip_bool(interp: &mut Interpreter, name: &str) {
    let sig = interp
        .var_signal(&Symbol::intern(name))
        .unwrap_or_else(|| panic!("the probe view declares `{name}`"));
    let current = interp.peek(sig).as_bool().unwrap_or(false);
    interp.write_var(sig, Value::Bool(!current));
}

// ── 1. The retained path is taken ──────────────────────────────────────────

#[test]
fn the_retained_layout_path_is_taken_on_a_value_only_frame() {
    // Was `#[ignore]`d, blocked on the interpreter having no per-element
    // change signal. RFC-0032 is that signal.
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
fn a_steady_scene_never_rebuilds_after_the_first_frame() {
    // The counter RFC-0032 §R7 names as the acceptance surface:
    // `full_computes` ~0 in steady state. Ten frames, no input at all.
    let (mut interp, tree) = build();
    let (_f, first) = frame(&mut interp, &tree);
    assert_eq!(first.full_computes, 1, "the first frame builds the tree");

    for i in 0..10 {
        let (_f, counts) = frame(&mut interp, &tree);
        assert_eq!(
            (counts.clears, counts.full_computes),
            (0, 0),
            "steady-state frame {i} rebuilt the atlas"
        );
    }
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

// ── 2. Every eligibility condition forces a rebuild (RFC-0032 §R4) ─────────
//
// One test per clause of the whitelist. A clause without a test is a clause
// that does not ship: "if any eligibility condition proves hard to test,
// remove it from the whitelist rather than shipping it untested."

/// Asserts that `frame_fn` produced a full rebuild rather than a retained pass,
/// and that the **whitelist** is what produced it.
///
/// The last assertion is the one with teeth, and it was missing. A frame the
/// whitelist wrongly admits is not wrong on screen — `end_retained_build`
/// refuses it and the caller clears and rebuilds — so it lands on exactly the
/// same `clears: 1, full_computes: 1, retained_recomputes: 0` as a frame the
/// whitelist rejected outright. Every §R4 clause could therefore be deleted
/// with this file still green, while production walked the tree twice on every
/// overlay toggle and every route change. `retained_attempts` is what tells the
/// two apart (INV-18: the assertion must fail when production stops taking the
/// cheaper path — here, the cheap *early-out*).
fn assert_rebuilt(counts: &path_counters::Counts, why: &str) {
    assert_eq!(counts.clears, 1, "{why} must force a full rebuild");
    assert_eq!(counts.full_computes, 1, "{why} must run a full layout pass");
    assert_eq!(
        counts.retained_recomputes, 0,
        "{why} must not take the retained path"
    );
    assert_eq!(
        counts.retained_attempts, 0,
        "{why} must be rejected by the §R4 whitelist before the atlas is \
         touched — this frame was admitted and then rolled back, which costs \
         the whole build walk twice and is invisible in every other counter"
    );
    assert_eq!(
        counts.retained_rollbacks, 0,
        "{why} produced a rolled-back retained build rather than a clean \
         rejection"
    );
}

#[test]
fn a_resize_forces_a_full_rebuild() {
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);
    let (_f, counts) = frame_at(&mut interp, &tree, W * 0.5, H);
    assert_rebuilt(&counts, "a viewport change");
}

#[test]
fn a_structural_change_forces_a_full_rebuild() {
    const STRUCTURAL: &str = r"
View Probe() {
    var shown = false
    Column #[width: 400, height: 300] {
        Box #[width: 100, height: 40, bg: 0x0000FF] {}
        when shown {
            Box #[width: 100, height: 40, bg: 0xFF0000] {}
        }
    }
}
";
    let (mut interp, tree) = build_from(STRUCTURAL);
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    flip_bool(&mut interp, "shown");
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "mounting a `when` branch");
}

#[test]
fn a_for_pool_growing_forces_a_full_rebuild() {
    const LOOP_SRC: &str = r"
View Probe() {
    var items = [1, 2]
    Column #[width: 400, height: 300] {
        for it in items {
            Box #[width: 100, height: 40, bg: 0x0000FF] {}
        }
    }
}
";
    let (mut interp, tree) = build_from(LOOP_SRC);
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    let sig = interp
        .var_signal(&Symbol::intern("items"))
        .expect("`items` is declared");
    interp.write_var(
        sig,
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
    );
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "a `for` list growing");
}

#[test]
fn a_theme_scheme_flip_forces_a_full_rebuild() {
    // RFC-0032 §Q6: a scheme flip changes nearly every resolved value at once,
    // so marking would visit everything and then recompute everything.
    let (mut interp, tree) = build();
    interp.set_theme(byard_compiler::interp::theme::Theme::default());
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    interp.set_theme_dark(!interp.theme_is_dark());
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "a theme scheme flip");
}

#[test]
fn a_hot_reload_forces_a_full_rebuild() {
    use byard_compiler::interp::reload::diff_view;

    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    let old = parse(SOURCE);
    let new = parse(SOURCE);
    interp.reload(&new.views[0], diff_view(&old.views[0], &new.views[0]));
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "a hot reload");
}

#[test]
fn mounting_an_overlay_forces_a_full_rebuild() {
    // An overlay mounts through a `when` guard (RFC-0017) — there is no
    // `visible:` attribute — so this also re-covers the structural clause. The
    // overlay-count clause it targets is nonetheless load-bearing on its own:
    // overlay and navigation pools do not travel through
    // `reconcile_structure`, which is why `a_route_push_forces_a_full_rebuild`
    // below can move a route without any `when` in sight.
    const OVERLAY_SRC: &str = r"
View Probe() {
    var open = false
    Column #[width: 400, height: 300] {
        Box #[width: 100, height: 40, bg: 0x0000FF] {}
        when open {
            Overlay #[modal: true] {
                Box #[width: 80, height: 80, bg: 0xFF0000] {}
            }
        }
    }
}
";
    let (mut interp, tree) = build_from(OVERLAY_SRC);
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    flip_bool(&mut interp, "open");
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "mounting an overlay");
}

#[test]
fn a_route_push_forces_a_full_rebuild() {
    const NAV_SRC: &str = r#"
View Probe() {
    var path = "/"
    NavStack(path: path) #[width: 400, height: 300] {
        route "/" {
            Box #[width: 100, height: 40, bg: 0x0000FF] {}
        }
        route "/detail" {
            Box #[width: 100, height: 40, bg: 0xFF0000] {}
        }
    }
}
"#;
    let (mut interp, tree) = build_from(NAV_SRC);
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    let sig = interp
        .var_signal(&Symbol::intern("path"))
        .expect("`path` is declared");
    interp.write_var(sig, Value::Str("/detail".to_string()));
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "a route push");
}

#[test]
fn unmounting_an_overlay_forces_a_full_rebuild() {
    // §R4 names "no overlay or route **mount/unmount**", and the two halves are
    // not the same clause running twice. A mount adds nodes, so a length check
    // catches it even if the overlay clause were missing; an unmount *removes*
    // them, which is the direction where a surviving stale entry is a rect the
    // spatial grid still answers from — a dismissed dialog that keeps eating
    // taps over the screen behind it (INV-23's failure mode, and invisible in a
    // screenshot).
    const OVERLAY_SRC: &str = r"
View Probe() {
    var open = true
    Column #[width: 400, height: 300] {
        Box #[width: 100, height: 40, bg: 0x0000FF] {}
        when open {
            Overlay #[modal: true] {
                Box #[width: 80, height: 80, bg: 0xFF0000] {}
            }
        }
    }
}
";
    let (mut interp, tree) = build_from(OVERLAY_SRC);
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    // `open` starts *true*, so this flip is the dismissal.
    flip_bool(&mut interp, "open");
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "dismissing an overlay");
}

#[test]
fn a_route_pop_forces_a_full_rebuild() {
    // The other half of §R4's "mount/unmount" clause, on the navigation pool.
    // A pop is the case that cannot be caught by a `flat_ids` length check
    // alone: RFC-0026 keeps the popped screen's subtree alive underneath for
    // state preservation, so the walk can come back the same length as a frame
    // that changed nothing.
    const NAV_SRC: &str = r#"
View Probe() {
    var path = "/"
    NavStack(path: path) #[width: 400, height: 300] {
        route "/" {
            Box #[width: 100, height: 40, bg: 0x0000FF] {}
        }
        route "/detail" {
            Box #[width: 100, height: 40, bg: 0xFF0000] {}
        }
    }
}
"#;
    let (mut interp, tree) = build_from(NAV_SRC);
    let sig = interp
        .var_signal(&Symbol::intern("path"))
        .expect("`path` is declared");

    // Push, and let the transition settle, so the frame under test is a pop and
    // nothing else.
    interp.write_var(sig, Value::Str("/detail".to_string()));
    for _ in 0..32 {
        let _pushing = frame(&mut interp, &tree);
    }

    interp.write_var(sig, Value::Str("/".to_string()));
    let (_f, counts) = frame(&mut interp, &tree);
    assert_rebuilt(&counts, "a route pop");
}

// ── 3. Wrapping text still wraps across a retained frame (RFC-0032 §R5) ────
//
// RFC-0005 lowers `Text` to three different layout shapes, and they are three
// different measurements rather than one with a parameter:
//
//   * no `width`, `wrap` defaulting to `true` — a measured leaf the atlas sizes
//     to whatever width its parent offers, resolved *inside* layout;
//   * an explicit `width` — a measured leaf with a fixed wrap width, the same
//     protocol against a different bound;
//   * `wrap: false` — not a measured leaf at all, but a plain fixed leaf at the
//     natural single-line size, which never reaches the sizer.
//
// The retained path can break the first two by losing the sizer and the third
// by reusing the wrong build-order slot, so each gets its own test.

/// One paragraph, shared by every wrap-mode fixture, so the three modes are
/// measurements of the *same string* and their heights can be compared.
const PARAGRAPH: &str = "A paragraph long enough that it must wrap onto several \
     lines when it is offered only the width of a narrow column, which is the \
     whole point of measuring it inside layout.";

/// A column holding a box, one `Text` carrying `text_attrs`, and a trailing box
/// whose `y` is the observable.
///
/// `col_width` and `text_attrs` may both read the `narrow` var, which is what
/// [`text_layout_across_a_retained_frame`] flips — so each mode can be given
/// the change that actually forces *its* leaf to be re-measured.
fn wrap_fixture(col_width: &str, text_attrs: &str) -> String {
    format!(
        r#"
View Probe() {{
    var narrow = false
    Column #[width: {col_width}, height: 600] {{
        Box #[width: 40, height: 20, bg: 0x0000FF] {{}}
        Text("{PARAGRAPH}") #[{text_attrs}]
        Box #[width: 40, height: 20, bg: 0x00FF00] {{}}
    }}
}}
"#
    )
}

const WRAP_SRC: &str = r#"
View Probe() {
    var hot = false
    Column #[width: 240, height: 400] {
        Box #[width: 40, height: 20, bg: hot ? 0xFF0000 : 0x0000FF] {}
        Text("A paragraph long enough that it must wrap onto several lines when it is offered only the width of this narrow column, which is the whole point of measuring it inside layout.")
            #[size: 14]
        Text("A second unwrapped run") #[size: 14, wrap: false]
        Box #[width: 40, height: 20, bg: 0x00FF00] {}
    }
}
"#;

/// How far down the column the *last* emitted solid box sits.
///
/// This is the observable that proves a paragraph above it wrapped: the
/// paragraph's own rect is not directly addressable from here, but everything
/// below it moves by exactly the height it occupies. An un-wrapped (collapsed
/// to one line) paragraph pulls its siblings up, which is precisely the
/// visible bug RFC-0032 §R5 exists to prevent.
fn last_box_y(f: &RenderFrame) -> f32 {
    f.instances()
        .iter()
        .map(|b| b.rect[1])
        .fold(f32::MIN, f32::max)
}

/// One line of the fixtures' 14 px text (`cosmic-text`'s 1.2× line height).
const LINE_H: f32 = 14.0 * 1.2;

/// The height of the box the wrap fixtures put *above* the paragraph, so the
/// trailing box's `y` can be turned back into the paragraph's own height.
const TOP_BOX_H: f32 = 20.0;

/// Drives one wrap-mode fixture through a full frame and then a **retained**
/// one with `narrow` flipped, and returns the **paragraph's height** on each.
///
/// The flip is the load-bearing part of this helper. Taffy invokes the measure
/// callback only for leaves it is actually recomputing, so a retained frame
/// that changes something *unrelated* to the paragraph never re-measures it —
/// and a test built on one passes whether or not the sizer is there at all.
/// (It did: the original §R5 test flipped a colour, and stayed green with
/// `recompute_dirty_with_text` swapped back to the sizer-less
/// `recompute_dirty`.) Each fixture therefore binds `narrow` to the bound that
/// governs *its* mode's wrap width, so the retained frame has no choice but to
/// re-measure the leaf through the protocol under test.
fn text_layout_across_a_retained_frame(src: &str) -> (f32, f32) {
    let (mut interp, tree) = build_from(src);
    let (full, first) = frame(&mut interp, &tree);
    assert_eq!(first.full_computes, 1, "the first frame is the full one");
    let full_h = last_box_y(&full) - TOP_BOX_H;

    flip_bool(&mut interp, "narrow");
    let (retained, counts) = frame(&mut interp, &tree);
    assert_eq!(counts.retained_recomputes, 1, "this frame must be retained");
    assert_eq!(counts.clears, 0);
    (full_h, last_box_y(&retained) - TOP_BOX_H)
}

/// Asserts that narrowing a wrapping leaf's bound made it **taller** on the
/// retained frame — i.e. that it re-wrapped rather than collapsing.
///
/// The failure mode this names is one-directional and worth stating: without
/// the sizer the leaf reports its natural *single-line* size, so the paragraph
/// gets **shorter**. Asserting "grew by at least one line" catches that with no
/// dependence on the exact font metrics of the machine running it.
fn assert_rewrapped(mode: &str, full_h: f32, retained_h: f32) {
    assert!(
        retained_h >= full_h + LINE_H,
        "{mode}: narrowing the wrap width did not add a line on the retained \
         frame — the paragraph went from {full_h} px tall to {retained_h}. \
         Anything at or below the starting height means it collapsed towards \
         its natural single line, which is the retained path having lost its \
         text sizer (RFC-0032 §R5)"
    );
}

/// The three modes, as they are written in `.byd`. Each binds `narrow` to the
/// bound that governs its own wrap width — the column's for the available-width
/// mode, the attribute's for the fixed one — except `wrap: false`, which has no
/// wrap width to govern and is pinned as unchanging instead.
fn available_width_fixture() -> String {
    wrap_fixture("narrow ? 200 : 400", "size: 14")
}
fn fixed_width_fixture() -> String {
    wrap_fixture("400", "size: 14, width: narrow ? 100 : 200")
}
fn no_wrap_fixture() -> String {
    wrap_fixture("narrow ? 200 : 400", "size: 14, wrap: false")
}

#[test]
fn wrapping_text_still_wraps_across_a_retained_frame() {
    // The trap this guards: `recompute_dirty` runs the measure protocol with
    // **no sizer**, so a wrapping `Text` leaf falls back to its natural
    // single-line size and every paragraph silently un-wraps on the frame
    // after any retained one. `recompute_dirty_with_text` exists solely for
    // this, and this test is what stops someone "simplifying" it away.
    //
    // Mode 1: no `width`, so the leaf wraps to whatever the parent offers and
    // the column's own width is what moves.
    let (full_h, retained_h) = text_layout_across_a_retained_frame(&available_width_fixture());
    assert!(
        full_h > LINE_H,
        "the fixture's paragraph must already wrap at 400 px for this test to \
         mean anything; it is {full_h} px tall, i.e. one line"
    );
    assert_rewrapped("wrap to the available width", full_h, retained_h);
}

#[test]
fn a_fixed_wrap_width_still_wraps_across_a_retained_frame() {
    // Mode 2, a different branch of the same protocol: an explicit `width`
    // fixes the wrap width, so `TextLeaf.width` is `Some(_)` and the leaf wraps
    // to *that* rather than to what the parent offers. It reaches the sizer by
    // the same route and un-wraps by the same failure, and nothing asserted it.
    let (full_h, retained_h) = text_layout_across_a_retained_frame(&fixed_width_fixture());
    assert!(
        full_h > LINE_H,
        "the fixture must already wrap at its 200 px fixed width; it is \
         {full_h} px tall, i.e. one line"
    );
    assert_rewrapped("wrap to a fixed width", full_h, retained_h);
}

#[test]
fn a_non_wrapping_run_keeps_its_single_line_across_a_retained_frame() {
    // Mode 3, and the one that does *not* go through the sizer at all:
    // `wrap: false` lowers to a plain fixed leaf at the natural single-line
    // size. It cannot un-wrap — which is exactly why it is worth pinning,
    // because it is the mode a retained pass must leave *alone*. Its column is
    // narrowed by the same flip the wrapping modes re-wrap on: the run must
    // overflow rather than reflow, and a build-order slot reused by the wrong
    // leaf shows up here as a run that suddenly acquires a paragraph's height.
    let (full_h, retained_h) = text_layout_across_a_retained_frame(&no_wrap_fixture());
    assert!(
        full_h < 2.0 * LINE_H,
        "`wrap: false` must stay on one line; the run measured {full_h} px \
         tall, which is more than one"
    );
    assert!(
        (retained_h - full_h).abs() < 0.5,
        "`wrap: false`: a run that opted out of wrapping reflowed anyway when \
         its column narrowed — it went from {full_h} px tall to {retained_h}"
    );
}

#[test]
fn the_three_wrap_modes_measure_three_different_things() {
    // The guard over the three tests above. Each of them asserts something
    // about how *its* mode responds to a narrower bound, and all three would
    // still pass if the modes had quietly collapsed into a single
    // implementation with the same measurement behind it.
    //
    // Same paragraph, three modes, three heights, in the order the widths
    // dictate: 200 px wraps onto the most lines, the column's 400 px onto
    // fewer, and the opted-out run onto exactly one.
    let (fixed, _) = text_layout_across_a_retained_frame(&fixed_width_fixture());
    let (available, _) = text_layout_across_a_retained_frame(&available_width_fixture());
    let (nowrap, _) = text_layout_across_a_retained_frame(&no_wrap_fixture());
    assert!(
        fixed > available && available > nowrap,
        "the three wrap modes must be three distinct measurements of the same \
         string; the paragraph measured {fixed} px tall (fixed 200 px), \
         {available} (the column's 400 px) and {nowrap} (`wrap: false`)"
    );
}

#[test]
fn a_text_content_change_is_layout_class_and_reflows_on_the_retained_path() {
    // The row RFC-0032 §R2's classification table turns on: text content is
    // layout-class. If it were treated as paint-only, an edited paragraph
    // would keep the previous string's line count and the box below it would
    // never move.
    const EDIT_SRC: &str = r#"
View Probe() {
    var long = false
    Column #[width: 200, height: 400] {
        Text(long ? "A much longer paragraph that certainly needs more than one line at this width and then some more words to be sure of it." : "short")
            #[size: 14]
        Box #[width: 40, height: 20, bg: 0x00FF00] {}
    }
}
"#;
    let (mut interp, tree) = build_from(EDIT_SRC);
    let (short, _) = frame(&mut interp, &tree);
    let short_y = last_box_y(&short);

    flip_bool(&mut interp, "long");
    let (long, counts) = frame(&mut interp, &tree);
    assert_eq!(
        counts.retained_recomputes, 1,
        "editing text is not a structural change — the tree is retained"
    );
    assert!(
        last_box_y(&long) > short_y,
        "the edited paragraph did not reflow: the box below it stayed at \
         y={short_y} (now {})",
        last_box_y(&long)
    );
    assert!(
        counts.populate_dirty_targets > 0,
        "a layout-affecting change must reach `populate_frame` as a dirty target"
    );
    assert_eq!(
        counts.populate_dirty_matched, counts.populate_dirty_targets,
        "every target must match a live node — a lower ratio means the targets \
         are generation-stale, which is a caller bug rather than an empty set"
    );
}

// ── 4. Hit-testing after a retained frame ─────────────────────────────────

#[test]
fn hit_testing_lands_on_the_element_that_is_visually_there() {
    // The failure this rules out is the one that stopped PR #148: a rect that
    // is stale but still in the spatial grid, so an element renders in its new
    // place and answers taps in its old one. Invisible in a screenshot.
    //
    // The case exercised is deliberately the hard one — a **sibling reflow**.
    // The first box's height changes, so the second box moves even though
    // nothing about *it* changed and nothing marked it. RFC-0032 §R3 delegates
    // that to Taffy's own dirty propagation and rebuilds the grid from the
    // resolved rects; this test is the proof that the delegation works.
    const REFLOW_SRC: &str = r"
View Probe() {
    var tall = false
    Column #[width: 200, height: 400] {
        Box #[width: 100, height: tall ? 120 : 40, bg: 0x0000FF] {}
        Box #[width: 100, height: 40, bg: 0x00FF00] {}
    }
}
";
    let (mut interp, tree) = build_from(REFLOW_SRC);
    let _warmup = frame(&mut interp, &tree);

    let before = interp
        .atlas
        .hit_test(50.0, 60.0)
        .and_then(|n| interp.atlas.node_index(n));

    flip_bool(&mut interp, "tall");
    let (_f, counts) = frame(&mut interp, &tree);
    assert_eq!(
        counts.retained_recomputes, 1,
        "a height change is a value change, not a structural one"
    );

    // y = 60 was inside the second box; after the first box grows to 120 it is
    // inside the *first* one. If the grid were stale, the answer would not
    // have changed.
    let after = interp
        .atlas
        .hit_test(50.0, 60.0)
        .and_then(|n| interp.atlas.node_index(n));
    assert!(
        before != after,
        "the spatial grid still answers from the pre-reflow geometry — a node \
         that moved because a sibling resized was never re-indexed"
    );

    // And the moved-but-unmarked sibling is reachable at its new position.
    let moved = interp.atlas.hit_test(50.0, 140.0);
    assert!(
        moved.is_some(),
        "the second box is not hit-testable at the position it was pushed to"
    );
}

// ── 5. A real dirty set, end to end ───────────────────────────────────────

#[test]
fn a_single_colour_change_produces_exactly_one_dirty_primitive() {
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (f, _counts) = frame(&mut interp, &tree);

    let dirty: Vec<usize> = f
        .instances_dirty()
        .iter()
        .enumerate()
        .filter(|(_, d)| **d)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        dirty.len(),
        1,
        "exactly the recoloured box is dirty; got {dirty:?} of {} instances",
        f.instances().len()
    );
    assert!(
        f.texts().iter().all(|t| !t.dirty),
        "the text did not change and must not be re-shaped"
    );
}

#[test]
fn a_paint_only_frame_marks_no_layout_targets() {
    // The other half of the previous test, and the reason `populate_frame`'s
    // target set is *not* simply "everything that changed": a recolour has no
    // layout consequence, so nothing is marked, nothing is recomputed, and no
    // text leaf is re-measured. `targets_received > 0` is the acceptance
    // criterion for a **layout-affecting** frame (covered above), not for
    // every frame in which some value moved.
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (_f, counts) = frame(&mut interp, &tree);
    assert_eq!(
        counts.populate_dirty_targets, 0,
        "a colour is not a layout input"
    );
    assert_eq!(counts.populate_calls, 1);
}

#[test]
fn an_unchanged_frame_marks_nothing_dirty_at_all() {
    let (mut interp, tree) = build();
    let _warmup = frame(&mut interp, &tree);
    let _settle = frame(&mut interp, &tree);

    let (f, counts) = frame(&mut interp, &tree);
    assert_eq!(counts.populate_dirty_targets, 0);
    assert!(
        f.instances_dirty().iter().all(|d| !d),
        "nothing changed, so no box may be reported dirty"
    );
    assert!(
        f.texts().iter().all(|t| !t.dirty),
        "nothing changed, so no text line may be re-shaped"
    );
}

#[test]
fn the_first_frame_reports_everything_dirty() {
    // The mirror image, and the reason the digest carries a `primed` flag: a
    // first frame in which nothing is dirty would simply never be drawn.
    let (mut interp, tree) = build();
    let (f, _counts) = frame(&mut interp, &tree);
    assert!(
        f.instances_dirty().iter().all(|d| *d),
        "every instance on the first frame must be dirty"
    );
    assert!(f.texts().iter().all(|t| t.dirty));
}

// ── Parity: the retained frame paints what a rebuilt frame paints ─────────

#[test]
fn a_retained_frame_is_byte_identical_to_a_forced_full_rebuild() {
    // INV-22: the retained path is not intended to change any output, so any
    // difference is a bug. Comparing the emitted primitives rather than pixels
    // makes the check exact and cheap enough to run on every commit — a pixel
    // comparison of the same frame lives in `byard-platform`'s readback tests.
    let (mut interp, tree) = build_from(WRAP_SRC);
    let _warmup = frame(&mut interp, &tree);

    change_one_colour(&mut interp);
    let (retained, counts) = frame(&mut interp, &tree);
    assert_eq!(counts.retained_recomputes, 1);

    // Same interpreter, same state, same frame — but rebuilt.
    interp.invalidate_retained_layout();
    let (rebuilt, counts) = frame(&mut interp, &tree);
    assert_eq!(counts.full_computes, 1);

    let boxes = |f: &RenderFrame| -> Vec<[f32; 12]> {
        f.instances()
            .iter()
            .map(|b| {
                let mut v = [0.0; 12];
                v[..4].copy_from_slice(&b.rect);
                v[4..8].copy_from_slice(&b.color);
                v[8..].copy_from_slice(&b.radii);
                v
            })
            .collect()
    };
    assert_eq!(
        boxes(&retained),
        boxes(&rebuilt),
        "the retained path emitted different solid geometry"
    );
    assert_eq!(
        retained.rects(),
        rebuilt.rects(),
        "the retained path resolved different layout rects"
    );
    let texts = |f: &RenderFrame| -> Vec<(String, f32, f32, f32)> {
        f.texts()
            .iter()
            .map(|t| (t.text.clone(), t.x, t.y, t.font_size))
            .collect()
    };
    assert_eq!(
        texts(&retained),
        texts(&rebuilt),
        "the retained path placed or shaped text differently"
    );
}
