//! `byard shot` renders the project it is pointed at, at the size and scale
//! asked for, with the colours the program wrote.
//!
//! Skips when the machine has no GPU adapter, which is the only way the
//! command itself can fail on a valid project.

use std::path::PathBuf;
use std::process::Command;

fn scratch() -> PathBuf {
    scratch_named("")
}

/// A fresh directory of its own, so tests that run side by side do not share one.
fn scratch_named(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-shot-test{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_shot_is_the_project_at_the_asked_size_in_its_own_colours() {
    let dir = scratch();
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"shot\"\nentry = \"main.byd\"\n[theme.color.light]\nprimary = \"#69737F\"\n",
    )
    .unwrap();
    // The left half is a theme token, the right half a literal, so the shot
    // proves it read the manifest as well as the view.
    std::fs::write(
        dir.join("main.byd"),
        "View Main() {\n    inject Theme as t\n    Row {\n        \
         Box #[bg: t.primary, width: 50, height: 40] {}\n        \
         Box #[bg: 0xE8543F, width: 50, height: 40] {}\n    }\n}\n",
    )
    .unwrap();
    let png = dir.join("out.png");
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .args(["shot", dir.to_str().unwrap(), "-o", png.to_str().unwrap()])
        .args(["--size", "100x40", "--scale", "2", "--at", "0"])
        .output()
        .expect("run byard shot");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if text.contains("no GPU adapter") {
        eprintln!("no GPU adapter, skipping byard shot");
        return;
    }
    assert!(out.status.success(), "{text}");

    let img = image::open(&png).expect("a PNG").to_rgb8();
    assert_eq!(
        img.dimensions(),
        (200, 80),
        "the logical size times the scale"
    );
    let close = |got: &image::Rgb<u8>, want: [u8; 3]| {
        got.0.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 2)
    };
    let token = img.get_pixel(50, 40);
    let literal = img.get_pixel(150, 40);
    assert!(close(token, [105, 115, 127]), "the theme token: {token:?}");
    assert!(close(literal, [232, 84, 63]), "the literal: {literal:?}");

    // Where the app drew nothing, the shot is what the window shows: opaque
    // black, not transparent.
    std::fs::write(
        dir.join("main.byd"),
        "View Main() { Box #[bg: 0xE8543F, width: 20, height: 20] {} }\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .args(["shot", dir.to_str().unwrap(), "-o", png.to_str().unwrap()])
        .args(["--size", "100x40", "--scale", "1", "--at", "0"])
        .output()
        .expect("run byard shot");
    assert!(out.status.success());
    let empty = *image::open(&png).unwrap().to_rgba8().get_pixel(80, 30);
    assert_eq!(empty.0, [0, 0, 0, 255], "an empty area is opaque black");
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0041 §7: `--tap`, `--type` and `--key` run in the order they are
/// written. Typing `ab` and then pressing Shift+Left selects the `b`; the same
/// keys with the press first select nothing, and the pictures say which.
#[test]
fn a_shot_taps_types_and_presses_keys_in_the_order_written() {
    let dir = scratch_named("-typed");
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"typed\"\nentry = \"main.byd\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.byd"),
        "View Main() {\n    var q: Str = \"\"\n    \
         TextField #[bind: q, bg: 0x20242C, color: 0xFFFFFF, size: 30, width: 200, height: 60]\n}\n",
    )
    .unwrap();
    let shot = |name: &str, steps: &[&str]| -> Option<Vec<u8>> {
        let png = dir.join(format!("{name}.png"));
        let out = Command::new(env!("CARGO_BIN_EXE_byard"))
            .args(["shot", dir.to_str().unwrap(), "-o", png.to_str().unwrap()])
            .args(["--size", "220x80", "--scale", "1", "--at", "0"])
            .args(steps)
            .output()
            .expect("run byard shot");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if text.contains("no GPU adapter") {
            return None;
        }
        assert!(out.status.success(), "{text}");
        Some(image::open(&png).expect("a PNG").to_rgb8().into_raw())
    };
    let Some(plain) = shot("plain", &["--tap", "100,30", "--type", "ab"]) else {
        eprintln!("no GPU adapter, skipping byard shot");
        return;
    };
    let selected = shot(
        "selected",
        &["--tap", "100,30", "--type", "ab", "--key", "shift+Left"],
    )
    .unwrap();
    let too_early = shot(
        "early",
        &["--tap", "100,30", "--key", "shift+Left", "--type", "ab"],
    )
    .unwrap();
    assert_ne!(plain, selected, "the selection is in the picture");
    assert_eq!(
        plain, too_early,
        "a key pressed before the text exists selects nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
