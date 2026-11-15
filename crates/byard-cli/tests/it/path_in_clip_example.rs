//! A path inside a `Clip`: the `path_in_clip` example checks clean and draws
//! (the drawing itself is read back in `byard-platform`'s `canvas_fill_readback`).

use std::path::PathBuf;
use std::process::Command;

#[test]
fn path_in_clip_example_checks_clean() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/path_in_clip");
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(dir)
        .output()
        .expect("run `byard check`");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("0 errors"), "{text}");
}
