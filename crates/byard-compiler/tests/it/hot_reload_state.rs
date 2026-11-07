//! A reactive-compatible hot reload keeps the root's state through the
//! lowering that follows it.
//!
//! `reload` kept each `var`'s signal, and the root was then lowered again,
//! which evaluated the view's declarations and gave every `var` a fresh
//! signal at its initial value: the state survived one step and was lost in
//! the next. Every reload under `byard dev` reset the root's state.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::reload::apply_program;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

fn shown(
    interp: &mut Interpreter,
    tree: &[byard_compiler::interp::eval::RenderNode],
) -> Vec<String> {
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 200.0);
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

#[test]
fn state_survives_a_reload_and_the_lowering_after_it() {
    let before = parse("View Main() {\n    var count: Int = 0\n    Text(\"count {count}\")\n}");
    let mut interp = Interpreter::new();
    interp.load_views(&before.views);
    let tree = interp.lower_view(&before.views[0], &["Main"]);
    let count = interp
        .var_signal(&byard_compiler::Symbol::intern("count"))
        .unwrap();
    interp.write_var(count, byard_compiler::interp::env::Value::Int(7));
    assert_eq!(shown(&mut interp, &tree), ["count 7"]);

    // Same declarations, a different way to show them.
    let after = parse("View Main() {\n    var count: Int = 0\n    Text(\"total: {count}\")\n}");
    let tree =
        apply_program(&mut interp, &before.views, &after.views).expect("the root is lowered again");
    assert_eq!(shown(&mut interp, &tree), ["total: 7"]);
}

/// A reload that changes the declarations starts them afresh, as before.
#[test]
fn a_new_declaration_starts_afresh() {
    let before = parse("View Main() {\n    var count: Int = 0\n    Text(\"count {count}\")\n}");
    let mut interp = Interpreter::new();
    interp.load_views(&before.views);
    let tree = interp.lower_view(&before.views[0], &["Main"]);
    let count = interp
        .var_signal(&byard_compiler::Symbol::intern("count"))
        .unwrap();
    interp.write_var(count, byard_compiler::interp::env::Value::Int(7));
    let _ = shown(&mut interp, &tree);

    let after = parse(
        "View Main() {\n    var count: Int = 0\n    var extra: Int = 1\n    Text(\"count {count}\")\n}",
    );
    let tree = apply_program(&mut interp, &before.views, &after.views).expect("lowered again");
    assert_eq!(shown(&mut interp, &tree), ["count 0"]);
}

/// A derived `let` holds no state, so adding, removing or moving one between
/// edits is not a change to what the view remembers: every `var` keeps its
/// value, through the lowering after the reload as well.
#[test]
fn adding_a_derived_let_keeps_every_var() {
    let before = parse(
        "View Main() {\n    var count: Int = 0\n    var name: Str = \"a\"\n    Text(\"{name} {count}\")\n}",
    );
    let mut interp = Interpreter::new();
    interp.load_views(&before.views);
    let tree = interp.lower_view(&before.views[0], &["Main"]);
    let sym = byard_compiler::Symbol::intern;
    let value = byard_compiler::interp::env::Value::Int;
    interp.write_var(interp.var_signal(&sym("count")).unwrap(), value(7));
    interp.write_var(
        interp.var_signal(&sym("name")).unwrap(),
        byard_compiler::interp::env::Value::Str("zed".into()),
    );
    assert_eq!(shown(&mut interp, &tree), ["zed 7"]);

    // A `let` appears between the vars, and the vars swap places.
    let after = parse(
        "View Main() {\n    var name: Str = \"a\"\n    let twice = count * 2\n    var count: Int = 0\n    Text(\"{name} {count} {twice}\")\n}",
    );
    let tree = apply_program(&mut interp, &before.views, &after.views).expect("lowered again");
    assert_eq!(shown(&mut interp, &tree), ["zed 7 14"]);

    // And removing it again.
    let tree = apply_program(&mut interp, &after.views, &before.views).expect("lowered again");
    assert_eq!(shown(&mut interp, &tree), ["zed 7"]);
}

/// A `var` whose declared type changed does not keep a value of the old one.
#[test]
fn a_retyped_var_starts_afresh() {
    let before = parse("View Main() {\n    var n: Int = 3\n    Text(\"{n}\")\n}");
    let mut interp = Interpreter::new();
    interp.load_views(&before.views);
    let tree = interp.lower_view(&before.views[0], &["Main"]);
    assert_eq!(shown(&mut interp, &tree), ["3"]);
    interp.write_var(
        interp
            .var_signal(&byard_compiler::Symbol::intern("n"))
            .unwrap(),
        byard_compiler::interp::env::Value::Int(9),
    );
    let after = parse("View Main() {\n    var n: Str = \"fresh\"\n    Text(\"{n}\")\n}");
    let tree = apply_program(&mut interp, &before.views, &after.views).expect("lowered again");
    assert_eq!(shown(&mut interp, &tree), ["fresh"]);
}

/// RFC-0051: editing the body of a view that binds a parameter, or turning a
/// value parameter into a binding, keeps the caller's `var` and the binding
/// stays live to it afterwards.
#[test]
fn a_binding_survives_a_reload_of_the_view_that_declares_it() {
    use byard_compiler::interp::env::Value;
    let sym = byard_compiler::Symbol::intern;
    let main = "View Main() {\n    var wifi: Bool = false\n    Switch(bind: wifi)\n}\n";
    let before = parse(&format!(
        "{main}View Switch(bind on: Bool) {{\n    Text(\"on={{on}}\")\n}}"
    ));
    let mut interp = Interpreter::new();
    interp.load_views(&before.views);
    let tree = interp.lower_view(&before.views[0], &["Main", "Switch"]);
    interp.write_var(interp.var_signal(&sym("wifi")).unwrap(), Value::Bool(true));
    assert_eq!(shown(&mut interp, &tree), ["on=true"]);

    // The body of the view changes: the caller's `var` keeps its value.
    let after = parse(&format!(
        "{main}View Switch(bind on: Bool) {{\n    Text(\"switch {{on}}\")\n}}"
    ));
    let tree = apply_program(&mut interp, &before.views, &after.views).expect("lowered again");
    assert_eq!(shown(&mut interp, &tree), ["switch true"]);
    // The binding is live to the same `var` after the reload.
    interp.write_var(interp.var_signal(&sym("wifi")).unwrap(), Value::Bool(false));
    assert_eq!(shown(&mut interp, &tree), ["switch false"]);

    // A value parameter becomes a binding, and the caller says so in the same
    // edit: the root is lowered again with the new call.
    let value = parse(
        "View Main() {\n    var wifi: Bool = false\n    Switch(on: wifi)\n}\nView Switch(on: Bool) {\n    Text(\"v={on}\")\n}",
    );
    let tree = apply_program(&mut interp, &after.views, &value.views).expect("lowered again");
    assert_eq!(shown(&mut interp, &tree), ["v=false"]);
    let tree = apply_program(&mut interp, &value.views, &before.views).expect("lowered again");
    assert_eq!(shown(&mut interp, &tree), ["on=false"]);
    interp.write_var(interp.var_signal(&sym("wifi")).unwrap(), Value::Bool(true));
    assert_eq!(shown(&mut interp, &tree), ["on=true"]);
}
