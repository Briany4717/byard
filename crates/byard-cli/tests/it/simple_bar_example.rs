//! Guards the RFC-0068 `Panel` example through the real `byard` binary: the
//! `simple_bar` project checks clean, `byard shot --surface` draws a panel at
//! the size the placement solver gives it with a transparent background,
//! `byard test` sees panels like windows, and `byard dev` stops with
//! `PanelsUnsupported` where there is no Wayland session to put them on.

use std::path::PathBuf;
use std::process::{Command, Output};

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/simple_bar")
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
    let dir = std::env::temp_dir().join(format!("byard-simple-bar-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_simple_bar_example_checks_clean() {
    let (out, text) = run(&["check", example_dir().to_str().unwrap()]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("0 errors"), "{text}");
}

/// The program the hand checklist of the shell backend runs: it has to check
/// clean, and it has to be a program of panels, or it would not exercise it.
#[test]
fn the_program_the_desktop_checklist_runs_is_a_pair_of_panels() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/shell_check");
    let (out, text) = run(&["check", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{text}");
    let source = std::fs::read_to_string(dir.join("src/main.byd")).unwrap();
    let parsed = byard_compiler::parser::parse(&source);
    let shape = byard_compiler::surfaces::surface_shape(&parsed.views);
    assert!(shape.has_panel && !shape.has_window);
    assert!(!source.contains("every "), "it must not move on its own");
}

#[test]
fn a_bar_is_drawn_across_the_screen_with_a_transparent_background() {
    let dir = scratch("bar");
    let png = dir.join("bar.png");
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--surface",
        "bar",
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
    let img = image::open(&png).expect("a PNG").to_rgba8();
    let (w, h) = img.dimensions();
    // The screen is the shot's size (440 wide), at scale 2.
    assert_eq!(w, 880, "the width of the screen, stretched");
    assert!(h > 20 && h < 120, "as tall as its content: {h}");
    assert_eq!(
        img.get_pixel(w - 1, h - 1).0[3],
        0,
        "nothing was drawn there"
    );
    assert!(img.pixels().any(|p| p.0[3] > 0), "but the text was drawn");
}

#[test]
fn an_island_is_as_large_as_its_content() {
    let dir = scratch("island");
    let png = dir.join("clock.png");
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--surface",
        "clock",
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
    let (w, h) = image::open(&png).unwrap().to_rgba8().dimensions();
    assert!(w < 440 && h < 200, "{w}x{h}");
    // A later moment has longer text: the panel was sent the new size.
    let later = dir.join("later.png");
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--surface",
        "clock",
        "--at",
        "12000",
        "-o",
        later.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{text}");
    let (w2, _) = image::open(&later).unwrap().to_rgba8().dimensions();
    assert!(w2 > w, "{w} then {w2}");
}

#[test]
fn the_dump_starts_each_panel_with_its_namespace_and_screen() {
    let dir = scratch("dump");
    let tree_file = dir.join("tree.txt");
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--a11y",
        tree_file.to_str().unwrap(),
        "--at",
        "0",
    ]);
    assert!(out.status.success(), "{text}");
    let dump = std::fs::read_to_string(&tree_file).unwrap();
    assert!(
        dump.lines().any(|l| l == "Panel \"bar\" @ screen-1"),
        "{dump}"
    );
    assert!(
        dump.lines().any(|l| l == "Panel \"clock\" @ screen-1"),
        "{dump}"
    );
}

#[test]
fn the_scenarios_see_the_panels() {
    let (out, text) = run(&["test", example_dir().to_str().unwrap()]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed; 0 failed"), "{text}");
}

/// A session with no compositor: the socket named does not exist, and the
/// runtime directory is empty. Nothing is ever opened, wherever the test runs.
fn no_wayland(args: &[&str]) -> (Output, String) {
    let runtime = scratch("no-wayland");
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .args(args)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("WAYLAND_DISPLAY", "byard-test-no-such-socket")
        .env_remove("WAYLAND_SOCKET")
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
fn a_program_with_panels_stops_where_there_is_no_wayland_session() {
    let (out, text) = no_wayland(&["dev", example_dir().to_str().unwrap()]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("PanelsUnsupported"), "{text}");
    assert!(text.contains("no Wayland session"), "{text}");
    assert!(text.contains("zwlr_layer_shell_v1"), "{text}");
}
