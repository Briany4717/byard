//! Guards the committed RFC-0031 §S9–§S10 example through the real `byard`
//! binary: the `shape_morph` project, the `ngon` shape kind across its whole
//! parameter set, the Material 3 Expressive loading indicator as seven members
//! and one `anim.linear(…, repeat: infinite)` scalar, a morph between two
//! *different* shape kinds, and an interaction-state spring, must check clean
//! (parse, type-check, lower, validate with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/shape_morph")
}

/// `byard check <project-dir>` on the shape-morph example reports no errors.
#[test]
fn shape_morph_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("0 errors"),
        "expected a clean check, got:\n{stdout}"
    );
}
