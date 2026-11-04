//! Document state management with cached parsing, type inference, and line indexing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use byard_compiler::infer::{Inference, check_views};
use byard_compiler::parser::{ParsedFile, parse};
use dashmap::DashMap;
use lsp_types::Uri;

use crate::syntax::line_index::LineIndex;

/// A thread-safe, immutable representation of a parsed `.byd` document.
#[derive(Debug)]
pub struct Document {
    /// Document URI.
    pub uri: Uri,
    /// Document version identifier.
    pub version: Option<i32>,
    /// Full source text content.
    pub content: String,
    /// Line index for position <-> offset mapping.
    pub line_index: LineIndex,
    /// Parsed file AST and imports.
    pub parsed: ParsedFile,
    /// Type inference results for all views.
    pub inference: Inference,
    /// The file the URI names, as the project walk spells it (canonical), so
    /// the document can be matched with the project's files. `None` for a
    /// URI that is not a file.
    pub path: Option<PathBuf>,
}

impl Document {
    /// Constructs a new `Document` by building the `LineIndex`, parsing the source, and running type inference.
    #[must_use]
    pub fn new(uri: Uri, version: Option<i32>, content: String) -> Self {
        let path = file_path_of(&uri);
        Self::at(uri, version, content, path)
    }

    /// Like [`Document::new`], for a caller that already knows the file's
    /// canonical path (the store resolves it once, when the document opens,
    /// not on every keystroke).
    #[must_use]
    pub fn at(uri: Uri, version: Option<i32>, content: String, path: Option<PathBuf>) -> Self {
        let line_index = LineIndex::new(&content);
        let parsed = parse(&content);
        let inference = check_views(&parsed.views);

        Self {
            uri,
            version,
            content,
            line_index,
            parsed,
            inference,
            path,
        }
    }

    /// Helper to get file path from Uri if available.
    #[must_use]
    pub fn file_path(&self) -> Option<PathBuf> {
        file_path_of(&self.uri)
    }
}

/// The file path a `file:` URI names.
fn file_path_of(uri: &Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str())
        .ok()
        .and_then(|u| u.to_file_path().ok())
}

/// Concurrent, lock-free store for open LSP documents.
#[derive(Default, Debug)]
pub struct DocumentStore {
    docs: DashMap<Uri, Arc<Document>>,
}

impl DocumentStore {
    /// Creates a new empty `DocumentStore`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            docs: DashMap::new(),
        }
    }

    /// Upserts a document in the store and returns an `Arc<Document>`.
    ///
    /// The file's canonical path is looked up when the document opens and kept
    /// across its edits: a path is what ties a buffer to the project file it
    /// stands for, and a syscall per keystroke to learn it again is waste.
    pub fn insert(&self, uri: Uri, version: Option<i32>, text: String) -> Arc<Document> {
        let path = match self.docs.get(&uri) {
            Some(previous) => previous.path.clone(),
            None => file_path_of(&uri).map(|p| p.canonicalize().unwrap_or(p)),
        };
        let doc = Arc::new(Document::at(uri.clone(), version, text, path));
        self.docs.insert(uri, Arc::clone(&doc));
        doc
    }

    /// The open document that stands for `path`, if there is one: its text
    /// is what the file means, whatever the disk holds.
    #[must_use]
    pub fn get_by_path(&self, path: &Path) -> Option<Arc<Document>> {
        self.docs
            .iter()
            .find(|entry| entry.value().path.as_deref() == Some(path))
            .map(|entry| Arc::clone(entry.value()))
    }

    /// Every open document.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<Document>> {
        self.docs.iter().map(|e| Arc::clone(e.value())).collect()
    }

    /// Gets a document from the store.
    #[must_use]
    pub fn get(&self, uri: &Uri) -> Option<Arc<Document>> {
        self.docs.get(uri).map(|r| Arc::clone(r.value()))
    }

    /// Removes a document from the store.
    pub fn remove(&self, uri: &Uri) -> Option<Arc<Document>> {
        self.docs.remove(uri).map(|(_, doc)| doc)
    }
}
