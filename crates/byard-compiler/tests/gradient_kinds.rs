//! The `gradient:` surface, extended with a kind (RFC-0035).
//!
//! The shape of a gradient is decided at compile time and travels to the GPU as
//! one tag plus four control floats whose meaning depends on it. These tests
//! are about that translation: which lane holds what for which kind, what a
//! file written before this RFC still means, and the three validations the RFC
//! asks for.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::frame::{GRADIENT_NONE, GradientKind, RenderFrame};

/// Lowers and renders one view, returning its frame.
fn frame_of(source: &str) -> (Interpreter, RenderFrame) {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree: Vec<RenderNode> = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    (interp, frame)
}

/// A box carrying `gradient: (<fields>)`.
fn card(fields: &str) -> String {
    format!(
        "View Main() {{ Box #[bg: 0x101010, radius: 12, width: 200, height: 100, \
         gradient: ({fields})] {{}} }}"
    )
}

#[test]
fn a_gradient_with_no_kind_is_the_linear_ramp_it_always_was() {
    let (interp, frame) = frame_of(&card("angle: 90deg, from: 0xFF0000FF, to: 0xFF00FF00"));
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let g = frame.decorated()[0].gradient.expect("a gradient");
    assert_eq!(g.kind, GradientKind::Linear);
    // The historical lane layout, unchanged: direction, mid position, offset.
    let [dx, dy, mid_pos, offset] = g.axis();
    assert!((dx - g.angle.cos()).abs() < f32::EPSILON);
    assert!((dy - g.angle.sin()).abs() < f32::EPSILON);
    assert!((mid_pos - 0.5).abs() < f32::EPSILON);
    assert!(offset.abs() < f32::EPSILON);
}

#[test]
fn a_radial_gradient_carries_its_centre_and_radius() {
    let (interp, frame) = frame_of(&card(
        "kind: radial, center: (1.0, 0.0), radius: 0.9, from: 0xFF0E3D2F, to: 0xFF000000",
    ));
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let g = frame.decorated()[0].gradient.expect("a gradient");
    assert_eq!(g.kind, GradientKind::Radial);
    // Bit equality: these are values the compiler wrote verbatim, not the
    // result of any arithmetic, so an exact comparison is the right claim.
    assert_eq!(
        g.axis().map(f32::to_bits),
        [1.0_f32, 0.0, 0.9, 0.5].map(f32::to_bits)
    );
}

#[test]
fn a_conic_gradient_carries_its_centre_and_start_angle() {
    let (interp, frame) = frame_of(&card(
        "kind: conic, center: (0.5, 0.5), start: 90deg, \
         from: 0xFFEF4444, mid: 0xFFF59E0B, to: 0xFFEF4444",
    ));
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let g = frame.decorated()[0].gradient.expect("a gradient");
    assert_eq!(g.kind, GradientKind::Conic);
    let [cx, cy, start, mid_pos] = g.axis();
    assert!((cx - 0.5).abs() < f32::EPSILON && (cy - 0.5).abs() < f32::EPSILON);
    assert!(
        (start - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
        "{start}"
    );
    assert!((mid_pos - 0.5).abs() < f32::EPSILON);
}

#[test]
fn a_negative_start_angle_is_the_same_bytes_as_its_positive_twin() {
    // `start: -90deg` and `start: 270deg` are the same sweep. Normalising at
    // compile time means the paint digest sees one value rather than two
    // spellings of it, so a file that switches between them does not repaint.
    let (_, a) = frame_of(&card(
        "kind: conic, start: -90deg, from: 0xFFEF4444, to: 0xFFEF4444",
    ));
    let (_, b) = frame_of(&card(
        "kind: conic, start: 270deg, from: 0xFFEF4444, to: 0xFFEF4444",
    ));
    let (ga, gb) = (
        a.decorated()[0].gradient.unwrap(),
        b.decorated()[0].gradient.unwrap(),
    );
    assert_eq!(ga.axis().map(f32::to_bits), gb.axis().map(f32::to_bits));
}

#[test]
fn a_two_stop_call_synthesizes_the_middle_stop() {
    let (_, frame) = frame_of(&card(
        "kind: radial, radius: 0.8, from: 0xFF000000, to: 0xFFFFFFFF",
    ));
    let g = frame.decorated()[0].gradient.expect("a gradient");
    for c in 0..3 {
        assert!(
            (g.mid[c] - 0.5).abs() < 0.01,
            "the mid stop is the midpoint of the ends, got {:?}",
            g.mid
        );
    }
}

/// Lowers and renders a source, returning the interpreter so its diagnostics
/// can be read.
fn diagnostics_of(source: &str) -> Interpreter {
    let parsed = parse(source);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp
}

#[test]
fn a_radius_of_zero_is_refused_rather_than_painted_flat() {
    // `radius` is a fraction of the element's half-extent, so `0` is not a
    // small glow, it is no glow: every fragment reads the last stop and the
    // author sees a flat wash they will read as "gradients are broken".
    let interp = diagnostics_of(&card(
        "kind: radial, radius: 0, from: 0xFF0E3D2F, to: 0xFF000000",
    ));
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| e.kind() == "AttributeTypeMismatch"),
        "expected a diagnostic, got {:?}",
        interp.errors()
    );
}

