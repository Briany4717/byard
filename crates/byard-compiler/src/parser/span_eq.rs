//! Structural equality of syntax trees with source positions ignored
//! (RFC-0044 §3).
//!
//! The formatter moves every token, so two trees that mean the same differ in
//! every [`Span`](crate::Span). The derived `PartialEq` on the AST compares spans too, and
//! it has to: hot reload and the checker rely on it. Rather than change how
//! spans are stored (the parser would pay for it on every node), this is a
//! second comparison that skips them, used only where a tree is compared
//! with a reparse of the same program.
//!
//! Every impl destructures its node **without** `..`, so a field added to the
//! AST stops these compiling until it is compared (or deliberately skipped
//! here, in a comment). The check cannot quietly stop covering a field.

use super::ParsedFile;
use super::ast::{
    Arg, Attr, AttrKind, Breakpoint, ElementNode, Expr, FnDecl, Member, Param, RecordDecl,
    RecordField, ResultArm, StateBlock, StrPart, StyleRule, Type, UseDecl, ViewDecl, ViewportCond,
};
use crate::symbol::Symbol;

/// Equality that skips every [`Span`](crate::Span): two trees are `span_eq` when they have
/// the same shape, names and literal values, wherever each was written.
pub trait SpanEq {
    /// Whether `self` and `other` are the same tree apart from positions.
    fn span_eq(&self, other: &Self) -> bool;
}

impl<T: SpanEq> SpanEq for Vec<T> {
    fn span_eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a.span_eq(b))
    }
}

impl<T: SpanEq> SpanEq for Option<T> {
    fn span_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(a), Some(b)) => a.span_eq(b),
            (None, None) => true,
            _ => false,
        }
    }
}

impl<T: SpanEq + ?Sized> SpanEq for Box<T> {
    fn span_eq(&self, other: &Self) -> bool {
        (**self).span_eq(&**other)
    }
}

/// Whether two parses of a file are the same program, ignoring positions and
/// diagnostics. A view's `helpers` and `records` are copies of the file's
/// `fns` and `records`, so they are compared once, at the file level.
#[must_use]
pub fn parsed_files_span_eq(a: &ParsedFile, b: &ParsedFile) -> bool {
    let ParsedFile {
        imports,
        views,
        fns,
        records,
        errors: _,
    } = a;
    imports.span_eq(&b.imports)
        && views.span_eq(&b.views)
        && fns.span_eq(&b.fns)
        && records.span_eq(&b.records)
}

impl SpanEq for Type {
    fn span_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Named {
                    name,
                    args,
                    span: _,
                },
                Self::Named {
                    name: n2,
                    args: a2,
                    span: _,
                },
            ) => name == n2 && args.span_eq(a2),
            (
                Self::Function {
                    params,
                    ret,
                    span: _,
                },
                Self::Function {
                    params: p2,
                    ret: r2,
                    span: _,
                },
            ) => params.span_eq(p2) && ret.span_eq(r2),
            _ => false,
        }
    }
}

impl SpanEq for Param {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            name,
            ty,
            default,
            span: _,
        } = self;
        *name == other.name && ty.span_eq(&other.ty) && default.span_eq(&other.default)
    }
}

impl SpanEq for UseDecl {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            package,
            alias,
            symbols,
            span: _,
        } = self;
        *package == other.package
            && *alias == other.alias
            && match (symbols, &other.symbols) {
                (Some(a), Some(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|((x, _), (y, _))| x == y)
                }
                (None, None) => true,
                _ => false,
            }
    }
}

impl SpanEq for ViewDecl {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            name,
            params,
            body,
            span: _,
            // Shared copies of the file's functions and records, compared
            // once in `parsed_files_span_eq`.
            helpers: _,
            records: _,
        } = self;
        *name == other.name && params.span_eq(&other.params) && body.span_eq(&other.body)
    }
}

impl SpanEq for RecordDecl {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            name,
            fields,
            span: _,
        } = self;
        *name == other.name && fields.span_eq(&other.fields)
    }
}

