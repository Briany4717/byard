//! `type Place = { … }`: a record type the checker knows the fields of.
//!
//! Data that arrives as records (JSON) had no declaration, so `hit.nmae` on a
//! search result checked clean and drew nothing. A declared type gives a list,
//! a loop variable or a parameter its fields, and a misspelt one is told.

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

const PLACE: &str = "type Place = { name: Str, country: Str, latitude: Float, longitude: Float }\n";

fn errors_of(files: &[(&str, &str)]) -> Vec<CompileError> {
    resolve_program(
        files
            .iter()
            .map(|(name, source)| SourceFile {
                name: (*name).to_string(),
                source: (*source).to_string(),
            })
            .collect(),
        &mut NoPackages,
    )
    .errors
}

/// The errors of one file: the type, then a view with `body`.
fn errors(body: &str) -> Vec<CompileError> {
    errors_of(&[(
        "main.byd",
        &format!("{PLACE}\nView Main() {{\n    var found: List<Place> = []\n{body}\n}}"),
    )])
}

fn unknown_fields(errs: &[CompileError]) -> Vec<(String, Option<String>)> {
    errs.iter()
        .filter_map(|e| match e {
            CompileError::UnknownField { name, hint, .. } => Some((name.clone(), hint.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_fields_a_type_declares_check_clean() {
    let errs = errors(
        "for hit in found {\n    Text(\"{hit.name}, {hit.country} at {hit.latitude}\")\n}\n\
         Text(\"{found[0].name}\")",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_misspelt_field_in_a_loop_is_told() {
    let errs = errors("for hit in found {\n    Text(hit.nmae)\n}");
    assert_eq!(
        unknown_fields(&errs),
        [("nmae".to_string(), Some("name".to_string()))]
    );
}

#[test]
fn a_misspelt_field_through_an_index_or_a_parameter_is_told() {
    let errs = errors("Text(\"{found[0].contry}\")");
    assert_eq!(unknown_fields(&errs)[0].0, "contry");
    let errs = errors("fn label(p: Place) -> Str => p.lattitude\nText(\"x\")");
    assert_eq!(unknown_fields(&errs)[0].0, "lattitude");
}

/// A field's own type is checked further down.
#[test]
fn a_fields_type_carries_on() {
    let errs = errors("for hit in found {\n    Text(\"{hit.name.uper()}\")\n}");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::UnknownMethod { name, .. } if name == "uper")),
        "{errs:?}"
    );
}

/// A type declared in one file is known in another.
#[test]
fn a_type_is_the_programs() {
    let errs = errors_of(&[
        ("types.byd", PLACE),
        (
            "main.byd",
            "View Main() {\n    var found: List<Place> = []\n    for hit in found { Text(hit.nmae) }\n}",
        ),
    ]);
    assert_eq!(unknown_fields(&errs)[0].0, "nmae");
}

#[test]
fn a_type_declared_twice_is_an_error() {
    let errs = errors_of(&[
        ("a.byd", PLACE),
        ("main.byd", &format!("{PLACE}View Main() {{ Text(\"x\") }}")),
    ]);
    assert!(
        errs.iter().any(
            |e| matches!(e, CompileError::DuplicateRecordType { name, .. } if name == "Place")
        ),
        "{errs:?}"
    );
}

/// `type` stays an ordinary name everywhere else.
#[test]
fn type_is_still_a_name() {
    let errs = errors_of(&[(
        "main.byd",
        "View Main() {\n    var type: Str = \"a\"\n    Text(type)\n}",
    )]);
    assert!(errs.is_empty(), "{errs:?}");
}
