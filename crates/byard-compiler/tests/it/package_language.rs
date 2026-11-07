//! Constants, enums, private names and `else when` (RFC-0055).
//!
//! Everything goes through `resolve_program`, the path every command takes,
//! then renders, so a test that expects a value also proves the name was
//! resolved, checked and evaluated.

use std::collections::BTreeMap;

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::resolve::{PackageProvider, ResolvedProgram, SourceFile, resolve_program};
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

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

fn resolve_with(files: &[(&str, &str)], mut packages: Packages) -> ResolvedProgram {
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

/// Renders a resolved program's root view, tapping the button at (20, 20)
/// `taps` times, and returns its texts.
fn render_after(program: &ResolvedProgram, taps: usize) -> Vec<String> {
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let root = byard_compiler::parser::ast::root_view(&program.views).unwrap();
    let tree = interp.lower_view(root, &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
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
        interp.render(&tree, &mut frame, 400.0, 400.0);
    }
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

fn render(program: &ResolvedProgram) -> Vec<String> {
    render_after(program, 0)
}

fn kinds(program: &ResolvedProgram) -> Vec<&'static str> {
    program.errors.iter().map(CompileError::kind).collect()
}

// ── Constants ───────────────────────────────────────────────────────────────

#[test]
fn a_constant_is_read_by_a_view_in_another_file() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(\"gap {gap}\") }"),
        ("tokens.byd", "let gap = 8"),
    ]);
    assert_eq!(render(&program), ["gap 8"]);
}

#[test]
fn a_constant_is_a_value_for_a_property() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { Column #[gap: gap, p: pad] { Text(\"a\") Text(\"b\") } }",
        ),
        ("tokens.byd", "let gap = 8\nlet pad = gap * 2"),
    ]);
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&program.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    let ys: Vec<f32> = frame.texts().iter().map(|t| t.y).collect();
    let xs: Vec<f32> = frame.texts().iter().map(|t| t.x).collect();
    // The padding is the constant's value: 16.
    assert!((xs[0] - 16.0).abs() < 0.5, "{xs:?}");
    // And the gap, 8, separates the two lines.
    assert!(ys[1] - ys[0] > 8.0, "{ys:?}");
}

#[test]
fn constants_may_read_each_other_in_any_order_across_files() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(\"{a} {b} {c}\") }"),
        ("one.byd", "let c = b + 1\nlet a = 1"),
        ("two.byd", "let b = a + 1"),
    ]);
    assert_eq!(render(&program), ["1 2 3"]);
}

#[test]
fn a_constant_may_call_a_function() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(\"{twice}\") }"),
        (
            "tokens.byd",
            "fn double(n: Int) -> Int => n * 2\nlet twice = double(base)\nlet base = 4",
        ),
    ]);
    assert_eq!(render(&program), ["8"]);
}

