//! Guards the committed image-seed example (RFC-0022 §5): it checks clean, and
//! the theme its `byard.toml` describes is the one the kite's colour derives,
//! though almost all of the picture is grey.

use byard_compiler::interp::theme::Theme;
use byard_project::manifest::Manifest;
use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/seed_image")
}

#[test]
fn seed_image_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
}

/// The kite in `brand.png` is filled with exactly this colour.
const KITE: i64 = 0xE8_543F;

#[test]
fn the_kite_is_the_seed_not_the_sky() {
    let manifest = Manifest::discover(Some(&example_dir())).expect("the manifest loads");
    let mut from_kite = Theme::byard_base();
    from_kite.apply_seed(KITE);
    for dark in [false, true] {
        for role in [
            "primary",
            "onPrimary",
            "primaryContainer",
            "tertiary",
            "surface",
        ] {
            assert_eq!(
                manifest.theme.color(role, dark),
                from_kite.color(role, dark),
                "{role}, dark: {dark}"
            );
        }
    }
}
