//! `{ drag = [from, to] }` in `byard test` and `--drag X,Y:X,Y` in `byard shot`:
//! a press, several moves and a release in one step, through the real binary.

use std::path::PathBuf;
use std::process::Command;

fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-drag-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"t\"\nentry = \"main.byd\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.byd"),
        "View Main() {\n    var moves = 0\n    var ends = 0\n    Column {\n        \
         Box #[width: 200, height: 200, bg: 0x336699, drag(e) => moves = moves + 1, \
         drag_end(e) => ends = ends + 1] {}\n        Text(\"moves {moves} ends {ends}\")\n    }\n}\n",
    )
    .unwrap();
    dir
}

fn run(dir: &PathBuf, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .args(args)
        .current_dir(dir)
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
fn a_drag_step_presses_moves_and_releases() {
    let dir = project("test");
    std::fs::write(
        dir.join("tests/drag.toml"),
        "[[test]]\nname = \"a drag moves and ends\"\nsteps = [\n  { drag = [\"20,20\", \"20,150\"] },\n  \
         { expect = \"moves 8 ends 1\" },\n]\n\n[[test]]\nname = \"a drag by lists\"\nsteps = [\n  \
         { drag = [[20, 20], [150, 20]] },\n]\nexpect = \"ends 1\"\n",
    )
    .unwrap();
    let (ok, text) = run(&dir, &["test"]);
    assert!(ok, "{text}");
    assert!(
        text.contains("2 passed") || text.contains("passed"),
        "{text}"
    );
}

#[test]
fn a_drag_step_needs_two_points() {
    let dir = project("bad");
    std::fs::write(
        dir.join("tests/drag.toml"),
        "[[test]]\nname = \"one point\"\nsteps = [{ drag = [\"20,20\"] }]\n",
    )
    .unwrap();
    let (ok, text) = run(&dir, &["test"]);
    assert!(!ok);
    assert!(text.contains("`drag` takes two points"), "{text}");
}

#[test]
fn shot_drag_reaches_the_tree() {
    let dir = project("shot");
    let tree = dir.join("tree.txt");
    let (ok, text) = run(
        &dir,
        &[
            "shot",
            "--drag",
            "20,20:20,150",
            "--a11y",
            tree.to_str().unwrap(),
        ],
    );
    assert!(ok, "{text}");
    let tree = std::fs::read_to_string(tree).unwrap();
    assert!(tree.contains("moves 8 ends 1"), "{tree}");
}