#[test]
fn constants_that_read_each_other_in_a_circle_are_an_error_with_the_path() {
    let program = resolve(&[(
        "main.byd",
        "let gap = pad\nlet pad = gap\nView Main() { Text(\"x\") }",
    )]);
    let cycle = program
        .errors
        .iter()
        .find_map(|e| match e {
            CompileError::ConstantCycle { path, .. } => Some(path.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no cycle reported: {:?}", program.errors));
    assert_eq!(cycle, "gap → pad → gap");
}

#[test]
fn a_cycle_through_a_function_is_found_too() {
    let program = resolve(&[(
        "main.byd",
        "fn f() -> Int => a\nlet a = f()\nView Main() { Text(\"x\") }",
    )]);
    assert!(
        kinds(&program).contains(&"ConstantCycle"),
        "{:?}",
        program.errors
    );
}

#[test]
fn a_constant_reading_itself_is_a_cycle() {
    let program = resolve(&[("main.byd", "let n = n + 1\nView Main() { Text(\"x\") }")]);
    assert!(
        kinds(&program).contains(&"ConstantCycle"),
        "{:?}",
        program.errors
    );
}

#[test]
fn a_constant_is_typed_by_its_value_and_by_its_annotation() {
    let ok = resolve(&[(
        "main.byd",
        "let ratio: Float = 2\nlet label: Str = \"x\"\nView Main() { Text(label) }",
    )]);
    assert!(ok.errors.is_empty(), "{:?}", ok.errors);
    let bad = resolve(&[(
        "main.byd",
        "let n: Int = \"x\"\nView Main() { Text(\"x\") }",
    )]);
    assert!(
        bad.errors.iter().any(|e| matches!(
            e,
            CompileError::ConstantTypeMismatch { name, expected, found, .. }
                if name == "n" && expected == "Int" && found == "Str"
        )),
        "{:?}",
        bad.errors
    );
}

#[test]
fn a_constant_is_checked_against_where_it_is_used() {
    let program = resolve(&[(
        "main.byd",
        "let gap = 8\nView Main() { let wrong = gap.nope()\n Text(\"x\") }",
    )]);
    assert!(
        kinds(&program).contains(&"UnknownMethod"),
        "a constant's type is known to the checker: {:?}",
        program.errors
    );
}

#[test]
fn a_constant_must_be_pure() {
    for init in ["count", "now()", "items.push(1)", "nowhere"] {
        let program = resolve(&[(
            "main.byd",
            &format!(
                "let c = {init}\nView Main() {{ var count = 0\n var items: List<Int> = [1]\n Text(\"x\") }}"
            ),
        )]);
        assert!(
            !program.errors.is_empty(),
            "`let c = {init}` should be refused"
        );
    }
    let assign = resolve(&[("main.byd", "let c = { n = 1 }\nView Main() { Text(\"x\") }")]);
    assert!(
        kinds(&assign).contains(&"ImpureConstant"),
        "{:?}",
        assign.errors
    );
}

#[test]
fn a_constant_of_two_files_with_the_same_name_is_an_error() {
    let program = resolve(&[
        ("a.byd", "let gap = 8\nView Main() { Text(\"x\") }"),
        ("b.byd", "let gap = 9"),
    ]);
    assert!(
        kinds(&program).contains(&"DuplicateConstant"),
        "{:?}",
        program.errors
    );
}

#[test]
fn what_a_view_binds_hides_the_constant_of_that_name() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { Column { Pair(3) Text(\"{gap}\") } }\nView Pair(gap: Int) { Text(\"{gap}\") }",
        ),
        ("tokens.byd", "let gap = 8"),
    ]);
    assert_eq!(render(&program), ["3", "8"]);
}

// ── Enums ───────────────────────────────────────────────────────────────────

const VARIANT: &str = "enum Variant { filled, tonal, outlined }";

#[test]
fn an_enum_case_compares_chooses_and_interpolates() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() {\n var v = Variant.tonal\n Text(\"{v}\")\n Text(v == Variant.tonal ? \"yes\" : \"no\")\n Text(v != Variant.filled ? \"other\" : \"same\") }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert_eq!(render(&program), ["tonal", "yes", "other"]);
}

#[test]
fn a_misspelt_case_is_an_error_with_a_suggestion() {
    let program = resolve(&[
        ("main.byd", "View Main() { Text(\"{Variant.tonl}\") }"),
        ("tokens.byd", VARIANT),
    ]);
    assert!(
        program.errors.iter().any(|e| matches!(
            e,
            CompileError::UnknownEnumCase { enum_name, name, hint, .. }
                if enum_name == "Variant" && name == "tonl" && hint.as_deref() == Some("tonal")
        )),
        "{:?}",
        program.errors
    );
}

#[test]
fn a_misspelt_case_in_a_comparison_is_told_not_silently_false() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { var v = Variant.tonal\n Text(v == Variant.filed ? \"a\" : \"b\") }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert!(
        kinds(&program).contains(&"UnknownEnumCase"),
        "{:?}",
        program.errors
    );
}

#[test]
fn a_parameter_typed_with_an_enum_takes_its_cases_with_a_default() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { Column { Chip(\"a\") Chip(\"b\", variant: Variant.outlined) Chip(\"c\", Variant.tonal) } }",
        ),
        (
            "chip.byd",
            "View Chip(label: Str, variant: Variant = Variant.filled) { Text(\"{label}={variant}\") }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert_eq!(render(&program), ["a=filled", "b=outlined", "c=tonal"]);
}

#[test]
fn the_short_form_names_the_case_where_the_parameter_gives_the_type() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { Column { Chip(\"a\", variant: .tonal) Chip(\"b\", .outlined) Chip(\"c\", variant: true ? .tonal : .filled) } }",
        ),
        (
            "chip.byd",
            "View Chip(label: Str, variant: Variant = .filled) { Text(\"{label}={variant}\") }\nView Plain(label: Str = .filled) { Text(\"x\") }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    // `Plain`'s default is a class reference on a `Str`, which is left alone.
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    assert_eq!(render(&program), ["a=tonal", "b=outlined", "c=tonal"]);
}

#[test]
fn a_misspelt_short_form_is_a_misspelt_case() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { Chip(\"a\", variant: .tonl) }\nView Chip(label: Str, variant: Variant = Variant.filled) { Text(label) }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert!(
        program.errors.iter().any(|e| matches!(
            e,
            CompileError::UnknownEnumCase { name, hint, .. }
                if name == "tonl" && hint.as_deref() == Some("tonal")
        )),
        "{:?}",
        program.errors
    );
}

#[test]
fn a_misspelt_default_is_told_in_the_declaration() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { Chip(\"a\") }\nView Chip(label: Str, variant: Variant = Variant.fild) { Text(label) }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert!(
        kinds(&program).contains(&"UnknownEnumCase"),
        "{:?}",
        program.errors
    );
}

#[test]
fn a_string_is_not_an_enum() {
    let argument = resolve(&[
        (
            "main.byd",
            "View Main() { Chip(\"a\", variant: \"tonal\") }\nView Chip(label: Str, variant: Variant = Variant.filled) { Text(label) }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert!(
        kinds(&argument).contains(&"ArgumentTypeMismatch"),
        "{:?}",
        argument.errors
    );
    let comparison = resolve(&[
        (
            "main.byd",
            "View Main() { var v = Variant.tonal\n Text(v == \"tonal\" ? \"a\" : \"b\") }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert!(
        kinds(&comparison).contains(&"TypeMismatch"),
        "{:?}",
        comparison.errors
    );
}

#[test]
fn two_enums_are_not_comparable() {
    let program = resolve(&[(
        "main.byd",
        "enum A { x }\nenum B { x }\nView Main() { Text(A.x == B.x ? \"a\" : \"b\") }",
    )]);
    assert!(
        kinds(&program).contains(&"TypeMismatch"),
        "{:?}",
        program.errors
    );
}

#[test]
fn the_short_form_in_a_comparison_says_to_write_the_enum() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() { var v = Variant.tonal\n Text(v == .tonal ? \"a\" : \"b\") }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert!(
        program.errors.iter().any(|e| matches!(
            e,
            CompileError::EnumShorthand { case, enum_name, .. }
                if case == "tonal" && enum_name.as_deref() == Some("Variant")
        )),
        "{:?}",
        program.errors
    );
}

#[test]
fn an_enum_declared_twice_or_with_a_case_twice_is_an_error() {
    let twice = resolve(&[(
        "main.byd",
        "enum E { a }\nenum E { b }\nView Main() { Text(\"x\") }",
    )]);
    assert!(
        kinds(&twice).contains(&"DuplicateEnum"),
        "{:?}",
        twice.errors
    );
    let case = resolve(&[(
        "main.byd",
        "enum E { a, b, a }\nView Main() { Text(\"x\") }",
    )]);
    assert!(
        case.errors.iter().any(|e| matches!(
            e,
            CompileError::DuplicateEnumCase { enum_name, name, .. } if enum_name == "E" && name == "a"
        )),
        "{:?}",
        case.errors
    );
}

#[test]
fn cases_may_be_separated_by_lines_and_end_with_a_comma() {
    let program = resolve(&[(
        "main.byd",
        "enum Size {\n    small,\n    large,\n}\nenum Tone { warm cool }\nView Main() { Text(\"{Size.large} {Tone.cool}\") }",
    )]);
    assert_eq!(render(&program), ["large cool"]);
}

#[test]
fn the_enums_name_alone_is_not_a_value() {
    let program = resolve(&[("main.byd", "enum E { a }\nView Main() { Text(\"{E}\") }")]);
    assert!(
        program.errors.iter().any(|e| matches!(
            e,
            CompileError::UnknownName { name, hint, .. } if name == "E" && hint.as_deref() == Some("E.a")
        )),
        "{:?}",
        program.errors
    );
}

#[test]
fn an_enum_value_in_a_view_branches_with_else_when() {
    let program = resolve(&[
        (
            "main.byd",
            "View Main() {\n var v = Variant.tonal\n when v == Variant.filled { Text(\"F\") } else when v == Variant.tonal { Text(\"T\") } else { Text(\"O\") } }",
        ),
        ("tokens.byd", VARIANT),
    ]);
    assert_eq!(render(&program), ["T"]);
}

// ── Packages ────────────────────────────────────────────────────────────────

fn kit() -> Packages {
    Packages(BTreeMap::from([(
        "kit",
        vec![
            (
                "kit/tokens.byd",
                "let gap = 8\nlet pad = gap * 2\nprivate let secret = 99\nenum Variant { filled, tonal, outlined }\nprivate enum Hidden { a }\nprivate type Inner = { n: Int }\nprivate fn tint(v: Variant) -> Str => v == Variant.filled ? \"F\" : \"T\"\nfn shout(s: Str) -> Str => s + \"!\"",
            ),
            (
                "kit/button.byd",
                "View Button(label: Str, variant: Variant = Variant.filled) { Text(\"{label}:{variant}:{tint(variant)}:{pad}:{secret}\") }\nprivate View Hint() { Text(\"hint\") }\nView Tip() { Hint() }",
            ),
        ],
    )]))
}

#[test]
fn a_consumer_reaches_a_packages_constants_and_enums_through_its_alias() {
    let program = resolve_with(
        &[(
            "main.byd",
            "use kit as m\nView Main() { Column { Text(\"{m.gap} {m.pad}\") m.Button(\"Save\") m.Button(\"Cancel\", variant: .tonal) m.Button(\"More\", variant: m.Variant.outlined) Text(m.shout(\"hey\")) } }",
        )],
        kit(),
    );
    assert_eq!(
        render(&program),
        [
            "8 16",
            "Save:filled:F:16:99",
            "Cancel:tonal:T:16:99",
            "More:outlined:T:16:99",
            "hey!"
        ]
    );
}

#[test]
fn a_consumer_types_a_parameter_with_a_package_enum() {
    let program = resolve_with(
        &[(
            "main.byd",
            "use kit as m\nView Main() { Mine(.tonal) }\nView Mine(v: m.Variant = m.Variant.filled) { m.Button(\"x\", variant: v) }",
        )],
        kit(),
    );
    assert_eq!(render(&program), ["x:tonal:T:16:99"]);
}

#[test]
fn a_package_view_used_by_the_consumer_may_use_private_views() {
    let program = resolve_with(
        &[("main.byd", "use kit as m\nView Main() { m.Tip() }")],
        kit(),
    );
    assert_eq!(render(&program), ["hint"]);
}

fn private_errors(main: &str) -> Vec<(String, String)> {
    resolve_with(&[("main.byd", main)], kit())
        .errors
        .iter()
        .filter_map(|e| match e {
            CompileError::PrivateName { name, package, .. } => {
                Some((name.clone(), package.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_consumer_cannot_use_a_private_view() {
    assert_eq!(
        private_errors("use kit as m\nView Main() { m.Hint() }"),
        [("Hint".to_string(), "package `kit`".to_string())]
    );
}

#[test]
fn a_consumer_cannot_import_a_private_view() {
    assert_eq!(
        private_errors("use kit.{Hint}\nView Main() { Hint() }"),
        [("Hint".to_string(), "package `kit`".to_string())]
    );
}

#[test]
fn a_consumer_cannot_call_a_private_function() {
    assert_eq!(
        private_errors("use kit as m\nView Main() { Text(m.tint(m.Variant.tonal)) }").len(),
        1
    );
}

#[test]
fn a_consumer_cannot_read_a_private_constant() {
    assert_eq!(
        private_errors("use kit as m\nView Main() { Text(\"{m.secret}\") }"),
        [("secret".to_string(), "package `kit`".to_string())]
    );
}

#[test]
fn a_consumer_cannot_name_a_private_enum_or_type() {
    assert_eq!(
        private_errors("use kit as m\nView Main() { Text(\"{m.Hidden.a}\") }"),
        [("Hidden".to_string(), "package `kit`".to_string())]
    );
    assert_eq!(
        private_errors(
            "use kit as m\nView Main() { Mine(1) }\nView Mine(n: m.Hidden) { Text(\"x\") }"
        ),
        [("Hidden".to_string(), "package `kit`".to_string())]
    );
    assert_eq!(
        private_errors("use kit as m\nView Main() { var x: Inner = { n: 1 }\n Text(\"x\") }"),
        [("Inner".to_string(), "package `kit`".to_string())]
    );
}

#[test]
fn the_message_says_the_name_is_private() {
    let program = resolve_with(
        &[("main.byd", "use kit as m\nView Main() { m.Hint() }")],
        kit(),
    );
    let line = program.source_map.render_line(&program.errors[0]);
    assert!(line.contains("error[PrivateName]"), "{line}");
    assert!(
        line.contains("`Hint` is private to package `kit`"),
        "{line}"
    );
}

#[test]
fn a_private_name_is_not_suggested_to_a_consumer() {
    let program = resolve_with(
        &[("main.byd", "use kit as m\nView Main() { m.Hin() }")],
        kit(),
    );
    assert!(
        program.errors.iter().any(|e| matches!(
            e,
            CompileError::UnknownImportSymbol { hint, .. } if hint.as_deref() != Some("Hint")
        )),
        "{:?}",
        program.errors
    );
}

#[test]
fn private_changes_nothing_in_an_app() {
    let program = resolve(&[(
        "main.byd",
        "private let gap = 8\nprivate enum E { a }\nprivate type T = { n: Int }\nprivate fn f() -> Int => gap\nprivate View Main() { Text(\"{f()} {E.a}\") }",
    )]);
    assert_eq!(render(&program), ["8 a"]);
}

#[test]
fn private_is_an_ordinary_name_elsewhere() {
    let program = resolve(&[(
        "main.byd",
        "View Main() { var private = 3\n var enum = 4\n Text(\"{private} {enum}\") }",
    )]);
    assert_eq!(render(&program), ["3 4"]);
}

#[test]
fn a_package_checked_on_its_own_sees_its_private_names() {
    let mut packages = Packages(BTreeMap::new());
    let program = byard_compiler::resolve::resolve_library(
        "kit",
        vec![
            SourceFile {
                name: "kit/a.byd".into(),
                source: "private let gap = 8\nprivate View Hint() { Text(\"{gap}\") }".into(),
            },
            SourceFile {
                name: "kit/b.byd".into(),
                source: "View Tip() { kit.Hint() }".into(),
            },
        ],
        &mut packages,
    );
    assert!(program.errors.is_empty(), "{:?}", program.errors);
}

#[test]
fn a_dependency_of_a_dependency_keeps_its_private_names_from_the_app_too() {
    let mut packages = Packages(BTreeMap::from([
        (
            "inner",
            vec![(
                "inner/x.byd",
                "private fn p() -> Int => 1\nfn q() -> Int => p()",
            )],
        ),
        (
            "outer",
            vec![(
                "outer/y.byd",
                "use inner\nView V() { Text(\"{inner.q()}\") }",
            )],
        ),
    ]));
    let program = resolve_program(
        vec![SourceFile {
            name: "main.byd".into(),
            source: "use outer\nuse inner\nView Main() { outer.V()\n Text(\"{inner.p()}\") }"
                .into(),
        }],
        &mut packages,
    );
    assert!(
        kinds(&program).contains(&"PrivateName"),
        "{:?}",
        program.errors
    );
}

// ── else when ───────────────────────────────────────────────────────────────

const STEPS: &str = "View Main() {\n var step = 0\n Button(\"next\") #[width: 80, height: 40] => step++\n when step == 0 { Text(\"zero\") } else when step == 1 { Text(\"one\") } else when step == 2 { Text(\"two\") } else { Text(\"many\") } }";

#[test]
fn else_when_shows_the_first_arm_that_holds() {
    let program = resolve(&[("main.byd", STEPS)]);
    let shown: Vec<Vec<String>> = (0..4).map(|n| render_after(&program, n)).collect();
    let arms: Vec<String> = shown
        .iter()
        .map(|texts| {
            texts
                .iter()
                .filter(|t| *t != "next")
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();
    assert_eq!(arms, ["zero", "one", "two", "many"]);
}

#[test]
fn else_when_is_the_nested_form_written_without_its_braces() {
    let sugar = parse(
        "View A() { var n = 0\n when n == 0 { Text(\"a\") } else when n == 1 { Text(\"b\") } else { Text(\"c\") } }",
    );
    let nested = parse(
        "View A() { var n = 0\n when n == 0 { Text(\"a\") } else { when n == 1 { Text(\"b\") } else { Text(\"c\") } } }",
    );
    assert!(sugar.errors.is_empty(), "{:?}", sugar.errors);
    assert!(byard_compiler::parser::span_eq::parsed_files_span_eq(
        &sugar, &nested
    ));
}

#[test]
fn an_else_when_chain_may_end_without_an_else() {
    let program = resolve(&[(
        "main.byd",
        "View Main() { var n = 5\n when n == 0 { Text(\"a\") } else when n == 5 { Text(\"five\") } }",
    )]);
    assert_eq!(render(&program), ["five"]);
}

#[test]
fn every_arm_of_an_else_when_chain_is_checked() {
    let program = resolve(&[(
        "main.byd",
        "View Main() { var n = 0\n when n == 0 { Text(\"a\") } else when n == 1 { Text(\"b\") } else when n == 2 { Bogus() } else { Text(\"c\") } }",
    )]);
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&program.views[0], &known);
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    assert!(
        interp.errors().is_empty(),
        "the third arm is not showing yet"
    );
    interp.lower_every_branch();
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownView { name, .. } if name == "Bogus")),
        "{:?}",
        interp.errors()
    );
}

// ── Reloading ───────────────────────────────────────────────────────────────

#[test]
fn reloading_a_changed_constant_updates_its_readers() {
    let main = (
        "main.byd",
        "View Main() { Text(\"{gap} {Variant.tonal}\") }",
    );
    let before = resolve(&[main, ("tokens.byd", "let gap = 8\nenum Variant { tonal }")]);
    let after = resolve(&[main, ("tokens.byd", "let gap = 12\nenum Variant { tonal }")]);
    assert!(before.errors.is_empty() && after.errors.is_empty());

    let mut interp = Interpreter::new();
    interp.load_views(&before.views);
    let known: Vec<&str> = before.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&before.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    assert_eq!(frame.texts()[0].text, "8 tonal");

    // What the dev runner does on a save.
    let tree =
        byard_compiler::interp::reload::apply_program(&mut interp, &before.views, &after.views)
            .expect("the root view was affected");
    interp.tick();
    frame.clear();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    assert_eq!(frame.texts()[0].text, "12 tonal");
}

// ── Parsing ─────────────────────────────────────────────────────────────────

#[test]
fn private_before_something_that_is_not_an_item_is_an_error_naming_what_is_expected() {
    let parsed = parse("private var x = 1\nView Main() { Text(\"x\") }");
    assert!(
        parsed.errors.iter().any(|e| matches!(
            e,
            CompileError::UnexpectedToken { expected, .. } if expected.contains("'enum'") && expected.contains("'let'")
        )),
        "{:?}",
        parsed.errors
    );
    // The file after it still parses.
    assert_eq!(parsed.views.len(), 1);
}

#[test]
fn an_enum_case_cannot_be_a_keyword() {
    let parsed = parse("enum E { a, when }\nView Main() { Text(\"x\") }");
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| matches!(e, CompileError::KeywordAsName { keyword, .. } if keyword == "when")),
        "{:?}",
        parsed.errors
    );
}

#[test]
fn a_file_level_let_is_a_constant_and_the_files_views_are_unchanged() {
    let parsed = parse("let a = 1\nprivate let b: Int = 2\nView V() { let c = 3\n Text(\"{c}\") }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    assert_eq!(parsed.consts.len(), 2);
    assert!(!parsed.consts[0].private && parsed.consts[1].private);
    assert!(parsed.consts[1].ty.is_some());
    assert_eq!(parsed.views.len(), 1);
}
