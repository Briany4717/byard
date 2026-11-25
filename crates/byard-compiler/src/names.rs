//! What things are called, and what is said twice (roadmap step 8).
//!
//! One pass over each view's syntax tree, run with the type checks, that finds
//! the mistakes which are about a *name* or a *repeat* and need no types:
//!
//! - two declarations of one name in a body, or one over a parameter
//!   ([`NameCollision`](CompileError::NameCollision)), and a parameter declared
//!   twice ([`DuplicateParam`](CompileError::DuplicateParam));
//! - `let`s that read each other in a circle
//!   ([`ConstantCycle`](CompileError::ConstantCycle));
//! - an attribute written twice in one `#[ … ]`
//!   ([`DuplicateAttribute`](CompileError::DuplicateAttribute)), and the same
//!   `on <state>` block twice in one element
//!   ([`DuplicateStateBlock`](CompileError::DuplicateStateBlock));
//! - a `with` curve anywhere but on an attribute's value
//!   ([`NotAnimatable`](CompileError::NotAnimatable));
//! - the three lints, warnings that `[lints]` in `byard.toml` can turn off:
//!   a `var` or `let` nothing reads, a parameter its view never reads, and a
//!   `private View` nothing draws.
//!
//! A scope is one body: the view's, or that of a `when`, a `for` or a `route`.
//! A name declared in an inner body may reuse one of an outer body, which is
//! how a loop's `let` can be called what the view's is; only a repeat in *one*
//! body is a mistake.

use std::collections::{HashMap, HashSet};

use crate::consts::names_read;
use crate::diagnostics::{CompileError, Span};
use crate::parser::ast::{AttrKind, ElementNode, Expr, Member, StateBlock, ViewDecl};
use crate::resolve::walk;
use crate::symbol::Symbol;

/// Checks every view of the program for the mistakes above, appending what it
/// finds to `out`.
pub(crate) fn check(views: &[ViewDecl], out: &mut Vec<CompileError>) {
    for view in views {
        check_view(view, out);
    }
}

/// The lints of the views `wanted` accepts: warnings, so they are not part of
/// what makes a program wrong, and left for the caller to ask for (and to
/// filter by what the project's `[lints]` allows).
///
/// A package's own views are skipped: a name that is never read in a
/// dependency is not something this project can fix.
#[must_use]
pub fn lints(views: &[ViewDecl], wanted: impl Fn(&ViewDecl) -> bool) -> Vec<CompileError> {
    let drawn = views_drawn(views);
    let mut out = Vec::new();
    for view in views {
        if wanted(view) && !view.name.as_str().contains('.') {
            lint_view(view, &drawn, &mut out);
        }
    }
    out
}

/// The kinds of the lints, the names `[lints]` in `byard.toml` switches, each
/// with the diagnostic it is for.
pub const LINTS: &[(&str, &str)] = &[
    ("unused-variable", "UnusedVariable"),
    ("unused-parameter", "UnusedParam"),
    ("unused-view", "UnusedView"),
];

fn check_view(view: &ViewDecl, out: &mut Vec<CompileError>) {
    let mut seen: HashMap<Symbol, &'static str> = HashMap::new();
    for param in &view.params {
        if seen.insert(param.name.clone(), "a parameter").is_some() {
            out.push(CompileError::DuplicateParam {
                span: param.span,
                name: param.name.as_str().to_string(),
                callee: view.name.as_str().to_string(),
                declared: true,
            });
        }
    }
    // Only the view's own body: a `let` may read one written below it there,
    // which is how a circle can be written at all. In a `when` or a `for` it
    // reads what is above it, and a name below is `UnknownName`, once.
    cycles(&view.body, out);
    scope(&view.body, seen, true, out);
}

/// Checks one body. `declares` is false for an element's children, where a
/// `let` or a `var` is its own mistake and a collision would be a second one.
fn scope(
    members: &[Member],
    mut seen: HashMap<Symbol, &'static str>,
    declares: bool,
    out: &mut Vec<CompileError>,
) {
    if declares {
        for member in members {
            let (name, origin, span) = match member {
                Member::Var { name, span, .. } => (name, "a `var`", *span),
                Member::Let { name, span, .. } => (name, "a `let`", *span),
                Member::Fn { name, span, .. } => (name, "a `fn`", *span),
                Member::Inject { name, span, .. } => (name, "an `inject`", *span),
                _ => continue,
            };
            match seen.get(name) {
                Some(first) => out.push(CompileError::NameCollision {
                    span,
                    name: name.as_str().to_string(),
                    first: (*first).to_string(),
                    second: origin.to_string(),
                    local: true,
                }),
                None => {
                    seen.insert(name.clone(), origin);
                }
            }
        }
    }
    animated_outside_an_attribute(members, out);
    for member in members {
        inside(member, out);
    }
}

