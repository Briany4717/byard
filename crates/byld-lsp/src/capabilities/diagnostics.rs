//! Diagnostic generation capability for `.byd` documents.

use byard_compiler::interp::eval::Interpreter;
use lsp_types::{Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams};

use crate::state::document::Document;

/// Computes diagnostics for a document (syntax, type inference, element lowering)
/// and builds `PublishDiagnosticsParams`.
#[must_use]
pub fn compute_diagnostics(doc: &Document) -> PublishDiagnosticsParams {
    let mut errors = doc.parsed.errors.clone();

    // Type checking errors are pre-computed in Document::new
    errors.extend(doc.inference.errors.clone());

    // Element & Intrinsic validation
    let mut interp = Interpreter::new();
    interp.load_views(&doc.parsed.views);
    let known_views: Vec<&str> = doc.parsed.views.iter().map(|v| v.name.as_str()).collect();
    for view in &doc.parsed.views {
        let _ = interp.lower_view(view, &known_views);
    }
    errors.extend(interp.errors().iter().cloned());

    let mut diagnostics = Vec::with_capacity(errors.len());
    for err in errors {
        let span = err.span();
        let range = doc.line_index.span_to_range(&doc.content, span);

        diagnostics.push(Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("byld-compiler".to_string()),
            message: err.headline(),
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
