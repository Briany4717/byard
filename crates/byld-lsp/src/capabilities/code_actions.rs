//! Code actions capability providing quick-fixes and automated refactorings.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    TextEdit, WorkspaceEdit,
};

use crate::state::document::Document;

/// Handles `textDocument/codeAction` request to generate automated quick-fixes.
#[must_use]
pub fn handle_code_action(doc: &Document, params: CodeActionParams) -> Option<CodeActionResponse> {
    let mut actions = Vec::new();

    // 1. Process diagnostic-driven quick fixes
    for diagnostic in &params.context.diagnostics {
        let msg = &diagnostic.message;

        if msg.contains("unknown property") || msg.contains("invalid attribute") {
            let edit = TextEdit {
                range: diagnostic.range,
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
                if let byard_compiler::parser::ast::Member::Var {
                    name, span, init, ..
                } = member
                {
                    if crate::syntax::ast_utils::span_contains(*span, offset) {
                        let var_range = doc.line_index.span_to_range(&doc.content, *span);
                        let let_replacement = format!("let {name} = {}", format_expr(init));
                        let edit = TextEdit {
                            range: var_range,
                            new_text: let_replacement,
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

fn format_expr(expr: &byard_compiler::parser::ast::Expr) -> String {
    match expr {
        byard_compiler::parser::ast::Expr::IntLit(val, _) => val.to_string(),
        byard_compiler::parser::ast::Expr::FloatLit(val, _) => val.to_string(),
        byard_compiler::parser::ast::Expr::Ident(sym, _) => sym.as_str().to_string(),
        _ => "0".to_string(),
    }
}