/// The bodies and elements inside `member`, each checked in turn.
fn inside(member: &Member, out: &mut Vec<CompileError>) {
    match member {
        Member::Element(el) => element(el, out),
        Member::For { body, .. } | Member::Route { body, .. } => {
            scope(body, HashMap::new(), true, out);
        }
        Member::Match { arms, .. } => {
            for arm in arms {
                scope(&arm.body, HashMap::new(), true, out);
            }
        }
        Member::When { then, els, .. } => {
            scope(then, HashMap::new(), true, out);
            if let Some(els) = els {
                scope(els, HashMap::new(), true, out);
            }
        }
        _ => {}
    }
}

fn element(el: &ElementNode, out: &mut Vec<CompileError>) {
    let mut seen: HashSet<(Symbol, Option<Symbol>)> = HashSet::new();
    for attr in &el.attrs {
        if matches!(attr.kind, AttrKind::Spread { .. }) {
            continue;
        }
        if !seen.insert((attr.name.clone(), attr.axis.clone())) {
            let name = match &attr.axis {
                Some(axis) => format!("{}.{}", attr.name.as_str(), axis.as_str()),
                None => attr.name.as_str().to_string(),
            };
            out.push(CompileError::DuplicateAttribute {
                span: attr.span,
                name,
            });
        }
    }
    for attr in &el.attrs {
        if let AttrKind::Spread {
            value: Expr::StyleValue { states, .. },
        } = &attr.kind
        {
            repeated_state_blocks(states, out);
        }
    }
    scope(&el.children, HashMap::new(), false, out);
    for slot in el.slot_members() {
        scope(slot, HashMap::new(), true, out);
    }
}

/// An element has one block for each set of states: `on hover` twice is the
/// second one never being read.
fn repeated_state_blocks(blocks: &[StateBlock], out: &mut Vec<CompileError>) {
    for (at, block) in blocks.iter().enumerate() {
        let same = |earlier: &StateBlock| {
            earlier.states.len() == block.states.len()
                && earlier.states.iter().all(|s| block.states.contains(s))
                && earlier.remote.len() == block.remote.len()
                && earlier.remote.iter().all(|r| {
                    block
                        .remote
                        .iter()
                        .any(|o| o.name == r.name && o.kind == r.kind)
                })
                && earlier.viewport == block.viewport
        };
        if blocks[..at].iter().any(same) {
            let mut written: Vec<String> = block
                .remote
                .iter()
                .map(|r| format!("{}.{}", r.name.as_str(), r.kind.as_str()))
                .collect();
            written.extend(block.states.iter().map(|s| s.as_str().to_string()));
            out.push(CompileError::DuplicateStateBlock {
                span: block.span,
                state: written.join("+"),
            });
        }
    }
}

/// Where a depth-first walk over the `let`s is with each of them.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Open,
    Done,
}

/// Walks from `at` along what each `let` reads, noting each circle it closes.
fn visit(
    at: usize,
    reads: &[Vec<usize>],
    marks: &mut Vec<Option<Mark>>,
    path: &mut Vec<usize>,
    found: &mut Vec<Vec<usize>>,
) {
    marks[at] = Some(Mark::Open);
    path.push(at);
    for &next in &reads[at] {
        match marks[next] {
            None => visit(next, reads, marks, path, found),
            Some(Mark::Open) => {
                let from = path.iter().position(|&n| n == next).unwrap_or(0);
                let mut circle = path[from..].to_vec();
                circle.push(next);
                found.push(circle);
            }
            Some(Mark::Done) => {}
        }
    }
    path.pop();
    marks[at] = Some(Mark::Done);
}

