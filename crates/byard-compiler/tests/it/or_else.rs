//! `a ?? b`: the left value, or the right one when the left has none.
//!
//! "None" is what a missing JSON field, an empty `find` or the `min` of an
//! empty list reads as. Without this, every read of data that may be absent
//! needed a test written around it, and the natural one, `.len > 0`, only
//! works for lists.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// What `Text(expr)` draws inside a view with a few values in scope.
fn shows(expr: &str) -> String {
    let src = format!(
        "View Main() {{
    let user = {{ name: \"Ada\" }}
    let empty: List<Int> = []
    let xs = [3, 9, 4]
    var count: Int = 2
    Text(\"{{{expr}}}\")
}}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let checked = byard_compiler::infer::check_views(&parsed.views).errors;
    assert!(checked.is_empty(), "the type checker: {checked:?}");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 200.0);
    frame.texts()[0].text.clone()
}

#[test]
fn a_value_that_is_there_is_kept() {
    assert_eq!(shows("user.name ?? \"nobody\""), "Ada");
    assert_eq!(shows("count ?? 0"), "2");
}

#[test]
fn a_missing_field_takes_the_default() {
    assert_eq!(shows("user.email ?? \"no email\""), "no email");
}

#[test]
fn nothing_found_takes_the_default() {
    assert_eq!(shows("xs.find(x => x > 100) ?? -1"), "-1");
    assert_eq!(shows("empty.max() ?? 0"), "0");
    assert_eq!(shows("xs.find(x => x > 5) ?? -1"), "9");
}

/// The default is a value like any other, so a list defaulted to `[]` has a
/// length.
#[test]
fn a_defaulted_list_reads_as_a_list() {
    assert_eq!(shows("(user.tags ?? []).len"), "0");
}

/// It binds looser than arithmetic and tighter than comparison.
#[test]
fn it_sits_between_arithmetic_and_comparison() {
    assert_eq!(shows("user.age ?? 40 + 2"), "42");
    assert_eq!(shows("user.age ?? 0 == 0"), "true");
}

#[test]
fn defaults_chain() {
    assert_eq!(shows("user.nick ?? user.handle ?? user.name"), "Ada");
}

/// A default of the wrong kind is told, where both sides are known.
#[test]
fn a_default_of_another_type_is_a_type_mismatch() {
    let parsed =
        parse("View Main() {\n    var count: Int = 2\n    Text(\"{count ?? \\\"none\\\"}\")\n}");
    let errors = format!(
        "{:?}",
        byard_compiler::infer::check_views(&parsed.views).errors
    );
    assert!(
        errors.contains("TypeMismatch") && errors.contains("??"),
        "{errors}"
    );
}
