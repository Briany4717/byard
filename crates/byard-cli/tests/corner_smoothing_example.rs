//! Guards the committed RFC-0031 §S1–S3 example through the real `byard`
//! binary: the `corner_smoothing` project, the `smooth` ramp, a shadow and a
//! border sharing their caster's profile, an animated `smooth` under
//! `anim.spring()`, and the `Canvas` `rect` kind's own `smooth:`, must check
//! clean (parse, type-check, lower, validate with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/corner_smoothing")
}

/// `byard check <project-dir>` on the corner-smoothing example reports no errors.
#[test]
fn corner_smoothing_example_checks_clean() {
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
