//! `byard shot` renders the project it is pointed at, at the size and scale
//! asked for, with the colours the program wrote.
//!
//! Skips when the machine has no GPU adapter, which is the only way the
//! command itself can fail on a valid project.

use std::path::PathBuf;
use std::process::Command;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-shot-test-{}", std::process::id()));
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
    let _ = std::fs::remove_dir_all(&dir);
}
