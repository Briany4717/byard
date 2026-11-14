//! Guards the committed context and optional values example (RFC-0065)
//! through the real `byard` binary: it checks clean, and a tap on a tab, drawn
//! through the slot of `Tabs`, reaches the provider and moves the selection.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/context_and_optional")
}

fn dump(tag: &str, args: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("byard-context-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("tree.txt");
    let run = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example_dir())
        .args(args)
        .arg("--a11y")
        .arg(&out)
        .arg("-o")
        .arg(dir.join("shot.png"))
        .output()
        .expect("run `byard shot`");
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read_to_string(&out).expect("the tree was written")
}

#[test]
fn the_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "got:\n{stdout}");
}

#[test]
fn a_group_that_binds_nothing_keeps_its_own_count() {
    let rest = dump("rest", &[]);
    assert!(rest.contains("Taps: 0"), "{rest}");
    let tapped = dump("tap", &["--tap", "100,172"]);
    assert!(tapped.contains("Taps: 1"), "{tapped}");
}
