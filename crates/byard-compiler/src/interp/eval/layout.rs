//! Building the layout tree (RFC-0005, RFC-0018, RFC-0032).
//!
//! Each frame the render tree is flattened into the concrete nodes to lay
//! out and paint (a `when` expands to its taken branch, a `for` to its live
//! pooled bodies), the `for` pools are reconciled against their lists, and
//! the result is built into the layout atlas: boxes and their flex style,
//! grids, stacks and windowed lists. The retained path builds the same tree
//! through the same walk, which is what makes it safe to reason about; the
//! state it keeps between frames lives here with the pools it describes.

use super::animation::AnimSlot;
use super::instance::MAX_INSTANCE_DEPTH;
use super::navigation::NavPool;
use super::overlay::OverlayLayout;
use super::scroll::WindowSpec;
use super::states::{resolve_state_attrs, viewport_holds};
use super::text_field::FieldLines;
use super::{Interpreter, RenderNode};
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::Value;
use crate::interp::reactive::ScopeId;

/// Whether the element being laid out is the direct child of a `ZStack`, which
/// places it in one grid cell where `align_self` and `grow` mean "fill the
/// stack" (RFC-0059).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Stacked {
    /// Not a direct child of a stack.
    #[default]
    No,
    /// A direct child of a stack.
    Yes,
}
use crate::parser::ast::{Attr, AttrKind, Member};
use crate::symbol::Symbol;

/// What a container's layout needs to apply its interaction states (RFC-0061):
/// its attributes as written, its `on <state>` blocks, and the signal a value
/// widget is bound to.
#[derive(Clone, Copy)]
pub(super) struct StateSource<'a> {
    pub(super) raw: &'a [Attr],
    pub(super) blocks: &'a [crate::parser::ast::StateBlock],
    pub(super) bound_sig: Option<super::super::env::SignalId>,
}

/// Every `for` pool reachable from `nodes` without leaving them, in any order.
pub(super) fn collect_pools(nodes: &[RenderNode], out: &mut Vec<usize>) {
    for n in nodes {
        match n {
            RenderNode::For { pool, .. } => out.push(*pool),
            RenderNode::Box { children, .. } | RenderNode::Overlay { children, .. } => {
                collect_pools(children, out);
            }
            _ => {}
        }
    }
}

/// One concrete node produced by expanding `when`/`for`, and the animation slot
/// it belongs to (RFC-0025).
///
/// The slot travels *with the node* rather than being recomputed by each
/// walker, because the expansion is the only place that knows it: after
/// flattening, a row of a `for` is indistinguishable from a node written
/// literally at that position.
#[derive(Clone, Copy)]
pub(crate) struct Concrete<'a> {
    pub(super) node: &'a RenderNode,
    pub(super) slot: super::animation::AnimSlot,
}

/// A borrowed view of the structural-reactivity caches (RFC-0018) and the
/// navigation stacks (RFC-0026), passed through the read-only build/paint phase
/// so `when`/`for`/`NavStack` expand consistently.
#[derive(Clone, Copy)]
pub(super) struct Pools<'a> {
    pub(super) fors: &'a [ForPool],
    pub(super) whens: &'a [WhenPool],
    pub(super) navs: &'a [NavPool],
}

/// A `when`'s lazily-lowered branch cache (RFC-0018). Each branch's AST is kept
/// and lowered only the first time the condition selects it, then reused, so an
/// untaken (possibly recursive) branch costs nothing until it is actually shown.
pub(super) struct WhenPool {
    /// The `then` branch AST.
    pub(super) then_ast: Vec<Member>,
    /// The `else` branch AST (empty when there is no `else`).
    pub(super) els_ast: Vec<Member>,
    /// Source range each branch spans, so a branch that goes away can take its
    /// animation state with it (RFC-0025: an animation lives and dies with its
    /// element). `(then, else)`.
    pub(super) branch_spans: (Span, Span),
    /// Which branch was selected on the previous reconcile, or `None` before the
    /// first one, the edge detector for that unmount.
    pub(super) last_take: Option<bool>,
    /// User-view names in scope at lower time.
    pub(super) known_views: Vec<String>,
    /// Instance env captured at lower time (RFC-0019), restored when lowering.
    pub(super) env_snapshot: Vec<(Symbol, super::env::Value)>,
    /// The view instance the `when` was written in, so a branch lowered later
    /// belongs to it and not to whatever was being lowered by then.
    pub(super) inst: u32,
    /// The slots of the view instance the `when` was written in (RFC-0062),
    /// put back while a branch is lowered so `content` and the named slots
    /// resolve there as they do outside it.
    pub(super) slots: Option<std::rc::Rc<super::SlotFrame>>,
    /// The contexts provided above the `when` (RFC-0065), put back while a
    /// branch is lowered so an `inject` in it finds them.
    pub(super) ctx: Option<std::rc::Rc<super::CtxNode>>,
    /// The lowered `then` branch, once first taken.
    pub(super) then: Option<Vec<RenderNode>>,
    /// The lowered `else` branch, once first taken.
    pub(super) els: Option<Vec<RenderNode>>,
    /// The reactive scopes lowering `then` opened, retired with the branch.
    pub(super) then_scopes: Vec<ScopeId>,
    /// The reactive scopes lowering `else` opened, retired with the branch.
    pub(super) els_scopes: Vec<ScopeId>,
}

/// A `for`'s reusable body pool (RFC-0018). Bodies are lowered once per slot and
/// reused across frames; each slot's element value lives in `item_slots[i]`,
/// which the driver rewrites from the current list before painting.
pub(super) struct ForPool {
    /// The loop variable name, bound to each slot's signal when lowering a body.
    pub(super) item_var: Symbol,
    /// The optional index variable (`for i, item in …`, RFC-0025). Bound to the
    /// slot's own index as a constant while its body is lowered, a pooled slot
    /// *is* its position, so the index never needs to be reactive.
    pub(super) index_var: Option<Symbol>,
    /// The loop body AST, re-lowered only when the pool grows to a new index.
    pub(super) body: Vec<Member>,
    /// User-view names in scope at lower time (for lowering new bodies).
    pub(super) known_views: Vec<String>,
    /// The instance env captured at lower time (RFC-0019), restored when lowering
    /// a new body so it resolves against the scope the `for` was written in.
    pub(super) env_snapshot: Vec<(Symbol, super::env::Value)>,
    /// The view instance the `for` was written in (see `WhenPool::inst`).
    pub(super) inst: u32,
    /// The slots of the view instance the `for` was written in (see
    /// `WhenPool::slots`).
    pub(super) slots: Option<std::rc::Rc<super::SlotFrame>>,
    /// The contexts provided above the `for` (see `WhenPool::ctx`).
    pub(super) ctx: Option<std::rc::Rc<super::CtxNode>>,
    /// One signal per pooled index, holding that slot's current element value.
    pub(super) item_slots: Vec<super::env::SignalId>,
    /// One lowered body per pooled index. A prefix of `item_slots`: a pool that
    /// shrank a long way drops the bodies it no longer needs (their nodes and
    /// scopes) and keeps the signals, so growing back lowers the rows again
    /// without growing the signal table.
    pub(super) bodies: Vec<Vec<RenderNode>>,
    /// The reactive scopes lowering each body opened (parallel to `bodies`),
    /// retired with the body.
    pub(super) slot_scopes: Vec<Vec<ScopeId>>,
    /// How many bodies are live (painted) this frame, the current list length.
    pub(super) len: usize,
}

/// What [`Interpreter::render`] remembers between frames so it can decide
/// whether the layout tree may be retained (RFC-0032 §R4).
///
/// Every field here exists to answer *one* eligibility question, and the
/// default answer is always "rebuild": a `None`/`false`/mismatched value takes
/// the full path. New structural mutations that nobody classified therefore
/// fall on the safe side by construction rather than by review.
#[derive(Default)]
pub(super) struct RetainedLayout {
    /// The atlas node ids the last successful build produced, in build order.
    /// Retained rather than local (it was `let flat_ids = Vec::new()` inside
    /// `render`) because the retained path **must reuse the stored ids**:
    /// `next_target_index()` is `nodes_by_index.len()`, so re-deriving them
    /// would silently reassign every id.
    pub(super) flat_ids: Vec<byard_core::atlas::layout::AtlasNodeId>,
    /// Viewport of the last successful build, a resize forces a rebuild.
    pub(super) viewport: Option<(f32, f32)>,
    /// How many overlays and how deep each navigation container was, so an
    /// overlay or route mount/unmount forces a rebuild.
    ///
    /// RFC-0032 §R4 justified this clause by saying those pools do not travel
    /// through `reconcile_structure`. They do: `reconcile_structure` descends
    /// into `RenderNode::Nav` via `reconcile_nav`, and an overlay mounts behind
    /// a `when`. Measured on every case the suite can construct, `shape` and
    /// `structure_changed` reject the same frames, so this is defence in
    /// depth, not the sole guard the RFC described, and it is kept as such: it
    /// is a *deny* clause, and the direction that costs a rebuild is the safe
    /// one. Deleting `structure_changed` leaves the overlay and route tests
    /// green on this clause alone, and vice versa.
    pub(super) shape: Option<(usize, Vec<usize>)>,
    /// Active colour scheme at the last successful build. A flip changes
    /// nearly every resolved value at once, so it forces a rebuild (RFC-0032
    /// §Q6), compared here rather than hooked onto the setter, because
    /// `theme.dark = …`, `bind: theme.dark` and the programmatic setter are
    /// three different write paths into the same signal and a hook would have
    /// to catch all three.
    pub(super) dark: Option<bool>,
    /// Set by [`Interpreter::reload`] and [`Interpreter::set_theme`]; cleared
    /// by the next render, which is forced to be a full one.
    pub(super) invalidated: bool,
}

