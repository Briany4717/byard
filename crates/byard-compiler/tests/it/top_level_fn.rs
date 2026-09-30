//! Top-level functions (RFC-0002): a pure helper declared once, at file
//! level, and called from any view of the project, or of another project
//! through its package's name.
//!
//! Everything goes through `resolve_program`, the path every command takes,
//! then renders, so a test that expects a value also proves the helper was
//! resolved, checked and inlined.

use std::collections::BTreeMap;

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::resolve::{PackageProvider, ResolvedProgram, SourceFile, resolve_program};
use byard_core::frame::RenderFrame;

/// Packages by name, each a list of `(file, source)`.
struct Packages(BTreeMap<&'static str, Vec<(&'static str, &'static str)>>);

impl PackageProvider for Packages {
    fn package_files(
        &mut self,
        _dependent: &str,
        package: &str,
    ) -> Result<Vec<SourceFile>, String> {
        self.0
            .get(package)
            .map(|files| {
                files
                    .iter()
                    .map(|(name, source)| SourceFile {
                        name: (*name).to_string(),
                        source: (*source).to_string(),
                    })
                    .collect()
            })
            .ok_or_else(|| "not declared".to_string())
    }
}

fn resolve_with(files: &[(&str, &str)], packages: Packages) -> ResolvedProgram {
    let mut packages = packages;
    resolve_program(
        files
            .iter()
            .map(|(name, source)| SourceFile {
                name: (*name).to_string(),
                source: (*source).to_string(),
            })
            .collect(),
        &mut packages,
    )
}

fn resolve(files: &[(&str, &str)]) -> ResolvedProgram {
    resolve_with(files, Packages(BTreeMap::new()))
}

/// Renders a resolved program's root view and returns its texts.
fn render(program: &ResolvedProgram) -> Vec<String> {
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let root = byard_compiler::parser::ast::root_view(&program.views).unwrap();
    let tree = interp.lower_view(root, &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

fn kinds(program: &ResolvedProgram) -> Vec<&'static str> {
    program.errors.iter().map(CompileError::kind).collect()
}

const CODES: &str = r#"
// The WMO weather code, in words.
fn sky(code: Int) -> Str => code == 0 ? "Clear" : code <= 3 ? "Cloudy" : "Rain"
fn shout(s: Str) -> Str => s + "!"
fn loud_sky(code: Int) -> Str => shout(sky(code))
"#;

#[test]
fn a_helper_in_one_file_is_called_from_a_view_in_another() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() {\n var code = 2\n Column { Text(sky(code)) Text(sky(0)) } }",
        ),
        ("codes.byd", CODES),
    ]);
    assert_eq!(render(&program), ["Cloudy", "Clear"]);
}

#[test]
fn a_helper_calls_another_helper() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(loud_sky(61)) }"),
        ("codes.byd", CODES),
    ]);
    assert_eq!(render(&program), ["Rain!"]);
}

#[test]
fn a_views_own_function_wins_over_a_helper_of_the_same_name() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() {\n fn sky(code: Int) -> Str => \"local\"\n Text(sky(0)) }",
        ),
        ("codes.byd", CODES),
    ]);
    assert_eq!(render(&program), ["local"]);
}

fn weather_package() -> Packages {
    Packages(BTreeMap::from([(
        "weather",
        vec![(
            "codes.byd",
            // `label` calls `sky` bare: inside the package that is its own
            // function, and the resolver canonicalises it.
            "fn sky(code: Int) -> Str => code == 0 ? \"Clear\" : \"Overcast\"\n\
             fn label(code: Int) -> Str => \"Sky: \" + sky(code)\n\
             View Badge(code: Int) { Text(label(code)) }",
        )],
    )]))
}

#[test]
fn a_package_function_is_called_through_its_alias() {
    let program = resolve_with(
        &[(
            "main.byd",
            "use weather as w\nView Main() { Column { Text(w.label(3)) w.Badge(code: 0) } }",
        )],
        weather_package(),
    );
    assert_eq!(render(&program), ["Sky: Overcast", "Sky: Clear"]);
}

