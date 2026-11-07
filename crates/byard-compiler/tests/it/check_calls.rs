//! Checking a program lowers the body of a view once for the calls that are
//! written alike, not once per call, and every diagnostic is still raised.
//!
//! A gallery of a hundred buttons took a hundred times a button's body to
//! check; the calls are now each checked against the signature, and the body
//! once for each shape of call.

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// Lowers and renders `Main` as `byard check` does; returns the texts drawn and
/// the errors, in `checking` mode or the way an app runs.
fn lower(src: &str, checking: bool) -> (Vec<String>, Vec<CompileError>) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interpreter::new();
    interp.set_checking(checking);
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let main = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == "Main")
        .expect("a `Main` view");
    let tree = interp.lower_view(main, &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 900.0);
    interp.lower_every_branch();
    let texts = frame.texts().iter().map(|t| t.text.clone()).collect();
    (texts, interp.errors().to_vec())
}

fn many(call: &str, n: usize) -> String {
    let calls = (0..n)
        .map(|i| call.replace('#', &i.to_string()))
        .collect::<Vec<_>>();
    format!(
        "View Card(title: Str, big: Bool = false) {{ Text(title) #[size: big ? 20 : 12] }}\n\
         View Main() {{ Column {{ {} }} }}",
        calls.join("\n")
    )
}

#[test]
fn calls_written_alike_lower_the_body_once() {
    let src = many("Card(\"card #\")", 50);
    let (running, _) = lower(&src, false);
    let (checking, errors) = lower(&src, true);
    assert_eq!(running.len(), 50, "an app draws every instance");
    assert_eq!(
        checking.len(),
        1,
        "check lowers the body once: {checking:?}"
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn calls_written_differently_each_lower_their_body() {
    let src = "View Card(title: Str, big: Bool = false) { Text(title) #[size: big ? 20 : 12] }\n\
               View Main() { Column {\n\
                   Card(\"a\")\n\
                   Card(\"\")\n\
                   Card(\"b\", big: true)\n\
                   Card(\"c\", big: false)\n\
                   Card(\"d\", big: false)\n\
               } }";
    let (checking, errors) = lower(src, true);
    // A text, an empty text, `big: true`, `big: false`; the last repeats the
    // one before.
    assert_eq!(checking.len(), 4, "{checking:?}");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_call_is_checked_against_the_signature_every_time() {
    let src = many("Card(\"x\", wide: #)", 5);
    let (_, errors) = lower(&src, true);
    let unknown = errors
        .iter()
        .filter(|e| matches!(e, CompileError::UnknownParam { .. }))
        .count();
    assert_eq!(unknown, 5, "{errors:?}");
    let missing = lower(&many("Card(big: true)", 5), true).1;
    let missing = missing
        .iter()
        .filter(|e| matches!(e, CompileError::MissingParam { .. }))
        .count();
    assert_eq!(missing, 5);
}

#[test]
fn the_errors_of_a_program_do_not_change() {
    // An error in the body of the called view, in a callback written in a
    // call, in a block passed to a view, and in the signature.
    let src = "View Card(title: Str, on_tap: Fn() = {}) {\n\
                   Text(title) #[bogus: 1]\n\
                   Box #[tap => on_tap()] { Text(missing_in_body) }\n\
               }\n\
               View Wrap(content) { Column { content } }\n\
               View Main() {\n\
                   var n = 0\n\
                   Column {\n\
                       Card(\"a\", on_tap: { n++ })\n\
                       Card(\"b\", on_tap: { n++ })\n\
                       Card(\"c\", on_tap: { nothing_here++ })\n\
                       Card(\"d\", on_tap: { nothing_here++ })\n\
                       Card()\n\
                       Card(\"e\", extra: 1)\n\
                       Wrap { Text(oops) }\n\
                       Wrap { Text(oops) }\n\
                   }\n\
               }";
    let (_, running) = lower(src, false);
    let (_, checking) = lower(src, true);
    assert_ne!(running.len(), 0);
    let dedup = |errors: Vec<CompileError>| {
        let mut out: Vec<CompileError> = Vec::new();
        for e in errors {
            if !out.contains(&e) {
                out.push(e);
            }
        }
        out
    };
    let (running, checking) = (dedup(running), dedup(checking));
    for e in &running {
        assert!(
            checking.contains(e),
            "checking lost {e:?}\nhas {checking:?}"
        );
    }
}