/// `let`s of one body that read each other in a circle: each circle once, the
/// way a circle of top-level constants is.
fn cycles(members: &[Member], out: &mut Vec<CompileError>) {
    let lets: Vec<(&Symbol, &Expr, Span)> = members
        .iter()
        .filter_map(|m| match m {
            Member::Let {
                name, init, span, ..
            } => Some((name, init, *span)),
            _ => None,
        })
        .collect();
    if lets.is_empty() {
        return;
    }
    let index: HashMap<&Symbol, usize> = lets.iter().enumerate().map(|(i, l)| (l.0, i)).collect();
    let reads: Vec<Vec<usize>> = lets
        .iter()
        .map(|(_, init, _)| {
            names_read(init, &HashSet::new())
                .iter()
                .filter_map(|n| index.get(n).copied())
                .collect()
        })
        .collect();

    let mut marks = vec![None; lets.len()];
    let mut found: Vec<Vec<usize>> = Vec::new();
    for start in 0..lets.len() {
        if marks[start].is_none() {
            visit(start, &reads, &mut marks, &mut Vec::new(), &mut found);
        }
    }
    // Every identifier of the body, with where it is, for telling whether
    // anything outside a circle reads it.
    let mut idents: Vec<(Symbol, Span)> = Vec::new();
    for member in members {
        let mut copy = member.clone();
        walk::member_exprs(&mut copy, &mut |e| {
            if let Expr::Ident(name, span) = e {
                idents.push((name.clone(), *span));
            }
        });
    }
    for circle in found {
        // A circle that nothing reads is never evaluated, so it cannot hang
        // or draw anything wrong; one that is read is an error.
        let inits: Vec<Span> = circle.iter().map(|&i| lets[i].1.span()).collect();
        let read_outside = idents.iter().any(|(name, at)| {
            circle.iter().any(|&i| lets[i].0 == name)
                && !inits
                    .iter()
                    .any(|init| init.start <= at.start && at.end <= init.end)
        });
        if !read_outside {
            continue;
        }
        out.push(CompileError::ConstantCycle {
            span: lets[circle[0]].2,
            path: circle
                .iter()
                .map(|&i| lets[i].0.as_str())
                .collect::<Vec<_>>()
                .join(" → "),
        });
    }
}

// ---- `with` outside an attribute ----------------------------------------

/// `x with anim.spring()` animates what an attribute shows, so it belongs on
/// an attribute's value. Anywhere else (a `let`, a condition, an action) it
/// would compile and do nothing.
fn animated_outside_an_attribute(members: &[Member], out: &mut Vec<CompileError>) {
    for member in members {
        match member {
            Member::Var { name, init, .. } | Member::Let { name, init, .. } => {
                animated_in(init, name.as_str(), out);
            }
            Member::Fn { name, body, .. } => animated_in(body, name.as_str(), out),
            Member::When { cond, .. } => animated_in(cond, "value", out),
            Member::For { iter, key, .. } => {
                animated_in(iter, "value", out);
                if let Some(key) = key {
                    animated_in(key, "value", out);
                }
            }
            Member::Lifecycle { action, .. }
            | Member::Measure { action, .. }
            | Member::Timer { action, .. } => animated_in(action, "value", out),
            Member::Change {
                watched, action, ..
            } => {
                for value in watched {
                    animated_in(value, "value", out);
                }
                animated_in(action, "value", out);
            }
            Member::Provide { fields, .. } => {
                for field in fields {
                    animated_in(&field.value, "value", out);
                }
            }
            // An element's own `( … )` is not checked: a shape of a `Canvas`
            // takes its parameters there, and animates them.
            Member::Element(el) => {
                if let Some(action) = &el.action {
                    animated_in(action, "value", out);
                }
                for attr in &el.attrs {
                    if let AttrKind::Event { action, .. } = &attr.kind {
                        animated_in(action, "value", out);
                    }
                }
            }
            Member::Expr(e) => animated_in(e, "value", out),
            _ => {}
        }
    }
}

/// Reports each outermost `with` in `expr`, except inside a `style { … }`,
/// whose values are attributes' own.
fn animated_in(expr: &Expr, what: &str, out: &mut Vec<CompileError>) {
    let mut copy = expr.clone();
    let mut animated: Vec<Span> = Vec::new();
    let mut styles: Vec<Span> = Vec::new();
    walk::expr(&mut copy, &mut |e| match e {
        Expr::Animated { span, .. } => animated.push(*span),
        Expr::StyleValue { span, .. } => styles.push(*span),
        _ => {}
    });
    let inside = |outer: &Span, inner: &Span| {
        outer.start <= inner.start && inner.end <= outer.end && outer != inner
    };
    for span in &animated {
        let in_style = styles.iter().any(|s| inside(s, span));
        let in_other = animated.iter().any(|a| inside(a, span));
        if !in_style && !in_other {
            out.push(CompileError::NotAnimatable {
                span: *span,
                prop: what.to_string(),
                use_instead: String::new(),
            });
        }
    }
}

// ---- lints ---------------------------------------------------------------

