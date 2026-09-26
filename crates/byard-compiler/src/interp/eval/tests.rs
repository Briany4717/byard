use super::*;
use crate::parser::ast::ElementNode;
use crate::parser::parse;

fn element(m: &Member) -> &ElementNode {
    match m {
        Member::Element(e) => e,
        _ => panic!("expected element"),
    }
}

// ── user-view registry & call-site recognition ──────────────────

/// Loads a multi-view file and lowers the named view to a render tree.
/// The linear value of an sRGB byte, for tests that identify a primitive by the
/// colour it was written with.
///
/// A colour literal is written encoded (`0x5B8DEF` is what the design tool
/// says) and reaches the frame decoded, because everything past the compiler
/// blends in linear light. A test comparing against `byte / 255` is comparing
/// the two spaces, which is exactly the mistake the double-encoding hid.
fn linear(byte: u8) -> f32 {
    crate::interp::intrinsics::srgb_to_linear(f32::from(byte) / 255.0)
}

fn lower_named(src: &str, name: &str) -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let view = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == name)
        .unwrap();
    let tree = interp.lower_view(view, &known);
    (interp, tree)
}

#[test]
fn vector_icon_lowers_to_a_vector_node() {
    let (_interp, tree) = lower_named(
        "View App() { VectorIcon(\"icons/gear.svg\") #[size: 24, color: 0xFFFFFF] }",
        "App",
    );
    assert!(
        matches!(&tree[0], RenderNode::Vector { .. }),
        "VectorIcon lowers to RenderNode::Vector, got {:?}",
        tree[0]
    );
}

#[test]
fn vector_icon_starts_as_a_placeholder_then_becomes_resident() {
    // Uses the real gear fixture from the M45 generator PR so this proves
    // the JIT dispatch end to end, not just the cache bookkeeping.
    let svg_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/svg/gear.svg");
    let src = format!("View App() {{ VectorIcon(\"{svg_path}\") #[size: 24, color: 0xFFFFFF] }}");
    let (mut interp, tree) = lower_named(&src, "App");

    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let first = frame.vector_instances()[0];
    assert!(
        first.color[3] < f32::EPSILON,
        "first tick must be a zero-opacity placeholder (INV-9), got alpha {}",
        first.color[3]
    );

    // Poll subsequent ticks until the background generation lands.
    let mut resident = None;
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let inst = frame.vector_instances()[0];
        if inst.color[3] > 0.0 {
            resident = Some(inst);
            break;
        }
    }
    let inst = resident.expect("the glyph must become resident within the poll window");
    assert!(
        (inst.color[3] - 1.0).abs() < f32::EPSILON,
        "full opacity once resident"
    );
    assert!(
        (inst.color[0] - 1.0).abs() < f32::EPSILON,
        "color: 0xFFFFFF tints white"
    );
}

// ── Binary arithmetic (`+ - * /`, RFC-0020 enabler) ─────────────

#[test]
fn eval_binary_promotes_and_never_panics() {
    // Int ∘ Int stays Int; division truncates.
    assert_eq!(
        eval_binary(BinOp::Add, Value::Int(2), Value::Int(3)),
        Value::Int(5)
    );
    assert_eq!(
        eval_binary(BinOp::Div, Value::Int(7), Value::Int(2)),
        Value::Int(3)
    );
    // Any Float operand promotes.
    assert_eq!(
        eval_binary(BinOp::Mul, Value::Int(25), Value::Float(3.6)),
        Value::Float(90.0)
    );
    // Division by zero is 0, not a panic or an IEEE infinity.
    assert_eq!(
        eval_binary(BinOp::Div, Value::Int(1), Value::Int(0)),
        Value::Int(0)
    );
    assert_eq!(
        eval_binary(BinOp::Div, Value::Float(1.0), Value::Float(0.0)),
        Value::Float(0.0)
    );
    // `Str + scalar` now concatenates (RFC-0027 §3), coercing the scalar.
    assert_eq!(
        eval_binary(BinOp::Add, Value::Str("a".into()), Value::Int(1)),
        Value::Str("a1".into())
    );
    // A truly incompatible pair (`Str + List`) still degrades to Unit.
    assert_eq!(
        eval_binary(BinOp::Add, Value::Str("a".into()), Value::List(vec![])),
        Value::Unit
    );
}

#[test]
fn arithmetic_expressions_evaluate_through_bindings() {
    // `let` chains with arithmetic reach paint properties: a Box whose
    // width is `base * 2 + 10`.
    let (mut interp, tree) = lower_named(
        "View App() { let base = 45 let w = base * 2 + 10 \
               Box #[width: w, height: 20, bg: 0xFF0000] {} }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let inst = frame.instances()[0];
    assert!(
        (inst.rect[2] - 100.0).abs() < 0.5,
        "width = 45 * 2 + 10 = 100, got {}",
        inst.rect[2]
    );
}

// ── RFC-0027: comparison, logic, strings, collections ───────────

/// Lowers `view`, ticks once, renders, and returns the first `Text`'s
/// resolved string, the simplest way to observe an expression's value.
fn first_text(src: &str, view: &str) -> String {
    let (mut interp, tree) = lower_named(src, view);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame.texts()[0].text.clone()
}

#[test]
fn comparison_operators_yield_bool() {
    assert_eq!(
        eval_compare(BinOp::Lt, &Value::Int(1), &Value::Int(2)),
        Value::Bool(true)
    );
    assert_eq!(
        eval_compare(BinOp::Ge, &Value::Int(2), &Value::Int(2)),
        Value::Bool(true)
    );
    assert_eq!(
        eval_compare(BinOp::Eq, &Value::Int(3), &Value::Int(3)),
        Value::Bool(true)
    );
    assert_eq!(
        eval_compare(BinOp::Ne, &Value::Int(3), &Value::Int(4)),
        Value::Bool(true)
    );
    // Int↔Float promotion.
    assert_eq!(
        eval_compare(BinOp::Lt, &Value::Int(2), &Value::Float(2.5)),
        Value::Bool(true)
    );
    // Str lexicographic ordering.
    assert_eq!(
        eval_compare(BinOp::Lt, &Value::Str("a".into()), &Value::Str("b".into())),
        Value::Bool(true)
    );
    // Structural list equality.
    assert_eq!(
        eval_compare(
            BinOp::Eq,
            &Value::List(vec![Value::Int(1), Value::Int(2)]),
            &Value::List(vec![Value::Int(1), Value::Int(2)])
        ),
        Value::Bool(true)
    );
}

#[test]
fn comparison_and_logic_reach_a_text() {
    assert_eq!(first_text("View V() { Text(\"{1 < 2}\") }", "V"), "true");
    assert_eq!(first_text("View V() { Text(\"{3 == 4}\") }", "V"), "false");
    assert_eq!(
        first_text(
            "View V() { var a = true\n var b = false\n Text(\"{a && b}\") }",
            "V"
        ),
        "false"
    );
    assert_eq!(
        first_text(
            "View V() { var a = true\n var b = false\n Text(\"{a || b}\") }",
            "V"
        ),
        "true"
    );
}

#[test]
fn boolean_negation_works() {
    assert_eq!(
        first_text(
            "View V() { var showList = true\n Text(\"{!showList}\") }",
            "V"
        ),
        "false"
    );
}

#[test]
fn short_circuit_and_returns_false_without_evaluating_rhs() {
    // `false && (1/0 == 0)` must not even matter, LHS false short-circuits.
    assert_eq!(
        first_text("View V() { Text(\"{false && true}\") }", "V"),
        "false"
    );
    assert_eq!(
        first_text("View V() { Text(\"{true || false}\") }", "V"),
        "true"
    );
}

#[test]
fn string_concat_and_scalar_coercion() {
    assert_eq!(
        first_text(
            "View V() { var count = 3\n Text(\"{\\\"n=\\\" + count}\") }",
            "V"
        ),
        "n=3"
    );
    assert_eq!(
        first_text(
            "View V() { var count = 3\n Text(\"{count + \\\"!\\\"}\") }",
            "V"
        ),
        "3!"
    );
    // `Str + List` is not concatenatable → Unit (empty display).
    assert_eq!(
        eval_concat(Value::Str("a".into()), Value::List(vec![])),
        Value::Unit
    );
}

#[test]
fn list_ops_are_pure_and_value_returning() {
    // push returns a grown list; index; contains; len.
    let src = "View V() { var xs = [1, 2, 3]\n let ys = xs.push(4)\n Text(\"{ys.len}\") }";
    assert_eq!(first_text(src, "V"), "4");
    // Index out of range degrades to Unit (empty display), never panics.
    assert_eq!(
        first_text("View V() { var xs = [1, 2]\n Text(\"{xs[99]}\") }", "V"),
        ""
    );
    assert_eq!(
        index_value(&Value::List(vec![Value::Int(7)]), &Value::Int(0)),
        Value::Int(7)
    );
    assert_eq!(
        index_value(&Value::List(vec![Value::Int(7)]), &Value::Int(5)),
        Value::Unit
    );
}

#[test]
fn filter_and_map_over_records() {
    // filter keeps not-done todos; `.len` counts them.
    let src = "View V() { \
            var todos = [{ text: \"a\", done: false }, { text: \"b\", done: true }, { text: \"c\", done: false }]\n \
            let remaining = todos.filter(t => !t.done).len\n \
            Text(\"{remaining}\") }";
    assert_eq!(first_text(src, "V"), "2");
    // map projects a field.
    let src2 = "View V() { \
            var todos = [{ text: \"a\", done: false }]\n \
            let names = todos.map(t => t.text)\n \
            Text(\"{names.len}\") }";
    assert_eq!(first_text(src2, "V"), "1");
}

#[test]
fn record_spread_returns_a_new_record() {
    // `{ ..r, done: true }` overrides `done`, keeps `text`.
    let src = "View V() { \
            var r = { text: \"a\", done: false }\n \
            let r2 = { ..r, done: true }\n \
            Text(\"{r2.done}\") }";
    assert_eq!(first_text(src, "V"), "true");
}

#[test]
fn structural_eq_promotes_and_recurses() {
    assert!(structural_eq(&Value::Int(2), &Value::Float(2.0)));
    assert!(structural_eq(
        &Value::Record(vec![(Symbol::intern("a"), Value::Int(1))]),
        &Value::Record(vec![(Symbol::intern("a"), Value::Int(1))])
    ));
    assert!(!structural_eq(
        &Value::List(vec![Value::Int(1)]),
        &Value::List(vec![Value::Int(2)])
    ));
}

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
    assert!(frame.canvas_shapes().is_empty());
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
    assert_ne!(head.member_hash, 0, "INV-26: the members are hashed in");
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
    assert!(frame.shape_records().is_empty());
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
    // INV-22 for the degenerate case.
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

/// **INV-26, end to end.** A fusion group whose *member* moves while its
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
                     still repaint (INV-26)"
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
    assert!(frame.canvas_shapes().is_empty());
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

// ── argument → parameter binding ────────────────────────────────

/// Parses `callee_src` (a single view) and a call element from `call_src`'s
/// first view body, returning `(callee, call_element)`.
fn callee_and_call(callee_src: &str, call_src: &str) -> (ViewDecl, ElementNode) {
    let callee = parse(callee_src).views.into_iter().next().unwrap();
    let host = parse(call_src).views.into_iter().next().unwrap();
    let Member::Element(call) = host.body.into_iter().next().unwrap() else {
        panic!("expected element")
    };
    (callee, call)
}

#[test]
fn named_positional_and_mixed_binding() {
    let (callee, _) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Text(\"x\") }",
    );
    // Named.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Avatar(url: \"a.png\", size: 40) }",
    );
    let b = interp.bind_args(&callee, &call);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(b.bindings.len(), 2);
    assert_eq!(b.bindings[0].0.as_str(), "url");
    assert_eq!(b.bindings[1].0.as_str(), "size");

    // Positional.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Avatar(\"a.png\", 40) }",
    );
    let b = interp.bind_args(&callee, &call);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(b.bindings.len(), 2);

    // Mixed: positional then named.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call(
        "View Avatar(url, size) { Text(url) }",
        "View H() { Avatar(\"a.png\") #[size: 40] }",
    );
    let b = interp.bind_args(&callee, &call);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(b.bindings.len(), 2);
}

#[test]
fn arity_unknown_duplicate_and_missing_diagnostics() {
    let (callee, _) = callee_and_call("View A(x, y) { Text(x) }", "View H() { Text(\"_\") }");

    // Over-arity: 3 positional for 2 params.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(1, 2, 3) }");
    interp.bind_args(&callee, &call);
    assert!(interp.errors().iter().any(|e| matches!(
        e,
        CompileError::ViewArityMismatch {
            expected: 2,
            found: 3,
            ..
        }
    )));

    // Unknown named param.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(z: 1) }");
    interp.bind_args(&callee, &call);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownParam { .. }))
    );

    // Duplicate: positional + named bind the same param.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(1) #[x: 2] }");
    interp.bind_args(&callee, &call);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::DuplicateParam { .. }))
    );

    // Missing required.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View A(x, y) { Text(x) }", "View H() { A(1) }");
    interp.bind_args(&callee, &call);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::MissingParam { name, .. } if name == "y"))
    );
}

#[test]
fn parent_var_arg_is_a_live_memo_literal_is_constant() {
    // The parent declares `var n = 1`; the call passes `n` to a parameter.
    // The projecting memo tracks the parent signal: writing `n` and ticking
    // changes the memo's value (dirty edge preserved).
    let callee = parse("View Foo(v) { Text(\"{v}\") }")
        .views
        .into_iter()
        .next()
        .unwrap();
    let mut interp = Interpreter::new();
    let init = Expr::IntLit(1, crate::diagnostics::Span::new(0, 1));
    let n = interp.define_var(Symbol::intern("n"), &init);

    let (_, call) = callee_and_call("View Foo(v) { Text(\"{v}\") }", "View H() { Foo(n) }");
    let b = interp.bind_args(&callee, &call);
    let memo = b.bindings[0].1;
    interp.tick();
    assert_eq!(interp.read_memo(memo), Value::Int(1));
    interp.write_var(n, Value::Int(7));
    interp.tick();
    assert_eq!(
        interp.read_memo(memo),
        Value::Int(7),
        "memo tracks the parent var"
    );

    // A literal argument is a constant memo: it never changes.
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View Foo(v) { Text(\"{v}\") }", "View H() { Foo(5) }");
    let b = interp.bind_args(&callee, &call);
    let memo = b.bindings[0].1;
    interp.tick();
    assert_eq!(interp.read_memo(memo), Value::Int(5));
}

// ── body expansion & per-instance scope ─────────────────────────

/// The string value of a `Text` node's content scope, after a tick.
fn text_value(interp: &mut Interpreter, node: &RenderNode) -> String {
    let RenderNode::Text { content, .. } = node else {
        panic!("expected Text node, got {node:?}");
    };
    interp.tick();
    match interp.binding_value(*content) {
        Some(Value::Str(s)) => s,
        other => panic!("expected Str binding, got {other:?}"),
    }
}

#[test]
fn user_view_expands_body_and_binds_a_parameter() {
    // `App` calls `Greet("Ada")`; the call expands to the callee body with
    // `name` bound, projecting "Hi Ada".
    let (mut interp, tree) = lower_named(
        "View Greet(name) { Text(\"Hi {name}\") }\nView App() { Greet(\"Ada\") }",
        "App",
    );
    assert_eq!(tree.len(), 1, "one spliced root");
    assert_eq!(text_value(&mut interp, &tree[0]), "Hi Ada");
}

#[test]
fn user_view_passes_value_to_an_intrinsic() {
    // `Avatar(url, size)` lowers `Image(url) #[width: size]`; the call's
    // arguments flow through to the intrinsic node.
    let (_interp, tree) = lower_named(
        "View Avatar(url, size) { Image(url) #[width: size, height: size] }\n\
             View App() { Avatar(\"ada.png\", 40) }",
        "App",
    );
    assert_eq!(tree.len(), 1);
    assert!(
        matches!(&tree[0], RenderNode::Image { .. }),
        "expected an Image node, got {:?}",
        tree[0]
    );
}

#[test]
fn a_call_yielding_multiple_roots_splices_as_siblings() {
    let (_interp, tree) = lower_named(
        "View Pair() { Text(\"a\")\n Text(\"b\") }\nView App() { Pair() }",
        "App",
    );
    assert_eq!(tree.len(), 2, "both callee roots spliced as siblings");
}

#[test]
fn nested_user_view_calls_expand() {
    // App → Outer → Inner → Text("x").
    let (mut interp, tree) = lower_named(
        "View Inner() { Text(\"x\") }\n\
             View Outer() { Inner() }\n\
             View App() { Outer() }",
        "App",
    );
    assert_eq!(tree.len(), 1);
    assert_eq!(text_value(&mut interp, &tree[0]), "x");
}

#[test]
fn two_instances_keep_independent_local_state() {
    // Two `Counter()` instances each lower their own `var n`; their content
    // scopes are distinct bindings (independent per-instance state).
    let (_interp, tree) = lower_named(
        "View Counter() { var n = 0\n Text(\"{n}\") }\n\
             View App() { Column { Counter()\n Counter() } }",
        "App",
    );
    // App → Column(Box) containing two expanded Counters.
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected a Column box, got {:?}", tree[0]);
    };
    let texts: Vec<&RenderNode> = children
        .iter()
        .filter(|c| matches!(c, RenderNode::Text { .. }))
        .collect();
    assert_eq!(texts.len(), 2, "two independent Counter texts");
    let scopes: Vec<ScopeId> = texts
        .iter()
        .map(|t| match t {
            RenderNode::Text { content, .. } => *content,
            _ => unreachable!(),
        })
        .collect();
    assert_ne!(scopes[0], scopes[1], "each instance has its own binding");
}

#[test]
fn two_level_composition_golden_shape() {
    // UserRow composes Avatar + Text inside a Row; App stacks two UserRows.
    let (_interp, tree) = lower_named(
        "View Avatar(url) { Image(url) }\n\
             View UserRow(name, avatar) { Row { Avatar(avatar)\n Text(name) } }\n\
             View App() { Column { UserRow(\"Ada\", \"ada.png\")\n UserRow(\"Alan\", \"alan.png\") } }",
        "App",
    );
    // App → Column(Box) → [Row(Box)[Image, Text], Row(Box)[Image, Text]].
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected Column");
    };
    assert_eq!(children.len(), 2, "two UserRow instances");
    for row in children {
        let RenderNode::Box { children: rc, .. } = row else {
            panic!("expected Row");
        };
        assert!(matches!(rc[0], RenderNode::Image { .. }));
        assert!(matches!(rc[1], RenderNode::Text { .. }));
    }
}

// ── recursion & cycle protection ────────────────────────────────

#[test]
fn unguarded_self_call_is_recursive_view_at_load() {
    let parsed = parse("View A() { A() }");
    let mut interp = Interpreter::new();
    let diags = interp.load_views(&parsed.views);
    assert!(
        diags
            .iter()
            .any(|d| matches!(d, CompileError::RecursiveView { .. })),
        "expected RecursiveView at load, got {diags:?}"
    );
}

#[test]
fn guarded_recursion_that_terminates_is_legal() {
    // RFC-0018: `Tree` recurses only in the `else` of a guard that is true, so
    // the recursive branch is never lowered (lazy `when`), it renders to a
    // finite depth with no diagnostic.
    let (mut interp, tree) = lower_named(
        "View Tree() { var leaf = true\n when leaf { Text(\"x\") } else { Tree() } }\n\
             View App() { Tree() }",
        "App",
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp.errors().is_empty(),
        "guarded terminating recursion is legal: {:?}",
        interp.errors()
    );
    assert_eq!(frame.texts().len(), 1);
    assert_eq!(frame.texts()[0].text, "x");
}

#[test]
fn runaway_guarded_recursion_hits_depth_bound_without_panicking() {
    // `go` is always true, so the guard never terminates at lower time. The
    // static check does not flag it (the cycle is guarded), so the runtime
    // depth bound must stop it with a diagnostic, not a stack overflow.
    let parsed = parse("View Loop() { var go = true\n when go { Loop() } }\nView App() { Loop() }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let load_diags = interp.load_views(&parsed.views);
    assert!(
        !load_diags
            .iter()
            .any(|d| matches!(d, CompileError::RecursiveView { .. })),
        "a guarded cycle is not a static error"
    );
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let app = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == "App")
        .unwrap();
    let tree = interp.lower_view(app, &known); // lazy: no recursion at lower
    // RFC-0018: the recursion now unrolls at render (reconcile) time, one
    // level per frame (a freshly-lowered `go` reads false until the next
    // pull), so each render is finite (no stack overflow). Over enough frames
    // the reconcile depth bound stops it with a diagnostic.
    let mut hit = false;
    for _ in 0..(MAX_INSTANCE_DEPTH + 8) {
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0); // must not overflow
        if interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::RecursiveView { .. }))
        {
            hit = true;
            break;
        }
    }
    assert!(
        hit,
        "the reconcile depth bound must stop the runaway recursion"
    );
}

// ── hot-reload across instances ─────────────────────────────────

#[test]
fn reloading_a_leaf_view_updates_all_its_instances() {
    use crate::interp::reload::{affected_views, diff_view};

    let old = parse("View Leaf() { Text(\"old\") }\nView App() { Column { Leaf()\n Leaf() } }");
    let new = parse("View Leaf() { Text(\"new\") }\nView App() { Column { Leaf()\n Leaf() } }");

    let mut interp = Interpreter::new();
    interp.load_views(&old.views);
    let known_old: Vec<&str> = old.views.iter().map(|v| v.name.as_str()).collect();
    let app_old = old.views.iter().find(|v| v.name.as_str() == "App").unwrap();
    let tree = interp.lower_view(app_old, &known_old);
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected Column");
    };
    assert_eq!(text_value(&mut interp, &children[0]), "old");

    // The edit to the leaf transitively affects App (RFC-0007 §5).
    let affected = affected_views(&old.views, &new.views);
    assert!(affected.contains(&Symbol::intern("App")));

    // Rebuild the registry and re-derive App; both Leaf instances update.
    interp.load_views(&new.views);
    let app_new = new.views.iter().find(|v| v.name.as_str() == "App").unwrap();
    interp.reload(app_new, diff_view(app_old, app_new));
    let known_new: Vec<&str> = new.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(app_new, &known_new);
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected Column");
    };
    assert_eq!(text_value(&mut interp, &children[0]), "new");
    assert_eq!(text_value(&mut interp, &children[1]), "new");
}

// ── slots & parameter defaults ──────────────────────────────────

#[test]
fn omitted_defaulted_param_uses_its_default() {
    // `label` is omitted; the default "?" is evaluated in the callee scope.
    let (mut interp, tree) = lower_named(
        "View Tag(label = \"?\") { Text(label) }\nView App() { Tag() }",
        "App",
    );
    assert!(
        interp.errors().is_empty(),
        "a defaulted param is not required: {:?}",
        interp.errors()
    );
    assert_eq!(text_value(&mut interp, &tree[0]), "?");
}

#[test]
fn supplied_argument_overrides_the_default() {
    let (mut interp, tree) = lower_named(
        "View Tag(label = \"?\") { Text(label) }\nView App() { Tag(\"hi\") }",
        "App",
    );
    assert_eq!(text_value(&mut interp, &tree[0]), "hi");
}

#[test]
fn missing_param_only_fires_for_required_params() {
    // `a` is required, `b` is defaulted; omitting both reports only `a`.
    let (callee, _) = callee_and_call("View V(a, b = 1) { Text(a) }", "View H() { Text(\"_\") }");
    let mut interp = Interpreter::new();
    let (_, call) = callee_and_call("View V(a, b = 1) { Text(a) }", "View H() { V() }");
    interp.bind_args(&callee, &call);
    let missing: Vec<&str> = interp
        .errors()
        .iter()
        .filter_map(|e| match e {
            CompileError::MissingParam { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(missing, vec!["a"], "only the required param is missing");
}

#[test]
fn content_slot_renders_the_passed_block() {
    // `Card` declares a `content` slot; `App` passes a `Text` block, which is
    // spliced where `content` appears inside the card body.
    let (mut interp, tree) = lower_named(
        "View Card(content) { Column { content } }\n\
             View App() { Card { Text(\"inside\") } }",
        "App",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let RenderNode::Box { children, .. } = &tree[0] else {
        panic!("expected the card's Column, got {:?}", tree[0]);
    };
    assert_eq!(children.len(), 1, "the passed block is spliced");
    assert_eq!(text_value(&mut interp, &children[0]), "inside");
}

#[test]
fn block_passed_to_a_slotless_view_is_unexpected_children() {
    let (interp, _tree) = lower_named(
        "View Plain() { Text(\"x\") }\nView App() { Plain { Text(\"no\") } }",
        "App",
    );
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnexpectedChildren { .. })),
        "expected UnexpectedChildren, got {:?}",
        interp.errors()
    );
}

#[test]
fn slot_block_captures_the_caller_scope() {
    // The block passed to `Card` reads the *caller's* `name` var, proving the
    // slot is lowered in the caller scope (not the callee's).
    let (mut interp, tree) = lower_named(
        "View Card(content) { content }\n\
             View App() { var name = \"Ada\"\n Card { Text(\"Hi {name}\") } }",
        "App",
    );
    assert_eq!(text_value(&mut interp, &tree[0]), "Hi Ada");
}

#[test]
fn var_text_binding_updates_after_mutation_and_tick() {
    let parsed =
        parse("View C() {\n var count = 0\n Text(\"{count}\")\n Button(\"+\") => count++\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];

    let mut interp = Interpreter::new();
    let Member::Var { name, init, .. } = &view.body[0] else {
        panic!("expected var");
    };
    interp.define_var(name.clone(), init);

    let text = element(&view.body[1]);
    let bind = interp.bind_value(&text.content[0].value);
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("0".to_string()))
    );

    // The Button's `=> count++` action.
    let action = element(&view.body[2]).action.as_ref().unwrap();
    interp.eval_action(action).unwrap();
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("1".to_string()))
    );
}

