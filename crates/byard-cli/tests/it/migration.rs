//! A project moves to the next edition and still checks: `byard fix`, then
//! `byard check`, over every project in the repository.
//!
//! This is the half of the step "three applications nobody on the project
//! wrote" that can be run without them: the migration is the same one they
//! will go through, so what breaks on this repository's own projects is what
//! would have broken on theirs.

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

/// The two directories of examples, which projects reach each other through
/// (`../assets/fonts/…`), so each is copied whole and its projects are the
/// ones inside the copy.
fn example_roots() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![root.join("examples"), root.join("../../examples")]
}

/// Every directory with a `byard.toml` directly inside `dir`.
fn projects_in(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("byard.toml").is_file())
        .collect();
    out.sort();
    out
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_name() == "target" {
            continue;
        }
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

#[test]
fn every_project_of_the_repository_moves_to_the_next_edition_and_checks_clean() {
    let scratch = std::env::temp_dir().join(format!("byard-migration-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let mut problems = Vec::new();
    let mut moved = 0;
    for (n, root) in example_roots().iter().enumerate() {
        let copy = scratch.join(n.to_string());
        copy_dir(root, &copy);
        for project in projects_in(&copy) {
            let name = project.file_name().unwrap().to_string_lossy().into_owned();
            let fixed = byard(&project, &["fix"]);
            if !fixed.status.success() {
                problems.push(format!("{name}: `byard fix` failed:\n{}", text(&fixed)));
                continue;
            }
            let checked = byard(&project, &["check"]);
            let said = text(&checked);
            if !checked.status.success() || said.contains("error[") {
                problems.push(format!(
                    "{name}: does not check in the next edition:\n{said}"
                ));
            }
            // A second `fix` has nowhere to go: it succeeds and changes nothing.
            let manifest = std::fs::read_to_string(project.join("byard.toml")).unwrap_or_default();
            let again = byard(&project, &["fix"]);
            if !again.status.success()
                || std::fs::read_to_string(project.join("byard.toml")).unwrap_or_default()
                    != manifest
                || !manifest.contains("2027")
            {
                problems.push(format!(
                    "{name}: not settled on the newest edition after `byard fix`:\n{}",
                    text(&again)
                ));
            }
            moved += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(moved > 40, "found the projects: {moved}");
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
    println!("migration: {moved} projects moved to the newest edition and check clean");
}
