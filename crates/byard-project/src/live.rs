//! What a running program needs to follow its project while it is edited:
//! which paths to watch, and the program re-read from them.
//!
//! Shared by `byard dev` and by an app built on `byard::App` that asks for
//! hot reload, so the two reload the same thing the same way.

use std::path::{Path, PathBuf};

use byard_compiler::CompileError;
use byard_compiler::interp::reload::ParsedFile;

use crate::deps::{FsProvider, cache_dir, resolve_project};
use crate::manifest::Manifest;

/// The directories to watch for a project: the project itself (its
/// sources, its manifest, the images beside them) and every `path`
/// dependency it resolved. Package cache checkouts are pinned and are not
/// watched. A lone `.byd` is not a project: only its directory.
#[must_use]
pub fn watch_paths(manifest: &Manifest, provider: &FsProvider) -> Vec<PathBuf> {
    let project_dir = if manifest.single_file {
        manifest
            .entry
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf()
    } else {
        manifest.project_root.clone()
    };
    let cache = cache_dir();
    let mut paths = vec![project_dir];
    for root in provider.resolved_roots().values() {
        if !root.starts_with(&cache) {
            paths.push(root.clone());
        }
    }
    paths
}

/// The whole program read again: the manifest discovered afresh (so a
/// `byard.toml` edit applies, its theme included), the module resolver run
/// again, and a project-level failure folded into the program's errors.
#[must_use]
pub fn reresolve(entry: Option<&Path>) -> ParsedFile {
    let resolved = Manifest::discover(entry)
        .and_then(|m| resolve_project(&m).map(|(program, _)| (program, m.theme, m.watch_files)));
    match resolved {
        Ok((program, theme, watch_files)) => ParsedFile {
            views: program.views,
            errors: program.errors,
            theme: Some(theme),
            watch_files,
        },
        Err(message) => ParsedFile {
            views: Vec::new(),
            errors: vec![CompileError::Project {
                span: byard_compiler::Span::new(0, 0),
                message,
            }],
            theme: None,
            watch_files: Vec::new(),
        },
    }
}
