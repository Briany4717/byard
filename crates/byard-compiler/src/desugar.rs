//! `match`, written as what the interpreter already runs (the one pass after
//! the checks, so that the checker sees what was written and nothing else does
//! have to know a new form exists).
//!
//! A `match` over a value is a chain of comparisons with it: as a view member a
//! chain of `when … else when …`, as an expression a chain of `? :`. An enum
//! case is the text of its name at run time, so `.idle` is a comparison with
//! `"idle"`. What the chain does when no arm holds is nothing, which the
//! checker has already said is not so for an enum (`NonExhaustive`).
//!
//! A case that carries values is a record at run time: its own name under
//! `case`, then each value under the name it was declared with. Built with
//! `Load.failed(message: "offline")` it is that record literal; matched with
//! `.failed(message)` it is `s.case == "failed"`, and the arm reads `message`
//! as `s.message`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::diagnostics::Span;
use crate::parser::ast::{
    Arm, BinOp, ConstDecl, EnumDecl, Expr, FnDecl, Member, Pattern, StrPart, ViewDecl,
};
use crate::resolve::walk;
use crate::symbol::Symbol;

/// Rewrites every `match` of `views`, and of the functions and constants they
/// share, as the chain it means.
pub(crate) fn matches(views: &mut [ViewDecl], map_loops: &[Span]) {
    let mut enums = Enums::of(views.first().map(|v| &v.enums[..]).unwrap_or_default());
    enums.map_loops = map_loops.iter().map(|s| (s.start, s.end)).collect();
    let (mut helpers, mut consts): (Vec<FnDecl>, Vec<ConstDecl>) = views
        .first()
        .map(|v| ((*v.helpers).clone(), (*v.consts).clone()))
        .unwrap_or_default();
    for decl in &mut helpers {
        in_expr(&mut decl.body, &enums);
        for p in &mut decl.params {
            if let Some(d) = &mut p.default {
                in_expr(d, &enums);
            }
        }
    }
    for decl in &mut consts {
        in_expr(&mut decl.init, &enums);
    }
    let helpers = Arc::new(helpers);
    let consts = Arc::new(consts);
    for view in views {
        in_members(&mut view.body, &enums);
        for p in &mut view.params {
            if let Some(d) = &mut p.default {
                in_expr(d, &enums);
            }
        }
        view.helpers = Arc::clone(&helpers);
        view.consts = Arc::clone(&consts);
    }
}

/// Every `match` expression inside `expr`, and every list of members a slot
/// block holds.
fn in_expr(expr: &mut Expr, enums: &Enums) {
    walk::expr(expr, &mut |e| rewrite(e, enums));
}

fn rewrite(e: &mut Expr, enums: &Enums) {
    if let Some(record) = enums.built(e) {
        *e = record;
        return;
    }
    // `for k, v in map { … }` as an action.
    if let Expr::ForEach {
        var,
        index,
        body,
        span,
        ..
    } = e
    {
        if enums.goes_over_a_map(*span) {
            let (entry, lets) = Enums::entry_lets(*span, var, index.as_ref());
            let mut stmts: Vec<Expr> = lets
                .into_iter()
                .map(|(name, value)| Expr::Let {
                    name,
                    ty: None,
                    value: Box::new(value),
                    span: *span,
                })
                .collect();
            match std::mem::replace(&mut **body, Expr::Error(*span)) {
                Expr::Block(inner, _) => stmts.extend(inner),
                other => stmts.push(other),
            }
            **body = Expr::Block(stmts, *span);
            *var = entry;
            *index = None;
        }
        return;
    }
    if matches!(e, Expr::Match { .. }) {
        let whole = std::mem::replace(e, Expr::Error(Span::new(0, 0)));
        if let Expr::Match {
            scrutinee,
            arms,
            span,
        } = whole
        {
            *e = chain_expr(&scrutinee, arms, span, enums);
        }
        return;
    }
    match e {
        Expr::SlotBlock { members, .. }
        | Expr::Lambda {
            slot: Some(members),
            ..
        } => in_members(members, enums),
        _ => {}
    }
}

