//! Style states, responsive blocks and style spreads (RFC-0012, RFC-0016,
//! RFC-0024).
//!
//! An element's base attributes and its `on <state>` blocks stay separate
//! until paint, so a state resolves each frame against the live mask: the
//! router's pointer, focus and drag bits, the prop-driven ones (`checked`,
//! `selected`, `invalid`), and a responsive block's viewport condition. This is
//! that resolution, the `..style` spreads and style values that produce the
//! base and the blocks, and the check that every breakpoint a block names is
//! one the theme declares.

use super::{Interpreter, StyleDef};
use crate::diagnostics::CompileError;
use crate::interp::env::Value;
use crate::parser::ast::{Attr, AttrKind, Expr, StateBlock, StyleStateKind};

/// Inserts `attr` into a resolved style set, replacing any existing attribute
/// with the same name and sub-property axis (last-wins) or appending it, so a
/// spread/inline override cleanly supersedes an earlier value (RFC-0016).
fn override_attr(set: &mut Vec<Attr>, attr: Attr) {
    if let Some(existing) = set
        .iter_mut()
        .find(|a| a.name == attr.name && a.axis == attr.axis)
    {
        *existing = attr;
    } else {
        set.push(attr);
    }
}

/// Builds a flat attribute list for *validation only* (RFC-0016): the base
/// attributes followed by every `on <state>` block's attributes, so a state
/// block's `bg:`/`scale:`/… is checked against the intrinsic's §5 contract just
/// like an inline attribute. Never emitted, rendering keeps base and states
/// separate so states resolve per-frame against the live mask.
pub(super) fn attrs_with_states(base: &[Attr], states: &[StateBlock]) -> Vec<Attr> {
    if states.is_empty() {
        return base.to_vec();
    }
    let mut all = base.to_vec();
    for sb in states {
        all.extend(sb.attrs.iter().cloned());
    }
    all
}

/// The `StyleState` bit a single [`StyleStateKind`] maps to (RFC-0024).
fn state_bit(kind: StyleStateKind) -> crate::interp::events::StyleState {
    use crate::interp::events::StyleState;
    match kind {
        StyleStateKind::Hover => StyleState::HOVER,
        StyleStateKind::Pressed => StyleState::PRESSED,
        StyleStateKind::Focused => StyleState::FOCUSED,
        StyleStateKind::Disabled => StyleState::DISABLED,
        StyleStateKind::Checked => StyleState::CHECKED,
        StyleStateKind::Selected => StyleState::SELECTED,
        StyleStateKind::Invalid => StyleState::INVALID,
        StyleStateKind::Indeterminate => StyleState::INDETERMINATE,
        StyleStateKind::Dragging => StyleState::DRAGGING,
    }
}

/// Whether a responsive block's viewport condition holds (RFC-0016).
///
/// A breakpoint nobody declared never holds. It has already been reported
/// when the view was lowered, and reading it as "always" or as zero would make
/// the block apply at every size, which is the one outcome worse than not
/// applying at all.
pub(super) fn viewport_holds(
    cond: &crate::parser::ast::ViewportCond,
    viewport: (f32, f32),
    theme: &super::theme::Theme,
) -> bool {
    use crate::parser::ast::{Breakpoint, ViewportAxis, ViewportOp};
    let at = match &cond.breakpoint {
        Breakpoint::Px(px) => *px,
        Breakpoint::Named(name, _) => match theme.breakpoint(name.as_str()) {
            Some(px) => px,
            None => return false,
        },
    };
    let extent = match cond.axis {
        ViewportAxis::Width => viewport.0,
        ViewportAxis::Height => viewport.1,
    };
    match cond.op {
        ViewportOp::AtLeast => extent >= at,
        ViewportOp::Below => extent < at,
    }
}

/// The combined-selector mask a state block requires (RFC-0024): every state in
/// `states` must be active for the block to apply.
fn state_block_mask(sb: &StateBlock) -> crate::interp::events::StyleState {
    sb.states
        .iter()
        .fold(crate::interp::events::StyleState::empty(), |m, &k| {
            m.union(state_bit(k))
        })
}

