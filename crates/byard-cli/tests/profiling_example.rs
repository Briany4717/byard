//! Guards the committed RFC-0030 §I1 example through the real `byard` binary:
//! the `profiling` project — the driver for the terminal telemetry block, with
//! toggles that move one scope at a time — must check clean (parse,
//! type-check, lower, validate every intrinsic with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/profiling")
}

/// `byard check <project-dir>` on the profiling example reports no errors.
#[test]
fn profiling_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("0 errors"),
        "expected a clean check, got:\n{stdout}"
    );
}

/// The example's header is its documentation: it tells a reader which control
/// moves which scope, and it is the only place the expected output shape is
/// written down. If a scope is ever renamed, this fails and the header gets
/// renamed with it rather than quietly describing a profiler that no longer
/// exists.
#[test]
fn the_example_header_names_every_instrumented_scope() {
    let source = std::fs::read_to_string(example_dir().join("src/main.byd")).expect("example");
    for scope in [
        "interp.dispatch_events",
        "interp.tick",
        "interp.render",
        "layout.taffy",
        "encode.frame",
        "relay.publish",
        // Added by the self-accounting erratum: `encode.frame`'s breakdown had
        // two of its three largest terms in no sub-scope at all, so they showed
        // up only as self-time nothing explained.
        "encode.scissor",
        "encode.bookkeeping",
        "encode.finish",
    ] {
        assert!(
            source.contains(scope),
            "the example must document {scope} — it is one of the scopes it exists to drive"
        );
    }
}

/// The two findings a developer can only confirm by *running* this, and which
/// no unit test can reach: that opening the HUD does not move the app's rows,
/// and that a re-shape count is printed rather than left to be timed.
///
/// Pinned here because the header is the only place the procedure is written
/// down, and a procedure nobody can find is a procedure nobody follows.
#[test]
fn the_header_tells_a_reader_how_to_check_the_hud_pays_for_itself() {
    let source = std::fs::read_to_string(example_dir().join("src/main.byd")).expect("example");
    for phrase in [
        // §A1: the attribution, checkable from the block itself.
        "dev, all threads",
        "no longer read `×2`",
        // §A2: the re-shape count, read rather than inferred.
        "re-shaped",
        // §A4: a missed vsync is not an over-budget frame.
        "waited",
        // The residual, named rather than hidden.
        "encode.finish",
    ] {
        assert!(
            source.contains(phrase),
            "the header must tell a reader to look for {phrase:?} — it is one \
             of the things only a real session can confirm"
        );
    }
}
