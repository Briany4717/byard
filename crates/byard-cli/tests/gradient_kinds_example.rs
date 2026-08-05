//! Guards the committed RFC-0035 example through the real `byard` binary: the
//! `gradient_kinds` project, with a linear ramp, a radial glow, a conic sweep,
//! an aspect-corrected glow on a wide card, a dial, corner smoothing on a
//! gradient box, and a reactive centre, must check clean (parse, type-check,
//! lower, validate with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/gradient_kinds")
}

/// `byard check <project-dir>` on the gradient-kinds example reports no errors.
#[test]
fn gradient_kinds_example_checks_clean() {
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
