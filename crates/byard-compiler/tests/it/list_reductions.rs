//! RFC-0027's second batch of list operations: `sort`, `sortBy`, `reduce`,
//! `find`, `indexOf`, `min` and `max`, the ones a chart or a summary needs.
//!
//! Every source goes through `resolve_program` first, the path `byard check`
//! takes, so a test that expects a value also proves the checker knows the
//! operation; then the view is rendered and its texts read back.

use std::fmt::Write as _;

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::resolve::{PackageProvider, ResolvedProgram, SourceFile, resolve_program};
use byard_core::frame::RenderFrame;

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

fn resolve(body: &str) -> ResolvedProgram {
    resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: format!("View Main() {{\n{body}\n}}"),
        }],
        &mut NoPackages,
    )
}

/// Checks `body`, requires it clean, renders it and returns every text drawn.
fn texts(body: &str) -> Vec<String> {
    let program = resolve(body);
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&program.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

/// The elements of the list `expr` evaluates to, one `Text` each, joined
/// with a space: a list drawn as a single string would draw nothing.
fn list(lets: &str, expr: &str) -> String {
    texts(&format!(
        "{lets}\nColumn {{\n for e in {expr} {{ Text(\"{{e}}\") }} }}"
    ))
    .join(" ")
}

/// One `Text` per expression, in order.
fn show(lets: &str, exprs: &[&str]) -> Vec<String> {
    let lines = exprs.iter().fold(String::new(), |mut out, e| {
        let _ = writeln!(out, "Text(\"{{{e}}}\")");
        out
    });
    texts(&format!("{lets}\nColumn {{\n{lines}}}"))
}

#[test]
fn sort_orders_numbers_and_text() {
    let lets = "let ints = [3, 1, 2]\n\
                let words = [\"pear\", \"apple\", \"fig\"]\n\
                let floats = [2.5, 1.25, 3.0]";
    assert_eq!(list(lets, "ints.sort()"), "1 2 3");
    assert_eq!(list(lets, "words.sort()"), "apple fig pear");
    assert_eq!(list(lets, "floats.sort()"), "1.25 2.5 3");
    assert_eq!(
        list(lets, "ints"),
        "3 1 2",
        "a sorted copy; the list is unchanged"
    );
}

#[test]
fn sort_by_orders_records_by_a_key_and_keeps_ties_in_place() {
    let lets = "let days = [\n\
                    { day: \"Mon\", high: 22 },\n\
                    { day: \"Tue\", high: 19 },\n\
                    { day: \"Wed\", high: 22 },\n\
                    { day: \"Thu\", high: 17 }\n\
                ]";
    assert_eq!(
        list(lets, "days.sortBy(d => d.high).map(d => d.day)"),
        "Thu Tue Mon Wed",
        "Mon stays before Wed"
    );
}

#[test]
fn reduce_folds_with_an_accumulator() {
    let drawn = show(
        "let xs = [1, 2, 3, 4]\n\
         let words = [\"a\", \"b\", \"c\"]",
        &[
            "xs.reduce(0, (acc, x) => acc + x)",
            "xs.reduce(1, (acc, x) => acc * x)",
            "words.reduce(\"\", (acc, w) => acc + w)",
            "[].reduce(7, (acc, x) => acc + x)",
        ],
    );
    assert_eq!(drawn, ["10", "24", "abc", "7"]);
}

#[test]
fn a_lambda_inside_reduce_reads_its_own_parameter() {
    // The inner `x` is the `map`'s, not the `reduce`'s.
    let drawn = show(
        "let rows = [[1, 2], [3, 4]]",
        &["rows.reduce(0, (acc, x) => acc + x.map(x => x * 10).reduce(0, (a, y) => a + y))"],
    );
    assert_eq!(drawn, ["100"]);
}

#[test]
fn find_returns_the_first_match_or_nothing() {
    let drawn = show(
        "let temps = [12, 18, 25, 31]\n\
         let cities = [{ name: \"Lima\", hot: false }, { name: \"Doha\", hot: true }]",
        &[
            "temps.find(t => t > 20)",
            "temps.find(t => t > 40)",
            "cities.find(c => c.hot).name",
        ],
    );
    assert_eq!(drawn, ["25", "", "Doha"], "no match is Unit, drawn empty");
}

#[test]
fn index_of_finds_structurally_equal_values() {
    let drawn = show(
        "let xs = [\"a\", \"b\", \"c\"]\n\
         let points = [{ x: 1 }, { x: 2 }]",
        &[
            "xs.indexOf(\"c\")",
            "xs.indexOf(\"z\")",
            "points.indexOf({ x: 2 })",
        ],
    );
    assert_eq!(drawn, ["2", "-1", "1"]);
}

#[test]
fn min_and_max_pick_by_value_and_an_empty_list_has_neither() {
    let drawn = show(
        "let highs = [22.4, 19.0, 26.8, 17.2]\n\
         let none: List<Int> = []",
        &[
            "highs.min()",
            "highs.max()",
            "[\"pear\", \"apple\"].min()",
            "none.max()",
        ],
    );
    assert_eq!(drawn, ["17.2", "26.8", "apple", ""]);
}

#[test]
fn the_checker_knows_every_new_operation() {
    // A misspelling of each is caught and pointed at the real name, which is
    // only possible if the checker's list includes it.
    for (typo, real) in [
        ("sortby", "sortBy"),
        ("redcue", "reduce"),
        ("indexof", "indexOf"),
        ("mx", "max"),
    ] {
        let program = resolve(&format!("let xs = [1, 2]\nText(\"{{xs.{typo}()}}\")"));
        let hint = program.errors.iter().find_map(|e| match e {
            CompileError::UnknownMethod { hint, .. } => hint.clone(),
            _ => None,
        });
        assert_eq!(hint.as_deref(), Some(real), "{typo}: {:?}", program.errors);
    }
}

#[test]
fn an_impure_lambda_is_rejected_in_every_new_position() {
    for body in [
        "xs.sortBy(x => { n = x })",
        "xs.find(x => { n = x })",
        "xs.reduce(0, (acc, x) => { n = x })",
    ] {
        let program = resolve(&format!(
            "var n = 0\nlet xs = [1, 2]\nlet r = {body}\nText(\"{{n}}\")"
        ));
        assert!(
            program
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::EffectInPureLambda { .. })),
            "{body}: {:?}",
            program.errors
        );
    }
}

#[test]
fn find_needs_a_boolean_condition() {
    let program = resolve("let xs = [1, 2]\nlet r = xs.find(x => \"yes\")\nText(\"{r}\")");
    assert!(
        program
            .errors
            .iter()
            .any(|e| matches!(e, CompileError::PredicateNotBool { .. })),
        "{:?}",
        program.errors
    );
}
