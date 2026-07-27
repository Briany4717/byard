//! RFC-0006 **C3** and **C7**, through the real `byard` binary.
//!
//! The two claims that matter here are about *bytes on stderr*, and neither can
//! be checked from inside the crate: they are about what a script and an
//! editor's problem matcher see. So this drives the binary.
//!
//! - The machine-readable first line — `file:line:col: error[kind]: message` —
//!   is unchanged and unstyled. Editor integrations parse it, and breaking one
//!   makes it go *quiet* rather than fail, which nobody notices for a long time.
//! - `--short` produces exactly that and nothing else, so a script that grew up
//!   on the pre-caret output keeps working byte for byte.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A project whose only file has one unknown attribute, written to a temp dir
/// so the test owns it and no committed example has to stay broken.
fn broken_project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "byard-diag-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp project");
    std::fs::write(
        dir.join("byard.toml"),
        "[project]\nname = \"diag\"\nentry = \"src/main.byd\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("src/main.byd"),
        "View Main() {\n    Column #[gap: 8] {\n        Box #[colour: 0xFF0000] {}\n    }\n}\n",
    )
    .expect("source");
    dir
}

fn check(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .args(args)
        .arg(dir)
        // Forced on so the test proves the *first line* stays unstyled even
        // when everything around it is coloured — the interesting case, and the
        // one a piped CI run would never exercise.
        .env("CLICOLOR_FORCE", "1")
        .output()
        .expect("run `byard check`");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// The diagnostic lines on stderr — the ones an editor parses — with the
/// caret block, if any, left in place.
fn stderr_lines(stderr: &str) -> Vec<String> {
    stderr.lines().map(str::to_string).collect()
}

#[test]
fn the_first_line_is_rustc_shaped_and_carries_no_escapes() {
    let dir = broken_project();
    let (_, stderr, ok) = check(&dir, &[]);
    assert!(!ok, "a project with an unknown attribute must fail");

    let first = stderr_lines(&stderr)
        .into_iter()
        .find(|l| l.contains("error["))
        .expect("a machine-readable diagnostic line");
    assert!(
        !first.contains('\u{1b}'),
        "the line editors parse must never be styled, even with CLICOLOR_FORCE=1: {first:?}"
    );
    assert!(
        first.starts_with("main.byd:3:15: error[UnknownAttribute]:"),
        "file:line:col: error[kind]: message is not negotiable: {first:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn short_output_is_the_first_line_and_nothing_else() {
    // A script that grew up on the pre-caret output must keep working byte for
    // byte. That is what `--short` is for, and asserting "contains" instead of
    // equality would let a stray line through.
    let dir = broken_project();
    let (_, verbose, _) = check(&dir, &[]);
    let (_, short, ok) = check(&dir, &["--short"]);
    assert!(!ok);

    let short_diags: Vec<String> = stderr_lines(&short);
    assert_eq!(
        short_diags.len(),
        1,
        "`--short` must print one line per error and nothing else: {short_diags:?}"
    );
    assert!(!short_diags[0].contains('\u{1b}'));

    let verbose_first = stderr_lines(&verbose)
        .into_iter()
        .find(|l| l.contains("error["))
        .expect("diagnostic");
    assert_eq!(
        short_diags[0], verbose_first,
        "the short form and the verbose form's first line must be the same bytes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_caret_block_appears_beneath_the_line_and_points_at_the_span() {
    let dir = broken_project();
    let (_, stderr, _) = check(&dir, &[]);
    let lines = stderr_lines(&stderr);
    let at = lines
        .iter()
        .position(|l| l.contains("error["))
        .expect("diagnostic");
    assert!(
        lines.len() >= at + 4,
        "expected a three-line caret block beneath the diagnostic:\n{stderr}"
    );

    let strip = |s: &str| -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    };

    let gutter = strip(&lines[at + 1]);
    let source = strip(&lines[at + 2]);
    let carets = strip(&lines[at + 3]);
    assert_eq!(gutter.trim_end(), "  |", "{gutter:?}");
    assert!(source.starts_with("3 | "), "{source:?}");
    assert!(source.contains("colour"), "{source:?}");
    assert!(carets.starts_with("  | "), "{carets:?}");

    // The first `^` must sit directly under the `c` of `colour`. Both lines
    // carry the same 4-column gutter, so the offsets are directly comparable.
    let caret_col = carets.find('^').expect("a caret");
    let span_col = source.find("colour").expect("the attribute");
    assert_eq!(
        caret_col, span_col,
        "the caret must line up with what it points at:\n{source}\n{carets}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clean_project_prints_no_diagnostics_on_stderr_at_all() {
    // The other half of C7's contract: silence on success, so a CI job's
    // stderr is a reliable signal rather than a place to look.
    let ok_dir = std::env::temp_dir().join(format!("byard-diag-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ok_dir);
    std::fs::create_dir_all(ok_dir.join("src")).expect("temp project");
    std::fs::write(
        ok_dir.join("byard.toml"),
        "[project]\nname = \"ok\"\nentry = \"src/main.byd\"\n",
    )
    .expect("manifest");
    std::fs::write(
        ok_dir.join("src/main.byd"),
        "View Main() {\n    Column #[gap: 8] { Text(\"hi\") }\n}\n",
    )
    .expect("source");

    let (stdout, stderr, ok) = check(&ok_dir, &[]);
    assert!(ok, "{stdout}{stderr}");
    assert!(
        stderr.is_empty(),
        "a clean check must say nothing on stderr: {stderr:?}"
    );
    assert!(stdout.contains("0 errors"), "{stdout}");
    let _ = std::fs::remove_dir_all(&ok_dir);
}
