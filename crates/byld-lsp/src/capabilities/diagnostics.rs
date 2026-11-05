//! Diagnostic generation capability for `.byd` documents.

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, PublishDiagnosticsParams, Range};

use crate::state::document::{Document, DocumentStore};
use crate::state::project::{Analysis, ProjectCache, analyze_alone};

/// Computes the diagnostics of a document on its own, as `byard check
/// file.byd` would, and builds `PublishDiagnosticsParams`.
#[must_use]
pub fn compute_diagnostics(doc: &Document) -> PublishDiagnosticsParams {
    publish(doc, &analyze_alone(doc))
}

/// Computes the diagnostics of a document as part of the project it belongs
/// to, with the other open buffers standing for their files: what `byard
/// check` says of this file, and nothing it does not.
#[must_use]
pub fn compute_project_diagnostics(
    doc: &Document,
    projects: &ProjectCache,
    open: &DocumentStore,
) -> PublishDiagnosticsParams {
    publish(doc, &projects.analyze(doc, open))
}

fn publish(doc: &Document, analysis: &Analysis) -> PublishDiagnosticsParams {
    let mut diagnostics = Vec::with_capacity(analysis.errors.len() + analysis.notices.len());
    for err in &analysis.errors {
        let range = doc.line_index.span_to_range(&doc.content, err.span());
        diagnostics.push(Diagnostic {
            range,
            // A warning is advice (RFC-0031 §Q5): `byard check` prints it and
            // passes, so the editor must not paint it as a failure.
            severity: Some(if err.is_warning() {
                DiagnosticSeverity::WARNING
            } else {
                DiagnosticSeverity::ERROR
            }),
            // The variant's name, so a quick fix matches the kind of problem
            // and not the wording of its message.
            code: Some(NumberOrString::String(err.kind().to_string())),
            code_description: None,
            source: Some("byld-compiler".to_string()),
            message: err.headline(),
            related_information: None,
            tags: None,
            data: None,
        });
    }
    for notice in &analysis.notices {
        diagnostics.push(Diagnostic {
            range: Range::default(),
            severity: Some(if notice.warning {
                DiagnosticSeverity::WARNING
            } else {
                DiagnosticSeverity::INFORMATION
            }),
            code: None,
            code_description: None,
            source: Some("byld-compiler".to_string()),
            message: notice.message.clone(),
            related_information: None,
            tags: None,
            data: None,
        });
    }

    PublishDiagnosticsParams {
        uri: doc.uri.clone(),
        diagnostics,
        version: doc.version,
    }
}
