//! Guards the committed RFC-0026 example through the real `byard` binary: the
//! `navigation` project, a `NavHost` of three tabs, a `NavStack` per tab with
//! `:id` route params, a catch-all, `slide`/`slide_up` transitions,
//! `swipe_back`, `deep_link`, and the `back`/`replace` actions, must check
//! clean (parse, type-check, lower, validate the intrinsic and route contracts
//! with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/navigation")
}

/// `byard check <project-dir>` on the navigation example reports no errors.
#[test]
fn navigation_example_checks_clean() {
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
