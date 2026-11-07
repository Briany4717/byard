//! Guards the committed RFC-0008 example workspace end-to-end through the real
//! `byard` binary: a project that `use`s a `path` package and a sibling file
//! must resolve into one module graph and check clean.

use std::path::PathBuf;
use std::process::Command;

fn example(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/workspace")
        .join(sub)
}

/// `byard check <project-dir>` resolves the whole graph (project files +
/// the `kit` path package) and reports no errors.
#[test]
fn workspace_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(example("app"))
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("1 package(s)") && stdout.contains("0 errors"),
        "expected a 1-package clean check, got:\n{stdout}"
    );
}

/// Pointing at the manifest resolves the same way as pointing at the directory.
#[test]
fn checking_the_manifest_path_is_equivalent() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(example("app/byard.toml"))
        .output()
        .expect("run `byard check`");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 errors"));
}

// ── RFC-0055: constants, enums, private names and `else when` ───────────────

/// Copies the example workspace to a directory of its own, for a test that
/// changes a file of it.
fn copy_of(tag: &str) -> PathBuf {
    fn copy(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("byard-workspace-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&example(""), &dir);
    dir
}

/// What `byard check` says of the app of `dir`, and whether it passed.
fn check(dir: &std::path::Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(dir.join("app"))
        .output()
        .expect("run `byard check`");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The app draws what the package's constant, enum and `else when` say: the
/// accessibility tree needs no GPU and names every text.
#[test]
fn the_example_draws_its_variants_and_each_arm_of_the_chain() {
    let tree =
        std::env::temp_dir().join(format!("byard-workspace-a11y-{}.txt", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("shot")
        .arg(example("app"))
        .arg("--a11y")
        .arg(&tree)
        .output()
        .expect("run `byard shot`");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&tree).unwrap();
    let _ = std::fs::remove_file(&tree);
    for wanted in [
        "Save",
        "Cancel",
        "More",
        "no messages",
        "one message",
        "4 messages",
        "more than 9 messages",
    ] {
        assert!(text.contains(wanted), "`{wanted}` in:\n{text}");
    }
}

#[test]
fn the_apps_misuse_of_the_package_is_told_in_words() {
    let dir = copy_of("misuse");
    let main = dir.join("app/src/main.byd");
    let write = |body: &str| {
        std::fs::write(
            &main,
            format!("use kit as k\n\nView Main() {{\n{body}\n}}\n"),
        )
        .unwrap();
    };

    write("    k.Caption(\"x\")");
    let (ok, said) = check(&dir);
    assert!(!ok, "{said}");
    assert!(said.contains("error[PrivateName]"), "{said}");
    assert!(
        said.contains("`Caption` is private to package `kit`"),
        "{said}"
    );

    write("    Text(\"{k.fill(k.Variant.tonal)}\")");
    let (_, said) = check(&dir);
    assert!(
        said.contains("`fill` is private to package `kit`"),
        "{said}"
    );

    write("    k.Button(\"a\", variant: .tonl)");
    let (ok, said) = check(&dir);
    assert!(!ok, "{said}");
    assert!(said.contains("error[UnknownEnumCase]"), "{said}");
    assert!(said.contains("did you mean `tonal`"), "{said}");

    write("    k.Button(\"a\", variant: \"tonal\")");
    let (ok, said) = check(&dir);
    assert!(!ok, "{said}");
    assert!(said.contains("error[ArgumentTypeMismatch]"), "{said}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A broken arm of an `else when` chain that is not the one showing is found
/// by `byard check`, as a broken `when` arm is.
#[test]
fn every_arm_of_an_else_when_chain_is_checked_by_the_command() {
    let dir = copy_of("arms");
    std::fs::write(
        dir.join("app/src/widgets.byd"),
        "View Inbox(count: Int) {\n    when count == 0 {\n        Text(\"none\")\n    } else when count == 1 {\n        Text(\"one\")\n    } else when count == 2 {\n        Bogus()\n    } else {\n        Text(\"many\")\n    }\n}\n",
    )
    .unwrap();
    let (ok, said) = check(&dir);
    assert!(!ok, "{said}");
    assert!(said.contains("Bogus"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The package checks clean on its own, private names included.
#[test]
fn the_package_alone_checks_clean_with_its_private_names() {
    let dir = copy_of("kit");
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(dir.join("kit"))
        .output()
        .expect("run `byard check`");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
