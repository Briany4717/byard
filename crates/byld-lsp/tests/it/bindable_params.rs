//! RFC-0051 in the editor: a view with a binding checks clean, hover says
//! `bind`, the declaration is coloured as one, and `BindNotAVar` is shown.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::client::{Client, assert_clean, code, uri};

const SOURCE: &str = "View Sw(bind on: Bool, label: Str = \"\") {\n  Toggle #[bind: on]\n  Text(label)\n}\n\nView Main() {\n  var wifi = true\n  Sw(bind: wifi, label: \"Wi-Fi\")\n}\n";

fn file(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("byld-lsp-bind-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap().join("main.byd")
}

fn hover(client: &mut Client, path: &Path, line: u32, character: u32) -> String {
    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri(path) },
            "position": { "line": line, "character": character }
        }),
    );
    hover["contents"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_view_with_a_binding_checks_clean() {
    let path = file("clean");
    let mut client = Client::start(false);
    client.open(&path, SOURCE);
    assert_clean(&client.next_diagnostics(&path));
}

#[test]
fn hover_on_a_binding_shows_bind_and_its_type() {
    let path = file("hover");
    let mut client = Client::start(false);
    client.open(&path, SOURCE);
    // The declaration, and the use in the body.
    for (line, character) in [(0, 14), (1, 17)] {
        let text = hover(&mut client, &path, line, character);
        assert!(text.contains("bind on: Bool"), "{line}:{character}: {text}");
    }
    // A parameter that is not a binding is not described as one.
    let text = hover(&mut client, &path, 2, 9);
    assert!(!text.contains("bind"), "{text}");
}

#[test]
fn bind_and_the_name_after_it_are_coloured_apart() {
    let path = file("tokens");
    let mut client = Client::start(false);
    client.open(&path, SOURCE);
    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri(&path) } }),
    );
    let data: Vec<u64> = tokens["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    // Absolute (line, start, length, type) of each token on the first line.
    let (mut line, mut start) = (0, 0);
    let mut first_line = Vec::new();
    for t in data.chunks(5) {
        if t[0] > 0 {
            line += t[0];
            start = 0;
        }
        start += t[1];
        if line == 0 {
            first_line.push((start, t[2], t[3]));
        }
    }
    // `View Sw(bind on: Bool, ...`: `bind` at 8 is a keyword (type 0), `on`
    // at 13 a variable (type 3).
    assert!(first_line.contains(&(8, 4, 0)), "{first_line:?}");
    assert!(first_line.contains(&(13, 2, 3)), "{first_line:?}");
}

#[test]
fn a_let_given_to_a_binding_is_an_error_in_the_editor() {
    let path = file("error");
    let text = SOURCE.replace("var wifi = true", "let wifi = true");
    let mut client = Client::start(false);
    client.open(&path, &text);
    let diagnostics: Vec<Value> = client.next_diagnostics(&path);
    let codes: Vec<&str> = diagnostics.iter().filter_map(code).collect();
    assert_eq!(codes, ["BindNotAVar"], "{diagnostics:#?}");
}