/// Whether a body writes a `let` of its own (RFC-0061).
fn has_lets(members: &[crate::parser::ast::Member]) -> bool {
    members
        .iter()
        .any(|m| matches!(m, crate::parser::ast::Member::Let { .. }))
}

impl Interpreter {
    /// The structural shape a retained build must match (RFC-0032 §R4): how
    /// many overlays are mounted, and how deep / how many screens live each
    /// navigation container holds.
    ///
    /// Overlay mount/unmount and route push/pop change the node sequence
    /// without ever travelling through `reconcile_structure`, they are pools
    /// of their own, so they need their own clause rather than being covered
    /// by the structural signal that already exists.
    pub(super) fn layout_shape(overlays: usize, pools: Pools<'_>) -> (usize, Vec<usize>) {
        let navs = pools
            .navs
            .iter()
            .flat_map(|p| [p.entries.len(), p.live.len()])
            .collect();
        (overlays, navs)
    }

    /// Builds this frame's layout tree into the atlas and returns
    /// `(main container, render root)`, or `None` when there is nothing to lay
    /// out.
    ///
    /// Called with the atlas either freshly [`clear`](byard_core::atlas::LayoutAtlas::clear)ed
    /// (full path) or opened for a retained build, the walk is **identical**
    /// either way, which is the property that makes the retained path safe to
    /// reason about: there is no second implementation to drift.
    pub(super) fn build_layout_pass<'a>(
        &mut self,
        tree: &'a [RenderNode],
        pools: Pools<'a>,
        viewport: (f32, f32),
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
        overlays: &[&'a RenderNode],
        overlay_layouts: &mut Vec<OverlayLayout<'a>>,
    ) -> Option<(
        Option<byard_core::atlas::layout::AtlasNodeId>,
        byard_core::atlas::layout::AtlasNodeId,
    )> {
        let (width, height) = viewport;
        // Refilled by this walk (RFC-0038). Cleared rather than appended to,
        // because a retained build that rolls back runs the walk twice and the
        // second run is the one that describes the frame.
        self.measure_targets.clear();
        self.elem_uids.clear();
        self.elem_value_states.clear();
        // Expand reactive `when`/`for` at the root, then build each concrete node.
        let root_children = self.build_children(tree, pools, flat_ids);

        for ov in overlays {
            if let Some(layout) = self.build_overlay_layout(ov, pools) {
                overlay_layouts.push(layout);
            }
        }

        // The main content container (viewport-sized, column). `None` when the
        // whole view is nothing but overlays.
        let main_id = if root_children.is_empty() {
            None
        } else {
            let root_style =
                byard_core::atlas::layout::ContainerStyle::new(Some(width), Some(height))
                    .with_direction(byard_core::atlas::layout::FlexDir::Column);
            self.atlas.add_container(root_style, &root_children).ok()
        };

        // The render root: with no overlay it is the main container itself (the
        // pre-RFC-0017 shape, unchanged). With overlays it is a super-root
        // holding the main content plus each overlay wrapper as an absolute
        // sibling that neither displaces nor is displaced by the main tree.
        let root_id = if overlay_layouts.is_empty() {
            main_id
        } else {
            let mut super_children = Vec::new();
            if let Some(m) = main_id {
                super_children.push(m);
            }
            for ol in overlay_layouts.iter() {
                super_children.push(ol.wrapper_id);
            }
            let super_style =
                byard_core::atlas::layout::ContainerStyle::new(Some(width), Some(height))
                    .with_direction(byard_core::atlas::layout::FlexDir::Column);
            self.atlas.add_container(super_style, &super_children).ok()
        };

        // Set while the atlas is still `Building`, on the retained path
        // `end_retained_build` flips it to `Computed` immediately afterwards,
        // and `set_root` refuses to run in that state.
        let root = root_id?;
        self.atlas.set_root(root).ok()?;
        Some((main_id, root))
    }

