//! `byard shot` goes straight to its last frame once the app is at rest, and
//! the picture is the one it drew when it rendered every frame.
//!
//! Each case is shot twice, the second time with `BYARD_SHOT_EVERY_FRAME`
//! (the 16 ms frames all drawn, as it used to), and the two PNGs and the
//! accessibility trees must be the same bytes. The examples are the ones with
//! something that moves or waits: springs, keyframes, a looping animation, a
//! ripple, a scroll snap, a theme change, a timer, `now()`, a canvas whose
//! paths are generated on another thread.
//!
//! Skips when the machine has no GPU adapter.

use std::path::{Path, PathBuf};
use std::process::Command;

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name)
}

/// One shot; `None` when there is no GPU to draw with.
fn shot(name: &str, every_frame: bool, args: &[&str]) -> Option<(Vec<u8>, String)> {
    let dir = std::env::temp_dir().join(format!(
        "byard-shot-idle-{}-{name}-{every_frame}-{}",
        std::process::id(),
        args.join("_").replace(['/', ' ', ','], "-")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (png, tree) = (dir.join("out.png"), dir.join("tree.txt"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_byard-cli"));
    cmd.arg("shot")
        .arg(example(name))
        .args([
            "-o",
            png.to_str().unwrap(),
            "--a11y",
            tree.to_str().unwrap(),
        ])
        .args(["--now", "2026-11-05T09:00:00"])
        .args(args);
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
    assert!(out.status.success(), "{name}: {said}");
    let result = (
        std::fs::read(&png).unwrap(),
        std::fs::read_to_string(&tree).unwrap(),
    );
    let _ = std::fs::remove_dir_all(&dir);
    Some(result)
}

fn same_either_way(name: &str, args: &[&str]) {
    let Some(fast) = shot(name, false, args) else {
        return;
    };
    let slow = shot(name, true, args).expect("the first shot had a GPU");
    assert!(
        fast.0 == slow.0,
        "{name} {args:?}: the picture differs when the idle frames are skipped"
    );
    assert_eq!(fast.1, slow.1, "{name} {args:?}: the accessibility tree");
}

#[test]
fn animated_examples_draw_the_same_picture() {
    for name in [
        "looping_animations",
        "view_animations",
        "list_animations",
        "colour_motion",
        "shape_morph",
        "style_states",
    ] {
        for at in ["0", "250", "1500"] {
            same_either_way(name, &["--at", at]);
        }
    }
}

#[test]
fn timers_clocks_and_late_work_draw_the_same_picture() {
    for name in ["timers", "canvas_shapes", "clock", "seed_scheme", "theme"] {
        if !example(name).exists() {
            continue;
        }
        same_either_way(name, &["--at", "1500"]);
        same_either_way(name, &["--at", "3000"]);
    }
}

#[test]
fn input_then_rest_draws_the_same_picture() {
    // A tap starts a ripple, which is in flight for a while and then not.
    same_either_way("ripple", &["--at", "1500", "--tap", "100,100"]);
    same_either_way("ripple", &["--at", "100", "--tap", "100,100"]);
    same_either_way("scroll_snap", &["--at", "1500", "--hover", "100,200"]);
    same_either_way(
        "text_editing",
        &["--at", "1500", "--tap", "100,100", "--type", "hi"],
    );
}

#[test]
fn a_wait_between_inputs_draws_the_same_picture() {
    // The ripple is over before the wait ends, so the wait skips its rest, and
    // the second tap must land at the same time as when every frame is drawn.
    same_either_way(
        "ripple",
        &[
            "--at", "1500", "--tap", "100,100", "--wait", "700", "--tap", "120,100",
        ],
    );
    same_either_way(
        "text_editing",
        &[
            "--at", "1500", "--tap", "100,100", "--wait", "1000", "--type", "hi",
        ],
    );
}