impl SpanEq for RecordField {
    fn span_eq(&self, other: &Self) -> bool {
        let Self { name, ty, optional } = self;
        *name == other.name && ty.span_eq(&other.ty) && *optional == other.optional
    }
}

impl SpanEq for FnDecl {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            name,
            params,
            ret,
            body,
            span: _,
        } = self;
        *name == other.name
            && params.span_eq(&other.params)
            && ret.span_eq(&other.ret)
            && body.span_eq(&other.body)
    }
}

impl SpanEq for Member {
    #[allow(clippy::too_many_lines)]
    fn span_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Var {
                    name,
                    ty,
                    init,
                    span: _,
                },
                Self::Var {
                    name: n2,
                    ty: t2,
                    init: i2,
                    span: _,
                },
            )
            | (
                Self::Let {
                    name,
                    ty,
                    init,
                    span: _,
                },
                Self::Let {
                    name: n2,
                    ty: t2,
                    init: i2,
                    span: _,
                },
            ) => name == n2 && ty.span_eq(t2) && init.span_eq(i2),
            (
                Self::Fn {
                    name,
                    params,
                    ret,
                    body,
                    span: _,
                },
                Self::Fn {
                    name: n2,
                    params: p2,
                    ret: r2,
                    body: b2,
                    span: _,
                },
            ) => name == n2 && params.span_eq(p2) && ret.span_eq(r2) && body.span_eq(b2),
            (
                Self::Inject { ty, name, span: _ },
                Self::Inject {
                    ty: t2,
                    name: n2,
                    span: _,
                },
            ) => name == n2 && ty.span_eq(t2),
            (Self::Element(a), Self::Element(b)) => a.span_eq(b),
            (
                Self::For {
                    var,
                    index,
                    iter,
                    body,
                    span: _,
                },
                Self::For {
                    var: v2,
                    index: x2,
                    iter: i2,
                    body: b2,
                    span: _,
                },
            ) => var == v2 && index == x2 && iter.span_eq(i2) && body.span_eq(b2),
            (
                Self::When {
                    cond,
                    then,
                    els,
                    span: _,
                },
                Self::When {
                    cond: c2,
                    then: t2,
                    els: e2,
                    span: _,
                },
            ) => cond.span_eq(c2) && then.span_eq(t2) && els.span_eq(e2),
            (
                Self::Route {
                    kind,
                    pattern,
                    params,
                    body,
                    // The pattern literal's position, for diagnostics.
                    pattern_span: _,
                    span: _,
                },
                Self::Route {
                    kind: k2,
                    pattern: p2,
                    params: q2,
                    body: b2,
                    pattern_span: _,
                    span: _,
                },
            ) => kind == k2 && pattern == p2 && params == q2 && body.span_eq(b2),
            (
                Self::Lifecycle {
                    on_mount,
                    action,
                    span: _,
                },
                Self::Lifecycle {
                    on_mount: m2,
                    action: a2,
                    span: _,
                },
            ) => on_mount == m2 && action.span_eq(a2),
            (
                Self::Timer {
                    every,
                    dur_ms,
                    action,
                    span: _,
                },
                Self::Timer {
                    every: e2,
                    dur_ms: d2,
                    action: a2,
                    span: _,
                },
            ) => every == e2 && dur_ms == d2 && action.span_eq(a2),
            (
                Self::Measure { action, span: _ },
                Self::Measure {
                    action: a2,
                    span: _,
                },
            ) => action.span_eq(a2),
            (
                Self::Change {
                    watched,
                    action,
                    span: _,
                },
                Self::Change {
                    watched: w2,
                    action: a2,
                    span: _,
                },
            ) => watched.span_eq(w2) && action.span_eq(a2),
            (Self::Style { rules, span: _ }, Self::Style { rules: r2, span: _ }) => {
                rules.span_eq(r2)
            }
            (Self::Expr(a), Self::Expr(b)) => a.span_eq(b),
            _ => false,
        }
    }
}

impl SpanEq for ResultArm {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            binding,
            action,
            span: _,
        } = self;
        *binding == other.binding && action.span_eq(&other.action)
    }
}

