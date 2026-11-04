//! Code actions capability providing quick-fixes and automated refactorings.

use std::collections::HashMap;

use byard_compiler::diagnostics::Span;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    NumberOrString, Range, TextEdit, WorkspaceEdit,
};

use crate::state::document::Document;

/// Handles `textDocument/codeAction` request to generate automated quick-fixes.
#[must_use]
pub fn handle_code_action(doc: &Document, params: CodeActionParams) -> Option<CodeActionResponse> {
    let mut actions = Vec::new();

    // 1. Process diagnostic-driven quick fixes
    for diagnostic in &params.context.diagnostics {
        // The compiler's own name for the problem, set by `diagnostics`, not
        // a guess at its wording: a message can be reworded, a kind cannot
        // without breaking this on purpose.
        if matches!(
            &diagnostic.code,
            Some(NumberOrString::String(kind)) if kind == "UnknownAttribute"
        ) {
            let edit = TextEdit {
                range: attribute_removal_range(doc, diagnostic.range),
                new_text: String::new(),
            };
            let mut changes = HashMap::new();
            changes.insert(doc.uri.clone(), vec![edit]);

            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Remove invalid attribute".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                is_preferred: Some(true),
                disabled: None,
                data: None,
            }));
        }
    }

    // 2. Refactoring code actions based on range/cursor position
    let offset = doc
        .line_index
        .position_to_offset(&doc.content, params.range.start)?;
    for view in &doc.parsed.views {
        if crate::syntax::ast_utils::span_contains(view.span, offset) {
            for member in &view.body {
                if let byard_compiler::parser::ast::Member::Var { name, span, .. } = member {
                    if crate::syntax::ast_utils::span_contains(*span, offset) {
                        // Only the keyword changes. Whatever follows it, the type
                        // written, the initializer, a comment on the line, is the
                        // person's text and stays exactly as they wrote it.
                        let keyword = Span::new(span.start, span.start + 3);
                        if doc
                            .content
                            .get(keyword.start as usize..keyword.end as usize)
                            != Some("var")
                        {
                            continue;
                        }
                        let edit = TextEdit {
                            range: doc.line_index.span_to_range(&doc.content, keyword),
                            new_text: "let".to_string(),
                        };
                        let mut changes = HashMap::new();
                        changes.insert(doc.uri.clone(), vec![edit]);

                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!("Convert 'var {name}' to 'let {name}' (Memo)"),
                            kind: Some(CodeActionKind::REFACTOR_REWRITE),
                            diagnostics: None,
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                document_changes: None,
                                change_annotations: None,
                            }),
                            command: None,
                            is_preferred: None,
                            disabled: None,
                            data: None,
                        }));
                    }
                }
            }
        }
    }

    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

/// The range to delete to take an attribute out of its `#[ … ]` list: the
/// attribute itself and one separating comma, so what is left is still a list
/// (`#[a: 1, b: 2]` without `b` is `#[a: 1]`, not `#[a: 1, ]`).
fn attribute_removal_range(doc: &Document, range: Range) -> Range {
    let text = doc.content.as_str();
    let (Some(start), Some(end)) = (
        doc.line_index.position_to_offset(text, range.start),
        doc.line_index.position_to_offset(text, range.end),
    ) else {
        return range;
    };
    let after = &text[end..];
    let trimmed = after.trim_start_matches([' ', '\t']);
    let (start, end) = if let Some(rest) = trimmed.strip_prefix(',') {
        // A comma follows: take it and the blanks after it.
        let gap = after.len() - trimmed.len();
        let blanks = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        (start, end + gap + 1 + blanks)
    } else {
        // The last in its list: take the comma before it instead.
        let before = &text[..start];
        let kept = before.trim_end_matches([' ', '\t']);
        match kept.strip_suffix(',') {
            Some(head) => (head.len(), end),
            None => (start, end),
        }
    };
    Range::new(
        doc.line_index.offset_to_position(text, start),
        doc.line_index.offset_to_position(text, end),
    )
}
