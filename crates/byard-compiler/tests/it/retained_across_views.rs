//! Lowering a different view is a new tree, and the retained layout pass
//! (RFC-0032 §R4) must not try to reuse the last one's.
//!
//! `byard check` renders every view of a program through one interpreter, a
//! package's views included. Before this, the second view's frame opened a
//! retained build against the first view's layout, and on a real package
//! (a component catalog of thirty files) that reached a debug assertion about
//! mismatched child lists and aborted the check. The assertion was right: the
//! walk had no business comparing two unrelated trees. What was missing is
//! that lowering another view says so.
//!
//! The control is the HUD's case: the *same* view re-lowered keeps its
//! retained eligibility, since that is what makes its periodic refresh cheap.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::atlas::layout::path_counters;
use byard_core::frame::RenderFrame;

const SRC: &str = r#"
View A() { Column #[gap: 4] { Box #[width: 10, height: 10] {} Box #[width: 20, height: 10] {} } }
View B() { Column #[gap: 4] { Text("b") Box #[width: 20, height: 10] {} } }
"#;

fn frame(interp: &mut Interpreter, tree: &[RenderNode]) -> path_counters::Counts {
    path_counters::reset();
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, 400.0, 300.0);
    path_counters::snapshot()
}

#[test]
fn lowering_another_view_does_not_retain_the_last_ones_layout() {
    let parsed = parse(SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);

    let a = interp.lower_view(&parsed.views[0], &known);
    let _ = frame(&mut interp, &a);
    let steady = frame(&mut interp, &a);
    assert_eq!(
        steady.retained_attempts, 1,
        "the probe: a steady frame retains"
    );

    // The HUD's case: the same view lowered again stays eligible.
    let a_again = interp.lower_view(&parsed.views[0], &known);
    let again = frame(&mut interp, &a_again);
    assert_eq!(
        again.retained_attempts, 1,
        "re-lowering the same view keeps the retained path"
    );

    // `check`'s case: a different view is a different tree.
    let b = interp.lower_view(&parsed.views[1], &known);
    let other = frame(&mut interp, &b);
    assert_eq!(
        other.retained_attempts, 0,
        "a different view must not open a retained build against the last one"
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
}
