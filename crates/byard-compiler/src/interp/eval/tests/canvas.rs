//! Canvas shapes, paths, groups and the morph.

use super::*;

// ── Canvas & shape commands (RFC-0020) ──────────────────────────

#[test]
fn canvas_lowers_to_a_canvas_node_carrying_its_shapes() {
    let (interp, tree) = lower_named(
        "View App() { Canvas #[width: 48, height: 48] { \
               arc(cx: 24, cy: 24, r: 20, start: -90, sweep: 270, \
                   stroke: 0x6750A4, stroke_width: 4, cap: round) \
               circle(cx: 24, cy: 24, r: 8, fill: 0xE8DEF8) } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let RenderNode::Canvas { shapes, .. } = &tree[0] else {
        panic!("Canvas lowers to RenderNode::Canvas, got {:?}", tree[0]);
    };
    assert_eq!(shapes.len(), 2);
    let shape_name = |i: usize| match &shapes[i] {
        CanvasItem::Shape(el) => el.name.as_str().to_string(),
        other => panic!("expected a shape command, got {other:?}"),
    };
    assert_eq!(shape_name(0), "arc");
    assert_eq!(shape_name(1), "circle");
}

/// RFC-0020 §1 as amended: a canvas's *shape count* comes from data.
///
/// Without this, the one thing a drawing surface is for, a chart, is
/// inexpressible: you write twenty-four `rect(…)` lines against twenty-four
/// separately named fields, which is a workaround for a missing feature
/// rather than a use of the language.
#[test]
fn a_for_inside_a_canvas_emits_one_shape_per_item() {
    let (mut interp, tree) = lower_named(
        "View App() {                let bars = [{ x: 0.0, h: 4.0 }, { x: 6.0, h: 12.0 }, { x: 12.0, h: 8.0 }]                Canvas #[width: 100, height: 20] {                  for b in bars { rect(x: b.x, y: 0, w: 4, h: b.h, fill: 0xD0BCFF) } } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let shapes = frame.canvas_shapes();
    assert_eq!(shapes.len(), 3, "one rect per item");
    // Each shape reads *its own* item, in order, the failure mode of a
    // binding that is pushed but never popped is three identical bars.
    let heights: Vec<f32> = shapes.iter().map(|s| s.params[3]).collect();
    assert!(
        (heights[0] - 4.0).abs() < 0.01
            && (heights[1] - 12.0).abs() < 0.01
            && (heights[2] - 8.0).abs() < 0.01,
        "each iteration must see its own binding, got {heights:?}"
    );
    // And the loop leaves nothing behind: `b` must not still be in scope.
    let mut second = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut second, 400.0, 300.0);
    assert_eq!(
        second.canvas_shapes().len(),
        3,
        "a second render must not accumulate bindings or shapes"
    );
}