#[test]
fn an_omitted_radius_is_a_documented_default_and_not_an_error() {
    // Half the element's larger half-extent: the glow that fills the box it is
    // in, which is what a `radial` with nothing else said should look like.
    let (interp, frame) = frame_of(&card("kind: radial, from: 0xFF0E3D2F, to: 0xFF000000"));
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(
        frame.decorated()[0].gradient.unwrap().radius.to_bits(),
        0.5_f32.to_bits()
    );
}

#[test]
fn an_unknown_kind_names_the_ones_that_exist() {
    let interp = diagnostics_of(&card(
        "kind: radiel, radius: 0.5, from: 0xFF000000, to: 0xFFFFFFFF",
    ));
    let hinted = interp.errors().iter().any(|e| {
        matches!(
            e,
            byard_compiler::diagnostics::CompileError::UnknownAttribute { hint: Some(h), .. }
                if h == "radial"
        )
    });
    assert!(hinted, "expected a hint, got {:?}", interp.errors());
}

#[test]
fn a_box_with_no_gradient_encodes_the_absent_tag() {
    // Presence used to be read off `grad_axis.xy`. A radial centred on the
    // box's top-left corner is `(0, 0)`, which is why it is a tag now: this
    // asserts the tag says "none" rather than the geometry implying it.
    let (_, frame) = frame_of(
        "View Main() { Box #[bg: 0x101010, radius: 12, width: 200, height: 100, \
         opacity: 0.5] {} }",
    );
    let instance =
        byard_core::encoder::decorated_box::DecoratedInstance::from(&frame.decorated()[0]);
    assert_eq!(instance.grad_kind, GRADIENT_NONE);
    assert_eq!(
        instance.grad_axis.map(f32::to_bits),
        [0.0_f32; 4].map(f32::to_bits)
    );
}

#[test]
fn the_kind_reaches_the_instance_lane() {
    let (_, frame) = frame_of(&card(
        "kind: conic, center: (0.5, 0.5), start: 0deg, from: 0xFF000000, to: 0xFFFFFFFF",
    ));
    let instance =
        byard_core::encoder::decorated_box::DecoratedInstance::from(&frame.decorated()[0]);
    assert_eq!(instance.grad_kind, GradientKind::Conic as u32);
    // …and it is not in `misc`, which belongs to opacity, depth, spread and
    // RFC-0031's corner smoothing (INV-28).
    assert_eq!(
        instance.misc[3].to_bits(),
        0.0_f32.to_bits(),
        "`misc.w` is `smooth` and nothing else"
    );
}

// ── RFC-0035 §"Canvas arc strokes": the ramp on a Tier-1 stroke ────────────

/// A canvas carrying one shape command.
fn canvas(cmd: &str) -> String {
    format!("View Main() {{ Canvas #[width: 200, height: 200] {{ {cmd} }} }}")
}

