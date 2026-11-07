//! The server on a small project it is given: sibling files, unsaved buffers,
//! saves, files that change on disk, a broken neighbour.

use std::path::PathBuf;

use crate::client::{Client, assert_clean, code};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        // Tests run in parallel in one process: a directory each.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("byld-lsp-it-{tag}-{}-{n}", std::process::id()));
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

const MAIN: &str = "View Main() {\n  Card()\n}\n";
const CARD: &str = "View Card() {\n  Text(\"hi\")\n}\n";
const MANIFEST: &str = "[project]\nname = \"p\"\nentry = \"main.byd\"\n";

fn project() -> (Dir, PathBuf, PathBuf) {
    let dir = Dir::new("project");
    dir.write("byard.toml", MANIFEST);
    let main = dir.write("main.byd", MAIN);
    let card = dir.write("card.byd", CARD);
    (dir, main, card)
}

fn kinds(diagnostics: &[serde_json::Value]) -> Vec<&str> {
    diagnostics.iter().filter_map(code).collect()
}

#[test]
fn a_view_from_a_sibling_file_is_known() {
    let (_dir, main, _card) = project();
    let mut client = Client::start(false);
    client.open(&main, MAIN);
    assert_clean(&client.next_diagnostics(&main));
}

#[test]
fn an_unsaved_sibling_buffer_is_what_the_file_means() {
    let (_dir, main, card) = project();
    let mut client = Client::start(false);
    client.open(&main, MAIN);
    assert_clean(&client.next_diagnostics(&main));

    // The card is opened and renamed, and not saved: the disk still has
    // `Card`. Opening it, then saving, is what tells `main` to look again.
    client.open(&card, CARD);
    client.next_diagnostics(&card);
    client.change(&card, 2, "View Tile() {\n  Text(\"hi\")\n}\n");
    client.diagnostics_at(&card, 2);
    client.save(&card);
    let after = client.next_diagnostics(&main);
    assert_eq!(kinds(&after), ["UnknownView"], "{after:#?}");
}

/// A save is the moment the project may have changed under every open file,
/// including by something that is not the editor.
#[test]
fn a_save_looks_at_the_disk_again() {
    let (dir, main, _card) = project();
    let mut client = Client::start(false);
    client.open(&main, MAIN);
    assert_clean(&client.next_diagnostics(&main));

    dir.write("card.byd", "View Tile() {\n  Text(\"hi\")\n}\n");
    client.save(&main);
    let after = client.next_diagnostics(&main);
    assert_eq!(kinds(&after), ["UnknownView"], "{after:#?}");
}

#[test]
fn a_file_that_changed_on_disk_is_heard_when_the_client_says_so() {
    let (dir, main, _card) = project();
    let mut client = Client::start(true);
    assert!(
        client.asked_to_watch(),
        "the server asks to watch the project's files"
    );
    client.open(&main, MAIN);
    assert_clean(&client.next_diagnostics(&main));

    dir.write("card.byd", "View Tile() {\n  Text(\"hi\")\n}\n");
    client.watched(&dir.0.join("card.byd"));
    let after = client.next_diagnostics(&main);
    assert_eq!(kinds(&after), ["UnknownView"], "{after:#?}");
}

#[test]
fn a_client_that_cannot_watch_is_not_asked_to() {
    let mut client = Client::start(false);
    assert!(!client.asked_to_watch());
}

#[test]
fn a_new_file_is_part_of_the_project_after_a_save() {
    let (dir, main, _card) = project();
    let extra = "View Main() {\n  Extra()\n}\n";
    dir.write("main.byd", extra);
    let mut client = Client::start(false);
    client.open(&main, extra);
    assert_eq!(kinds(&client.next_diagnostics(&main)), ["UnknownView"]);

    let new = dir.write("extra.byd", "View Extra() {\n  Text(\"x\")\n}\n");
    client.watched(&new);
    assert_clean(&client.next_diagnostics(&main));
}

