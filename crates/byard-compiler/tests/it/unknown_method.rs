//! A method the receiver does not have is an error at check time.
//!
//! The interpreter runs an unknown method as a call that does nothing, so
//! `xs.pusj(1)` used to draw an app that quietly never grew. The diagnostic
//! existed in the type checker but nothing ran that checker outside the
//! language server, and inside it the walk never reached a view body's
//! elements, actions or interpolations. These tests go through
//! `resolve_program`, the entry every command uses, so they cover the wiring
//! as well as the check.

use byard_compiler::CompileError;
use byard_compiler::resolve::{PackageProvider, ResolvedProgram, SourceFile, resolve_program};

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
    let source = format!("View Main() {{\n{body}\n}}");
    resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source,
        }],
        &mut NoPackages,
    )
}

/// The `(name, hint)` of every `UnknownMethod` in `body`.
fn unknown_methods(body: &str) -> Vec<(String, Option<String>)> {
    resolve(body)
        .errors
        .iter()
        .filter_map(|e| match e {
            CompileError::UnknownMethod { name, hint, .. } => Some((name.clone(), hint.clone())),
            _ => None,
        })
        .collect()
}

fn assert_clean(body: &str) {
    let program = resolve(body);
    assert!(program.errors.is_empty(), "{:?}", program.errors);
}

fn one(name: &str, hint: Option<&str>) -> Vec<(String, Option<String>)> {
    vec![(name.to_string(), hint.map(str::to_string))]
}

#[test]
fn a_misspelt_method_on_a_list_var_is_reported() {
    let found = unknown_methods("var xs = [1, 2, 3]\nlet ys = xs.pusj(1)");
    assert_eq!(found, one("pusj", Some("push")));
}

#[test]
fn a_misspelt_method_on_a_list_let_is_reported() {
    let found = unknown_methods("let xs = [1, 2, 3]\nlet ys = xs.contain(1)");
    assert_eq!(found, one("contain", Some("contains")));
}

#[test]
fn a_misspelt_method_on_an_annotated_list_is_reported() {
    let found = unknown_methods("var xs: List<Str> = []\nlet ys = xs.removeat(0)");
    assert_eq!(found, one("removeat", Some("removeAt")));
}

#[test]
fn a_misspelt_method_inside_an_interpolation_is_reported() {
    let found = unknown_methods("var xs = [1, 2, 3]\nText(\"{xs.pusj(1).len}\")");
    assert_eq!(found, one("pusj", Some("push")));
}

#[test]
fn a_misspelt_method_inside_a_button_action_is_reported() {
    let found = unknown_methods("var xs = [1, 2, 3]\nButton(\"Add\") => { xs = xs.pusj(4) }");
    assert_eq!(found, one("pusj", Some("push")));
}

#[test]
fn a_misspelt_method_inside_an_event_attribute_is_reported() {
    let found =
        unknown_methods("var xs = [1, 2, 3]\nColumn #[pointer_down => { xs = xs.pusj(4) }] {}");
    assert_eq!(found, one("pusj", Some("push")));
}