    /// Flattens a node slice into the concrete nodes to lay out/paint this frame
    /// (RFC-0018 structural reactivity): a `When` expands to its taken branch
    /// (condition re-read live), a `For` to its live pooled bodies, recursively.
    /// A concrete node (`Box`/`Text`/…) passes through unchanged, its *own*
    /// children are expanded when it is built/walked, not here. Build, paint,
    /// `flat_len`, and overlay collection all funnel through this one function, so
    /// they agree on the exact node sequence and the flat-id cursor stays aligned.
    pub(super) fn expand_concrete<'a>(
        &self,
        nodes: &'a [RenderNode],
        pools: Pools<'a>,
    ) -> Vec<Concrete<'a>> {
        self.expand_concrete_in(nodes, pools, self.anim_slot)
    }

    /// [`expand_concrete`](Self::expand_concrete) with the enclosing animation
    /// slot passed explicitly, so a `for` can stamp its rows with theirs.
    ///
    /// A node passes through carrying whatever slot it inherited; a `for` body
    /// replaces it with that slot's own element signal. `when` is transparent,
    /// a branch is not an instance, it is the same one under a condition.
    fn expand_concrete_in<'a>(
        &self,
        nodes: &'a [RenderNode],
        pools: Pools<'a>,
        slot: AnimSlot,
    ) -> Vec<Concrete<'a>> {
        let mut out = Vec::new();
        for n in nodes {
            match n {
                RenderNode::When { cond, pool } => {
                    let take = self
                        .binding_value(*cond)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // The taken branch was lowered by `reconcile_structure`; an
                    // as-yet-unselected branch is `None` and expands to nothing.
                    if let Some(p) = pools.whens.get(*pool) {
                        let branch = if take {
                            p.then.as_ref()
                        } else {
                            p.els.as_ref()
                        };
                        if let Some(branch) = branch {
                            out.extend(self.expand_concrete_in(branch, pools, slot));
                        }
                    }
                }
                RenderNode::For { pool, .. } => {
                    if let Some(p) = pools.fors.get(*pool) {
                        for (i, body) in p.bodies.iter().take(p.len).enumerate() {
                            // The slot's own element signal, unique across the
                            // program, because a nested `for` is lowered once
                            // per outer slot and gets its own fresh signals.
                            let row = p
                                .item_slots
                                .get(i)
                                .map_or(slot, |s| AnimSlot { row: s.0, ..slot });
                            out.extend(self.expand_concrete_in(body, pools, row));
                        }
                    }
                }
                // An effect has no pixels and no layout box, so it is dropped
                // *here*, before anything downstream can count it. Giving it a
                // 0x0 leaf instead (the `Overlay` shape) would put it in the
                // flat-id sequence, which is the RFC-0032 retained path's
                // identity, for a node that can never draw.
                RenderNode::Effect { .. } => {}
                other => out.push(Concrete {
                    node: other,
                    slot: AnimSlot {
                        inst: other.inst(),
                        ..slot
                    },
                }),
            }
        }
        out
    }

    /// Lowers, once and offline, every branch that lowering leaves for later:
    /// both arms of every `when`, a body of every `for` (with an element that
    /// has no value yet), and every `route`/`tab` of every navigation
    /// container, nested ones included. What `byard check` runs after
    /// rendering the view, so a diagnostic that lowering raises is raised for
    /// code that is not on screen at the start either: a tab that is not the
    /// active one, a route the initial path does not match, a `when` that
    /// starts false, a list that has not loaded.
    ///
    /// Runtime lowering stays lazy and never calls this. Each distinct body is
    /// lowered once (a recursive view lowers its body once, not without end),
    /// and nothing is mounted: the result of lowering is dropped, only its
    /// diagnostics and the pools it registers are kept.
    pub fn lower_every_branch(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut seen = std::collections::HashSet::<u64>::new();
        let mut key = |tag: &str, body: &dyn std::fmt::Debug| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            tag.hash(&mut h);
            format!("{body:?}").hash(&mut h);
            seen.insert(h.finish())
        };
        // Pools appear while others are lowered; sweep until a sweep finds
        // nothing new. `seen` is what makes that end.
        loop {
            let mut changed = false;
            let mut i = 0;
            while i < self.when_pools.len() {
                for take in [true, false] {
                    let (lowered, ast) = if take {
                        (
                            self.when_pools[i].then.is_some(),
                            &self.when_pools[i].then_ast,
                        )
                    } else {
                        (
                            self.when_pools[i].els.is_some(),
                            &self.when_pools[i].els_ast,
                        )
                    };
                    if lowered || ast.is_empty() || !key(if take { "then" } else { "else" }, ast) {
                        continue;
                    }
                    let (nodes, scopes) = self.lower_when_branch(i, take);
                    if take {
                        self.when_pools[i].then = Some(nodes);
                        self.when_pools[i].then_scopes = scopes;
                    } else {
                        self.when_pools[i].els = Some(nodes);
                        self.when_pools[i].els_scopes = scopes;
                    }
                    changed = true;
                }
                i += 1;
            }
            let mut i = 0;
            while i < self.for_pools.len() {
                let p = &self.for_pools[i];
                if p.bodies.is_empty() && key("for", &(&p.item_var, &p.index_var, &p.body)) {
                    let slot = self.ctx.create_signal(Value::Unit);
                    let (nodes, scopes) = self.lower_for_body(i, slot);
                    // Held as the pool's one pooled body, with `len` still 0:
                    // it is never drawn, and the nested pools it registered
                    // stay reachable for the next sweep.
                    self.for_pools[i].item_slots.push(slot);
                    self.for_pools[i].bodies.push(nodes);
                    self.for_pools[i].slot_scopes.push(scopes);
                    changed = true;
                }
                i += 1;
            }
            let mut i = 0;
            while i < self.nav_pools.len() {
                for route in 0..self.nav_route_count(i) {
                    let print = self.nav_route_fingerprint(i, route);
                    if key("route", &print) {
                        self.lower_route_offline(i, route);
                        changed = true;
                    }
                }
                i += 1;
            }
            if !changed {
                break;
            }
        }
    }

    /// Lowers the members of a `for` or `when` body (RFC-0061): its `let`s are
    /// defined first, in the body's own scope, so the elements read them; then
    /// the body is lowered as a scope of its own, with its `as` names declared
    /// before any element reads them (RFC-0062). A view body defines its `let`s
    /// when the view is instantiated, which is why this is not in
    /// [`lower_members`](Self::lower_members).
    fn lower_body_members(
        &mut self,
        members: &[crate::parser::ast::Member],
        known_views: &[&str],
    ) -> Vec<RenderNode> {
        for m in members {
            if matches!(m, crate::parser::ast::Member::Let { .. }) {
                self.eval_member(m);
            }
        }
        self.lower_scope(members, known_views)
    }

    /// Lowers one branch of a `when` pool from its AST and the environment
    /// captured where the `when` was written, returning the nodes and the
    /// reactive scopes lowering opened. The one place a branch is lowered, so
    /// the render-time reconcile and the offline
    /// [`lower_every_branch`](Self::lower_every_branch) (`byard check`) cannot
    /// disagree about what a branch means.
    pub(super) fn lower_when_branch(
        &mut self,
        pool: usize,
        take: bool,
    ) -> (Vec<RenderNode>, Vec<ScopeId>) {
        let ast = if take {
            self.when_pools[pool].then_ast.clone()
        } else {
            self.when_pools[pool].els_ast.clone()
        };
        let known: Vec<String> = self.when_pools[pool].known_views.clone();
        let env_snap = self.when_pools[pool].env_snapshot.clone();
        let env_base = self.env.len();
        for (k, v) in &env_snap {
            self.env.push(k.clone(), v.clone());
        }
        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
        // A branch lowered here is lowered *away from where it was written*:
        // the scope it belongs to exists only as the snapshot just pushed, and
        // it is truncated again below. Anything inside the branch that resolves
        // later, an event action, an animated attribute reading the row it
        // belongs to, a nested `for`/`when` pool, has to capture that scope
        // now, or it will look for the row in an environment that no longer
        // has one.
        // A `let` in the branch has to be captured by the elements that read
        // it (RFC-0061), which they only do while the scope counts as restored.
        let restored = !env_snap.is_empty() || has_lets(&ast);
        self.restored_depth += u32::from(restored);
        let enclosing = std::mem::replace(&mut self.cur_inst, self.when_pools[pool].inst);
        let slots = self.when_pools[pool].slots.clone();
        let pushed = slots.is_some();
        if let Some(frame) = slots {
            self.slot_stack.push(frame);
        }
        let outer_ctx = std::mem::replace(&mut self.ctx_chain, self.when_pools[pool].ctx.clone());
        // The scopes the branch opens are recorded, so a route popped with this
        // `when` in it can retire them.
        self.ctx.push_scope_tracker();
        let nodes = self.lower_body_members(&ast, &known_refs);
        let scopes = self.ctx.pop_scope_tracker();
        if pushed {
            self.slot_stack.pop();
        }
        self.ctx_chain = outer_ctx;
        self.restored_depth -= u32::from(restored);
        self.cur_inst = enclosing;
        self.env.truncate(env_base);
        (nodes, scopes)
    }

    /// Lowers the next body of a `for` pool (index `bodies.len()`), reading its
    /// element from `slot`. Shared with the offline pass, like
    /// [`lower_when_branch`](Self::lower_when_branch).
    pub(super) fn lower_for_body(
        &mut self,
        pool: usize,
        slot: crate::interp::env::SignalId,
    ) -> (Vec<RenderNode>, Vec<ScopeId>) {
        // Clone the lowering inputs out first so the borrow on `self.for_pools`
        // is released before `lower_members` (which may append *nested* pools).
        let item_var = self.for_pools[pool].item_var.clone();
        let index_var = self.for_pools[pool].index_var.clone();
        #[allow(clippy::cast_possible_wrap)]
        let slot_index = self.for_pools[pool].bodies.len() as i64;
        let body_ast = self.for_pools[pool].body.clone();
        let known: Vec<String> = self.for_pools[pool].known_views.clone();
        let env_snap = self.for_pools[pool].env_snapshot.clone();
        let env_base = self.env.len();
        for (k, v) in &env_snap {
            self.env.push(k.clone(), v.clone());
        }
        self.env.push(item_var, Value::Signal(slot));
        if let Some(index_var) = index_var {
            self.env.push(index_var, Value::Int(slot_index));
        }
        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
        // Mark that we are lowering inside a `for` body so any event action
        // captures the element in its env snapshot (the binding is truncated
        // below before render, RFC-0027).
        self.for_depth += 1;
        let enclosing = std::mem::replace(&mut self.cur_inst, self.for_pools[pool].inst);
        let slots = self.for_pools[pool].slots.clone();
        let pushed = slots.is_some();
        if let Some(frame) = slots {
            self.slot_stack.push(frame);
        }
        let outer_ctx = std::mem::replace(&mut self.ctx_chain, self.for_pools[pool].ctx.clone());
        self.ctx.push_scope_tracker();
        let body_nodes = self.lower_body_members(&body_ast, &known_refs);
        let scopes = self.ctx.pop_scope_tracker();
        if pushed {
            self.slot_stack.pop();
        }
        self.ctx_chain = outer_ctx;
        self.for_depth -= 1;
        self.cur_inst = enclosing;
        self.env.truncate(env_base);
        (body_nodes, scopes)
    }

    /// Reconciles the reactive `for` pools before the paint walk (RFC-0018,
    /// coarse D7): reads each live `for`'s list, grows its pool to the list length
    /// (lowering a body the first time an index is needed), rewrites each slot's
    /// element signal, and records the live count. Returns `true` if any slot or
    /// pool changed, so the caller re-pulls to project the new values. Descends
    /// through `when` (taken branch) and `for` (live bodies) so nested loops
    /// reconcile too.
    pub(super) fn reconcile_structure(&mut self, nodes: &[RenderNode], depth: u32) -> bool {
        // Bound the reconcile recursion: a guarded recursion whose guard never
        // terminates (`when go { Recurse() }` with `go` always true) lowers a new
        // level each descent, so cap it here, the same role `instance_depth`
        // plays at lower time (RFC-0007 §4), but for the reconcile-time expansion.
        // Truncate with a diagnostic rather than overflow the stack (D4: never a
        // silent failure); dedup so a re-render doesn't spam the error list.
        if depth >= MAX_INSTANCE_DEPTH {
            let already = self
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::RecursiveView { .. }));
            if !already {
                self.errors.push(CompileError::RecursiveView {
                    span: crate::diagnostics::Span::new(0, 0),
                    path: format!(
                        "(reactive `when`/`for` recursion exceeded {MAX_INSTANCE_DEPTH})"
                    ),
                });
            }
            return false;
        }
        let mut dirtied = false;
        for n in nodes {
            match n {
                RenderNode::When { cond, pool } => {
                    let take = self
                        .binding_value(*cond)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // RFC-0025: an animation lives and dies with its element, so a
                    // branch that has just been unmounted drops its animation
                    // state, a spinner that comes back starts its turn again
                    // rather than resuming a stale phase. (This is the opposite of
                    // §2's *offscreen* rule, where the element is still mounted and
                    // must resume exactly where it paused.)
                    let previous = self.when_pools[*pool].last_take;
                    if previous.is_some_and(|was| was != take) {
                        let (then_span, els_span) = self.when_pools[*pool].branch_spans;
                        let gone = if take { els_span } else { then_span };
                        self.drop_animation_state(gone, self.when_pools[*pool].inst);
                        dirtied = true;
                    }
                    self.when_pools[*pool].last_take = Some(take);
                    // Lazily lower the taken branch on first selection (so an
                    // untaken recursive branch never lowers), then descend into it
                    // to reconcile any nested `when`/`for`.
                    let already = if take {
                        self.when_pools[*pool].then.is_some()
                    } else {
                        self.when_pools[*pool].els.is_some()
                    };
                    if !already {
                        let (nodes, scopes) = self.lower_when_branch(*pool, take);
                        if take {
                            self.when_pools[*pool].then = Some(nodes);
                            self.when_pools[*pool].then_scopes = scopes;
                        } else {
                            self.when_pools[*pool].els = Some(nodes);
                            self.when_pools[*pool].els_scopes = scopes;
                        }
                        dirtied = true;
                    }
                    // Descend into the (now-lowered) taken branch. Take it out to
                    // avoid aliasing `self.when_pools` during nested reconcile.
                    let branch = if take {
                        self.when_pools[*pool].then.take()
                    } else {
                        self.when_pools[*pool].els.take()
                    };
                    if let Some(branch) = branch {
                        dirtied |= self.reconcile_structure(&branch, depth + 1);
                        if take {
                            self.when_pools[*pool].then = Some(branch);
                        } else {
                            self.when_pools[*pool].els = Some(branch);
                        }
                    }
                }
                RenderNode::For { pool, list } => {
                    let items = match self.binding_value(*list) {
                        Some(Value::List(items)) => items,
                        _ => Vec::new(),
                    };
                    let new_len = items.len();
                    // Grow the pool: lower one body per newly-needed index, each
                    // reading its element from a fresh per-slot signal.
                    while self.for_pools[*pool].bodies.len() < new_len {
                        // A slot signal outlives its body (see `trim_for_pool`),
                        // so growing back after a shrink takes the one the index
                        // had rather than adding another to the signal table.
                        let reused = self.for_pools[*pool]
                            .item_slots
                            .get(self.for_pools[*pool].bodies.len())
                            .copied();
                        let slot = reused.unwrap_or_else(|| self.ctx.create_signal(Value::Unit));
                        let (body_nodes, scopes) = self.lower_for_body(*pool, slot);
                        // Index `*pool` is still valid (nested lowering only
                        // appended higher indices).
                        if reused.is_none() {
                            self.for_pools[*pool].item_slots.push(slot);
                        }
                        self.for_pools[*pool].bodies.push(body_nodes);
                        self.for_pools[*pool].slot_scopes.push(scopes);
                        dirtied = true;
                    }
                    // Update each live slot's element value (value-deduped).
                    for (i, item) in items.iter().enumerate() {
                        let slot = self.for_pools[*pool].item_slots[i];
                        if self.ctx.peek_signal(slot) != *item {
                            self.ctx.write_signal(slot, item.clone());
                            dirtied = true;
                        }
                    }
                    // RFC-0025: an animation lives and dies with its element,
                    // so a row that just left the list drops its state, a
                    // re-grown row starts fresh rather than resuming a stale
                    // timeline. The pool keeps a row's lowered nodes and its
                    // element signal for the next occupant of index `i` (up to
                    // the bound in `trim_for_pool`), which is exactly why the
                    // state has to be dropped explicitly: it would otherwise be
                    // waiting there for the next occupant.
                    let previous_len = self.for_pools[*pool].len;
                    if new_len < previous_len {
                        self.drop_slot_state(*pool, new_len, previous_len);
                        dirtied = true;
                    }
                    self.for_pools[*pool].len = new_len;
                    self.trim_for_pool(*pool);
                    // Reconcile nested loops inside the live bodies. Take the
                    // bodies out so nested growth (which mutates `self.for_pools`)
                    // can't alias this pool's own vector.
                    let bodies = std::mem::take(&mut self.for_pools[*pool].bodies);
                    for body in bodies.iter().take(new_len) {
                        dirtied |= self.reconcile_structure(body, depth + 1);
                    }
                    self.for_pools[*pool].bodies = bodies;
                }
                // RFC-0026: reconcile the navigation state, advance the
                // transition, and descend into whichever screens are live.
                RenderNode::Nav { pool, path } => {
                    dirtied |= self.reconcile_nav(*pool, *path, depth);
                }
                RenderNode::Box { children, .. } | RenderNode::Overlay { children, .. } => {
                    dirtied |= self.reconcile_structure(children, depth + 1);
                }
                // Being reached by this walk *is* being mounted (RFC-0028
                // §4b); `settle_effects` reads the marks once the walk has
                // reached its fixpoint.
                RenderNode::Effect { index } => self.mark_effect_seen(*index),
                _ => {}
            }
        }
        dirtied
    }

    /// Builds the atlas layout for a node slice, expanding reactive `when`/`for`
    /// (RFC-0018) into their concrete children first. Returns the child atlas ids
    /// in paint order, appending each subtree's flat-id list to `flat_ids`.
    fn build_children(
        &mut self,
        nodes: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Vec<byard_core::atlas::layout::AtlasNodeId> {
        // The expansion refs borrow `nodes`/`pools`, never `self`, so `&mut self`
        // stays free for `build_layout_tree` below.
        let concrete = self.expand_concrete(nodes, pools);
        let mut ids = Vec::with_capacity(concrete.len());
        // The instance each child belongs to, exactly as the paint walk does it:
        // a `with` on a *layout* prop is sampled here, one pass before paint, and
        // must key to the same row there and here or the two passes drive two
        // different animations off one written clause.
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            if let Ok(id) = self.build_layout_tree(c.node, pools, flat_ids) {
                ids.push(id);
            }
        }
        // The caller's own style (`gap`, `p`, …) is evaluated *after* this
        // returns, so the walk owes it the slot it came in with rather than
        // whichever row happened to be last.
        self.anim_slot = enclosing;
        ids
    }

    /// The number of flattened layout nodes a concrete [`RenderNode`] subtree
    /// contributes, mirroring [`build_children`](Self::build_children)/
    /// [`build_layout_tree`](Self::build_layout_tree) exactly (one entry plus its
    /// expanded children). Used to advance the flat-id cursor past a culled
    /// `ScrollView` child without walking it (RFC-0005). `when`/`for` are
    /// expanded, so their live subtree is counted (RFC-0018).
    pub(super) fn flat_len(&self, node: &RenderNode, pools: Pools<'_>) -> usize {
        match node {
            RenderNode::Box { children, .. } => {
                1 + self
                    .expand_concrete(children, pools)
                    .iter()
                    .map(|c| self.flat_len(c.node, pools))
                    .sum::<usize>()
            }
            // RFC-0026: the container itself, plus, per live screen, that
            // screen's synthesized wrapper and its subtree.
            RenderNode::Nav { pool, .. } => {
                let Some(p) = pools.navs.get(*pool) else {
                    return 1;
                };
                1 + p
                    .live
                    .iter()
                    .map(|screen| {
                        1 + self
                            .expand_concrete(&p.entries[screen.entry].nodes, pools)
                            .iter()
                            .map(|c| self.flat_len(c.node, pools))
                            .sum::<usize>()
                    })
                    .sum::<usize>()
            }
            _ => 1,
        }
    }

    /// The layout leaf of a `Text`, measured against the environment the caller
    /// has restored.
    fn build_text_leaf(
        &mut self,
        attrs: &[Attr],
        state_blocks: &[crate::parser::ast::StateBlock],
        content: ScopeId,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        use byard_core::atlas::layout::LeafSize;
        // RFC-0016: a responsive `size:` changes what is measured, so
        // it has to be seen here as well as at paint.
        let raw_attrs = attrs;
        let attrs = resolve_state_attrs(
            attrs,
            state_blocks,
            crate::interp::events::StyleState::empty(),
            &std::collections::HashMap::new(),
            &|c| viewport_holds(c, self.viewport, &self.theme),
        );
        // RFC-0061: a leaf has no children, so its place in the build order is
        // known now and its interaction states can move its layout properties.
        let attrs = self.with_layout_states(
            "Text",
            &attrs,
            StateSource {
                raw: raw_attrs,
                blocks: state_blocks,
                bound_sig: None,
            },
        );
        let attrs = self.pick_keywords(std::borrow::Cow::Owned(attrs.into_owned()));
        let attrs: &[Attr] = &attrs;
        let text = match self.binding_value(content) {
            Some(Value::Str(s)) => s,
            other => other.map_or_else(String::new, |v| v.to_text()),
        };
        let typo_size = self.eval_typo_size(attrs);
        let font_size = self.text_font_size(attrs, typo_size);
        // RFC-0005 default text wrap: `wrap` defaults to `true`. A
        // wrapping `Text` becomes a measured leaf that the atlas sizes to
        // the width its parent offers during layout (via the shared
        // `TextMeasurer`), so it reflows without an explicit `width`. An
        // explicit `width` fixes the wrap width; `wrap: false` opts out to
        // a fixed natural single-line leaf (may overflow, the caller's
        // choice). `fallback` is the natural size for the no-sizer path.
        let weight = self.resolve_weight(attrs);
        let family = self.resolve_family(attrs);
        let (nat_w, nat_h) =
            self.measure_text_wrapped(&text, font_size, None, weight, family.as_deref());
        // The layout attributes an element in a `Row` or `Column`
        // takes. A `Text` with none of them never reaches this, so the
        // common leaf stays what it was: no style evaluated, none set.
        let boxed = self.text_box(attrs);
        let pad_w = boxed.padding.left + boxed.padding.right;
        let pad_h = boxed.padding.top + boxed.padding.bottom;
        #[allow(clippy::cast_precision_loss)]
        let explicit_w = self.eval_px_prop(attrs, "width");
        if self.eval_bool_prop(attrs, "wrap") == Some(false) {
            // A single line is never reshaped to a width, so `width`
            // is the leaf's width whatever the text measures (the
            // paint pass clips what does not fit), and without one the
            // leaf is the text's natural width.
            let size = LeafSize::new(
                explicit_w.unwrap_or(nat_w + pad_w),
                boxed.height.unwrap_or(nat_h + pad_h),
            );
            let id = if boxed == byard_core::atlas::TextBox::default() {
                self.atlas.add_leaf(size)?
            } else {
                self.atlas.add_fixed_text_leaf(size, boxed)?
            };
            self.apply_flex_item(attrs, id, false)?;
            flat_ids.push(id);
            return Ok(id);
        }
        let max_lines = self.text_max_lines(attrs);
        let id = self.atlas.add_text_leaf_boxed(
            byard_core::atlas::TextLeaf {
                content: text,
                font_size,
                weight,
                family,
                width: explicit_w,
                fallback: (nat_w + pad_w, nat_h + pad_h),
                max_lines,
            },
            boxed,
        )?;
        self.apply_flex_item(attrs, id, false)?;
        flat_ids.push(id);
        Ok(id)
    }

    pub(super) fn build_layout_tree(
        &mut self,
        node: &RenderNode,
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        use byard_core::atlas::layout::LeafSize;
        match node {
            // Reactive `when`/`for` are expanded to their concrete children by
            // `build_children` before reaching here (RFC-0018), so they never
            // arrive as a single layout node.
            RenderNode::When { .. } | RenderNode::For { .. } => {
                unreachable!("when/for are expanded by build_children before build_layout_tree")
            }
            // An effect has no layout box; `expand_concrete` drops it before
            // the build walk can reach one.
            RenderNode::Effect { .. } => {
                unreachable!("effects are dropped by expand_concrete before build_layout_tree")
            }
            // RFC-0026: a navigation container lays out one wrapper per live
            // screen. The screen the navigation names is in the container's
            // normal flow, so the container measures to it exactly as it would
            // to an ordinary child; a screen that is only transitioning in or
            // out is absolute over the same rect, so the two overlap without
            // either displacing the other or perturbing the measured size.
            RenderNode::Nav { pool, .. } => {
                use byard_core::atlas::layout::{ContainerStyle, FlexDir};
                let Some(p) = pools.navs.get(*pool) else {
                    let id = self.atlas.add_leaf(LeafSize::new(0.0, 0.0))?;
                    flat_ids.push(id);
                    return Ok(id);
                };
                let mut screen_ids = Vec::with_capacity(p.live.len());
                let mut temp_flat = Vec::new();
                for screen in &p.live {
                    let mut child_flat = Vec::new();
                    let child_ids =
                        self.build_children(&p.entries[screen.entry].nodes, pools, &mut child_flat);
                    let style = ContainerStyle::default()
                        .with_direction(FlexDir::Column)
                        .with_grow(1.0)
                        .with_absolute(!screen.in_flow);
                    let screen_id = self.atlas.add_container(style, &child_ids)?;
                    temp_flat.push(screen_id);
                    temp_flat.extend(child_flat);
                    screen_ids.push(screen_id);
                }
                let style = self.eval_container_style("NavStack", &p.attrs);
                let id = self.atlas.add_container(style, &screen_ids)?;
                flat_ids.push(id);
                flat_ids.extend(temp_flat);
                Ok(id)
            }
            RenderNode::Spacer { attrs, .. } => {
                // RFC-0005: a `Spacer` is a *flexible* gap, `grow` (default 1)
                // is its share of the parent's free space, `basis` its size
                // before growing. Both are read through the ordinary prop path,
                // so they are reactive (and animatable) like any other value.
                let grow = self.eval_float_prop(attrs, "grow").unwrap_or(1.0) as f32;
                let basis = self.eval_float_prop(attrs, "basis").unwrap_or(0.0) as f32;
                let id = self.atlas.add_flex_leaf(grow, basis)?;
                flat_ids.push(id);
                Ok(id)
            }
            // RFC-0017: an `Overlay` occupies zero space in its parent's flow,
            // its children are laid out separately against the viewport in the
            // deferred overlay phase. A 0×0 leaf keeps the parallel flat-id
            // cursor aligned without displacing any sibling.
            RenderNode::Overlay { .. } => {
                let id = self.atlas.add_leaf(LeafSize::new(0.0, 0.0))?;
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Image {
                attrs,
                env_snapshot,
                ..
            } => {
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let w = self.eval_px_prop(attrs, "width").unwrap_or(100.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(100.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                self.apply_flex_item(attrs, id, false)?;
                self.env.truncate(env_base);
                flat_ids.push(id);
                Ok(id)
            }
            // A `Canvas` is a fixed-size drawing surface (RFC-0020 §1): both
            // dimensions are required (enforced by `validate_canvas`); the 0
            // fallback keeps a mis-declared canvas laid out (collapsed) rather
            // than aborting the whole tree.
            RenderNode::Canvas {
                attrs,
                env_snapshot,
                ..
            } => {
                // Sized from the scope it was instantiated in, like any other
                // element (see the `Box` arm below).
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let w = self.eval_px_prop(attrs, "width").unwrap_or(0.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(0.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                self.apply_flex_item(attrs, id, false)?;
                self.env.truncate(env_base);
                flat_ids.push(id);
                Ok(id)
            }
            // RFC-0039: a native view is laid out by asking it. The `width`
            // and `height` props, if written, are what layout already knows,
            // and `measure` gets the chance to answer with something else (an
            // intrinsic size) or to keep them.
            //
            // A view that leaves both axes free is a flex leaf rather than a
            // zero-sized one: "fill" is the answer a chart or a map gives, and
            // a leaf of size zero would be an invisible widget with no error
            // anywhere, which is exactly the silent failure the engine forbids.
            RenderNode::Native {
                attrs,
                env_snapshot,
                slot,
                ..
            } => {
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let known = byard_core::render::Measure {
                    width: self.eval_px_prop(attrs, "width"),
                    height: self.eval_px_prop(attrs, "height"),
                };
                let answer = self
                    .native_views
                    .get(*slot)
                    .map_or(known, |v| v.measure(known));
                let id = match (answer.width, answer.height) {
                    (Some(w), Some(h)) => self.atlas.add_leaf(LeafSize::new(w, h))?,
                    (w, h) => {
                        // One free axis still fills the other: a fixed extent
                        // on the axis that has one, growth on the axis that
                        // does not.
                        let leaf = self.atlas.add_flex_leaf(1.0, 0.0)?;
                        let _ = (w, h);
                        leaf
                    }
                };
                self.apply_flex_item(attrs, id, false)?;
                self.env.truncate(env_base);
                flat_ids.push(id);
                Ok(id)
            }
            // A VectorIcon is a square leaf sized by its `size` prop (default 24),
            // RFC-0009 §1.
            RenderNode::Vector {
                attrs,
                env_snapshot,
                ..
            } => {
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let s = self.eval_px_prop(attrs, "size").unwrap_or(24.0);
                let id = self.atlas.add_leaf(LeafSize::new(s, s))?;
                self.apply_flex_item(attrs, id, false)?;
                self.env.truncate(env_base);
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Text {
                attrs,
                content,
                state_blocks,
                env_snapshot,
                ..
            } => {
                // The scope the run was written in, so a `size:` that reads a
                // view parameter measures with the instance's value.
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let out = self.build_text_leaf(attrs, state_blocks, *content, flat_ids);
                self.env.truncate(env_base);
                out
            }
            RenderNode::Box {
                name,
                attrs,
                state_blocks,
                children,
                env_snapshot,
                bound_sig,
                measure,
                uid,
                ..
            } => {
                // RFC-0016 responsive variants reach layout here, one pass ahead
                // of paint and with no interaction state, so only viewport
                // blocks can apply. An interaction block's layout properties
                // still do not relayout (hover must not move its own box), and
                // a breakpoint's do, since a narrow window is exactly when a
                // padding or a direction should change. Borrowed unchanged when
                // the box has no blocks, which is nearly every box.
                let raw_attrs: &[Attr] = attrs;
                let attrs = resolve_state_attrs(
                    attrs,
                    state_blocks,
                    crate::interp::events::StyleState::empty(),
                    &std::collections::HashMap::new(),
                    &|c| viewport_holds(c, self.viewport, &self.theme),
                );
                // The same restore the paint walk does, for the same reason and
                // one pass earlier: a box's *size* is as much a function of the
                // scope it was instantiated in as its colour is. `width: row.w`
                // inside a `for`, a user view's `width: size`, layout reads
                // those attrs through the ordinary prop path, so without the
                // instance environment they resolve to nothing and the element
                // silently falls back to its default (a `width`-less box fills
                // its parent). Empty at the top level (no-op).
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                // A `?:` between keywords is read now that the scope is back.
                let attrs = self.pick_keywords(attrs);
                let attrs: &[Attr] = &attrs;
                // Taken before the children are built, so only the direct child
                // of a `ZStack` sees that it is one.
                let in_stack = std::mem::take(&mut self.stacked) == Stacked::Yes;
                let out = self.build_box_layout(
                    name,
                    attrs,
                    StateSource {
                        raw: raw_attrs,
                        blocks: state_blocks,
                        bound_sig: *bound_sig,
                    },
                    children,
                    pools,
                    flat_ids,
                );
                if let Ok(id) = &out {
                    self.apply_flex_item(attrs, *id, in_stack)?;
                }
                // RFC-0062: an element whose states another names has its value
                // states (`checked`, `selected`, `invalid`, `disabled`) read here,
                // where its attributes are in scope, so that an element written
                // before it can follow them in the same frame.
                if self.remote_targets.contains(uid) {
                    let mut value = self.prop_style_state(attrs, *bound_sig, name.as_str());
                    if self.eval_bool_prop(attrs, "disabled") == Some(true) {
                        value = value.union(crate::interp::events::StyleState::DISABLED);
                    }
                    self.elem_value_states.insert(*uid, value);
                }
                self.env.truncate(env_base);
                // RFC-0038: pair the element with the node that will hold its
                // resolved rect. Recorded here because this is the one point
                // where both are in hand, and skipped entirely by an element
                // that declared no `on measure`.
                if let (Some(slot), Ok(id)) = (measure, &out) {
                    self.measure_targets.push((*id, *slot));
                }
                // The index this element got is its place in the build order;
                // the uid is what it is. Focus follows the second.
                if let Ok(id) = &out {
                    if let Some(idx) = self.atlas.node_index(*id) {
                        self.elem_uids.push((idx, *uid));
                    }
                }
                out
            }
        }
    }

    /// Lays the `shrink` and `align_self` an element wrote over its layout node.
    /// An element that writes neither costs nothing, unless its node had them
    /// last build (a breakpoint dropped them), in which case they are undone.
    ///
    /// In a `ZStack` (`in_stack`) the child is placed in one grid cell, where
    /// `align_self` reaches only the vertical axis. `align_self: stretch`, and a
    /// `grow` above zero, both mean "fill the stack" there, so they stretch the
    /// horizontal axis too (RFC-0059); a child with a size of its own keeps it.
    fn apply_flex_item(
        &mut self,
        attrs: &[Attr],
        id: byard_core::atlas::layout::AtlasNodeId,
        in_stack: bool,
    ) -> Result<(), byard_core::atlas::AtlasError> {
        use byard_core::atlas::{Align, FlexItem};
        let writes = attrs.iter().any(|a| {
            matches!(a.kind, AttrKind::Prop { .. })
                && (matches!(
                    a.name.as_str(),
                    "shrink"
                        | "align_self"
                        | "min_width"
                        | "max_width"
                        | "min_height"
                        | "max_height"
                ) || (in_stack && a.name.as_str() == "grow"))
        });
        if !writes && !self.atlas.has_flex_items() {
            return Ok(());
        }
        let mut item = FlexItem::default();
        if writes {
            #[allow(clippy::cast_possible_truncation)]
            {
                item.shrink = self.eval_float_prop(attrs, "shrink").map(|v| v as f32);
            }
            // RFC-0064: static bounds on the size layout settles on.
            item.min_width = self.eval_px_prop(attrs, "min_width");
            item.max_width = self.eval_px_prop(attrs, "max_width");
            item.min_height = self.eval_px_prop(attrs, "min_height");
            item.max_height = self.eval_px_prop(attrs, "max_height");
            item.align_self = attrs.iter().find_map(|a| match &a.kind {
                AttrKind::Prop { value } if a.name.as_str() == "align_self" => {
                    Self::enum_token(value).map(|t| match t {
                        "start" => Align::Start,
                        "center" => Align::Center,
                        "end" => Align::End,
                        _ => Align::Stretch,
                    })
                }
                _ => None,
            });
            if in_stack {
                let grows = self.eval_float_prop(attrs, "grow").is_some_and(|g| g > 0.0);
                if grows || item.align_self == Some(Align::Stretch) {
                    item.align_self = Some(Align::Stretch);
                    item.justify_self = Some(Align::Stretch);
                }
            }
        }
        self.atlas.set_flex_item(id, item)
    }

    /// The `max_lines` of a `Text`, `None` when it writes none (RFC-0064).
    pub(super) fn text_max_lines(&mut self, attrs: &[Attr]) -> Option<u32> {
        self.eval_int_prop(attrs, "max_lines")
            .map(|n| u32::try_from(n.max(1)).unwrap_or(u32::MAX))
    }

    /// The layout box of a `Text`: its `height`, `grow`, padding and
    /// margin. All-default, without evaluating anything, when the `Text`
    /// writes none of them.
    fn text_box(&mut self, attrs: &[Attr]) -> byard_core::atlas::TextBox {
        let has_box = attrs
            .iter()
            .any(|a| matches!(a.kind, AttrKind::Prop { .. }) && is_text_box_attr(a.name.as_str()));
        if !has_box {
            return byard_core::atlas::TextBox::default();
        }
        let style = self.eval_container_style("Text", attrs);
        byard_core::atlas::TextBox {
            height: style.height,
            grow: style.grow,
            basis: style.basis,
            padding: style.padding,
            margin: style.margin,
            min_height: None,
            max_height: None,
        }
    }

    /// The `Box`-family layout body, split out so its one caller can bracket it
    /// with the box's restored instance environment across every early return.
    fn build_box_layout(
        &mut self,
        name: &Symbol,
        attrs: &[Attr],
        states: StateSource<'_>,
        children: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        use byard_core::atlas::layout::LeafSize;
        // Value widgets are leaf nodes with intrinsic default sizes.
        match name.as_str() {
            "Toggle" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(50.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(30.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            "Slider" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(200.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(24.0);
                let grow = self.eval_float_prop(attrs, "grow").unwrap_or(0.0) as f32;
                let id = self.atlas.add_growing_leaf(LeafSize::new(w, h), grow)?;
                flat_ids.push(id);
                return Ok(id);
            }
            // RFC-0018 Checkbox: an 18×18 square by default; `width`/
            // `height` override. Sized square unless overridden so the
            // container and checkmark geometry stay proportional.
            "Checkbox" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(18.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(18.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            // RFC-0018 RadioButton: a 20×20 circle by default.
            "RadioButton" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(20.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(20.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            "TextField" => {
                let width = self.eval_px_prop(attrs, "width");
                let explicit_h = self.eval_px_prop(attrs, "height");
                let grow = self.eval_float_prop(attrs, "grow").unwrap_or(0.0) as f32;
                // RFC-0064: a field of several lines is as tall as its lines,
                // or, with a range, as its content, between the two.
                let lines = self.field_lines(attrs);
                let (pad_x, pad_top, pad_bottom) = self.field_pads(attrs);
                let line_h = self.field_line_height(attrs);
                let h = match (explicit_h, lines) {
                    (None, FieldLines::Fixed(n)) => n as f32 * line_h + pad_top + pad_bottom,
                    (Some(h), _) => h,
                    _ => 36.0,
                };
                if let (None, FieldLines::Range { min, max }) = (explicit_h, lines) {
                    let text = match states.bound_sig.map(|s| self.ctx.peek_signal(s)) {
                        Some(Value::Str(t)) => t,
                        _ => String::new(),
                    };
                    let font_size = self.field_font_size(attrs);
                    let weight = self.resolve_weight(attrs);
                    let family = self.resolve_family(attrs);
                    let pad = byard_core::atlas::Spacing {
                        top: pad_top,
                        right: pad_x,
                        bottom: pad_bottom,
                        left: pad_x,
                    };
                    let fill = width.is_none() && grow > 0.0;
                    let w = width.unwrap_or(200.0);
                    let boxed = byard_core::atlas::TextBox {
                        height: None,
                        grow,
                        basis: fill.then_some(0.0),
                        padding: pad,
                        margin: byard_core::atlas::Spacing::default(),
                        min_height: Some(min as f32 * line_h + pad_top + pad_bottom),
                        max_height: max.map(|m| m as f32 * line_h + pad_top + pad_bottom),
                    };
                    let id = self.atlas.add_text_leaf_boxed(
                        byard_core::atlas::TextLeaf {
                            content: text,
                            font_size,
                            weight,
                            family,
                            width: (!fill).then_some(w),
                            fallback: (w, line_h + pad_top + pad_bottom),
                            max_lines: None,
                        },
                        boxed,
                    )?;
                    flat_ids.push(id);
                    return Ok(id);
                }
                // A field that is told to grow and not how wide to start has no
                // width of its own: it takes the room `grow` hands it, so it
                // cannot squeeze what is beside it. The 200 is the fallback for
                // a field that says neither.
                let id = match width {
                    None if grow > 0.0 => self.atlas.add_fill_leaf(h, grow)?,
                    _ => self
                        .atlas
                        .add_growing_leaf(LeafSize::new(width.unwrap_or(200.0), h), grow)?,
                };
                flat_ids.push(id);
                return Ok(id);
            }
            _ => {}
        }
        // RFC-0005 windowed ScrollView: when opted in, build only the
        // visible slice of a single uniform-height list child, bracketed
        // by spacer leaves for the elided rows, so layout is O(visible),
        // not O(list). The same window is recomputed in the render pass.
        if name.as_str() == "ScrollView" {
            if let [
                RenderNode::Box {
                    name: list_name,
                    attrs: list_attrs,
                    children: rows_raw,
                    ..
                },
            ] = children
            {
                // RFC-0018: expand a reactive `for` (or literal rows) to
                // the concrete row nodes, then window over them, so
                // virtualization still lays out only the visible slice.
                let rows = self.expand_concrete(rows_raw, pools);
                if let Some(win) = self.scroll_window(attrs, rows.len()) {
                    let mut temp_flat = Vec::new();
                    let list_id = self.build_windowed_list(
                        list_name,
                        list_attrs,
                        &rows,
                        win,
                        pools,
                        &mut temp_flat,
                    )?;
                    let style = self.eval_container_style(name.as_str(), attrs);
                    let id = self.atlas.add_container(style, &[list_id])?;
                    flat_ids.push(id);
                    flat_ids.extend(temp_flat);
                    return Ok(id);
                }
            }
        }
        // RFC-0018 `Grid`: a CSS-grid container. Built via its own path so
        // the parent gets grid tracks/gaps and each child can carry an
        // explicit placement.
        if name.as_str() == "Grid" {
            return self.build_grid(attrs, states, children, pools, flat_ids);
        }
        // RFC-0018 `ZStack`: overlapping children, a single-cell grid.
        if name.as_str() == "ZStack" {
            return self.build_zstack(attrs, states, children, pools, flat_ids);
        }
        let mut temp_flat = Vec::new();
        // RFC-0018: expand reactive `when`/`for` children before layout.
        let child_ids = self.build_children(children, pools, &mut temp_flat);
        // RFC-0061: with the children built, this container's place in the build
        // order is known, which is what names it to the router, so its `on
        // <state>` blocks can move its layout properties.
        let attrs = self.with_layout_states(name.as_str(), attrs, states);
        let style = self.eval_container_style(name.as_str(), &attrs);
        let id = self.atlas.add_container(style, &child_ids)?;
        flat_ids.push(id);
        flat_ids.extend(temp_flat);
        Ok(id)
    }

    /// `attrs` with the interaction states the container is in applied, when a
    /// state block writes a layout property (RFC-0061); `attrs` as they are, with
    /// no work, when none does (nearly every box).
    ///
    /// The mask is the one paint reads: the router's hover, pressed, focus and
    /// drag bits for the node this container is about to become, and the
    /// prop-driven ones. `disabled:` is read from the element, not from the
    /// router, which only learns of it at paint.
    fn with_layout_states<'a>(
        &mut self,
        name: &str,
        attrs: &'a [Attr],
        states: StateSource<'_>,
    ) -> std::borrow::Cow<'a, [Attr]> {
        use crate::interp::events::StyleState;
        let moves_layout = states.blocks.iter().any(|b| {
            !b.states.is_empty()
                && b.attrs
                    .iter()
                    .any(|a| crate::interp::intrinsics::layout_motion(a.name.as_str()))
        });
        if !moves_layout {
            return std::borrow::Cow::Borrowed(attrs);
        }
        let state = if self.eval_bool_prop(states.raw, "disabled") == Some(true) {
            StyleState::DISABLED
        } else {
            self.router
                .style_state(self.atlas.next_build_slot())
                .union(self.prop_style_state(states.raw, states.bound_sig, name))
        };
        let resolved =
            resolve_state_attrs(states.raw, states.blocks, state, &self.elem_states, &|c| {
                viewport_holds(c, self.viewport, &self.theme)
            })
            .into_owned();
        let resolved = self.pick_keywords(std::borrow::Cow::Owned(resolved));
        std::borrow::Cow::Owned(resolved.into_owned())
    }

    /// Builds the atlas subtree for a `Grid` (RFC-0018). Children are expanded
    /// (reactive `when`/`for` first), each built and, if it carries `col`/`row`/
    /// `col_span`/`row_span`, placed explicitly; the rest auto-place. The
    /// container is emitted in the same parent-then-children `flat_ids` order the
    /// generic container path uses, so the render walk's parallel cursor stays
    /// aligned.
    fn build_grid(
        &mut self,
        attrs: &[Attr],
        states: StateSource<'_>,
        children: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        let concrete = self.expand_concrete(children, pools);
        let mut child_ids = Vec::with_capacity(concrete.len());
        let mut placements: Vec<(
            byard_core::atlas::layout::AtlasNodeId,
            byard_core::atlas::GridItemPlacement,
        )> = Vec::new();
        let mut temp_flat = Vec::new();
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            if let Ok(cid) = self.build_layout_tree(c.node, pools, &mut temp_flat) {
                if let Some(p) = self.grid_child_placement(c.node) {
                    placements.push((cid, p));
                }
                child_ids.push(cid);
            }
        }
        self.anim_slot = enclosing;
        let attrs = self.with_layout_states("Grid", attrs, states);
        let attrs: &[Attr] = &attrs;
        let base = self.eval_container_style("Grid", attrs);
        let (cols, rows) = self.eval_grid_templates(attrs);
        let (col_gap, row_gap) = self.eval_grid_gaps(attrs);
        let id = self
            .atlas
            .add_grid_container(base, &cols, &rows, col_gap, row_gap, &child_ids)?;
        for (cid, p) in placements {
            // A rejected placement (e.g. a foreign node) is non-fatal, the child
            // simply auto-places, so it never aborts the frame.
            let _ = self.atlas.set_grid_item(cid, p);
        }
        flat_ids.push(id);
        flat_ids.extend(temp_flat);
        Ok(id)
    }

    /// Reads a grid child's explicit placement (`col`/`row`/`col_span`/
    /// `row_span`) from its attrs, or `None` if it carries none (→ auto-placed).
    /// Only `Box`-family children (Box/Column/Row/Grid/…) carry placement.
    fn grid_child_placement(
        &mut self,
        child: &RenderNode,
    ) -> Option<byard_core::atlas::GridItemPlacement> {
        let RenderNode::Box { attrs, .. } = child else {
            return None;
        };
        let col = self.eval_int_prop(attrs, "col");
        let row = self.eval_int_prop(attrs, "row");
        let col_span = self.eval_int_prop(attrs, "col_span");
        let row_span = self.eval_int_prop(attrs, "row_span");
        if col.is_none() && row.is_none() && col_span.is_none() && row_span.is_none() {
            return None;
        }
        Some(byard_core::atlas::GridItemPlacement {
            col_start: col.and_then(|n| i16::try_from(n).ok()),
            col_span: col_span
                .and_then(|n| u16::try_from(n.max(1)).ok())
                .unwrap_or(1),
            row_start: row.and_then(|n| i16::try_from(n).ok()),
            row_span: row_span
                .and_then(|n| u16::try_from(n.max(1)).ok())
                .unwrap_or(1),
        })
    }

    /// Parses a `Grid`'s `columns:`/`rows:` templates (RFC-0018). A missing or
    /// malformed `columns` defaults to a single `auto` track (one column);
    /// missing `rows` leaves rows implicit (Taffy auto-creates them). A malformed
    /// template also pushes a [`CompileError::InvalidGridTemplate`].
    ///
    /// [`CompileError::InvalidGridTemplate`]: crate::diagnostics::CompileError::InvalidGridTemplate
    fn eval_grid_templates(
        &mut self,
        attrs: &[Attr],
    ) -> (
        Vec<byard_core::atlas::GridTrack>,
        Vec<byard_core::atlas::GridTrack>,
    ) {
        let cols = self
            .eval_grid_axis(attrs, "columns")
            .unwrap_or_else(|| vec![byard_core::atlas::GridTrack::Auto]);
        let rows = self.eval_grid_axis(attrs, "rows").unwrap_or_default();
        (cols, rows)
    }

    /// Resolves one grid-template axis attribute to tracks, pushing an
    /// `InvalidGridTemplate` diagnostic on a malformed string. `None` = the
    /// attribute is absent (or not a string).
    fn eval_grid_axis(
        &mut self,
        attrs: &[Attr],
        name: &str,
    ) -> Option<Vec<byard_core::atlas::GridTrack>> {
        let attr = attrs.iter().find(|a| a.name.as_str() == name)?;
        let AttrKind::Prop { value } = &attr.kind else {
            return None;
        };
        let Value::Str(s) = self.eval_pure(value) else {
            return None;
        };
        let parsed = super::intrinsics::parse_grid_template(&s);
        if parsed.is_none() {
            self.errors.push(CompileError::InvalidGridTemplate {
                span: attr.span,
                template: s,
            });
        }
        parsed
    }

    /// Resolves a `Grid`'s per-axis gaps: `col_gap`/`row_gap` each fall back to
    /// the shared `gap` (default 0). Returns `(col_gap, row_gap)`.
    fn eval_grid_gaps(&mut self, attrs: &[Attr]) -> (f32, f32) {
        let gap = self.eval_px_prop(attrs, "gap").unwrap_or(0.0);
        let col_gap = self.eval_px_prop(attrs, "col_gap").unwrap_or(gap);
        let row_gap = self.eval_px_prop(attrs, "row_gap").unwrap_or(gap);
        (col_gap, row_gap)
    }

    /// Builds the atlas subtree for a `ZStack` (RFC-0018): a single-cell grid in
    /// which every child overlaps. Emitted in the same parent-then-children
    /// `flat_ids` order as the generic container path so the render walk's cursor
    /// stays aligned, and rendered through the ordinary Box paint path (bg +
    /// children in declaration order, last on top).
    fn build_zstack(
        &mut self,
        attrs: &[Attr],
        states: StateSource<'_>,
        children: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        let concrete = self.expand_concrete(children, pools);
        let mut child_ids = Vec::with_capacity(concrete.len());
        let mut temp_flat = Vec::new();
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            self.stacked = Stacked::Yes;
            if let Ok(cid) = self.build_layout_tree(c.node, pools, &mut temp_flat) {
                child_ids.push(cid);
            }
            self.stacked = Stacked::No;
        }
        self.anim_slot = enclosing;
        let attrs = self.with_layout_states("ZStack", attrs, states);
        let attrs: &[Attr] = &attrs;
        let base = self.eval_container_style("ZStack", attrs);
        let align = self.eval_stack_align(attrs);
        let id = self.atlas.add_stack_container(base, align, &child_ids)?;
        flat_ids.push(id);
        flat_ids.extend(temp_flat);
        Ok(id)
    }

    /// Resolves a `ZStack`'s `alignment` prop to a [`StackAlign`], default
    /// `Center`.
    ///
    /// [`StackAlign`]: byard_core::atlas::StackAlign
    fn eval_stack_align(&mut self, attrs: &[Attr]) -> byard_core::atlas::StackAlign {
        use byard_core::atlas::StackAlign;
        for attr in attrs {
            if attr.name.as_str() != "alignment" {
                continue;
            }
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            if let Some(s) = Self::enum_token(value) {
                return match s {
                    "top_start" => StackAlign::TopStart,
                    "top_end" => StackAlign::TopEnd,
                    "bottom_start" => StackAlign::BottomStart,
                    "bottom_end" => StackAlign::BottomEnd,
                    "top" => StackAlign::Top,
                    "bottom" => StackAlign::Bottom,
                    "start" => StackAlign::Start,
                    "end" => StackAlign::End,
                    _ => StackAlign::Center,
                };
            }
        }
        StackAlign::Center
    }

    /// Builds the atlas subtree for a windowed `ScrollView`'s list child
    /// (RFC-0005): a leading spacer sized to the rows scrolled off the top, the
    /// materialised rows `win.start..win.end`, then a trailing spacer for the
    /// rows below the window. The two spacers preserve the container's content
    /// extent (so the scroll clamp is exact) and every visible row's position,
    /// while only `end − start` rows are ever laid out. `flat_ids` receives the
    /// same `[container, top-spacer, rows…, bottom-spacer]` order the render pass
    /// walks, keeping the parallel cursor aligned.
    fn build_windowed_list(
        &mut self,
        list_name: &Symbol,
        list_attrs: &[Attr],
        rows: &[Concrete<'_>],
        win: WindowSpec,
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        use byard_core::atlas::layout::LeafSize;
        #[allow(clippy::cast_precision_loss)]
        let top_h = win.start as f32 * win.row_height;
        #[allow(clippy::cast_precision_loss)]
        let bottom_h = (win.n - win.end) as f32 * win.row_height;

        let mut child_ids = Vec::with_capacity(win.end - win.start + 2);
        let mut temp = Vec::new();
        let top = self.atlas.add_leaf(LeafSize::new(0.0, top_h))?;
        temp.push(top);
        child_ids.push(top);
        let enclosing = self.anim_slot;
        for row in &rows[win.start..win.end] {
            self.anim_slot = row.slot;
            let id = self.build_layout_tree(row.node, pools, &mut temp)?;
            child_ids.push(id);
        }
        self.anim_slot = enclosing;
        let bottom = self.atlas.add_leaf(LeafSize::new(0.0, bottom_h))?;
        temp.push(bottom);
        child_ids.push(bottom);

        let mut style = self.eval_container_style(list_name.as_str(), list_attrs);
        // Rows are positioned purely by `row_height` (spacing folded in); a flex
        // gap would add phantom space around the spacers and desync the window.
        style.gap = 0.0;
        let id = self.atlas.add_container(style, &child_ids)?;
        flat_ids.push(id);
        flat_ids.extend(temp);
        Ok(id)
    }
}

/// Whether a `Text` attribute is one of the box ones (`height`, `grow`,
/// padding, margin), as opposed to its text and paint attributes.
fn is_text_box_attr(name: &str) -> bool {
    matches!(
        name,
        "height"
            | "grow"
            | "basis"
            | "p"
            | "padding"
            | "pt"
            | "pr"
            | "pb"
            | "pl"
            | "m"
            | "margin"
            | "mx"
            | "my"
            | "mt"
            | "mr"
            | "mb"
            | "ml"
    )
}