#[test]
fn let_memo_recomputes_when_its_source_changes() {
    let parsed = parse(
        "View C() {\n var count = 0\n let doubled = count\n Text(\"{doubled}\")\n Button(\"+\") => count++\n}",
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();

    let Member::Var { name, init, .. } = &view.body[0] else {
        panic!()
    };
    interp.define_var(name.clone(), init);
    let Member::Let { name, init, .. } = &view.body[1] else {
        panic!()
    };
    let memo = interp.define_let(name.clone(), init);

    let text = element(&view.body[2]);
    let bind = interp.bind_value(&text.content[0].value);
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("0".to_string()))
    );
    let evals = interp.ctx().eval_count(memo);

    let action = element(&view.body[3]).action.as_ref().unwrap();
    interp.eval_action(action).unwrap();
    interp.tick();
    assert_eq!(
        interp.binding_value(bind),
        Some(Value::Str("1".to_string()))
    );
    assert!(interp.ctx().eval_count(memo) > evals, "memo recomputed");
}

#[test]
fn assignment_to_a_let_is_not_assignable() {
    let parsed = parse("View C() {\n var count = 0\n let y = count\n Button(\"x\") => y = 5\n}");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();

    let Member::Var { name, init, .. } = &view.body[0] else {
        panic!()
    };
    interp.define_var(name.clone(), init);
    let Member::Let { name, init, .. } = &view.body[1] else {
        panic!()
    };
    interp.define_let(name.clone(), init);

    let action = element(&view.body[2]).action.as_ref().unwrap();
    let err = interp.eval_action(action).unwrap_err();
    assert!(matches!(err, CompileError::NotAssignable { .. }));
}

#[test]
fn lower_view_emits_expected_render_tree() {
    let parsed = parse(
        "View C() {\n var count = 0\n Column #[bg: 0x222222, radius: 16] {\n Text(\"Count: {count}\")\n }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];

    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    // One top-level Column box with the literal bg/radius and one Text child.
    assert_eq!(tree.len(), 1);
    let RenderNode::Box {
        attrs, children, ..
    } = &tree[0]
    else {
        panic!("expected a Box, got {:?}", tree[0]);
    };
    let bg = interp.eval_color_prop(attrs, "bg");
    let radius = interp.eval_int_prop(attrs, "radius");
    assert_eq!(bg, Some(0x0022_2222));
    assert_eq!(radius, Some(16));
    assert_eq!(children.len(), 1);
    let RenderNode::Text { content, .. } = &children[0] else {
        panic!("expected a Text child");
    };

    // The Text projects the reactive count.
    interp.tick();
    assert_eq!(
        interp.binding_value(*content),
        Some(Value::Str("Count: 0".to_string()))
    );
}

#[test]
fn lowering_an_unknown_element_records_unknown_view() {
    let parsed = parse("View C() { Colunm #[gap: 8] {} }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(view, &[]);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownView { .. }))
    );
}

#[test]
fn mutation_on_an_undeclared_name_is_not_assignable() {
    let parsed = parse("View C() { Button(\"x\") => ghost++ }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let action = element(&view.body[0]).action.as_ref().unwrap();
    assert!(matches!(
        interp.eval_action(action).unwrap_err(),
        CompileError::NotAssignable { .. }
    ));
}

#[test]
fn spacing_convenience_parses_correctly() {
    use byard_core::atlas::layout::Spacing;

    let test_cases = vec![
        // 1-value positional
        ("View C() { Column #[p: (10)] {} }", Spacing::all(10.0)),
        // 2-value positional
        (
            "View C() { Column #[p: (2, 3)] {} }",
            Spacing::symmetric(2.0, 3.0),
        ),
        // 4-value positional: CSS order top, right, bottom, left
        (
            "View C() { Column #[p: (1, 2, 3, 4)] {} }",
            Spacing {
                top: 1.0,
                right: 2.0,
                bottom: 3.0,
                left: 4.0,
            },
        ),
        // Named top only, unspecified sides default to 0
        (
            "View C() { Column #[p: (top: 10)] {} }",
            Spacing {
                top: 10.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
        ),
        // Named bottom only
        (
            "View C() { Column #[p: (bottom: 7)] {} }",
            Spacing {
                top: 0.0,
                right: 0.0,
                bottom: 7.0,
                left: 0.0,
            },
        ),
        // Named mixed sides
        (
            "View C() { Column #[p: (left: 5, bottom: 3)] {} }",
            Spacing {
                top: 0.0,
                right: 0.0,
                bottom: 3.0,
                left: 5.0,
            },
        ),
        // Verbose axis shorthands (the only accepted shorthands)
        (
            "View C() { Column #[p: (horizontal: 10, vertical: 5)] {} }",
            Spacing {
                top: 5.0,
                right: 10.0,
                bottom: 5.0,
                left: 10.0,
            },
        ),
    ];

    for (source, expected_spacing) in test_cases {
        let parsed = parse(source);
        assert!(
            parsed.errors.is_empty(),
            "Failed to parse: {}\nErrors: {:?}",
            source,
            parsed.errors
        );
        let view = &parsed.views[0];
        let mut interp = Interpreter::new();
        let tree = interp.lower_view(view, &[]);
        let RenderNode::Box { name, attrs, .. } = &tree[0] else {
            panic!("expected a Box");
        };
        let style = interp.eval_container_style(name.as_str(), attrs);
        assert_eq!(
            style.padding.top, expected_spacing.top,
            "top mismatch for source: {}",
            source
        );
        assert_eq!(
            style.padding.right, expected_spacing.right,
            "right mismatch for source: {}",
            source
        );
        assert_eq!(
            style.padding.bottom, expected_spacing.bottom,
            "bottom mismatch for source: {}",
            source
        );
        assert_eq!(
            style.padding.left, expected_spacing.left,
            "left mismatch for source: {}",
            source
        );
    }
}

#[test]
fn individual_margin_padding_properties_override() {
    use byard_core::atlas::layout::Spacing;

    let parsed = parse("View C() { Column #[p: (10), pt: 2, pb: 4, ml: 5, mt: 1] {} }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    let RenderNode::Box { name, attrs, .. } = &tree[0] else {
        panic!("expected a Box");
    };
    let style = interp.eval_container_style(name.as_str(), attrs);
    // padding.top overridden to 2, padding.bottom overridden to 4, others stay 10
    assert_eq!(
        style.padding,
        Spacing {
            top: 2.0,
            right: 10.0,
            bottom: 4.0,
            left: 10.0
        }
    );
    // margins
    assert_eq!(
        style.margin,
        Spacing {
            top: 1.0,
            right: 0.0,
            bottom: 0.0,
            left: 5.0
        }
    );
}

// ── M25: `Len` padding/margin forms ──────────────────────────────────

/// Lowers a single-`Box` view and returns the resolved padding plus any
/// errors raised during style resolution.
fn resolve_padding(src: &str) -> (byard_core::atlas::layout::Spacing, Vec<CompileError>) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    let RenderNode::Box { name, attrs, .. } = &tree[0] else {
        panic!("expected a Box");
    };
    let style = interp.eval_container_style(name.as_str(), attrs);
    (style.padding, interp.errors().to_vec())
}

#[test]
fn impl30_scalar_sets_all_sides() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: 5] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(p, Spacing::all(5.0));
}

#[test]
fn impl30_pair_is_vertical_horizontal() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (10, 5)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    // (vertical, horizontal): top=bottom=10, left=right=5.
    assert_eq!(
        p,
        Spacing {
            top: 10.0,
            right: 5.0,
            bottom: 10.0,
            left: 5.0
        }
    );
}

#[test]
fn impl30_quad_is_css_order() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (4, 6, 8, 7)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        p,
        Spacing {
            top: 4.0,
            right: 6.0,
            bottom: 8.0,
            left: 7.0
        }
    );
}

#[test]
fn impl30_named_single_side_defaults_rest_to_zero() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (bottom: 7)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        p,
        Spacing {
            top: 0.0,
            right: 0.0,
            bottom: 7.0,
            left: 0.0
        }
    );
}

#[test]
fn impl30_named_axis_shorthands() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (horizontal: 10, vertical: 5)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        p,
        Spacing {
            top: 5.0,
            right: 10.0,
            bottom: 5.0,
            left: 10.0
        }
    );
}

#[test]
fn impl30_unknown_side_is_unknown_attribute_with_hint() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (tpo: 4)] {} }");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::UnknownAttribute { name, hint: Some(h), .. }
                if name == "tpo" && h == "top"
        )),
        "expected UnknownAttribute(tpo)->top, got {errs:?}"
    );
}

#[test]
fn impl30_axis_plus_component_conflicts() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (horizontal: 10, left: 3)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ConflictingSpacingField { .. })),
        "expected ConflictingSpacingField, got {errs:?}"
    );
}

#[test]
fn impl30_non_int_side_is_type_mismatch() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (top: 4, left: \"x\")] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::AttributeTypeMismatch { .. })),
        "expected AttributeTypeMismatch, got {errs:?}"
    );
}

#[test]
fn impl30_wrong_positional_arity_is_arity_mismatch() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (1, 2, 3)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { .. })),
        "expected ArityMismatch for a 3-tuple, got {errs:?}"
    );
}

#[test]
fn impl30_mixing_named_and_positional_errors() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (10, top: 4)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ConflictingSpacingField { .. })),
        "expected a conflict for mixed named/positional, got {errs:?}"
    );
}

#[test]
fn impl30_px_py_are_now_unknown_attributes() {
    let parsed = parse("View C() { Column #[px: 5] {} }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(view, &[]);
    assert!(
        interp.errors().iter().any(|e| matches!(
            e,
            CompileError::UnknownAttribute { name, .. } if name == "px"
        )),
        "px must now be UnknownAttribute, got {:?}",
        interp.errors()
    );
}

// ── Per-corner `radius` ──────────────────────────────────────────────

/// Lowers a single-element view and returns `resolve_radii`'s result for
/// its `radius` attribute alongside any errors raised.
fn resolve_radius_test(src: &str) -> ([f32; 4], Vec<CompileError>) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let attrs = element(&view.body[0]).attrs.clone();
    let radii = interp.resolve_radii(&attrs, "radius");
    (radii, interp.errors().to_vec())
}

#[test]
fn impl44_radius_scalar_broadcasts_to_all_four_corners() {
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: 16] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(radii, [16.0, 16.0, 16.0, 16.0]);
}

#[test]
fn impl44_radius_quad_sets_independent_corners_in_css_order() {
    // top_left, top_right, bottom_right, bottom_left (frame.rs / WGSL convention).
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: (4, 8, 12, 16)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(radii, [4.0, 8.0, 12.0, 16.0]);
}

#[test]
fn impl44_radius_missing_attribute_defaults_to_zero() {
    let (radii, errs) = resolve_radius_test("View C() { Column {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(radii, [0.0; 4]);
}

#[test]
fn impl44_radius_wrong_arity_is_arity_mismatch() {
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: (4, 8)] {} }");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::ArityMismatch {
                expected: 4,
                found: 2,
                ..
            }
        )),
        "expected ArityMismatch(4, found 2), got {errs:?}"
    );
    assert_eq!(radii, [0.0; 4]);
}

#[test]
fn impl44_radius_named_corner_field_is_rejected() {
    let (radii, errs) = resolve_radius_test(
        "View C() { Column #[radius: (top_left: 4, top_right: 8, bottom_right: 12, bottom_left: 16)] {} }",
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ConflictingSpacingField { .. })),
        "expected ConflictingSpacingField for named corners, got {errs:?}"
    );
    assert_eq!(radii, [0.0; 4]);
}

#[test]
fn impl44_radius_non_numeric_corner_is_type_mismatch() {
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: (4, \"x\", 12, 16)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::AttributeTypeMismatch { .. })),
        "expected AttributeTypeMismatch, got {errs:?}"
    );
    // Valid corners still resolve; the bad one is left at the [0.0;4]
    // default rather than aborting the whole tuple.
    assert_eq!(radii, [4.0, 0.0, 12.0, 16.0]);
}

#[test]
fn impl44_decorated_box_carries_independent_corner_radii_into_box_instance() {
    // End-to-end: a quad `radius` on a Box that also has `bg` (so it's a
    // plain BoxInstance push, not a DecoratedBox) reaches the GPU instance
    // with all four corners intact rather than being collapsed to a scalar.
    let parsed =
        parse("View C() { Box #[bg: 0xFF0000, radius: (4, 8, 12, 16), width: 50, height: 50] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let instances = frame.instances();
    assert_eq!(instances.len(), 1, "expected exactly one BoxInstance");
    assert_eq!(instances[0].radii, [4.0, 8.0, 12.0, 16.0]);
}

// ── RFC-0011: paint-time transform attribute surface ─────────────────

#[test]
fn transform_attrs_reach_the_box_instance() {
    let parsed = parse(
        "View C() { Box #[bg: 0xFF0000, width: 50, height: 50, \
             translate: (5, 10), scale: 1.5, rotate: 90deg] }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let instances = frame.instances();
    assert_eq!(instances.len(), 1);
    let t = instances[0].transform;
    assert_eq!(t.translate, [5.0, 10.0]);
    assert_eq!(t.scale, [1.5, 1.5]);
    assert!((t.rotate - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    // Unset `origin` defaults to the element's own center, not (0,0).
    assert_eq!(t.origin, [25.0, 25.0]);
}

#[test]
fn with_animation_interpolates_toward_the_target_and_settles() {
    // A linear ramp gives deterministic sample points to assert on.
    let parsed = parse(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, width: 10, height: 10, \
             scale: on ? 2.0 : 1.0 with anim.linear(1000)] }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();

    let render_scale = |interp: &mut Interpreter, now: u32| -> f32 {
        interp.set_now_ms(now);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.instances()[0].transform.scale[0]
    };

    // At rest: target is 1.0, nothing is animating.
    assert!((render_scale(&mut interp, 0) - 1.0).abs() < 1e-3);
    assert!(!interp.has_active_animations());

    // Flip the target to 2.0 at t=0, the motion retargets from the current
    // value (~1.0) and is now active.
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    assert!(
        (render_scale(&mut interp, 0) - 1.0).abs() < 1e-2,
        "starts where it was"
    );
    assert!(
        interp.has_active_animations(),
        "a just-retargeted motion is active"
    );

    // Halfway through the 1000 ms ramp → ~1.5.
    assert!((render_scale(&mut interp, 500) - 1.5).abs() < 5e-2);

    // Past the end → arrived at 2.0 and settled (idle again).
    assert!((render_scale(&mut interp, 1000) - 2.0).abs() < 1e-3);
    assert!(
        !interp.has_active_animations(),
        "settles once the ramp completes"
    );
}

/// Drives a `on ? … : …` paint prop through a 1000 ms linear ramp and
/// returns the value `sample` reads from the rendered frame at t = 0 (just
/// after the flip), 500, and 1000 ms, the shared body of the coverage tests
/// below, which each assert a different paint prop interpolates.
fn ramp_paint_prop(src: &str, sample: impl Fn(&byard_core::frame::RenderFrame) -> f32) -> [f32; 3] {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    // Seed at rest, then flip the target on at t = 0 so the motion retargets.
    interp.set_now_ms(0);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    let at = |interp: &mut Interpreter, now: u32| {
        interp.set_now_ms(now);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        sample(&frame)
    };
    [
        at(&mut interp, 0),
        at(&mut interp, 500),
        at(&mut interp, 1000),
    ]
}

#[test]
fn radius_animates_as_a_paint_prop() {
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, width: 40, height: 40, radius: on ? 20 : 4 with anim.linear(1000)] }",
        |f| f.instances()[0].radii[0],
    );
    assert!((a - 4.0).abs() < 0.5, "starts near 4, got {a}");
    assert!((b - 12.0).abs() < 1.5, "~halfway, got {b}");
    assert!((c - 20.0).abs() < 0.5, "arrives at 20, got {c}");
}

#[test]
fn border_width_animates_as_a_paint_prop() {
    let sample = |f: &byard_core::frame::RenderFrame| {
        f.decorated()
            .iter()
            .map(|d| d.border_width)
            .fold(0.0_f32, f32::max)
    };
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, border: 0xFFFFFF, width: 40, height: 40, \
             border_width: on ? 8 : 2 with anim.linear(1000)] }",
        sample,
    );
    assert!((a - 2.0).abs() < 0.5, "starts near 2, got {a}");
    assert!((b - 5.0).abs() < 1.0, "~halfway, got {b}");
    assert!((c - 8.0).abs() < 0.5, "arrives at 8, got {c}");
}

#[test]
fn a_shadow_field_animates() {
    let sample = |f: &byard_core::frame::RenderFrame| {
        f.decorated()
            .iter()
            .map(|d| d.shadow_dy)
            .fold(0.0_f32, f32::max)
    };
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, width: 40, height: 40, \
             shadow: (y: (on ? 12 : 2) with anim.linear(1000), blur: 8, color: 0x80000000)] }",
        sample,
    );
    assert!((a - 2.0).abs() < 0.6, "starts near 2, got {a}");
    assert!((b - 7.0).abs() < 1.5, "~halfway, got {b}");
    assert!((c - 12.0).abs() < 0.6, "arrives at 12, got {c}");
}

// ── RFC-0025: looping & indefinite animations ────────────────────────

/// Renders `src` at each of `times` (ms on the engine clock) and returns
/// what `sample` reads from the frame, plus whether the animation was still
/// in the active set at that instant, the two things every looping test
/// asks about.
fn sample_over_time(
    src: &str,
    times: &[u32],
    sample: impl Fn(&byard_core::frame::RenderFrame) -> f32,
) -> Vec<(f32, bool)> {
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    times
        .iter()
        .map(|ms| {
            interp.set_now_ms(*ms);
            let mut frame = byard_core::frame::RenderFrame::new();
            interp.render(&tree, &mut frame, 400.0, 300.0);
            (sample(&frame), interp.has_active_animations())
        })
        .collect()
}

/// The first emitted box's paint-time `translate.x`.
fn translate_x(frame: &byard_core::frame::RenderFrame) -> f32 {
    frame.instances()[0].transform.translate[0]
}

#[test]
fn an_infinite_repeat_wraps_and_keeps_the_frames_coming() {
    // A 400 ms linear ramp from 0 → 100, repeating forever: it must wrap at
    // the period and never let the app idle (that is what a spinner needs).
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(400ms, from: 0, repeat: infinite)] }";
    let seen = sample_over_time(src, &[0, 200, 400, 600, 4_000], translate_x);
    let x: Vec<f32> = seen.iter().map(|(v, _)| *v).collect();
    assert!((x[0] - 0.0).abs() < 1.0, "starts at `from`, got {}", x[0]);
    assert!((x[1] - 50.0).abs() < 2.0, "halfway, got {}", x[1]);
    assert!((x[2] - 0.0).abs() < 1.0, "wrapped, got {}", x[2]);
    assert!((x[3] - 50.0).abs() < 2.0, "second play, got {}", x[3]);
    assert!(
        seen.iter().all(|(_, active)| *active),
        "an infinite animation never settles"
    );
}

#[test]
fn reverse_oscillates_between_the_two_endpoints() {
    // The RFC's pulsing badge: `from` and the target are the two ends, and
    // alternating plays turn it into a continuous oscillation.
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(400ms, from: 0, \
                   repeat: infinite, reverse: true)] }";
    let x: Vec<f32> = sample_over_time(src, &[0, 200, 400, 600, 800], translate_x)
        .into_iter()
        .map(|(v, _)| v)
        .collect();
    assert!((x[0] - 0.0).abs() < 1.0, "out from `from`, got {}", x[0]);
    assert!((x[1] - 50.0).abs() < 2.0);
    assert!((x[2] - 100.0).abs() < 1.0, "at the far end, got {}", x[2]);
    assert!((x[3] - 50.0).abs() < 2.0, "coming back, got {}", x[3]);
    assert!((x[4] - 0.0).abs() < 1.0, "home again, got {}", x[4]);
}

#[test]
fn a_counted_repeat_stops_and_lets_the_app_idle() {
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(400ms, from: 0, repeat: 2)] }";
    let seen = sample_over_time(src, &[0, 500, 799, 900, 5_000], translate_x);
    assert!(seen[1].1, "still playing the second time");
    assert!((seen[3].0 - 100.0).abs() < 1.0, "holds the final value");
    assert!(
        !seen[3].1 && !seen[4].1,
        "a finite repeat leaves the active set, so the app idles"
    );
}

#[test]
fn a_delay_holds_the_start_value_then_animates() {
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(200ms, from: 0, delay: 300ms)] }";
    let seen = sample_over_time(src, &[0, 200, 400, 600], translate_x);
    assert!((seen[0].0).abs() < 0.01, "held at `from` during the delay");
    assert!((seen[1].0).abs() < 0.01, "still held at 200ms");
    assert!(
        seen[1].1,
        "a pending delay keeps requesting frames, otherwise it would never start"
    );
    assert!((seen[2].0 - 50.0).abs() < 2.0, "halfway, got {}", seen[2].0);
    assert!((seen[3].0 - 100.0).abs() < 0.5, "arrived");
}

#[test]
fn stagger_offsets_each_item_by_its_index() {
    // RFC-0025 §"Stagger": one written animation, a different start per item.
    let src = "View V() { let xs = [1, 2, 3] \
                   Column { for i, x in xs { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with \
                             anim.stagger(linear(200ms, from: 0), 100ms, i)] {} \
                   } } }";
    // Every reading starts from a render at t = 0: an animation's timeline
    // begins when it first appears, exactly as it does in a running app.
    let at = |ms: u32| {
        *sample_over_time(src, &[0, ms], |f| {
            // Each row's translate, in row order.
            f.instances()
                .iter()
                .map(|b| b.transform.translate[0])
                .sum::<f32>()
        })
        .last()
        .map(|(v, _)| v)
        .unwrap()
    };
    // At 100 ms the first row is halfway (50), the second just starting (0),
    // the third still waiting (0).
    assert!((at(100) - 50.0).abs() < 3.0, "only row 0 has moved");
    // At 200 ms: row 0 arrived (100), row 1 halfway (50), row 2 starting (0).
    assert!((at(200) - 150.0).abs() < 4.0, "the wave has advanced");
    // Once every row has played out, they are all at the target.
    assert!((at(1_000) - 300.0).abs() < 0.5, "all rows arrived");
}

#[test]
fn keyframes_walk_their_steps_and_loop() {
    // RFC-0025 §3: a three-step sequence over 400 ms, looping forever.
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: anim.keyframes(0%: 0, 50%: 80, 100%: 0, \
                   duration: 400ms, loop: true)] }";
    let seen = sample_over_time(src, &[0, 100, 200, 300, 400, 500], translate_x);
    let x: Vec<f32> = seen.iter().map(|(v, _)| *v).collect();
    assert!((x[0] - 0.0).abs() < 0.5, "the 0% step");
    assert!(
        (x[1] - 40.0).abs() < 2.0,
        "midway to the 50% step, got {}",
        x[1]
    );
    assert!((x[2] - 80.0).abs() < 0.5, "the 50% step, got {}", x[2]);
    assert!((x[3] - 40.0).abs() < 2.0, "coming back down, got {}", x[3]);
    assert!((x[4] - 0.0).abs() < 0.5, "wrapped to the start");
    assert!((x[5] - 40.0).abs() < 2.0, "second play");
    assert!(
        seen.iter().all(|(_, active)| *active),
        "a loop never settles"
    );
}

