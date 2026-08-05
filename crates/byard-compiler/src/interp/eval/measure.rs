//! Element self-measurement (RFC-0038): `on measure => size = it`.
//!
//! Layout already computes every element's rect. This module is the one wire
//! that carries it back to the element that owns it, and everything here is
//! shaped by two properties that wire must have.
//!
//! **It fires on change, not per frame.** A static layout must cost nothing, or
//! the event turns every screen into a per-frame reactive write and the
//! RFC-0032 dirty model collapses under it. So each slot remembers the size it
//! last delivered and compares against it, through
//! [`f32::to_bits`](f32::to_bits) rather than the floats themselves, for
//! exactly the reason RFC-0032's fingerprints hash bits: `-0.0 == 0.0` would
//! make a collapsed element permanently *clean*, silently.
//!
//! **It reads layout, and never feeds it.** The rect is final for the frame
//! before anything here runs (INV-29), and the write goes out through the
//! ordinary reactive path, so a parent measuring itself for a child's benefit
//! is one frame of settling and then nothing. The dangerous shape, an element
//! feeding its measured size back into its *own* layout, is caught at compile
//! time when it is statically visible ([`Interpreter::lower_measures`]), and
//! bounded to one fire per element per frame when it is not, with a warning
//! that names the declaration rather than leaving a developer staring at a
//! twitching layout.

use std::cell::RefCell;
use std::rc::Rc;

use crate::diagnostics::{CompileError, Span};
use crate::parser::ast::{Attr, AttrKind, ElementNode, Expr, Member, StrPart};
use crate::symbol::Symbol;

use super::super::env::Value;
use super::super::intrinsics::AttrClass;
use super::{Interpreter, MEASURE_FEEDBACK_FLIPS, MeasureSlot, PerfWarning};

/// The name an `on measure` action binds its payload to.
const PAYLOAD: &str = "it";

impl Interpreter {
    /// Consumes `el`'s `on measure` declarations (RFC-0038), returning the slot
    /// it allocated and the element's remaining members.
    ///
    /// The members are returned borrowed unless a declaration was removed, so
    /// an element without the event pays one `is_empty`-shaped scan and no
    /// allocation.
    pub(super) fn lower_measures<'a>(
        &mut self,
        el: &'a ElementNode,
        attrs: &[Attr],
    ) -> (Option<u32>, std::borrow::Cow<'a, [Member]>) {
        if !el
            .children
            .iter()
            .any(|m| matches!(m, Member::Measure { .. }))
        {
            return (None, std::borrow::Cow::Borrowed(&el.children));
        }
        let mut slot = None;
        let mut first: Option<Span> = None;
        let mut kept = Vec::with_capacity(el.children.len());
        for member in &el.children {
            let Member::Measure { action, span } = member else {
                kept.push(member.clone());
                continue;
            };
            // A second declaration on one element is refused rather than
            // silently outranked or silently doubled (INV-4): both would fire,
            // both would write, and which one won would be an ordering detail
            // no reader of the source could see.
            if let Some(first) = first {
                self.errors
                    .push(CompileError::DuplicateMeasure { span: *span, first });
                continue;
            }
            first = Some(*span);
            self.check_measure_cycle(el, attrs, action, *span);
            if let Ok(lowered) = self.lower_action(action, Some(Symbol::intern(PAYLOAD))) {
                slot = Some(self.register_measure(lowered, *span));
            }
        }
        (slot, std::borrow::Cow::Owned(kept))
    }

    /// Allocates a measure slot for a lowered action.
    fn register_measure(&mut self, action: super::super::events::Action, span: Span) -> u32 {
        let index = self.measures.len();
        self.measures.push(MeasureSlot {
            action: Rc::new(RefCell::new(action)),
            last: None,
            prev: None,
            flips: 0,
            fired_frame: 0,
            span,
        });
        // A program with four billion `on measure` declarations has a problem
        // this cast is not part of.
        #[allow(clippy::cast_possible_truncation)]
        {
            index as u32
        }
    }

    /// Raises [`CompileError::MeasureFeedback`] when an element's own
    /// layout-class attribute reads a binding its `on measure` writes
    /// (RFC-0038 "no feedback loop", the statically visible half).
    ///
    /// Deliberately narrow: it catches the direct shape, and says nothing about
    /// a value laundered through a `let` or an ancestor, which is the runtime
    /// clamp's job. A cycle check that guessed would either refuse the safe and
    /// common case (a parent sizing a child) or claim a guarantee it cannot
    /// keep.
    fn check_measure_cycle(&mut self, el: &ElementNode, attrs: &[Attr], action: &Expr, span: Span) {
        let mut written = Vec::new();
        collect_assigned(action, &mut written);
        if written.is_empty() {
            return;
        }
        let Some(intrinsic) = super::super::intrinsics::lookup(el.name.as_str()) else {
            return;
        };
        for attr in attrs {
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            if intrinsic.property_class(attr.name.as_str()) != Some(AttrClass::Layout) {
                continue;
            }
            let Some(name) = written.iter().find(|w| reads_binding(value, w)) else {
                continue;
            };
            self.errors.push(CompileError::MeasureFeedback {
                span,
                prop: attr.name.as_str().to_string(),
                binding: name.as_str().to_string(),
            });
            return;
        }
    }

    /// Fires this frame's `on measure` events (RFC-0038), for the elements
    /// whose rect changed.
    ///
    /// Runs after layout is final and before the paint walk. The write lands in
    /// the ordinary reactive path, so what a handler changes is drawn on the
    /// next frame, the same as any other write from an event.
    pub(super) fn fire_measures(&mut self) {
        if self.measure_targets.is_empty() {
            return;
        }
        let frame = self.frame_seq;
        let targets = std::mem::take(&mut self.measure_targets);
        for (node, index) in &targets {
            let Ok(Some(rect)) = self.atlas.resolved_rect(*node) else {
                continue;
            };
            let size = (rect.width.to_bits(), rect.height.to_bits());
            let Some(slot) = self.measures.get_mut(*index as usize) else {
                continue;
            };
            // One resolve per frame, per element. The same lowered element can
            // reach the walk twice (a view's child block spliced into two call
            // sites), and a feedback loop must be bounded even when it does.
            if slot.fired_frame == frame || slot.last == Some(size) {
                continue;
            }
            let flipped_back = slot.prev == Some(size);
            slot.prev = slot.last;
            slot.last = Some(size);
            slot.fired_frame = frame;
            slot.flips = if flipped_back { slot.flips + 1 } else { 0 };
            let warn = (slot.flips == MEASURE_FEEDBACK_FLIPS).then_some(slot.span);
            let action = Rc::clone(&slot.action);
            let payload = Value::Record(vec![
                (Symbol::intern("w"), Value::Float(f64::from(rect.width))),
                (Symbol::intern("h"), Value::Float(f64::from(rect.height))),
            ]);
            (action.borrow_mut())(&mut self.ctx, Some(&payload));
            if let Some(span) = warn {
                self.perf_warnings
                    .push(PerfWarning::MeasureFeedback { span });
            }
        }
        // Hand the buffer back rather than dropping it: the targets are
        // rebuilt every frame, and this is the allocation they reuse.
        self.measure_targets = targets;
        self.measure_targets.clear();
    }
}

