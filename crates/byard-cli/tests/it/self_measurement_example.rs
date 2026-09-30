//! Guards the committed RFC-0038 example through the real `byard` binary: the
//! `self_measurement` project, an `on measure` card whose `Canvas`, rule and
//! bars are all placed from the measured width, must check clean (parse,
//! type-check, lower, validate with no diagnostics).
//!
//! It is the one guard that would catch the example drifting away from the
//! language: `on measure` is a statement form, so a regression in the parser's
//! contextual `on` handling shows up here as a parse error rather than as a
//! demo nobody ran.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/self_measurement")
}

/// `byard check <project-dir>` on the self-measurement example reports no errors.
#[test]
fn self_measurement_example_checks_clean() {
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