#[test]
fn keyframe_pairs_interpolate_component_wise() {
    // A coordinate pair per step (the RFC's indeterminate-progress shape).
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: anim.keyframes(0%: (-100, 10), 100%: (300, 30), duration: 400ms)] }";
    let seen = sample_over_time(src, &[0, 200], |f| {
        let t = f.instances()[0].transform.translate;
        // Encode both axes in one sample: x + y * 1000.
        t[0] + t[1] * 1000.0
    });
    let (x, y) = (seen[1].0 % 1000.0, (seen[1].0 / 1000.0).floor());
    assert!(
        (x - 100.0).abs() < 2.0,
        "x halfway from -100 to 300, got {x}"
    );
    assert!((y - 20.0).abs() < 0.5, "y halfway from 10 to 30, got {y}");
}

#[test]
fn a_keyframe_segment_uses_its_own_easing() {
    // Same two-segment shape, but the first arrives with `ease_out`: at the
    // quarter mark it must be further along than the linear version.
    let linear = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                      translate: anim.keyframes(0%: 0, 50%: 100, 100%: 0, duration: 400ms)] }";
    let eased = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                     translate: anim.keyframes(0%: 0, 50%: 100 ease_out, 100%: 0, \
                     duration: 400ms)] }";
    let at_quarter = |src: &str| sample_over_time(src, &[0, 100], translate_x)[1].0;
    assert!(
        at_quarter(eased) > at_quarter(linear) + 10.0,
        "ease_out leads linear: {} vs {}",
        at_quarter(eased),
        at_quarter(linear)
    );
}

#[test]
fn a_keyframed_colour_blends_in_oklab() {
    // A colour sequence samples its two surrounding steps perceptually,
    // never by lerping the packed integer.
    let src = "View V() { Box #[width: 10, height: 10, \
                   bg: anim.keyframes(0%: 0x000000, 100%: 0xFFFFFF, duration: 400ms)] }";
    let grey = |f: &byard_core::frame::RenderFrame| f.instances()[0].color[0];
    let seen = sample_over_time(src, &[0, 200, 400], grey);
    assert!(seen[0].0 < 0.01, "starts black, got {}", seen[0].0);
    // The frame carries linear light, so the number read here is the *luminance*
    // of the midpoint. OKLab's midpoint of black→white is `L = 0.5`, the
    // perceptual middle, which is a luminance of about 0.125 and encodes to
    // roughly `0x63`. The mistake this rules out is lerping the two colours in
    // linear light, which lands on 0.5 luminance, a grey most of a stop too
    // bright and the reason a naive fade looks like it washes out in the middle.
    assert!(
        seen[1].0 > 0.09 && seen[1].0 < 0.17,
        "the OKLab midpoint of black→white is the perceptual middle \
             (luminance ≈ 0.125), not the linear-light midpoint (0.5), got {}",
        seen[1].0
    );
    assert!(seen[2].0 > 0.99, "ends white, got {}", seen[2].0);
}

#[test]
fn a_looping_colour_oscillates_between_two_colours() {
    let src = "View V() { Box #[width: 10, height: 10, \
                   bg: 0xFFFFFF with anim.linear(400ms, from: 0x000000, \
                   repeat: infinite, reverse: true)] }";
    let grey = |f: &byard_core::frame::RenderFrame| f.instances()[0].color[0];
    let seen = sample_over_time(src, &[0, 400, 800], grey);
    assert!(seen[0].0 < 0.01, "starts black");
    assert!(seen[1].0 > 0.99, "reaches white");
    assert!(seen[2].0 < 0.01, "and comes back");
    assert!(seen.iter().all(|(_, active)| *active));
}

#[test]
fn an_unmounted_branch_starts_its_animation_over() {
    // RFC-0025: "no separate stop-animation API, the animation lives and
    // dies with its element". A collapsed `when` branch really *unmounts*,
    // so its animation state goes with it: when the branch comes back the
    // spinner starts its turn again instead of resuming a stale phase.
    let src = "View V() { var shown: Bool = true \
                   when shown { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with anim.linear(400ms, from: 0, \
                             repeat: infinite)] {} \
                   } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        (
            frame.instances().first().map(|b| b.transform.translate[0]),
            interp.has_active_animations(),
        )
    };
    render(&mut interp, 0);
    let (x, active) = render(&mut interp, 100);
    assert!((x.unwrap() - 25.0).abs() < 2.0, "a quarter in, got {x:?}");
    assert!(active);

    // Hide it: nothing is drawn, and the app can idle.
    let shown = interp.var_signal(&Symbol::intern("shown")).unwrap();
    interp.write_var(shown, Value::Bool(false));
    interp.tick();
    let (x, active) = render(&mut interp, 900);
    assert_eq!(x, None, "nothing is drawn while hidden");
    assert!(!active, "an unmounted loop costs no frames");

    // Show it again: a fresh mount, so a fresh turn from `from`.
    interp.write_var(shown, Value::Bool(true));
    interp.tick();
    let (x, _) = render(&mut interp, 900);
    assert!(x.unwrap().abs() < 2.0, "a remount starts over, got {x:?}");
    let (x, _) = render(&mut interp, 1_000);
    assert!(
        (x.unwrap() - 25.0).abs() < 3.0,
        "…and runs from there, got {x:?}"
    );
}

#[test]
fn an_animation_that_stops_being_drawn_pauses_and_resumes_in_phase() {
    // RFC-0025 §2: the element is still mounted, it just is not being
    // *painted*, here a windowed `ScrollView` row scrolled out of the
    // window and back. That is a **pause**: the app idles while the row is
    // away and the motion continues exactly where it stopped, with no jump.
    //
    // The mechanism matters, and this test used to use the wrong one. It
    // emptied the list, which is an *unmount*, not an element that is
    // merely off-screen, and RFC-0025 is explicit that an animation lives
    // and dies with its element. Scrolling is the case §2 describes: the
    // pool keeps its rows and their state, only the emission stops.
    let src = "View V() { var y: Float = 0.0 \
                   ScrollView #[width: 40, height: 20, row_height: 20, \
                                windowed: true, offset: (0, y)] { \
                     Column { for row in [1, 2, 3, 4, 5, 6, 7, 8] { \
                       Box #[bg: 0x808080, width: 10, height: 20, \
                             translate: (100, 0) with anim.linear(400ms, from: 0, \
                             repeat: infinite)] {} \
                     } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.instances().first().map(|b| b.transform.translate[0])
    };
    let scroll_to = |interp: &mut Interpreter, y: f64| {
        let sig = interp.var_signal(&Symbol::intern("y")).unwrap();
        interp.write_var(sig, Value::Float(y));
        interp.tick();
    };

    render(&mut interp, 0);
    let x = render(&mut interp, 100);
    assert!((x.unwrap() - 25.0).abs() < 2.0, "a quarter in, got {x:?}");

    // Scroll the first row out of the window. It is still mounted; it is
    // simply not drawn, so its timeline pauses.
    scroll_to(&mut interp, 120.0);
    for step in 2..10 {
        render(&mut interp, step * 100);
    }

    // Back to the top: the row resumes where it stopped rather than
    // jumping to where a wall clock would have carried it.
    scroll_to(&mut interp, 0.0);
    let x = render(&mut interp, 900);
    assert!(
        (x.unwrap() - 25.0).abs() < 4.0,
        "resumes in phase, got {x:?}"
    );
}

/// **The defect the instance half of the key exists for.** Two `for` rows
/// heading for *different* targets each reach their own.
///
/// Before the animation key carried an instance, the two rows shared one
/// `Motion`: every frame, row A retargeted it to A's goal and row B
/// immediately retargeted it to B's, each reseeding `from` to the current
/// sampled value and restarting the clock. The pair stalled near `t = 0`
/// and neither arrived. Anyone with a list of animated components was
/// affected, and the symptom, "my springs are sluggish in a list", points
/// nowhere near the cause.
#[test]
fn two_for_rows_with_different_targets_each_reach_their_own() {
    let src = "View V() { var rows = [{ w: 40 }, { w: 200 }] \
                   Column { for row in rows { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (row.w, 0) with anim.linear(200ms, from: 0)] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };

    render(&mut interp, 0);
    // Mid-flight the two rows are already apart, in proportion to their
    // own targets, a shared `Motion` could only ever produce one value.
    let mid = render(&mut interp, 100);
    assert_eq!(mid.len(), 2, "two rows");
    assert!(
        mid[1] > mid[0] * 2.0,
        "the rows must animate independently, got {mid:?}"
    );
    // …and each settles on its own target rather than fighting to a stall.
    let settled = render(&mut interp, 400);
    assert!(
        (settled[0] - 40.0).abs() < 1.0 && (settled[1] - 200.0).abs() < 1.0,
        "each row must reach its own target, got {settled:?}"
    );
    assert!(
        !interp.has_active_animations(),
        "both arrived, so nothing is still in motion"
    );
}

/// Nested loops distinguish instances too, and without a path to fold: a
/// `for` inside a `for` body is lowered once per *outer* row, so its pool,
/// and every element signal in it, belongs to that row alone.
#[test]
fn nested_for_rows_animate_independently() {
    let src = "View V() { var groups = [{ items: [{ w: 30 }] }, { items: [{ w: 180 }] }] \
                   Column { for g in groups { \
                     Column { for item in g.items { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (item.w, 0) with anim.linear(200ms, from: 0)] {} \
                     } } \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .filter(|x| *x > 0.0)
            .collect()
    };
    render(&mut interp, 0);
    render(&mut interp, 100);
    let settled = render(&mut interp, 400);
    assert_eq!(settled.len(), 2, "two innermost rows, got {settled:?}");
    assert!(
        (settled[0] - 30.0).abs() < 1.0 && (settled[1] - 180.0).abs() < 1.0,
        "each nested row must reach its own target, got {settled:?}"
    );
}

/// The other side of the same coin, and what the instance key changed: a row
/// that leaves the *list* is unmounted, and an unmount takes its animation
/// with it (RFC-0025: "no separate stop-animation API, the animation lives and
/// dies with its element").
///
/// This is not a preference between two readings. A `for` pool's bodies are
/// grow-only, so index 0 keeps its lowered nodes and its element signal
/// across a shrink; without dropping the state, a re-grown row would resume
/// **a different element's** timeline. Starting fresh is the only answer
/// that describes something real.
#[test]
fn a_row_that_leaves_the_list_drops_its_animation() {
    let src = "View V() { var rows: List<Int> = [1] \
                   Column { for row in rows { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with anim.linear(400ms, from: 0, \
                             repeat: infinite)] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        (
            frame.instances().first().map(|b| b.transform.translate[0]),
            interp.has_active_animations(),
        )
    };
    render(&mut interp, 0);
    let (x, _) = render(&mut interp, 100);
    assert!((x.unwrap() - 25.0).abs() < 2.0, "a quarter in, got {x:?}");

    let rows = interp.var_signal(&Symbol::intern("rows")).unwrap();
    interp.write_var(rows, Value::List(vec![]));
    interp.tick();
    let (x, active) = render(&mut interp, 900);
    assert_eq!(x, None, "nothing is drawn while it is gone");
    assert!(!active, "and it costs no frames");

    // A new row in the same pool slot is a new element, not the old one
    // resuming: it starts at the beginning of its own timeline.
    interp.write_var(rows, Value::List(vec![Value::Int(1)]));
    interp.tick();
    let (x, _) = render(&mut interp, 900);
    assert!(x.unwrap() < 5.0, "a re-grown row starts fresh, got {x:?}");
}

/// A `when` inside a `for`, the shape every real list has, because rows are
/// filtered, must not cost the row its identity.
///
/// A `when` branch is lowered **lazily, at render time**, with the scope it
/// was written in restored from a snapshot and truncated again immediately.
/// Anything inside it that resolves later, an animated attribute reading
/// `row`, an event action, a nested pool, has to capture that scope while it
/// is briefly back. Without it, `row` resolves to nothing at render time and
/// every animated target in the branch silently reads `0`: the list looks
/// mounted and correct, and none of it moves.
#[test]
fn a_row_inside_a_when_still_knows_which_row_it_is() {
    let src = "View V() { var count = 2 let rows = [{ w: 40 }, { w: 200 }] \
                   Column { for i, row in rows { when i < count { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (row.w, 0) with anim.linear(200ms, from: 0)] {} \
                   } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };
    render(&mut interp, 0);
    let settled = render(&mut interp, 400);
    assert_eq!(settled.len(), 2, "two rows, got {settled:?}");
    assert!(
        (settled[0] - 40.0).abs() < 1.0 && (settled[1] - 200.0).abs() < 1.0,
        "each filtered row must reach its own target, got {settled:?}"
    );
}

/// The same loss, seen through the other thing a row knows about itself: its
/// index. A stagger's delay is `step × i`, so a branch that forgot `i` gives
/// every row a delay of zero and the cascade arrives as one flash.
#[test]
fn a_stagger_inside_a_when_still_cascades_by_index() {
    let src = "View V() { var count = 3 let rows = [1, 2, 3] \
                   Column { for i, row in rows { when i < count { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with anim.stagger(linear(100ms, from: 0), \
                             200ms, i)] {} \
                   } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };
    render(&mut interp, 0);
    // 250ms in: row 0 has played out, row 1 is halfway through its own play,
    // row 2 has not started. One clause, three phases.
    let mid = render(&mut interp, 250);
    assert_eq!(mid.len(), 3, "three rows, got {mid:?}");
    assert!(
        (mid[0] - 100.0).abs() < 1.0 && (mid[1] - 50.0).abs() < 5.0 && mid[2] < 1.0,
        "the cascade must run in index order, got {mid:?}"
    );
}

/// A row's *size* comes from the same scope its colour does.
///
/// Layout runs one pass ahead of paint over the same tree, and paint
/// restores each box's captured instance environment before reading its
/// attrs. The layout pass did not, so every dimension written in terms of
/// the element it belongs to, `width: row.w` in a list, `width: size` in a
/// user view, resolved to nothing there. Nothing reported it: a `width`
/// that resolves to nothing is a box with no width, which is a box that
/// fills its parent. So the list laid out as one full-width column and
/// painted the colours perfectly.
#[test]
fn a_row_is_laid_out_from_its_own_scope() {
    let src = "View V() { let rows = [{ w: 60 }, { w: 220 }] \
                   Column { for row in rows { \
                       Row #[bg: 0x808080, height: 10, width: row.w] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 700.0, 300.0);
    let widths: Vec<f32> = frame.instances().iter().map(|b| b.rect[2]).collect();
    assert_eq!(
        widths,
        vec![60.0, 220.0],
        "each row is laid out at its own width, not stretched to the viewport"
    );
}

/// The loss compounds: a pool lowered *inside* a lazily-lowered branch
/// inherits that branch's scope, so a nested `for` whose body reads the
/// **outer** row, a per-row detail on each of a row's children, needs the
/// capture to have happened one level up before it can capture it in turn.
#[test]
fn a_nested_for_inside_a_when_still_reads_its_outer_row() {
    let src = "View V() { var count = 2 \
                   let groups = [{ w: 30, items: [1] }, { w: 180, items: [1] }] \
                   Column { for i, g in groups { when i < count { \
                     Column { for item in g.items { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (g.w, 0) with anim.linear(200ms, from: 0)] {} \
                     } } \
                   } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };
    render(&mut interp, 0);
    let settled = render(&mut interp, 400);
    assert_eq!(settled.len(), 2, "one box per nested row, got {settled:?}");
    assert!(
        (settled[0] - 30.0).abs() < 1.0 && (settled[1] - 180.0).abs() < 1.0,
        "each nested row must reach its own target, got {settled:?}"
    );
}

#[test]
fn a_retarget_cancels_a_pending_delay_but_never_a_stagger() {
    // RFC-0025 §5: a delayed transition must not overwrite a newer target,
    // so a target change restarts it immediately, while a stagger's
    // entrance offset survives (it is sequencing, not a response).
    let delayed = "View V() { var on: Bool = false \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: ((on ? 100 : 0), 0) with anim.linear(200ms, from: 0, \
                             delay: 400ms)] }";
    let (mut interp, tree) = lower_named(delayed, "V");
    interp.tick();
    let at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        translate_x(&frame)
    };
    at(&mut interp, 0);
    // Flip the target at 300 ms, inside the original delay window.
    let on = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(on, Value::Bool(true));
    interp.tick();
    at(&mut interp, 300);
    // The pending delay is cancelled, so the property heads for the new
    // target at once rather than sitting still for another 400 ms.
    assert!(
        (at(&mut interp, 400) - 50.0).abs() < 3.0,
        "halfway 100 ms after the retarget, got {}",
        at(&mut interp, 400)
    );
    assert!((at(&mut interp, 500) - 100.0).abs() < 1.0, "arrived");

    // A stagger, by contrast, honours its offset again, the cascade
    // *replays* in item order instead of snapping.
    let staggered = "View V() { var on: Bool = false \
                         Column { for i, row in [1, 2] { \
                             Box #[bg: 0x808080, width: 10, height: 10, \
                                   translate: ((on ? 100 : 0), 0) with \
                                   anim.stagger(linear(100ms, from: 0), 200ms, i)] {} \
                         } } }";
    let (mut interp, tree) = lower_named(staggered, "V");
    interp.tick();
    let xs = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect::<Vec<_>>()
    };
    xs(&mut interp, 0);
    let on = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(on, Value::Bool(true));
    interp.tick();
    xs(&mut interp, 0);
    // 150 ms in: row 0 (no offset) has arrived, row 1 is still waiting out
    // its 200 ms offset.
    let seen = xs(&mut interp, 150);
    assert!((seen[0] - 100.0).abs() < 1.0, "row 0 played, got {seen:?}");
    assert!(seen[1].abs() < 1.0, "row 1 still waiting, got {seen:?}");
    let seen = xs(&mut interp, 400);
    assert!(
        seen.iter().all(|x| (x - 100.0).abs() < 1.0),
        "the whole cascade has played, got {seen:?}"
    );
}

#[test]
fn a_restart_witness_replays_the_animation_in_order() {
    // RFC-0025 §5's replay case: an entrance's endpoints never change, so
    // nothing would ever retarget it. `restart:` is the reference-free "play
    // that again", and because a replay is intentional sequencing, the
    // stagger offsets are honoured again rather than cancelled.
    let src = "View V() { var attempt: Int = 0 \
                   Column { for i, row in [1, 2] { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with \
                             anim.stagger(linear(100ms, from: 0), 200ms, i, restart: attempt)] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let xs = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect::<Vec<_>>()
    };
    // The first cascade plays out.
    xs(&mut interp, 0);
    let seen = xs(&mut interp, 600);
    assert!(
        seen.iter().all(|x| (x - 100.0).abs() < 1.0),
        "both rows arrived, got {seen:?}"
    );

    // Bump the witness: the cascade runs again, in item order.
    let attempt = interp.var_signal(&Symbol::intern("attempt")).unwrap();
    interp.write_var(attempt, Value::Int(1));
    interp.tick();
    let seen = xs(&mut interp, 600);
    assert!(
        seen[0].abs() < 1.0 && seen[1].abs() < 1.0,
        "the replay starts both rows over, got {seen:?}"
    );
    let seen = xs(&mut interp, 700);
    assert!(
        (seen[0] - 100.0).abs() < 1.0,
        "row 0 has replayed, got {seen:?}"
    );
    assert!(
        seen[1].abs() < 1.0,
        "row 1 is still waiting out its offset, got {seen:?}"
    );
    let seen = xs(&mut interp, 950);
    assert!(
        seen.iter().all(|x| (x - 100.0).abs() < 1.0),
        "the whole cascade replayed, got {seen:?}"
    );
}

#[test]
fn a_restart_witness_also_replays_a_keyframe_sequence() {
    let src = "View V() { var attempt: Int = 0 \
                   Box #[bg: 0x808080, width: 10, height: 10, \
                         translate: anim.keyframes(0%: 0, 100%: 100, duration: 400ms, \
                         restart: attempt)] {} }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        translate_x(&frame)
    };
    at(&mut interp, 0);
    assert!((at(&mut interp, 400) - 100.0).abs() < 1.0, "played out");
    let attempt = interp.var_signal(&Symbol::intern("attempt")).unwrap();
    interp.write_var(attempt, Value::Int(1));
    interp.tick();
    assert!(at(&mut interp, 400).abs() < 1.0, "the sequence starts over");
    assert!((at(&mut interp, 600) - 50.0).abs() < 3.0, "and runs again");
}

#[test]
fn stopping_the_looping_example_empties_the_active_set() {
    // Ties the shipped RFC-0025 example to the promise its header makes: with
    // `loading` off, *every* endless animation is unmounted, so the active set
    // empties and a host may park its event loop instead of spinning. A
    // stray infinite animation left outside the `when` would show up here as
    // an app that can never idle.
    let src = include_str!("../../../../byard-cli/examples/looping_animations/src/main.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 700.0, 750.0);
        interp.has_active_animations()
    };
    assert!(at(&mut interp, 0), "the loaders animate on mount");
    assert!(at(&mut interp, 2_000), "and keep animating");

    let loading = interp.var_signal(&Symbol::intern("loading")).unwrap();
    interp.write_var(loading, Value::Bool(false));
    interp.tick();
    for ms in [2_100_u32, 3_000, 9_000] {
        assert!(
            !at(&mut interp, ms),
            "nothing may still be animating at t={ms} after Stop"
        );
    }
    // Starting again brings the motion back (a fresh mount, from the top).
    interp.write_var(loading, Value::Bool(true));
    interp.tick();
    assert!(at(&mut interp, 9_100), "Start resumes the motion");
}

#[test]
fn a_layout_property_cannot_be_keyframed() {
    // RFC-0025 §3 defers to RFC-0010: keyframes on a layout prop would
    // relayout every frame (INV-8), so they are rejected like `with` is.
    let parsed = parse(
        "View V() { Box #[bg: 0x808080, height: 10, \
             width: anim.keyframes(0%: 0, 100%: 200, duration: 1s)] }",
    );
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(&parsed.views[0], &[]);
    assert!(
        interp.errors().iter().any(
            |e| matches!(e, CompileError::LayoutPropNotAnimatable { prop, .. } if prop == "width")
        ),
        "expected LayoutPropNotAnimatable, got {:?}",
        interp.errors()
    );
}

/// RFC-0005 `ScrollView`: content is clipped to the viewport and translated
/// by `−offset`, so scrolling moves the content up without relayout.
#[test]
fn scrollview_clips_and_translates_content_by_offset() {
    let src = "View V() { var off: Int = 0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Box #[bg: 0xFF0000, width: 180, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    // The red content box's paint-time translate.y (where the scroll offset
    // lives, the shader applies it, so the layout rect is untouched, i.e.
    // no relayout on scroll), plus whether a content clip was emitted.
    let sample = |interp: &mut Interpreter| -> (f32, f32, usize) {
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let red = *frame
            .instances()
            .iter()
            .find(|b| b.color[0] > 0.8 && b.color[1] < 0.3 && b.color[2] < 0.3)
            .expect("the red content box is emitted");
        (red.rect[1], red.transform.translate[1], frame.clips().len())
    };

    let (rect_y0, tx0, clips0) = sample(&mut interp);
    assert!(clips0 >= 1, "the ScrollView must emit a content clip");

    // Scroll down by 40 logical px → the content's paint translate moves up
    // by 40, while its layout rect is unchanged (INV-8: no relayout).
    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    interp.write_var(off, Value::Int(40));
    interp.tick();
    let (rect_y1, tx1, clips1) = sample(&mut interp);
    assert!(clips1 >= 1);
    assert!(
        (rect_y0 - rect_y1).abs() < 0.01,
        "layout rect must not move on scroll (no relayout): {rect_y0} vs {rect_y1}"
    );
    assert!(
        (tx0 - tx1 - 40.0).abs() < 0.5,
        "content must translate up by the offset: tx0={tx0} tx1={tx1}"
    );
}

/// RFC-0005: the mouse wheel over a `ScrollView` scrolls it by writing the
/// signal behind `offset.y`, clamped to `[0, content − viewport]`.
#[test]
fn wheel_over_a_scrollview_scrolls_and_clamps_the_offset() {
    // Content = 4 × 60px = 240 tall in a 100px viewport → max scroll 140.
    let src = "View V() { var off: Float = 0.0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Box #[bg: 0xFF0000, width: 180, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                     Box #[bg: 0xFFFF00, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    // Render once to record the scroll target (viewport at the top-left).
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    let peek_f = |interp: &Interpreter| -> f32 {
        match interp.peek(off) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => panic!("offset must be numeric"),
        }
    };
    let wheel = |interp: &mut Interpreter, dy: f32| {
        interp.dispatch_events(&[byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::Wheel,
            pos: (100.0, 50.0), // inside the 200×100 viewport
            delta: (0.0, dy),
            payload: None,
            time_ms: 0,
        }]);
        interp.tick();
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 400.0, 300.0);
    };

    // Wheel forward by 2 lines (× 40px) → scroll down by 80.
    wheel(&mut interp, -2.0);
    let after_one = peek_f(&interp);
    assert!(
        (after_one - 80.0).abs() < 1.0,
        "one wheel notch scrolls by lines×40, got {after_one}"
    );

    // A big wheel clamps to the content extent (max = 240 − 100 = 140).
    wheel(&mut interp, -20.0);
    let clamped = peek_f(&interp);
    assert!(
        (clamped - 140.0).abs() < 1.0,
        "scroll must clamp to content−viewport, got {clamped}"
    );

    // Wheel back up past the top clamps at 0.
    wheel(&mut interp, 20.0);
    let top = peek_f(&interp);
    assert!(top.abs() < 1.0, "scroll must clamp at 0, got {top}");
}

