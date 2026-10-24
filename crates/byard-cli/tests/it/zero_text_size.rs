//! A text at size 0, written or bound to a `var` that holds 0, takes no room
//! and draws nothing. Both used to panic inside `cosmic-text` ("line height
//! cannot be 0"): `byard check` measures text to lay a view out, so the check
//! itself crashed, and so did any window or shot that drew one.
//!
//! The shot half skips when the machine has no GPU adapter.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VIEW: &str = "View Main() {
    var size = 0
    Column #[p: 24, gap: 12, bg: 0x000000, grow: 1] {
        Text(\"Above\") #[size: 24, color: 0xFFFFFF]
        Text(\"written as 0\") #[size: 0]
        Text(\"bound to a var holding 0\") #[size: size]
        Text(\"Below\") #[size: 24, color: 0xFFFFFF]
    }
}
";

/// A project of its own for each test: they run at the same time, and one
/// removing a shared directory under the other is a failure of neither.
fn project(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "byard-zero-text-size-{test}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"zero\"\nentry = \"src/main.byd\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.byd"), VIEW).unwrap();
    dir
}

fn byard(dir: &Path, args: &[&str]) -> (Output, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .args(args)
        .arg(dir)
        .output()
        .expect("run byard");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

#[test]
fn byard_check_passes_a_text_at_size_zero() {
    let dir = project("check");
    let (out, text) = byard(&dir, &["check"]);
    assert!(!text.contains("panicked"), "{text}");
    assert!(out.status.success(), "{text}");
    assert!(text.contains("0 errors"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_shot_draws_the_text_around_a_text_at_size_zero() {
    let dir = project("shot");
    let png = dir.join("out.png");
    let (out, text) = byard(
        &dir,
        &[
            "shot",
            "-o",
            png.to_str().unwrap(),
            "--size",
            "200x140",
            "--at",
            "0",
        ],
    );
    if text.contains("no GPU adapter") {
        eprintln!("no GPU adapter, skipping the shot");
        return;
    }
    assert!(!text.contains("panicked"), "{text}");
    assert!(out.status.success(), "{text}");

    // "Above" and "Below" are drawn, so the picture is not one flat colour.
    let img = image::open(&png).expect("a PNG").to_rgb8();
    let first = *img.get_pixel(0, 0);
    assert!(
        img.pixels()
            .any(|p| p.0.iter().zip(first.0).any(|(a, b)| a.abs_diff(b) > 64)),
        "nothing was drawn"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
