//! Aura Weather is a real app of seven `.byd` files that share views,
//! functions and record types, a custom theme, a native view drawn in Rust and
//! the framework's capabilities. `byard check` is clean on it. The editor has
//! to say the same of every file, opened alone, with nothing else open: a red
//! line in a file that compiles is the first thing that teaches a person to
//! ignore the editor.

use std::path::{Path, PathBuf};

use crate::client::{Client, code, is_error};

fn project() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/aura_weather")
        .canonicalize()
        .expect("the example is in the repository")
}

fn sources() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(project().join("src"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "byd"))
        .collect();
    files.sort();
    assert!(files.len() >= 7, "the app's views: {files:?}");
    files
}

/// Every file, opened by itself in its own session, has no diagnostics: not
/// an error, and not a warning either, which is what `byard check` prints.
#[test]
fn every_file_of_aura_weather_is_clean_opened_alone() {
    for file in sources() {
        let mut client = Client::start(false);
        client.open(&file, &std::fs::read_to_string(&file).unwrap());
        let diagnostics = client.next_diagnostics(&file);
        assert!(
            diagnostics.is_empty(),
            "{}: the editor reports what `byard check` does not: {diagnostics:#?}",
            file.display()
        );
    }
}

/// And all of them open at once, one session, the way a person has a project.
#[test]
fn every_file_of_aura_weather_is_clean_opened_together() {
    let mut client = Client::start(true);
    let files = sources();
    for file in &files {
        client.open(file, &std::fs::read_to_string(file).unwrap());
    }
    for file in &files {
        let diagnostics = client.next_diagnostics(file);
        assert!(
            !diagnostics.iter().any(crate::client::is_error),
            "{}: {diagnostics:#?}",
            file.display()
        );
    }
}

/// The control: the same text with no project around it is full of errors, so
/// the test above is measuring the project and not an editor that says nothing.
#[test]
fn the_same_files_outside_their_project_are_not_clean() {
    let dir = std::env::temp_dir().join(format!("byld-lsp-aura-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut client = Client::start(false);
    let mut with_errors = 0;
    for file in sources() {
        let copy = dir.join(file.file_name().unwrap());
        std::fs::copy(&file, &copy).unwrap();
        client.open(&copy, &std::fs::read_to_string(&copy).unwrap());
        let found = client.next_diagnostics(&copy);
        with_errors += usize::from(found.iter().any(is_error));
    }
    let _ = std::fs::remove_dir_all(&dir);
    // Six of the seven reach for something that lives in another file.
    assert!(with_errors >= 6, "{with_errors} files with errors");
}

/// What the editor said before this existed, for the record: the names of the
/// problems the project context removes.
#[test]
fn what_the_project_context_removes() {
    let file = project().join("src/forecast.byd");
    let mut client = Client::start(false);
    client.open(&file, &std::fs::read_to_string(&file).unwrap());
    let diagnostics = client.next_diagnostics(&file);
    let kinds: Vec<_> = diagnostics.iter().filter_map(code).collect();
    assert!(kinds.is_empty(), "{kinds:?}");
}
