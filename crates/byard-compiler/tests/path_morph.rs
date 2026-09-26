//! RFC-0031 §S11: two body paths with the same commands morph command by
//! command, and two that differ are refused, naming the first command that
//! disagrees.
//!
//! What is asserted is geometry and counts, not pixels: where the blended
//! outline lands, that one fill is drawn rather than every member, and that
//! the mesh is re-tessellated while the phase moves and not once it settles.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;

fn build(src: &str) -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    (interp, tree)
}

fn render_at(interp: &mut Interpreter, tree: &[RenderNode], ms: u32) -> RenderFrame {
    interp.set_now_ms(ms);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 300.0);
    frame
}

/// A 100 x 100 right triangle and a 200 x 50 one: the same three commands,
/// so they morph, and every blend has a known extent.
fn triangles(morph: &str) -> String {
    format!(
        "View Main() {{
            Canvas #[width: 300, height: 200, morph: {morph}] {{
                path(fill: 0xFF3366CC) {{ move(0, 0) line(100, 0) line(0, 100) close() }}
                path(fill: 0xFFE8734A) {{ move(0, 0) line(200, 0) line(0, 50) close() }}
            }}
        }}"
    )
}

fn extent(frame: &RenderFrame) -> (f32, f32) {
    let fills = frame.fills();
    assert_eq!(
        fills.len(),
        1,
        "a morph draws one blended path, not each member"
    );
    (fills[0].mesh.bounds[2], fills[0].mesh.bounds[3])
}

#[test]
fn a_midpoint_lies_between_the_endpoints_command_by_command() {
    for (phase, want) in [
        ("0.0", (100.0, 100.0)),
        ("1.0", (200.0, 50.0)),
        ("0.5", (150.0, 75.0)),
        ("0.25", (125.0, 87.5)),
    ] {
        let (mut interp, tree) = build(&triangles(phase));
        let frame = render_at(&mut interp, &tree, 0);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let (w, h) = extent(&frame);
        assert!(
            (w - want.0).abs() < 0.5 && (h - want.1).abs() < 0.5,
            "phase {phase}: {w} x {h}, wanted {want:?}"
        );
    }
}

/// The phase wraps, as the distance-field morph's does: two members and a
/// phase of 2 is the first member again, and 1.5 is half way back.
#[test]
fn the_phase_wraps_round_the_sequence() {
    for (phase, want) in [("2.0", (100.0, 100.0)), ("1.5", (150.0, 75.0))] {
        let (mut interp, tree) = build(&triangles(phase));
        let (w, h) = extent(&render_at(&mut interp, &tree, 0));
        assert!(
            (w - want.0).abs() < 0.5 && (h - want.1).abs() < 0.5,
            "phase {phase}: {w} x {h}"
        );
    }
}

/// The fill rides the same phase, so a morph that also changes colour is one
/// animation. Half way it is neither end.
#[test]
fn the_fill_blends_with_the_geometry() {
    let colour = |phase: &str| {
        let (mut interp, tree) = build(&triangles(phase));
        render_at(&mut interp, &tree, 0).fills()[0].color
    };
    let (a, mid, b) = (colour("0.0"), colour("0.5"), colour("1.0"));
    let d = |x: [f32; 4], y: [f32; 4]| -> f32 { x.iter().zip(y).map(|(p, q)| (p - q).abs()).sum() };
    assert!(d(a, b) > 0.2, "the two ends differ: {a:?} {b:?}");
    assert!(
        d(mid, a) > 0.05 && d(mid, b) > 0.05,
        "half way is between: {a:?} {mid:?} {b:?}"
    );
}

/// Unlike structures are a check-time error naming the path and the command.
#[test]
fn mismatched_structures_name_the_first_command_that_disagrees() {
    let (interp, _) = build(
        "View Main() {
            Canvas #[width: 300, height: 200, morph: 0.5] {
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) line(0, 100) close() }
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) cubic(50, 50, 20, 80, 0, 100) close() }
            }
        }",
    );
    let errs = format!("{:?}", interp.errors());
    assert!(errs.contains("MorphPathMismatch"), "{errs}");
    assert!(
        errs.contains("member: 1") && errs.contains("index: 2"),
        "path 1, command 2: {errs}"
    );
    assert!(
        errs.contains("expected: \"line\"") && errs.contains("found: \"cubic\""),
        "{errs}"
    );
}