/// Resolves an element's effective attributes for the current interaction state
/// (RFC-0016 §"Resolution order", extended by RFC-0024 §2): a block applies when
/// its required mask is a **subset** of the live `StyleState` (all its states are
/// active). Matching blocks overlay the base last-wins, ordered by **specificity**
/// (number of states, a combined `on focused+hover` beats a single `on hover`)
/// then **declaration order** for equal specificity.
///
/// The common stateless case (no blocks) borrows the base with no allocation.
///
/// A responsive block (RFC-0016, `on width >= md`) also has to have its viewport
/// condition hold, which `holds` answers. It counts as one unit of specificity,
/// the same as a single interaction state, so declaration order settles a tie
/// between `on hover` and `on width >= md` exactly as it settles one between two
/// states.
pub(super) fn resolve_state_attrs<'a>(
    base: &'a [Attr],
    state_blocks: &[StateBlock],
    active: crate::interp::events::StyleState,
    holds: &dyn Fn(&crate::parser::ast::ViewportCond) -> bool,
) -> std::borrow::Cow<'a, [Attr]> {
    if state_blocks.is_empty() {
        return std::borrow::Cow::Borrowed(base);
    }
    // Collect blocks whose full mask is active, tagged with (specificity, order).
    let mut matching: Vec<(u32, usize)> = state_blocks
        .iter()
        .enumerate()
        .filter_map(|(i, sb)| {
            let required = state_block_mask(sb);
            let viewport_ok = sb.viewport.as_ref().is_none_or(holds);
            let spec = required.count() + u32::from(sb.viewport.is_some());
            (active.contains(required) && viewport_ok).then_some((spec, i))
        })
        .collect();
    if matching.is_empty() {
        return std::borrow::Cow::Borrowed(base);
    }
    // Apply lowest-specificity first, then declaration order, so a more specific
    // (or later) block wins on conflicting properties, the `(spec, idx)` tuples
    // sort lexicographically.
    matching.sort_unstable();
    let mut resolved = base.to_vec();
    for (_, idx) in matching {
        for a in &state_blocks[idx].attrs {
            override_state_attr(&mut resolved, a.clone());
        }
    }
    std::borrow::Cow::Owned(resolved)
}

/// Inserts a state-block attr over the resolved base set, keeping the base's
/// `with` animation shell when the state provides a bare value, the
/// RFC-0010 × RFC-0012/0016 state-driven-animation contract:
/// `blur: 0 with anim.spring()` + `on hover { blur: 16 }` must *animate* to
/// 16, not pop. The state changes the target; the base owns the curve. The
/// wrapped value reuses the base `Animated` node's span, which is the
/// persisted `Motion`'s key, so entering and leaving the state retargets
/// one interruptible animation instead of restarting a fresh one each flip.
fn override_state_attr(set: &mut Vec<Attr>, attr: Attr) {
    let Some(existing) = set
        .iter_mut()
        .find(|a| a.name == attr.name && a.axis == attr.axis)
    else {
        set.push(attr);
        return;
    };
    if let (
        AttrKind::Prop {
            value: Expr::Animated { anim, span, .. },
        },
        AttrKind::Prop { value: incoming },
    ) = (&existing.kind, &attr.kind)
    {
        if !matches!(incoming, Expr::Animated { .. }) {
            let wrapped = Attr {
                kind: AttrKind::Prop {
                    value: Expr::Animated {
                        value: Box::new(incoming.clone()),
                        anim: anim.clone(),
                        span: *span,
                    },
                },
                ..attr.clone()
            };
            *existing = wrapped;
            return;
        }
    }
    *existing = attr;
}

impl Interpreter {
    /// The prop/value-driven style states an element contributes (RFC-0024), on
    /// top of the router's pointer/focus/drag states: `checked` (a value-widget's
    /// bound value is true), `selected` (the `selected:` prop, or a `RadioButton`
    /// whose `bind == value`), `invalid` (the `invalid:` prop), and
    /// `indeterminate` (a `Checkbox`'s mixed prop). `checked` and `indeterminate`
    /// are mutually exclusive, `indeterminate` clears `checked` (RFC-0024).
    pub(super) fn prop_style_state(
        &mut self,
        attrs: &[Attr],
        bound_sig: Option<super::env::SignalId>,
        name: &str,
    ) -> crate::interp::events::StyleState {
        use crate::interp::events::StyleState;
        let mut s = StyleState::empty();
        if self.eval_bool_prop(attrs, "selected") == Some(true) {
            s = s.union(StyleState::SELECTED);
        }
        if self.eval_bool_prop(attrs, "invalid") == Some(true) {
            s = s.union(StyleState::INVALID);
        }
        let indeterminate =
            name == "Checkbox" && self.eval_bool_prop(attrs, "indeterminate") == Some(true);
        if indeterminate {
            s = s.union(StyleState::INDETERMINATE);
        }
        // `checked` from a value-widget's bound bool, suppressed while mixed.
        if matches!(name, "Checkbox" | "Toggle") && !indeterminate {
            let checked =
                bound_sig.is_some_and(|sig| self.ctx.peek_signal(sig).as_bool().unwrap_or(false));
            if checked {
                s = s.union(StyleState::CHECKED);
            }
        }
        // `selected` from a `RadioButton` whose group var equals its value.
        if name == "RadioButton" {
            let value = self.eval_str_prop(attrs, "value").unwrap_or_default();
            let selected = bound_sig.is_some_and(
                |sig| matches!(self.ctx.peek_signal(sig), Value::Str(v) if v == value),
            );
            if selected {
                s = s.union(StyleState::SELECTED);
            }
        }
        s
    }

