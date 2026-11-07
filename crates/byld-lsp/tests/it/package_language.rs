//! Constants, enums and private names in the editor (RFC-0055): a package
//! that uses them checks clean, a consumer is told when it reaches a private
//! name, and completion and hover know the new declarations.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::client::{Client, assert_clean, code, uri};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("byld-lsp-lang-{tag}-{}-{n}", std::process::id()));
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

const TOKENS: &str = "let gap = 8\nprivate let secret = 1\n\nenum Variant { filled, tonal, outlined }\n\nprivate fn tint(v: Variant) -> Int => v == Variant.filled ? 1 : 2\n";
const BUTTON: &str = "View Button(label: Str, variant: Variant = Variant.filled) {\n  Box #[p: gap] { Text(\"{label}{tint(variant)}{secret}\") }\n}\n\nprivate View Hint() {\n  Text(\"hint\")\n}\n";

fn scene(app_src: &str) -> (Dir, PathBuf, PathBuf, PathBuf) {
    let dir = Dir::new("scene");
    dir.write(
        "kit/byard.toml",
        "[package]\nname = \"kit\"\nversion = \"0.1.0\"\n",
    );
    let tokens = dir.write("kit/src/tokens.byd", TOKENS);
    let button = dir.write("kit/src/button.byd", BUTTON);
    dir.write(
        "app/byard.toml",
        "[project]\nname = \"app\"\nentry = \"main.byd\"\n\n[dependencies]\nkit = { path = \"../kit\" }\n",
    );
    let app = dir.write("app/main.byd", app_src);
    (dir, tokens, button, app)
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

fn labels(response: &Value) -> Vec<String> {
    response
        .as_array()
        .or_else(|| response["items"].as_array())
        .unwrap()
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn the_package_checks_clean_in_the_editor() {
    let (_dir, tokens, button, _app) = scene("View Main() { Text(\"x\") }\n");
    let mut client = Client::start(false);
    client.open(&tokens, TOKENS);
    assert_clean(&client.next_diagnostics(&tokens));
    client.open(&button, BUTTON);
    assert_clean(&client.next_diagnostics(&button));
}

#[test]
fn a_consumer_using_the_public_names_is_clean_and_a_private_one_is_told() {
    let src = "use kit as m\n\nView Main() {\n  Column #[gap: m.gap] {\n    m.Button(\"a\", variant: .tonal)\n    m.Hint()\n  }\n}\n";
    let (_dir, tokens, _button, app) = scene(src);
    let mut client = Client::start(false);
    client.open(&tokens, TOKENS);
    client.open(&app, src);
    let diagnostics = client.next_diagnostics(&app);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(code(&diagnostics[0]), Some("PrivateName"));
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(
        message.contains("`Hint` is private to package `kit`"),
        "{message}"
    );
}

#[test]
fn a_misspelt_case_is_an_error_in_the_editor_with_the_suggestion() {
    let src = "use kit as m\n\nView Main() {\n  Text(\"{m.Variant.tonl}\")\n}\n";
    let (_dir, _tokens, _button, app) = scene(src);
    let mut client = Client::start(false);
    client.open(&app, src);
    let diagnostics = client.next_diagnostics(&app);
    assert_eq!(
        code(&diagnostics[0]),
        Some("UnknownEnumCase"),
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("did you mean `tonal`"),
        "{diagnostics:#?}"
    );
}

#[test]
fn completion_after_an_enums_name_offers_its_cases() {
    let src = "enum Variant { filled, tonal }\n\nView Main() {\n  Text(\"{Variant.}\")\n}\n";
    let dir = Dir::new("own");
    let path = dir.write("main.byd", src);
    let mut client = Client::start(false);
    client.open(&path, src);
    let response = at(&mut client, "textDocument/completion", &path, 3, 17);
    assert_eq!(labels(&response), ["filled", "tonal"]);
}

#[test]
fn completion_after_a_package_alias_offers_what_is_public() {
    let src = "use kit as m\n\nView Main() {\n  Text(\"{m.}\")\n}\n";
    let (_dir, _tokens, _button, app) = scene(src);
    let mut client = Client::start(false);
    client.open(&app, src);
    let response = at(&mut client, "textDocument/completion", &app, 3, 11);
    let labels = labels(&response);
    for wanted in ["gap", "Variant", "Button"] {
        assert!(labels.contains(&wanted.to_string()), "{labels:?}");
    }
    for hidden in ["secret", "Hint"] {
        assert!(
            !labels.contains(&hidden.to_string()),
            "{hidden} is private: {labels:?}"
        );
    }
}

#[test]
fn completion_after_a_package_enum_offers_its_cases() {
    let src = "use kit as m\n\nView Main() {\n  Text(\"{m.Variant.}\")\n}\n";
    let (_dir, _tokens, _button, app) = scene(src);
    let mut client = Client::start(false);
    client.open(&app, src);
    let response = at(&mut client, "textDocument/completion", &app, 3, 19);
    assert_eq!(labels(&response), ["filled", "tonal", "outlined"]);
}

#[test]
fn hover_shows_a_constants_type_and_an_enums_cases() {
    let src = "let gap = 8\nenum Variant { filled, tonal }\n\nView Main() {\n  Text(\"{gap} {Variant.tonal}\")\n}\n";
    let dir = Dir::new("hover");
    let path = dir.write("main.byd", src);
    let mut client = Client::start(false);
    client.open(&path, src);
    let constant = at(&mut client, "textDocument/hover", &path, 0, 5);
    assert!(
        constant["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("let gap: Int"),
        "{constant}"
    );
    let enumeration = at(&mut client, "textDocument/hover", &path, 1, 7);
    assert!(
        enumeration["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("enum Variant { filled, tonal }"),
        "{enumeration}"
    );
    let case = at(&mut client, "textDocument/hover", &path, 4, 28);
    assert!(
        case["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("A case of the enum `Variant`"),
        "{case}"
    );
}

#[test]
fn the_outline_lists_constants_and_enums_with_their_cases() {
    let src = "let gap = 8\nenum Variant { filled, tonal }\nView Main() { Text(\"x\") }\n";
    let dir = Dir::new("outline");
    let path = dir.write("main.byd", src);
    let mut client = Client::start(false);
    client.open(&path, src);
    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri(&path) } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(names, ["Variant", "gap", "Main"]);
    let cases: Vec<&str> = symbols[0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(cases, ["filled", "tonal"]);
}