#[test]
fn an_unknown_package_function_is_named_with_a_hint() {
    let program = resolve_with(
        &[(
            "main.byd",
            "use weather as w\nView Main() { Text(w.labl(3)) }",
        )],
        weather_package(),
    );
    let hint = program.errors.iter().find_map(|e| match e {
        CompileError::UnknownImportSymbol { name, hint, .. } if name == "labl" => hint.clone(),
        _ => None,
    });
    assert_eq!(hint.as_deref(), Some("label"), "{:?}", program.errors);
}

#[test]
fn a_helper_that_writes_or_reads_outside_its_parameters_is_rejected() {
    for helper in [
        // Writes state.
        "fn bump(n: Int) -> Int => { count = n }",
        // Reads a name that is not a parameter: the calling view's state.
        "fn peek(n: Int) -> Int => n + count",
        // Reaches a controller.
        "fn fetch(url: Str) -> Str => http.get(url)",
    ] {
        let program = resolve(&[
            (
                "main.byd",
                "View Main() {\n var count = 0\n Text(\"{count}\") }",
            ),
            ("helpers.byd", helper),
        ]);
        assert!(
            kinds(&program).contains(&"ImpureFunction"),
            "{helper}: {:?}",
            program.errors
        );
    }
}

#[test]
fn a_lambda_parameter_inside_a_helper_is_its_own() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(\"{total([1, 2, 3])}\") }"),
        (
            "helpers.byd",
            "fn total(xs: List<Int>) -> Int => xs.map(x => x * 10).len",
        ),
    ]);
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    assert_eq!(render(&program), ["3"]);
}

#[test]
fn recursion_is_reported_not_lowered_forever() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(\"{loop(3)}\") }"),
        ("helpers.byd", "fn loop(n: Int) -> Int => loop(n)"),
    ]);
    assert!(program.errors.is_empty(), "the checker allows the call");
    let mut interp = Interpreter::new();
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let _ = interp.lower_view(&program.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&[], &mut frame, 100.0, 100.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| e.kind() == "RecursiveFunction"),
        "{:?}",
        interp.errors()
    );
}

#[test]
fn the_same_helper_twice_in_one_project_is_an_error() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(sky(0)) }"),
        ("a.byd", "fn sky(code: Int) -> Str => \"a\""),
        ("b.byd", "fn sky(code: Int) -> Str => \"b\""),
    ]);
    assert!(
        kinds(&program).contains(&"DuplicateFunction"),
        "{:?}",
        program.errors
    );
}

#[test]
fn file_level_state_is_still_not_a_thing() {
    let program = resolve(&[("main.byd", "let shared = 1\nView Main() { Text(\"x\") }")]);
    assert!(
        kinds(&program).contains(&"UnexpectedToken"),
        "{:?}",
        program.errors
    );
}

#[test]
fn reloading_a_changed_helper_updates_its_callers() {
    let main = ("main.byd", "View Main() { Text(sky(0)) }");
    let before = resolve(&[main, ("codes.byd", CODES)]);
    let after = resolve(&[main, ("codes.byd", "fn sky(code: Int) -> Str => \"Sunny\"")]);

    // What the dev runner does on a save: load the new program's views,
    // which carry the new function table, and lower the root again.
    let mut interp = Interpreter::new();
    interp.load_views(&before.views);
    let known: Vec<&str> = before.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&before.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    assert_eq!(frame.texts()[0].text, "Clear");

    interp.load_views(&after.views);
    let tree = interp.lower_view(&after.views[0], &known);
    interp.tick();
    frame.clear();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    assert_eq!(frame.texts()[0].text, "Sunny");
}

/// Both parameters of a two-parameter lambda are the lambda's own names, not
/// free names the purity check has to reject.
#[test]
fn a_helper_can_reduce_with_a_two_parameter_lambda() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(\"{total([1, 2, 3])}\") }"),
        (
            "helpers.byd",
            "fn total(xs: List<Int>) -> Int => xs.reduce(0, (acc, x) => acc + x)",
        ),
    ]);
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    assert_eq!(render(&program), ["6"]);
}