    /// Reports every responsive block that names a breakpoint the theme does
    /// not declare (RFC-0016), once per written block.
    ///
    /// Once per *span*, because the same element is lowered again for every
    /// row of a `for` and on every re-lowering, and a diagnostic repeated per
    /// row is a diagnostic nobody reads.
    pub(super) fn check_breakpoints(&mut self, blocks: &[StateBlock]) {
        for block in blocks {
            let Some(crate::parser::ast::ViewportCond {
                breakpoint: crate::parser::ast::Breakpoint::Named(name, span),
                ..
            }) = &block.viewport
            else {
                continue;
            };
            if self.theme.breakpoint(name.as_str()).is_some() {
                continue;
            }
            let already = self
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::UnknownBreakpoint { span: s, .. } if s == span));
            if already {
                continue;
            }
            let names = self.theme.breakpoint_names();
            self.errors.push(CompileError::UnknownBreakpoint {
                span: *span,
                name: name.as_str().to_string(),
                hint: crate::util::closest_match(name.as_str(), names.iter().map(String::as_str))
                    .map(str::to_string),
            });
        }
    }

    /// Expands `..style` spreads (RFC-0016) in an attribute list into a flat
    /// set: each spread splices the referenced style's attributes in written
    /// order (a later spread overrides an earlier one), then inline attributes
    /// override every spread. The common, spread-free case returns a plain
    /// clone with no work. A spread that doesn't resolve to a known style is a
    /// [`CompileError::NotAStyle`].
    pub(super) fn expand_style_spreads(&mut self, attrs: &[Attr]) -> (Vec<Attr>, Vec<StateBlock>) {
        if !attrs
            .iter()
            .any(|a| matches!(a.kind, AttrKind::Spread { .. }))
        {
            return (attrs.to_vec(), Vec::new());
        }
        let mut resolved: Vec<Attr> = Vec::new();
        let mut states: Vec<StateBlock> = Vec::new();
        // 1) Spreads first, in written order. Each spread contributes its base
        //    attributes (last-write-wins) and appends its `on <state>` blocks
        //    (a later spread's block of the same state wins at resolve time).
        for a in attrs {
            if let AttrKind::Spread { value } = &a.kind {
                match self.resolve_style_expr(value) {
                    Some(def) => {
                        for sa in def.base {
                            override_attr(&mut resolved, sa);
                        }
                        states.extend(def.states);
                    }
                    None => self.errors.push(CompileError::NotAStyle { span: a.span }),
                }
            }
        }
        // 2) Inline attributes win over the spreads.
        for a in attrs {
            if !matches!(a.kind, AttrKind::Spread { .. }) {
                override_attr(&mut resolved, a.clone());
            }
        }
        (resolved, states)
    }

    /// Resolves a style expression to a [`StyleDef`] (base attributes + state
    /// blocks): a `let`-bound style name, an inline `style { … }` value, or a
    /// `merge` of two styles.
    pub(super) fn resolve_style_expr(&self, value: &Expr) -> Option<StyleDef> {
        match value {
            Expr::Ident(name, _) => self.styles.get(name).cloned(),
            Expr::StyleValue { attrs, states, .. } => Some(StyleDef {
                base: attrs.clone(),
                states: states.clone(),
            }),
            // `a merge b` (RFC-0016): the right style overrides the left, its
            // base attributes overlay last-write-wins, and its state blocks are
            // appended so a later block of the same state wins at resolve time.
            Expr::Merge { left, right, .. } => {
                let mut def = self.resolve_style_expr(left)?;
                let over = self.resolve_style_expr(right)?;
                for a in over.base {
                    override_attr(&mut def.base, a);
                }
                def.states.extend(over.states);
                Some(def)
            }
            _ => None,
        }
    }
}
