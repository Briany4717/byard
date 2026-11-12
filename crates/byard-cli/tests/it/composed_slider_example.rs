//! Guards the committed composed slider example (RFC-0063) through the real
//! `byard` binary: it checks clean, and its keys and its drag change the value
//! a screen reader is told.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/composed_slider")
}

fn dump(tag: &str, args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("byard-slider-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("tree.txt");
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
        .args(["--size", "360x300"])
        .args(args)
        .arg("--a11y")
        .arg(&out)
        .output()
        .expect("run `byard shot`");
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read_to_string(&out).expect("the tree was written")
}

#[test]
fn composed_slider_example_checks_clean() {
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
fn the_row_is_a_slider_with_a_value_and_a_description() {
    let tree = dump("rest", &[]);
    assert!(
        tree.contains(
            r#"Slider "Volume" description "Drag, or use the arrow keys, Home and End" = "40 percent" [focusable]"#
        ),
        "{tree}"
    );
}

#[test]
fn home_end_and_the_arrows_move_it() {
    assert!(dump("end", &["--key", "Tab", "--key", "End"]).contains(r#"= "100 percent""#));
    assert!(dump("home", &["--key", "Tab", "--key", "Home"]).contains(r#"= "0 percent""#));
    assert!(
        dump("arrow", &["--key", "Tab", "--key", "Right"]).contains(r#"= "45 percent""#),
        "an arrow steps by five"
    );
}

#[test]
fn a_drag_follows_the_pointer_and_a_press_jumps() {
    let drag = dump(
        "drag",
        &[
            "--hold",
            "60,80",
            "--hover",
            "900,80",
            "--release",
            "900,80",
        ],
    );
    assert!(drag.contains(r#"= "100 percent""#), "{drag}");
    let press = dump("press", &["--tap", "180,80"]);
    assert!(press.contains(r#"= "50 percent""#), "{press}");
}
