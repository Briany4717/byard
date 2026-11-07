//! Guards the committed composed controls example (RFC-0052) through the real
//! `byard` binary: it checks clean, the keys and the tap its header gives
//! change the state it says, and the ring is a picture the keyboard draws and
//! the pointer does not.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/composed_controls")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-composed-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs `byard shot` with `args` and returns its output text.
fn shot(args: &[&str], out_flag: &str, out: &PathBuf) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_byard-cli"));
    cmd.arg("shot").arg(example_dir()).args(args);
    cmd.arg(out_flag).arg(out);
    let run = cmd.output().expect("run `byard shot`");
    (
        run.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        ),
    )
}

/// The accessibility dump after `args` (keys and taps) in order.
fn dump(tag: &str, args: &[&str]) -> String {
    let out = scratch(tag).join("tree.txt");
    let (ok, text) = shot(args, "--a11y", &out);
    assert!(ok, "{text}");
    std::fs::read_to_string(&out).expect("the tree was written")
}

fn line<'a>(dump: &'a str, starts: &str) -> &'a str {
    dump.lines()
        .find(|l| l.starts_with(starts))
        .unwrap_or_else(|| panic!("no `{starts}` in\n{dump}"))
}

#[test]
fn composed_controls_example_checks_clean() {
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
fn the_boxes_are_a_switch_a_checkbox_and_a_link_named_for_a_reader() {
    let tree = dump("roles", &[]);
    assert_eq!(
        line(&tree, "Switch"),
        "Switch \"Wi-Fi\" = on [focusable] @ 24,64 52x32"
    );
    assert!(
        line(&tree, "CheckBox").starts_with("CheckBox \"I agree\" [focusable]"),
        "{tree}"
    );
    assert!(
        line(&tree, "Link").starts_with("Link \"Docs 0\" [focusable]"),
        "{tree}"
    );
}

#[test]
fn tab_space_and_enter_press_the_composed_controls_as_their_role_says() {
    // Tab, Tab: the checkbox. Enter does nothing to it, Space ticks it.
    let enter = dump("enter", &["--key", "Tab", "--key", "Tab", "--key", "Enter"]);
    assert!(
        line(&enter, "CheckBox").contains("focusable focused]"),
        "{enter}"
    );
    let space = dump("space", &["--key", "Tab", "--key", "Tab", "--key", "Space"]);
    assert!(
        line(&space, "CheckBox").contains("focusable focused checked"),
        "{space}"
    );
    assert!(
        line(&space, "Label \"Wi-Fi").contains("agreed yes"),
        "{space}"
    );

    // Tab x3: the link. Space does nothing, Enter follows it.
    let link_space = dump(
        "link-space",
        &[
            "--key", "Tab", "--key", "Tab", "--key", "Tab", "--key", "Space",
        ],
    );
    assert!(line(&link_space, "Link").contains("Docs 0"), "{link_space}");
    let link_enter = dump(
        "link-enter",
        &[
            "--key", "Tab", "--key", "Tab", "--key", "Tab", "--key", "Enter",
        ],
    );
    assert!(line(&link_enter, "Link").contains("Docs 1"), "{link_enter}");
}

/// The ring is a picture, so it is compared as one: Tab draws it, a tap does
/// not (the tap flips the switch, so the tapped picture is compared with the one
/// of the same state reached with Space, which has the ring).
#[test]
fn the_ring_is_drawn_for_the_keyboard_and_not_for_a_tap() {
    let png = |tag: &str, args: &[&str]| -> Option<Vec<u8>> {
        let out = scratch(tag).join("out.png");
        let (ok, text) = shot(args, "-o", &out);
        if text.contains("no GPU adapter") {
            return None;
        }
        assert!(ok, "{text}");
        Some(std::fs::read(&out).expect("a PNG"))
    };
    let Some(plain) = png("plain", &[]) else {
        eprintln!("no GPU adapter, skipping the ring pictures");
        return;
    };
    let tabbed = png("tab", &["--key", "Tab"]).expect("the same machine has the same GPU");
    assert_ne!(plain, tabbed, "Tab draws the ring");

    // Switched off by a key (ring) and by a tap (no ring): same state, different picture.
    let by_key = png("by-key", &["--key", "Tab", "--key", "Space"]).unwrap();
    let by_tap = png("by-tap", &["--tap", "40,80"]).unwrap();
    assert_ne!(by_key, by_tap, "only the keyboard draws the ring");
}
