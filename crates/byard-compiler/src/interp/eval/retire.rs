//! Retiring what a discarded subtree owned (RFC-0018, RFC-0026).
//!
//! A route, a `when` branch and a `for` row are lowered lazily, and each lowers
//! into reactive scopes, pools for the `when`/`for`/`NavStack` inside it, and
//! lifecycle effects. The subtree is the unit that goes away: a route when it is
//! popped, a row when the list shrinks a long way. This is the one place that
//! knows what going away means, so that it is the same for all of them.
//!
//! Nothing is dropped on the spot. A popped screen's `on unmount` still has to
//! run, and it reads the screen's `let`s, so the scopes are queued here and
//! released by [`release_retired`](Interpreter::release_retired) once the frame's
//! effects have settled. Between the two, a scope is still alive and still
//! answers; after, its id is refused (see [`ScopeId`]) and its slot is free for
//! the next screen.

use super::layout::{ForPool, WhenPool};
use super::navigation::NavPool;
use super::{Interpreter, RenderNode};
use crate::interp::reactive::ScopeId;

/// Fewest `for` rows a pool keeps lowered however short its list gets. A list
/// of three that is filtered down to one and back is the common case, and it
/// should not lower its rows again each time.
const FOR_POOL_FLOOR: usize = 16;

/// How far past `max(len, FOR_POOL_FLOOR)` a pool may sit before it is trimmed.
/// A pool is only trimmed after a *drastic* shrink: a list that goes from a
/// thousand rows to a handful gives the rest back, while one that wobbles
/// between 90 and 100 keeps every body it has lowered.
const FOR_POOL_SLACK: usize = 4;

/// What retired subtrees left to be released once the frame has settled.
#[derive(Default)]
pub(super) struct Retired {
    /// Scopes whose subtree is gone.
    pub(super) scopes: Vec<ScopeId>,
    /// Lifecycle effects whose node is gone, as indices into the effect slots.
    effects: Vec<usize>,
    /// `on measure` slots whose element is gone.
    measures: Vec<u32>,
}

impl Interpreter {
    /// Queues a discarded subtree: the scopes lowering it opened, and the nodes
    /// themselves, which are walked for the pools and slots they own.
    pub(super) fn retire_subtree(&mut self, scopes: Vec<ScopeId>, nodes: Vec<RenderNode>) {
        self.retired.scopes.extend(scopes);
        self.retire_nodes(nodes);
    }

    /// Walks `nodes` for what they own that is not a scope of their own: the
    /// scopes of the `when` branches and `for` rows lowered under them, the
    /// pools those live in, and the effect and measure slots. A node's own
    /// scopes (its text, its `when` condition) are in the list of whoever lowered
    /// the node, which is why they are not counted again here.
    fn retire_nodes(&mut self, nodes: Vec<RenderNode>) {
        for node in nodes {
            match node {
                RenderNode::Box {
                    children, measure, ..
                } => {
                    self.retired.measures.extend(measure);
                    self.retire_nodes(children);
                }
                RenderNode::Overlay { children, .. } => self.retire_nodes(children),
                RenderNode::When { pool, .. } => self.retire_when_pool(pool),
                RenderNode::For { pool, .. } => self.retire_for_pool(pool),
                RenderNode::Nav { pool, .. } => self.retire_nav_pool(pool),
                RenderNode::Effect { index } => self.retired.effects.push(index),
                RenderNode::Text { .. }
                | RenderNode::Spacer { .. }
                | RenderNode::Image { .. }
                | RenderNode::Native { .. }
                | RenderNode::Vector { .. }
                | RenderNode::Canvas { .. } => {}
            }
        }
    }

    fn retire_when_pool(&mut self, pool: usize) {
        // The same node can be in a tree more than once (a `content` block that
        // is written twice is spliced twice), and a pool must only be given back
        // once or two owners would be handed the same index.
        if self.free_when_pools.contains(&pool) {
            return;
        }
        let w = &mut self.when_pools[pool];
        let then = w.then.take();
        let els = w.els.take();
        let then_scopes = std::mem::take(&mut w.then_scopes);
        let els_scopes = std::mem::take(&mut w.els_scopes);
        w.then_ast = Vec::new();
        w.els_ast = Vec::new();
        w.known_views = Vec::new();
        w.env_snapshot = Vec::new();
        w.slots = None;
        w.last_take = None;
        self.free_when_pools.push(pool);
        self.retired.scopes.extend(then_scopes);
        self.retired.scopes.extend(els_scopes);
        for branch in [then, els].into_iter().flatten() {
            self.retire_nodes(branch);
        }
    }