/// Every `match` of a list of members, at any depth.
fn in_members(members: &mut Vec<Member>, enums: &Enums) {
    for member in members.iter_mut() {
        // `for k, v in map { … }` in a view.
        if let Member::For {
            var,
            index,
            body,
            span,
            ..
        } = member
        {
            if enums.goes_over_a_map(*span) {
                let (entry, lets) = Enums::entry_lets(*span, var, index.as_ref());
                let mut inner: Vec<Member> = lets
                    .into_iter()
                    .map(|(name, init)| Member::Let {
                        name,
                        ty: None,
                        init,
                        span: *span,
                    })
                    .collect();
                inner.append(body);
                *body = inner;
                *var = entry;
                *index = None;
            }
        }
        walk::member_exprs(member, &mut |e| rewrite(e, enums));
        match member {
            Member::Element(el) => in_members(&mut el.children, enums),
            Member::For { body, .. } | Member::Route { body, .. } => in_members(body, enums),
            Member::When { then, els, .. } => {
                in_members(then, enums);
                if let Some(els) = els {
                    in_members(els, enums);
                }
            }
            Member::Match { arms, .. } => {
                for arm in arms {
                    in_members(&mut arm.body, enums);
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
            } => chain_member(&scrutinee, arms, span, enums),
            other => other,
        });
    }
}

/// What the program's enums say about the cases that carry values.
pub(crate) struct Enums {
    /// For each enum, each case with the names of its fields (empty for a
    /// case that is its name alone).
    by_enum: HashMap<Symbol, HashMap<Symbol, Vec<Symbol>>>,
    /// The spans of the `for` loops that go over a map.
    map_loops: std::collections::HashSet<(u32, u32)>,
}

impl Enums {
    fn of(decls: &[EnumDecl]) -> Self {
        let by_enum = decls
            .iter()
            .map(|d| {
                let cases = d
                    .cases
                    .iter()
                    .map(|(case, _)| {
                        let fields = d
                            .fields
                            .iter()
                            .find(|(c, _)| c == case)
                            .map(|(_, f)| f.iter().map(|(n, _)| n.clone()).collect())
                            .unwrap_or_default();
                        (case.clone(), fields)
                    })
                    .collect();
                (d.name.clone(), cases)
            })
            .collect();
        Self {
            by_enum,
            map_loops: std::collections::HashSet::new(),
        }
    }

    /// Whether the `for` at `span` goes over a map.
    fn goes_over_a_map(&self, span: Span) -> bool {
        self.map_loops.contains(&(span.start, span.end))
    }

    /// The name a map loop reads each entry as, and the names it binds from
    /// it: `for k, v in m` runs for each entry `e` with `k = e.key` and
    /// `v = e.value`.
    fn entry_lets(
        span: Span,
        var: &Symbol,
        index: Option<&Symbol>,
    ) -> (Symbol, Vec<(Symbol, Expr)>) {
        let entry = Symbol::intern(&format!("__entry_{}", span.start));
        let read = |field: &str| Expr::Member {
            base: Box::new(Expr::Ident(entry.clone(), span)),
            field: Symbol::intern(field),
            span,
        };
        let mut lets = Vec::new();
        if let Some(index) = index {
            lets.push((index.clone(), read("key")));
        }
        lets.push((var.clone(), read("value")));
        (entry, lets)
    }

    /// `Load.failed(message: "offline")` as the record it is at run time.
    fn built(&self, e: &Expr) -> Option<Expr> {
        let Expr::Call { callee, args, span } = e else {
            return None;
        };
        let Expr::Member { base, field, .. } = &**callee else {
            return None;
        };
        let Expr::Ident(name, _) = &**base else {
            return None;
        };
        if self.by_enum.get(name)?.get(field)?.is_empty() {
            return None;
        }
        let mut fields = vec![(Symbol::intern("case"), text(field, *span))];
        fields.extend(
            args.iter()
                .filter_map(|a| a.name.clone().map(|n| (n, a.value.clone()))),
        );
        Some(Expr::Record {
            fields,
            spread: None,
            span: *span,
        })
    }

    /// The fields a pattern binds, as the names the case declares them with,
    /// and whether the case is a record (it carries values) at run time.
    fn carried(&self, enum_name: Option<&Symbol>, case: &Symbol) -> Option<&Vec<Symbol>> {
        let mut found = self
            .by_enum
            .iter()
            .filter(|(name, _)| enum_name.is_none_or(|w| w == *name))
            .filter_map(|(_, cases)| cases.get(case));
        found.find(|fields| !fields.is_empty())
    }

