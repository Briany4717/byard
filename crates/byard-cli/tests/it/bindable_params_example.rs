//! Guards the committed RFC-0051 example through the real `byard` binary: the
//! `bindable_params` project checks clean, a tap on a switch inside `Switch`
//! reaches the `var` in `Main` in the same frame, and `Main` writing the `var`
//! moves the switches.
//!
//! The text is read from the accessibility dump `byard shot --a11y` writes,
//! which needs no GPU.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/bindable_params")
}

/// The labels of a shot of the example, run with `extra` flags.
fn labels(tag: &str, extra: &[&str]) -> Vec<String> {
    let out = std::env::temp_dir().join(format!("byard-bind-{tag}-{}.txt", std::process::id()));
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
        .args(["--size", "380x420"])
        .arg("--a11y")
        .arg(&out)
        .args(extra)
        .output()
        .expect("run `byard shot --a11y`");
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let dump = std::fs::read_to_string(&out).expect("the tree was written");
    let _ = std::fs::remove_file(&out);
    dump.lines()
        .filter_map(|l| l.strip_prefix("Label \""))
        .filter_map(|l| l.split("\" @").next())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_bindable_params_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "got:\n{stdout}");
}

#[test]
fn a_tap_in_the_switch_reaches_the_callers_var_in_the_same_frame() {
    let before = labels("before", &[]);
    assert!(
        before.contains(&"wifi=false, bluetooth=true".to_string()),
        "{before:?}"
    );
    // The `Wi-Fi` label is a `Button` in `Switch`, which writes its binding.
    let after = labels("tap", &["--tap", "105,41"]);
    assert!(
        after.contains(&"wifi=true, bluetooth=true".to_string()),
        "{after:?}"
    );
}

#[test]
fn the_caller_writing_its_vars_moves_both_switches_and_the_field_writes_two() {
    let off = labels("off", &["--tap", "105,41", "--tap", "190,226"]);
    assert!(
        off.contains(&"wifi=false, bluetooth=false".to_string()),
        "{off:?}"
    );
    // Typing in `FormField` writes `text`, and its `input` event writes `valid`.
    let typed = labels(
        "typed",
        &[
            "--tap",
            "100,158",
            "--key",
            "End",
            "--key",
            "Backspace",
            "--key",
            "Backspace",
        ],
    );
    assert!(
        typed.contains(&"name=A, valid=false".to_string()),
        "{typed:?}"
    );
    assert!(
        typed.contains(&"At least three letters".to_string()),
        "{typed:?}"
    );
}
