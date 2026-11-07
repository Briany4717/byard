//! Guards the RFC-0057 example, `colour_motion`, through the real binary: it
//! checks clean, a tap shows it half-way (different from the start and from
//! the end), and a gradient animated into one of another kind is refused.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/colour_motion")
}

fn byard(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .args(args)
        .output()
        .expect("run byard");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn colour_motion_example_checks_clean() {
    let (ok, text) = byard(&["check", example_dir().to_str().unwrap()]);
    assert!(ok && text.contains("0 errors"), "{text}");
}

#[test]
fn a_tap_is_drawn_part_way_between_the_two_colours() {
    let dir = std::env::temp_dir().join(format!("byard-colour-motion-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let shot = |at: &str| -> Option<Vec<u8>> {
        let png = dir.join(format!("at{at}.png"));
        let (ok, text) = byard(&[
            "shot",
            example_dir().to_str().unwrap(),
            "--tap",
            "60,70",
            "--at",
            at,
            "--size",
            "440x560",
            "--scale",
            "1",
            "-o",
            png.to_str().unwrap(),
        ]);
        if text.contains("no GPU adapter") {
            return None;
        }
        assert!(ok, "{text}");
        Some(image::open(&png).expect("a PNG").to_rgb8().into_raw())
    };
    let Some(start) = shot("0") else {
        eprintln!("no GPU adapter, skipping byard shot");
        return;
    };
    let mid = shot("120").unwrap();
    let end = shot("1500").unwrap();
    assert_ne!(start, mid, "the tap has started something by 120 ms");
    assert_ne!(mid, end, "and it has not finished");
    // Deterministic: the same time is the same picture.
    assert_eq!(mid, shot("120").unwrap());
}

#[test]
fn animating_into_a_gradient_of_another_kind_is_refused() {
    let dir = std::env::temp_dir().join(format!("byard-gradient-shape-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"g\"\nentry = \"main.byd\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.byd"),
        "View Main() {\n    var on = false\n    Box #[width: 40, height: 40, bg: 0x000000,\n \
         gradient: on ? (from: 0xFF0000, to: 0x0000FF)\n \
         : (kind: radial, radius: 0.8, from: 0xFF0000, to: 0x0000FF) with anim.spring()] {}\n}\n",
    )
    .unwrap();
    let (ok, text) = byard(&["check", dir.to_str().unwrap()]);
    assert!(!ok, "{text}");
    assert!(text.contains("GradientShapeMismatch"), "{text}");
}