/// Every view some view draws, by the name it is written with.
fn views_drawn(views: &[ViewDecl]) -> HashSet<Symbol> {
    let mut drawn = HashSet::new();
    for view in views {
        views_drawn_in(&view.body, &mut drawn);
    }
    drawn
}

/// Adds to `drawn` the name of every element in `list`, at any depth.
fn views_drawn_in(list: &[Member], drawn: &mut HashSet<Symbol>) {
    for member in list {
        match member {
            Member::Element(el) => {
                drawn.insert(el.name.clone());
                views_drawn_in(&el.children, drawn);
                for slot in el.slot_members() {
                    views_drawn_in(slot, drawn);
                }
            }
            Member::For { body, .. } | Member::Route { body, .. } => views_drawn_in(body, drawn),
            Member::Match { arms, .. } => {
                for arm in arms {
                    views_drawn_in(&arm.body, drawn);
                }
            }
            Member::When { then, els, .. } => {
                views_drawn_in(then, drawn);
                if let Some(els) = els {
                    views_drawn_in(els, drawn);
                }
            }
            _ => {}
        }
    }
}

fn lint_view(view: &ViewDecl, drawn: &HashSet<Symbol>, out: &mut Vec<CompileError>) {
    if view.private && !drawn.contains(&view.name) {
        out.push(CompileError::UnusedView {
            span: view.span,
            name: view.name.as_str().to_string(),
        });
    }

    // A name is used where it is written as an identifier, to read it or to
    // write it. (A `var` that is only ever written is not said to be unused:
    // "never read" is a stricter test, and a later one.)
    let mut seen: HashSet<Symbol> = HashSet::new();
    let mut count = |e: &mut Expr| {
        if let Expr::Ident(name, _) = e {
            seen.insert(name.clone());
        }
    };
    for param in &view.params {
        if let Some(default) = &param.default {
            let mut copy = default.clone();
            walk::expr(&mut copy, &mut count);
        }
    }
    for member in &view.body {
        let mut copy = member.clone();
        walk::member_exprs(&mut copy, &mut count);
    }
    // A slot parameter is placed by writing its name where an element goes.
    views_drawn_in(&view.body, &mut seen);
    let read = |name: &Symbol| seen.contains(name);

    for param in &view.params {
        if !param.bind && !param.name.as_str().starts_with('_') && !read(&param.name) {
            out.push(CompileError::UnusedParam {
                span: param.span,
                name: param.name.as_str().to_string(),
                view: view.name.as_str().to_string(),
            });
        }
    }
    declared(&view.body, &mut |keyword, name, span| {
        if !name.as_str().starts_with('_') && !read(name) {
            out.push(CompileError::UnusedVariable {
                span,
                name: name.as_str().to_string(),
                keyword: keyword.to_string(),
            });
        }
    });
}

/// Calls `f` for every `var` and `let` a body declares, in the bodies of its
/// `when`s, `for`s and routes too.
fn declared(members: &[Member], f: &mut dyn FnMut(&str, &Symbol, Span)) {
    for member in members {
        match member {
            Member::Var { name, span, .. } => f("var", name, *span),
            Member::Let { name, span, .. } => f("let", name, *span),
            Member::For { body, .. } | Member::Route { body, .. } => declared(body, f),
            Member::Match { arms, .. } => {
                for arm in arms {
                    declared(&arm.body, f);
                }
            }
            Member::When { then, els, .. } => {
                declared(then, f);
                if let Some(els) = els {
                    declared(els, f);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn a_dependencys_views_are_not_linted() {
        let mut parsed = parse("View Card(a: Int) { Text(\"x\") }");
        assert_eq!(lints(&parsed.views, |_| true).len(), 1);
        parsed.views[0].name = Symbol::intern("kit.Card");
        assert_eq!(lints(&parsed.views, |_| true).len(), 0);
    }

    #[test]
    fn only_the_views_asked_for_are_linted() {
        let parsed = parse("View A(a: Int) { Text(\"x\") }\nView B(b: Int) { Text(\"y\") }");
        let found = lints(&parsed.views, |v| v.name.as_str() == "B");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind(), "UnusedParam");
    }

    #[test]
    fn a_circle_of_lets_is_named_in_the_order_it_is_read() {
        let parsed = parse("View Main() { let a = b + 1 let b = a + 1 Text(\"{a}\") }");
        let mut out = Vec::new();
        check(&parsed.views, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            out[0].headline(),
            "constants and `let`s cannot read each other in a circle: a → b → a"
        );
    }
}