#[test]
fn a_for_inside_a_canvas_binds_the_index_when_the_two_variable_form_is_used() {
    let (mut interp, tree) = lower_named(
        "View App() {                let bars = [3.0, 6.0, 9.0]                Canvas #[width: 100, height: 20] {                  for i, h in bars { rect(x: i * 10, y: 0, w: 4, h: h, fill: 0xD0BCFF) } } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let xs: Vec<f32> = frame.canvas_shapes().iter().map(|s| s.params[0]).collect();
    assert_eq!(xs.len(), 3);
    assert!(
        (xs[1] - xs[0] - 10.0).abs() < 0.01 && (xs[2] - xs[1] - 10.0).abs() < 0.01,
        "the index must advance per iteration, got {xs:?}"
    );
}

#[test]
fn a_when_inside_a_canvas_takes_one_branch() {
    let (mut interp, tree) = lower_named(
        "View App() { let on = false                Canvas #[width: 100, height: 100] {                  when on { circle(cx: 5, cy: 5, r: 2, fill: 0xFF0000) }                  else { line(x1: 0, y1: 0, x2: 9, y2: 9, stroke: 0x00FF00) } } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let shapes = frame.canvas_shapes();
    assert_eq!(shapes.len(), 1, "exactly one branch, never both");
    assert_eq!(shapes[0].kind, byard_core::frame::CANVAS_SHAPE_LINE);
}

#[test]
fn an_empty_list_in_a_canvas_for_emits_nothing_rather_than_failing() {
    // The first frame of any data-driven chart, before the data arrives.
    let (mut interp, tree) = lower_named(
        "View App() { let bars = []                Canvas #[width: 100, height: 20] {                  for b in bars { rect(x: 0, y: 0, w: 4, h: 4, fill: 0xD0BCFF) } } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        frame.canvas_shapes().is_empty(),
        "{:?}",
        frame.canvas_shapes()
    );
}

#[test]
fn canvas_shapes_render_into_the_canvas_pool_with_evaluated_params() {
    // The sweep is an expression over a view binding, proving shape
    // params run through the ordinary evaluator, so they are reactive.
    let (mut interp, tree) = lower_named(
        "View App() { let p = 0.5 \
               Canvas #[width: 100, height: 100] { \
                 arc(cx: 50, cy: 50, r: 40, start: 0, sweep: p * 360, \
                     stroke: 0xFF0000, stroke_width: 4) \
                 line(x1: 0, y1: 0, x2: 100, y2: 100, stroke: 0x00FF00) \
                 rect(x: 10, y: 10, w: 30, h: 20, radius: 4, fill: 0x0000FF) \
                 text(\"hi\", x: 50, y: 50, align: center, color: 0xFFFFFF) } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let shapes = frame.canvas_shapes();
    assert_eq!(shapes.len(), 3, "arc + line + rect on the CanvasShape pool");
    // `p * 360` with p = 0.5 → a 180° sweep, stored in radians.
    let arc = &shapes[0];
    assert_eq!(arc.kind, byard_core::frame::CANVAS_SHAPE_ARC);
    assert!(
        (arc.params[4] - std::f32::consts::PI).abs() < 1e-3,
        "sweep 180° → π rad, got {}",
        arc.params[4]
    );
    assert!(
        (arc.stroke_color[0] - 1.0).abs() < 1e-6 && arc.stroke_color[3] > 0.99,
        "stroke: 0xFF0000 is opaque red"
    );
    // Shape coordinates are canvas-local + canvas origin (canvas at 0,0
    // in this single-child layout).
    assert!((arc.params[0] - 50.0).abs() < 0.5);
    // The `text(…)` command lowers to an ordinary TextLine, centred
    // around x=50 (its left edge sits before the anchor).
    assert_eq!(frame.texts().len(), 1);
    assert!(frame.texts()[0].x < 50.0);
    // Depths are parallel and strictly ordered (later = nearer).
    let d = frame.canvas_depths();
    assert_eq!(d.len(), 3);
    assert!(d[0] > d[1] && d[1] > d[2]);
}

/// RFC-0031 §S4/§S10: a `Canvas` with `morph:` lowers to **one** instance
/// and N member records, not N instances.
#[test]
fn a_morph_canvas_lowers_to_one_head_and_its_members() {
    let (mut interp, tree) = lower_named(
        "View App() { var phase = 1.5 \
               Canvas #[width: 100, height: 100, morph: phase] { \
                 ngon(cx: 50, cy: 50, r: 40, n: 4, corner: 8, fill: 0x6750A4) \
                 ngon(cx: 50, cy: 50, r: 40, n: 6, inner: 0.85, fill: 0x6750A4) \
                 ngon(cx: 50, cy: 50, r: 40, n: 7, inner: 0.75, fill: 0x6750A4) } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let shapes = frame.canvas_shapes();
    assert_eq!(shapes.len(), 1, "a group is one draw, one instance");
    let head = &shapes[0];
    assert_eq!(head.group_mode, byard_core::frame::GROUP_MORPH);
    assert_eq!(head.group_count, 3);
    assert!(
        (head.group_param - 1.5).abs() < 1e-6,
        "the phase reaches the head, got {}",
        head.group_param
    );
    assert_ne!(
        head.member_hash, 0,
        "digest completeness: the members are hashed in"
    );
    assert_eq!(frame.shape_records().len(), 3);

    // §S4: the head's quad is the union of its members' bounds, not its own
    // geometry, every member must fit inside it.
    let quad = head.bounds();
    for rec in frame.shape_records() {
        let (cx, cy, r) = (rec.params0[0], rec.params0[1], rec.params0[2]);
        assert!(
            cx - r >= quad.x - 0.01 && cx + r <= quad.x + quad.width + 0.01,
            "a member reaches outside the head's quad horizontally"
        );
        assert!(
            cy - r >= quad.y - 0.01 && cy + r <= quad.y + quad.height + 0.01,
            "a member reaches outside the head's quad vertically"
        );
    }

    // The vertex counts survived as integers, in declaration order.
    let counts: Vec<f32> = frame.shape_records().iter().map(|r| r.params1[2]).collect();
    assert_eq!(
        counts.iter().map(|n| *n as i32).collect::<Vec<_>>(),
        [4, 6, 7]
    );
}

/// The same body without `morph:` is unchanged: three ordinary instances
/// and no records. RFC-0031 costs nothing to a canvas that does not use it.
#[test]
fn a_canvas_without_a_combine_mode_is_untouched() {
    let (mut interp, tree) = lower_named(
        "View App() { Canvas #[width: 100, height: 100] { \
                 ngon(cx: 50, cy: 50, r: 40, n: 4, fill: 0x6750A4) \
                 ngon(cx: 50, cy: 50, r: 40, n: 6, fill: 0x6750A4) \
                 ngon(cx: 50, cy: 50, r: 40, n: 7, fill: 0x6750A4) } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.canvas_shapes().len(), 3);
    assert!(
        frame.shape_records().is_empty(),
        "{:?}",
        frame.shape_records()
    );
    assert!(
        frame
            .canvas_shapes()
            .iter()
            .all(|s| s.group_mode == byard_core::frame::GROUP_NONE)
    );
}

/// RFC-0031 §S4: a fusion group's quad is inflated by `k`, because fusion
/// bulges *outward* by up to the smoothing radius.
///
/// An under-inflated quad clips exactly the bridge the feature exists to
/// draw, and it does so only when the shapes are close enough to fuse,
/// i.e. only in the screenshot the author took to show it off. A morph
/// never leaves its members' union and must not pay for this.
#[test]
fn a_fusion_groups_quad_is_inflated_by_its_smoothing_radius() {
    let body = "circle(cx: 30, cy: 30, r: 20, fill: 0x6750A4) \
                    circle(cx: 70, cy: 30, r: 20, fill: 0xEF5350)";
    let quad_of = |attrs: &str| {
        let (mut interp, tree) = lower_named(
            &format!("View App() {{ Canvas #[width: 100, height: 60, {attrs}] {{ {body} }} }}"),
            "App",
        );
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let head = frame.canvas_shapes()[0].clone();
        assert_eq!(head.group_count, 2);
        head.bounds()
    };

    let plain = quad_of("morph: 0.0");
    let fused = quad_of("fuse: 16");
    assert!(
        fused.width >= plain.width + 31.0 && fused.height >= plain.height + 31.0,
        "a `fuse: 16` quad must grow by 16 on every side: {plain:?} → {fused:?}"
    );
    assert!(
        fused.x <= plain.x - 15.9 && fused.y <= plain.y - 15.9,
        "…on the near sides too: {plain:?} → {fused:?}"
    );
    // `fuse: 0` bulges by nothing, so it must not grow the quad either,
    // parity for the degenerate case.
    let zero = quad_of("fuse: 0");
    assert!(
        (zero.width - plain.width).abs() < 0.01,
        "fuse: 0 must not inflate: {plain:?} → {zero:?}"
    );
}

#[test]
fn a_fuse_canvas_lowers_to_a_fusion_group() {
    let (mut interp, tree) = lower_named(
        "View App() { var k = 12.0 \
               Canvas #[width: 140, height: 60, fuse: k] { \
                 circle(cx: 30, cy: 30, r: 18, fill: 0x6750A4) \
                 circle(cx: 70, cy: 30, r: 14, fill: 0xEF5350) \
                 circle(cx: 110, cy: 30, r: 10, fill: 0x4CC38A) } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let head = &frame.canvas_shapes()[0];
    assert_eq!(frame.canvas_shapes().len(), 1, "a group is one draw");
    assert_eq!(head.group_mode, byard_core::frame::GROUP_FUSE);
    assert_eq!(head.group_count, 3);
    assert!((head.group_param - 12.0).abs() < 1e-6, "k reaches the head");
    assert_eq!(frame.shape_records().len(), 3);
    // Each member keeps its own colour: fusion blends them per fragment
    // rather than flattening them at lowering.
    let reds: Vec<f32> = frame
        .shape_records()
        .iter()
        .map(|r| r.fill_color[0])
        .collect();
    assert!(
        reds.windows(2).any(|w| (w[0] - w[1]).abs() > 0.1),
        "the members' colours must survive individually, got {reds:?}"
    );
}

/// **Digest completeness, end to end.** A fusion group whose *member* moves while its
/// head does not must repaint.
///
/// This is the case the invariant exists for and the one an example would
/// never catch: the head's own bytes, mode, `k`, colours, quad, are
/// identical frame to frame, so without the member hash folded into the
/// digest the group is judged clean and paints last frame's shape forever.
/// `morph` escapes it by accident, because its parameter is the phase.
/// `fuse` with a static `k` does not.
#[test]
fn an_animated_member_repaints_its_fusion_group() {
    // The moving circle stays *inside* the union the two fixed ones already
    // span, so the head's quad, and therefore every byte of the head, is
    // identical frame to frame. That is what makes this test about the
    // member hash and nothing else.
    let (mut interp, tree) = lower_named(
        "View App() { \
               Canvas #[width: 340, height: 120, fuse: 22] { \
                 circle(cx: 60, cy: 60, r: 30, fill: 0x5B8DEF) \
                 circle(cx: 280, cy: 60, r: 30, fill: 0x5B8DEF) \
                 circle(cx: 220 with anim.linear(1800ms, from: 120, repeat: infinite), \
                        cy: 60, r: 18, fill: 0xE8734A) } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    let mut digest = byard_core::frame::PaintDigest::new();
    let mut previous_head: Option<[u32; 8]> = None;
    let mut previous_member = 0.0_f32;

    for step in 0..6_u32 {
        interp.set_now_ms(step * 100);
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        digest.apply(&mut frame);

        let head = &frame.canvas_shapes()[0];
        let member = frame.shape_records()[2].params0[0];
        let bytes = head.params.map(f32::to_bits);
        if let Some(before) = previous_head {
            assert!(
                (member - previous_member).abs() > 0.5,
                "frame {step}: the member must actually be moving"
            );
            assert_eq!(
                bytes, before,
                "frame {step}: the head must be byte-identical, or this \
                     test proves nothing about its members"
            );
            assert!(
                head.dirty,
                "frame {step}: an unchanged head with a moved member must \
                     still repaint (digest completeness)"
            );
        }
        previous_head = Some(bytes);
        previous_member = member;
    }
}

/// The same rule, one pool over: a wrapping line's width is not in the line.
///
/// A paragraph pinned to the top-left of a window that is resized keeps its
/// string, its origin and its colour, and breaks in different places. If the
/// wrap width is left out of the comparison the line is reported clean, and
/// `dirty` is what builds the incremental redraw region, so the correctly
/// shaped new text is clipped out of the rectangle that gets repainted.
#[test]
fn a_resize_repaints_a_paragraph_that_only_changed_where_it_breaks() {
    let (mut interp, tree) = lower_named(
        "View App() { Column #[p: 24] { \
             Text(\"a paragraph long enough to wrap inside its column, pinned to the \
                   top left so a resize moves nothing about it except the width it \
                   wraps at\") #[color: 0xFFFFFF, size: 14] } }",
        "App",
    );
    let mut digest = byard_core::frame::PaintDigest::new();
    let mut render = |interp: &mut Interpreter, width: f32| {
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, width, 600.0);
        digest.apply(&mut frame);
        let line = frame.texts()[0].clone();
        let wrap = frame.text_wraps()[0];
        (line, wrap)
    };
    let (before, wrap_before) = render(&mut interp, 800.0);
    let (after, wrap_after) = render(&mut interp, 900.0);
    assert_ne!(wrap_before, wrap_after, "the resize must change the wrap");
    assert_eq!(before.text, after.text, "and nothing else about the line");
    assert!((before.x - after.x).abs() < f32::EPSILON);
    assert!((before.y - after.y).abs() < f32::EPSILON);
    assert!(
        after.dirty,
        "a line that breaks in new places has to be repainted"
    );
}

/// A `for` inside a grouped canvas can generate more members than the cap
/// allows, and how many is only knowable at render time. The static check
/// cannot see it; this one can, and it diagnoses rather than truncating in
/// silence.
#[test]
fn a_for_that_overruns_the_group_cap_is_diagnosed_at_render_time() {
    let (mut interp, tree) = lower_named(
        "View App() { var xs = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9] \
               Canvas #[width: 200, height: 100, morph: 0.0] { \
                 for x in xs { ngon(cx: 50, cy: 50, r: 40, n: 5, fill: 0x6750A4) } } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::TooManyGroupMembers { found: 10, .. })),
        "expected a cap diagnostic, got {:?}",
        interp.errors()
    );
    assert_eq!(
        frame.shape_records().len(),
        byard_core::frame::MAX_GROUP_MEMBERS,
        "and the group is bounded rather than overflowing the shader's loop"
    );

    // Rendering again must not re-report it: a diagnostic that repeats
    // sixty times a second is noise, not information.
    let before = interp.errors().len();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    assert_eq!(interp.errors().len(), before);
}

#[test]
fn full_sweep_arcs_collapse_to_the_cheaper_circle_kind() {
    let (mut interp, tree) = lower_named(
        "View App() { Canvas #[width: 48, height: 48] { \
               arc(cx: 24, cy: 24, r: 20, start: 0, sweep: 360, stroke: 0xFFFFFF) } }",
        "App",
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(
        frame.canvas_shapes()[0].kind,
        byard_core::frame::CANVAS_SHAPE_CIRCLE
    );
}

#[test]
fn bezier_flattens_to_a_contiguous_round_capped_polyline() {
    let (mut interp, tree) = lower_named(
        "View App() { Canvas #[width: 100, height: 100] { \
               bezier(x1: 0, y1: 80, cx1: 30, cy1: 0, cx2: 70, cy2: 0, x2: 100, y2: 80, \
                      stroke: 0xFFFFFF, stroke_width: 2) } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let shapes = frame.canvas_shapes();
    assert!(shapes.len() >= 8, "flattening yields several segments");
    for s in shapes {
        assert_eq!(s.kind, byard_core::frame::CANVAS_SHAPE_LINE);
        assert_eq!(s.cap, byard_core::frame::CANVAS_CAP_ROUND);
    }
    // Endpoints chain: each segment starts where the previous ended, and
    // the whole polyline spans the curve's anchors.
    for pair in shapes.windows(2) {
        assert!((pair[0].params[2] - pair[1].params[0]).abs() < 1e-4);
        assert!((pair[0].params[3] - pair[1].params[1]).abs() < 1e-4);
    }
    assert!((shapes[0].params[0] - 0.0).abs() < 0.5);
    assert!((shapes[shapes.len() - 1].params[2] - 100.0).abs() < 0.5);
}

#[test]
fn a_shapeless_paintless_command_emits_nothing() {
    // No stroke and no fill → invisible → skipped entirely.
    let (mut interp, tree) = lower_named(
        "View App() { Canvas #[width: 48, height: 48] { circle(cx: 24, cy: 24, r: 20) } }",
        "App",
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        frame.canvas_shapes().is_empty(),
        "{:?}",
        frame.canvas_shapes()
    );
}

#[test]
fn canvas_validation_errors_surface_through_lowering() {
    let (interp, _tree) = lower_named(
        "View App() { Canvas #[width: 48] { arc(cx: 1, cy: 1) Text(\"no\") } }",
        "App",
    );
    let errs = interp.errors();
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::CanvasMissingSize { .. })),
        "{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::MissingShapeParam { .. })),
        "{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::UnknownShapeCommand { .. })),
        "{errs:?}"
    );
}

#[test]
fn user_view_call_is_recognized_and_no_unknown_view_fires() {
    // `App` calls `Card` (a user view); no `UnknownView` diagnostic fires.
    let (interp, _tree) = lower_named("View Card() { Text(\"hi\") }\nView App() { Card() }", "App");
    assert!(
        !interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownView { .. })),
        "no UnknownView expected: {:?}",
        interp.errors()
    );
}

#[test]
fn intrinsic_named_view_reports_shadowed_at_load() {
    let parsed = parse("View Row() { Text(\"x\") }");
    let mut interp = Interpreter::new();
    let diags = interp.load_views(&parsed.views);
    assert!(
        diags
            .iter()
            .any(|d| matches!(d, CompileError::IntrinsicShadowed { .. })),
        "expected IntrinsicShadowed, got {diags:?}"
    );
}
