//! Guards the committed RFC-0006 §3.4/§3.5 example through the real `byard`
//! binary: the `error_overlay` project must check clean.
//!
//! The example's whole job is to be broken *on purpose, by hand*, so the one
//! thing a test can assert about it is that its committed state is the good
//! one. An example that ships broken teaches nothing and fails every other
//! example's guard by association.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/error_overlay")
}

#[test]
fn error_overlay_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("ok") && stdout.contains("0 errors"),
        "the committed example must be the *good* state:\n{stdout}"
    );
}

/// The scene has to be worth blurring.
///
/// A single flat-coloured box would prove nothing: it looks the same blurred
/// and unblurred, so a reader checking "is my view still behind this?" would
/// get no answer either way. The example needs several distinct colours and
/// some real text, and if someone simplifies it into a placeholder this fails
/// rather than quietly making the demonstration meaningless.
#[test]
fn the_scene_is_varied_enough_to_tell_blurred_from_absent() {
    let source = std::fs::read_to_string(example_dir().join("src/main.byd")).expect("example");
    let body = source
        .lines()
        .skip_while(|l| !l.starts_with("View "))
        .collect::<Vec<_>>()
        .join("\n");
    let swatches = ["0x6750A4", "0x386A20", "0xB3261E", "0xE8B931"];
    for colour in swatches {
        assert!(
            body.contains(colour),
            "the scene lost a colour swatch ({colour}); a flat field looks the \
             same blurred and absent"
        );
    }
    assert!(
        body.matches("Text(").count() >= 4,
        "the scene needs real text, so a reader can check they *cannot* read it \
         through the backdrop"
    );
}

/// The header is the example's documentation, and the only place the two
/// promises being demonstrated are written down alongside how to trigger them.
#[test]
fn the_header_explains_both_promises_and_how_to_break_the_file() {
    let source = std::fs::read_to_string(example_dir().join("src/main.byd")).expect("example");
    for phrase in [
        // The blurred backdrop, and the census that proves it cheaply.
        "blurred",
        "boxes",
        // The gate, and its indicator.
        "pending",
        "hold",
        // The caret diagnostics and the script-shaped escape hatch.
        "--short",
        "problem matcher",
    ] {
        assert!(
            source.contains(phrase),
            "the example header no longer mentions {phrase:?}, so a reader has \
             no way to know what to look at"
        );
    }
}
