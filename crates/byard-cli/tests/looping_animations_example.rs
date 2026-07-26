//! Guards the committed RFC-0025 example through the real `byard` binary: the
//! `looping_animations` project — `repeat`/`reverse`/`delay`/`from` modifiers,
//! two `anim.keyframes(…)` sequences per element, a looping colour, and
//! `anim.stagger(…)` over a `for` loop's index — must check clean (parse,
//! type-check, lower, validate with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/looping_animations")
}

/// `byard check <project-dir>` on the looping-animations example reports no errors.
#[test]
fn looping_animations_example_checks_clean() {
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
