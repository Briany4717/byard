//! Document state management with cached parsing, type inference, and line indexing.

use std::path::PathBuf;
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
}

impl Document {
    /// Constructs a new `Document` by building the `LineIndex`, parsing the source, and running type inference.
    #[must_use]
    pub fn new(uri: Uri, version: Option<i32>, content: String) -> Self {
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
        }
    }

    /// Helper to get file path from Uri if available.
    #[must_use]
    pub fn file_path(&self) -> Option<PathBuf> {
        url::Url::parse(self.uri.as_str())
            .ok()
            .and_then(|u| u.to_file_path().ok())
    }
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
    pub fn insert(&self, uri: Uri, version: Option<i32>, text: String) -> Arc<Document> {
        let doc = Arc::new(Document::new(uri.clone(), version, text));
        self.docs.insert(uri, Arc::clone(&doc));
        doc
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
