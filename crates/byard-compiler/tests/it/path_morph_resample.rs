//! RFC-0031 §S11, `morph_mode: resample`: two body paths of any structure
//! morph, outline to outline, when the author asks for it.
//!
//! The claims are on the blended outline's bounds, which the alignment and the
//! winding change: a morph that twists or turns inside out collapses towards
//! its centre half way, and a morph that does not keeps its size.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
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

/// The blended outline's bounds `[x, y, w, h]` at `phase`.
fn bounds(mode: &str, phase: f32, from: &str, to: &str) -> [f32; 4] {
    let (mut interp, tree) = build(&format!(
        "View Main() {{
            Canvas #[width: 300, height: 200, morph: {phase}{mode}] {{
                path(fill: 0xFF3366CC) {{ {from} }}
                path(fill: 0xFF3366CC) {{ {to} }}
            }}
        }}"
    ));
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(frame.fills().len(), 1, "one blended outline");
    frame.fills()[0].mesh.bounds
}

const RESAMPLE: &str = ", morph_mode: resample";

/// A 100 x 100 square from its top-left corner, clockwise on screen.
const SQUARE: &str = "move(20, 20) line(120, 20) line(120, 120) line(20, 120) close()";
/// The same square with twice the commands, from the opposite corner.
const SQUARE_FROM_BOTTOM_RIGHT: &str = "move(120, 120) line(70, 120) line(20, 120) line(20, 70) \
     line(20, 20) line(70, 20) line(120, 20) line(120, 70) close()";
/// The same square, drawn the other way round.
const SQUARE_COUNTER_CLOCKWISE: &str =
    "move(20, 20) line(20, 120) line(120, 120) line(120, 20) close()";
/// A circle of radius 40 about (170, 70), four cubics.
const CIRCLE: &str = "move(210, 70) cubic(210, 92.1, 192.1, 110, 170, 110) \
     cubic(147.9, 110, 130, 92.1, 130, 70) cubic(130, 47.9, 147.9, 30, 170, 30) \
     cubic(192.1, 30, 210, 47.9, 210, 70) close()";

fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
}

#[test]
fn a_square_drawn_from_another_corner_stays_that_square() {
    // Point for point without alignment, (20, 20) would travel to (120, 120)
    // and every point would cross the centre: half way, a speck.
    let mid = bounds(RESAMPLE, 0.5, SQUARE, SQUARE_FROM_BOTTOM_RIGHT);
    assert!(
        close(mid, [20.0, 20.0, 100.0, 100.0], 0.5),
        "half way it is still the square: {mid:?}"
    );
}

#[test]
fn a_square_drawn_the_other_way_round_stays_that_square() {
    let mid = bounds(RESAMPLE, 0.5, SQUARE, SQUARE_COUNTER_CLOCKWISE);
    assert!(
        close(mid, [20.0, 20.0, 100.0, 100.0], 0.5),
        "the winding is matched before the start is: {mid:?}"
    );
}

#[test]
fn a_square_morphs_into_a_circle_within_their_bounds() {
    let (lo, hi) = ([20.0f32, 20.0], [210.0f32, 120.0]);
    for phase in [0.1, 0.25, 0.5, 0.75, 0.9] {
        let b = bounds(RESAMPLE, phase, SQUARE, CIRCLE);
        assert!(
            b[0] >= lo[0] - 0.5
                && b[1] >= lo[1] - 0.5
                && b[0] + b[2] <= hi[0] + 0.5
                && b[1] + b[3] <= hi[1] + 0.5,
            "phase {phase}: {b:?} leaves the union of the two shapes' bounds"
        );
        // Neither collapsed nor inside out: at least as big as the circle.
        assert!(b[2] >= 79.0 && b[3] >= 79.0, "phase {phase}: {b:?}");
    }
}

#[test]
fn the_ends_of_a_resampled_morph_are_its_two_paths() {
    let start = bounds(RESAMPLE, 0.001, SQUARE, CIRCLE);
    assert!(close(start, [20.0, 20.0, 100.0, 100.0], 0.5), "{start:?}");
    let end = bounds(RESAMPLE, 0.999, SQUARE, CIRCLE);
    assert!(close(end, [130.0, 30.0, 80.0, 80.0], 0.5), "{end:?}");
}

/// Without `morph_mode` a structural mismatch is still an error, and it now
/// names the way out.
#[test]
fn the_strict_default_still_refuses_and_names_resample() {
    let (interp, _) = build(&format!(
        "View Main() {{
            Canvas #[width: 300, height: 200, morph: 0.5] {{
                path(fill: 0xFF3366CC) {{ {SQUARE} }}
                path(fill: 0xFF3366CC) {{ {CIRCLE} }}
            }}
        }}"
    ));
    let err = interp
        .errors()
        .iter()
        .find(|e| e.kind() == "MorphPathMismatch")
        .unwrap_or_else(|| panic!("{:?}", interp.errors()));
    assert!(
        err.headline().contains("morph_mode: resample"),
        "{}",
        err.headline()
    );
}

/// Resampling pairs outline with outline; a path with a hole and one without
/// have no correspondence, and that is an error rather than a guess.
#[test]
fn a_different_number_of_subpaths_is_refused() {
    let (interp, _) = build(&format!(
        "View Main() {{
            Canvas #[width: 300, height: 200, morph: 0.5{RESAMPLE}] {{
                path(fill: 0xFF3366CC) {{
                    {SQUARE} move(50, 50) line(90, 50) line(90, 90) line(50, 90) close()
                }}
                path(fill: 0xFF3366CC) {{ {CIRCLE} }}
            }}
        }}"
    ));
    let err = interp
        .errors()
        .iter()
        .find(|e| e.kind() == "MorphSubpathMismatch")
        .unwrap_or_else(|| panic!("{:?}", interp.errors()));
    assert!(
        err.headline().contains("1 subpaths") && err.headline().contains('2'),
        "{}",
        err.headline()
    );
}
