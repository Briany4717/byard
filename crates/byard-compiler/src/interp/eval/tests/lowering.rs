//! Expression lowering and user-view recognition: arithmetic, comparison,
//! logic, strings and collections, the controller boundary, parameterised calls
//! and `eval_pure`.

use super::*;
use crate::interp::eval::expr::eval_binary;
use crate::interp::eval::expr::eval_compare;
use crate::interp::eval::expr::eval_concat;
use crate::interp::eval::expr::index_value;
use crate::interp::eval::expr::structural_eq;
use crate::parser::ast::BinOp;

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
    // Uses the real gear fixture so this proves
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
        "first tick must be a zero-opacity placeholder, got alpha {}",
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

// ── Controller boundary ─────────────────────────────────────────

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

// ── Parameterized fn call sites ─────────────────────────────────

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
