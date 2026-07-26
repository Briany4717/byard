//! Identifier renaming capability for safe refactoring across `.byd` files.

use std::collections::HashMap;

use byard_compiler::diagnostics::Span;
use byard_compiler::parser::ast::{AttrKind, ElementNode, Expr, Member};
use lsp_types::{
    Position, PrepareRenameResponse, TextEdit, WorkspaceEdit,
};

use crate::state::document::Document;
use crate::syntax::ast_utils::{find_hover_target, span_contains, HoverTarget};

/// Handles `textDocument/prepareRename` to check if a symbol at position can be renamed.
#[must_use]
pub fn handle_prepare_rename(doc: &Document, pos: Position) -> Option<PrepareRenameResponse> {
    let offset = doc.line_index.position_to_offset(&doc.content, pos)?;
    let target = find_hover_target(&doc.parsed.views, offset)?;

    match target {
        HoverTarget::VarIdent { span, .. } => {
            let range = doc.line_index.span_to_range(&doc.content, span);
            Some(PrepareRenameResponse::Range(range))
        }
        HoverTarget::Intrinsic { .. } | HoverTarget::Attribute { .. } => None,
    }
}

/// Handles `textDocument/rename` to safely rename an identifier across the document.
#[must_use]
pub fn handle_rename(
    doc: &Document,
    pos: Position,
    new_name: String,
) -> Option<WorkspaceEdit> {
    let offset = doc.line_index.position_to_offset(&doc.content, pos)?;
    let target = find_hover_target(&doc.parsed.views, offset)?;

    let HoverTarget::VarIdent { name: old_name, .. } = target else {
        return None;
    };

    let enclosing_view = doc
        .parsed
        .views
        .iter()
        .find(|v| span_contains(v.span, offset))?;

    let mut occurrences = Vec::new();

    // 1. Check parameters of the enclosing view
    for param in &enclosing_view.params {
        if param.name.as_str() == old_name {
            let name_span = Span::new(
                param.span.start,
                param.span.start + old_name.len() as u32,
            );
            occurrences.push(name_span);
        }
    }

    // 2. Check body members and expressions in the view
    collect_name_spans_in_members(&enclosing_view.body, &old_name, &mut occurrences);

    if occurrences.is_empty() {
        return None;
    }

    let edits: Vec<TextEdit> = occurrences
        .into_iter()
        .map(|span| TextEdit {
            range: doc.line_index.span_to_range(&doc.content, span),
            new_text: new_name.clone(),
        })
        .collect();

    let mut changes = HashMap::new();
    changes.insert(doc.uri.clone(), edits);

    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

fn collect_name_spans_in_members(
    members: &[Member],
    target_name: &str,
    out: &mut Vec<Span>,
) {
    for member in members {
        match member {
            Member::Var { name, init, span, .. } | Member::Let { name, init, span, .. } => {
                if name.as_str() == target_name {
                    out.push(Span::new(span.start, span.start + name.as_str().len() as u32));
                }
                collect_name_spans_in_expr(init, target_name, out);
            }
            Member::Fn {
                name,
                params,
                body,
                span,
                ..
            } => {
                if name.as_str() == target_name {
                    out.push(Span::new(span.start, span.start + name.as_str().len() as u32));
                }
                for param in params {
                    if param.name.as_str() == target_name {
                        out.push(Span::new(
                            param.span.start,
                            param.span.start + target_name.len() as u32,
                        ));
                    }
                }
                collect_name_spans_in_expr(body, target_name, out);
            }
            Member::Inject { name, span, .. } => {
                if name.as_str() == target_name {
                    out.push(Span::new(span.start, span.start + name.as_str().len() as u32));
                }
            }
            Member::Element(el) => {
                collect_name_spans_in_element(el, target_name, out);
            }
            Member::For {
                var, iter, body, span, ..
            } => {
                if var.as_str() == target_name {
                    out.push(Span::new(
                        span.start + 4,
                        span.start + 4 + var.as_str().len() as u32,
                    ));
                }
                collect_name_spans_in_expr(iter, target_name, out);
                collect_name_spans_in_members(body, target_name, out);
            }
            Member::Route { body, .. } => {
                collect_name_spans_in_members(body, target_name, out);
            }
            Member::When {
                cond,
                then,
                els,
                ..
            } => {
                collect_name_spans_in_expr(cond, target_name, out);
                collect_name_spans_in_members(then, target_name, out);
                if let Some(els_members) = els {
                    collect_name_spans_in_members(els_members, target_name, out);
                }
            }
            Member::Expr(expr) => {
                collect_name_spans_in_expr(expr, target_name, out);
            }
            Member::Style { .. } => {}
        }
    }
}

fn collect_name_spans_in_element(el: &ElementNode, target_name: &str, out: &mut Vec<Span>) {
    for arg in &el.content {
        collect_name_spans_in_expr(&arg.value, target_name, out);
    }
    for attr in &el.attrs {
        match &attr.kind {
            AttrKind::Prop { value } | AttrKind::Spread { value } => {
                collect_name_spans_in_expr(value, target_name, out);
            }
            AttrKind::Event { action, .. } => {
                collect_name_spans_in_expr(action, target_name, out);
            }
        }
    }
    if let Some(action) = &el.action {
        collect_name_spans_in_expr(action, target_name, out);
    }
    collect_name_spans_in_members(&el.children, target_name, out);
}

fn collect_name_spans_in_expr(expr: &Expr, target_name: &str, out: &mut Vec<Span>) {
    match expr {
        Expr::Ident(sym, span) if sym.as_str() == target_name => {
            out.push(*span);
        }
        Expr::Array(items, _) => {
            for item in items {
                collect_name_spans_in_expr(item, target_name, out);
            }
        }
        Expr::Tuple(args, _) => {
            for arg in args {
                collect_name_spans_in_expr(&arg.value, target_name, out);
            }
        }
        Expr::Member { base, field, span } => {
            collect_name_spans_in_expr(base, target_name, out);
            if field.as_str() == target_name {
                let field_start = span.end as usize - field.as_str().len();
                out.push(Span::new(field_start as u32, span.end));
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_name_spans_in_expr(callee, target_name, out);
            for arg in args {
                collect_name_spans_in_expr(&arg.value, target_name, out);
            }
        }
        Expr::Lambda { body, .. } => {
            collect_name_spans_in_expr(body, target_name, out);
        }
        Expr::Block(stmts, _) => {
            for stmt in stmts {
                collect_name_spans_in_expr(stmt, target_name, out);
            }
        }
        _ => {}
    }
}
