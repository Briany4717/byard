//! A package that declares a view named like a built-in: the package checks
//! clean in the editor, and the qualified name (`m.Button`, `ds.Button`) gets
//! hover, definition and completion while the bare one stays the built-in.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::client::{Client, assert_clean, uri};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        // Tests run in parallel in one process: a directory each.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("byld-lsp-names-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const LIB: &str =
    "View Button(label: Str) {\n  Button(label)\n}\n\nView Pair() {\n  ds.Button(\"a\")\n}\n";
const APP: &str = "use ds as m\n\nView Main() {\n  m.Button(\"a\")\n  Button(\"b\")\n}\n";

fn scene() -> (Dir, PathBuf, PathBuf) {
    let dir = Dir::new("scene");
    dir.write(
        "ds/byard.toml",
        "[package]\nname = \"ds\"\nversion = \"0.1.0\"\n",
    );
    let lib = dir.write("ds/src/lib.byd", LIB);
    dir.write(
        "app/byard.toml",
        "[project]\nname = \"app\"\nentry = \"main.byd\"\n\n[dependencies]\nds = { path = \"../ds\" }\n",
    );
    let app = dir.write("app/main.byd", APP);
    (dir, lib, app)
}

fn at(client: &mut Client, method: &str, path: &Path, line: u32, character: u32) -> Value {
    client.request(
        method,
        json!({
            "textDocument": { "uri": uri(path) },
            "position": { "line": line, "character": character }
        }),
    )
}

#[test]
fn the_package_checks_clean_in_the_editor() {
    let (_dir, lib, _app) = scene();
    let mut client = Client::start(false);
    client.open(&lib, LIB);
    assert_clean(&client.next_diagnostics(&lib));
}

#[test]
fn definition_of_the_qualified_name_is_the_package_view() {
    let (_dir, lib, app) = scene();
    let mut client = Client::start(false);
    client.open(&lib, LIB);
    client.open(&app, APP);
    // `ds.Button` inside the package: its own `View Button`, line 0.
    let own = at(&mut client, "textDocument/definition", &lib, 5, 6);
    assert_eq!(own["uri"], json!(uri(&lib)), "{own}");
    assert_eq!(own["range"]["start"]["line"], json!(0), "{own}");
    // `m.Button` in the app: the same view, in the package's file.
    let from_app = at(&mut client, "textDocument/definition", &app, 3, 4);
    assert_eq!(from_app["uri"], json!(uri(&lib)), "{from_app}");
    assert_eq!(from_app["range"]["start"]["line"], json!(0), "{from_app}");
    // A bare `Button` is the built-in, which has no declaration to go to.
    let bare = at(&mut client, "textDocument/definition", &app, 4, 4);
    assert!(bare.is_null(), "{bare}");
    let inner = at(&mut client, "textDocument/definition", &lib, 1, 4);
    assert!(inner.is_null(), "{inner}");
}

#[test]
fn hover_on_the_qualified_name_shows_the_view() {
    let (_dir, _lib, app) = scene();
    let mut client = Client::start(false);
    client.open(&app, APP);
    let hover = at(&mut client, "textDocument/hover", &app, 3, 4);
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(value.contains("View Button(label)"), "{value}");
    assert!(value.contains("package `ds`"), "{value}");
}

#[test]
fn the_package_offers_its_own_view_qualified() {
    let (_dir, lib, _app) = scene();
    let text = format!("{LIB}\nView Third() {{\n  \n}}\n");
    let mut client = Client::start(false);
    client.open(&lib, &text);
    let at_body = at(&mut client, "textDocument/completion", &lib, 8, 2);
    let labels: Vec<&str> = at_body
        .as_array()
        .or_else(|| at_body["items"].as_array())
        .unwrap()
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();
    assert!(labels.contains(&"ds.Button"), "{labels:?}");
    assert!(labels.contains(&"Pair"), "{labels:?}");
}
