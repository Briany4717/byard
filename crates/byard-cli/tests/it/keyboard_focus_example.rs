//! Guards the committed keyboard focus example (issue #347) through the real
//! `byard` binary: it checks clean, and the keys its header gives, pressed by
//! `byard shot --key`, move focus and change state as the header says.
//!
//! The state is read from the accessibility dump (no GPU needed). The picture,
//! which shows the focus ring, is compared against the same shot with no keys
//! pressed, and skipped on a machine with no GPU adapter.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/keyboard_focus")
}

fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("byard-keyboard-focus-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The accessibility dump after pressing `keys` in order.
fn dump(tag: &str, keys: &[&str]) -> String {
    let out = scratch(tag).join("tree.txt");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_byard"));
    cmd.arg("shot").arg(example_dir()).arg("--a11y").arg(&out);
    for key in keys {
        cmd.arg("--key").arg(key);
    }
    let run = cmd.output().expect("run `byard shot --a11y`");
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read_to_string(&out).expect("the tree was written")
}

fn line<'a>(dump: &'a str, starts: &str) -> &'a str {
    dump.lines()
        .find(|l| l.starts_with(starts))
        .unwrap_or_else(|| panic!("no `{starts}` in\n{dump}"))
}

#[test]
fn keyboard_focus_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "got:\n{stdout}");
}

#[test]
fn tab_and_space_focus_and_press_the_save_button() {
    let before = dump("none", &[]);
    assert!(line(&before, "Button \"Save\"").contains("[focusable]"));
    assert!(
        !before.contains("focused]") && !before.contains("focused "),
        "{before}"
    );
    assert!(line(&before, "Label \"saved").contains("saved 0"));

    let after = dump("tab-space", &["Tab", "Space"]);
    assert!(
        line(&after, "Button \"Save\"").contains("focusable focused"),
        "{after}"
    );
    assert!(line(&after, "Label \"saved").contains("saved 1"), "{after}");
    // The disabled button is in the tree, focusable in name only, and never pressed.
    assert!(
        line(&after, "Button \"Locked\"").contains("disabled"),
        "{after}"
    );
}

#[test]
fn the_keys_reach_the_toggle_and_the_slider_and_skip_the_disabled_button() {
    // Tab, Tab: past "Locked", onto the toggle. Space flips it.
    let toggled = dump("toggle", &["Tab", "Tab", "Space"]);
    assert!(line(&toggled, "Switch").contains("= on"), "{toggled}");
    assert!(line(&toggled, "Switch").contains("focused"), "{toggled}");
    assert!(
        line(&toggled, "Label \"saved").contains("saved 0"),
        "{toggled}"
    );

    // Three Tabs reach the slider; ArrowRight raises it by its step.
    let slid = dump("slider", &["Tab", "Tab", "Tab", "Right"]);
    assert!(line(&slid, "Slider").contains("= 0.6"), "{slid}");
    assert!(
        line(&slid, "Label \"saved").contains("volume 0.6"),
        "{slid}"
    );

    // Shift+Tab goes back to the toggle.
    let back = dump("back", &["Tab", "Tab", "Tab", "shift+Tab", "Space"]);
    assert!(line(&back, "Switch").contains("= on"), "{back}");
}

/// The ring is a picture, so it is compared as one: the first Tab changes it.
#[test]
fn the_focus_ring_is_drawn_on_the_focused_button() {
    let png = |tag: &str, keys: &[&str]| -> Option<Vec<u8>> {
        let out = scratch(tag).join("out.png");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_byard"));
        cmd.arg("shot").arg(example_dir()).arg("-o").arg(&out);
        for key in keys {
            cmd.arg("--key").arg(key);
        }
        let run = cmd.output().expect("run `byard shot`");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        if text.contains("no GPU adapter") {
            return None;
        }
        assert!(run.status.success(), "{text}");
        Some(std::fs::read(&out).expect("a PNG"))
    };
    let Some(plain) = png("plain", &[]) else {
        eprintln!("no GPU adapter, skipping the ring picture");
        return;
    };
    let focused = png("ring", &["Tab"]).expect("the same machine has the same GPU");
    assert_ne!(plain, focused, "a focused button draws a ring");
}
