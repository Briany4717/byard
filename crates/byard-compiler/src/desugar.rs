//! `match`, written as what the interpreter already runs (the one pass after
//! the checks, so that the checker sees what was written and nothing else does
//! have to know a new form exists).
//!
//! A `match` over a value is a chain of comparisons with it: as a view member a
//! chain of `when … else when …`, as an expression a chain of `? :`. An enum
//! case is the text of its name at run time, so `.idle` is a comparison with
//! `"idle"`. What the chain does when no arm holds is nothing, which the
//! checker has already said is not so for an enum (`NonExhaustive`).

use std::sync::Arc;

use crate::diagnostics::Span;
use crate::parser::ast::{Arm, BinOp, ConstDecl, Expr, FnDecl, Member, Pattern, StrPart, ViewDecl};
use crate::resolve::walk;
use crate::symbol::Symbol;

/// Rewrites every `match` of `views`, and of the functions and constants they
/// share, as the chain it means.
pub(crate) fn matches(views: &mut [ViewDecl]) {
    let (mut helpers, mut consts): (Vec<FnDecl>, Vec<ConstDecl>) = views
        .first()
        .map(|v| ((*v.helpers).clone(), (*v.consts).clone()))
        .unwrap_or_default();
    for decl in &mut helpers {
        in_expr(&mut decl.body);
        for p in &mut decl.params {
            if let Some(d) = &mut p.default {
                in_expr(d);
            }
        }
    }
    for decl in &mut consts {
        in_expr(&mut decl.init);
    }
    let helpers = Arc::new(helpers);
    let consts = Arc::new(consts);
    for view in views {
        in_members(&mut view.body);
        for p in &mut view.params {
            if let Some(d) = &mut p.default {
                in_expr(d);
            }
        }
        view.helpers = Arc::clone(&helpers);
        view.consts = Arc::clone(&consts);
    }
}

/// Every `match` expression inside `expr`, and every list of members a slot
/// block holds.
fn in_expr(expr: &mut Expr) {
    walk::expr(expr, &mut |e| rewrite(e));
}

fn rewrite(e: &mut Expr) {
    if matches!(e, Expr::Match { .. }) {
        let whole = std::mem::replace(e, Expr::Error(Span::new(0, 0)));
        if let Expr::Match {
            scrutinee,
            arms,
            span,
        } = whole
        {
            *e = chain_expr(&scrutinee, arms, span);
        }
        return;
    }
    match e {
        Expr::SlotBlock { members, .. }
        | Expr::Lambda {
            slot: Some(members),
            ..
        } => in_members(members),
        _ => {}
    }
}

/// Every `match` of a list of members, at any depth.
fn in_members(members: &mut Vec<Member>) {
    for member in members.iter_mut() {
        walk::member_exprs(member, &mut |e| rewrite(e));
        match member {
            Member::Element(el) => in_members(&mut el.children),
            Member::For { body, .. } | Member::Route { body, .. } => in_members(body),
            Member::When { then, els, .. } => {
                in_members(then);
                if let Some(els) = els {
                    in_members(els);
                }
            }
            Member::Match { arms, .. } => {
                for arm in arms {
                    in_members(&mut arm.body);
                }
            }
            _ => {}
        }
    }
    for member in std::mem::take(members) {
        members.push(match member {
            Member::Match {
                scrutinee,
                arms,
                span,
            } => chain_member(&scrutinee, arms, span),
            other => other,
        });
    }
}

/// The condition that makes `pattern` the arm to take, `None` for `else`.
fn test(scrutinee: &Expr, pattern: &Pattern) -> Option<Expr> {
    let (value, at) = match pattern {
        Pattern::Else(_) => return None,
        Pattern::Case { name, span, .. } => (
            Expr::StrLit(vec![StrPart::Text(name.as_str().to_string())], *span),
            *span,
        ),
        Pattern::Literal(e) => (e.clone(), e.span()),
    };
    Some(Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(scrutinee.clone()),
        rhs: Box::new(value),
        span: at,
    })
}

/// `match` as an expression: `c1 ? v1 : c2 ? v2 : … : else-or-nothing`.
fn chain_expr(scrutinee: &Expr, arms: Vec<Arm<Expr>>, span: Span) -> Expr {
    let nothing = Expr::Ident(Symbol::intern("none"), span);
    let mut out = nothing;
    for arm in arms.into_iter().rev() {
        out = match test(scrutinee, &arm.pattern) {
            None => arm.body,
            Some(cond) => Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(arm.body),
                els: Box::new(out),
                span,
            },
        };
    }
    out
}

/// `match` as a view member: `when c1 { … } else when c2 { … } else { … }`.
fn chain_member(scrutinee: &Expr, arms: Vec<Arm<Vec<Member>>>, span: Span) -> Member {
    let mut out: Option<Vec<Member>> = None;
    for arm in arms.into_iter().rev() {
        out = Some(match test(scrutinee, &arm.pattern) {
            None => arm.body,
            Some(cond) => vec![Member::When {
                cond,
                then: arm.body,
                els: out,
                span,
            }],
        });
    }
    // A single member: the chain, or nothing to draw for a `match` with no arm.
    match out {
        Some(mut members) if members.len() == 1 => members.remove(0),
        Some(members) => Member::When {
            cond: Expr::Ident(Symbol::intern("true"), span),
            then: members,
            els: None,
            span,
        },
        None => Member::When {
            cond: Expr::Ident(Symbol::intern("false"), span),
            then: Vec::new(),
            els: None,
            span,
        },
    }
}