#[test]
fn a_misspelt_method_inside_a_lifecycle_and_timer_action_is_reported() {
    let found = unknown_methods(
        "var xs = [1, 2, 3]\non mount => { xs = xs.pusj(1) }\nevery 5s => { xs = xs.pusj(2) }",
    );
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn a_misspelt_method_in_a_nested_element_and_a_for_body_is_reported() {
    let found = unknown_methods(
        "var xs = [1, 2, 3]\nColumn {\n for x in xs {\n  Text(\"{xs.slic(1)}\")\n }\n}",
    );
    assert_eq!(found, one("slic", Some("slice")));
}

#[test]
fn a_misspelt_method_in_a_fn_body_and_a_when_condition_is_reported() {
    let found = unknown_methods(
        "var xs = [1]\nfn more(ys: List<Int>) -> Bool => ys.contain(1)\nwhen xs.contain(2) { Text(\"y\") }",
    );
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn the_hint_names_the_closest_method() {
    let found = unknown_methods("let xs = [1, 2, 3]\nlet ys = xs.slic(1)");
    assert_eq!(found, one("slic", Some("slice")));
}

#[test]
fn a_method_nothing_resembles_has_no_hint() {
    let found = unknown_methods("let xs = [1, 2, 3]\nlet ys = xs.zzzzzzzzzz()");
    assert_eq!(found, one("zzzzzzzzzz", None));
}

#[test]
fn the_diagnostic_points_at_the_method_name() {
    let program = resolve("var xs = [1, 2, 3]\nlet ys = xs.pusj(1)");
    let line = program.source_map.render_line(&program.errors[0]);
    // `View Main() {` is line 1; `let ys = xs.pusj(1)` is line 3, and the name
    // starts at column 13 (the receiver `xs` is not underlined).
    assert!(
        line.starts_with("main.byd:3:13: error[UnknownMethod]:"),
        "{line}"
    );
    assert!(line.contains("(did you mean `push`?)"), "{line}");
}

#[test]
fn every_real_list_method_checks_clean() {
    assert_clean(
        "var xs = [1, 2, 3]\n\
         let a = xs.push(4)\n\
         let b = xs.removeAt(0)\n\
         let c = xs.contains(2)\n\
         let d = xs.map(|x| x + 1)\n\
         let e = xs.filter(|x| x > 1)\n\
         let f = xs.slice(1)\n\
         let g = xs.slice(1, 2)\n\
         Text(\"{a.len} {b.len} {c} {d.len} {e.len} {f.len} {g.len}\")",
    );
}

#[test]
fn a_method_on_the_result_of_map_is_checked_and_a_real_one_is_clean() {
    assert_clean("let xs = [1, 2]\nlet ys = xs.map(|x| x + 1).push(3)");
    let found = unknown_methods("let xs = [1, 2]\nlet ys = xs.map(|x| x + 1).pusj(3)");
    assert_eq!(found, one("pusj", Some("push")));
}

#[test]
fn str_slice_is_a_real_method_and_len_is_a_field() {
    assert_clean("var s: Str = \"hello\"\nText(\"{s.slice(1, 3)} {s.len}\")");
}

#[test]
fn a_method_a_str_does_not_have_is_reported() {
    let found = unknown_methods("var s: Str = \"hello\"\nText(\"{s.slicee(1)}\")");
    assert_eq!(found, one("slicee", Some("slice")));
    // The runtime runs none of these on text: they are silent no-ops.
    let found = unknown_methods("let s = \"hello\"\nlet a = s.contains(\"h\")");
    assert_eq!(found, one("contains", None));
}

#[test]
fn a_call_on_a_controller_handle_is_not_checked() {
    assert_clean(
        "inject Http as http\n\
         inject Store as store\n\
         on mount => {\n\
             http.get(\"https://example.com\")\n\
                 ok r => { store.set(\"k\", r) }\n\
                 err e => { store.remove(\"k\") }\n\
         }\n\
         Button(\"go\") => { http.anything_at_all(1) }",
    );
}

#[test]
fn a_call_on_a_record_or_an_untyped_value_is_not_checked() {
    assert_clean(
        "let r = { a: 1, b: [1, 2] }\n\
         let x = r.b.whatever(1)\n\
         let y = r.helper(2)",
    );
}

#[test]
fn a_call_on_an_anim_curve_is_not_checked() {
    assert_clean(
        "var on = false\n\
         Column #[opacity: (on ? 1.0 : 0.0) with anim.spring(stiffness: 180)] {}",
    );
}

#[test]
fn a_view_parameter_with_an_unmodelled_type_is_not_checked() {
    let source = "View Card(item: Item) {\n Text(\"{item.title.upper()}\")\n}\nView Main() {}";
    let program = resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: source.to_string(),
        }],
        &mut NoPackages,
    );
    assert!(program.errors.is_empty(), "{:?}", program.errors);
}

#[test]
fn a_name_that_shadows_a_list_is_not_taken_for_the_list() {
    // The for variable, a lambda parameter, an event payload and an async arm
    // binding all reuse the name of the outer list. None is that list.
    assert_clean(
        "inject Http as http\n\
         var xs = [1, 2, 3]\n\
         var rows = [[1], [2]]\n\
         Column {\n\
             for xs in rows {\n\
                 Text(\"{xs.shout()}\")\n\
             }\n\
         }\n\
         let ys = rows.map(|xs| xs.shout())\n\
         Column #[pointer_move(xs) => { xs.shout() }] {}\n\
         on mount => {\n\
             http.get(\"u\") ok xs => { xs.shout() }\n\
         }",
    );
}

#[test]
fn a_binding_local_to_a_block_does_not_outlive_it() {
    // `xs` is a list only inside the `when` branch; outside it is unknown.
    assert_clean(
        "when true {\n\
             let xs = [1]\n\
             Text(\"{xs.len}\")\n\
         }\n\
         Text(\"{xs.shout()}\")",
    );
}

#[test]
fn a_fn_parameter_does_not_leak_into_the_view() {
    assert_clean(
        "fn head(xs: List<Int>) -> Int => xs.len\n\
         Text(\"{xs.shout()}\")",
    );
}

#[test]
fn the_content_slot_needs_no_annotation_but_another_bare_parameter_does() {
    let check = |source: &str| {
        resolve_program(
            vec![SourceFile {
                name: "main.byd".to_string(),
                source: source.to_string(),
            }],
            &mut NoPackages,
        )
        .errors
    };
    let errors = check("View Frame(title: Str, content) { Column { content } }\nView Main() {}");
    assert!(errors.is_empty(), "{errors:?}");
    let errors = check("View Frame(title) { Text(title) }\nView Main() {}");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, CompileError::MissingAnnotation { .. })),
        "{errors:?}"
    );
}

#[test]
fn a_misspelt_method_in_an_if_body_and_an_else_branch_is_reported() {
    let found = unknown_methods(
        "var xs = [1, 2, 3]\nButton(\"Add\") => { if xs.len > 2 { xs = xs.pusj(4) } else { xs = xs.removeat(0) } }",
    );
    assert_eq!(
        found,
        [
            ("pusj".to_string(), Some("push".to_string())),
            ("removeat".to_string(), Some("removeAt".to_string())),
        ]
    );
}

#[test]
fn an_else_if_chain_is_checked_and_real_methods_are_clean() {
    let found = unknown_methods(
        "var xs = [1]\nButton(\"Go\") => { if xs.len > 2 { xs = xs.push(1) } else if xs.len > 1 { xs = xs.contain(1) } }",
    );
    assert_eq!(found, one("contain", Some("contains")));
    assert_clean(
        "var xs = [1]\nButton(\"Go\") => { if xs.len > 2 { xs = xs.push(1) } else { xs = xs.removeAt(0) } }",
    );
}
