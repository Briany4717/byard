//! Building the layout tree (RFC-0005, RFC-0018, RFC-0032).
//!
//! Each frame the render tree is flattened into the concrete nodes to lay
//! out and paint (a `when` expands to its taken branch, a `for` to its live
//! pooled bodies), the `for` pools are reconciled against their lists, and
//! the result is built into the layout atlas: boxes and their flex style,
//! grids, stacks and windowed lists. The retained path builds the same tree
//! through the same walk, which is what makes it safe to reason about; the
//! state it keeps between frames lives here with the pools it describes.

use super::instance::MAX_INSTANCE_DEPTH;
use super::navigation::NavPool;
use super::overlay::OverlayLayout;
use super::scroll::WindowSpec;
use super::states::{resolve_state_attrs, viewport_holds};
use super::{Interpreter, RenderNode};
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::Value;
use crate::parser::ast::{Attr, AttrKind, Member};
use crate::symbol::Symbol;

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
    pub(super) slot: u32,
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
    /// The lowered `then` branch, once first taken.
    pub(super) then: Option<Vec<RenderNode>>,
    /// The lowered `else` branch, once first taken.
    pub(super) els: Option<Vec<RenderNode>>,
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
    /// One signal per pooled index, holding that slot's current element value.
    pub(super) item_slots: Vec<super::env::SignalId>,
    /// One lowered body per pooled index (parallel to `item_slots`).
    pub(super) bodies: Vec<Vec<RenderNode>>,
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
        slot: u32,
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
                            let row = p.item_slots.get(i).map_or(slot, |s| s.0);
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
                other => out.push(Concrete { node: other, slot }),
            }
        }
        out
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
                        self.drop_animation_state(gone);
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
                        let ast = if take {
                            self.when_pools[*pool].then_ast.clone()
                        } else {
                            self.when_pools[*pool].els_ast.clone()
                        };
                        let known: Vec<String> = self.when_pools[*pool].known_views.clone();
                        let env_snap = self.when_pools[*pool].env_snapshot.clone();
                        let env_base = self.env.len();
                        for (k, v) in &env_snap {
                            self.env.push(k.clone(), v.clone());
                        }
                        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
                        // A branch lowered here is lowered *away from where it
                        // was written*: the scope it belongs to exists only as
                        // the snapshot just pushed, and it is truncated again
                        // below. Anything inside the branch that resolves later
                        //, an event action, an animated attribute reading the
                        // row it belongs to, a nested `for`/`when` pool, has to
                        // capture that scope now, or it will look for the row in
                        // an environment that no longer has one.
                        let restored = !env_snap.is_empty();
                        self.restored_depth += u32::from(restored);
                        let nodes = self.lower_members(&ast, &known_refs);
                        self.restored_depth -= u32::from(restored);
                        self.env.truncate(env_base);
                        if take {
                            self.when_pools[*pool].then = Some(nodes);
                        } else {
                            self.when_pools[*pool].els = Some(nodes);
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
                        let slot = self.ctx.create_signal(Value::Unit);
                        // Clone the lowering inputs out first so the borrow on
                        // `self.for_pools` is released before `lower_members`
                        // (which may append *nested* pools to `self.for_pools`).
                        let item_var = self.for_pools[*pool].item_var.clone();
                        let index_var = self.for_pools[*pool].index_var.clone();
                        #[allow(clippy::cast_possible_wrap)]
                        let slot_index = self.for_pools[*pool].bodies.len() as i64;
                        let body_ast = self.for_pools[*pool].body.clone();
                        let known: Vec<String> = self.for_pools[*pool].known_views.clone();
                        let env_snap = self.for_pools[*pool].env_snapshot.clone();
                        let env_base = self.env.len();
                        for (k, v) in &env_snap {
                            self.env.push(k.clone(), v.clone());
                        }
                        self.env.push(item_var, Value::Signal(slot));
                        if let Some(index_var) = index_var {
                            self.env.push(index_var, Value::Int(slot_index));
                        }
                        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
                        // Mark that we are lowering inside a `for` body so any
                        // event action captures `t` in its env snapshot (the
                        // binding is truncated below before render, RFC-0027).
                        self.for_depth += 1;
                        let body_nodes = self.lower_members(&body_ast, &known_refs);
                        self.for_depth -= 1;
                        self.env.truncate(env_base);
                        // Index `*pool` is still valid (nested lowering only
                        // appended higher indices).
                        self.for_pools[*pool].item_slots.push(slot);
                        self.for_pools[*pool].bodies.push(body_nodes);
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
                    // timeline. The pool's *bodies* are grow-only (index `i`
                    // reuses its lowered nodes and its element signal), which is
                    // exactly why the state has to be dropped explicitly: it
                    // would otherwise be waiting there for the next occupant.
                    let previous_len = self.for_pools[*pool].len;
                    if new_len < previous_len {
                        self.drop_slot_state(*pool, new_len, previous_len);
                        dirtied = true;
                    }
                    self.for_pools[*pool].len = new_len;
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
            RenderNode::Spacer { attrs } => {
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
            RenderNode::Image { attrs, .. } => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(100.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(100.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
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
                self.env.truncate(env_base);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
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
                self.env.truncate(env_base);
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
                flat_ids.push(id);
                Ok(id)
            }
            // A VectorIcon is a square leaf sized by its `size` prop (default 24),
            // RFC-0009 §1.
            RenderNode::Vector { attrs, .. } => {
                let s = self.eval_int_prop(attrs, "size").unwrap_or(24) as f32;
                let id = self.atlas.add_leaf(LeafSize::new(s, s))?;
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Text {
                attrs,
                content,
                state_blocks,
            } => {
                // RFC-0016: a responsive `size:` changes what is measured, so
                // it has to be seen here as well as at paint.
                let attrs = resolve_state_attrs(
                    attrs,
                    state_blocks,
                    crate::interp::events::StyleState::empty(),
                    &|c| viewport_holds(c, self.viewport, &self.theme),
                );
                let attrs: &[Attr] = &attrs;
                let text = match self.binding_value(*content) {
                    Some(Value::Str(s)) => s,
                    other => other.map_or_else(String::new, |v| format!("{v:?}")),
                };
                let typo_size = self.eval_typo_size(attrs);
                #[allow(clippy::cast_precision_loss)]
                let font_size = self
                    .eval_int_prop(attrs, "size")
                    .or(typo_size)
                    .unwrap_or(self.theme.font_size as i64) as f32;
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
                if self.eval_bool_prop(attrs, "wrap") == Some(false) {
                    let id = self.atlas.add_leaf(LeafSize::new(nat_w, nat_h))?;
                    flat_ids.push(id);
                    return Ok(id);
                }
                #[allow(clippy::cast_precision_loss)]
                let explicit_w = self.eval_px_prop(attrs, "width");
                let id = self.atlas.add_text_leaf(byard_core::atlas::TextLeaf {
                    content: text,
                    font_size,
                    weight,
                    family,
                    width: explicit_w,
                    fallback: (nat_w, nat_h),
                })?;
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Box {
                name,
                attrs,
                state_blocks,
                children,
                env_snapshot,
                measure,
                ..
            } => {
                // RFC-0016 responsive variants reach layout here, one pass ahead
                // of paint and with no interaction state, so only viewport
                // blocks can apply. An interaction block's layout properties
                // still do not relayout (hover must not move its own box), and
                // a breakpoint's do, since a narrow window is exactly when a
                // padding or a direction should change. Borrowed unchanged when
                // the box has no blocks, which is nearly every box.
                let attrs = resolve_state_attrs(
                    attrs,
                    state_blocks,
                    crate::interp::events::StyleState::empty(),
                    &|c| viewport_holds(c, self.viewport, &self.theme),
                );
                let attrs: &[Attr] = &attrs;
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
                let out = self.build_box_layout(name, attrs, children, pools, flat_ids);
                self.env.truncate(env_base);
                // RFC-0038: pair the element with the node that will hold its
                // resolved rect. Recorded here because this is the one point
                // where both are in hand, and skipped entirely by an element
                // that declared no `on measure`.
                if let (Some(slot), Ok(id)) = (measure, &out) {
                    self.measure_targets.push((*id, *slot));
                }
                out
            }
        }
    }

    /// The `Box`-family layout body, split out so its one caller can bracket it
    /// with the box's restored instance environment across every early return.
    fn build_box_layout(
        &mut self,
        name: &Symbol,
        attrs: &[Attr],
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
                let w = self.eval_px_prop(attrs, "width").unwrap_or(200.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(36.0);
                let grow = self.eval_float_prop(attrs, "grow").unwrap_or(0.0) as f32;
                let id = self.atlas.add_growing_leaf(LeafSize::new(w, h), grow)?;
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
            return self.build_grid(attrs, children, pools, flat_ids);
        }
        // RFC-0018 `ZStack`: overlapping children, a single-cell grid.
        if name.as_str() == "ZStack" {
            return self.build_zstack(attrs, children, pools, flat_ids);
        }
        let mut temp_flat = Vec::new();
        // RFC-0018: expand reactive `when`/`for` children before layout.
        let child_ids = self.build_children(children, pools, &mut temp_flat);
        let style = self.eval_container_style(name.as_str(), attrs);
        let id = self.atlas.add_container(style, &child_ids)?;
        flat_ids.push(id);
        flat_ids.extend(temp_flat);
        Ok(id)
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
            if let Ok(cid) = self.build_layout_tree(c.node, pools, &mut temp_flat) {
                child_ids.push(cid);
            }
        }
        self.anim_slot = enclosing;
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