/// The descriptor read off the frame is the descriptor written in the source.
///
/// Deterministic, and first: a readback can say a ring is not flat, but only
/// this can say the ring is carrying a *conic* rather than a linear ramp that
/// happens to look wrong in the same direction.
#[test]
fn a_stroke_gradient_reaches_the_shape_as_the_kind_it_was_written_as() {
    let (interp, frame) = frame_of(&canvas(
        "arc(cx: 100, cy: 100, r: 70, start: -90, sweep: 270, stroke_width: 14, \
         stroke_gradient: (kind: conic, start: -90deg, from: 0xFFEF4444, \
         mid: 0xFFF59E0B, to: 0xFF34D399))",
    ));
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let s = &frame.canvas_shapes()[0];
    let g = s
        .stroke_gradient
        .expect("the arc carries a stroke gradient");
    assert_eq!(g.kind, GradientKind::Conic);
    // The `from` stop, converted to linear space by the same parser a box
    // gradient goes through: `0xFFEF4444` opaque red.
    assert!((g.from[0] - 0.863_157_3).abs() < 1e-5, "{:?}", g.from);
    assert!((g.from[3] - 1.0).abs() < 1e-6, "{:?}", g.from);
    // The conic's start angle lands in the lane the shader reads it from,
    // normalised into `[0, TAU)` by the same parser a box gradient uses, so
    // `-90deg` and `270deg` are one sweep with one set of bytes.
    assert!(
        (g.axis()[2] - 3.0 * std::f32::consts::FRAC_PI_2).abs() < 1e-5,
        "{:?}",
        g.axis()
    );
}

/// A shape that says nothing carries no gradient, so the flat-stroke path is
/// still the one every shape written before this takes.
#[test]
fn a_shape_with_no_stroke_gradient_carries_none() {
    let (_, frame) = frame_of(&canvas(
        "circle(cx: 100, cy: 100, r: 60, stroke: 0xFFFFFFFF, stroke_width: 4)",
    ));
    assert!(frame.canvas_shapes()[0].stroke_gradient.is_none());
}

/// A stroke gradient is a stroke. Dropping the shape because the flat colour
/// it never set is transparent would be the "nothing to paint" check firing on
/// the one case it should not.
#[test]
fn a_stroke_gradient_alone_is_enough_to_paint_a_shape() {
    let (_, frame) = frame_of(&canvas(
        "arc(cx: 100, cy: 100, r: 70, sweep: 270, stroke_width: 10, \
         stroke_gradient: (kind: conic, from: 0xFFEF4444, to: 0xFF34D399))",
    ));
    assert_eq!(
        frame.canvas_shapes().len(),
        1,
        "a shape with a gradient and no `stroke:` must still be emitted"
    );
}

/// The gradient is part of what decides the shape's pixels, so a shape whose
/// gradient moved is a shape that must be repainted (INV-26).
#[test]
fn a_moved_stroke_gradient_is_not_judged_clean() {
    use byard_core::frame::PaintDigest;
    let mut digest = PaintDigest::new();
    let render = |digest: &mut PaintDigest, to: &str| {
        let src = canvas(&format!(
            "arc(cx: 100, cy: 100, r: 70, sweep: 270, stroke_width: 10, \
             stroke_gradient: (kind: conic, from: 0xFFEF4444, to: {to}))"
        ));
        let parsed = parse(&src);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree: Vec<RenderNode> = interp.lower_view(&parsed.views[0], &known);
        interp.tick();
        let mut frame = RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        digest.apply(&mut frame);
        frame.canvas_shapes()[0].dirty
    };
    let _first = render(&mut digest, "0xFF34D399");
    assert!(
        !render(&mut digest, "0xFF34D399"),
        "an unchanged shape must be clean, or the assertion below proves nothing"
    );
    assert!(
        render(&mut digest, "0xFF3B82F6"),
        "a shape whose gradient changed colour must be repainted"
    );
}

/// A misspelt kind is a diagnostic, on a canvas stroke as much as on a box.
/// One parser means one message.
#[test]
fn a_misspelt_kind_on_a_stroke_gradient_is_a_diagnostic() {
    let (interp, _) = frame_of(&canvas(
        "arc(cx: 100, cy: 100, r: 70, sweep: 270, stroke_width: 10, \
         stroke_gradient: (kind: conics, from: 0xFFEF4444, to: 0xFF34D399))",
    ));
    let errs = format!("{:?}", interp.errors());
    assert!(errs.contains("conic"), "{errs}");
}
