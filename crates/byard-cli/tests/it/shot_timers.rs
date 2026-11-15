//! `byard shot` (and `byard test`, which draws through the same scene) runs
//! `after` and `every` on the scene's clock: a wait or `--at` moves them as it
//! moves animations, and skipping idle frames does not skip a timer.
//!
//! Skips when the machine has no GPU adapter.

use std::path::Path;
use std::process::Command;

/// The accessibility tree of the timers example at `at` ms, every frame drawn
/// or not; `None` when there is no GPU to draw with.
fn tree_at(at: &str, every_frame: bool) -> Option<String> {
    let dir = std::env::temp_dir().join(format!(
        "byard-shot-timers-{}-{at}-{every_frame}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (png, tree) = (dir.join("out.png"), dir.join("tree.txt"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_byard-cli"));
    cmd.arg("shot")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/timers"))
        .args(["--size", "640x640", "--now", "2026-11-05T09:00:00"])
        .args(["--at", at, "-o", png.to_str().unwrap()])
        .args(["--a11y", tree.to_str().unwrap()]);
    if every_frame {
        cmd.env("BYARD_SHOT_EVERY_FRAME", "1");
    }
    let out = cmd.output().expect("run byard shot");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if said.contains("no GPU adapter") {
        eprintln!("no GPU adapter, skipping byard shot");
        return None;
    }
    assert!(out.status.success(), "{said}");
    let text = std::fs::read_to_string(&tree).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Some(text)
}

#[test]
fn every_and_after_run_on_the_shot_clock() {
    let Some(start) = tree_at("0", false) else {
        return;
    };
    assert!(start.contains(r#"Label "0s""#), "{start}");
    assert!(!start.contains(r#"Label "✓""#), "{start}");

    let later = tree_at("3500", false).expect("the first shot had a GPU");
    // `every 1s` ticked three times and `now()` moved with it.
    assert!(later.contains(r#"Label "3s""#), "{later}");
    assert!(later.contains(r#"Label "Thu 5 Nov, 09:00:03""#), "{later}");
    // `after 3s` revealed its panel.
    assert!(later.contains(r#"Label "✓""#), "{later}");
}

#[test]
fn skipping_idle_frames_does_not_skip_a_timer() {
    for at in ["1000", "2990", "3010", "3500"] {
        let Some(fast) = tree_at(at, false) else {
            return;
        };
        let slow = tree_at(at, true).expect("the first shot had a GPU");
        assert_eq!(fast, slow, "--at {at}");
    }
}