/// RFC-0005 emission culling: a `ScrollView` child scrolled entirely out of
/// the viewport is never pushed to the frame (only its visible slice costs
/// anything), while the flat-id cursor stays aligned so siblings still paint.
#[test]
fn scrollview_culls_children_scrolled_out_of_view() {
    let src = "View V() { var off: Int = 0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Box #[bg: 0xFF0000, width: 180, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    // Present iff a box with the given dominant colour channel was emitted.
    let has = |interp: &mut Interpreter, chan: usize| -> bool {
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .any(|b| b.color[chan] > 0.8 && b.color[(chan + 1) % 3] < 0.3)
    };

    // At rest, the third box (y 120..180) sits fully below the 100px
    // viewport → culled. The first two overlap it → kept.
    assert!(has(&mut interp, 0), "red (top) is visible at rest");
    assert!(has(&mut interp, 1), "green (straddling) is visible at rest");
    assert!(!has(&mut interp, 2), "blue (below) is culled at rest");

    // Scroll down 120px: the first box (now y -120..-60) leaves the top →
    // culled; the third box scrolls into view.
    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    interp.write_var(off, Value::Int(120));
    interp.tick();
    assert!(
        !has(&mut interp, 0),
        "red is culled once scrolled past the top"
    );
    assert!(has(&mut interp, 2), "blue scrolls into view");
}

/// RFC-0005: dragging on inert `ScrollView` content scrolls it, the content
/// tracks the pointer between press and release, clamped to the extent.
#[test]
fn drag_on_scrollview_content_scrolls_and_clamps() {
    use byard_core::platform::EventKind as K;
    // Content = 4 × 60px = 240 tall in a 100px viewport → max scroll 140.
    let src = "View V() { var off: Float = 0.0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Box #[bg: 0xFF0000, width: 180, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                     Box #[bg: 0xFFFF00, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    let peek_f = |interp: &Interpreter| match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("offset must be numeric"),
    };
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };

    // Press on inert content, then drag up 50px → content scrolls down 50.
    interp.dispatch_events(&[ev(K::PointerDown, 100.0, 80.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, 30.0)]);
    assert!(
        (peek_f(&interp) - 50.0).abs() < 1.0,
        "drag up 50px scrolls down 50, got {}",
        peek_f(&interp)
    );

    // Dragging further up clamps at the content extent (140).
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, -200.0)]);
    assert!(
        (peek_f(&interp) - 140.0).abs() < 1.0,
        "drag clamps to content−viewport, got {}",
        peek_f(&interp)
    );

    // Releasing ends the gesture: a later stray move no longer scrolls.
    interp.dispatch_events(&[ev(K::PointerUp, 100.0, -200.0)]);
    let held = peek_f(&interp);
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, 300.0)]);
    assert!(
        (peek_f(&interp) - held).abs() < 0.01,
        "no drag is in flight after release, got {} (was {held})",
        peek_f(&interp)
    );
}

// ── RFC-0021: snap + pagination + on_end_reached ─────────────────────

#[test]
fn page_snap_settles_to_nearest_page_on_release() {
    use byard_core::platform::EventKind as K;
    // Horizontal `snap: page`: three 100px pages in a 100px viewport (max
    // 200). Drag left 60px → offset 60; release snaps to page 1 (offset 100),
    // reflects `page`, and would fire `page_change`.
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var pg: Int = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), page: pg, \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let pg = interp.var_signal(&Symbol::intern("pg")).unwrap();
    let peek_f = |interp: &Interpreter| match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };

    interp.dispatch_events(&[ev(K::PointerDown, 50.0, 50.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, -10.0, 50.0)]); // drag left 60 → offX 60
    assert!(
        (peek_f(&interp) - 60.0).abs() < 2.0,
        "mid-drag offset ~60, got {}",
        peek_f(&interp)
    );
    interp.dispatch_events(&[ev(K::PointerUp, -10.0, 50.0)]); // release → snap
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "snapped to page 1 (offset 100), got {}",
        peek_f(&interp)
    );
    assert_eq!(interp.peek(pg), Value::Int(1), "the reflected page is 1");
}

#[test]
fn on_end_reached_fires_once_past_threshold() {
    use byard_core::platform::EventKind as K;
    // 400px of content in a 100px viewport (max 300); `end_threshold: 0.8`.
    // Drag up 250 → offset 250 → visible bottom (250+100)/400 = 0.875 ≥ 0.8,
    // so `end_reached` fires and sets `loaded`.
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var loaded = false \
             ScrollView #[offset: (offX, offY), width: 100, height: 100, \
                          end_threshold: 0.8, end_reached => loaded = true] { \
                 Column { Box #[width: 100, height: 400] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let loaded = interp.var_signal(&Symbol::intern("loaded")).unwrap();
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };
    assert_eq!(
        interp.peek(loaded),
        Value::Bool(false),
        "not loaded at rest"
    );

    interp.dispatch_events(&[ev(K::PointerDown, 50.0, 50.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 50.0, -200.0)]); // drag up 250 → offY 250
    interp.tick();
    assert_eq!(
        interp.peek(loaded),
        Value::Bool(true),
        "end_reached fired past the 0.8 threshold"
    );
}

#[test]
fn setting_the_page_var_scrolls_to_that_page() {
    // Reflected `page:` (reverse): writing `page = 2` scrolls the horizontal
    // offset to page 2 (offset 200) on the next render.
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var pg: Int = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), page: pg, \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0); // records the scroll target

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let pg = interp.var_signal(&Symbol::intern("pg")).unwrap();

    interp.write_var(pg, Value::Int(2));
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 300.0); // sync scrolls to page 2

    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 200.0).abs() < 1.0,
        "page = 2 scrolled the offset to 200, got {got}"
    );
}

#[test]
fn page_reflects_on_wheel_scroll_without_a_release() {
    use byard_core::platform::EventKind as K;
    // A trackpad/wheel `Scroll` updates the reflected `page` continuously,
    // no drag release needed (the desktop scrolling case).
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var pg: Int = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), page: pg, \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let pg = interp.var_signal(&Symbol::intern("pg")).unwrap();
    // Scroll right ~120px (a `Scroll` delta is pixels): offX → 120, page → 1.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-120.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    assert_eq!(
        interp.peek(pg),
        Value::Int(1),
        "page reflects the wheel scroll"
    );
}

#[test]
fn wheel_scroll_snaps_to_a_page_after_settling() {
    use byard_core::platform::EventKind as K;
    // Wheel-scroll 60px, then hold still: once the offset stops moving for a
    // few frames the settle fires and snaps to page 1, no release event, no
    // wall clock, just observed stillness (clock-independent settle).
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-60.0, 0.0), // offX → 60 (not yet a page boundary)
        payload: None,
        time_ms: 0,
    }]);
    // Render successive idle frames with the offset held still; the settle
    // counts stable frames and snaps once it reaches its threshold.
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 100.0).abs() < 1.0,
        "wheel scroll settled and snapped to page 1 (offset 100), got {got}"
    );
}

/// RFC-0021: the stillness settle must not fire while a page is *actively*
/// being scrolled, the offset moving each frame restarts the settle count,
/// so a mid-scroll frame never snaps out from under the motion.
#[test]
fn wheel_scroll_does_not_snap_while_still_moving() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let wheel = |dx: f32| byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (dx, 0.0),
        payload: None,
        time_ms: 0,
    };
    // Several small scrolls, each followed by a render (offset keeps moving),
    // so the settle count resets every frame and never snaps mid-motion.
    for _ in 0..3 {
        interp.dispatch_events(&[wheel(-20.0)]);
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 60.0).abs() < 1.0,
        "offset must track the in-progress scroll (60), not snap early, got {got}"
    );
}

/// RFC-0021: an enum keyword prop (`snap: page`) must keep working even when
/// the view declares a `var` of the *same name* as the keyword, the token is
/// read from the AST, so the variable can never shadow it. This is exactly the
/// `scroll_snap` example's shape (`var page` + `snap: page`), which silently
/// disabled snapping before enum props stopped resolving through the env.
#[test]
fn snap_page_keyword_is_not_shadowed_by_a_same_named_var() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX = 0.0 var offY = 0.0 var page = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          page: page, width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let pg = interp.var_signal(&Symbol::intern("page")).unwrap();
    // Wheel two-thirds of a page (past the snap midpoint) then hold still: the
    // stillness settle must snap forward to page 1, not stay put.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-70.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 100.0).abs() < 1.0,
        "`snap: page` must engage despite the `var page`; snapped to {got}, want 100"
    );
    assert_eq!(interp.peek(pg), Value::Int(1), "reflected page is 1");
}

fn page_snap_view() -> &'static str {
    "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }"
}

/// RFC-0021 smooth snap: with an advancing clock the offset *glides* to the
/// page over several frames (a spring), rather than hard-jumping, some frame
/// must land strictly between the release offset and the page boundary.
#[test]
fn page_snap_glides_smoothly_when_a_clock_is_advancing() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(page_snap_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.set_now_ms(0);
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    // Wheel two-thirds of a page toward page 1, then let it settle: past the
    // quiet threshold it springs from 70 → 100.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-70.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    let mut saw_intermediate = false;
    for step in 1..=60 {
        interp.set_now_ms(step * 16);
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let v = peek_f(&interp);
        if v > 70.5 && v < 99.5 {
            saw_intermediate = true; // mid-glide, neither the start nor the page
        }
    }
    assert!(
        saw_intermediate,
        "the offset must glide through intermediate positions, not hard-jump"
    );
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "the glide settles exactly on page 1 (100), got {}",
        peek_f(&interp)
    );
}

/// RFC-0021: a stream of shrinking scroll deltas (trackpad momentum) must not
/// trigger a snap while the fling is still delivering events, the offset
/// tracks the input and only snaps once the scroll goes quiet, so the snap and
/// the scroll never fight.
#[test]
fn momentum_scroll_does_not_snap_until_it_goes_quiet() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(page_snap_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    let wheel = |dx: f32| byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (dx, 0.0),
        payload: None,
        time_ms: 0,
    };
    // Five momentum frames (input every frame) accumulate to offset 75 without
    // ever snapping, each event restarts the quiet countdown.
    let mut acc = 0.0;
    for _ in 0..5 {
        interp.dispatch_events(&[wheel(-15.0)]);
        interp.render(&tree, &mut frame, 400.0, 300.0);
        acc += 15.0;
        assert!(
            (peek_f(&interp) - acc).abs() < 1.0,
            "offset must track momentum ({acc}), never snap mid-fling; got {}",
            peek_f(&interp)
        );
    }
    // Fling over: a few quiet renders and it snaps to the nearest page (75 → 100).
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "once quiet, it snaps to page 1 (100), got {}",
        peek_f(&interp)
    );
}

/// RFC-0021 `snap: item`: with unequal-width items the offset settles to the
/// nearest *child boundary* (not a fixed page), so a carousel of varied cards
/// snaps each card to the viewport edge.
#[test]
fn item_snap_settles_to_the_nearest_child_boundary() {
    use byard_core::platform::EventKind as K;
    // Three items of widths 80/140/100 in a 120px viewport. Boundaries (item
    // starts) are 0, 80, 220; content 320, max 200. A wheel to offX 60 is
    // nearer boundary 80 than 0 → snaps to 80.
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: item, offset: (offX, offY), \
                          width: 120, height: 100] { \
                 Row { Box #[width: 80, height: 100] {} \
                       Box #[width: 140, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-60.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 80.0).abs() < 1.0,
        "snaps to the item-2 boundary (80), got {}",
        peek_f(&interp)
    );
}

/// RFC-0021 fling projection: a fast flick that stops short of the midpoint
/// still advances one page in the fling direction (the velocity carries it),
/// whereas the same offset with no velocity snaps back to the nearest page.
#[test]
fn fast_fling_projects_one_page_forward() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    let wheel = |dx: f32, t: u64| byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (dx, 0.0),
        payload: None,
        time_ms: t,
    };
    // Two quick flicks 20px apart in 40ms → ~500 px/s, well past 150 dp/s, but
    // the offset only reaches 40, short of the page-0→1 midpoint (50).
    interp.dispatch_events(&[wheel(-20.0, 0)]);
    interp.dispatch_events(&[wheel(-20.0, 40)]);
    assert!(
        (peek_f(&interp) - 40.0).abs() < 1.0,
        "offset reached 40 (short of the midpoint), got {}",
        peek_f(&interp)
    );
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "the fling projects forward to page 1 (100), got {}",
        peek_f(&interp)
    );
}

/// Finds the first `collapse_header` `ScrollView`'s `scroll_fraction` signal
/// (carried on `bound_sig`) in a lowered tree.
fn find_collapse_sig(nodes: &[RenderNode]) -> Option<SignalId> {
    for n in nodes {
        if let RenderNode::Box {
            name,
            bound_sig,
            children,
            ..
        } = n
        {
            if name.as_str() == "ScrollView" {
                if let Some(sig) = bound_sig {
                    return Some(*sig);
                }
            }
            if let Some(s) = find_collapse_sig(children) {
                return Some(s);
            }
        }
    }
    None
}

/// RFC-0021 collapsing header: `scroll_fraction` runs 0 → 1 over the header's
/// collapsible range (natural height − `collapse_min`) and clamps past it.
#[test]
fn collapse_header_drives_scroll_fraction() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             ScrollView #[collapse_header: true, collapse_min: 40, offset: (sx, sy), \
                          width: 200, height: 150] { \
                 Box #[bg: 0xAABBCC, width: 200] { Box #[width: 200, height: 100] {} } \
                 Column { Box #[bg: 0x112233, width: 200, height: 120] {} \
                          Box #[bg: 0x223344, width: 200, height: 120] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let frac = find_collapse_sig(&tree).expect("scroll_fraction signal exists");
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let mut frame = byard_core::frame::RenderFrame::new();
    let read = |i: &Interpreter| match i.peek(frac) {
        Value::Float(f) => f as f32,
        _ => -1.0,
    };
    // Range is 100 − 40 = 60px.
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        read(&interp).abs() < 1e-3,
        "expanded → 0, got {}",
        read(&interp)
    );
    interp.write_var(sy, Value::Float(30.0));
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (read(&interp) - 0.5).abs() < 1e-2,
        "halfway → 0.5, got {}",
        read(&interp)
    );
    interp.write_var(sy, Value::Float(90.0));
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (read(&interp) - 1.0).abs() < 1e-3,
        "past range → clamped 1, got {}",
        read(&interp)
    );
}

/// RFC-0021 collapsing header: the header (first child) stays pinned to the
/// viewport top as the content scrolls under it.
#[test]
fn collapse_header_pins_header_while_content_scrolls() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             ScrollView #[collapse_header: true, offset: (sx, sy), width: 200, height: 150] { \
                 Box #[bg: 0xAABBCC, width: 200, height: 80] {} \
                 Box #[bg: 0x112233, width: 200, height: 200] {} \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let header_rgba = crate::interp::intrinsics::color_to_rgba(0x00AA_BBCC, false);
    let content_rgba = crate::interp::intrinsics::color_to_rgba(0x0011_2233, false);
    // Scroll is applied as a paint-time translate, not baked into the layout
    // rect, so the painted y is `rect.y + transform.translate.y`.
    let y_of = |frame: &byard_core::frame::RenderFrame, rgba: [f32; 4]| -> Option<f32> {
        frame
            .instances()
            .iter()
            .find(|b| b.color == rgba)
            .map(|b| b.rect[1] + b.transform.translate[1])
    };
    let mut f0 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f0, 400.0, 300.0);
    let (h0, c0) = (y_of(&f0, header_rgba), y_of(&f0, content_rgba));
    // Scroll down 50px.
    interp.write_var(sy, Value::Float(50.0));
    let mut f1 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f1, 400.0, 300.0);
    let (h1, c1) = (y_of(&f1, header_rgba), y_of(&f1, content_rgba));
    let (h0, h1) = (h0.expect("header painted"), h1.expect("header painted"));
    let (c0, c1) = (c0.expect("content painted"), c1.expect("content painted"));
    assert!((h1 - h0).abs() < 0.5, "header stays pinned: {h0} → {h1}");
    assert!(
        (c1 - (c0 - 50.0)).abs() < 0.5,
        "content scrolls up by 50: {c0} → {c1}"
    );
    // The header must paint *on top* of the content that scrolls under it,
    // draw-order depth is emission order and `draw_depth` decreases with it, so
    // the (last-emitted) header's depth is strictly nearer than the content's.
    let depth_of = |frame: &byard_core::frame::RenderFrame, rgba: [f32; 4]| -> f32 {
        let i = frame
            .instances()
            .iter()
            .position(|b| b.color == rgba)
            .expect("painted");
        frame.solid_depths()[i]
    };
    assert!(
        depth_of(&f1, header_rgba) < depth_of(&f1, content_rgba),
        "the pinned header paints over the scrolling content"
    );
}

/// RFC-0021 collapsing header: the pin + `scroll_fraction` work with the
/// example's real shape, the ScrollView nested in an outer `Column`, and a
/// header that is a `Box` wrapping a `Column` of `Text`s (not a bare box).
#[test]
fn collapse_header_works_nested_like_the_example() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             Column #[bg: 0x14141C, width: 200] { \
                 ScrollView #[collapse_header: true, collapse_min: 30, offset: (sx, sy), \
                              width: 200, height: 120] { \
                     Box #[bg: 0xAABBCC, width: 200, p: 10] { \
                         Column { Text(\"Title\") Text(\"Sub\") } \
                     } \
                     Column #[p: 8] { \
                         Box #[bg: 0x112233, width: 180, height: 90] {} \
                         Box #[bg: 0x223344, width: 180, height: 90] {} \
                     } \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    // The `scroll_fraction` signal must be minted even when nested.
    let frac = find_collapse_sig(&tree).expect("nested collapse header is detected");
    interp.tick();
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let header_rgba = crate::interp::intrinsics::color_to_rgba(0x00AA_BBCC, false);
    let y_of = |frame: &byard_core::frame::RenderFrame| -> Option<f32> {
        frame
            .instances()
            .iter()
            .find(|b| b.color == header_rgba)
            .map(|b| b.rect[1] + b.transform.translate[1])
    };
    let mut f0 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f0, 400.0, 300.0);
    let h0 = y_of(&f0).expect("header painted");
    interp.write_var(sy, Value::Float(40.0));
    let mut f1 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f1, 400.0, 300.0);
    let h1 = y_of(&f1).expect("header painted");
    assert!(
        (h1 - h0).abs() < 0.5,
        "the header stays pinned when nested: {h0} → {h1}"
    );
    assert!(
        matches!(interp.peek(frac), Value::Float(f) if f > 0.5),
        "scroll_fraction advanced past 0.5 after scrolling 40px, got {:?}",
        interp.peek(frac)
    );
}

/// RFC-0021 collapsing header: a header child's `opacity: 1.0 -
/// scroll_fraction` actually fades its text as the header collapses.
#[test]
fn collapse_header_fades_child_via_scroll_fraction() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             ScrollView #[collapse_header: true, collapse_min: 20, offset: (sx, sy), \
                          width: 200, height: 100] { \
                 Box #[bg: 0xAABBCC, width: 200, p: 8] { \
                     Box #[opacity: 1.0 - scroll_fraction] { Text(\"Sub\") #[color: 0xC7BFDE] } \
                 } \
                 Column #[p: 8] { Box #[bg: 0x112233, width: 180, height: 120] {} \
                                  Box #[bg: 0x223344, width: 180, height: 120] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let sub_rgb = crate::interp::intrinsics::color_to_rgba(0x00C7_BFDE, false);
    let sub_alpha = |frame: &byard_core::frame::RenderFrame| -> Option<f32> {
        frame
            .texts()
            .iter()
            .find(|t| t.color[..3] == sub_rgb[..3])
            .map(|t| t.color[3])
    };
    let mut f0 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f0, 400.0, 300.0);
    let a0 = sub_alpha(&f0).expect("subtitle painted");
    interp.write_var(sy, Value::Float(80.0));
    let mut f1 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f1, 400.0, 300.0);
    let a1 = sub_alpha(&f1).expect("subtitle painted");
    assert!(a0 > 0.9, "expanded subtitle is opaque, got {a0}");
    assert!(a1 < 0.1, "collapsed subtitle has faded out, got {a1}");
}

/// RFC-0021 `snap_align: center`: the snapped item is centred in the viewport,
/// so its rest offset is its start minus half the slack `(viewport − item)`.
#[test]
fn item_snap_align_center_centres_the_item() {
    use byard_core::platform::EventKind as K;
    // Uniform 100px items in a 140px viewport. Centring item 1 (start 100)
    // rests at 100 − (140 − 100)/2 = 80. A wheel near there settles to 80.
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: item, snap_align: center, \
                          offset: (offX, offY), width: 140, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-75.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 80.0).abs() < 1.0,
        "centres item 1 at offset 80, got {}",
        peek_f(&interp)
    );
}

/// RFC-0021 `snap_spring`: a custom spring override checks clean and still
/// snaps to the page (the override changes the glide feel, not the target).
#[test]
fn snap_spring_override_checks_and_snaps() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, \
                          snap_spring: anim.spring(stiffness: 320, damping: 18), \
                          offset: (offX, offY), width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(
        interp.errors().is_empty(),
        "snap_spring resolves cleanly: {:?}",
        interp.errors()
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-70.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 100.0).abs() < 1.0,
        "still snaps to page 1 with the custom spring, got {got}"
    );
}

/// RFC-0021 `snap_spring`: a malformed curve is reported (and falls back to the
/// default), not silently accepted.
#[test]
fn snap_spring_malformed_is_reported() {
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, snap_spring: anim.wobble, \
                          offset: (offX, offY), width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    // `snap_spring` is resolved during render (scroll-target build), so drive a
    // frame to surface the diagnostic.
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        !interp.errors().is_empty(),
        "an unknown curve name is diagnosed"
    );
}

fn pull_refresh_view() -> &'static str {
    "View V() { var refreshing = false var refreshed = false \
             ScrollView #[pull_refresh: true, refreshing: refreshing, \
                          refresh => refreshed = true, width: 200, height: 120] { \
                 Column { Box #[width: 180, height: 50] {} \
                          Box #[width: 180, height: 50] {} } \
             } }"
}

/// RFC-0021 pull-to-refresh: a downward over-drag past the threshold fires
/// `refresh`, sets the reflected `refreshing` var, and rests the indicator;
/// the app clearing `refreshing` retracts it.
#[test]
fn pull_past_threshold_fires_refresh_and_retracts_when_cleared() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(pull_refresh_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let refreshing = interp.var_signal(&Symbol::intern("refreshing")).unwrap();
    let refreshed = interp.var_signal(&Symbol::intern("refreshed")).unwrap();
    let ev = |kind, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (100.0, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };
    // Press at the top, drag down 130px (past the elastic threshold), release.
    interp.dispatch_events(&[ev(K::PointerDown, 12.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 142.0)]);
    interp.dispatch_events(&[ev(K::PointerUp, 142.0)]);
    assert_eq!(
        interp.peek(refreshed),
        Value::Bool(true),
        "release past the threshold fires `refresh`"
    );
    assert_eq!(
        interp.peek(refreshing),
        Value::Bool(true),
        "the engine sets `refreshing` while it loads"
    );
    // The controller finishes: clearing `refreshing` retracts the indicator.
    interp.write_var(refreshing, Value::Bool(false));
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // (no panic / clean retract, with no clock the spring resolves instantly)
}

/// RFC-0021 pull-to-refresh: a short pull that never reaches the threshold
/// retracts on release without firing `refresh`.
#[test]
fn short_pull_retracts_without_refreshing() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(pull_refresh_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let refreshing = interp.var_signal(&Symbol::intern("refreshing")).unwrap();
    let refreshed = interp.var_signal(&Symbol::intern("refreshed")).unwrap();
    let ev = |kind, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (100.0, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };
    // Only a 20px pull, well under the threshold.
    interp.dispatch_events(&[ev(K::PointerDown, 12.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 32.0)]);
    interp.dispatch_events(&[ev(K::PointerUp, 32.0)]);
    assert_eq!(
        interp.peek(refreshed),
        Value::Bool(false),
        "a short pull does not fire `refresh`"
    );
    assert_eq!(interp.peek(refreshing), Value::Bool(false));
}

