//! Every project in the repository's `examples/`: for each file, the editor
//! and the `byard check` of the whole project say the same.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use byard_project::check::check_program;
use byard_project::deps::{project_source_paths, resolve_project};
use byard_project::manifest::Manifest;

use crate::client::{Client, code, line};

fn projects() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    for parent in ["examples", "crates/byard-cli/examples"] {
        let Ok(entries) = std::fs::read_dir(root.join(parent)) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir.join("byard.toml").is_file() {
                found.push(dir.canonicalize().unwrap());
            }
        }
    }
    found.sort();
    found
}

/// What `byard check` says of the project, as `(file, line, kind)`.
fn check(manifest: &Manifest) -> BTreeSet<(String, u64, String)> {
    let (program, _provider) = resolve_project(manifest).unwrap();
    let errors = check_program(&program, manifest.theme.clone());
    errors
        .iter()
        .filter_map(|e| {
            let (entry, local) = program.source_map.locate(e.span())?;
            let (line, _) = byard_compiler::resolve::line_col(&entry.source, local.start as usize);
            Some((entry.file.clone(), line as u64, e.kind().to_string()))
        })
        .collect()
}

#[test]
fn the_editor_and_byard_check_agree_on_every_example() {
    let mut compared = 0;
    let mut with_findings = 0;
    for dir in projects() {
        let Ok(manifest) = Manifest::discover(Some(&dir)) else {
            continue;
        };
        // A package that needs fetching cannot be checked here, by either.
        if resolve_project(&manifest).is_err() {
            continue;
        }
        let expected = check(&manifest);
        let mut client = Client::start(false);
        let mut got = BTreeSet::new();
        for (name, path) in project_source_paths(&manifest).unwrap() {
            client.open(&path, &std::fs::read_to_string(&path).unwrap());
            for d in client.next_diagnostics(&path) {
                // Only what the compiler says: the notices the server adds
                // about its own state have no code.
                if let Some(kind) = code(&d) {
                    got.insert((name.clone(), line(&d), kind.to_string()));
                }
            }
        }
        assert_eq!(
            got,
            expected,
            "{}: the editor and `byard check` disagree",
            dir.display()
        );
        compared += 1;
        with_findings += usize::from(!expected.is_empty());
    }
    assert!(compared >= 30, "{compared} projects compared");
    println!("{compared} projects compared, {with_findings} of them with findings");
}
