//! `byard check` inside a library package (`[package]`, no `[project]`):
//! the package's own files are checked as a consumer would get them, with no
//! entry to require. The commands that open an app say why they cannot.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn example(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(rel)
}

/// The two library packages the examples ship check clean where they are,
/// `brand` with its own fonts named as its consumers name them.
#[test]
fn the_example_packages_check_clean_in_place() {
    for package in ["package_theme/brand", "workspace/kit"] {
        let out = byard(&example(package), &["check"]);
        let said = text(&out);
        assert!(out.status.success(), "{package}: {said}");
        assert!(said.contains("checking package"), "{package}: {said}");
        assert!(said.contains("0 errors"), "{package}: {said}");
    }
}

/// A mistake in a package is reported against the package's own file.
#[test]
fn an_error_in_a_package_names_its_file() {
    let dir = std::env::temp_dir().join(format!("byard-libcheck-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("byard.toml"),
        "[package]\nname = \"gauges\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/ok.byd"), "View Dial() { Text(\"dial\") }\n").unwrap();
    std::fs::write(
        dir.join("src/broken.byd"),
        "View Needle() {\n    var angle = 0\n    Text(\"{angle}\") #[colour: 0xFF0000]\n}\n",
    )
    .unwrap();
    let out = byard(&dir, &["check"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("src/broken.byd"), "named by its file: {said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library has nothing to open: the commands that run an app say so, and
/// point at the one that works.
#[test]
fn running_a_library_says_what_it_is() {
    let brand = example("package_theme/brand");
    for args in [&["shot", "-o", "/dev/null"][..], &["dev"][..]] {
        let out = byard(&brand, args);
        let said = text(&out);
        assert!(!out.status.success(), "{args:?}: {said}");
        assert!(said.contains("is a library package"), "{args:?}: {said}");
        assert!(
            said.contains("`byard check` works here"),
            "{args:?}: {said}"
        );
    }
}
