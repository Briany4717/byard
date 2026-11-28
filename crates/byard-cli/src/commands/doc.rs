//! `byard doc [PATH] [-o FILE]`: the Markdown page of a project or a package,
//! from what its files declare and what their `///` comments say.

use std::path::Path;

use crate::deps::project_source_files;
use crate::manifest::Manifest;
use crate::style;

pub fn run(path: Option<&Path>, out: Option<&Path>) -> Result<(), String> {
    let manifest = Manifest::discover(path).map_err(|e| e.to_string())?;
    let files: Vec<(String, String)> = project_source_files(&manifest)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|f| (f.name, f.source))
        .collect();
    let page = byard_compiler::doc::page(&manifest.name, &files);
    match out {
        Some(out) => {
            std::fs::write(out, &page).map_err(|e| format!("{}: {e}", out.display()))?;
            style::ok(&format!("wrote {}", out.display()), None);
        }
        None => print!("{page}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_is_written_into_a_page_with_its_doc_comments() {
        let dir = std::env::temp_dir().join(format!("byard_doc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("byard.toml"), "[package]\nname = \"kit\"\n").unwrap();
        std::fs::write(
            dir.join("src/main.byd"),
            "/// A card.\nView Card(label: Str) { Text(label) }\nprivate View Inner() { Text(\"i\") }\n",
        )
        .unwrap();
        let out = dir.join("kit.md");
        run(Some(&dir), Some(&out)).expect("byard doc");
        let page = std::fs::read_to_string(&out).unwrap();
        assert!(page.starts_with("# kit\n"), "{page}");
        assert!(
            page.contains("View Card(label: Str)") && page.contains("A card."),
            "{page}"
        );
        assert!(!page.contains("Inner"), "{page}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
