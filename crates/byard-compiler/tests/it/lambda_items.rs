//! A lambda reads its own item, however it is nested and whatever it calls.
//!
//! A `fn` called with the item of a list lambda runs once per item (not once
//! for the first), and a lambda inside a lambda reads the outer one's item.
//! Each source goes through `resolve_program` first, the path `byard check`
//! takes, then is rendered and its texts read back.

use std::fmt::Write as _;

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::resolve::{PackageProvider, SourceFile, resolve_program};
use byard_core::frame::RenderFrame;

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(&mut self, _: &str, _: &str) -> Result<Vec<SourceFile>, String> {
        Err("no packages in this test".to_string())
    }
}

/// Renders `source` (a whole file whose first view is drawn) and returns every
/// text drawn, in order.
fn texts(source: &str) -> Vec<String> {
    let program = resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: source.to_string(),
        }],
        &mut NoPackages,
    );
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

const PRELUDE: &str = "type P = { qty: Int, cat: Str }\n\
    fn big(qty: Int) -> Bool => qty > 10\n\
    fn plus(qty: Int, by: Int) -> Int => qty + by\n\
    fn twice(qty: Int) -> Int => plus(qty, qty)\n";

/// `PRELUDE`, then `View Main` with the shared list `ps` and one `Text` per line.
fn show(extra: &str, lines: &[&str]) -> Vec<String> {
    let body = lines.iter().fold(String::new(), |mut out, l| {
        let _ = writeln!(out, "Text(\"{{{l}}}\")");
        out
    });
    texts(&format!(
        "{PRELUDE}View Main() {{\n\
         var ps: List<P> = [{{ qty: 50, cat: \"a\" }}, {{ qty: 3, cat: \"b\" }}, {{ qty: 20, cat: \"a\" }}]\n\
         {extra}\nColumn {{\n{body}}}\n}}"
    ))
}

#[test]
fn a_shared_fn_called_with_the_item_runs_per_item() {
    let drawn = show(
        "",
        &[
            "ps.filter(p => big(p.qty)).len",
            "ps.map(p => plus(p.qty, 100)).join(\",\")",
            "ps.map(p => twice(p.qty)).join(\",\")",
        ],
    );
    assert_eq!(drawn, ["2", "150,103,120", "100,6,40"]);
}

#[test]
fn a_view_local_fn_called_with_the_item_runs_per_item() {
    let drawn = show(
        "fn small(q: Int) -> Bool => q < 10\nfn add(q: Int, by: Int) -> Int => q + by",
        &[
            "ps.filter(p => small(p.qty)).len",
            "ps.map(p => add(p.qty, 1)).join(\",\")",
        ],
    );
    assert_eq!(drawn, ["1", "51,4,21"]);
}

#[test]
fn a_fn_outside_any_lambda_still_works() {
    let drawn = texts(&format!(
        "{PRELUDE}View Main() {{\nvar n = 5\nlet r = plus(n, 1)\nColumn {{ Text(\"{{r}}\")\n\
         Button(\"go\") => n = 9 }}\n}}"
    ));
    assert_eq!(drawn[0], "6");
}

#[test]
fn a_lambda_inside_a_lambda_reads_the_outer_item() {
    let drawn = show(
        "",
        &[
            "[\"a\", \"b\"].map(k => ps.filter(p => p.cat == k).len).join(\",\")",
            // The same name inside: the inner one shadows.
            "[1, 2].map(x => [10, 20].map(x => x + 1).join(\"-\")).join(\",\")",
            // Three deep, reading both outer items.
            "[1, 2].map(a => [10, 20].map(b => [100].map(c => a + b + c).join(\"\")).join(\"-\")).join(\",\")",
        ],
    );
    assert_eq!(drawn, ["2,1", "11-21,11-21", "111-121,112-122"]);
}

#[test]
fn two_parameter_lambdas_nest_in_a_map_and_hold_the_outer_item() {
    let drawn = show(
        "",
        &[
            "[\"a\", \"b\"].map(k => ps.filter(p => p.cat == k).reduce(0, (acc, p) => acc + p.qty)).join(\",\")",
            "[1, 2].map(m => ps.reduce(0, (acc, p) => acc + p.qty * m)).join(\",\")",
        ],
    );
    assert_eq!(drawn, ["70,3", "73,146"]);
}

#[test]
fn sort_by_and_find_nest_and_read_the_outer_item() {
    let drawn = show(
        "",
        &[
            "[\"a\", \"b\"].map(k => ps.find(p => p.cat == k).qty).join(\",\")",
            "[1, -1].map(s => ps.sortBy(p => p.qty * s).map(p => p.qty).join(\"/\")).join(\",\")",
            "[\"b\"].map(k => ps.sortBy(p => p.qty).find(p => p.cat != k).qty).join(\",\")",
        ],
    );
    assert_eq!(drawn, ["50,3", "3/20/50,50/20/3", "20"]);
}

#[test]
fn a_fn_called_with_the_item_from_a_nested_lambda() {
    let drawn = show(
        "",
        &[
            "[\"a\", \"b\"].map(k => ps.filter(p => p.cat == k).map(p => plus(p.qty, 1)).join(\"/\")).join(\",\")",
            "[1, 2].map(m => ps.filter(p => big(p.qty * m)).len).join(\",\")",
        ],
    );
    assert_eq!(drawn, ["51/21,4", "2,2"]);
}

#[test]
fn a_for_variable_is_read_by_a_lambda_in_the_loop_body() {
    let drawn = texts(&format!(
        "{PRELUDE}View Main() {{\n\
         var ps: List<P> = [{{ qty: 50, cat: \"a\" }}, {{ qty: 3, cat: \"b\" }}]\n\
         Column {{ for k in [\"a\", \"b\"] {{\n\
         Text(\"{{k}}={{ps.filter(p => p.cat == k).len}}/{{ps.map(p => plus(p.qty, k.len)).join(\",\")}}\")\n\
         }} }}\n}}"
    ));
    assert_eq!(drawn, ["a=1/51,4", "b=1/51,4"]);
}
