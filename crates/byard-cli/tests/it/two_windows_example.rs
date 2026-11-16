//! Guards the RFC-0068 `Window` example through the real `byard` binary: the
//! `two_windows` project checks clean, `byard shot --surface` draws each of its
//! windows at its own size, a shot that does not say which says what the names
//! are, and `byard test` runs scenarios that tap in one window and read the
//! other.

use std::path::PathBuf;
use std::process::{Command, Output};

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/two_windows")
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
    let dir = std::env::temp_dir().join(format!("byard-two-windows-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_two_windows_example_checks_clean() {
    let (out, text) = run(&["check", example_dir().to_str().unwrap()]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("0 errors"), "{text}");
}

#[test]
fn each_window_is_drawn_at_its_own_size() {
    let dir = scratch("sizes");
    for (name, size) in [("counter", (720, 480)), ("inspector", (600, 400))] {
        let png = dir.join(format!("{name}.png"));
        let (out, text) = run(&[
            "shot",
            example_dir().to_str().unwrap(),
            "--surface",
            name,
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
        let img = image::open(&png).expect("a PNG").to_rgb8();
        assert_eq!(
            img.dimensions(),
            size,
            "{name}: its opening size at scale 2"
        );
    }
}

#[test]
fn a_shot_of_several_windows_is_the_screen_and_a_wrong_name_lists_them() {
    let dir = scratch("names");
    let png = dir.join("x.png");
    // With no name the windows are drawn together, on the screen.
    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
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
    assert_eq!(
        image::open(&png).unwrap().to_rgba8().dimensions(),
        (880, 1760)
    );

    let (out, text) = run(&[
        "shot",
        example_dir().to_str().unwrap(),
        "--surface",
        "nope",
        "-o",
        png.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("no surface named `nope`"), "{text}");
    assert!(
        text.contains("`counter`") && text.contains("`inspector`"),
        "{text}"
    );
}

#[test]
fn the_dump_starts_each_window_with_its_own_line() {
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
    let lines: Vec<&str> = dump.lines().collect();
    assert_eq!(lines[0], "Window \"Counter\"", "{dump}");
    assert!(
        lines[1].starts_with("  "),
        "the tree is indented below: {dump}"
    );
    assert!(lines.contains(&"Window \"Inspector\""), "{dump}");
}

#[test]
fn a_program_with_one_window_is_drawn_whole() {
    let dir = scratch("one");
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"one\"\nentry = \"main.byd\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.byd"),
        "View Main() {\n    Window #[title: \"Only\", width: 200, height: 100] {\n        \
         Box #[bg: 0xE8543F, width: 50, height: 50] {}\n    }\n}\n",
    )
    .unwrap();
    let png = dir.join("out.png");
    let (out, text) = run(&[
        "shot",
        dir.to_str().unwrap(),
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
    let img = image::open(&png).expect("a PNG").to_rgb8();
    assert_eq!(img.dimensions(), (400, 200));
    let px = img.get_pixel(50, 50);
    assert!(px.0[0] > 200 && px.0[2] < 100, "the box is drawn: {px:?}");
}

#[test]
fn surface_is_for_a_program_that_has_windows() {
    let dir = scratch("none");
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"none\"\nentry = \"main.byd\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("main.byd"), "View Main() { Text(\"x\") }\n").unwrap();
    let (out, text) = run(&[
        "shot",
        dir.to_str().unwrap(),
        "--surface",
        "a",
        "-o",
        dir.join("x.png").to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("writes no `Window`"), "{text}");
}

#[test]
fn the_scenarios_tap_in_one_window_and_read_the_other() {
    let (out, text) = run(&["test", example_dir().to_str().unwrap()]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("4 passed; 0 failed"), "{text}");
}