impl SpanEq for ElementNode {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            name,
            content,
            attrs,
            action,
            children,
            anchor_name,
            span: _,
        } = self;
        *name == other.name
            && content.span_eq(&other.content)
            && attrs.span_eq(&other.attrs)
            && action.span_eq(&other.action)
            && children.span_eq(&other.children)
            && *anchor_name == other.anchor_name
    }
}

impl SpanEq for Attr {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            name,
            axis,
            kind,
            span: _,
        } = self;
        *name == other.name && *axis == other.axis && kind.span_eq(&other.kind)
    }
}

impl SpanEq for AttrKind {
    fn span_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Prop { value }, Self::Prop { value: v2 })
            | (Self::Spread { value }, Self::Spread { value: v2 }) => value.span_eq(v2),
            (
                Self::Event { payload, action },
                Self::Event {
                    payload: p2,
                    action: a2,
                },
            ) => payload == p2 && action.span_eq(a2),
            _ => false,
        }
    }
}

impl SpanEq for StateBlock {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            states,
            attrs,
            viewport,
            span: _,
        } = self;
        *states == other.states && attrs.span_eq(&other.attrs) && viewport.span_eq(&other.viewport)
    }
}

impl SpanEq for ViewportCond {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            axis,
            op,
            breakpoint,
        } = self;
        *axis == other.axis && *op == other.op && breakpoint.span_eq(&other.breakpoint)
    }
}

impl SpanEq for Breakpoint {
    fn span_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Named(a, _), Self::Named(b, _)) => a == b,
            (Self::Px(a), Self::Px(b)) => a == b,
            _ => false,
        }
    }
}

impl SpanEq for StyleRule {
    fn span_eq(&self, other: &Self) -> bool {
        let Self {
            class,
            attrs,
            span: _,
        } = self;
        *class == other.class && attrs.span_eq(&other.attrs)
    }
}

impl SpanEq for Arg {
    fn span_eq(&self, other: &Self) -> bool {
        let Self { name, value } = self;
        *name == other.name && value.span_eq(&other.value)
    }
}

impl SpanEq for StrPart {
    fn span_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Interp(e, d), Self::Interp(e2, d2)) => d == d2 && e.span_eq(e2),
            _ => false,
        }
    }
}

/// A named `(Symbol, Expr)` field, as a record literal holds them.
impl SpanEq for (Symbol, Expr) {
    fn span_eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1.span_eq(&other.1)
    }
}

