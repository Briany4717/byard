//! The order top-level constants are evaluated in (RFC-0055 §3).
//!
//! A constant may read other constants, directly or through a function it
//! calls. That is a graph; without a cycle it has an order, and the
//! interpreter evaluates the constants in it, once, so every constant is
//! computed after the ones it reads. With a cycle there is no order and
//! `byard check` says so.
//!
//! Both users, the checker (which reports the cycle) and the interpreter
//! (which needs the order), call [`order`], so they cannot disagree about what
//! a cycle is.

use std::collections::{HashMap, HashSet};

use crate::parser::ast::{ConstDecl, Expr, FnDecl};
use crate::resolve::walk;
use crate::symbol::Symbol;

/// The result of ordering a program's constants.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ordered {
    /// Indices into the constants, each after every constant it reads. A
    /// constant in a cycle is left out.
    pub order: Vec<usize>,
    /// Each cycle found, as the names along it with the first repeated at the
    /// end (`gap`, `pad`, `gap`), and the index of the constant it starts at.
    pub cycles: Vec<(usize, Vec<Symbol>)>,
}

/// How far the depth-first search is with a constant.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Open,
    Done,
}

/// Orders `consts`, which read one another and call `fns`.
#[must_use]
pub fn order(consts: &[ConstDecl], fns: &[FnDecl]) -> Ordered {
    let index: HashMap<&Symbol, usize> = consts
        .iter()
        .enumerate()
        .map(|(i, c)| (&c.name, i))
        .collect();
    if consts.is_empty() {
        return Ordered::default();
    }
    let fn_refs: HashMap<&Symbol, Vec<Symbol>> = fns
        .iter()
        .map(|f| {
            let bound: HashSet<Symbol> = f.params.iter().map(|p| p.name.clone()).collect();
            (&f.name, names_read(&f.body, &bound))
        })
        .collect();

    // The constants each constant reads, through functions too.
    let deps: Vec<Vec<usize>> = consts
        .iter()
        .map(|c| {
            let mut found = Vec::new();
            let mut seen_fns: HashSet<&Symbol> = HashSet::new();
            let mut pending = names_read(&c.init, &HashSet::new());
            while let Some(name) = pending.pop() {
                if let Some(&i) = index.get(&name) {
                    if !found.contains(&i) {
                        found.push(i);
                    }
                } else if let Some((key, refs)) = fn_refs.get_key_value(&name) {
                    if seen_fns.insert(key) {
                        pending.extend(refs.iter().cloned());
                    }
                }
            }
            found.sort_unstable();
            found
        })
        .collect();

    let mut marks: Vec<Option<Mark>> = vec![None; consts.len()];
    let mut out = Ordered::default();
    let mut path: Vec<usize> = Vec::new();
    let mut in_cycle: HashSet<usize> = HashSet::new();

    // Iterative depth-first search: a program's constants are few, but a deep
    // chain must not be able to exhaust the stack.
    for root in 0..consts.len() {
        if marks[root].is_some() {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        marks[root] = Some(Mark::Open);
        path.push(root);
        while let Some(&(node, next)) = stack.last() {
            let Some(&dep) = deps[node].get(next) else {
                marks[node] = Some(Mark::Done);
                path.pop();
                stack.pop();
                out.order.push(node);
                continue;
            };
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            match marks[dep] {
                None => {
                    marks[dep] = Some(Mark::Open);
                    path.push(dep);
                    stack.push((dep, 0));
                }
                Some(Mark::Open) => {
                    let from = path.iter().position(|&n| n == dep).unwrap_or(0);
                    let members = &path[from..];
                    if !members.iter().any(|m| in_cycle.contains(m)) {
                        let mut names: Vec<Symbol> =
                            members.iter().map(|&n| consts[n].name.clone()).collect();
                        names.push(consts[dep].name.clone());
                        out.cycles.push((dep, names));
                    }
                    in_cycle.extend(members.iter().copied());
                }
                Some(Mark::Done) => {}
            }
        }
    }
    // A constant that reads one in a cycle can never be evaluated either.
    let mut dropped = in_cycle;
    let mut changed = !dropped.is_empty();
    while changed {
        changed = false;
        for (i, reads) in deps.iter().enumerate() {
            if !dropped.contains(&i) && reads.iter().any(|d| dropped.contains(d)) {
                dropped.insert(i);
                changed = true;
            }
        }
    }
    out.order.retain(|i| !dropped.contains(i));
    out
}

/// Every name an expression reads, in order, except lambda parameters and the
/// names in `bound`.
fn names_read(expr: &Expr, bound: &HashSet<Symbol>) -> Vec<Symbol> {
    let mut copy = expr.clone();
    let mut bound = bound.clone();
    let mut used: Vec<Symbol> = Vec::new();
    walk::expr(&mut copy, &mut |e| match e {
        Expr::Lambda { params, .. } => bound.extend(params.iter().cloned()),
        Expr::Ident(name, _) => used.push(name.clone()),
        _ => {}
    });
    used.retain(|n| !bound.contains(n));
    used
}