/// RFC-0005: a press that lands on an interactive child (here a `Button`)
/// is that child's gesture, drag-to-scroll defers and the list stays put.
#[test]
fn drag_defers_to_interactive_children() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var off: Float = 0.0 var c: Int = 0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Button(\"tap\") #[width: 180, height: 60] => c++ \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };

    // Press on the Button (top 60px), then drag: the button owns the press,
    // so the list must not scroll.
    interp.dispatch_events(&[ev(K::PointerDown, 100.0, 30.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, -60.0)]);
    let scrolled = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    assert!(
        scrolled.abs() < 0.01,
        "a press on a Button must not drag-scroll the list, got {scrolled}"
    );
}

/// RFC-0005 `axis: horizontal`: content overflows on the inline axis and the
/// wheel's x delta scrolls `offset.x`, clamped to the horizontal extent.
#[test]
fn horizontal_scrollview_scrolls_offset_x_by_wheel() {
    // A Row of 4 × 100px cards = 400 wide in a 200px viewport → max x 200.
    let src = "View V() { var panX: Float = 0.0 \
             ScrollView #[width: 200, height: 80, axis: horizontal, offset: (panX, 0)] { \
                 Row { \
                     Box #[bg: 0xFF0000, width: 100, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 100, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 100, height: 60] {} \
                     Box #[bg: 0xFFFF00, width: 100, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let pan = interp.var_signal(&Symbol::intern("panX")).unwrap();
    let peek = |interp: &Interpreter| match interp.peek(pan) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    // Sample the red card's paint translate.x, so we prove the content
    // actually shifts left (−offset), not just that the signal moved.
    let red_tx = |interp: &mut Interpreter| -> f32 {
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 400.0, 300.0);
        f.instances()
            .iter()
            .find(|b| b.color[0] > 0.8 && b.color[1] < 0.3 && b.color[2] < 0.3)
            .map_or(0.0, |b| b.transform.translate[0])
    };
    let tx0 = red_tx(&mut interp);

    let wheel_x = |interp: &mut Interpreter, dx: f32| {
        interp.dispatch_events(&[byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::Wheel,
            pos: (100.0, 40.0),
            delta: (dx, 0.0),
            payload: None,
            time_ms: 0,
        }]);
        interp.tick();
    };

    // Wheel 2 lines right (×40) → scroll right 80; red card shifts left 80.
    wheel_x(&mut interp, -2.0);
    assert!(
        (peek(&interp) - 80.0).abs() < 1.0,
        "x wheel scrolls, got {}",
        peek(&interp)
    );
    assert!(
        (tx0 - red_tx(&mut interp) - 80.0).abs() < 0.5,
        "content shifts left by the x offset"
    );

    // A big wheel clamps to content−viewport (400 − 200 = 200).
    wheel_x(&mut interp, -20.0);
    assert!(
        (peek(&interp) - 200.0).abs() < 1.0,
        "x clamps to extent, got {}",
        peek(&interp)
    );
}

/// RFC-0005 `axis: both`: a single drag pans the content in 2D, each axis
/// clamped independently.
#[test]
fn both_axis_scrollview_pans_in_two_dimensions_by_drag() {
    use byard_core::platform::EventKind as K;
    // A 400×400 content grid in a 200×200 viewport → max 200 on each axis.
    let src = "View V() { var panX: Float = 0.0 var panY: Float = 0.0 \
             ScrollView #[width: 200, height: 200, axis: both, offset: (panX, panY)] { \
                 Column { \
                     Row { Box #[bg: 0xFF0000, width: 200, height: 200] {} \
                           Box #[bg: 0x00FF00, width: 200, height: 200] {} } \
                     Row { Box #[bg: 0x0000FF, width: 200, height: 200] {} \
                           Box #[bg: 0xFFFF00, width: 200, height: 200] {} } \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let px = interp.var_signal(&Symbol::intern("panX")).unwrap();
    let py = interp.var_signal(&Symbol::intern("panY")).unwrap();
    let peek = |interp: &Interpreter, s| match interp.peek(s) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };

    // Press mid-viewport, drag up-left 60px each → pan right 60, down 60.
    interp.dispatch_events(&[ev(K::PointerDown, 100.0, 100.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 40.0, 40.0)]);
    assert!(
        (peek(&interp, px) - 60.0).abs() < 1.0,
        "panX, got {}",
        peek(&interp, px)
    );
    assert!(
        (peek(&interp, py) - 60.0).abs() < 1.0,
        "panY, got {}",
        peek(&interp, py)
    );
}

/// RFC-0005 windowed layout: a `windowed` ScrollView lays out only the
/// visible slice of a long uniform list, O(visible), not O(list), while a
/// plain ScrollView over the same list lays out every row.
#[test]
fn windowed_scrollview_lays_out_only_the_visible_window() {
    // 1000 rows × 20px in a 100px viewport. A windowed pass should build only
    // a handful of rows (viewport/row + overscan) + 2 spacers + containers.
    let list = |windowed: &str| {
        format!(
            "View V() {{ var y: Float = 0.0 \
                 ScrollView #[width: 200, height: 100, row_height: 20, {windowed} offset: (0, y)] {{ \
                     Column {{ \
                         for i in [{}] {{ \
                             Box #[bg: 0x6495ED, width: 180, height: 20] {{}} \
                         }} \
                     }} \
                 }} }}",
            (0..1000)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let node_count = |src: String| {
        let parsed = parse(&src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let tree = interp.lower_view(&parsed.views[0], &[]);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        (interp.atlas_node_count(), frame.instances().len())
    };
    let (windowed_nodes, windowed_boxes) = node_count(list("windowed: true,"));
    let (plain_nodes, _) = node_count(list(""));

    assert!(
        windowed_nodes < 40,
        "a windowed 1000-row list lays out O(visible), got {windowed_nodes} nodes"
    );
    assert!(
        plain_nodes > 1000,
        "a plain list lays out every row, got {plain_nodes} nodes"
    );
    assert!(
        windowed_boxes < 30,
        "only the visible rows are emitted, got {windowed_boxes}"
    );
}

/// RFC-0005 windowed layout: the two spacer leaves preserve the full content
/// extent, so the scroll clamp still reaches the true bottom of the list.
#[test]
fn windowed_scrollview_preserves_scroll_extent() {
    // 500 rows × 20 = 10 000 tall in a 100px viewport → max scroll 9 900.
    let src = format!(
        "View V() {{ var y: Float = 0.0 \
             ScrollView #[width: 200, height: 100, row_height: 20, windowed: true, offset: (0, y)] {{ \
                 Column {{ for i in [{}] {{ Box #[bg: 0x6495ED, width: 180, height: 20] {{}} }} }} \
             }} }}",
        (0..500)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let y = interp.var_signal(&Symbol::intern("y")).unwrap();
    // A huge wheel must clamp to content − viewport = 9 900, proving the
    // elided rows still count toward the extent (spacers, not shrinkage).
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::Wheel,
        pos: (100.0, 50.0),
        delta: (0.0, -10_000.0),
        payload: None,
        time_ms: 0,
    }]);
    let clamped = match interp.peek(y) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    assert!(
        (clamped - 9_900.0).abs() < 1.0,
        "windowed extent must span the whole list, clamped at {clamped}"
    );
}

/// RFC-0005 windowed layout: as the offset grows the window slides, so a row
/// deep in the list becomes visible while the atlas stays O(visible).
#[test]
fn windowed_scrollview_slides_the_window_on_scroll() {
    let src = format!(
        "View V() {{ var y: Float = 0.0 \
             ScrollView #[width: 200, height: 100, row_height: 20, windowed: true, offset: (0, y)] {{ \
                 Column {{ for i in [{}] {{ Box #[bg: 0x6495ED, width: 180, height: 20] {{}} }} }} \
             }} }}",
        (0..500)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    // The emitted rows' *layout* Y band (their true list positions, not the
    // scrolled screen position), and the atlas size, at the current offset.
    let sample = |interp: &mut Interpreter| -> (f32, f32, usize) {
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let ys: Vec<f32> = frame.instances().iter().map(|b| b.rect[1]).collect();
        let min = ys.iter().copied().fold(f32::INFINITY, f32::min);
        let max = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (min, max, interp.atlas_node_count())
    };
    let (_, at_rest_bottom, nodes0) = sample(&mut interp);
    assert!(
        at_rest_bottom < 300.0,
        "at rest the window sits at the top of the list, got bottom {at_rest_bottom}"
    );

    // Jump near the bottom (offset 8000 → row ~400 of 500). The window must
    // slide to the deep rows: they lay out at y ≈ 8000 (row_index × 20).
    let y = interp.var_signal(&Symbol::intern("y")).unwrap();
    interp.write_var(y, Value::Float(8000.0));
    interp.tick();
    let (deep_top, _, nodes1) = sample(&mut interp);
    assert!(
        deep_top > 7000.0,
        "the window slid to the deep rows (laid out near y≈8000), got top {deep_top}"
    );
    assert!(nodes0 < 40, "the window is O(visible) at rest: {nodes0}");
    assert!(
        nodes1 < 40,
        "the window stays O(visible) after scrolling deep: {nodes1}"
    );
}

/// RFC-0005 windowed layout regression: with uniform rows whose stride equals
/// `row_height`, the materialised rows must stay on an exact `row_height` grid
/// at every offset, including across a window-slide boundary. A spacer sized
/// off-grid would shift the whole content when `start` ticks (the "small
/// jumps" bug), so this pins the invariant that a scroll of 1px moves the
/// content by exactly 1px, never a row.
#[test]
fn windowed_rows_stay_on_an_exact_grid_across_slides() {
    // 500 rows laid out at exactly row_height (height 20, no gap → stride 20).
    let src = format!(
        "View V() {{ var y: Float = 0.0 \
             ScrollView #[width: 200, height: 100, windowed: true, row_height: 20, offset: (0, y)] {{ \
                 Column {{ for i in [{}] {{ Box #[bg: 0x6495ED, width: 180, height: 20] {{}} }} }} \
             }} }}",
        (0..500)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let y = interp.var_signal(&Symbol::intern("y")).unwrap();

    // Sweep offsets straddling several window-slide boundaries (start ticks
    // every 20px). At each, the emitted rows must be exactly 20px apart.
    for off in [0.0, 19.0, 20.0, 21.0, 79.0, 80.0, 81.0, 200.0, 205.0] {
        interp.write_var(y, Value::Float(off));
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let mut ys: Vec<f32> = frame.instances().iter().map(|b| b.rect[1]).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in ys.windows(2) {
            let stride = w[1] - w[0];
            assert!(
                (stride - 20.0).abs() < 0.01,
                "rows must stay on the 20px grid at offset {off}, got stride {stride} in {ys:?}"
            );
        }
    }
}

#[test]
fn oklab_hex_round_trips_within_one_lsb() {
    for hex in [
        0x00_0000_i64,
        0xFF_FFFF,
        0x64_95ED,
        0xEF_4444,
        0x10_B981,
        0x80_8080,
    ] {
        let back = hex_from_oklab(oklab_from_hex(hex));
        for shift in [16, 8, 0] {
            let a = (hex >> shift) & 0xFF;
            let b = (back >> shift) & 0xFF;
            assert!(
                (a - b).abs() <= 1,
                "channel drift for {hex:#08x}: {a} vs {b}"
            );
        }
    }
}

#[test]
fn with_animation_lerps_color_in_oklab_and_settles() {
    let parsed = parse(
        "View V() { var on: Bool = false \
             Box #[width: 10, height: 10, \
             bg: on ? 0x000000 : 0xFFFFFF with anim.linear(1000)] }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();

    let render_r = |interp: &mut Interpreter, now: u32| -> f32 {
        interp.set_now_ms(now);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.instances()[0].color[0]
    };

    // At rest the target is white; nothing is animating.
    assert!((render_r(&mut interp, 0) - 1.0).abs() < 1e-2);
    assert!(!interp.has_active_animations());

    // Flip toward black: starts near white and is active.
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    let start = render_r(&mut interp, 0);
    assert!(start > 0.9, "starts near white, got {start}");
    assert!(interp.has_active_animations());

    // Mid-flight it's a grey between the endpoints, still moving.
    let mid = render_r(&mut interp, 500);
    assert!((0.05..0.95).contains(&mid), "mid-flight grey, got {mid}");

    // Arrives at black and settles (idle again).
    assert!(render_r(&mut interp, 1000) < 1e-2, "arrives black");
    assert!(!interp.has_active_animations());
}

#[test]
fn animation_is_inert_until_the_clock_is_advanced() {
    // A host that never advances the clock must resolve the value to its
    // target and never mark it active, otherwise a wait-based runner would
    // spin forever redrawing a motion pinned at t=0.
    let parsed = parse(
        "View V() { var on: Bool = true \
             Box #[bg: 0x808080, width: 10, height: 10, \
             scale: on ? 2.0 : 1.0 with anim.spring()] }",
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (frame.instances()[0].transform.scale[0] - 2.0).abs() < 1e-6,
        "with no clock the value jumps straight to its target"
    );
    assert!(
        !interp.has_active_animations(),
        "an un-advanced clock must never leave an animation active"
    );
}

#[test]
fn opacity_dims_descendant_text_not_only_the_background() {
    // Regression: a translucent Button dims its *label* too, not just its
    // background, `opacity` folds into the alpha of every primitive the
    // element and its descendants emit.
    let parsed = parse(
        "View V() { var c: Int = 0 \
             Button(\"x\") #[bg: 0x6495ED, opacity: 0.4, width: 100, height: 44] => c++ }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let label = frame
        .texts()
        .iter()
        .find(|t| t.text == "x")
        .expect("the button's label was emitted");
    assert!(
        (label.color[3] - 0.4).abs() < 1e-3,
        "label alpha should inherit the 0.4 opacity, got {}",
        label.color[3]
    );
}

#[test]
fn style_value_spreads_onto_an_element_and_inline_overrides() {
    // RFC-0016: a `let`-bound style is spliced by `..`, and inline attrs win.
    let parsed = parse(
        "View V() { \
             let btn = style { bg: 0x112233, radius: 8 } \
             Box #[..btn, width: 10, height: 10] {} \
             Box #[..btn, bg: 0x445566, width: 10, height: 10] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let insts = frame.instances();
    // First box takes `bg` from the spread (0x11 red channel).
    assert!(
        (insts[0].color[0] - linear(0x11)).abs() < 1e-3,
        "spread bg reaches the box, got {:?}",
        insts[0].color
    );
    // Second box: inline `bg` overrides the spread (0x44 red channel).
    assert!(
        (insts[1].color[0] - linear(0x44)).abs() < 1e-3,
        "inline bg overrides the spread, got {:?}",
        insts[1].color
    );
}

#[test]
fn merge_composes_two_styles_right_wins() {
    // RFC-0016: `base merge overrides`, the right style wins on conflicts,
    // the left's non-conflicting attributes survive.
    let parsed = parse(
        "View V() { \
             let base = style { bg: 0x111111, radius: 8 } \
             let hot = base merge style { bg: 0x445566 } \
             Box #[..hot, width: 10, height: 10] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let inst = frame.instances()[0];
    // `bg` comes from the right side of the merge (0x44 red channel)…
    assert!(
        (inst.color[0] - linear(0x44)).abs() < 1e-3,
        "right style's bg wins, got {:?}",
        inst.color
    );
    // …while `radius` (only on the base) survives (radii != 0).
    assert!(inst.radii[0] > 0.0, "base radius survives the merge");
}

#[test]
fn parent_scale_is_inherited_by_child_text_and_boxes() {
    // RFC-0011 group transforms: a scaled container carries its descendants,
    // the reported bug was that a scaled parent's *text* stayed the same size.
    let parsed = parse(
        "View V() {\n Column #[scale: 2.0, width: 100, height: 100, bg: 0x111111] {\n \
             Text(\"hi\") #[size: 10]\n Box #[bg: 0x222222, width: 20, height: 20]\n }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // The child text's font size doubled with the parent scale (the fix).
    let line = frame
        .texts()
        .iter()
        .find(|t| t.text == "hi")
        .expect("the child text line is emitted");
    assert!(
        (line.font_size - 20.0).abs() < 1e-3,
        "child text scaled 2× with its parent (10 → 20), got {}",
        line.font_size
    );

    // Both boxes carry a 2× scale: the parent's own, and the child's inherited.
    for inst in frame.instances() {
        assert!(
            (inst.transform.scale[0] - 2.0).abs() < 1e-3
                && (inst.transform.scale[1] - 2.0).abs() < 1e-3,
            "every box in the group inherits the 2× scale, got {:?}",
            inst.transform.scale
        );
    }
}

#[test]
fn resolve_state_attrs_applies_specificity_then_declaration_order() {
    // RFC-0024 §2: single-state blocks apply in declaration order (equal
    // specificity → later wins); a combined `on hover+focused` (higher
    // specificity) beats both single-state blocks.
    use crate::interp::events::StyleState;
    let sp = crate::diagnostics::Span::new(0, 0);
    let prop = |name: &str, v: i64| Attr {
        name: Symbol::intern(name),
        axis: None,
        kind: AttrKind::Prop {
            value: Expr::IntLit(v, sp),
        },
        span: sp,
    };
    let block = |states: Vec<StyleStateKind>, v: i64| StateBlock {
        states,
        attrs: vec![prop("bg", v)],
        span: sp,
    };
    let base = vec![prop("bg", 1)];
    let blocks = vec![
        block(vec![StyleStateKind::Hover], 2),
        block(vec![StyleStateKind::Disabled], 3),
        block(vec![StyleStateKind::Hover, StyleStateKind::Focused], 4),
    ];

    // No state active → base survives, and the borrow is cheap (no clone).
    let none = resolve_state_attrs(&base, &blocks, StyleState::empty());
    assert!(matches!(none, std::borrow::Cow::Borrowed(_)));
    assert_eq!(find_int(&none, "bg"), Some(1));

    // Hover alone → the hover block overlays (the combined block needs focus).
    let hov = resolve_state_attrs(&base, &blocks, StyleState::HOVER);
    assert_eq!(find_int(&hov, "bg"), Some(2));

    // Hover + disabled (equal specificity) → disabled wins by declaration
    // order (declared after hover).
    let both = resolve_state_attrs(
        &base,
        &blocks,
        StyleState::HOVER.union(StyleState::DISABLED),
    );
    assert_eq!(find_int(&both, "bg"), Some(3));

    // Hover + focused → the combined `hover+focused` block (specificity 2)
    // beats the single-state `hover` block regardless of declaration order.
    let combined =
        resolve_state_attrs(&base, &blocks, StyleState::HOVER.union(StyleState::FOCUSED));
    assert_eq!(find_int(&combined, "bg"), Some(4));
}

#[test]
fn checkbox_on_checked_state_recolours_the_accent() {
    // RFC-0024: a checked value drives the `checked` state, so `on checked`
    // overlays reach the checkbox's own filled-accent visual.
    let src = "View C() {\n var c = true\n \
                   let chk = style { bg: 0x111111 on checked { bg: 0x00FF00 } }\n \
                   Checkbox #[..chk, bind: c, width: 20, height: 20]\n}";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // The filled square is the first decorated box; its accent is the
    // `on checked` green.
    let fill = frame.decorated()[0].base.color;
    assert!(
        fill[1] > 0.9 && fill[0] < 0.1,
        "checked accent is the `on checked` green, got {fill:?}"
    );
}

#[test]
fn universal_selected_prop_drives_the_selected_state() {
    // RFC-0024: `selected: true` on any element activates the `selected`
    // state, so `on selected { bg }` recolours it.
    let src = "View C() {\n \
                   let s = style { bg: 0x111111 on selected { bg: 0x00FF00 } }\n \
                   Box #[..s, selected: true, width: 20, height: 20] {}\n}";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let fill = frame.instances()[0].color;
    assert!(
        fill[1] > 0.9 && fill[0] < 0.1,
        "selected box uses the `on selected` green, got {fill:?}"
    );
}

fn find_int(attrs: &[Attr], name: &str) -> Option<i64> {
    attrs
        .iter()
        .find(|a| a.name == Symbol::intern(name))
        .and_then(|a| match &a.kind {
            AttrKind::Prop {
                value: Expr::IntLit(n, _),
            } => Some(*n),
            _ => None,
        })
}

#[test]
fn disabled_state_block_recolours_in_the_same_frame() {
    // A `disabled:` box with an `on disabled { bg }` block resolves the
    // DISABLED state on the very frame it renders (the router is marked
    // before state styles resolve), so the disabled bg wins immediately.
    let parsed = parse(
        "View V() { \
             let btn = style { bg: 0x111111 on disabled { bg: 0x445566 } } \
             Box #[..btn, disabled: true, width: 40, height: 20] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let inst = frame.instances()[0];
    assert!(
        (inst.color[0] - linear(0x44)).abs() < 1e-3,
        "disabled bg overlays the base, got {:?}",
        inst.color
    );
}

#[test]
fn hover_state_block_recolours_after_pointer_enters() {
    // RFC-0016: an `on hover { bg }` block lights up once the pointer moves
    // over the element, even though the element registers no handler of its
    // own (it is tracked as a bare hover region).
    let parsed = parse(
        "View V() { \
             let btn = style { bg: 0x111111 on hover { bg: 0x445566 } } \
             Box #[..btn, width: 40, height: 20] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    // First frame: pointer hasn't entered, base bg (0x11 red channel).
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (frame.instances()[0].color[0] - linear(0x11)).abs() < 1e-3,
        "base bg before hover, got {:?}",
        frame.instances()[0].color
    );

    // Move the pointer inside the box, then re-render.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerMove,
        pos: (10.0, 10.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    assert!(
        (frame2.instances()[0].color[0] - linear(0x44)).abs() < 1e-3,
        "hover bg overlays after the pointer enters, got {:?}",
        frame2.instances()[0].color
    );
}

#[test]
fn unknown_state_name_is_an_error_with_a_hint() {
    let parsed = parse("View V() { let s = style { bg: 1 on hoover { bg: 2 } } Box #[..s] {} }");
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| matches!(e, CompileError::UnknownStyleState { .. })),
        "an unknown state name must be an UnknownStyleState error, got {:?}",
        parsed.errors
    );
}

#[test]
fn spreading_a_non_style_is_an_error() {
    let parsed = parse("View V() { let x = 5 Box #[..x, width: 10, height: 10] {} }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(view, &[]);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::NotAStyle { .. })),
        "spreading a non-style must be a NotAStyle error, got {:?}",
        interp.errors()
    );
}

#[test]
fn no_transform_attrs_produces_identity() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50] }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // `origin` alone isn't checked against `Transform::IDENTITY`'s [0,0]:
    // the compiler defaults an unset `origin` to the element's own
    // center (RFC-0011's stated default), which is a real but *inert*
    // difference from the engine's raw identity, pivot is irrelevant
    // when scale = 1 and rotate = 0, so the render is pixel-identical.
    let t = frame.instances()[0].transform;
    assert_eq!(t.translate, [0.0, 0.0]);
    assert_eq!(t.scale, [1.0, 1.0]);
    assert_eq!(t.rotate, 0.0);
    assert_eq!(t.opacity, 1.0);
}

#[test]
fn sub_property_axis_sets_one_axis_and_leaves_the_other_default() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50, translate.y: 7] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(frame.instances()[0].transform.translate, [0.0, 7.0]);
}

#[test]
fn named_tuple_scale_sets_one_axis_and_leaves_the_other_at_one() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50, scale: (y: 2.0)] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(frame.instances()[0].transform.scale, [1.0, 2.0]);
}

#[test]
fn origin_token_resolves_relative_to_the_laid_out_rect() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 40, height: 20, origin: top_left] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    // The Box lays out at the view's origin (0,0) by default.
    assert_eq!(frame.instances()[0].transform.origin, [0.0, 0.0]);
}

#[test]
fn unknown_origin_token_is_a_compile_error_with_a_hint() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50, origin: centre] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(matches!(
        &interp.errors()[0],
        CompileError::UnknownAttribute { hint: Some(h), .. } if h.contains("center")
    ));
}

// ── M16: Toggle/Slider/TextField write-back ──────────────────────────

