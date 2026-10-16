//! `[native_views]`: an element the app draws in Rust, declared in the
//! manifest so the command line knows it.
//!
//! `byard check` cannot link an app's Rust, so a project that used one of its
//! own elements failed there with "unknown view" and checked nothing past it.
//! A declared element is checked like any other: a property it does not take,
//! a value of the wrong type and a misspelt name are errors.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A project with one declared element, `Gauge`, and `body` as its view.
fn project(name: &str, manifest_tail: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-native-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("byard.toml"),
        format!("[project]\nname = \"{name}\"\nentry = \"src/main.byd\"\n\n{manifest_tail}"),
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.byd"),
        format!("View Main() {{\n    var level: Float = 0.5\n    {body}\n}}\n"),
    )
    .unwrap();
    dir
}

const GAUGE: &str = "[native_views.Gauge]\nvalue = \"float\"\ntint = \"color\"\n";

fn check(dir: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(dir)
        .output()
        .expect("run `byard check`");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn a_declared_element_checks_clean() {
    let dir = project(
        "clean",
        GAUGE,
        "Gauge #[value: level, tint: 0xFF8800, width: 120, height: 40]",
    );
    let (ok, text) = check(&dir);
    assert!(ok && text.contains("0 errors"), "{text}");
}

#[test]
fn an_element_nobody_declared_is_still_unknown() {
    let dir = project(
        "undeclared",
        "",
        "Gauge #[value: level, width: 120, height: 40]",
    );
    let (ok, text) = check(&dir);
    assert!(!ok && text.contains("Gauge"), "{text}");
}

#[test]
fn a_property_it_does_not_take_is_an_error() {
    let dir = project(
        "wrong-name",
        GAUGE,
        "Gauge #[valeu: level, width: 120, height: 40]",
    );
    let (ok, text) = check(&dir);
    assert!(!ok && text.contains("valeu"), "{text}");
}

/// The native view example declares its three elements, so the command line
/// can check a project whose drawing is in Rust.
#[test]
fn the_native_view_example_checks_clean() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/sparkline_view");
    let (ok, text) = check(&dir);
    assert!(ok && text.contains("0 errors"), "{text}");
}

#[test]
fn a_property_of_the_wrong_type_is_an_error() {
    let dir = project(
        "wrong-type",
        GAUGE,
        "Gauge #[value: \"half\", width: 120, height: 40]",
    );
    let (ok, text) = check(&dir);
    assert!(!ok && text.contains("AttributeTypeMismatch"), "{text}");
}

#[test]
fn a_misspelt_name_as_a_number_property_is_an_error() {
    let dir = project(
        "misspelt-value",
        GAUGE,
        "Gauge #[value: levl, width: 120, height: 40]",
    );
    let (ok, text) = check(&dir);
    assert!(
        !ok && text.contains("levl") && text.contains("level"),
        "{text}"
    );
}
