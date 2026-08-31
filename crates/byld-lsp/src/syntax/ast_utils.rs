//! Utility functions for AST inspection, span matching, and scope traversal.

use byard_compiler::diagnostics::Span;
use byard_compiler::parser::ast::{AttrKind, Expr, Member, StrPart, ViewDecl};

/// Represents the hover or definition target located under the cursor.
#[derive(Debug, Clone)]
pub enum HoverTarget {
    /// Intrinsic component (e.g., `Column`, `Button`).
    Intrinsic {
        /// Intrinsic component name.
        name: String,
    },
    /// Component attribute or property.
    Attribute {
        /// Parent element name.
        element_name: String,
        /// Attribute name.
        attr_name: String,
    },
    /// Variable or parameter identifier.
    VarIdent {
        /// Identifier name.
        name: String,
        /// Identifier span.
        span: Span,
    },
}

/// Checks if a byte offset falls strictly inside a Span.
#[must_use]
pub fn span_contains(span: Span, offset: usize) -> bool {
    offset >= span.start as usize && offset < span.end as usize
}

/// Finds an element name enclosing a given byte offset.
#[must_use]
pub fn find_element_at_offset(members: &[Member], offset: usize) -> Option<String> {
    for member in members {
        match member {
            Member::Element(el) if span_contains(el.span, offset) => {
                if let Some(child_name) = find_element_at_offset(&el.children, offset) {
                    return Some(child_name);
                }
                return Some(el.name.to_string());
            }
            Member::For { body, span, .. } | Member::Route { body, span, .. }
                if span_contains(*span, offset) =>
            {
                if let Some(name) = find_element_at_offset(body, offset) {
                    return Some(name);
                }
            }
            Member::When {
                then, els, span, ..
            } if span_contains(*span, offset) => {
                if let Some(name) = find_element_at_offset(then, offset) {
                    return Some(name);
                }
                if let Some(els_members) = els {
                    if let Some(name) = find_element_at_offset(els_members, offset) {
                        return Some(name);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Collects all local variables, memos, and helper functions in scope at offset.
pub fn collect_locals_in_members(
    members: &[Member],
    offset: usize,
    locals: &mut Vec<(String, lsp_types::CompletionItemKind, String)>,
) {
    for member in members {
        match member {
            Member::Var { name, .. } => {
                locals.push((
                    name.to_string(),
                    lsp_types::CompletionItemKind::VARIABLE,
                    "Variable (Signal)".to_string(),
                ));
            }
            Member::Let { name, .. } => {
                locals.push((
                    name.to_string(),
                    lsp_types::CompletionItemKind::VARIABLE,
                    "Computed Value (Memo)".to_string(),
                ));
            }
            Member::Fn { name, .. } => {
                locals.push((
                    name.to_string(),
                    lsp_types::CompletionItemKind::FUNCTION,
                    "Helper Function".to_string(),
                ));
            }
            Member::Inject { name, ty, .. } => {
                locals.push((
                    name.to_string(),
                    lsp_types::CompletionItemKind::VARIABLE,
                    format!("Injected Value: {ty:?}"),
                ));
            }
            Member::For {
                var, body, span, ..
            } if span_contains(*span, offset) => {
                locals.push((
                    var.to_string(),
                    lsp_types::CompletionItemKind::VARIABLE,
                    "Loop Item".to_string(),
                ));
                collect_locals_in_members(body, offset, locals);
            }
            Member::Route {
                params, body, span, ..
            } if span_contains(*span, offset) => {
                locals.push((
                    "route".to_string(),
                    lsp_types::CompletionItemKind::VARIABLE,
                    "Route".to_string(),
                ));
                if let Some(params) = params {
                    locals.push((
                        params.to_string(),
                        lsp_types::CompletionItemKind::VARIABLE,
                        "Route Params".to_string(),
                    ));
                }
                collect_locals_in_members(body, offset, locals);
            }
            Member::When {
                then, els, span, ..
            } if span_contains(*span, offset) => {
                collect_locals_in_members(then, offset, locals);
                if let Some(els_members) = els {
                    collect_locals_in_members(els_members, offset, locals);
                }
            }
            Member::Element(el) if span_contains(el.span, offset) => {
                collect_locals_in_members(&el.children, offset, locals);
            }
            _ => {}
        }
    }
}

/// Finds the AST target under cursor for hover or definition lookup.
#[must_use]
pub fn find_hover_target(views: &[ViewDecl], offset: usize) -> Option<HoverTarget> {
    for view in views {
        if !span_contains(view.span, offset) {
            continue;
        }
        for param in &view.params {
            if span_contains(param.span, offset) {
                return Some(HoverTarget::VarIdent {
                    name: param.name.to_string(),
                    span: param.span,
                });
            }
        }
        if let Some(target) = find_in_members(&view.body, offset, None) {
            return Some(target);
        }
    }
    None
}

/// Recursively traverses view members to find hover targets.
#[must_use]
pub fn find_in_members(
    members: &[Member],
    offset: usize,
    parent_element: Option<&str>,
) -> Option<HoverTarget> {
    for member in members {
        match member {
            Member::Var {
                name, init, span, ..
            } if span_contains(*span, offset) => {
                let name_start = span.start as usize + 4;
                let name_end = name_start + name.as_str().len();
                if offset >= name_start && offset < name_end {
                    return Some(HoverTarget::VarIdent {
                        name: name.to_string(),
                        span: Span::new(name_start as u32, name_end as u32),
                    });
                }
                return find_in_expr(init, offset);
            }
            Member::Let {
                name, init, span, ..
            } if span_contains(*span, offset) => {
                let name_start = span.start as usize + 4;
                let name_end = name_start + name.as_str().len();
                if offset >= name_start && offset < name_end {
                    return Some(HoverTarget::VarIdent {
                        name: name.to_string(),
                        span: Span::new(name_start as u32, name_end as u32),
                    });
                }
                return find_in_expr(init, offset);
            }
            Member::Fn {
                name,
                params,
                body,
                span,
                ..
            } if span_contains(*span, offset) => {
                let name_start = span.start as usize + 3;
                let name_end = name_start + name.as_str().len();
                if offset >= name_start && offset < name_end {
                    return Some(HoverTarget::VarIdent {
                        name: name.to_string(),
                        span: Span::new(name_start as u32, name_end as u32),
                    });
                }
                for param in params {
                    if span_contains(param.span, offset) {
                        return Some(HoverTarget::VarIdent {
                            name: param.name.to_string(),
                            span: param.span,
                        });
                    }
                }
                if span_contains(body.span(), offset) {
                    return find_in_expr(body, offset);
                }
            }
            Member::Element(el) if span_contains(el.span, offset) => {
                let name_start = el.span.start as usize;
                let name_end = name_start + el.name.as_str().len();
                if offset >= name_start && offset < name_end {
                    return Some(HoverTarget::Intrinsic {
                        name: el.name.to_string(),
                    });
                }
                for arg in &el.content {
                    if let Some(target) = find_in_expr(&arg.value, offset) {
                        return Some(target);
                    }
                }
                for attr in &el.attrs {
                    if span_contains(attr.span, offset) {
                        let attr_name_start = attr.span.start as usize;
                        let attr_name_end = attr_name_start + attr.name.as_str().len();
                        if offset >= attr_name_start && offset < attr_name_end {
                            return Some(HoverTarget::Attribute {
                                element_name: el.name.to_string(),
                                attr_name: attr.name.to_string(),
                            });
                        }
                        match &attr.kind {
                            AttrKind::Prop { value } | AttrKind::Spread { value } => {
                                if let Some(target) = find_in_expr(value, offset) {
                                    return Some(target);
                                }
                            }
                            AttrKind::Event { action, .. } => {
                                if let Some(target) = find_in_expr(action, offset) {
                                    return Some(target);
                                }
                            }
                        }
                    }
                }
                if let Some(action) = &el.action {
                    if let Some(target) = find_in_expr(action, offset) {
                        return Some(target);
                    }
                }
                if let Some(target) = find_in_members(&el.children, offset, Some(el.name.as_str()))
                {
                    return Some(target);
                }
            }
            Member::For {
                var,
                index,
                iter,
                body,
                span,
            } if span_contains(*span, offset) => {
                let var_start =
                    span.start as usize + 4 + index.as_ref().map_or(0, |i| i.as_str().len() + 2);
                let var_end = var_start + var.as_str().len();
                if offset >= var_start && offset < var_end {
                    return Some(HoverTarget::VarIdent {
                        name: var.to_string(),
                        span: Span::new(var_start as u32, var_end as u32),
                    });
                }
                if let Some(target) = find_in_expr(iter, offset) {
                    return Some(target);
                }
                if let Some(target) = find_in_members(body, offset, parent_element) {
                    return Some(target);
                }
            }
            Member::Route { body, span, .. } if span_contains(*span, offset) => {
                if let Some(target) = find_in_members(body, offset, parent_element) {
                    return Some(target);
                }
            }
            Member::When {
                cond,
                then,
                els,
                span,
            } if span_contains(*span, offset) => {
                if let Some(target) = find_in_expr(cond, offset) {
                    return Some(target);
                }
                if let Some(target) = find_in_members(then, offset, parent_element) {
                    return Some(target);
                }
                if let Some(els_members) = els {
                    if let Some(target) = find_in_members(els_members, offset, parent_element) {
                        return Some(target);
                    }
                }
            }
            Member::Expr(expr) => {
                if let Some(target) = find_in_expr(expr, offset) {
                    return Some(target);
                }
            }
            _ => {}
        }
    }
    None
}

/// Recursively traverses expressions to find hover targets.
#[must_use]
pub fn find_in_expr(expr: &Expr, offset: usize) -> Option<HoverTarget> {
    if !span_contains(expr.span(), offset) {
        return None;
    }
    match expr {
        Expr::Ident(sym, span) => Some(HoverTarget::VarIdent {
            name: sym.to_string(),
            span: *span,
        }),
        Expr::Array(items, _) => {
            for item in items {
                if let Some(target) = find_in_expr(item, offset) {
                    return Some(target);
                }
            }
            None
        }
        Expr::Tuple(args, _) => {
            for arg in args {
                if let Some(target) = find_in_expr(&arg.value, offset) {
                    return Some(target);
                }
            }
            None
        }
        Expr::Member { base, field, span } => {
            if span_contains(base.span(), offset) {
                return find_in_expr(base, offset);
            }
            let field_start = span.end as usize - field.as_str().len();
            if offset >= field_start && offset < span.end as usize {
                return Some(HoverTarget::VarIdent {
                    name: field.to_string(),
                    span: Span::new(field_start as u32, span.end),
                });
            }
            None
        }
        Expr::Call { callee, args, .. } => {
            if span_contains(callee.span(), offset) {
                return find_in_expr(callee, offset);
            }
            for arg in args {
                if span_contains(arg.value.span(), offset) {
                    return find_in_expr(&arg.value, offset);
                }
            }
            None
        }
        Expr::Lambda { body, .. } => {
            if span_contains(body.span(), offset) {
                return find_in_expr(body, offset);
            }
            None
        }
        Expr::Block(stmts, _) => {
            for stmt in stmts {
                if span_contains(stmt.span(), offset) {
                    return find_in_expr(stmt, offset);
                }
            }
            None
        }
        Expr::StrLit(parts, _) => {
            for part in parts {
                if let StrPart::Interp(expr) = part {
                    if let Some(target) = find_in_expr(expr, offset) {
                        return Some(target);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Finds an element reference at the specified offset across views.
#[must_use]
pub fn find_element_ref_at_offset(views: &[ViewDecl], offset: usize) -> Option<String> {
    for view in views {
        if !span_contains(view.span, offset) {
            continue;
        }
        if let Some(name) = find_element_ref_in_members(&view.body, offset) {
            return Some(name);
        }
    }
    None
}

/// Finds an element reference at offset in members.
#[must_use]
pub fn find_element_ref_in_members(members: &[Member], offset: usize) -> Option<String> {
    for member in members {
        match member {
            Member::Element(el) => {
                let name_len = el.name.as_str().len();
                let name_span = Span::new(el.span.start, el.span.start + name_len as u32);
                if span_contains(name_span, offset) {
                    return Some(el.name.to_string());
                }
                if let Some(name) = find_element_ref_in_members(&el.children, offset) {
                    return Some(name);
                }
            }
            Member::For { body, span, .. } | Member::Route { body, span, .. }
                if span_contains(*span, offset) =>
            {
                if let Some(name) = find_element_ref_in_members(body, offset) {
                    return Some(name);
                }
            }
            Member::When {
                then, els, span, ..
            } if span_contains(*span, offset) => {
                if let Some(name) = find_element_ref_in_members(then, offset) {
                    return Some(name);
                }
                if let Some(els_members) = els {
                    if let Some(name) = find_element_ref_in_members(els_members, offset) {
                        return Some(name);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Finds the declaration span of a local variable name.
#[must_use]
pub fn find_local_declaration_span(
    members: &[Member],
    var_name: &str,
    offset: usize,
) -> Option<Span> {
    for member in members {
        match member {
            Member::Var { name, span, .. } if name.as_str() == var_name => {
                return Some(*span);
            }
            Member::Let { name, span, .. } if name.as_str() == var_name => {
                return Some(*span);
            }
            Member::Fn { name, span, .. } if name.as_str() == var_name => {
                return Some(*span);
            }
            Member::Inject { name, span, .. } if name.as_str() == var_name => {
                return Some(*span);
            }
            Member::For {
                var, body, span, ..
            } if span_contains(*span, offset) => {
                if var.as_str() == var_name {
                    return Some(Span::new(
                        span.start,
                        span.start + 4 + var.as_str().len() as u32,
                    ));
                }
                if let Some(s) = find_local_declaration_span(body, var_name, offset) {
                    return Some(s);
                }
            }
            Member::Route { body, span, .. } if span_contains(*span, offset) => {
                if let Some(s) = find_local_declaration_span(body, var_name, offset) {
                    return Some(s);
                }
            }
            Member::When {
                then, els, span, ..
            } if span_contains(*span, offset) => {
                if let Some(s) = find_local_declaration_span(then, var_name, offset) {
                    return Some(s);
                }
                if let Some(els_members) = els {
                    if let Some(s) = find_local_declaration_span(els_members, var_name, offset) {
                        return Some(s);
                    }
                }
            }
            Member::Element(el) if span_contains(el.span, offset) => {
                if let Some(s) = find_local_declaration_span(&el.children, var_name, offset) {
                    return Some(s);
                }
            }
            _ => {}
        }
    }
    None
}

/// Finds style class reference at offset across views.
#[must_use]
pub fn find_class_ref_at_offset(views: &[ViewDecl], offset: usize) -> Option<String> {
    for view in views {
        if !span_contains(view.span, offset) {
            continue;
        }
        if let Some(name) = find_class_ref_in_members(&view.body, offset) {
            return Some(name);
        }
    }
    None
}

/// Finds style class reference in members.
#[must_use]
pub fn find_class_ref_in_members(members: &[Member], offset: usize) -> Option<String> {
    for member in members {
        match member {
            Member::Element(el) => {
                for attr in &el.attrs {
                    match &attr.kind {
                        AttrKind::Prop { value } | AttrKind::Spread { value } => {
                            if let Some(name) = find_class_ref_in_expr(value, offset) {
                                return Some(name);
                            }
                        }
                        AttrKind::Event { action, .. } => {
                            if let Some(name) = find_class_ref_in_expr(action, offset) {
                                return Some(name);
                            }
                        }
                    }
                }
                if let Some(action) = &el.action {
                    if let Some(name) = find_class_ref_in_expr(action, offset) {
                        return Some(name);
                    }
                }
                if let Some(name) = find_class_ref_in_members(&el.children, offset) {
                    return Some(name);
                }
            }
            Member::Var { init, .. } | Member::Let { init, .. } => {
                if let Some(name) = find_class_ref_in_expr(init, offset) {
                    return Some(name);
                }
            }
            Member::Fn { body, .. } => {
                if let Some(name) = find_class_ref_in_expr(body, offset) {
                    return Some(name);
                }
            }
            Member::For {
                iter, body, span, ..
            } if span_contains(*span, offset) => {
                if let Some(name) = find_class_ref_in_expr(iter, offset) {
                    return Some(name);
                }
                if let Some(name) = find_class_ref_in_members(body, offset) {
                    return Some(name);
                }
            }
            Member::Route { body, span, .. } if span_contains(*span, offset) => {
                if let Some(name) = find_class_ref_in_members(body, offset) {
                    return Some(name);
                }
            }
            Member::When {
                cond,
                then,
                els,
                span,
            } if span_contains(*span, offset) => {
                if let Some(name) = find_class_ref_in_expr(cond, offset) {
                    return Some(name);
                }
                if let Some(name) = find_class_ref_in_members(then, offset) {
                    return Some(name);
                }
                if let Some(els_members) = els {
                    if let Some(name) = find_class_ref_in_members(els_members, offset) {
                        return Some(name);
                    }
                }
            }
            Member::Expr(expr) => {
                if let Some(name) = find_class_ref_in_expr(expr, offset) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Finds style class reference in expressions.
#[must_use]
pub fn find_class_ref_in_expr(expr: &Expr, offset: usize) -> Option<String> {
    if !span_contains(expr.span(), offset) {
        return None;
    }
    match expr {
        Expr::ClassRef(sym, span) => {
            if span_contains(*span, offset) {
                return Some(sym.as_str().to_string());
            }
            None
        }
        Expr::Array(items, _) => {
            for item in items {
                if let Some(name) = find_class_ref_in_expr(item, offset) {
                    return Some(name);
                }
            }
            None
        }
        Expr::Tuple(args, _) => {
            for arg in args {
                if let Some(name) = find_class_ref_in_expr(&arg.value, offset) {
                    return Some(name);
                }
            }
            None
        }
        Expr::Member { base, .. } => find_class_ref_in_expr(base, offset),
        Expr::Call { callee, args, .. } => {
            if let Some(name) = find_class_ref_in_expr(callee, offset) {
                return Some(name);
            }
            for arg in args {
                if let Some(name) = find_class_ref_in_expr(&arg.value, offset) {
                    return Some(name);
                }
            }
            None
        }
        Expr::Lambda { body, .. } => find_class_ref_in_expr(body, offset),
        Expr::Block(stmts, _) => {
            for stmt in stmts {
                if let Some(name) = find_class_ref_in_expr(stmt, offset) {
                    return Some(name);
                }
            }
            None
        }
        Expr::Assign { target, value, .. } => {
            if let Some(name) = find_class_ref_in_expr(target, offset) {
                return Some(name);
            }
            find_class_ref_in_expr(value, offset)
        }
        Expr::Postfix { target, .. } => find_class_ref_in_expr(target, offset),
        Expr::Ternary {
            cond, then, els, ..
        } => {
            if let Some(name) = find_class_ref_in_expr(cond, offset) {
                return Some(name);
            }
            if let Some(name) = find_class_ref_in_expr(then, offset) {
                return Some(name);
            }
            find_class_ref_in_expr(els, offset)
        }
        _ => None,
    }
}
