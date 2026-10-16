//! A lambda's parameters can say their types: `(asked: Str, page: Int) => …`.
//!
//! They could not; `(asked: Str)` was a parse error, so a named action that
//! took a value took it untyped, and its body was checked against nothing.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// Parses and checks a view with `body` in it; returns the checker's errors.
fn check(body: &str) -> Vec<String> {
    let parsed = parse(&format!(
        "View Main() {{\n    var n: Int = 0\n    var q: Str = \"ab\"\n{body}\n    Text(\"{{n}}\")\n}}"
    ));
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    byard_compiler::infer::check_views(&parsed.views)
        .errors
        .iter()
        .map(|e| format!("{e:?}"))
        .collect()
}

#[test]
fn typed_parameters_parse_and_check_clean() {
    for body in [
        "let search = (asked: Str) => { n = asked.len }",
        "let move = (lat: Float, lon: Float, label: Str) => { n = label.len }",
        "let total = (xs: List<Int>) => { n = xs.len }",
        "let run = (done: Fn()) => { done() }",
        // Mixed, and none at all, as before.
        "let mixed = (a, b: Int) => { n = b }",
        "let plain = (a, b) => { n = 1 }",
    ] {
        let errors = check(body);
        assert!(errors.is_empty(), "{body}: {errors:?}");
    }
}

/// The body is checked against the written type.
#[test]
fn the_body_is_checked_against_the_parameters_type() {
    let errors = check("let shout = (asked: Str) => { n = asked.uper().len }");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("UnknownMethod") && e.contains("uper")),
        "{errors:?}"
    );
}

/// A parenthesis that no arrow follows is still an expression.
#[test]
fn a_parenthesised_expression_is_still_one() {
    let errors = check("let double = (n + 1) * 2");
    assert!(errors.is_empty(), "{errors:?}");
}

/// The types are for the checker; the action runs as it always did.
#[test]
fn a_typed_action_runs() {
    let parsed = parse(
        "View Main() {
    var shown: Str = \"-\"
    let say = (word: Str, times: Int) => { shown = \"{word} x{times}\" }
    on mount => say(\"hi\", 3)
    Text(shown)
}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 100.0);
    interp.tick();
    frame.clear();
    interp.render(&tree, &mut frame, 400.0, 100.0);
    assert_eq!(frame.texts()[0].text, "hi x3");
}
