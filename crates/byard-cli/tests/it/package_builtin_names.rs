//! A package may declare a view named like a built-in (`Button`), reached
//! qualified (`m.Button`) from outside and as `pkg.Button` from inside; the
//! bare name stays the built-in everywhere. An app has no namespace, so there
//! it stays an error.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn byard(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_byard-cli"))
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
        .join("examples/builtin_names")
        .join(rel)
}

/// A scratch project with a `ds` package beside it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str, package_src: &str, app_src: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("byard-builtin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ds/src")).unwrap();
        std::fs::create_dir_all(dir.join("app")).unwrap();
        std::fs::write(
            dir.join("ds/byard.toml"),
            "[package]\nname = \"ds\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("ds/src/lib.byd"), package_src).unwrap();
        std::fs::write(
            dir.join("app/byard.toml"),
            "[project]\nname = \"app\"\nentry = \"main.byd\"\n\n[dependencies]\nds = { path = \"../ds\" }\n",
        )
        .unwrap();
        std::fs::write(dir.join("app/main.byd"), app_src).unwrap();
        Self(dir)
    }
    fn ds(&self) -> PathBuf {
        self.0.join("ds")
    }
    fn app(&self) -> PathBuf {
        self.0.join("app")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const WRAPPER: &str = "View Button(label: Str) {\n    Button(label)\n}\n";

/// The package wraps the built-in, checks clean alone, and an app uses both.
#[test]
fn a_package_declares_a_view_named_like_a_builtin() {
    let out = byard(&example("ds"), &["check"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("0 errors"), "{said}");

    let out = byard(&example("app"), &["check"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
}

/// The qualified view is the package's and the bare one the built-in, in the
/// same file.
#[test]
fn qualified_is_the_package_and_bare_is_the_builtin() {
    let dir = Scratch::new(
        "both",
        WRAPPER,
        "use ds as m\nView Main() {\n    Column {\n        m.Button(\"a\")\n        Button(\"b\")\n    }\n}\n",
    );
    let a11y = dir.0.join("tree.txt");
    let out = byard(
        &dir.app(),
        &[
            "shot",
            "-o",
            dir.0.join("o.png").to_str().unwrap(),
            "--a11y",
            a11y.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let tree = std::fs::read_to_string(&a11y).unwrap();
    assert_eq!(tree.matches("Button").count(), 2, "{tree}");
}

/// `use ds.{Button}` would hide the built-in in that file.
#[test]
fn importing_the_name_asks_for_the_qualified_one() {
    let dir = Scratch::new(
        "use",
        WRAPPER,
        "use ds.{Button}\nView Main() { Button(\"a\") }\n",
    );
    let out = byard(&dir.app(), &["check"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("ImportShadowsBuiltin"), "{said}");
    assert!(said.contains("ds.Button"), "{said}");
}

/// An app has no namespace to keep the two apart.
#[test]
fn an_app_declaring_the_name_is_still_an_error() {
    let dir = Scratch::new("app", WRAPPER, "View Main() { Text(\"x\") }\n");
    std::fs::write(
        dir.app().join("main.byd"),
        format!("View Main() {{ Text(\"x\") }}\n{WRAPPER}"),
    )
    .unwrap();
    let out = byard(&dir.app(), &["check"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("IntrinsicShadowed"), "{said}");
}

/// A package view that calls itself by its package name is the ordinary
/// recursion error, not a loop.
#[test]
fn a_builtin_named_view_calling_itself_is_recursion() {
    let dir = Scratch::new(
        "rec",
        "View Button(label: Str) {\n    ds.Button(label)\n}\n",
        "View Main() { Text(\"x\") }\n",
    );
    let out = byard(&dir.ds(), &["check"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("RecursiveView"), "{said}");
}
