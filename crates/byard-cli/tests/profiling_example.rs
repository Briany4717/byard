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
        stdout.contains("ok (0 errors)"),
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
    ] {
        assert!(
            source.contains(scope),
            "the example must document {scope} — it is one of the six scopes it exists to drive"
        );
    }
}
