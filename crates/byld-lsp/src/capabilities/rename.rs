//! Identifier renaming capability for safe refactoring across `.byd` files.

use std::collections::HashMap;

use byard_compiler::diagnostics::Span;
use byard_compiler::parser::ast::{AttrKind, ElementNode, Expr, Member, StrPart};
use lsp_types::{Position, PrepareRenameResponse, TextEdit, WorkspaceEdit};

use crate::state::document::Document;

/// `"var "` and `"let "` are both four characters, which is how far past a
/// member's span start its declared name begins.
const KEYWORD_PREFIX: u32 = 4;
use crate::syntax::ast_utils::{HoverTarget, find_hover_target, span_contains};

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
pub fn handle_rename(doc: &Document, pos: Position, new_name: String) -> Option<WorkspaceEdit> {
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
            let name_span = Span::new(param.span.start, param.span.start + old_name.len() as u32);
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

fn collect_name_spans_in_members(members: &[Member], target_name: &str, out: &mut Vec<Span>) {
    for member in members {
        match member {
            Member::Var {
                name, init, span, ..
            }
            | Member::Let {
                name, init, span, ..
            } => {
                if name.as_str() == target_name {
                    // `span` starts at the `var`/`let` keyword, so the name
                    // begins four characters in. Anchoring at `span.start`
                    // rewrote "var c" instead of "count".
                    let name_start = span.start + KEYWORD_PREFIX;
                    out.push(Span::new(
                        name_start,
                        name_start + name.as_str().len() as u32,
                    ));
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
                    out.push(Span::new(
                        span.start,
                        span.start + name.as_str().len() as u32,
                    ));
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
                    out.push(Span::new(
                        span.start,
                        span.start + name.as_str().len() as u32,
                    ));
                }
            }
            Member::Element(el) => {
                collect_name_spans_in_element(el, target_name, out);
            }
            Member::For {
                var,
                iter,
                body,
                span,
                ..
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
                cond, then, els, ..
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
            Member::Lifecycle { action, .. }
            | Member::Timer { action, .. }
            | Member::Measure { action, .. } => {
                collect_name_spans_in_expr(action, target_name, out);
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
        collect_name_spans_in_attr(attr, target_name, out);
    }
    if let Some(action) = &el.action {
        collect_name_spans_in_expr(action, target_name, out);
    }
    collect_name_spans_in_members(&el.children, target_name, out);
}

/// An attribute's value or action body, wherever an attribute appears: on an
/// element, or inside a first-class `Style` value's attrs and state blocks.
fn collect_name_spans_in_attr(
    attr: &byard_compiler::parser::ast::Attr,
    target_name: &str,
    out: &mut Vec<Span>,
) {
    match &attr.kind {
        AttrKind::Prop { value } | AttrKind::Spread { value } => {
            collect_name_spans_in_expr(value, target_name, out);
        }
        AttrKind::Event { action, .. } => {
            collect_name_spans_in_expr(action, target_name, out);
        }
    }
}

fn collect_name_spans_in_expr(expr: &Expr, target_name: &str, out: &mut Vec<Span>) {
    // Exhaustive on purpose. A catch-all arm here is invisible: a rename that
    // silently skips an expression kind rewrites every *other* mention of the
    // binding and leaves that one behind, which corrupts the file rather than
    // failing. A new `Expr` variant must break this match, not slip past it.
    match expr {
        Expr::Ident(sym, span) => {
            if sym.as_str() == target_name {
                out.push(*span);
            }
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
        Expr::ControllerCall { call, ok, err, .. } => {
            collect_name_spans_in_expr(call, target_name, out);
            for arm in [ok, err].into_iter().flatten() {
                collect_name_spans_in_expr(&arm.action, target_name, out);
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
        // An l-value is a mention like any other: `count = 1` and `count++`
        // must be rewritten with the declaration.
        Expr::Assign { target, value, .. } => {
            collect_name_spans_in_expr(target, target_name, out);
            collect_name_spans_in_expr(value, target_name, out);
        }
        Expr::Postfix { target, .. } => {
            collect_name_spans_in_expr(target, target_name, out);
        }
        Expr::Unary { rhs, .. } => {
            collect_name_spans_in_expr(rhs, target_name, out);
        }
        Expr::Index { base, index, .. } => {
            collect_name_spans_in_expr(base, target_name, out);
            collect_name_spans_in_expr(index, target_name, out);
        }
        Expr::Record { fields, spread, .. } => {
            // Field *keys* are the record's own names, not the binding's, so
            // only the values (and any spread base) are renameable.
            for (_, value) in fields {
                collect_name_spans_in_expr(value, target_name, out);
            }
            if let Some(base) = spread {
                collect_name_spans_in_expr(base, target_name, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_name_spans_in_expr(lhs, target_name, out);
            collect_name_spans_in_expr(rhs, target_name, out);
        }
        Expr::Ternary {
            cond, then, els, ..
        } => {
            collect_name_spans_in_expr(cond, target_name, out);
            collect_name_spans_in_expr(then, target_name, out);
            collect_name_spans_in_expr(els, target_name, out);
        }
        Expr::Animated { value, anim, .. } => {
            collect_name_spans_in_expr(value, target_name, out);
            collect_name_spans_in_expr(anim, target_name, out);
        }
        Expr::KeyframeStep { value, .. } => {
            collect_name_spans_in_expr(value, target_name, out);
        }
        Expr::StyleValue { attrs, states, .. } => {
            for attr in attrs {
                collect_name_spans_in_attr(attr, target_name, out);
            }
            for state in states {
                for attr in &state.attrs {
                    collect_name_spans_in_attr(attr, target_name, out);
                }
            }
        }
        Expr::Merge { left, right, .. } => {
            collect_name_spans_in_expr(left, target_name, out);
            collect_name_spans_in_expr(right, target_name, out);
        }
        // An interpolated string is the commonest mention of all: a
        // `Text("{count}")` reads the binding just as much as `count + 1` does.
        Expr::StrLit(parts, _) => {
            for part in parts {
                if let StrPart::Interp(inner) = part {
                    collect_name_spans_in_expr(inner, target_name, out);
                }
            }
        }
        // Nothing inside these can name a binding.
        Expr::IntLit(..)
        | Expr::FloatLit(..)
        | Expr::AngleLit(..)
        | Expr::ClassRef(..)
        | Expr::Error(..) => {}
    }
}
