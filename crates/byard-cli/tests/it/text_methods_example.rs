//! Guards the committed RFC-0047 example through the real `byard` binary: the
//! `text_methods` project checks clean, and the lines it draws are the ones its
//! header says they are.
//!
//! The lines are read from the accessibility dump `byard shot --a11y` writes,
//! which needs no GPU, and compared as text rather than as bounds so the test
//! says what the methods answer, not how wide the font is.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/text_methods")
}

#[test]
fn text_methods_example_checks_clean() {
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
fn the_example_draws_what_its_header_says() {
    let out = std::env::temp_dir().join(format!("byard-text-methods-{}.txt", std::process::id()));
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
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
    let dump = std::fs::read_to_string(&out).expect("the tree was written");
    let _ = std::fs::remove_file(&out);

    let labels: Vec<&str> = dump
        .lines()
        .filter_map(|l| l.strip_prefix("Label \""))
        .filter_map(|l| l.split("\" @").next())
        .collect();
    assert_eq!(
        labels,
        [
            "RFC-0047",
            "matches: Ada Lovelace",
            "words: not or be to",
            "a+b+c 2 5",
            // 7 % 3, -7 % 3 (the dividend's sign), 10%3 (no spaces), 5 % 0.
            "1 -1 1 0",
            "SS true false",
        ],
        "{dump}"
    );
}
