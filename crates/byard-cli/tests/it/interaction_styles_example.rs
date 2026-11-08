//! Guards the committed interaction styles example (RFC-0059) through the real
//! `byard` binary: it checks clean, the keys and the tap its header gives change
//! the state it says, a role comes from a value, and each state is a picture of
//! its own.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/interaction_styles")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-interaction-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs `byard shot` with `args` and returns whether it worked and its output.
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
fn one_root_is_a_button_or_a_toggle_and_the_disabled_one_is_skipped() {
    let tree = dump("roles", &[]);
    assert!(
        line(&tree, "Button \"Save\"").contains("[focusable]"),
        "{tree}"
    );
    assert!(
        line(&tree, "ToggleButton \"Star\"").contains("[focusable]"),
        "{tree}"
    );
    assert!(
        line(&tree, "Button \"Off\"").contains("[focusable disabled]"),
        "{tree}"
    );

    // Tab, Tab lands on the toggle, not on the disabled one after it.
    let tabbed = dump("skip", &["--key", "Tab", "--key", "Tab", "--key", "Tab"]);
    assert!(
        line(&tabbed, "Button \"Save\"").contains("focused"),
        "the third Tab wraps to the first control, skipping the disabled one:\n{tabbed}"
    );
}

#[test]
fn space_presses_the_toggle_and_a_reader_hears_it_on() {
    let tree = dump("space", &["--key", "Tab", "--key", "Tab", "--key", "Space"]);
    assert!(
        line(&tree, "ToggleButton \"Star\"").contains("focusable focused checked"),
        "{tree}"
    );
    let plain = dump("save", &["--key", "Tab", "--key", "Space"]);
    assert!(plain.contains("Taps: 1"), "{plain}");
}

/// Each state is a picture of its own: the layer, the held press, the ring.
#[test]
fn rest_hover_hold_and_tab_are_four_different_pictures() {
    let png = |tag: &str, args: &[&str]| -> Option<Vec<u8>> {
        let out = scratch(tag).join("out.png");
        let (ok, text) = shot(args, "-o", &out);
        if text.contains("no GPU adapter") {
            return None;
        }
        assert!(ok, "{text}");
        Some(std::fs::read(&out).expect("a PNG"))
    };
    let Some(rest) = png("rest", &[]) else {
        eprintln!("no GPU adapter, skipping the pictures");
        return;
    };
    let hover = png("hover", &["--hover", "80,100"]).unwrap();
    let hold = png("hold", &["--hold", "80,100", "--at", "120"]).unwrap();
    let tab = png("tab", &["--key", "Tab"]).unwrap();
    let tap = png("tap", &["--tap", "80,100"]).unwrap();
    for (name, picture) in [("hover", &hover), ("hold", &hold), ("tab", &tab)] {
        assert_ne!(&rest, picture, "{name} draws something");
    }
    assert_ne!(hover, hold, "pressed is not hovered");
    assert_ne!(hover, tab, "the ring is not the layer");
    assert_ne!(tab, tap, "only the keyboard draws the ring");
}