#[test]
fn toggle_with_bg_has_no_background_slab() {
    // Regression: `bg` on a Toggle is the ON accent, not a full-rect fill
    // painted behind the control (that stray slab made widgets look "off").
    let parsed = parse(
        "View C() {\n var on = true\n Toggle #[bind: on, bg: 0x10B981, width: 52, height: 30]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // Exactly track + thumb, no extra background rectangle, no DecoratedBox.
    assert_eq!(
        frame.instances().len(),
        2,
        "toggle should emit only track + thumb"
    );
    assert_eq!(frame.decorated().len(), 0);
}

#[test]
fn slider_with_bg_has_no_background_slab() {
    // Regression: `bg` on a Slider is the fill accent, not a full-rect fill.
    let parsed = parse(
        "View C() {\n var v = 0.5\n Slider #[bind: v, bg: 0xEF4444, min: 0.0, max: 1.0, width: 200, height: 24]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // track + fill + thumb(accent disc) + thumb(white inner) = 4; no slab.
    assert_eq!(
        frame.instances().len(),
        4,
        "slider should emit track + fill + thumb (2 discs), no slab"
    );
}

#[test]
fn toggle_tap_flips_bound_var() {
    let parsed = parse("View C() {\n var on = false\n Toggle #[bind: on]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("on"))
        .unwrap();
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(false));

    // Simulate a render so handlers are registered.
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap inside the Toggle rect.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Bool(true),
        "toggle flipped to true"
    );

    // Second tap flips back, gap > DOUBLE_TAP_MS (300ms) so it's a plain tap.
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 400,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 450,
        },
    ]);
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(false), "toggle flipped back");
}

// ── RFC-0018: Checkbox ────────────────────────────────────────────────

/// Renders `src`'s first view and returns `(instances, decorated)` counts,
/// plus the first decorated box, for the Checkbox visual tests.
fn checkbox_frame(src: &str) -> byard_core::frame::RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
}

#[test]
fn unchecked_checkbox_is_a_single_borderless_slot() {
    // Unchecked with no styled border: one decorated slot, no mark. (The
    // container is a DecoratedBox so it *can* carry a border when styled; with
    // none, it's a borderless muted fill.)
    let frame = checkbox_frame("View C() {\n var c = false\n Checkbox #[bind: c]\n}");
    assert_eq!(frame.instances().len(), 0, "no solid instances");
    assert_eq!(frame.decorated().len(), 1, "just the muted slot, no mark");
    let slot = frame.decorated()[0];
    assert!(
        slot.base.color[3] > 0.0,
        "the slot has an opaque muted fill"
    );
    assert_eq!(slot.border_width, 0.0, "no border when none is styled");
}

#[test]
fn checked_checkbox_fills_and_draws_a_two_stroke_check() {
    // Checked: a filled accent square + two checkmark stroke quads on top,
    // three decorated boxes (all on the decorated pipeline, in push order, so
    // the check is never hidden behind the fill).
    let frame = checkbox_frame("View C() {\n var c = true\n Checkbox #[bind: c]\n}");
    assert_eq!(frame.instances().len(), 0, "no solid instances");
    assert_eq!(
        frame.decorated().len(),
        3,
        "filled square + two rotated stroke quads"
    );
    assert!(
        frame.decorated()[0].base.color[3] > 0.0,
        "the checked square has an opaque accent fill"
    );
    // The two strokes rotate in opposite senses about their midpoints, proof
    // the mark is angled geometry, not two axis-aligned bars.
    let r1 = frame.decorated()[1].base.transform.rotate;
    let r2 = frame.decorated()[2].base.transform.rotate;
    assert!(
        (r1 - r2).abs() > 0.1,
        "the two strokes are at different angles"
    );
}

#[test]
fn indeterminate_checkbox_fills_and_draws_a_single_dash() {
    // Mixed state: filled square + one horizontal bar, no checkmark.
    let frame =
        checkbox_frame("View C() {\n var c = false\n Checkbox #[bind: c, indeterminate: true]\n}");
    assert_eq!(frame.instances().len(), 0, "no solid instances");
    assert_eq!(frame.decorated().len(), 2, "filled square + one dash");
    assert!(
        frame.decorated()[0].base.color[3] > 0.0,
        "filled accent square"
    );
}

#[test]
fn checkbox_bg_is_the_accent_not_a_full_rect_slab() {
    // Regression parity with Toggle/Slider: `bg` on a Checkbox is the checked
    // accent (the box fill), never a background slab, a checked box is the
    // filled square plus the two mark strokes, nothing more.
    let frame = checkbox_frame(
        "View C() {\n var c = true\n Checkbox #[bind: c, bg: 0x10B981, width: 24, height: 24]\n}",
    );
    assert_eq!(
        frame.decorated().len(),
        3,
        "filled square + the two checkmark strokes"
    );
    let fill = frame.decorated()[0].base.color;
    let accent = crate::interp::intrinsics::color_to_rgba(0x0010_B981, false);
    assert!(
        (fill[0] - accent[0]).abs() < 0.01 && (fill[1] - accent[1]).abs() < 0.01,
        "the filled square carries the `bg` accent"
    );
}

#[test]
fn checkbox_tap_flips_bound_var() {
    let parsed = parse("View C() {\n var c = false\n Checkbox #[bind: c]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("c"))
        .unwrap();
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(false));

    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
        },
    ]);
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(true), "tap checked the box");
}

#[test]
fn checkbox_space_key_toggles_when_focused() {
    let parsed = parse("View C() {\n var c = false\n Checkbox #[bind: c]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("c"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // Click to focus the box, then press Space.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::KeyDown,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload: Some(byard_core::platform::InputPayload::Key(" ".to_string())),
            time_ms: 10,
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Bool(true),
        "Space toggled the focused checkbox"
    );
}

// ── RFC-0018: RadioButton ─────────────────────────────────────────────

#[test]
fn unselected_radio_is_a_ring_only() {
    // `choice != value` → the ring only, no inner dot.
    let frame = checkbox_frame(
        "View C() {\n var choice = \"work\"\n RadioButton #[value: \"home\", bind: choice]\n}",
    );
    assert_eq!(frame.instances().len(), 0, "no inner dot when unselected");
    assert_eq!(frame.decorated().len(), 1, "just the outer ring");
    assert!(
        frame.decorated()[0].border_width > 0.0,
        "the ring is a bordered decorated box"
    );
    assert_eq!(
        frame.decorated()[0].base.color[3],
        0.0,
        "the ring interior is transparent"
    );
}

#[test]
fn selected_radio_draws_ring_plus_inner_dot() {
    // `choice == value` → the ring plus a filled inner dot.
    let frame = checkbox_frame(
        "View C() {\n var choice = \"home\"\n RadioButton #[value: \"home\", bind: choice]\n}",
    );
    assert_eq!(frame.decorated().len(), 1, "the outer ring");
    assert_eq!(frame.instances().len(), 1, "the inner dot");
    assert!(
        frame.instances()[0].color[3] > 0.0,
        "the dot has an opaque accent fill"
    );
}

#[test]
fn radio_tap_selects_its_value() {
    let parsed = parse(
        "View C() {\n var choice = \"home\"\n RadioButton #[value: \"work\", bind: choice]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("choice"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Str("work".to_string()),
        "tapping the radio wrote its value to the group var"
    );
}

#[test]
fn radio_group_is_mutually_exclusive_via_the_shared_var() {
    // Two radios on one `var`: tapping the second selects it, which
    // deselects the first (they read the same var, no explicit exclusion).
    let parsed = parse(
        "View C() {\n var choice = \"home\"\n \
             Column #[gap: 40] {\n \
               RadioButton #[value: \"home\", bind: choice, width: 44, height: 44]\n \
               RadioButton #[value: \"work\", bind: choice, width: 44, height: 44]\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("choice"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // Tap the SECOND radio (below the first, gap 40 keeps hit rects apart).
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (20.0, 106.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (20.0, 106.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Str("work".to_string()),
        "the group var now holds the second radio's value"
    );
}

#[test]
fn radio_arrow_keys_move_selection_with_wrap() {
    let parsed = parse(
        "View C() {\n var choice = \"home\"\n \
             Column #[gap: 40] {\n \
               RadioButton #[value: \"home\", bind: choice, width: 44, height: 44]\n \
               RadioButton #[value: \"work\", bind: choice, width: 44, height: 44]\n \
               RadioButton #[value: \"other\", bind: choice, width: 44, height: 44]\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("choice"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();

    // Focus the FIRST radio with a press (no release → no tap, so `choice`
    // stays "home").
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (20.0, 20.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Str("home".to_string()));

    // A helper: re-render (repopulates the group ordering + handlers), press
    // the arrow, tick, and read back the group var.
    let press = |interp: &mut Interpreter, key: &str| -> String {
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 400.0, 300.0);
        interp.dispatch_events(&[byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::KeyDown,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload: Some(byard_core::platform::InputPayload::Key(key.to_string())),
            time_ms: 10,
        }]);
        interp.tick();
        match interp.peek(sig) {
            Value::Str(s) => s,
            other => panic!("expected Str, got {other:?}"),
        }
    };

    assert_eq!(press(&mut interp, "ArrowDown"), "work", "home → work");
    assert_eq!(press(&mut interp, "ArrowDown"), "other", "work → other");
    assert_eq!(
        press(&mut interp, "ArrowDown"),
        "home",
        "other → home (forward wrap)"
    );
    assert_eq!(
        press(&mut interp, "ArrowUp"),
        "other",
        "home → other (backward wrap)"
    );
}

// ── RFC-0018: Grid ────────────────────────────────────────────────────

#[test]
fn grid_auto_places_children_into_columns() {
    // Two 1fr columns in a 200px-wide grid → 100px each; two auto-flowed
    // children land one per column (x≈0 and x≈100).
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr 1fr\", width: 200, height: 100] {\n \
               Box #[bg: 0xFF0000, height: 40] {}\n \
               Box #[bg: 0x00FF00, height: 40] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2, "two child fills");
    let mut xs: Vec<f32> = frame.instances().iter().map(|i| i.rect[0]).collect();
    xs.sort_by(f32::total_cmp);
    assert!(xs[0] < 5.0, "first column near x=0, got {xs:?}");
    assert!(
        (xs[1] - 100.0).abs() < 5.0,
        "second column near x=100, got {xs:?}"
    );
}

#[test]
fn grid_explicit_col_placement_pins_a_child() {
    // A lone child pinned to column 2 sits in the right column (x≈100),
    // proving `set_grid_item` wires `col:` through to Taffy.
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr 1fr\", width: 200, height: 100] {\n \
               Box #[bg: 0xFF0000, height: 40, col: 2] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 1, "one child fill");
    assert!(
        (frame.instances()[0].rect[0] - 100.0).abs() < 5.0,
        "pinned to column 2 (x≈100), got {:?}",
        frame.instances()[0].rect
    );
}

#[test]
fn grid_row_gap_offsets_the_second_row() {
    // One 1fr column, two explicit 40px rows, row_gap 20 → the second row
    // starts at y = 40 + 20 = 60. (Explicit row tracks so the assertion is
    // independent of grid `align-content`, which stretches *auto* rows to
    // fill a fixed-height container.)
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr\", rows: \"40 40\", row_gap: 20, width: 100] {\n \
               Box #[bg: 0xFF0000] {}\n \
               Box #[bg: 0x00FF00] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2);
    let mut ys: Vec<f32> = frame.instances().iter().map(|i| i.rect[1]).collect();
    ys.sort_by(f32::total_cmp);
    assert!(ys[0] < 5.0, "first row at top, got {ys:?}");
    assert!(
        (ys[1] - 60.0).abs() < 5.0,
        "second row after 40px row + 20px gap, got {ys:?}"
    );
}

#[test]
fn grid_invalid_template_reports_a_diagnostic_and_still_renders() {
    let parsed = parse("View C() {\n Grid #[columns: \"1fr bogus\"] { Box #[bg: 0xFF0000] {} }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::InvalidGridTemplate { .. })),
        "expected InvalidGridTemplate, got {:?}",
        interp.errors()
    );
    // Non-fatal: the grid still lays out (falls back to a single auto column).
    assert_eq!(frame.instances().len(), 1, "the child still renders");
}

// ── RFC-0018: ZStack ──────────────────────────────────────────────────

#[test]
fn zstack_overlaps_children_at_the_same_origin() {
    // Two children with bg: the small one centres over the big one (they
    // overlap), unlike a Column which would stack them vertically.
    let frame = checkbox_frame(
        "View C() {\n ZStack #[width: 100, height: 100] {\n \
               Box #[bg: 0xFF0000, width: 100, height: 100] {}\n \
               Box #[bg: 0x00FF00, width: 40, height: 40] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2, "two child fills");
    // Declaration order: big first (bottom), small second (on top).
    let big = frame.instances()[0].rect;
    let small = frame.instances()[1].rect;
    assert!(
        big[0] < 5.0 && big[1] < 5.0,
        "big child at origin, got {big:?}"
    );
    // Small (40) centred in the 100 stack → (100 − 40) / 2 = 30.
    assert!(
        (small[0] - 30.0).abs() < 5.0 && (small[1] - 30.0).abs() < 5.0,
        "small child centred, got {small:?}"
    );
}

#[test]
fn zstack_alignment_pins_child_to_corner() {
    // `bottom_end` puts the small child at the bottom-right corner.
    let frame = checkbox_frame(
        "View C() {\n ZStack #[width: 100, height: 100, alignment: bottom_end] {\n \
               Box #[bg: 0xFF0000, width: 100, height: 100] {}\n \
               Box #[bg: 0x00FF00, width: 20, height: 20] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2);
    let small = frame.instances()[1].rect;
    assert!(
        (small[0] - 80.0).abs() < 5.0 && (small[1] - 80.0).abs() < 5.0,
        "small child at bottom-right (80,80), got {small:?}"
    );
}

// ── RFC-0005: default text wrap ───────────────────────────────────────

#[test]
fn text_wraps_to_parent_width_by_default() {
    // A long line in a narrow fixed-width column wraps with NO explicit
    // `wrap`/`width`, default wrap reflows it to the column's width.
    let parsed = parse(
        "View C() {\n Column #[width: 120] {\n \
               Text(\"This is a fairly long sentence that must wrap within a narrow column.\")\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 1, "one text run");
    let wrap = frame.text_wraps()[0];
    assert!(wrap.is_some(), "default wrap is on");
    assert!(
        wrap.unwrap() <= 130.0,
        "wrapped to ~the 120px column (not the natural width), got {wrap:?}"
    );
}

#[test]
fn wrap_false_opts_out_to_a_single_line() {
    let parsed = parse(
        "View C() {\n Column #[width: 120] {\n \
               Text(\"A long unwrapped line that overflows the column.\") #[wrap: false]\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 1);
    assert!(
        frame.text_wraps()[0].is_none(),
        "wrap: false → single line (no wrap width), got {:?}",
        frame.text_wraps()[0]
    );
}

#[test]
fn slider_drag_sets_float_value() {
    let parsed =
        parse("View C() {\n var vol = 0.0\n Slider #[bind: vol, min: 0, max: 1, width: 100]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("vol"))
        .unwrap();
    interp.tick();

    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // PointerDown at ~50% of track (x=50 on a 100px track starting at x=0).
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (50.0, 5.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    interp.tick();

    let val = match interp.peek(sig) {
        Value::Float(f) => f,
        other => panic!("expected Float, got {other:?}"),
    };
    assert!(
        (val - 0.5).abs() < 0.1,
        "slider at 50% should be ~0.5, got {val}"
    );
}

#[test]
fn slider_value_has_no_f32_widening_tail() {
    // Regression: a drag landing on 0.6 used to be stored as
    // `f64::from(0.6_f32)` = 0.6000000238418579 because the value math ran
    // in f32 and was only widened at the end. The value path now stays in
    // f64, so a pixel-aligned 60% drag round-trips to a clean "0.6".
    let parsed =
        parse("View C() {\n var vol = 0.0\n Slider #[bind: vol, min: 0, max: 1, width: 100]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("vol"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // x = 60 on a 100px track starting at x = 0 → exactly 60%.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (60.0, 5.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    interp.tick();

    let val = match interp.peek(sig) {
        Value::Float(f) => f,
        other => panic!("expected Float, got {other:?}"),
    };
    assert_eq!(
        format!("{val}"),
        "0.6",
        "slider value must not carry an f32 widening tail"
    );
}

#[test]
fn slider_with_step_does_not_emit_more_decimals_than_the_step() {
    // step: 0.1 landing on 60% used to store 6 * 0.1 = 0.6000000000000001.
    // The value is now rounded to the step's precision → a clean "0.6".
    let parsed = parse(
        "View C() {\n var vol = 0.0\n Slider #[bind: vol, min: 0, max: 1, step: 0.1, width: 100]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("vol"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (60.0, 5.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    }]);
    interp.tick();

    let val = match interp.peek(sig) {
        Value::Float(f) => f,
        other => panic!("expected Float, got {other:?}"),
    };
    assert_eq!(format!("{val}"), "0.6");
}

#[test]
fn text_field_change_event_round_trips() {
    let parsed = parse("View C() {\n var query = \"\"\n TextField #[bind: query]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("query"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Change event with new value.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::Change,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Str("hello".to_string())),
        time_ms: 0,
    }]);
    assert_eq!(interp.peek(sig), Value::Str("hello".to_string()));

    // Re-delivering the same value is deduped (E1).
    let before = interp.peek(sig);
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::Change,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Str("hello".to_string())),
        time_ms: 1,
    }]);
    assert_eq!(interp.peek(sig), before, "equal value deduped");
}

#[test]
fn bind_to_non_var_produces_no_bound_sig() {
    // `let y = 0` is not a var → resolve_bind_sig returns None.
    let parsed = parse("View C() {\n let y = 0\n Toggle #[bind: y]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    // No error expected at lowering; just bound_sig is None (non-var silently ignored).
    let RenderNode::Box { bound_sig, .. } = &tree[0] else {
        panic!("expected Box");
    };
    assert!(bound_sig.is_none(), "let binding yields no bound_sig");
}

// ── M17: Keyboard delivery ───────────────────────────────────────────

#[test]
fn text_field_receives_keyboard_text_input() {
    let parsed2 = parse("View C() {\n var text = \"\"\n TextField #[bind: text]\n}");
    assert!(parsed2.errors.is_empty(), "{:?}", parsed2.errors);
    let view2 = &parsed2.views[0];
    let mut interp2 = Interpreter::new();
    let tree2 = interp2.lower_view(view2, &[]);
    let sig = interp2
        .var_signal(&crate::symbol::Symbol::intern("text"))
        .unwrap();
    interp2.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp2.render(&tree2, &mut frame, 400.0, 300.0);

    // Focus the TextField by tapping it first.
    interp2.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
        },
    ]);
    interp2.tick();
    interp2.render(&tree2, &mut frame, 400.0, 300.0);

    // Type "ab".
    interp2.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::TextInput,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("a".to_string())),
        time_ms: 100,
    }]);
    interp2.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::TextInput,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("b".to_string())),
        time_ms: 200,
    }]);
    interp2.tick();
    assert_eq!(
        interp2.peek(sig),
        Value::Str("ab".to_string()),
        "typed 'ab'"
    );

    // Backspace removes last char.
    interp2.render(&tree2, &mut frame, 400.0, 300.0);
    interp2.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key(
            "Backspace".to_string(),
        )),
        time_ms: 300,
    }]);
    interp2.tick();
    assert_eq!(
        interp2.peek(sig),
        Value::Str("a".to_string()),
        "backspace deleted 'b'"
    );
}

// ── M18: Tab focus traversal ─────────────────────────────────────────

#[test]
fn tab_key_advances_focus_through_text_fields() {
    // Two TextFields, Tab should cycle between them.
    let parsed = parse(
        "View C() {\n var fa = false\n var fb = false\n TextField #[bind: fa, focused: fa]\n TextField #[bind: fb, focused: fb]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let fa = interp
        .var_signal(&crate::symbol::Symbol::intern("fa"))
        .unwrap();
    let fb = interp
        .var_signal(&crate::symbol::Symbol::intern("fb"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tab: should focus the first field (none focused yet → index 0).
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("Tab".to_string())),
        time_ms: 0,
    }]);
    interp.tick();
    assert_eq!(
        interp.peek(fa),
        Value::Bool(true),
        "first field focused after Tab"
    );
    assert_eq!(interp.peek(fb), Value::Bool(false));

    // Second Tab: advances to second field.
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("Tab".to_string())),
        time_ms: 100,
    }]);
    interp.tick();
    assert_eq!(interp.peek(fa), Value::Bool(false), "first field blurred");
    assert_eq!(interp.peek(fb), Value::Bool(true), "second field focused");
}

// ── M20: Structural for/when in render tree ──────────────────────────

#[test]
fn when_true_includes_then_branch() {
    // RFC-0018: `when` lowers to one reactive `When` node; its taken branch is
    // expanded at paint time. With the condition true, the branch paints.
    let parsed = parse("View C() {\n var show = true\n when show { Text(\"visible\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(tree.len(), 1);
    assert!(
        matches!(tree[0], RenderNode::When { .. }),
        "when → When node"
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts().len(), 1, "then-branch paints when true");
    assert_eq!(frame.texts()[0].text, "visible");
}

#[test]
fn when_false_emits_nothing_without_else() {
    let parsed = parse("View C() {\n var hide = false\n when hide { Text(\"hidden\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(frame.texts().is_empty(), "false, no else → nothing paints");
}

#[test]
fn when_reacts_to_a_var_flip_at_runtime() {
    // RFC-0018: the whole point, flipping the guard `var` mounts/unmounts the
    // subtree at runtime, with no re-lowering.
    let parsed = parse(
        "View C() {\n var show = false\n when show { Text(\"hi\") } else { Text(\"bye\") }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let show = interp
        .var_signal(&crate::symbol::Symbol::intern("show"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts()[0].text, "bye", "else branch first");

    interp.write_var(show, Value::Bool(true));
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    assert_eq!(frame2.texts()[0].text, "hi", "then branch after flip");
}

#[test]
fn for_loop_emits_one_node_per_item() {
    // RFC-0018: `for` lowers to one reactive `For` node; the driver renders one
    // pooled body per element at paint time.
    let parsed =
        parse("View C() {\n var items = [1, 2, 3]\n for item in items { Text(\"{item}\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!(matches!(tree[0], RenderNode::For { .. }), "for → For node");
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let texts: Vec<&str> = frame.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["1", "2", "3"], "one node per item, in order");
}

#[test]
fn for_reacts_to_list_growth_and_element_change() {
    // RFC-0018: growing the list mounts more rows; changing an element updates
    // its row, all without re-lowering.
    let parsed = parse("View C() {\n var xs = [10, 20]\n for x in xs { Text(\"{x}\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let xs = interp
        .var_signal(&crate::symbol::Symbol::intern("xs"))
        .unwrap();
    interp.tick();
    let mut f = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f, 400.0, 300.0);
    let t: Vec<&str> = f.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t, ["10", "20"]);

    // Grow + change: [10, 20] → [10, 99, 30].
    interp.write_var(
        xs,
        Value::List(vec![Value::Int(10), Value::Int(99), Value::Int(30)]),
    );
    interp.tick();
    let mut f2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f2, 400.0, 300.0);
    let t2: Vec<&str> = f2.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t2, ["10", "99", "30"], "grew to 3 rows, element updated");

    // Shrink: [10, 99, 30] → [7].
    interp.write_var(xs, Value::List(vec![Value::Int(7)]));
    interp.tick();
    let mut f3 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f3, 400.0, 300.0);
    let t3: Vec<&str> = f3.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t3, ["7"], "shrank to 1 row");
}

// ── M23: Controller boundary ─────────────────────────────────────────

#[test]
fn inject_provider_is_visible_to_view() {
    let parsed = parse("View C() {\n inject AppEnv as env\n Text(\"{env}\")\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    // Provide the ambient value before lowering.
    interp.inject_provider("AppEnv", Value::Str("prod".to_string()));
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // The Text should contain the injected value.
    assert_eq!(frame.texts()[0].text, "prod");
}

#[test]
fn apply_io_callbacks_writes_to_var_and_ticks() {
    let parsed = parse("View C() {\n var data = \"\"\n Text(\"{data}\")\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let _tree = interp.lower_view(view, &[]);
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("data"))
        .unwrap();
    interp.tick();

    // Simulate an async I/O result writing to the `data` var.
    interp.apply_io_callbacks([Box::new(move |interp: &mut Interpreter| {
        interp.write_var(sig, Value::Str("loaded".to_string()));
    }) as Box<dyn FnOnce(&mut Interpreter) + Send>]);
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Str("loaded".to_string()));
}

// ── M25: Parameterized fn call sites ─────────────────────────────────

#[test]
fn parameterized_fn_call_binds_args() {
    // fn identity(n: Int) => n  →  let y = identity(42)  →  Text renders "42"
    let src = "View C() {\n fn identity(n: Int) => n\n var x = 42\n let y = identity(x)\n Text(\"{y}\")\n}";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts()[0].text, "42", "identity(42) == 42");
}

