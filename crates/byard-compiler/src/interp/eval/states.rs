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
use crate::symbol::Symbol;

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

/// The value a state-only attribute rests at, for a base that does not write
/// it: nothing visible (a fully transparent colour, no width, no offset).
/// `None` for an attribute with no such rest, which then keeps whatever the base
/// writes.
fn resting_value(name: &str, span: crate::diagnostics::Span) -> Option<Expr> {
    match name {
        // Transparent, and tagged as carrying alpha so it is not read as the
        // opaque black `0` would be.
        "overlay" | "outline" => Some(Expr::IntLit(crate::lexer::COLOR_HAS_ALPHA_TAG, span)),
        "outline_width" | "outline_offset" => Some(Expr::IntLit(0, span)),
        _ => None,
    }
}

/// RFC-0059: a state that moves an attribute with `with` moves it back on the
/// same curve when the state ends.
///
/// `override_state_attr` keeps the base's `with` when a block gives a bare
/// value. This closes the other direction, once, when the element is lowered,
/// so a frame costs what it did:
///
/// - the base value of an attribute a block animates takes the block's curve,
///   unless it has one of its own, and where the base does not write the
///   attribute at all and it has a resting value (`overlay`, `outline`, the
///   outline's width and offset) the rest is written for it;
/// - every block's animated value of the same attribute shares one animation
///   identity (the base's, else the first block's), so `hover` to `pressed`
///   retargets one motion and does not start a second.
///
/// An element with no state block, or none with a `with`, is not touched.
pub(super) fn inherit_state_motion(base: &mut Vec<Attr>, blocks: &mut [StateBlock]) {
    fn animated(a: &Attr) -> Option<(&Expr, crate::diagnostics::Span)> {
        match &a.kind {
            AttrKind::Prop {
                value: Expr::Animated { anim, span, .. },
            } => Some((anim, *span)),
            _ => None,
        }
    }
    let mut names: Vec<(crate::Symbol, Option<crate::Symbol>)> = Vec::new();
    for a in blocks.iter().flat_map(|b| &b.attrs) {
        if animated(a).is_some() && !names.iter().any(|(n, x)| *n == a.name && *x == a.axis) {
            names.push((a.name.clone(), a.axis.clone()));
        }
    }
    for (name, axis) in names {
        let first = blocks
            .iter()
            .flat_map(|b| &b.attrs)
            .find(|a| a.name == name && a.axis == axis && animated(a).is_some())
            .and_then(animated)
            .map(|(anim, span)| (anim.clone(), span));
        let Some((state_anim, state_span)) = first else {
            continue;
        };
        let at = base.iter().position(|a| a.name == name && a.axis == axis);
        // The base has a curve of its own, and with it the identity; else the
        // base takes the block's curve (written for it where there is none).
        let key = if let Some((_, span)) = at.and_then(|i| animated(&base[i])) {
            span
        } else {
            if let Some(i) = at {
                if let AttrKind::Prop { value } = &mut base[i].kind {
                    let inner = std::mem::replace(value, Expr::IntLit(0, state_span));
                    *value = Expr::Animated {
                        value: Box::new(inner),
                        anim: Box::new(state_anim),
                        span: state_span,
                    };
                }
            } else {
                let Some(rest) = resting_value(name.as_str(), state_span) else {
                    continue;
                };
                base.push(Attr {
                    name: name.clone(),
                    axis: axis.clone(),
                    kind: AttrKind::Prop {
                        value: Expr::Animated {
                            value: Box::new(rest),
                            anim: Box::new(state_anim),
                            span: state_span,
                        },
                    },
                    span: state_span,
                });
            }
            state_span
        };
        for a in blocks.iter_mut().flat_map(|b| &mut b.attrs) {
            if a.name != name || a.axis != axis {
                continue;
            }
            if let AttrKind::Prop {
                value: Expr::Animated { span, .. },
            } = &mut a.kind
            {
                *span = key;
            }
        }
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
        StyleStateKind::FocusVisible => StyleState::FOCUS_VISIBLE,
        StyleStateKind::FocusWithin => StyleState::FOCUS_WITHIN,
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
    remote: &std::collections::HashMap<u32, crate::interp::events::StyleState>,
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
            // RFC-0062: every state of another element named by the block holds
            // there too. A name that did not resolve, or an element not drawn
            // this frame, holds nothing.
            let remote_ok = sb.remote.iter().all(|r| {
                r.target
                    .and_then(|t| remote.get(&t))
                    .is_some_and(|m| m.contains(state_bit(r.kind)))
            });
            let spec = required.count() + sb.remote.len() as u32 + u32::from(sb.viewport.is_some());
            (active.contains(required) && viewport_ok && remote_ok).then_some((spec, i))
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
    /// The router's interaction states of element `index`, with `focus_within`
    /// added when a block asked for it (RFC-0062): held by the focused element
    /// and every ancestor of it in the layout tree. The chain is built the first
    /// time a frame asks, and nothing is built unless a block names the state.
    pub(super) fn element_style_state(&mut self, index: u32) -> crate::interp::events::StyleState {
        let state = self.router.style_state(index);
        if !self.state_uses.focus_within {
            return state;
        }
        if self.focus_chain.is_none() {
            let mut chain = Vec::new();
            let mut node = self
                .router
                .focused_element()
                .and_then(|f| self.atlas.node_at(f));
            while let Some(n) = node {
                chain.extend(self.atlas.node_index(n));
                node = self.atlas.parent_node(n);
            }
            self.focus_chain = Some(chain);
        }
        if self
            .focus_chain
            .as_ref()
            .is_some_and(|c| c.contains(&index))
        {
            state.union(crate::interp::events::StyleState::FOCUS_WITHIN)
        } else {
            state
        }
    }

    /// Publishes, before the frame paints, the pointer and focus states of every
    /// element a state block names (RFC-0062 §5), so that an element written
    /// *before* the one it follows reads this frame's. The element replaces it
    /// with its full mask when it paints, which also has the value states
    /// (`checked`, `selected`, `invalid`, `disabled`), so those reach the
    /// elements written after it.
    pub(super) fn publish_named_states(&mut self) {
        if self.remote_targets.is_empty() {
            return;
        }
        let table = self.elem_uids.clone();
        for (index, uid) in table {
            if self.remote_targets.contains(&uid) {
                let state = self.element_style_state(index);
                self.elem_states.insert(uid, state);
            }
        }
    }

    /// Declares, before a view's body (a `when` branch, a `for` body, a slot's
    /// block) is lowered, the identity of every element in it tagged `as name`,
    /// so that a state block can name an element written after it
    /// (RFC-0062 §5). The scan stops at a nested `when`, `for` or route, which
    /// declare their own, and costs nothing when no element is named.
    pub(super) fn declare_state_names(&mut self, members: &[crate::parser::ast::Member]) {
        fn scan(members: &[crate::parser::ast::Member], names: &mut Vec<Symbol>) {
            for member in members {
                if let crate::parser::ast::Member::Element(el) = member {
                    if let Some(name) = &el.anchor_name {
                        if !names.contains(name) {
                            names.push(name.clone());
                        }
                    }
                    scan(&el.children, names);
                }
            }
        }
        let mut names = Vec::new();
        scan(members, &mut names);
        for name in names {
            let uid = self.fresh_elem_uid();
            self.env.push(
                Symbol::intern(&format!("@{}", name.as_str())),
                Value::Int(i64::from(uid)),
            );
        }
    }

    /// Lowers a body that opens a scope of its own, `declare_state_names` first.
    pub(super) fn lower_scope(
        &mut self,
        members: &[crate::parser::ast::Member],
        known_views: &[&str],
    ) -> Vec<super::RenderNode> {
        self.declare_state_names(members);
        self.lower_members(members, known_views)
    }

    /// Resolves the names in `blocks` that refer to other elements (RFC-0062
    /// §5) against the elements tagged `as name` so far in this view, and
    /// notes which features the frame has to pay for. A name nothing declared
    /// is `UnknownStateTarget`.
    pub(super) fn resolve_remote_states(&mut self, blocks: &mut [StateBlock]) {
        for block in blocks {
            if block.states.contains(&StyleStateKind::FocusWithin)
                || block
                    .remote
                    .iter()
                    .any(|r| r.kind == StyleStateKind::FocusWithin)
            {
                self.state_uses.focus_within = true;
            }
            // Layout is computed before states, so a block that follows another
            // element's states, or focus within, can only paint.
            let state = block
                .remote
                .first()
                .map(|r| format!("{}.{}", r.name.as_str(), r.kind.as_str()))
                .or_else(|| {
                    block
                        .states
                        .contains(&StyleStateKind::FocusWithin)
                        .then(|| "focus_within".to_string())
                });
            if let Some(state) = state {
                for attr in &block.attrs {
                    if crate::interp::intrinsics::is_layout_attribute(attr.name.as_str()) {
                        self.errors.push(CompileError::LayoutInStateBlock {
                            span: attr.span,
                            name: attr.name.as_str().to_string(),
                            state: state.clone(),
                        });
                    }
                }
            }
            for remote in &mut block.remote {
                let key = Symbol::intern(&format!("@{}", remote.name.as_str()));
                if let Some(Value::Int(uid)) = self.env.lookup(&key) {
                    remote.target = u32::try_from(*uid).ok();
                    self.remote_targets.extend(remote.target);
                } else {
                    remote.target = None;
                    let names: Vec<String> = self
                        .env
                        .snapshot()
                        .into_iter()
                        .filter_map(|(k, _)| k.as_str().strip_prefix('@').map(str::to_string))
                        .collect();
                    let hint = crate::util::closest_match(
                        remote.name.as_str(),
                        names.iter().map(String::as_str),
                    )
                    .map(str::to_string);
                    self.errors.push(CompileError::UnknownStateTarget {
                        span: remote.span,
                        name: remote.name.as_str().to_string(),
                        hint,
                    });
                }
            }
        }
    }

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
        inherit_state_motion(&mut resolved, &mut states);
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