/// A path that ends early is a mismatch at the first command it lacks, and
/// with three members the wrap from the last back to the first is checked.
#[test]
fn a_shorter_path_and_the_wrap_are_checked_too() {
    let (interp, _) = build(
        "View Main() {
            Canvas #[width: 300, height: 200, morph: 0.5] {
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) line(0, 100) close() }
                path(fill: 0xFF3366CC) { move(0, 0) line(90, 0) line(0, 90) close() }
                path(fill: 0xFF3366CC) { move(0, 0) line(80, 0) line(0, 80) }
            }
        }",
    );
    let errs: Vec<String> = interp
        .errors()
        .iter()
        .map(|e| format!("{e:?}"))
        .filter(|e| e.contains("MorphPathMismatch"))
        .collect();
    assert_eq!(errs.len(), 2, "1->2 and the wrap 2->0: {errs:?}");
    assert!(errs.iter().all(|e| e.contains("index: 3")), "{errs:?}");
    assert!(
        errs.iter().any(|e| e.contains("member: 0")),
        "the wrap names the first path: {errs:?}"
    );
}

/// Body paths and distance-field shapes cannot share a sequence, and a
/// `path(d: …)` cannot morph at all.
#[test]
fn a_morph_cannot_mix_paths_with_other_shapes() {
    let (interp, _) = build(
        "View Main() {
            Canvas #[width: 300, height: 200, morph: 0.5] {
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) line(0, 100) close() }
                circle(cx: 50, cy: 50, r: 40, fill: 0xFF3366CC)
            }
        }",
    );
    let errs = format!("{:?}", interp.errors());
    assert!(errs.contains("MorphMemberKind"), "{errs}");

    let (interp, _) = build(
        "View Main() {
            Canvas #[width: 300, height: 200, morph: 0.5] {
                path(d: \"M0 0 L10 0 L0 10 Z\", fill: 0xFF3366CC)
                circle(cx: 50, cy: 50, r: 40, fill: 0xFF3366CC)
            }
        }",
    );
    let errs = format!("{:?}", interp.errors());
    assert!(errs.contains("MorphMemberKind"), "{errs}");
}

/// When a `when` chooses the members, the order is only known while
/// rendering, and the same rule is applied there: reported once, not per
/// frame.
#[test]
fn a_mismatch_chosen_by_when_is_reported_while_rendering_once() {
    let (mut interp, tree) = build(
        "View Main() {
            var round = true
            Canvas #[width: 300, height: 200, morph: 0.5] {
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) line(0, 100) close() }
                when round {
                    path(fill: 0xFF3366CC) { move(0, 0) quad(100, 100, 0, 100) close() }
                }
            }
        }",
    );
    assert!(
        !format!("{:?}", interp.errors()).contains("MorphPathMismatch"),
        "not knowable before rendering"
    );
    for ms in [0, 16, 32] {
        let _ = render_at(&mut interp, &tree, ms);
    }
    let count = interp
        .errors()
        .iter()
        .filter(|e| format!("{e:?}").contains("MorphPathMismatch"))
        .count();
    assert_eq!(count, 1, "{:?}", interp.errors());
}

/// While the phase moves the blend is new geometry every frame and is
/// tessellated; once it settles the numbers repeat and the cache answers.
#[test]
fn the_mesh_retessellates_while_morphing_and_stops_when_it_settles() {
    let (mut interp, tree) = build(
        "View Main() {
            var on = false
            Canvas #[width: 300, height: 200,
                     morph: on ? 1.0 : 0.0 with anim.linear(200ms)] {
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) line(0, 100) close() }
                path(fill: 0xFF3366CC) { move(0, 0) line(200, 0) line(0, 50) close() }
            }
        }",
    );
    let _ = render_at(&mut interp, &tree, 1000);
    let _ = render_at(&mut interp, &tree, 1016);
    let rest = interp.tessellations();
    let sig = interp.var_signal(&Symbol::intern("on")).expect("`on`");
    interp.write_var(sig, Value::Bool(true));

    let mut widths = Vec::new();
    for ms in [1032, 1080, 1128, 1176] {
        widths.push(extent(&render_at(&mut interp, &tree, ms)).0);
    }
    // The first of those frames is the one the write lands in: the animation
    // starts there, at phase 0, which is the resting geometry and a cache hit.
    // Every later one is new.
    let moving = interp.tessellations() - rest;
    assert!(
        (widths[0] - 100.0).abs() < 0.5,
        "the morph starts from where it rested: {widths:?}"
    );
    assert!(
        widths.windows(2).all(|w| w[1] > w[0]),
        "the outline moves towards the second path: {widths:?}"
    );
    assert_eq!(
        moving, 3,
        "each moving frame is new geometry, and only those: {widths:?}"
    );

    let _ = render_at(&mut interp, &tree, 2000);
    let settled = interp.tessellations();
    for ms in [2016, 2032, 2048] {
        let (w, _) = extent(&render_at(&mut interp, &tree, ms));
        assert!((w - 200.0).abs() < 0.5, "settled on the second path: {w}");
    }
    assert_eq!(
        interp.tessellations(),
        settled,
        "a settled morph must not tessellate again"
    );
}