#[test]
fn parameterized_fn_reacts_to_signal_arg() {
    // fn greet(name: Str) => "Hi {name}"  →  reactive on `greeting` signal
    let src = "View C() {\n fn greet(name: Str) => \"Hi {name}\"\n var greeting = \"Alice\"\n let msg = greet(greeting)\n Text(\"{msg}\")\n}";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("greeting"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts()[0].text, "Hi Alice");

    // Change greeting → "Bob": msg should become "Hi Bob".
    interp.write_var(sig, Value::Str("Bob".into()));
    interp.tick();
    frame.clear();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(
        frame.texts()[0].text,
        "Hi Bob",
        "greet reacts to signal change"
    );
}

// ── M21: DecoratedBox / TextureSampler ───────────────────────────────

#[test]
fn image_lowers_to_texture_sampler_in_frame() {
    let parsed =
        parse("View C() {\n Image(\"photo.jpg\") #[fit: \"cover\", width: 200, height: 150]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!(
        matches!(tree[0], RenderNode::Image { .. }),
        "Image element lowers to RenderNode::Image"
    );

    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.textures().len(), 1, "one TextureSampler in frame");
    let tex = &frame.textures()[0];
    assert_eq!(tex.src, "photo.jpg");
    assert_eq!(tex.fit, byard_core::frame::ImageFit::Cover);
}

#[test]
fn image_fit_defaults_to_fill() {
    let parsed = parse("View C() {\n Image(\"img.png\")\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.textures()[0].fit, byard_core::frame::ImageFit::Fill);
}

#[test]
fn box_with_border_becomes_decorated_box() {
    // `border` is the catalog Color attr; it yields a 2px ring.
    let parsed = parse("View C() {\n Box #[bg: 0xffffff, border: 0x000000]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // A bordered container splits into an opaque SolidBox fill
    // (so it stays behind its children, which also paint as solids) plus a
    // decorated *overlay* whose interior is transparent and only strokes the
    // 2px border, it can't occlude the children drawn beneath it.
    assert_eq!(
        frame.instances().len(),
        1,
        "opaque fill on the SolidBox pass"
    );
    assert_eq!(
        frame.instances()[0].color,
        [1.0, 1.0, 1.0, 1.0],
        "the fill carries the bg colour"
    );
    assert_eq!(frame.decorated().len(), 1, "border overlay → DecoratedBox");
    assert!((frame.decorated()[0].border_width - 2.0).abs() < f32::EPSILON);
    assert_eq!(
        frame.decorated()[0].base.color,
        [0.0; 4],
        "the overlay interior is transparent so children stay visible"
    );
}

#[test]
fn bordered_container_paints_fill_before_its_child_widget() {
    // The regression behind the "widgets invisible" report: an opaque,
    // bordered card must NOT paint over the solid boxes of the widgets it
    // contains. The card's fill is a SolidBox pushed *before*
    // the child's, and the only decorated primitive is a transparent-interior
    // border overlay, so the child's fill is never occluded.
    let parsed = parse(
        "View C() {\n Column #[bg: 0x222233, border: 0x445566] {\n Box #[bg: 0xFF0000, width: 20, height: 20]\n }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Two solid fills: the card, then the child, in that paint order, so
    // the child (drawn second) lands on top of the card, not under it.
    assert_eq!(frame.instances().len(), 2, "card fill + child fill");
    assert_ne!(
        frame.instances()[0].color,
        [1.0, 0.0, 0.0, 1.0],
        "the first solid fill is the card, not the child"
    );
    assert_eq!(
        frame.instances()[1].color,
        [1.0, 0.0, 0.0, 1.0],
        "the child's red fill paints last (on top)"
    );
    // Every decorated primitive in this frame is a transparent-interior
    // overlay, so nothing opaque is layered above the child.
    assert!(
        frame.decorated().iter().all(|d| d.base.color[3] == 0.0),
        "all decorated overlays have transparent interiors"
    );
}

#[test]
fn box_with_shadow_token_becomes_decorated_box() {
    let parsed = parse("View C() {\n Box #[bg: 0x222222, shadow: \"md\"]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.decorated().len(), 1, "shadowed box → DecoratedBox");
    assert!(frame.decorated()[0].shadow_blur > 0.0);
    assert!(
        frame.decorated()[0].shadow_color[3] > 0.0,
        "shadow is translucent"
    );
}

/// Renders `src`'s first view and returns the frame's decorated boxes'
/// shadow triples `(dy, blur, spread)`, for the custom-shadow tests below.
fn shadow_params(src: &str) -> Vec<(f32, f32, f32)> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
        .decorated()
        .iter()
        .map(|d| (d.shadow_dy, d.shadow_blur, d.shadow_spread))
        .collect()
}

#[test]
fn named_custom_shadow_sets_offset_blur_and_spread() {
    let got = shadow_params(
        "View C() { Box #[bg: 0x222222, shadow: (y: 6, blur: 12, spread: 3, color: 0x80000000)] {} }",
    );
    assert_eq!(got.len(), 1, "one shadow instance beneath the fill");
    let (dy, blur, spread) = got[0];
    assert!((dy - 6.0).abs() < 0.01, "dy={dy}");
    assert!((blur - 12.0).abs() < 0.01, "blur={blur}");
    assert!((spread - 3.0).abs() < 0.01, "spread={spread}");
}

#[test]
fn positional_shadow_maps_x_y_blur_spread_color_by_slot() {
    let got =
        shadow_params("View C() { Box #[bg: 0x222222, shadow: (0, 4, 8, 2, 0x80000000)] {} }");
    assert_eq!(got.len(), 1);
    let (dy, blur, spread) = got[0];
    assert!((dy - 4.0).abs() < 0.01 && (blur - 8.0).abs() < 0.01 && (spread - 2.0).abs() < 0.01);
}

#[test]
fn layered_shadows_emit_one_instance_each() {
    let got = shadow_params(
        "View C() { Box #[bg: 0x222222, shadow: [(y: 2, blur: 4), (y: 8, blur: 16)]] {} }",
    );
    assert_eq!(got.len(), 2, "two layered shadows → two instances");
    let mut blurs: Vec<f32> = got.iter().map(|s| s.1).collect();
    blurs.sort_by(f32::total_cmp);
    assert!((blurs[0] - 4.0).abs() < 0.01 && (blurs[1] - 16.0).abs() < 0.01);
}

#[test]
fn shadow_none_and_absent_emit_no_shadow() {
    assert!(shadow_params("View C() { Box #[bg: 0x222222] {} }").is_empty());
    assert!(shadow_params("View C() { Box #[bg: 0x222222, shadow: \"none\"] {} }").is_empty());
}

// ── M22: Theme system ────────────────────────────────────────────────

#[test]
fn text_without_color_uses_theme_on_surface() {
    let parsed = parse("View C() {\n Text(\"hi\")\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let expected_color = crate::interp::intrinsics::color_to_rgba(interp.theme.on_surface(), false);
    assert_eq!(
        frame.texts()[0].color,
        expected_color,
        "no-color Text gets theme on_surface"
    );
}

#[test]
fn typo_token_resolves_to_concrete_size() {
    let parsed = parse("View C() {\n Text(\"hi\") #[typo: \"titleLarge\"]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert!(
        (frame.texts()[0].font_size - 22.0).abs() < f32::EPSILON,
        "titleLarge → 22pt, got {}",
        frame.texts()[0].font_size
    );
}

// ── RFC-0022: theme runtime (injected reactive tokens) ────────────────

/// Lowers `src`'s first view against `theme` (installed as the ambient
/// `Theme`, RFC-0022), ticks, and renders one frame.
fn theme_render(interp: &mut Interpreter, src: &str) -> byard_core::frame::RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
}

#[test]
fn injected_theme_color_token_paints_and_flips_with_scheme() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let src = "View C() {\n inject Theme as t\n Column #[bg: t.primary] {}\n}";

    let frame = theme_render(&mut interp, src);
    let light = frame.instances()[0].color;
    let expected_light = crate::interp::intrinsics::color_to_rgba(
        interp.theme.color("primary", false).unwrap(),
        false,
    );
    assert_eq!(
        light, expected_light,
        "light scheme paints the light primary"
    );

    // Flip the scheme, a single reactive write, and re-render.
    interp.set_theme_dark(true);
    let mut frame2 = byard_core::frame::RenderFrame::new();
    // Re-lower against the same env so the injected `t` still resolves.
    let tree = interp.lower_view(&parse(src).views[0], &[]);
    interp.tick();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    let dark = frame2.instances()[0].color;
    let expected_dark = crate::interp::intrinsics::color_to_rgba(
        interp.theme.color("primary", true).unwrap(),
        false,
    );
    assert_eq!(dark, expected_dark, "dark scheme paints the dark primary");
    assert_ne!(light, dark, "flipping the scheme recolours the box");
}

#[test]
fn theme_typo_accessor_sets_font_size() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Text(\"hi\") #[typo: t.titleLarge]\n}",
    );
    assert!(
        (frame.texts()[0].font_size - 22.0).abs() < f32::EPSILON,
        "t.titleLarge → 22pt, got {}",
        frame.texts()[0].font_size
    );
}

#[test]
fn theme_shape_accessor_sets_corner_radius() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Box #[bg: 0x222222, radius: t.cornerLg] {}\n}",
    );
    assert!(
        (frame.instances()[0].radii[0] - 16.0).abs() < f32::EPSILON,
        "t.cornerLg → 16px radius, got {:?}",
        frame.instances()[0].radii
    );
}

#[test]
fn manifest_custom_token_resolves_over_base() {
    let mut theme = super::super::theme::Theme::byard_base();
    theme.set_color("light", "primary", 0x0012_3456);
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Column #[bg: t.primary] {}\n}",
    );
    assert_eq!(
        frame.instances()[0].color,
        crate::interp::intrinsics::color_to_rgba(0x0012_3456, false),
        "a manifest-overridden token wins over byard-base"
    );
}

#[test]
fn unknown_theme_token_is_a_compile_error() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let parsed = parse("View C() {\n inject Theme as t\n Column #[bg: t.nope] {}\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    // The bad token surfaces when the `bg` prop is evaluated at render time.
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownThemeToken { field, .. } if field == "nope")),
        "t.nope → UnknownThemeToken, got {:?}",
        interp.errors()
    );
}

#[test]
fn theme_dark_is_assignable_and_bindable() {
    // `t.dark = …` (assign) and `bind: t.dark` (Toggle) must both resolve to
    // the scheme signal, neither is `NotAssignable` (RFC-0022 §1).
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let _ = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n \
             Column {\n Button(\"x\") => t.dark = true\n \
             Toggle #[bind: t.dark]\n }\n}",
    );
    assert!(
        interp.errors().is_empty(),
        "assignable/bindable theme.dark should not error: {:?}",
        interp.errors()
    );
}

#[test]
fn theme_mode_string_reflects_active_scheme() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Text(\"{t.mode}\")\n}",
    );
    assert_eq!(frame.texts()[0].text, "light");
    assert!(!interp.theme_is_dark());
    interp.set_theme_dark(true);
    assert!(interp.theme_is_dark());
}

#[test]
fn inline_size_overrides_typo_token() {
    let parsed = parse("View C() {\n Text(\"hi\") #[typo: \"titleLarge\", size: 30]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert!(
        (frame.texts()[0].font_size - 30.0).abs() < f32::EPSILON,
        "inline size: 30 overrides typo token"
    );
}

#[test]
fn plain_box_stays_as_box_instance() {
    let parsed = parse("View C() {\n Box #[bg: 0x111111]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.instances().len(), 1, "plain box → BoxInstance");
    assert_eq!(frame.decorated().len(), 0);
}

// ── RFC-0005: default text wrap ──────────────────────────────────────

#[test]
fn explicit_width_pins_wrap_and_yields_a_taller_leaf() {
    let long = "the quick brown fox jumps over the lazy dog again and again";
    // Same text: the first wraps to the 400px root by default, the second is
    // pinned to width 120 (both wrap, wrap is default now).
    let src = format!("View C() {{\n Text(\"{long}\")\n Text(\"{long}\") #[width: 120]\n}}");
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 2);
    assert_eq!(frame.text_wraps().len(), 2, "wrap slice parallel to texts");
    let w0 = frame.text_wraps()[0].expect("default wrap on the first line");
    let w1 = frame.text_wraps()[1].expect("explicit-width line still wraps");
    assert!(w0 > 200.0, "first wraps to the wide root, got {w0}");
    assert!(
        (w1 - 120.0).abs() < 1.0,
        "second wraps to its 120 width, got {w1}"
    );

    // The 120-wide leaf is narrower and at least as tall (more lines) as the
    // one that wrapped to the wide root.
    let wide = frame.rects()[1]; // C root is rects[0]; first Text is rects[1]
    let narrow = frame.rects()[2];
    assert!(
        (narrow.width - 120.0).abs() < 1.0,
        "the pinned leaf is 120 wide, got {}",
        narrow.width
    );
    assert!(
        narrow.height >= wide.height,
        "the narrower leaf wraps onto ≥ lines: {} vs {}",
        narrow.height,
        wide.height
    );
}

#[test]
fn reactive_demo_example_reacts_live() {
    // Ties the shipped RFC-0018 example to the suite: `when` toggles a subtree
    // and `for` grows a list, both at runtime.
    let src = include_str!("../../../examples/reactive_demo.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let show = interp
        .var_signal(&crate::symbol::Symbol::intern("showList"))
        .unwrap();
    let names = interp
        .var_signal(&crate::symbol::Symbol::intern("names"))
        .unwrap();
    let render = |interp: &mut Interpreter| {
        interp.tick();
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 640.0, 520.0);
        f.texts().iter().map(|t| t.text.clone()).collect::<Vec<_>>()
    };

    // Initially: list shown → both names present.
    let t0 = render(&mut interp);
    assert!(t0.contains(&"Ada".to_string()) && t0.contains(&"Alan".to_string()));
    assert!(!t0.iter().any(|s| s.starts_with("List hidden")));

    // Hide the list live.
    interp.write_var(show, Value::Bool(false));
    let t1 = render(&mut interp);
    assert!(!t1.contains(&"Ada".to_string()), "list unmounted live");
    assert!(
        t1.iter().any(|s| s.starts_with("List hidden")),
        "else branch"
    );

    // Show again + grow the list live → four rows.
    interp.write_var(show, Value::Bool(true));
    interp.write_var(
        names,
        Value::List(vec![
            Value::Str("Ada".into()),
            Value::Str("Alan".into()),
            Value::Str("Grace".into()),
            Value::Str("Katherine".into()),
        ]),
    );
    let t2 = render(&mut interp);
    for n in ["Ada", "Alan", "Grace", "Katherine"] {
        assert!(t2.contains(&n.to_string()), "{n} row mounted after growth");
    }
}

// ── RFC-0017: Overlay & z-layer system ───────────────────────────────

/// Finds the emitted solid box closest to the given colour channels.
fn find_solid_by_red(
    frame: &byard_core::frame::RenderFrame,
    red: f32,
) -> Option<(usize, byard_core::BoxInstance)> {
    frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - red).abs() < 0.05)
        .map(|(i, b)| (i, *b))
}

#[test]
fn overlay_takes_no_flow_space_and_paints_above_main() {
    // A main box (red) followed by an overlay whose scrim (blue, grow:1)
    // fills the viewport. The overlay must not displace the main box, and
    // its scrim must paint *above* the main box (nearer draw-order depth).
    let src = "View C() {\n \
            Box #[bg: 0xFF0000, width: 40, height: 40] {}\n \
            Overlay #[modal: false] {\n \
                Box #[bg: 0x0000FF, grow: 1] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let (red_i, red) = find_solid_by_red(&frame, 1.0).expect("main red box emitted");
    let (blue_i, blue) = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| b.color[2] > 0.95 && b.color[0] < 0.05)
        .map(|(i, b)| (i, *b))
        .expect("overlay scrim emitted");

    // Main box keeps its natural 40×40 at the origin, the overlay's 0×0
    // flow leaf did not push it down.
    assert!((red.rect[0]).abs() < 0.01 && (red.rect[1]).abs() < 0.01);
    assert!((red.rect[2] - 40.0).abs() < 0.01);
    // The scrim fills the whole viewport.
    assert!((blue.rect[2] - 400.0).abs() < 0.5 && (blue.rect[3] - 300.0).abs() < 0.5);
    // Painter's order: the overlay is emitted after the main tree, so its
    // depth is strictly nearer (smaller NDC-z) → it composites on top.
    assert!(
        frame.solid_depths()[blue_i] < frame.solid_depths()[red_i],
        "overlay scrim must paint above the main box"
    );
}

#[test]
fn overlay_center_anchor_positions_content_in_the_viewport() {
    let src = "View C() {\n \
            Overlay {\n \
                Column #[anchor: center, bg: 0x222222, width: 100, height: 60] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let dialog = frame
        .instances()
        .iter()
        .find(|b| (b.color[0] - linear(0x22)).abs() < 0.02)
        .expect("dialog emitted");
    // Centred: (400−100)/2 = 150, (300−60)/2 = 120.
    assert!(
        (dialog.rect[0] - 150.0).abs() < 1.0,
        "x centred, got {}",
        dialog.rect[0]
    );
    assert!(
        (dialog.rect[1] - 120.0).abs() < 1.0,
        "y centred, got {}",
        dialog.rect[1]
    );
}

#[test]
fn overlay_bottom_anchor_pins_content_to_the_viewport_bottom() {
    let src = "View C() {\n \
            Overlay {\n \
                Column #[anchor: bottom, bg: 0x333333, width: 200, height: 80] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let sheet = frame
        .instances()
        .iter()
        .find(|b| (b.color[0] - linear(0x33)).abs() < 0.02)
        .expect("sheet emitted");
    // Pinned to the bottom: y = 300 − 80 = 220; centred x = (400−200)/2 = 100.
    assert!(
        (sheet.rect[1] - 220.0).abs() < 1.0,
        "y bottom, got {}",
        sheet.rect[1]
    );
    assert!(
        (sheet.rect[0] - 100.0).abs() < 1.0,
        "x centred, got {}",
        sheet.rect[0]
    );
}

#[test]
fn modal_overlay_blocks_the_main_tree_and_dismisses_on_outside_tap() {
    // A main button sits behind a modal overlay. Its scrim fills the
    // viewport; a small confirm button is centred. Tapping the scrim (an
    // outside tap) fires `dismiss` and must NOT reach the button behind.
    let src = "View C() {\n \
            var open = true\n \
            var behind = false\n \
            var confirmed = false\n \
            Button(\"behind\") #[width: 400, height: 300] => behind = true\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[bg: 0x000000, opacity: 0.3, grow: 1] {}\n \
                Column #[anchor: center, width: 80, height: 40] {\n \
                    Button(\"ok\") #[width: 80, height: 40] => confirmed = true\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    let behind = interp
        .var_signal(&crate::symbol::Symbol::intern("behind"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap the top-left corner, over the scrim, outside the centred content.
    let tap = |t: u64, p: (f32, f32)| {
        [
            byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::PointerDown,
                pos: p,
                delta: (0.0, 0.0),
                payload: None,
                time_ms: t,
            },
            byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::PointerUp,
                pos: p,
                delta: (0.0, 0.0),
                payload: None,
                time_ms: t + 20,
            },
        ]
    };
    interp.dispatch_events(&tap(0, (10.0, 10.0)));
    interp.tick();

    assert_eq!(
        interp.peek(open),
        Value::Bool(false),
        "outside tap dismissed"
    );
    assert_eq!(
        interp.peek(behind),
        Value::Bool(false),
        "modal scrim blocked the button behind it"
    );
}

#[test]
fn modal_overlay_content_wins_over_the_scrim() {
    // A tap on the centred confirm button fires its action, not the scrim's
    // dismiss, the content is registered after the scrim, so it wins.
    let src = "View C() {\n \
            var open = true\n \
            var confirmed = false\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[bg: 0x000000, grow: 1] {}\n \
                Column #[anchor: center, width: 80, height: 40] {\n \
                    Button(\"ok\") #[width: 80, height: 40] => confirmed = true\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    let confirmed = interp
        .var_signal(&crate::symbol::Symbol::intern("confirmed"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Centre of the viewport = centre of the confirm button.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (200.0, 150.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (200.0, 150.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
        },
    ]);
    interp.tick();

    assert_eq!(
        interp.peek(confirmed),
        Value::Bool(true),
        "content button fired"
    );
    assert_eq!(
        interp.peek(open),
        Value::Bool(true),
        "scrim dismiss did NOT fire"
    );
}

#[test]
fn escape_dismisses_the_topmost_modal_overlay() {
    let src = "View C() {\n \
            var open = true\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[grow: 1] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("Escape".into())),
        time_ms: 0,
    }]);
    interp.tick();
    assert_eq!(
        interp.peek(open),
        Value::Bool(false),
        "Escape dismissed the modal"
    );
}

#[test]
fn when_gated_overlay_unmounts_live_on_dismiss() {
    // RFC-0018 × RFC-0017: a `when`-gated modal overlay now dismisses at
    // runtime, tapping the scrim flips the guard `var`, and the very next
    // render unmounts the overlay (no hot-reload needed). This is the headline
    // reactivity win: overlays are live.
    let src = "View C() {\n \
            var open = true\n \
            when open {\n \
                Overlay #[modal: true, dismiss => open = false] {\n \
                    Box #[bg: 0x000000, opacity: 0.4, grow: 1] {}\n \
                    Column #[anchor: center, bg: 0xFFFFFF, width: 100, height: 60] {}\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // The overlay is mounted: the white dialog surface is present.
    assert!(
        frame.instances().iter().any(|b| b.color[0] > 0.95),
        "overlay mounted initially"
    );

    // Tap the scrim (top-left, outside the centred content).
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
        },
    ]);
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    // `open` flipped false → the `when` unmounts the overlay entirely.
    assert!(
        !frame2.instances().iter().any(|b| b.color[0] > 0.95),
        "overlay unmounted live after dismiss"
    );
    assert!(frame2.instances().is_empty(), "nothing left on screen");
}

#[test]
fn non_modal_overlay_lets_taps_fall_through() {
    // A non-modal overlay (a snackbar-style surface) must not block the main
    // tree: a tap on the button behind still fires.
    let src = "View C() {\n \
            var behind = false\n \
            Button(\"behind\") #[width: 400, height: 300] => behind = true\n \
            Overlay #[modal: false] {\n \
                Column #[anchor: bottom, width: 100, height: 20] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let behind = interp
        .var_signal(&crate::symbol::Symbol::intern("behind"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap top-left, away from the bottom-anchored surface.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (10.0, 10.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (10.0, 10.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(behind),
        Value::Bool(true),
        "non-modal overlay let the tap fall through"
    );
}

#[test]
fn overlay_demo_example_renders_dialog_above_the_base_app() {
    // Ties the shipped visual example to the test suite: it must parse,
    // lower, and render, with the modal dialog surface compositing above the
    // base app (RFC-0017). Guards the demo against silent breakage.
    let src = include_str!("../../../examples/overlay_demo.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 900.0, 560.0);

    // The base app background (0x14141C) is emitted early; the dialog surface
    // (0xECE6F0) is an overlay emitted later, so it sits at a nearer depth than
    // the base app. Both are matched by their written colour, decoded: the
    // frame is in linear light.
    let base = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x14)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("base app background emitted");
    let (dialog, dialog_box) = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| {
            (b.color[0] - linear(0xEC)).abs() < 0.02 && (b.color[1] - linear(0xE6)).abs() < 0.02
        })
        .map(|(i, b)| (i, *b))
        .expect("dialog surface emitted");
    assert!(
        frame.solid_depths()[dialog] < frame.solid_depths()[base],
        "the modal dialog must composite above the base app"
    );

    // No dialog text line may overflow the dialog surface, line wrap is not
    // built yet, so the example is authored to fit. Guards the reported
    // overflow against regression: every dark-on-light label painted inside
    // the surface must end before the surface's right edge.
    let mut measurer = byard_core::text::TextMeasurer::new();
    let surf_left = dialog_box.rect[0];
    let surf_right = dialog_box.rect[0] + dialog_box.rect[2];
    for (line, wrap) in frame.texts().iter().zip(frame.text_wraps()) {
        let inside = line.x >= surf_left && line.x < surf_right && line.color[0] < 0.5;
        if inside {
            // Honour the wrap width (RFC-0018): a wrapped label's laid-out
            // width is bounded to it, not the full one-line measurement.
            let (w, _) = measurer.measure_wrapped(&line.text, line.font_size, *wrap, 400, None);
            assert!(
                line.x + w <= surf_right + 0.5,
                "dialog text {:?} overflows the surface: {} + {} > {}",
                line.text,
                line.x,
                w,
                surf_right
            );
        }
    }
}

#[test]
fn nested_overlays_stack_in_mount_order() {
    // An overlay whose content mounts a second overlay: both are collected,
    // and the inner one is emitted later (on top).
    let src = "View C() {\n \
            Overlay #[modal: false] {\n \
                Box #[bg: 0x111111, grow: 1] {}\n \
                Overlay #[modal: false] {\n \
                    Box #[bg: 0x222222, grow: 1] {}\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let outer = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x11)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("outer overlay box");
    let inner = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x22)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("inner overlay box");
    assert!(
        frame.solid_depths()[inner] < frame.solid_depths()[outer],
        "nested overlay stacks above its parent overlay"
    );
}

// ── RFC-0023 ripple ink ─────────────────────────────────────────────

/// A 200×100 box at the layout origin with a semi-transparent white
/// ripple, triggered by `on pressed`, the RFC-0023 guide example shape.
const RIPPLE_SRC: &str = "View V() {
        let btn = style {
            bg: 0x6750A4, radius: 20, ripple: 0x80FFFFFF
            on pressed { ripple_active: true }
        }
        Box #[..btn, width: 200, height: 100] {}
    }";

