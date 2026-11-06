//! Guards the committed RFC-0049 example through the real `byard` binary: the
//! `timers` project, which now shows the clock, checks clean, and `byard shot
//! --now` draws the time it was told, with no wait on a real clock.
//!
//! The text is read from the accessibility dump `byard shot --a11y` writes,
//! which needs no GPU.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/timers")
}

/// The labels of a shot of the example, run with `extra` flags.
fn labels(tag: &str, extra: &[&str]) -> Vec<String> {
    let out = std::env::temp_dir().join(format!("byard-clock-{tag}-{}.txt", std::process::id()));
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
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
fn the_timers_example_checks_clean() {
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
fn a_shot_with_now_shows_that_time_and_a_tap_stamps_it() {
    let shown = labels("tap", &["--now", "2026-11-05T09:00:00", "--tap", "100,324"]);
    assert!(
        shown.contains(&"Thu 5 Nov, 09:00:00".to_string()),
        "`on mount` wrote the fixed time: {shown:?}"
    );
    assert!(
        shown.contains(&"2026-11-05T09:00:00".to_string()),
        "the button stamped it: {shown:?}"
    );
    // The same command twice is the same picture's text: that is the point.
    let again = labels(
        "again",
        &["--now", "2026-11-05T09:00:00", "--tap", "100,324"],
    );
    assert_eq!(shown, again);
}

#[test]
fn a_shot_without_now_reads_the_real_clock() {
    let shown = labels("real", &["--tap", "100,324"]);
    let stamp = shown
        .iter()
        .find(|l| l.len() == 19 && l.as_bytes()[10] == b'T')
        .unwrap_or_else(|| panic!("a stamp is shown: {shown:?}"));
    assert!(stamp.starts_with("20"), "{stamp}");
    assert_ne!(stamp, "2026-11-05T09:00:00");
}

#[test]
fn a_now_that_is_not_a_date_is_refused_before_anything_is_drawn() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
        .args(["--now", "tomorrow", "-o"])
        .arg(std::env::temp_dir().join("byard-clock-never.png"))
        .output()
        .expect("run `byard shot`");
    assert!(!out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("--now") && text.contains("2026-11-05T09:00:00"),
        "{text}"
    );
}
