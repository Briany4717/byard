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
