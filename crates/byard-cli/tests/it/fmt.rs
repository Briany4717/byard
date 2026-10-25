//! `byard fmt` (RFC-0044): formats in place, `--check` lists and fails, a file
//! it cannot format is reported and left alone.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MESSY: &str =
    "View Main(){\nvar n=0   // taps\nColumn #[gap:8,p:(horizontal:24)]{\nText(\"{n}\")\n}\n}\n";
const TIDY: &str = "View Main() {\n    var n = 0 // taps\n    Column #[gap: 8, p: (horizontal: 24)] {\n        Text(\"{n}\")\n    }\n}\n";

fn byard(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_byard"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run byard")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byard-fmt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// RFC-0044 Acceptance: `--check` exits 1 on an unformatted fixture, says
/// which file, changes nothing; `byard fmt` formats it; `--check` then exits
/// 0.
#[test]
fn check_fails_on_an_unformatted_file_and_passes_after_fmt() {
    let dir = scratch("check");
    write(&dir.join("main.byd"), MESSY);

    let out = byard(&dir, &["fmt", "--check", "main.byd"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "main.byd");
    assert_eq!(
        std::fs::read_to_string(dir.join("main.byd")).unwrap(),
        MESSY
    );

    let out = byard(&dir, &["fmt", "main.byd"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(dir.join("main.byd")).unwrap(), TIDY);

    let out = byard(&dir, &["fmt", "--check", "main.byd"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 file formatted"),
        "no file is listed when all is formatted: {}",
        text(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// With no path, it formats the project it is in: the entry's directory.
#[test]
fn with_no_path_it_formats_the_project() {
    let dir = scratch("project");
    write(
        &dir.join("byard.toml"),
        "[project]\nname = \"p\"\nentry = \"src/main.byd\"\n",
    );
    write(&dir.join("src/main.byd"), MESSY);
    write(&dir.join("src/more/card.byd"), MESSY);
    write(&dir.join("src/.hidden/skip.byd"), MESSY);
    write(&dir.join(".byard/cache/skip.byd"), MESSY);
    write(&dir.join("elsewhere/outside.byd"), MESSY);

    let out = byard(&dir, &["fmt"]);
    assert!(out.status.success(), "{}", text(&out));
    for formatted in ["src/main.byd", "src/more/card.byd"] {
        assert_eq!(std::fs::read_to_string(dir.join(formatted)).unwrap(), TIDY);
    }
    for untouched in [
        "src/.hidden/skip.byd",
        ".byard/cache/skip.byd",
        "elsewhere/outside.byd",
    ] {
        assert_eq!(
            std::fs::read_to_string(dir.join(untouched)).unwrap(),
            MESSY,
            "{untouched} is not the project's"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A path dependency is the project's own code, wherever it sits; a package
/// in the cache is its author's and is not touched (RFC-0044 Q1).
#[test]
fn path_dependencies_are_formatted_and_fetched_packages_are_not() {
    let dir = scratch("deps");
    write(
        &dir.join("app/byard.toml"),
        "[project]\nname = \"app\"\nentry = \"src/main.byd\"\n\n[dependencies]\nkit = { path = \"../kit\" }\n",
    );
    write(&dir.join("app/src/main.byd"), MESSY);
    write(&dir.join("kit/byard.toml"), "[package]\nname = \"kit\"\n");
    write(&dir.join("kit/src/chip.byd"), MESSY);
    write(&dir.join("kit/target/junk.byd"), MESSY);

    let out = byard(&dir.join("app"), &["fmt"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("app/src/main.byd")).unwrap(),
        TIDY
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("kit/src/chip.byd")).unwrap(),
        TIDY
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("kit/target/junk.byd")).unwrap(),
        MESSY
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory that is not a project is walked for every `.byd` in it.
#[test]
fn a_directory_that_is_not_a_project_is_walked() {
    let dir = scratch("walk");
    write(&dir.join("a/one.byd"), MESSY);
    write(&dir.join("a/b/two.byd"), MESSY);
    write(&dir.join("a/notes.txt"), "not byld");
    let out = byard(&dir, &["fmt", "--check", "a"]);
    assert_eq!(out.status.code(), Some(1));
    let listed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(listed.lines().count(), 2, "{listed}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that does not parse is named with its line and left as it was;
/// the others are still formatted, and the exit is 1.
#[test]
fn a_file_that_does_not_parse_is_reported_and_left_alone() {
    let dir = scratch("broken");
    let broken = "View Main() {\n    Text(\n}\n";
    write(&dir.join("a.byd"), broken);
    write(&dir.join("b.byd"), MESSY);

    let out = byard(&dir, &["fmt", "a.byd", "b.byd"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("a.byd:"), "{said}");
    assert!(!said.contains("b.byd:"), "{said}");
    assert_eq!(std::fs::read_to_string(dir.join("a.byd")).unwrap(), broken);
    assert_eq!(std::fs::read_to_string(dir.join("b.byd")).unwrap(), TIDY);
    let _ = std::fs::remove_dir_all(&dir);
}

/// CRLF files stay CRLF.
#[test]
fn line_endings_are_kept() {
    let dir = scratch("crlf");
    write(&dir.join("main.byd"), &MESSY.replace('\n', "\r\n"));
    assert!(byard(&dir, &["fmt", "main.byd"]).status.success());
    assert_eq!(
        std::fs::read_to_string(dir.join("main.byd")).unwrap(),
        TIDY.replace('\n', "\r\n")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_that_does_not_exist_is_an_error() {
    let dir = scratch("missing");
    let out = byard(&dir, &["fmt", "nope.byd"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("nope.byd"), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0044 Acceptance, the check a person can do: the examples the
/// repository ships are formatted.
#[test]
fn the_shipped_examples_are_formatted() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let out = byard(&examples, &["fmt", "--check", "."]);
    assert!(out.status.success(), "{}", text(&out));
}
