//! `byard fix [path]`, move a project to the next edition.
//!
//! Rewrites the project's own `.byd` files so they mean in the next edition
//! what they mean in the one the manifest names, prints each one that
//! changed through the formatter, and then names the next edition in
//! `byard.toml`. Nothing is written until every file has been rewritten
//! without trouble, so a file that does not parse leaves the project as it
//! was. Path dependencies are other projects, with editions of their own:
//! `byard fix` them where they are.

use crate::deps::byd_files_under;
use crate::manifest::{Manifest, with_edition};
use crate::style;
use byard_compiler::edition::Edition;
use std::path::{Path, PathBuf};

/// Moves the project at `path` (or the current one) to the next edition.
pub fn run(path: Option<&Path>) -> Result<(), String> {
    let started = std::time::Instant::now();
    let manifest = Manifest::discover(path).map_err(|e| e.to_string())?;
    let manifest_path = manifest.project_root.join("byard.toml");
    if manifest.single_file || !manifest_path.is_file() {
        return Err(format!(
            "`byard fix` needs a project with a byard.toml, and found none at {}",
            manifest.project_root.display()
        ));
    }
    let from = manifest.edition;
    let Some(to) = from.next() else {
        style::ok(&format!("already on the newest edition, {from}"), None);
        return Ok(());
    };

    let rewritten = rewrite(&manifest, from)?;
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let text = with_edition(&text, to)?;

    for (file, source) in &rewritten {
        std::fs::write(file, source).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    std::fs::write(&manifest_path, text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;

    let changed = rewritten.len();
    style::ok(
        &format!(
            "{} moved from {from} to {to}, {changed} file{} rewritten",
            manifest.name,
            if changed == 1 { "" } else { "s" }
        ),
        Some(started.elapsed()),
    );
    Ok(())
}

/// Each source file of the project that `from`'s rewrites change, with what
/// it becomes. Any file that cannot be rewritten fails the lot.
fn rewrite(manifest: &Manifest, from: Edition) -> Result<Vec<(PathBuf, String)>, String> {
    let dir = if manifest.library {
        manifest.project_root.clone()
    } else {
        manifest
            .entry
            .parent()
            .unwrap_or(&manifest.project_root)
            .to_path_buf()
    };
    let mut changed = Vec::new();
    let mut failed = Vec::new();
    for file in byd_files_under(&dir).map_err(|e| e.to_string())? {
        let source =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        match from.migrate(&source) {
            Ok(Some(next)) => changed.push((file, next)),
            Ok(None) => {}
            Err(e) => failed.push(format!("{}:{e}", file.display())),
        }
    }
    if failed.is_empty() {
        Ok(changed)
    } else {
        Err(format!(
            "nothing was changed: {} file{} could not be rewritten\n{}",
            failed.len(),
            if failed.len() == 1 { "" } else { "s" },
            failed.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(tag: &str, manifest: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("byard-fix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        std::fs::write(dir.join("byard.toml"), manifest).expect("a manifest");
        std::fs::write(dir.join("main.byd"), "View Main() {  Text(\"a\")  }\n").expect("a view");
        dir
    }

    #[test]
    fn a_project_moves_to_the_next_edition_and_a_second_run_changes_nothing() {
        let dir = project("moves", "[project]\nname = \"x\"\n");
        run(Some(&dir)).expect("fixed");
        let manifest = std::fs::read_to_string(dir.join("byard.toml")).expect("read");
        assert_eq!(manifest, "[project]\nedition = \"2027\"\nname = \"x\"\n");
        // Nothing needed rewriting, so the source is exactly as it was.
        let source = std::fs::read_to_string(dir.join("main.byd")).expect("read");
        assert_eq!(source, "View Main() {  Text(\"a\")  }\n");
        run(Some(&dir)).expect("already there");
        assert_eq!(
            std::fs::read_to_string(dir.join("byard.toml")).expect("read"),
            manifest
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_package_names_its_edition_under_package() {
        let dir = project("library", "[package]\nname = \"k\"\nversion = \"0.1.0\"\n");
        run(Some(&dir)).expect("fixed");
        let manifest = std::fs::read_to_string(dir.join("byard.toml")).expect("read");
        assert!(
            manifest.starts_with("[package]\nedition = \"2027\"\n"),
            "{manifest}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lone_file_is_not_a_project() {
        let dir = project("lone", "[project]\nname = \"x\"\n");
        let err = run(Some(&dir.join("main.byd"))).expect_err("no project");
        assert!(err.contains("needs a project"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