impl SpanEq for Expr {
    #[allow(clippy::too_many_lines)]
    fn span_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::IntLit(a, _), Self::IntLit(b, _)) => a == b,
            (Self::FloatLit(a, _), Self::FloatLit(b, _))
            | (Self::AngleLit(a, _), Self::AngleLit(b, _)) => a == b,
            (Self::StrLit(a, _), Self::StrLit(b, _)) => a.span_eq(b),
            (Self::Ident(a, _), Self::Ident(b, _))
            | (Self::ClassRef(a, _), Self::ClassRef(b, _)) => a == b,
            (Self::Array(a, _), Self::Array(b, _)) | (Self::Block(a, _), Self::Block(b, _)) => {
                a.span_eq(b)
            }
            (Self::Tuple(a, _), Self::Tuple(b, _)) => a.span_eq(b),
            (
                Self::Member {
                    base,
                    field,
                    span: _,
                },
                Self::Member {
                    base: b2,
                    field: f2,
                    span: _,
                },
            ) => field == f2 && base.span_eq(b2),
            (
                Self::Call {
                    callee,
                    args,
                    span: _,
                },
                Self::Call {
                    callee: c2,
                    args: a2,
                    span: _,
                },
            ) => callee.span_eq(c2) && args.span_eq(a2),
            (
                Self::ControllerCall {
                    call,
                    ok,
                    err,
                    span: _,
                },
                Self::ControllerCall {
                    call: c2,
                    ok: o2,
                    err: e2,
                    span: _,
                },
            ) => call.span_eq(c2) && ok.span_eq(o2) && err.span_eq(e2),
            (
                Self::Lambda {
                    params,
                    types,
                    body,
                    span: _,
                },
                Self::Lambda {
                    params: p2,
                    types: t2,
                    body: b2,
                    span: _,
                },
            ) => params == p2 && types.span_eq(t2) && body.span_eq(b2),
            (
                Self::Assign {
                    target,
                    op,
                    value,
                    span: _,
                },
                Self::Assign {
                    target: t2,
                    op: o2,
                    value: v2,
                    span: _,
                },
            ) => op == o2 && target.span_eq(t2) && value.span_eq(v2),
            (
                Self::Postfix {
                    target,
                    op,
                    span: _,
                },
                Self::Postfix {
                    target: t2,
                    op: o2,
                    span: _,
                },
            ) => op == o2 && target.span_eq(t2),
            (
                Self::Unary { op, rhs, span: _ },
                Self::Unary {
                    op: o2,
                    rhs: r2,
                    span: _,
                },
            ) => op == o2 && rhs.span_eq(r2),
            (
                Self::Index {
                    base,
                    index,
                    span: _,
                },
                Self::Index {
                    base: b2,
                    index: i2,
                    span: _,
                },
            ) => base.span_eq(b2) && index.span_eq(i2),
            (
                Self::Record {
                    fields,
                    spread,
                    span: _,
                },
                Self::Record {
                    fields: f2,
                    spread: s2,
                    span: _,
                },
            ) => fields.span_eq(f2) && spread.span_eq(s2),
            (
                Self::Binary {
                    op,
                    lhs,
                    rhs,
                    span: _,
                },
                Self::Binary {
                    op: o2,
                    lhs: l2,
                    rhs: r2,
                    span: _,
                },
            ) => op == o2 && lhs.span_eq(l2) && rhs.span_eq(r2),
            (
                Self::Ternary {
                    cond,
                    then,
                    els,
                    span: _,
                },
                Self::Ternary {
                    cond: c2,
                    then: t2,
                    els: e2,
                    span: _,
                },
            ) => cond.span_eq(c2) && then.span_eq(t2) && els.span_eq(e2),
            (
                Self::If {
                    cond,
                    then,
                    els,
                    span: _,
                },
                Self::If {
                    cond: c2,
                    then: t2,
                    els: e2,
                    span: _,
                },
            ) => cond.span_eq(c2) && then.span_eq(t2) && els.span_eq(e2),
            (
                Self::Let {
                    name,
                    ty,
                    value,
                    span: _,
                },
                Self::Let {
                    name: n2,
                    ty: t2,
                    value: v2,
                    span: _,
                },
            ) => name == n2 && ty.span_eq(t2) && value.span_eq(v2),
            (
                Self::Animated {
                    value,
                    anim,
                    span: _,
                },
                Self::Animated {
                    value: v2,
                    anim: a2,
                    span: _,
                },
            ) => value.span_eq(v2) && anim.span_eq(a2),
            (
                Self::KeyframeStep {
                    percent,
                    value,
                    easing,
                    span: _,
                },
                Self::KeyframeStep {
                    percent: p2,
                    value: v2,
                    easing: e2,
                    span: _,
                },
            ) => {
                percent == p2
                    && value.span_eq(v2)
                    && match (easing, e2) {
                        (Some((a, _)), Some((b, _))) => a == b,
                        (None, None) => true,
                        _ => false,
                    }
            }
            (
                Self::StyleValue {
                    attrs,
                    states,
                    span: _,
                },
                Self::StyleValue {
                    attrs: a2,
                    states: s2,
                    span: _,
                },
            ) => attrs.span_eq(a2) && states.span_eq(s2),
            (
                Self::Merge {
                    left,
                    right,
                    span: _,
                },
                Self::Merge {
                    left: l2,
                    right: r2,
                    span: _,
                },
            ) => left.span_eq(l2) && right.span_eq(r2),
            (Self::Error(_), Self::Error(_)) => true,
            _ => false,
        }
    }
}
