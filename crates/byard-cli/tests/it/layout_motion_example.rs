//! Guards the committed layout motion example (RFC-0061) through the real
//! `byard` binary: it checks clean, and a held press and a tap draw the states
//! its header says.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/layout_motion")
}

fn run(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .args(args)
        .arg(example_dir())
        .output()
        .expect("run byard");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn the_example_checks_clean() {
    let (ok, text) = run(&["check"]);
    assert!(ok && text.contains("0 errors"), "{text}");
}

#[test]
fn the_pictures_in_the_header_draw() {
    let dir = std::env::temp_dir().join(format!("byard-layout-motion-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut bytes = Vec::new();
    for (name, extra) in [
        ("rest", vec![]),
        ("hold", vec!["--hold", "164,60", "--at", "1000"]),
        ("chip", vec!["--tap", "60,140", "--at", "1000"]),
    ] {
        let out = dir.join(format!("{name}.png"));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_byard-cli"));
        cmd.arg("shot")
            .arg(example_dir())
            .args(&extra)
            .arg("-o")
            .arg(&out);
        let run = cmd.output().unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        bytes.push(std::fs::read(&out).unwrap());
    }
    assert_ne!(bytes[0], bytes[1], "the held press widens the button");
    assert_ne!(bytes[0], bytes[2], "the tap opens the check slot");
}
