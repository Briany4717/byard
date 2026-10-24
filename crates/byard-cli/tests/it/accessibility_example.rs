//! Guards the committed RFC-0042 example through the real `byard` binary: the
//! `accessibility` project must check clean, and the tree `byard shot --a11y`
//! writes for it is the committed `tree.txt`, byte for byte.
//!
//! The dump needs no GPU, no screen reader and no window, which is what lets a
//! test compare a whole window's accessibility tree. Its bounds depend on how
//! wide text is, so the example sets its text in a declared font and the file
//! comes out the same on every machine.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/accessibility")
}

fn dump() -> String {
    // One file per call: the tests run side by side in one process.
    static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let out =
        std::env::temp_dir().join(format!("byard-a11y-example-{}-{n}.txt", std::process::id()));
    let run = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("shot")
        .arg(example_dir())
        .arg("--a11y")
        .arg(&out)
        .output()
        .expect("run `byard shot --a11y`");
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
fn accessibility_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "got:\n{stdout}");
}

/// The tree of the example is the golden file. If a change here is on
/// purpose, regenerate it with
/// `cargo run -p byard-cli -- shot crates/byard-cli/examples/accessibility --a11y crates/byard-cli/examples/accessibility/tree.txt`
/// and read the diff: it is the change in what a screen reader is told.
#[test]
fn the_example_tree_is_the_committed_golden_file() {
    let golden = std::fs::read_to_string(example_dir().join("tree.txt")).expect("tree.txt");
    assert_eq!(dump(), golden);
}

/// The example has exactly one control a screen reader could not name, and
/// the dump says which: that is its whole use as a check.
#[test]
fn the_dump_marks_the_one_control_it_cannot_name() {
    let text = dump();
    let unnamed: Vec<&str> = text.lines().filter(|l| l.contains("(unnamed)")).collect();
    assert_eq!(unnamed.len(), 1, "{text}");
    assert!(unnamed[0].starts_with("Button (unnamed)"), "{text}");
    // The hidden hero image and the decoration inside the buttons are not in it.
    assert!(!text.contains("Image"), "{text}");
}

/// `--a11y` alone draws no picture: it works where there is no GPU.
#[test]
fn asking_only_for_the_tree_writes_no_picture() {
    let dir = std::env::temp_dir().join(format!("byard-a11y-nopng-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tree = dir.join("tree.txt");
    let run = Command::new(env!("CARGO_BIN_EXE_byard"))
        .current_dir(&dir)
        .arg("shot")
        .arg(example_dir())
        .arg("--a11y")
        .arg(&tree)
        .output()
        .expect("run `byard shot --a11y`");
    assert!(run.status.success());
    assert!(tree.exists());
    assert!(!dir.join("shot.png").exists(), "no picture was asked for");
    let _ = std::fs::remove_dir_all(&dir);
}
