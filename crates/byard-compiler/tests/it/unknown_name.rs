//! A name nothing declares is an error at check time.
//!
//! At runtime an undeclared name is its own text, which is how `align: center`
//! works, and why `Text("{nmae}")` drew "nmae" and `count = cuont + 1` wrote a
//! string into a number, both without a word from `byard check`. The check
//! reports a bare name wherever it has to be a value, and leaves it alone
//! wherever it may be a word the element understands.

use byard_compiler::CompileError;
use byard_compiler::resolve::{PackageProvider, SourceFile, resolve_program};

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

/// The `(name, hint)` of every `UnknownName` in a program, and whether it
/// reported anything else.
fn check(source: &str) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let errors = resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: source.to_string(),
        }],
        &mut NoPackages,
    )
    .errors;
    let mut names = Vec::new();
    let mut others = Vec::new();
    for e in errors {
        match e {
            CompileError::UnknownName { name, hint, .. } => names.push((name, hint)),
            other => others.push(format!("{other:?}")),
        }
    }
    (names, others)
}

/// The unknown names in the body of one view that declares `count`, `name`
/// and `items`.
fn unknown(body: &str) -> Vec<String> {
    let (names, others) = check(&format!(
        "View Main() {{
    var count: Int = 0
    var name: Str = \"a\"
    var items: List<Int> = [1, 2]
{body}
}}"
    ));
    assert!(others.is_empty(), "{others:?}");
    names.into_iter().map(|(n, _)| n).collect()
}

#[test]
fn a_misspelt_name_in_a_text_is_reported_with_a_suggestion() {
    let (names, _) = check(
        "View Main() {
    var count: Int = 0
    Text(\"{cuont}\")
}",
    );
    assert_eq!(names, [("cuont".to_string(), Some("count".to_string()))]);
}

#[test]
fn it_is_reported_wherever_a_value_is_read() {
    for body in [
        "Text(nmae)",
        "Text(\"{nmae}\")",
        "Text(\"{nmae.len}\")",
        "Text(\"{count + cuont}\")",
        "Text(\"{items[idx]}\")",
        "Text(\"{nmae == \\\"a\\\" ? 1 : 2}\")",
        "when cuont > 1 { Text(\"x\") }",
        "for item in itmes { Text(\"{item}\") }",
        "let double = cuont * 2\nText(\"{double}\")",
        "Box #[width: cuont * 2] {}",
        "Box #[opacity: flag ? 1.0 : 0.5] {}",
        "Button(\"x\") => count = cuont + 1",
        "Button(\"x\") => cuont++",
        "Button(\"x\") => { items = items.push(cuont) }",
        "Button(\"x\") #[tap => name = nmae]",
        "on mount => count = cuont",
        "Text(\"{items.map(x => x + cuont)}\")",
    ] {
        assert_eq!(unknown(body).len(), 1, "{body}");
    }
}

/// A bare word as a property's value, a named argument or part of a
/// property's tuple is the element's to judge, not a name.
#[test]
fn a_word_an_element_understands_is_not_a_name() {
    for body in [
        "Column #[align: center, justify: between] {}",
        "Text(name) #[weight: bold, wrap: none]",
        "Box #[gradient: (kind: radial, from: 0xFF0000, to: 0x0000FF)] {}",
        "Box #[width: count > 1 ? 100 : 50, align: count > 1 ? center : start] {}",
        "Box #[opacity: 1.0 with anim.linear(300ms, repeat: infinite)] {}",
        "Box #[scale: 1.0 with anim.spring(bouncy)] {}",
        "Canvas #[width: 20, height: 20] { circle(cx: 10, cy: 10, r: 5, fill: 0xFF0000) }",
    ] {
        let names = unknown(body);
        assert!(names.is_empty(), "{body}: {names:?}");
    }
}

