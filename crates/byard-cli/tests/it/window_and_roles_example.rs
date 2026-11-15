//! Guards the committed RFC-0067 example through the real `byard` binary: it
//! checks clean, a phone gets a bar and a tablet a rail, and the roles reach
//! the accessibility tree.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/window_and_roles")
}

fn dump(tag: &str, args: &[&str]) -> String {
    let out = std::env::temp_dir().join(format!("byard-wr-{tag}-{}.txt", std::process::id()));
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
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
    let text = std::fs::read_to_string(&out).expect("the tree was written");
    let _ = std::fs::remove_file(&out);
    text
}

#[test]
fn window_and_roles_example_checks_clean() {
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
fn a_phone_has_a_bar_and_a_tablet_a_rail() {
    let phone = dump("phone", &["--size", "440x880"]);
    let tablet = dump("tablet", &["--size", "760x520"]);
    assert!(phone.contains("Navigation \"Sections\" @ 0,"), "{phone}");
    assert!(phone.contains("440x64"), "a bar is full width\n{phone}");
    assert!(
        tablet.contains("Navigation \"Sections\" @ 0,0 80x"),
        "{tablet}"
    );
}

#[test]
fn the_roles_reach_the_tree() {
    let tree = dump("roles", &[]);
    assert!(
        tree.contains("SpinButton \"Hours\" = 4 in 1..12 step 1"),
        "{tree}"
    );
    assert!(tree.contains("Grid \"November 2026\""), "{tree}");
    assert!(
        tree.contains("GridCell \"November 12\" [focusable selected]"),
        "{tree}"
    );
    assert!(
        tree.contains("GridCell \"November 16\" [focusable disabled]"),
        "{tree}"
    );
}

#[test]
fn the_arrows_walk_the_grid_and_page_down_is_the_cells() {
    // Tab x3 reaches the selected day; Down skips nothing, Right goes on.
    let walk = dump(
        "walk",
        &[
            "--key", "Tab", "--key", "Tab", "--key", "Tab", "--key", "Down", "--key", "Right",
        ],
    );
    assert!(
        walk.contains("GridCell \"November 20\" [focusable focused]"),
        "{walk}"
    );
    let page = dump(
        "page",
        &[
            "--key", "Tab", "--key", "Tab", "--key", "Tab", "--key", "PageDown",
        ],
    );
    assert!(
        page.contains("GridCell \"November 12\" [focusable focused]"),
        "{page}"
    );
    assert!(
        page.contains("GridCell \"November 19\" [focusable selected]"),
        "{page}"
    );
}
