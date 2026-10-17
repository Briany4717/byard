//! `text.formatDate(pattern)` from `byld`: an ISO date written out, and
//! nothing for text that is not one.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

fn shows(expr: &str) -> String {
    let src = format!(
        "View Main() {{\n    let day = \"2026-10-16\"\n    let hour = \"2026-10-16T09:30\"\n    Text(\"{{{expr}}}\")\n}}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let checked = byard_compiler::infer::check_views(&parsed.views).errors;
    assert!(checked.is_empty(), "{checked:?}");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 600.0, 100.0);
    frame.texts()[0].text.clone()
}

#[test]
fn a_date_and_a_time_are_written_out() {
    assert_eq!(shows("day.formatDate(\"ddd D MMM\")"), "Fri 16 Oct");
    assert_eq!(shows("hour.formatDate(\"HH:mm\")"), "09:30");
}

#[test]
fn text_that_is_not_a_date_is_nothing_and_takes_a_default() {
    assert_eq!(shows("\"soon\".formatDate(\"D\") ?? \"—\""), "—");
}

#[test]
fn the_checker_knows_it() {
    let parsed =
        parse("View Main() {\n    let day = \"2026-10-16\"\n    Text(day.formatDat(\"D\"))\n}");
    let errors = format!(
        "{:?}",
        byard_compiler::infer::check_views(&parsed.views).errors
    );
    assert!(errors.contains("formatDate"), "a suggestion: {errors}");
}