fn pointer(
    kind: byard_core::platform::EventKind,
    x: f32,
    y: f32,
    t: u64,
) -> byard_core::platform::InputEvent {
    byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
    }
}

/// Lowers `src`, renders once at `t = 0` (registering the hit region),
/// presses at `(x, y)` with press identity `press_t`, and renders once at
/// `t = 10`, the frame that spawns the ripple, so its `start_ms` is 10.
fn pressed_ripple_named(src: &str, x: f32, y: f32, press_t: u64) -> (Interpreter, Vec<RenderNode>) {
    use byard_core::platform::EventKind as K;
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[pointer(K::PointerDown, x, y, press_t)]);
    let spawn_frame = render_at(&mut interp, &tree, 10);
    assert_eq!(
        spawn_frame.ripples().len(),
        1,
        "the press spawns one ripple"
    );
    (interp, tree)
}

/// [`pressed_ripple_named`] over the canonical [`RIPPLE_SRC`] view.
fn pressed_ripple_interp(x: f32, y: f32, press_t: u64) -> (Interpreter, Vec<RenderNode>) {
    pressed_ripple_named(RIPPLE_SRC, x, y, press_t)
}

/// Renders `tree` at `ms` and returns the frame.
fn render_at(
    interp: &mut Interpreter,
    tree: &[RenderNode],
    ms: u32,
) -> byard_core::frame::RenderFrame {
    interp.set_now_ms(ms);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 300.0);
    frame
}

#[test]
fn a_press_spawns_a_ripple_at_the_tap_point() {
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    let frame = render_at(&mut interp, &tree, 50);

    assert_eq!(frame.ripples().len(), 1, "one press spawns one ripple");
    let r = frame.ripples()[0];
    // The circle is centred on the tap point, in absolute coordinates.
    assert!(
        (r.params[0] - 30.0).abs() < 0.01,
        "center x: {:?}",
        r.params
    );
    assert!(
        (r.params[1] - 40.0).abs() < 0.01,
        "center y: {:?}",
        r.params
    );
    // Mid-animation: expanding and still visible.
    assert!(r.params[2] > 0.0, "radius has started expanding");
    assert!(r.params[3] > 0.0 && r.params[3] < 1.0, "fade in flight");
    // The ink colour is the resolved `ripple:` colour (50% white).
    assert!((r.color[3] - 0.5).abs() < 0.01, "ink alpha: {:?}", r.color);
    // Clipped to the element's rounded rect: rect and radii carried over.
    assert_eq!(r.rect, [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(r.radii, [20.0; 4]);
    assert!(
        interp.has_active_animations(),
        "a live ripple keeps requesting frames"
    );
}

#[test]
fn ripple_expands_monotonically_and_retires_after_its_duration() {
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    let r1 = render_at(&mut interp, &tree, 50).ripples()[0].params[2];
    let f2 = render_at(&mut interp, &tree, 150);
    let r2 = f2.ripples()[0].params[2];
    assert!(r2 > r1, "radius grows with time ({r1} → {r2})");

    // Past the 300 ms default duration the ripple is gone, and with it the
    // frame demand (no other animation is in flight in this view).
    let f3 = render_at(&mut interp, &tree, 400);
    assert!(f3.ripples().is_empty(), "faded ripple retires");
    assert!(!interp.has_active_animations(), "no lingering frame demand");
}

#[test]
fn the_auto_max_radius_reaches_the_farthest_corner() {
    // Tap at the top-left origin of the 200×100 box: the farthest corner
    // is (200, 100), so the ink must grow to cover hypot(200, 100).
    let (mut interp, tree) = pressed_ripple_interp(0.0, 0.0, 1);
    let expected = 200.0_f32.hypot(100.0);
    // One ms before retirement the ease-out ramp has essentially arrived.
    let frame = render_at(&mut interp, &tree, 299);
    let r = frame.ripples()[0].params[2];
    assert!(
        r > expected * 0.99,
        "radius {r} must reach the farthest corner ({expected})"
    );
}

#[test]
fn a_hold_spawns_once_while_rapid_taps_spawn_one_ripple_each() {
    use byard_core::platform::EventKind as K;
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    // Held press across two renders: still exactly one ripple.
    let _ = render_at(&mut interp, &tree, 20);
    let held = render_at(&mut interp, &tree, 40);
    assert_eq!(held.ripples().len(), 1, "a hold never respawns");

    // Release and tap again (a fresh press identity): a second ripple
    // joins the first, still fading, their ink pools on the GPU.
    interp.dispatch_events(&[pointer(K::PointerUp, 30.0, 40.0, 60)]);
    interp.dispatch_events(&[pointer(K::PointerDown, 80.0, 50.0, 80)]);
    let both = render_at(&mut interp, &tree, 100);
    assert_eq!(both.ripples().len(), 2, "each tap spawns its own ripple");
}

#[test]
fn ripple_radius_and_duration_props_override_the_defaults() {
    let src = "View V() {
            let btn = style {
                bg: 0x6750A4, ripple: 0x80FFFFFF
                ripple_radius: 10, ripple_duration: 100
                on pressed { ripple_active: true }
            }
            Box #[..btn, width: 200, height: 100] {}
        }";
    // Spawned at t = 10 by the helper.
    let (mut interp, tree) = pressed_ripple_named(src, 30.0, 40.0, 1);

    let mid = render_at(&mut interp, &tree, 100);
    assert_eq!(mid.ripples().len(), 1);
    assert!(
        mid.ripples()[0].params[2] <= 10.0,
        "`ripple_radius` caps the expansion: {:?}",
        mid.ripples()[0].params
    );
    let done = render_at(&mut interp, &tree, 120);
    assert!(
        done.ripples().is_empty(),
        "`ripple_duration: 100` retires the ink after 100 ms"
    );
}

#[test]
fn a_ripple_without_an_active_trigger_never_spawns() {
    use byard_core::platform::EventKind as K;
    // `ripple:` set but no `ripple_active` anywhere: pressing must not ink.
    let src = "View V() {
            Box #[bg: 0x6750A4, ripple: 0x80FFFFFF, width: 200, height: 100] {}
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[pointer(K::PointerDown, 30.0, 40.0, 1)]);
    let frame = render_at(&mut interp, &tree, 50);
    assert!(frame.ripples().is_empty(), "no trigger, no ink");
}

#[test]
fn ripple_depth_sits_between_the_background_and_the_children() {
    // A box with a child label: the ink must draw above the box fill but
    // beneath the text (RFC-0023 compositing order).
    let src = "View V() {
            let btn = style {
                bg: 0x6750A4, ripple: 0x80FFFFFF
                on pressed { ripple_active: true }
            }
            Box #[..btn, width: 200, height: 100] {
                Text(\"Save\") #[color: 0xFFFFFF]
            }
        }";
    let (mut interp, tree) = pressed_ripple_named(src, 30.0, 40.0, 1);
    let frame = render_at(&mut interp, &tree, 50);
    let ripple_depth = frame.ripples()[0].depth;
    let bg_depth = frame.solid_depths()[0];
    let text_depth = frame.text_depths()[0];
    // Later emission = nearer = smaller NDC-z.
    assert!(
        ripple_depth < bg_depth,
        "ink above the background ({ripple_depth} vs {bg_depth})"
    );
    assert!(
        text_depth < ripple_depth,
        "children above the ink ({text_depth} vs {ripple_depth})"
    );
}

// ── RFC-0005 scroll × hit-testing ───────────────────────────────────

#[test]
fn hit_targets_follow_the_scroll_offset() {
    use byard_core::platform::EventKind as K;
    // A tappable card that starts at y 80..160 inside a 100-high
    // viewport. Scrolling down 60 shows it at y 20..100 on screen.
    let src = "View V() { var n = 0 var off = 0
            ScrollView #[width: 200, height: 100, offset: (0, off)] {
                Column {
                    Box #[bg: 0x111111, width: 200, height: 80] {}
                    Box #[bg: 0x222222, width: 200, height: 80, tap => n = n + 1] {}
                }
            }
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let n = interp.var_signal(&Symbol::intern("n")).unwrap();
    let taps = |interp: &Interpreter| match interp.peek(n) {
        Value::Int(v) => v,
        other => panic!("n must be an Int, got {other:?}"),
    };
    let tap_at = |interp: &mut Interpreter, x: f32, y: f32, t: u64| {
        interp.dispatch_events(&[pointer(K::PointerDown, x, y, t)]);
        interp.dispatch_events(&[pointer(K::PointerUp, x, y, t + 20)]);
    };

    // Scroll down 60 and re-render (handlers re-register shifted).
    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    interp.write_var(off, Value::Int(60));
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tapping where the card now IS on screen fires its action…
    tap_at(&mut interp, 100.0, 30.0, 100);
    assert_eq!(taps(&interp), 1, "the on-screen position must be tappable");

    // …and tapping where layout *placed* it (y 110, scrolled away and
    // outside the viewport) must not, the hit rect moved with the
    // content and is clipped to the scroll viewport.
    tap_at(&mut interp, 100.0, 110.0, 200);
    assert_eq!(taps(&interp), 1, "the stale laid-out position is inert");
}

// ── RFC-0023 §2 backdrop blur / vibrancy ────────────────────────────

fn rendered_frame(src: &str) -> (Interpreter, byard_core::frame::RenderFrame) {
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    (interp, frame)
}

#[test]
fn blur_props_emit_a_backdrop_with_the_resolved_fields() {
    let (_interp, frame) = rendered_frame(
        "View V() { Box #[width: 200, height: 100, radius: 16, blur: 20, \
             backdrop_tint: 0x80FFFFFF, blur_saturation: 2.0, blur_quality: high] {} }",
    );
    assert_eq!(frame.backdrops().len(), 1);
    let b = frame.backdrops()[0];
    assert_eq!(b.rect, [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(b.radii, [16.0; 4]);
    assert!((b.blur - 20.0).abs() < f32::EPSILON);
    assert!((b.tint[3] - 0.5).abs() < 0.01, "tint alpha: {:?}", b.tint);
    assert!((b.saturation - 2.0).abs() < f32::EPSILON);
    assert_eq!(b.quality, byard_core::frame::BLUR_QUALITY_HIGH);
}

#[test]
fn blur_clamps_to_the_max_radius_and_defaults_saturation() {
    let (_interp, frame) =
        rendered_frame("View V() { Box #[width: 100, height: 100, blur: 500] {} }");
    let b = frame.backdrops()[0];
    assert!(
        (b.blur - byard_core::frame::BLUR_MAX_RADIUS).abs() < f32::EPSILON,
        "500 clamps to the max, got {}",
        b.blur
    );
    assert!((b.saturation - 1.8).abs() < 0.01, "vibrancy default 1.8");
    assert_eq!(b.quality, byard_core::frame::BLUR_QUALITY_AUTO);
}

#[test]
fn a_tint_without_blur_lowers_to_a_plain_translucent_fill() {
    // No blur → no barrier, no off-screen work: the identical composite
    // is a translucent fill over the content behind.
    let (_interp, frame) =
        rendered_frame("View V() { Box #[width: 100, height: 100, backdrop_tint: 0x80336699] {} }");
    assert!(frame.backdrops().is_empty(), "no blur, no backdrop barrier");
    let wash = frame
        .decorated()
        .iter()
        .find(|d| (d.base.color[3] - 0.5).abs() < 0.01)
        .expect("the tint lowers to a translucent decorated fill");
    assert_eq!(wash.base.rect, [0.0, 0.0, 100.0, 100.0]);
}

// ── Gradient fills (RFC-0001 §3.1) ──────────────────────────────────

#[test]
fn a_gradient_promotes_the_box_and_resolves_its_ramp() {
    let (_interp, frame) = rendered_frame(
        "View V() { Box #[width: 200, height: 40, bg: 0x2B2930, \
             gradient: (angle: 90deg, from: 0x00FFFFFF, mid: 0x40FFFFFF, to: 0x00FFFFFF, \
             mid_pos: 0.25), gradient_offset: 0.5] {} }",
    );
    assert!(
        frame.instances().is_empty(),
        "a gradient promotes the box off the flat SolidBox path"
    );
    let g = frame.decorated()[0]
        .gradient
        .expect("the ramp reaches the frame");
    assert!(
        (g.angle - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
        "90deg is canonicalized to radians by the lexer, got {}",
        g.angle
    );
    assert!((g.mid_pos - 0.25).abs() < 1e-6);
    assert!((g.offset - 0.5).abs() < 1e-6);
    // Straight-alpha stops: a transparent white end and a 25 %-alpha middle.
    assert!(
        g.from[3] < 0.01 && g.to[3] < 0.01,
        "the ends are transparent"
    );
    assert!(
        (g.mid[3] - 0.25).abs() < 0.01,
        "the band's alpha, got {:?}",
        g.mid
    );
    assert!(g.mid[0] > 0.99, "…and it is white");
}

#[test]
fn an_omitted_mid_stop_is_the_midpoint_of_the_two_ends() {
    let (_interp, frame) = rendered_frame(
        "View V() { Box #[width: 100, height: 20, \
             gradient: (from: 0xFF000000, to: 0xFFFFFFFF)] {} }",
    );
    let g = frame.decorated()[0].gradient.unwrap();
    // A two-stop ramp: the implicit middle is exactly halfway, so the ramp
    // is indistinguishable from a plain linear gradient.
    for channel in 0..3 {
        assert!(
            (g.mid[channel] - f32::midpoint(g.from[channel], g.to[channel])).abs() < 1e-6,
            "channel {channel} must be the midpoint"
        );
    }
    assert!(
        !frame.decorated()[0].base.color[3].is_nan(),
        "a gradient with no `bg` still paints a surface"
    );
}

#[test]
fn a_gradient_offset_animates_like_any_other_number() {
    // The travelling-sweep case: `gradient_offset` is an ordinary numeric
    // prop, so RFC-0025's looping keyframes drive it with no extra plumbing.
    let src = "View V() { Box #[width: 100, height: 20, bg: 0x222222, \
                   gradient: (from: 0x00FFFFFF, mid: 0x40FFFFFF, to: 0x00FFFFFF), \
                   gradient_offset: anim.keyframes(0%: 0.0, 100%: 1.0, \
                   duration: 400ms, loop: true)] {} }";
    let seen = sample_over_time(src, &[0, 200, 400], |f| {
        f.decorated()[0].gradient.map_or(-1.0, |g| g.offset)
    });
    assert!((seen[0].0).abs() < 0.01, "starts at 0, got {}", seen[0].0);
    assert!((seen[1].0 - 0.5).abs() < 0.05, "halfway, got {}", seen[1].0);
    assert!((seen[2].0).abs() < 0.01, "wrapped, got {}", seen[2].0);
    assert!(
        seen.iter().all(|(_, active)| *active),
        "a loop keeps frames coming"
    );
}

#[test]
fn a_malformed_gradient_is_reported_not_ignored() {
    let errs = |src: &str| {
        let parsed = parse(src);
        let mut interp = Interpreter::new();
        let tree = interp.lower_view(&parsed.views[0], &[]);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        interp.errors().to_vec()
    };
    // A single end is a flat wash the author could have written as `bg`.
    assert!(matches!(
        errs("View V() { Box #[width: 10, height: 10, gradient: (from: 0xFF0000)] {} }").first(),
        Some(CompileError::AttributeTypeMismatch { .. })
    ));
    // A misspelt field names the fix instead of silently doing nothing.
    assert!(matches!(
        errs("View V() { Box #[width: 10, height: 10, \
              gradient: (from: 0xFF0000, to: 0x00FF00, angel: 90deg)] {} }")
            .first(),
        Some(CompileError::UnknownAttribute { hint: Some(h), .. }) if h == "gradient.angle"
    ));
    // Not a tuple at all.
    assert!(matches!(
        errs("View V() { Box #[width: 10, height: 10, gradient: 0xFF0000] {} }").first(),
        Some(CompileError::AttributeTypeMismatch { .. })
    ));
}

// ── Spacer flexes (RFC-0005) ─────────────────────────────────────────

#[test]
fn a_spacer_absorbs_the_free_space_of_its_row() {
    // RFC-0005: `Spacer` is a *flexible* gap (`grow`, default 1). Before it
    // flexed, a trailing item sat glued to the leading one instead of at the
    // far end of the row.
    let (_interp, frame) = rendered_frame(
        "View V() { Row #[width: 300, height: 20] { \
                 Box #[bg: 0xFF0000, width: 20, height: 20] {} \
                 Spacer \
                 Box #[bg: 0x00FF00, width: 20, height: 20] {} \
             } }",
    );
    let x_of = |red: bool| {
        frame
            .instances()
            .iter()
            .find(|b| (b.color[0] > 0.8) == red && (b.color[1] > 0.8) != red)
            .map(|b| b.rect[0])
            .expect("both boxes are emitted")
    };
    assert!((x_of(true) - 0.0).abs() < 0.5, "the first box stays put");
    assert!(
        (x_of(false) - 280.0).abs() < 0.5,
        "the second is pushed to the far end, got {}",
        x_of(false)
    );
}

#[test]
fn a_spacer_honours_grow_and_basis() {
    // `grow: 0` degenerates to a fixed `basis`-sized gap; two spacers with
    // different `grow` split the free space in proportion.
    let (_interp, frame) = rendered_frame(
        "View V() { Row #[width: 300, height: 20] { \
                 Spacer #[grow: 0, basis: 40] \
                 Box #[bg: 0xFF0000, width: 20, height: 20] {} \
                 Spacer #[grow: 1] \
                 Box #[bg: 0x00FF00, width: 20, height: 20] {} \
                 Spacer #[grow: 3] \
             } }",
    );
    let x_of = |red: bool| {
        frame
            .instances()
            .iter()
            .find(|b| (b.color[0] > 0.8) == red && (b.color[1] > 0.8) != red)
            .map(|b| b.rect[0])
            .unwrap()
    };
    assert!((x_of(true) - 40.0).abs() < 0.5, "the fixed 40px gap held");
    // 300 − 40 − 20 − 20 = 220 free, split 1 : 3 → 55 then 165.
    assert!(
        (x_of(false) - (40.0 + 20.0 + 55.0)).abs() < 1.0,
        "the free space split 1:3, got {}",
        x_of(false)
    );
}

#[test]
fn the_backdrop_barrier_snapshots_the_content_behind_it() {
    // A solid card behind, then a bg-less glass pane with a child label:
    // the barrier must capture the card (and nothing later), and the
    // pane's depth must sit between the card and the label.
    let src = "View V() {
            Column #[width: 300, height: 200] {
                Box #[bg: 0xFF3366, width: 300, height: 80] {}
                Box #[blur: 12, width: 300, height: 80] {
                    Text(\"glass\") #[color: 0x000000]
                }
            }
        }";
    let (_interp, frame) = rendered_frame(src);
    assert_eq!(frame.backdrops().len(), 1);
    let mark = frame.backdrop_marks()[0];
    assert_eq!(mark.solid, 1, "the card was emitted behind the barrier");
    assert_eq!(mark.text, 0, "the pane's own label comes after it");
    let d = frame.backdrops()[0].depth;
    assert!(d < frame.solid_depths()[0], "pane above the card");
    assert!(frame.text_depths()[0] < d, "label above the pane");
}

#[test]
fn blur_animates_as_a_paint_prop() {
    let sample = |f: &byard_core::frame::RenderFrame| f.backdrops().first().map_or(0.0, |b| b.blur);
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[width: 100, height: 100, blur: on ? 16.0 : 4.0 with anim.linear(1000)] }",
        sample,
    );
    assert!((a - 4.0).abs() < 0.5, "starts near 4, got {a}");
    assert!((b - 10.0).abs() < 1.5, "~halfway, got {b}");
    assert!((c - 16.0).abs() < 0.5, "arrives at 16, got {c}");
}

#[test]
fn a_state_override_retargets_the_base_with_animation() {
    use byard_core::platform::EventKind as K;
    // RFC-0010 × RFC-0012/0016: the state changes the target, the base
    // owns the curve, `on hover { blur: 16 }` over
    // `blur: 4 with anim.linear(1000)` must ramp, not pop.
    let src = "View V() {
            let glass = style {
                blur: 4.0 with anim.linear(1000)
                on hover { blur: 16 }
            }
            Box #[..glass, width: 200, height: 100] {}
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Hover the pane; the next renders sample the retargeted ramp.
    interp.dispatch_events(&[pointer(K::PointerMove, 50.0, 50.0, 1)]);
    let blur_at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.backdrops().first().map_or(-1.0, |b| b.blur)
    };
    let a = blur_at(&mut interp, 0);
    let b = blur_at(&mut interp, 500);
    let c = blur_at(&mut interp, 1000);
    assert!((a - 4.0).abs() < 0.5, "starts at the base value, got {a}");
    assert!(
        (b - 10.0).abs() < 1.5,
        "~halfway to the hover target, got {b}"
    );
    assert!(
        (c - 16.0).abs() < 0.5,
        "arrives at the hover target, got {c}"
    );
}

#[test]
fn an_animated_color_ramps_its_alpha_channel_too() {
    // RFC-0023 regression: a translucent `backdrop_tint` fades in, the
    // alpha byte animates alongside the OKLab channels. The old 3-channel
    // path dropped alpha entirely, collapsing `0x00FFFFFF → 0x80FFFFFF`
    // into an instant opaque white.
    let sample =
        |f: &byard_core::frame::RenderFrame| f.backdrops().first().map_or(-1.0, |b| b.tint[3]);
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[width: 100, height: 100, blur: 8, \
             backdrop_tint: on ? 0x80FFFFFF : 0x00FFFFFF with anim.linear(1000)] }",
        sample,
    );
    assert!(a.abs() < 0.02, "starts fully transparent, got {a}");
    assert!(
        (b - 0.25).abs() < 0.05,
        "~half the 0.5 target mid-ramp, got {b}"
    );
    assert!((c - 0.502).abs() < 0.02, "arrives at 0x80 alpha, got {c}");
}

#[test]
fn deepest_rect_overlap_counts_the_tallest_stack() {
    let r = |x: f32| [x, 0.0, 100.0, 100.0];
    assert_eq!(deepest_rect_overlap(&[]), 0);
    assert_eq!(deepest_rect_overlap(&[r(0.0)]), 1);
    // Disjoint panes never stack.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(200.0), r(400.0)]), 1);
    // Two overlapping panes plus a distant third: the cluster is 2.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(80.0), r(400.0)]), 2);
    // A chain a∩b, b∩c clusters to 3 around b, deliberately
    // conservative (see the fn docs), erring toward the diagnostic.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(80.0), r(160.0)]), 3);
    // Three co-located panes: a genuine 3-deep stack.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(10.0), r(20.0)]), 3);
}

#[test]
fn three_stacked_glass_panes_raise_the_overlap_warning() {
    let (interp, frame) = rendered_frame(
        "View V() { ZStack #[width: 200, height: 200] { \
             Box #[blur: 8, width: 180, height: 180] {} \
             Box #[blur: 8, width: 160, height: 160] {} \
             Box #[blur: 8, width: 140, height: 140] {} } }",
    );
    assert_eq!(frame.backdrops().len(), 3);
    assert_eq!(
        interp.perf_warnings(),
        &[PerfWarning::OverlappingBlurs { count: 3 }]
    );

    // Two panes are fine, stacked glass only warns at three.
    let (interp, _frame) = rendered_frame(
        "View V() { ZStack #[width: 200, height: 200] { \
             Box #[blur: 8, width: 180, height: 180] {} \
             Box #[blur: 8, width: 160, height: 160] {} } }",
    );
    assert!(interp.perf_warnings().is_empty());
}
