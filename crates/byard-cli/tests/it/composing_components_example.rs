//! Guards the committed composing components example (RFC-0062) through the
//! real `byard` binary: it checks clean, the switch in a trailing slot works,
//! only the item that was given `on_tap` is a button, and Tab reaches the field.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/composing_components")
}

fn dump(tag: &str, args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("byard-composing-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("tree.txt");
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
        .args(args)
        .arg("--a11y")
        .arg(&out)
        .arg("-o")
        .arg(dir.join("shot.png"))
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
fn the_example_checks_clean() {
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
fn only_the_item_given_on_tap_is_a_button_and_slots_are_drawn() {
    let tree = dump("rest", &[]);
    assert!(tree.contains("Button \"About\" [focusable]"), "{tree}");
    assert!(!tree.contains("Button \"Wi-Fi\""), "{tree}");
    assert!(!tree.contains("Button \"Bluetooth\""), "{tree}");
    assert!(
        tree.contains("Switch (unnamed) = on"),
        "the trailing slot: {tree}"
    );
    assert!(tree.contains("Label \"Name\""), "{tree}");
}

#[test]
fn a_tap_on_the_switch_in_the_trailing_slot_turns_it_off() {
    let tree = dump("tap", &["--tap", "378,88"]);
    assert!(tree.contains("Switch (unnamed) = off"), "{tree}");
}

#[test]
fn tab_reaches_the_field_after_the_switch_and_the_clickable_item() {
    let tree = dump(
        "tab",
        &[
            "--key", "Tab", "--key", "Tab", "--key", "Tab", "--type", "Ada",
        ],
    );
    assert!(
        tree.contains("TextInput (unnamed) = \"Ada\" [focusable focused]"),
        "{tree}"
    );
}
