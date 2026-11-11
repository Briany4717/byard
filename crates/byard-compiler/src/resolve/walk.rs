//! A visit of every expression in a view, for rewrites the resolver makes
//! after parsing (a package's function calls to their canonical names).
//!
//! Exhaustive on purpose, with no wildcard arm, like the span rebase next to
//! it: a new expression form has to say how it is walked, or it does not
//! compile, so a call written inside it can never be skipped in silence.

use crate::parser::ast::{
    Arg, Attr, AttrKind, ElementNode, Expr, Member, Param, StrPart, Type, ViewDecl,
};

/// Calls `f` on every expression of `member`, children before parents (a
/// call's arguments are visited before the call itself).
pub(crate) fn member_exprs(member: &mut Member, f: &mut dyn FnMut(&mut Expr)) {
    match member {
        Member::Var { init, .. } | Member::Let { init, .. } => expr(init, f),
        Member::Fn { params, body, .. } => {
            for p in params {
                if let Some(default) = &mut p.default {
                    expr(default, f);
                }
            }
            expr(body, f);
        }
        Member::Inject { .. } => {}
        Member::Element(el) => element(el, f),
        Member::For { iter, body, .. } => {
            expr(iter, f);
            for m in body {
                member_exprs(m, f);
            }
        }
        Member::When {
            cond, then, els, ..
        } => {
            expr(cond, f);
            for m in then {
                member_exprs(m, f);
            }
            if let Some(els) = els {
                for m in els {
                    member_exprs(m, f);
                }
            }
        }
        Member::Route { body, .. } => {
            for m in body {
                member_exprs(m, f);
            }
        }
        Member::Style { rules, .. } => {
            for rule in rules {
                for a in &mut rule.attrs {
                    attr(a, f);
                }
            }
        }
        Member::Timer { action, .. }
        | Member::Lifecycle { action, .. }
        | Member::Measure { action, .. } => expr(action, f),
        Member::Change {
            watched, action, ..
        } => {
            for value in watched {
                expr(value, f);
            }
            expr(action, f);
        }
        Member::Expr(e) => expr(e, f),
    }
}

fn element(el: &mut ElementNode, f: &mut dyn FnMut(&mut Expr)) {
    for a in &mut el.content {
        arg(a, f);
    }
    for a in &mut el.attrs {
        attr(a, f);
    }
    if let Some(action) = &mut el.action {
        expr(action, f);
    }
    for child in &mut el.children {
        member_exprs(child, f);
    }
}

fn attr(a: &mut Attr, f: &mut dyn FnMut(&mut Expr)) {
    match &mut a.kind {
        AttrKind::Prop { value } | AttrKind::Spread { value } => expr(value, f),
        AttrKind::Event { action, .. } => expr(action, f),
    }
}

fn arg(a: &mut Arg, f: &mut dyn FnMut(&mut Expr)) {
    expr(&mut a.value, f);
}