/// Collects the names an action assigns to (`x = …`, `x += …`, `x++`).
fn collect_assigned(expr: &Expr, out: &mut Vec<Symbol>) {
    if let Expr::Assign { target, .. } | Expr::Postfix { target, .. } = expr {
        if let Expr::Ident(name, _) = target.as_ref() {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    }
    for child in children(expr) {
        collect_assigned(child, out);
    }
}

/// Whether `expr` reads `name`, directly or as the base of a field access.
fn reads_binding(expr: &Expr, name: &Symbol) -> bool {
    if matches!(expr, Expr::Ident(id, _) if id == name) {
        return true;
    }
    children(expr).into_iter().any(|c| reads_binding(c, name))
}

/// Every sub-expression of `expr`, in written order.
///
/// Exhaustive on purpose, with no wildcard arm: a new expression form has to
/// declare whether it can hold a read or a write, rather than defaulting to
/// "invisible to both checks above" and quietly weakening them.
fn children(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::IntLit(..)
        | Expr::FloatLit(..)
        | Expr::AngleLit(..)
        | Expr::Ident(..)
        | Expr::ClassRef(..)
        | Expr::Error(_) => Vec::new(),
        Expr::StrLit(parts, _) => parts
            .iter()
            .filter_map(|p| match p {
                StrPart::Interp(e) => Some(e.as_ref()),
                StrPart::Text(_) => None,
            })
            .collect(),
        Expr::Array(items, _) | Expr::Block(items, _) => items.iter().collect(),
        Expr::Tuple(args, _) => args.iter().map(|a| &a.value).collect(),
        Expr::Member { base, .. }
        | Expr::Postfix { target: base, .. }
        | Expr::Unary { rhs: base, .. }
        | Expr::Lambda { body: base, .. }
        | Expr::KeyframeStep { value: base, .. } => vec![base.as_ref()],
        Expr::Call { callee, args, .. } => std::iter::once(callee.as_ref())
            .chain(args.iter().map(|a| &a.value))
            .collect(),
        Expr::Assign { target, value, .. } => vec![target.as_ref(), value.as_ref()],
        Expr::Binary { lhs, rhs, .. }
        | Expr::Merge {
            left: lhs,
            right: rhs,
            ..
        } => {
            vec![lhs.as_ref(), rhs.as_ref()]
        }
        Expr::Index { base, index, .. } => vec![base.as_ref(), index.as_ref()],
        Expr::Animated { value, anim, .. } => vec![value.as_ref(), anim.as_ref()],
        Expr::Record { fields, spread, .. } => fields
            .iter()
            .map(|(_, v)| v)
            .chain(spread.as_deref())
            .collect(),
        Expr::Ternary {
            cond, then, els, ..
        } => vec![cond.as_ref(), then.as_ref(), els.as_ref()],
        Expr::StyleValue { attrs, states, .. } => attrs
            .iter()
            .chain(states.iter().flat_map(|s| s.attrs.iter()))
            .map(|a| match &a.kind {
                AttrKind::Prop { value } | AttrKind::Spread { value } => value,
                AttrKind::Event { action, .. } => action,
            })
            .collect(),
        Expr::ControllerCall { call, ok, err, .. } => std::iter::once(call.as_ref())
            .chain([ok, err].into_iter().flatten().map(|a| a.action.as_ref()))
            .collect(),
    }
}
