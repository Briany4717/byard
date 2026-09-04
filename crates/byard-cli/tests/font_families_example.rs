//! Guards the committed RFC-0034 example through the real `byard` binary.
//!
//! The `check` is not a formality here: it is the only gate that reads the
//! example's `byard.toml`, so it is what proves the declared families are
//! files that exist and parse. A font path that rotted would otherwise surface
//! as text quietly set in the system font, which is exactly the failure the
//! whole feature was built to stop.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/font_families")
}

fn check(dir: &PathBuf) -> (bool, String) {
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

/// `byard check <project-dir>` on the font-families example reports no errors.
#[test]
fn font_families_example_checks_clean() {
    let (ok, text) = check(&example_dir());
    assert!(ok, "check failed:\n{text}");
    assert!(
        text.contains("0 errors"),
        "expected a clean check, got:\n{text}"
    );
}

/// A misspelt family in that same project is a compile error naming the
/// nearest declared one.
///
/// Written against a copy of the shipped example rather than a fixture, so the
/// claim is about the project a reader can actually run, and so the hint is
/// checked against the families that project really declares.
#[test]
fn a_misspelt_family_in_the_example_is_a_diagnostic() {
    let tmp = std::env::temp_dir().join("byard_font_families_typo");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).expect("scratch project");
    // The manifest's font paths are relative to the project root, so they are
    // rewritten to absolute ones for the copy.
    let manifest = std::fs::read_to_string(example_dir().join("byard.toml")).unwrap();
    let fonts = example_dir().join("../assets/fonts");
    let fonts = fonts.canonicalize().unwrap();
    let manifest = manifest.replace("../assets/fonts", fonts.to_str().unwrap());
    std::fs::write(tmp.join("byard.toml"), manifest).unwrap();
    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    std::fs::write(
        tmp.join("src/main.byd"),
        // Targeted at an attribute rather than the first textual match: the
        // example's own comments talk about `font: display`, and a typo
        // planted in a comment proves nothing.
        src.replacen("#[font: display, size: 34", "#[font: dispaly, size: 34", 1),
    )
    .unwrap();

    let (ok, text) = check(&tmp);
    assert!(!ok, "a misspelt family must not check clean:\n{text}");
    assert!(text.contains("UnknownFontFamily"), "{text}");
    assert!(text.contains("dispaly"), "{text}");
    assert!(
        text.contains("did you mean `display`"),
        "the nearest declared family must be offered:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
