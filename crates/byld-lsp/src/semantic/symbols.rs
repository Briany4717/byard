//! Symbol indexing and package resolution for package imports (`use`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use byard_compiler::parser::ast::{UseDecl, ViewDecl};
use byard_compiler::parser::parse;

/// The exported views of every package the document's manifest declares.
#[derive(Default, Debug, Clone)]
pub struct PackageIndex {
    /// Package name -> its exported views.
    pub exports: HashMap<String, Vec<ViewDecl>>,
    /// Every declared dependency name (even unresolvable ones), for `use <TAB>` completions.
    pub declared: Vec<String>,
}

impl PackageIndex {
    /// Builds the index for the document at `doc_path` by walking up to its `byard.toml`.
    #[must_use]
    pub fn build(doc_path: Option<&Path>) -> Self {
        let Some(doc_path) = doc_path else {
            return Self::default();
        };
        let Some(manifest_dir) = doc_path
            .ancestors()
            .skip(1)
            .find(|d| d.join("byard.toml").exists())
        else {
            return Self::default();
        };
        let Ok(src) = std::fs::read_to_string(manifest_dir.join("byard.toml")) else {
            return Self::default();
        };
        let Ok(table) = src.parse::<toml::Table>() else {
            return Self::default();
        };
        let Some(deps) = table.get("dependencies").and_then(|d| d.as_table()) else {
            return Self::default();
        };

        let mut index = Self::default();
        for (name, spec) in deps {
            index.declared.push(name.clone());
            let Some(rel) = spec.get("path").and_then(|p| p.as_str()) else {
                continue;
            };
            let root = manifest_dir.join(rel);
            let scan = if root.join("src").is_dir() {
                root.join("src")
            } else {
                root.clone()
            };
            let mut views = Vec::new();
            collect_package_views(&scan, &mut views);
            index.exports.insert(name.clone(), views);
        }
        index
    }

    /// The exported view named `view` of package `pkg`, if indexed.
    #[must_use]
    pub fn view(&self, pkg: &str, view: &str) -> Option<&ViewDecl> {
        self.exports
            .get(pkg)?
            .iter()
            .find(|v| v.name.as_str() == view)
    }
}

/// Recursively parses every `.byd` under `dir` and collects its views.
fn collect_package_views(dir: &Path, out: &mut Vec<ViewDecl>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if path.is_dir() {
            if name
                .as_deref()
                .is_some_and(|n| n.starts_with('.') || n == "target")
            {
                continue;
            }
            collect_package_views(&path, out);
        } else if path.extension().is_some_and(|e| e == "byd") {
            if let Ok(src) = std::fs::read_to_string(&path) {
                out.extend(parse(&src).views);
            }
        }
    }
}

/// Resolves an element name to a package-exported view through this file's imports.
#[must_use]
pub fn resolve_package_view<'i>(
    el_name: &str,
    imports: &[UseDecl],
    index: &'i PackageIndex,
) -> Option<&'i ViewDecl> {
    if let Some((head, rest)) = el_name.split_once('.') {
        let import = imports.iter().find(|i| {
            i.symbols.is_none()
                && i.alias
                    .as_ref()
                    .map_or(i.package.as_str(), byard_compiler::Symbol::as_str)
                    == head
        })?;
        return index.view(import.package.as_str(), rest);
    }
    imports.iter().find_map(|i| {
        i.symbols.as_ref().and_then(|symbols| {
            symbols
                .iter()
                .find(|(s, _)| s.as_str() == el_name)
                .and_then(|_| index.view(i.package.as_str(), el_name))
        })
    })
}
