//! Guards the RFC-0068 headless screens through the real `byard` binary, on the
//! `shell_demo` example: two screens, a bar and a dock on each, a menu that is a
//! popup, and a window.

use std::path::PathBuf;
use std::process::{Command, Output};

const SCREENS: &str = "DP-1:1600x900, HDMI-A-1:1280x720";

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/shell_demo")
}

fn run(args: &[&str]) -> (Output, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .args(args)
        .output()
        .expect("run byard");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-shell-demo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_shell_demo_checks_clean() {
    let (out, text) = run(&["check", example_dir().to_str().unwrap()]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("0 errors"), "{text}");
}

#[test]
fn the_scenarios_see_every_surface() {
    let (out, text) = run(&["test", example_dir().to_str().unwrap()]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("4 passed; 0 failed"), "{text}");
}

#[test]
fn the_dump_names_each_panel_with_its_screen() {
    let dir = scratch("dump");
    let file = dir.join("tree.txt");
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--screens",
        SCREENS,
        "--tap",
        "40,30",
        "--a11y",
        file.to_str().unwrap(),
        "--at",
        "0",
    ]);
    assert!(out.status.success(), "{text}");
    let dump = std::fs::read_to_string(&file).unwrap();
    for line in [
        "Panel \"bar\" @ DP-1",
        "Panel \"bar\" @ HDMI-A-1",
        "Panel \"dock\" @ HDMI-A-1",
        "Window \"Settings\"",
        "Popup \"menu\" @ DP-1",
    ] {
        assert!(dump.lines().any(|l| l == line), "{line}: {dump}");
    }
    assert!(!dump.contains("Popup \"menu\" @ HDMI-A-1"), "{dump}");
}

#[test]
fn a_tap_on_the_bars_button_opens_the_menu_and_a_tap_elsewhere_closes_it() {
    let dir = scratch("tap");
    let file = dir.join("tree.txt");
    let dir_arg = example_dir();
    let dump = |taps: &[&str]| {
        let mut args = vec![
            "shot",
            dir_arg.to_str().unwrap(),
            "--screens",
            SCREENS,
            "--a11y",
            file.to_str().unwrap(),
            "--at",
            "0",
        ];
        for t in taps {
            args.extend(["--tap", t]);
        }
        let (out, text) = run(&args);
        assert!(out.status.success(), "{text}");
        std::fs::read_to_string(&file).unwrap()
    };
    assert!(dump(&["40,30"]).contains("Popup"));
    // The second screen sits right of the first: its button is at x 1600 + 40.
    let second = dump(&["1640,30"]);
    assert!(second.contains("Popup \"menu\" @ HDMI-A-1"), "{second}");
    // Outside the popup is the compositor's `popup_done`: it closes, and the
    // press goes no further.
    assert!(!dump(&["40,30", "800,600"]).contains("Popup"));
}

#[test]
fn the_screens_are_drawn_side_by_side_with_every_surface_on_them() {
    let dir = scratch("png");
    let png = dir.join("screens.png");
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--screens",
        SCREENS,
        "--scale",
        "1",
        "--tap",
        "40,30",
        "--at",
        "0",
        "-o",
        png.to_str().unwrap(),
    ]);
    if text.contains("no GPU adapter") {
        eprintln!("no GPU adapter, skipping byard shot");
        return;
    }
    assert!(out.status.success(), "{text}");
    let img = image::open(&png).unwrap().to_rgba8();
    // 1600 + 1280 wide, the taller screen high.
    assert_eq!(img.dimensions(), (2880, 900));
    // The bar's island is drawn at the top of the first screen, and the corner
    // of the shorter second screen is outside both: the checkerboard's dark.
    assert_ne!(img.get_pixel(800, 20).0, img.get_pixel(800, 400).0);
    let outside = img.get_pixel(2800, 880).0;
    assert!(outside[0] < 0x20 && outside[3] == 255, "{outside:?}");
}

#[test]
fn one_surface_alone_and_a_surface_made_per_screen_needs_the_screen() {
    let dir = scratch("one");
    let png = dir.join("bar.png");
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--screens",
        SCREENS,
        "--surface",
        "bar",
        "-o",
        png.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("`DP-1`, `HDMI-A-1`") && text.contains("--screen"),
        "{text}"
    );
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--screens",
        SCREENS,
        "--surface",
        "bar",
        "--screen",
        "HDMI-A-1",
        "--scale",
        "1",
        "--at",
        "0",
        "-o",
        png.to_str().unwrap(),
    ]);
    if text.contains("no GPU adapter") {
        eprintln!("no GPU adapter, skipping byard shot");
        return;
    }
    assert!(out.status.success(), "{text}");
    // The bar of the second screen: its width less the margins, 44 high.
    assert_eq!(
        image::open(&png).unwrap().to_rgba8().dimensions(),
        (1256, 44)
    );
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--screens",
        SCREENS,
        "--screen",
        "DP-9",
        "-o",
        png.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("no screen named `DP-9`"), "{text}");
}

#[test]
fn a_mistake_in_the_screens_is_one_error_that_says_how_to_write_them() {
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--screens",
        "DP-1:wide",
    ]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("NAME:WxH"), "{text}");
}
