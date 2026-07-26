//! Guards the committed RFC-0032 example through the real `byard` binary: the
//! `incremental` project — twelve paragraphs that never change under one thing
//! that changes every frame — must check clean (parse, type-check, lower,
//! validate every intrinsic with no diagnostics).
//!
//! Every other example in this directory has had such a test since it landed;
//! this one shipped without one, which meant the scene the retained layout path
//! is demonstrated on was the only scene in the repository a grammar change
//! could break silently.

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/incremental")
}

/// `byard check <project-dir>` on the incremental example reports no errors.
#[test]
fn incremental_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    // The prefix and the count, not the sentence between them: RFC-0030 §P4
    // owns the line's shape, and a test that pins the exact rendering makes
    // every future grammar change look like a regression in this example.
    assert!(
        stdout.contains("ok") && stdout.contains("0 errors"),
        "expected a clean check, got:\n{stdout}"
    );
}

/// The example demonstrates the retained path by holding a **paint-class**
/// animation over content that never changes: the spinner's `rotate` must never
/// mark layout dirty, which is the whole reason the scene stays retained frame
/// after frame.
///
/// If someone reclassifies `rotate` as layout-class, RFC-0032 §Q8's diagnostic
/// makes this example stop compiling and `incremental_example_checks_clean`
/// above fails — which is the intended outcome, and worth stating here so the
/// next reader knows the two tests are one argument.
#[test]
fn the_example_animates_only_paint_class_attributes() {
    let source = std::fs::read_to_string(example_dir().join("src/main.byd")).expect("example");
    let animated: Vec<&str> = source
        .lines()
        .filter(|l| l.contains(" with anim."))
        .collect();
    assert!(
        !animated.is_empty(),
        "the example must animate something — it is how the scene proves a \
         frame can change without any layout work"
    );
    for line in animated {
        assert!(
            line.contains("rotate:") || line.contains("opacity:") || line.contains("scale:"),
            "the example animates a non-paint attribute, which would relayout \
             every frame and make the scene demonstrate the opposite of what \
             it claims (RFC-0010 INV-8): {line}"
        );
    }
}

/// The example's header is its documentation, and unusually for this repository
/// it is also the acceptance criteria a human runs by hand: seven numbered
/// checks, four in the terminal and three on screen. Two of them name the
/// hazards that are invisible in a screenshot — a paragraph that silently
/// un-wraps, and an element that answers taps where it used to be — so the
/// header going stale is the difference between a verifiable example and a
/// decorative one.
#[test]
fn the_example_header_documents_what_a_reader_must_verify() {
    let source = std::fs::read_to_string(example_dir().join("src/main.byd")).expect("example");
    for claim in [
        "atlas  retained", // the path counter line in the terminal
        "layout.taffy",    // the scope that goes to ~0
        "encode.glyphs",   // the scope the win actually comes from
        "present.acquire", // the frame finishing early rather than late
        "STILL WRAPPED",   // RFC-0032 §R5, the most likely visible bug
        "STILL LANDS",     // INV-23, the hazard no screenshot can show
        "STILL REFLOWS",   // text content is layout-class
    ] {
        assert!(
            source.contains(claim),
            "the example's header must still tell a reader to verify {claim:?}"
        );
    }
}