#[test]
fn a_theme_token_of_the_manifest_is_known() {
    let dir = Dir::new("theme");
    dir.write(
        "byard.toml",
        "[project]\nname = \"t\"\nentry = \"main.byd\"\n\n\
         [theme.typography]\nhero = { size = 80, family = \"Display\", weight = \"bold\" }\n",
    );
    let text = "View Main() {\n  inject Theme as t\n  Text(\"hi\") #[typo: t.hero]\n}\n";
    let main = dir.write("main.byd", text);
    let mut client = Client::start(false);
    client.open(&main, text);
    let diagnostics = client.next_diagnostics(&main);
    assert!(
        !diagnostics.iter().any(crate::client::is_error),
        "{diagnostics:#?}"
    );
}

#[test]
fn a_native_view_of_the_manifest_is_known() {
    let dir = Dir::new("native");
    dir.write(
        "byard.toml",
        "[project]\nname = \"n\"\nentry = \"main.byd\"\n\n[native_views.Gauge]\nvalue = \"float\"\n",
    );
    let text = "View Main() {\n  Gauge #[value: 0.5]\n}\n";
    let main = dir.write("main.byd", text);
    let mut client = Client::start(false);
    client.open(&main, text);
    assert_clean(&client.next_diagnostics(&main));
}

#[test]
fn a_broken_neighbour_is_said_and_does_not_hide_as_silence() {
    let (dir, main, _card) = project();
    dir.write("card.byd", "View Card( {\n");
    let mut client = Client::start(false);
    client.open(&main, MAIN);
    let diagnostics = client.next_diagnostics(&main);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0]["severity"], 3, "information, not an error");
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("card.byd"),
        "{diagnostics:#?}"
    );
}

#[test]
fn a_manifest_that_cannot_be_read_is_a_warning_not_a_wall_of_errors() {
    let dir = Dir::new("broken-manifest");
    dir.write("byard.toml", "[project\n");
    let text = "View Main() {\n  Somewhere()\n}\n";
    let main = dir.write("main.byd", text);
    let mut client = Client::start(false);
    client.open(&main, text);
    let diagnostics = client.next_diagnostics(&main);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0]["severity"], 2);
}

/// A burst of edits is answered with the diagnostics of the last one.
#[test]
fn a_burst_of_edits_ends_with_the_last_ones_diagnostics() {
    let (_dir, main, _card) = project();
    let mut client = Client::start(false);
    client.open(&main, MAIN);
    client.next_diagnostics(&main);
    for version in 2..=30 {
        let text = if version == 30 {
            "View Main() {\n  Nowhere()\n}\n".to_string()
        } else {
            format!("View Main() {{\n  Card() // {version}\n}}\n")
        };
        client.change(&main, version, &text);
    }
    let diagnostics = client.diagnostics_at(&main, 30);
    assert_eq!(kinds(&diagnostics), ["UnknownView"]);
}

#[test]
fn closing_a_file_clears_its_diagnostics() {
    let dir = Dir::new("close");
    let text = "View Main() {\n  Nowhere()\n}\n";
    let main = dir.write("main.byd", text);
    let mut client = Client::start(false);
    client.open(&main, text);
    assert_eq!(client.next_diagnostics(&main).len(), 1);
    client.close(&main);
    assert_clean(&client.next_diagnostics(&main));
}

/// The editor shows what `byard check` shows: code that is not on screen when
/// the program starts (a tab that is not the active one, an arm of a `when`
/// that starts false) is checked too.
#[test]
fn a_tab_that_is_not_the_active_one_is_checked() {
    let src = "View Main() {\n  var tab = \"a\"\n  var on = false\n  \
        NavHost(active: tab) {\n    tab \"a\" { Text(\"x\") }\n    tab \"b\" { Bogus() }\n  }\n  \
        when on { Text(\"y\") } else { Other() }\n}\n";
    let dir = Dir::new("tabs");
    dir.write("byard.toml", MANIFEST);
    let main = dir.write("main.byd", src);
    let mut client = Client::start(false);
    client.open(&main, src);
    let found = client.next_diagnostics(&main);
    assert_eq!(kinds(&found), ["UnknownView", "UnknownView"], "{found:#?}");
}