    fn retire_for_pool(&mut self, pool: usize) {
        if self.free_for_pools.contains(&pool) {
            return;
        }
        let f = &mut self.for_pools[pool];
        let bodies = std::mem::take(&mut f.bodies);
        let slot_scopes = std::mem::take(&mut f.slot_scopes);
        let slots = std::mem::take(&mut f.item_slots);
        f.body = Vec::new();
        f.known_views = Vec::new();
        f.env_snapshot = Vec::new();
        f.slots = None;
        f.len = 0;
        f.leaving_to = 0;
        self.free_for_pools.push(pool);
        // The signals themselves stay in the table (signals are not recycled),
        // but nothing reads them again, so what they hold is let go.
        for slot in slots {
            self.ctx.reset_signal_silent(slot, super::Value::Unit);
        }
        for scopes in slot_scopes {
            self.retired.scopes.extend(scopes);
        }
        for body in bodies {
            self.retire_nodes(body);
        }
    }

    fn retire_nav_pool(&mut self, pool: usize) {
        if self.free_nav_pools.contains(&pool) {
            return;
        }
        let inst = self.nav_pools[pool].inst;
        let (entries, path_sig) = self.nav_pools[pool].vacate();
        self.free_nav_pools.push(pool);
        if let Some(slot) = self.nav_elems.get_mut(pool) {
            *slot = None;
        }
        if self.nav_swipe.is_some_and(|(p, _)| p == pool) {
            self.nav_swipe = None;
        }
        self.nav_targets.retain(|&(_, p)| p != pool);
        // The shared cell is keyed by the signal behind `path:`; another
        // container driven by the same `var` keeps using it.
        if let Some(sig) = path_sig {
            if !self.nav_pools.iter().any(|p| p.path_signal() == Some(sig)) {
                self.nav_shared.remove(&sig);
            }
        }
        for entry in entries {
            self.drop_animation_state(entry.body_span, inst);
            self.retire_subtree(entry.scopes, entry.nodes);
        }
    }

    /// Gives a `for` pool back its unused bodies after a drastic shrink
    /// (RFC-0018), so a list that went from a thousand rows to one does not keep
    /// nine hundred and ninety-nine lowered bodies for the rest of the run.
    ///
    /// Bounded rather than exact: the pool keeps twice `max(len, 16)` bodies, and
    /// is only trimmed once it holds more than four times that, so a list that
    /// wobbles never lowers a row again. The slot signals stay (the index still
    /// owns its signal, and a pool that grows back reuses it), as do the
    /// animation timelines, which `drop_slot_state` has already dropped for every
    /// row that left.
    pub(super) fn trim_for_pool(&mut self, pool: usize) {
        let f = &self.for_pools[pool];
        let base = f.len.max(f.leaving_to).max(FOR_POOL_FLOOR);
        if f.bodies.len() <= base * FOR_POOL_SLACK {
            return;
        }
        let keep = base * 2;
        let f = &mut self.for_pools[pool];
        let bodies = f.bodies.split_off(keep);
        let slot_scopes = f.slot_scopes.split_off(keep);
        for scopes in slot_scopes {
            self.retired.scopes.extend(scopes);
        }
        for body in bodies {
            self.retire_nodes(body);
        }
    }

    /// Takes a `when` pool index, reusing a retired one before growing.
    pub(super) fn alloc_when_pool(&mut self, pool: WhenPool) -> usize {
        if let Some(index) = self.free_when_pools.pop() {
            self.when_pools[index] = pool;
            index
        } else {
            self.when_pools.push(pool);
            self.when_pools.len() - 1
        }
    }

    /// Takes a `for` pool index, reusing a retired one before growing.
    pub(super) fn alloc_for_pool(&mut self, pool: ForPool) -> usize {
        if let Some(index) = self.free_for_pools.pop() {
            self.for_pools[index] = pool;
            index
        } else {
            self.for_pools.push(pool);
            self.for_pools.len() - 1
        }
    }

    /// Takes a navigation pool index, reusing a retired one before growing.
    /// `nav_elems` is parallel to the pools, so it is reset with the slot.
    pub(super) fn alloc_nav_pool(&mut self, pool: NavPool) -> usize {
        if let Some(index) = self.free_nav_pools.pop() {
            self.nav_pools[index] = pool;
            self.nav_elems[index] = None;
            index
        } else {
            self.nav_pools.push(pool);
            self.nav_elems.push(None);
            self.nav_pools.len() - 1
        }
    }

    /// Releases everything retired since the last call: the scopes (their slots
    /// go back to the arena), and the closures of the effects and `on measure`
    /// slots that belonged to the subtrees, which would otherwise sit in their
    /// tables holding the environment they captured.
    ///
    /// Called once the frame's effects have settled, so a screen's `on unmount`
    /// has run against scopes that were still there.
    pub(super) fn release_retired(&mut self) {
        if self.retired.scopes.is_empty()
            && self.retired.effects.is_empty()
            && self.retired.measures.is_empty()
        {
            return;
        }
        for scope in self.retired.scopes.drain(..) {
            self.ctx.drop_scope(scope);
        }
        for index in std::mem::take(&mut self.retired.effects) {
            self.release_effect(index);
        }
        for index in std::mem::take(&mut self.retired.measures) {
            if let Some(slot) = self.measures.get_mut(index as usize) {
                slot.action = std::rc::Rc::new(std::cell::RefCell::new(Box::new(|_, _| {})));
            }
        }
    }
}