/// Calls `f` on `e` and every expression inside it, children first.
pub(crate) fn expr(e: &mut Expr, f: &mut dyn FnMut(&mut Expr)) {
    match e {
        Expr::IntLit(..)
        | Expr::FloatLit(..)
        | Expr::AngleLit(..)
        | Expr::Ident(..)
        | Expr::ClassRef(..)
        | Expr::Error(_) => {}
        Expr::StrLit(parts, _) => {
            for part in parts {
                if let StrPart::Interp(inner, _) = part {
                    expr(inner, f);
                }
            }
        }
        Expr::Array(items, _) | Expr::Block(items, _) => {
            for item in items {
                expr(item, f);
            }
        }
        Expr::Tuple(args, _) => {
            for a in args {
                arg(a, f);
            }
        }
        Expr::SlotBlock { members, .. } => {
            for m in members {
                member_exprs(m, f);
            }
        }
        Expr::Member { base, .. } => expr(base, f),
        Expr::Call { callee, args, .. } => {
            expr(callee, f);
            for a in args {
                arg(a, f);
            }
        }
        Expr::Lambda { body, .. } => expr(body, f),
        Expr::Assign { target, value, .. } => {
            expr(target, f);
            expr(value, f);
        }
        Expr::Postfix { target, .. } => expr(target, f),
        Expr::Binary { lhs, rhs, .. } => {
            expr(lhs, f);
            expr(rhs, f);
        }
        Expr::Unary { rhs, .. } => expr(rhs, f),
        Expr::Index { base, index, .. } => {
            expr(base, f);
            expr(index, f);
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                expr(value, f);
            }
            if let Some(spread) = spread {
                expr(spread, f);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            expr(cond, f);
            expr(then, f);
            if let Some(els) = els {
                expr(els, f);
            }
        }
        Expr::Ternary {
            cond, then, els, ..
        } => {
            expr(cond, f);
            expr(then, f);
            expr(els, f);
        }
        Expr::Animated { value, anim, .. } => {
            expr(value, f);
            expr(anim, f);
        }
        Expr::StyleValue { attrs, states, .. } => {
            for a in attrs {
                attr(a, f);
            }
            for state in states {
                for a in &mut state.attrs {
                    attr(a, f);
                }
            }
        }
        Expr::Merge { left, right, .. } => {
            expr(left, f);
            expr(right, f);
        }
        Expr::KeyframeStep { value, .. } | Expr::Let { value, .. } => expr(value, f),
        Expr::ControllerCall { call, ok, err, .. } => {
            expr(call, f);
            for a in [ok, err].into_iter().flatten() {
                expr(&mut a.action, f);
            }
        }
    }
    f(e);
}

/// Calls `f` on every type annotation of `view` (a type's own arguments are
/// visited after it): parameters, `var`/`let`/`fn`/`inject` declarations at any
/// depth, and the lambda parameters and `let` statements inside expressions.
pub(crate) fn view_types(view: &mut ViewDecl, f: &mut dyn FnMut(&mut Type)) {
    for p in &mut view.params {
        param_types(p, f);
    }
    for m in &mut view.body {
        member_types(m, f);
    }
}

/// [`view_types`] for one parameter, including its default's expression.
pub(crate) fn param_types(p: &mut Param, f: &mut dyn FnMut(&mut Type)) {
    if let Some(t) = &mut p.ty {
        ty(t, f);
    }
    if let Some(default) = &mut p.default {
        expr(default, &mut |e| expr_types(e, f));
    }
}

/// Calls `f` on the types written inside one expression node (not its
/// children, which a walk of the expression reaches itself).
pub(crate) fn expr_types(e: &mut Expr, f: &mut dyn FnMut(&mut Type)) {
    match e {
        Expr::Lambda { types, .. } => {
            for t in types.iter_mut().flatten() {
                ty(t, f);
            }
        }
        Expr::Let { ty: Some(t), .. } => ty(t, f),
        _ => {}
    }
}

/// Calls `f` on `t` and the types inside it.
pub(crate) fn ty(t: &mut Type, f: &mut dyn FnMut(&mut Type)) {
    f(t);
    match t {
        Type::Named { args, .. } => {
            for a in args {
                ty(a, f);
            }
        }
        Type::Function { params, ret, .. } => {
            for p in params {
                ty(p, f);
            }
            if let Some(r) = ret {
                ty(r, f);
            }
        }
    }
}

fn member_types(m: &mut Member, f: &mut dyn FnMut(&mut Type)) {
    member_exprs(m, &mut |e| expr_types(e, f));
    declared_types(m, f);
}

fn declared_types(m: &mut Member, f: &mut dyn FnMut(&mut Type)) {
    match m {
        Member::Var { ty: Some(t), .. }
        | Member::Let { ty: Some(t), .. }
        | Member::Inject { ty: t, .. } => ty(t, f),
        Member::Fn { params, ret, .. } => {
            for p in params {
                if let Some(t) = &mut p.ty {
                    ty(t, f);
                }
            }
            if let Some(t) = ret {
                ty(t, f);
            }
        }
        Member::Element(el) => {
            for c in &mut el.children {
                declared_types(c, f);
            }
        }
        Member::For { body, .. } | Member::Route { body, .. } => {
            for c in body {
                declared_types(c, f);
            }
        }
        Member::When { then, els, .. } => {
            for c in then {
                declared_types(c, f);
            }
            if let Some(els) = els {
                for c in els {
                    declared_types(c, f);
                }
            }
        }
        _ => {}
    }
}