#[test]
fn everything_a_view_can_declare_is_known() {
    let (names, others) = check(
        "fn twice(n: Int) -> Int => n * 2

View Main() {
    inject Theme as t
    var count: Int = 0
    let double = count * 2
    fn label(n: Int) -> Str => n == 1 ? \"one\" : \"many\"
    let reset = () => { count = 0 }
    let chip = style { radius: 8 }

    Column #[bg: t.background, ..chip] {
        Text(\"{double} {label(count)} {twice(count)}\")
        for i, item in [1, 2] { Text(\"{i}: {item}\") }
        Button(\"x\") #[tap(e) => count = count + 1]
        Button(\"y\") => reset()
        Box { on measure => count = it.w }
        Card(title: \"{count}\", shown: count > 0) { Text(\"inside\") }
    }
}

View Card(title: Str, shown: Bool, content) {
    when shown { Text(title) }
    content
}",
    );
    assert!(names.is_empty(), "{names:?}");
    assert!(others.is_empty(), "{others:?}");
}

/// A `let` may read a `var` written below it.
#[test]
fn the_order_of_declarations_does_not_matter() {
    let names = unknown("let later = below + 1\nvar below: Int = 2\nText(\"{later}\")");
    assert!(names.is_empty(), "{names:?}");
}

/// What a view is called with is a value, named or not.
#[test]
fn a_view_argument_is_checked() {
    let (names, _) = check(
        "View Main() {
    var place: Str = \"Madrid\"
    Screen(title: plcae)
}

View Screen(title: Str) {
    Text(title)
}",
    );
    assert_eq!(names, [("plcae".to_string(), Some("place".to_string()))]);
}

/// The names bound for one block end with it.
#[test]
fn a_name_does_not_outlive_its_block() {
    assert_eq!(
        unknown("for item in items { Text(\"{item}\") }\nText(\"{item}\")"),
        ["item"]
    );
    assert_eq!(
        unknown("Button(\"x\") #[tap(e) => count = 1]\nText(\"{e}\")"),
        ["e"]
    );
}

#[test]
fn the_message_names_it_and_suggests() {
    let errors = resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: "View Main() {\n    var count: Int = 0\n    Text(\"{cuont}\")\n}".to_string(),
        }],
        &mut NoPackages,
    )
    .errors;
    let message =
        errors[0].render("View Main() {\n    var count: Int = 0\n    Text(\"{cuont}\")\n}");
    assert!(
        message.contains("cuont") && message.contains("count"),
        "{message}"
    );
}

/// A property that takes only values (a colour, a number, a length)
/// reads a bare name as a value, so a misspelt one is told. A property that
/// takes words keeps them.
#[test]
fn a_property_that_takes_values_reads_a_bare_name_as_one() {
    for body in [
        "Box #[bg: prmary] {}",
        "Box #[width: cuont, height: 10] {}",
        "Box #[opacity: nmae] {}",
        "Box #[radius: radus] {}",
    ] {
        assert_eq!(unknown(body).len(), 1, "{body}");
    }
    for body in [
        "Box #[origin: center, align: center] {}",
        "Text(name) #[weight: bold, align: center]",
        "Box #[bg: 0x112233, width: count] {}",
    ] {
        let names = unknown(body);
        assert!(names.is_empty(), "{body}: {names:?}");
    }
}

/// A called name has to be a function, where a value is read.
#[test]
fn a_misspelt_function_is_reported() {
    assert_eq!(
        unknown("let double = (n) => n * 2\nText(\"{dobule(count)}\")"),
        ["dobule"]
    );
    assert_eq!(
        unknown("let reset = () => { count = 0 }\nButton(\"x\") => rest()"),
        ["rest"]
    );
    // The language's own: navigation.
    let names = unknown("Button(\"x\") => back(\"/\")\nButton(\"y\") => navigate(\"/a\")");
    assert!(names.is_empty(), "{names:?}");
}

/// A registered native view's properties are checked the same way: a number
/// property reads a bare name as a value.
#[test]
fn a_native_views_value_property_reads_a_bare_name_as_one() {
    use byard_core::render::registry::{DeclaredProp, NativePropType, declare};
    declare(
        "NameCheckGauge",
        &[
            DeclaredProp {
                name: "level".into(),
                ty: NativePropType::Float,
                layout: false,
            },
            DeclaredProp {
                name: "caption".into(),
                ty: NativePropType::Str,
                layout: false,
            },
        ],
    );
    assert_eq!(
        unknown("NameCheckGauge #[level: cuont, width: 10, height: 10]"),
        ["cuont"]
    );
    let names = unknown("NameCheckGauge #[level: count, caption: name, width: 10, height: 10]");
    assert!(names.is_empty(), "{names:?}");
}

/// A canvas shape's numeric argument reads a bare name as a name; a word
/// argument (`cap: round`) keeps its word.
#[test]
fn a_shapes_numeric_argument_reads_a_bare_name_as_one() {
    assert_eq!(
        unknown(
            "Canvas #[width: 40, height: 40] {\n    circle(cx: 20, cy: 20, r: raduis, fill: 0xFF0000)\n}"
        ),
        ["raduis"]
    );
    assert_eq!(
        unknown(
            "Canvas #[width: 40, height: 40] {\n    path(fill: 0xFF0000) {\n        move(x: 0, y: 0)\n        line(x: wdth, y: 40)\n        close()\n    }\n}"
        ),
        ["wdth"]
    );
    let names = unknown(
        "Canvas #[width: 40, height: 40] {\n    arc(cx: 20, cy: 20, r: count, start: 0deg, sweep: 90deg, stroke: 0xFF0000, cap: round)\n}",
    );
    assert!(names.is_empty(), "{names:?}");
}
