//! `let` inside an action block: a name for a value, held for the statements
//! after it. With a record type it is checked like a `var` of one: its fields
//! are known to `byard check`, and what arrives is compared with the type.

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::resolve::{PackageProvider, SourceFile, resolve_program};
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(
        &mut self,
        _dependent: &str,
        _package: &str,
    ) -> Result<Vec<SourceFile>, String> {
        Err("no packages in this test".to_string())
    }
}

/// Renders `src`, taps its button `taps` times, and returns the texts shown
/// and the errors the interpreter reported.
fn run(src: &str, taps: usize) -> (Vec<String>, Vec<CompileError>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 200.0);
    for t in 0..taps {
        let at = |kind, time| InputEvent {
            kind,
            pos: (20.0, 20.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: time,
            mods: byard_core::KeyModifiers::default(),
        };
        let time = 100 + t as u64 * 100;
        interp.dispatch_events(&[
            at(EventKind::PointerDown, time),
            at(EventKind::PointerUp, time + 8),
        ]);
        interp.tick();
        frame.clear();
        interp.render(&tree, &mut frame, 400.0, 200.0);
    }
    let texts = frame.texts().iter().map(|t| t.text.clone()).collect();
    (texts, interp.errors().to_vec())
}

/// What `byard check` says about `src`.
fn check(src: &str) -> Vec<String> {
    resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: src.to_string(),
        }],
        &mut NoPackages,
    )
    .errors
    .iter()
    .map(CompileError::headline)
    .collect()
}

#[test]
fn a_let_holds_its_value_for_the_statements_after_it() {
    let (texts, errors) = run(
        "View Main() {
    var count: Int = 1
    Button(\"go\") #[width: 100, height: 40] => {
        let next = count + 1
        let twice = next * 2
        count = twice + next
    }
    Text(\"{count}\")
}",
        2,
    );
    assert!(errors.is_empty(), "{errors:?}");
    // 1 -> (2*2 + 2) = 6 -> (7*2 + 7) = 21
    assert_eq!(texts.last().map(String::as_str), Some("21"), "{texts:?}");
}

/// A `let` reads the names before it, its own name included: `let count =
/// count + 1` is the old `count`, and the `var` is untouched by it.
#[test]
fn a_let_may_shadow_and_reads_what_came_before() {
    let (texts, errors) = run(
        "View Main() {
    var count: Int = 5
    var shown: Int = 0
    Button(\"go\") #[width: 100, height: 40] => {
        let count = count + 1
        shown = count
    }
    Text(\"{count} {shown}\")
}",
        1,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(texts.last().map(String::as_str), Some("5 6"), "{texts:?}");
}

/// A lifecycle block binds names like a handler does.
#[test]
fn a_let_in_on_mount() {
    let (texts, errors) = run(
        "View Main() {
    var greeting: Str = \"\"
    on mount => {
        let who = \"world\"
        greeting = \"hello {who}\"
    }
    Text(greeting)
}",
        0,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(texts, ["hello world"]);
}

#[test]
fn a_typed_let_knows_its_fields() {
    let errors = check(
        "type Reply = { results: List<Str>? }
View Main() {
    var found: List<Str> = []
    Button(\"go\") => {
        let reply: Reply = { results: [\"a\"] }
        found = reply.resluts ?? []
    }
}",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("resluts") && errors[0].contains("results"),
        "{}",
        errors[0]
    );
}

/// A `let`'s name ends with its block.
#[test]
fn a_let_does_not_outlive_its_block() {
    let errors = check(
        "View Main() {
    var n: Int = 0
    Button(\"a\") => { let step = 2 n = n + step }
    Button(\"b\") => { n = n + step }
    Button(\"c\") => {
        if n > 0 { let jump = 3 n = n + jump }
        n = n + jump
    }
}",
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].contains("step"), "{}", errors[0]);
    assert!(errors[1].contains("jump"), "{}", errors[1]);
}

/// What arrives in a typed `let` is compared with the type, once.
#[test]
fn a_typed_let_compares_what_arrives() {
    let (_, errors) = run(
        "type Place = { name: Str, country: Str }
View Main() {
    var shown: Str = \"\"
    Button(\"go\") #[width: 100, height: 40] => {
        let place: Place = { name: \"Lis\" }
        shown = place.name
    }
    Text(shown)
}",
        3,
    );
    let told: Vec<String> = errors
        .iter()
        .filter(|e| matches!(e, CompileError::ShapeMismatch { .. }))
        .map(CompileError::headline)
        .collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("place") && told[0].contains("country"),
        "{}",
        told[0]
    );
}