    /// Whether a case of that name is written as its name alone in some enum
    /// the pattern may mean.
    fn plain(&self, enum_name: Option<&Symbol>, case: &Symbol) -> bool {
        self.by_enum
            .iter()
            .filter(|(name, _)| enum_name.is_none_or(|w| w == *name))
            .filter_map(|(_, cases)| cases.get(case))
            .any(Vec::is_empty)
    }
}

fn text(name: &Symbol, span: Span) -> Expr {
    Expr::StrLit(vec![StrPart::Text(name.as_str().to_string())], span)
}

/// The condition that makes `pattern` the arm to take, `None` for `else`.
fn test(scrutinee: &Expr, pattern: &Pattern, enums: &Enums) -> Option<Expr> {
    let eq = |lhs: Expr, rhs: Expr, span: Span| Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
        span,
    };
    match pattern {
        Pattern::Else(_) => None,
        Pattern::Case {
            enum_name,
            name,
            span,
            ..
        } => {
            let name_of = |s: &Expr| Expr::Member {
                base: Box::new(s.clone()),
                field: Symbol::intern("case"),
                span: *span,
            };
            let by_value = eq(scrutinee.clone(), text(name, *span), *span);
            if enums.carried(enum_name.as_ref(), name).is_none() {
                return Some(by_value);
            }
            let by_case = eq(name_of(scrutinee), text(name, *span), *span);
            // The same name is a plain case of another enum: either holds.
            Some(if enums.plain(enum_name.as_ref(), name) {
                Expr::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(by_value),
                    rhs: Box::new(by_case),
                    span: *span,
                }
            } else {
                by_case
            })
        }
        Pattern::Literal(e) => Some(eq(scrutinee.clone(), e.clone(), e.span())),
    }
}

/// The values a pattern binds, as `name = scrutinee.field` in the order the
/// case declares its fields.
fn bound(scrutinee: &Expr, pattern: &Pattern, enums: &Enums) -> Vec<(Symbol, Expr, Span)> {
    let Pattern::Case {
        enum_name,
        name,
        binds,
        span,
    } = pattern
    else {
        return Vec::new();
    };
    let Some(fields) = enums.carried(enum_name.as_ref(), name) else {
        return Vec::new();
    };
    binds
        .iter()
        .zip(fields)
        .map(|(bind, field)| {
            let read = Expr::Member {
                base: Box::new(scrutinee.clone()),
                field: field.clone(),
                span: *span,
            };
            (bind.clone(), read, *span)
        })
        .collect()
}

/// `match` as an expression: `c1 ? v1 : c2 ? v2 : … : else-or-nothing`.
fn chain_expr(scrutinee: &Expr, arms: Vec<Arm<Expr>>, span: Span, enums: &Enums) -> Expr {
    let nothing = Expr::Ident(Symbol::intern("none"), span);
    let mut out = nothing;
    for arm in arms.into_iter().rev() {
        let lets = bound(scrutinee, &arm.pattern, enums);
        let body = if lets.is_empty() {
            arm.body
        } else {
            let mut stmts: Vec<Expr> = lets
                .into_iter()
                .map(|(name, value, at)| Expr::Let {
                    name,
                    ty: None,
                    value: Box::new(value),
                    span: at,
                })
                .collect();
            stmts.push(arm.body);
            Expr::Block(stmts, span)
        };
        out = match test(scrutinee, &arm.pattern, enums) {
            None => body,
            Some(cond) => Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(body),
                els: Box::new(out),
                span,
            },
        };
    }
    out
}

/// `match` as a view member: `when c1 { … } else when c2 { … } else { … }`.
fn chain_member(
    scrutinee: &Expr,
    arms: Vec<Arm<Vec<Member>>>,
    span: Span,
    enums: &Enums,
) -> Member {
    let mut out: Option<Vec<Member>> = None;
    for arm in arms.into_iter().rev() {
        let mut body: Vec<Member> = bound(scrutinee, &arm.pattern, enums)
            .into_iter()
            .map(|(name, init, at)| Member::Let {
                name,
                ty: None,
                init,
                span: at,
            })
            .collect();
        body.extend(arm.body);
        out = Some(match test(scrutinee, &arm.pattern, enums) {
            None => body,
            Some(cond) => vec![Member::When {
                cond,
                then: body,
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
