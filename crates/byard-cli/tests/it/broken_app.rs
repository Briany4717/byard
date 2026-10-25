//! RFC-0043: an app whose views do not compile says so the way `byard check`
//! does, not as a platform error with the headline squeezed into it.
//!
//! The fixture is a small project with two misspelt names in two files. It is
//! run through [`byard::Shot`], which loads the project the way
//! [`byard::App::run`] does and fails at the same place, before any GPU is
//! asked for, so the test needs no window and no device.
//!
//! What a person sees from `cargo run` is stderr, so stderr is what is
//! compared: the test starts itself again as a child (`--nocapture`, no
//! colour), has the child do what a `fn main() -> Result<(), ByardError>`
//! does with the error, and holds the captured text against the committed
//! snapshot `tests/fixtures/broken_app.stderr`. To read the snapshot is to
//! read the output. After a change that is meant to alter it, run the test
//! with `BYARD_BLESS=1` and review the diff.

use std::path::PathBuf;
use std::process::Command;

use byard::{App, ByardError, ByardErrorExt as _, Shot};

/// Set in the child process, which is the half that runs the app.
const CHILD: &str = "BYARD_BROKEN_APP_CHILD";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn shot_the_broken_app() -> ByardError {
    let png = std::env::temp_dir().join(format!("byard-broken-app-{}.png", std::process::id()));
    let result = App::new(fixtures().join("broken_app"))
        .size(240, 160)
        .shot(&png, &Shot::after(0));
    let _ = std::fs::remove_file(&png);
    result.expect_err("a project that does not compile must not draw")
}

/// The child half. In a normal run there is nothing to do; started by
/// [`an_app_that_does_not_compile_prints_what_check_prints`] it behaves like
/// the `main` of an app that returns the error: the runtime prints `Error: `
/// and the error's `Debug`.
#[test]
fn child_shot_of_the_broken_app() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let error = shot_the_broken_app();
    eprintln!("Error: {error:?}");
}

/// What the child wrote to stderr.
fn the_apps_stderr() -> String {
    let out = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "broken_app::child_shot_of_the_broken_app",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("start the child test process");
    assert!(
        out.status.success(),
        "the child failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stderr).expect("stderr is text")
}

#[test]
fn an_app_that_does_not_compile_prints_what_check_prints() {
    let stderr = the_apps_stderr();

    let snapshot = fixtures().join("broken_app.stderr");
    if std::env::var_os("BYARD_BLESS").is_some() {
        std::fs::write(&snapshot, &stderr).expect("write the snapshot");
    }
    // A Windows checkout with `core.autocrlf` gives the snapshot CRLF
    // endings; what the app prints has LF ones on every platform.
    let expected = std::fs::read_to_string(&snapshot)
        .expect("the snapshot is committed (BYARD_BLESS=1 writes it)")
        .replace("\r\n", "\n");
    assert_eq!(
        stderr, expected,
        "stderr differs from tests/fixtures/broken_app.stderr"
    );

    // One renderer: every line `byard check` prints for the project is in the
    // app's output, in the same order, and nothing is repeated.
    let check = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(fixtures().join("broken_app"))
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run `byard check`");
    let check_stderr = String::from_utf8_lossy(&check.stderr);
    let diagnostics: Vec<&str> = check_stderr
        .lines()
        .take_while(|l| !l.starts_with("err "))
        .collect();
    let app_lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        &app_lines[..diagnostics.len()],
        &diagnostics[..],
        "the app and `byard check` disagree about what the errors look like"
    );
    assert_eq!(
        &app_lines[diagnostics.len()..],
        ["Error: 2 errors in the project's views (shown above)"],
        "after the diagnostics comes one line, not the diagnostics again"
    );
}

/// The error is a value an app can tell apart from the platform's.
#[test]
fn the_error_is_compile_and_holds_each_diagnostic_with_its_file() {
    let error = shot_the_broken_app();
    assert!(
        !matches!(error, ByardError::Platform(_)),
        "a broken view is not a broken GPU: {error:?}"
    );
    let report = error.as_compile().expect("a Compile error");
    assert_eq!(report.error_count(), 2);
    let files: Vec<String> = report
        .diagnostics()
        .iter()
        .map(|d| {
            let (entry, _) = report
                .source_map()
                .locate(d.span())
                .expect("a located span");
            entry.file.clone()
        })
        .collect();
    assert_eq!(files, ["main.byd", "forecast.byd"]);
    let text = report.render();
    assert!(text.contains("main.byd:4:29"), "{text}");
    assert!(text.contains("did you mean `count`?"), "{text}");
    assert!(
        text.contains("    Box #[bg: prmary, p: 8] {\n  |               ^^^^^^"),
        "{text}"
    );
}
