//! Several surfaces in one program (RFC-0068 §1, §2, §5, §6).
//!
//! A program with no `Window` is one window drawn by [`Interpreter::render`],
//! and nothing here is built for it. A program that writes `Window` is drawn by
//! [`Interpreter::render_surfaces`], and every `Window` that is mounted is a
//! surface with its own layout tree, hit-test data, hover, pointer capture,
//! keyboard focus, scroll state and accessibility tree.
//!
//! # How a surface is kept apart
//!
//! There is one interpreter, one reactive graph and one set of pools, so a
//! `var` written in one window is read by an element of another in the same
//! tick. What belongs to a surface is the state the interpreter keeps *about a
//! layout*: the atlas, the router, the scroll and snap state keyed by layout
//! index, the paint digest, the focused field's edit state. All of it is a
//! field of [`SurfaceState`], and the interpreter's own fields hold the state
//! of the surface that is bound: `bind` swaps the two, a handful of moves of
//! pointers, and the code that draws or routes a surface is the code a single
//! window runs, unchanged.
//!
//! # What a tick does
//!
//! 1. the structure of the whole program is settled once (every `when` and
//!    `for`, every mount and unmount edge), so a window's `on mount` fires
//!    whether or not the window is drawn this tick;
//! 2. the mounted windows are found, and compared with the surfaces that are
//!    open: a new one is a [`SurfaceChange::Create`], one that is gone a
//!    `Destroy`, one whose `title` or bounds moved a `Configure`;
//! 3. each surface is asked whether anything it showed has changed (input to
//!    it, an animation in it, a signal or a memo it read, its size), and only
//!    those are drawn, each into a frame of its own.
//!
//! A tick in which nothing was written, nothing was routed and nothing is
//! moving does none of this: it returns after comparing two integers.

use std::cell::RefCell;
use std::rc::Rc;

use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind as CoreKind, InputPayload};
use byard_core::{InputEvent, SurfaceChange, SurfaceId, SurfaceSpec};

use super::layout::{Pools, SharedFrame};
use super::{Interpreter, RenderNode};
use crate::interp::env::{SignalId, Value};
use crate::interp::events::Action;
use crate::interp::reactive::ReadSet;
use crate::parser::ast::{Attr, AttrKind, ElementNode, Member};
use crate::symbol::Symbol;

/// The element that is a window.
pub(super) const WINDOW: &str = "Window";

/// The name of the context node a `Window` puts in the chain while its content
/// is lowered: no program can write a name that starts with `$`.
const WINDOW_CTX: &str = "$window";

/// Everything the interpreter keeps about one surface's layout, parked while
/// another surface is bound. See the module documentation.
#[derive(Default)]
pub(super) struct SurfaceState {
    atlas: byard_core::atlas::layout::LayoutAtlas,
    router: crate::interp::events::EventRouter,
    viewport: (f32, f32),
    retained: super::layout::RetainedLayout,
    paint: byard_core::frame::PaintDigest,
    scroll_targets: Vec<super::scroll::ScrollTarget>,
    wheel_fields: Vec<(crate::interp::intrinsics::Rect, super::editing::FieldHandle)>,
    native_targets: Vec<super::wiring::NativeTarget>,
    nav_targets: Vec<(crate::interp::intrinsics::Rect, usize)>,
    nav_swipe: Option<(usize, f32)>,
    scroll_item_bounds: std::collections::HashMap<u32, Vec<f32>>,
    end_reached_fired: std::collections::HashSet<u32>,
    scroll_page_last: std::collections::HashMap<u32, i64>,
    scroll_quiet: std::collections::HashMap<u32, u64>,
    snap_anims: std::collections::HashMap<u32, super::scroll::SnapAnim>,
    pull_distance: std::collections::HashMap<u32, f32>,
    pull_anims: std::collections::HashMap<u32, super::scroll::PullAnim>,
    refreshing_seen: std::collections::HashMap<u32, bool>,
    scroll_vel: std::collections::HashMap<u32, f32>,
    scroll_vel_last: std::collections::HashMap<u32, (f32, u64)>,
    scroll_drag: Option<super::scroll::ScrollDrag>,
    pending_scroll: Option<super::scroll::ScrollDrag>,
    ripples: Vec<super::paint::ActiveRipple>,
    ripple_spawned: Option<(u32, u64)>,
    frame_scopes: Vec<crate::interp::reactive::ScopeId>,
    held_scopes: Vec<crate::interp::reactive::ScopeId>,
    elem_uids: Vec<(u32, u32)>,
    prev_elem_uids: Vec<(u32, u32)>,
    entered_prev: std::collections::HashSet<u64>,
    entered_now: std::collections::HashSet<u64>,
    live_scopes: Vec<super::focus::LiveScope>,
    edit: Rc<RefCell<crate::interp::edit::EditStore>>,
    composition: Option<super::text_field::Composition>,
    blink: Option<super::editing::Blink>,
    caret: super::editing::CaretFlags,
}

/// Swaps every field of [`SurfaceState`] with the interpreter's own.
macro_rules! swap_state {
    ($it:expr, $st:expr; $($field:ident),+ $(,)?) => {
        $( std::mem::swap(&mut $it.$field, &mut $st.$field); )+
    };
}

/// One open surface: a mounted `Window`.
// The flags are independent reasons to draw it again, each its own switch.
#[allow(clippy::struct_excessive_bools)]
struct Live {
    id: SurfaceId,
    /// The identity of the `Window` node that is this surface: its element's
    /// uid, which a `for … key` keeps with the item through any reorder.
    uid: u32,
    spec: SurfaceSpec,
    /// The size the surface is drawn at, logical pixels. The spec's opening
    /// size until the host says otherwise.
    size: (f32, f32),
    /// The signals behind `window.width` and `window.height` inside it.
    win: (SignalId, SignalId),
    close: Option<Rc<RefCell<Action>>>,
    /// The surface's own state while another is bound; `None` while this one is.
    state: Option<Box<SurfaceState>>,
    /// What the last drawing read, and the sequence it ended at.
    reads: ReadSet,
    drawn_seq: u64,
    /// The size it was last drawn at; `None` before the first drawing.
    drawn_size: Option<(f32, f32)>,
    /// Something is still moving in it.
    wants_frame: bool,
    /// Input was routed to it since it was drawn.
    input: bool,
    /// Its own structure moved since it was drawn.
    structure_moved: bool,
    /// Whether the caret was blinked off when it was last drawn.
    caret_seen: bool,
    /// The hash of the accessibility tree last handed to the relay.
    access_hash: Option<u64>,
    /// How many times it has been drawn.
    paints: u64,
}

/// The surfaces of a program with windows.
#[derive(Default)]
pub(super) struct Surfaces {
    /// The next id: `MAIN` is the implicit window of a single-surface program,
    /// so a window gets one from `1`, and none is used twice in a run.
    next_id: u32,
    live: Vec<Live>,
    changes: Vec<SurfaceChange>,
    /// The program asked to end (a window closed with no `close =>`, or the
    /// last window closed).
    quit: bool,
    /// Whether a window was ever open, so the last one closing can be told
    /// from none having opened yet.
    had_any: bool,
    /// The sequence the last tick that drew ended at; nothing to do while it
    /// has not moved.
    idle_seq: Option<u64>,
    /// Every surface is drawn again at the next tick.
    force: bool,
}

impl Surfaces {
    /// Marks every surface to be drawn again.
    pub(super) fn force_all(&mut self) {
        self.force = true;
    }

    fn index(&self, id: SurfaceId) -> Option<usize> {
        self.live.iter().position(|l| l.id == id)
    }
}

impl Live {
    /// Whether anything this surface showed may have changed since it was
    /// drawn.
    fn needs_drawing(&self, ctx: &crate::interp::reactive::ReactiveCtx, force: bool) -> bool {
        self.drawn_size != Some(self.size)
            || force
            || self.input
            || self.wants_frame
            || self.structure_moved
            || ctx.reads_changed_since(&self.reads, self.drawn_seq)
    }
}

impl Interpreter {
    // ── lowering ────────────────────────────────────────────────────────

    /// The signals behind `window.width` and `window.height` of the `Window`
    /// being lowered (RFC-0068 §4), found in the context chain the window put
    /// there.
    pub(super) fn enclosing_window(&self) -> Option<(SignalId, SignalId)> {
        let mut node = self.ctx_chain.as_ref();
        while let Some(n) = node {
            if n.name.as_str() == WINDOW_CTX {
                return match (&n.fields[0].1, &n.fields[1].1) {
                    (Value::Signal(w), Value::Signal(h)) => Some((*w, *h)),
                    _ => None,
                };
            }
            node = n.parent.as_ref();
        }
        None
    }

    /// Lowers a `Window`: an element whose content is lowered with its own
    /// `window` in scope. The node is a plain `Box` named `Window`; the
    /// surface level (see [`Interpreter::render_surfaces`]) is what gives it
    /// a meaning, and nothing else ever lays it out.
    pub(super) fn lower_window(
        &mut self,
        el: &ElementNode,
        attrs: Vec<Attr>,
        uid: u32,
        members: &[Member],
        known_views: &[&str],
    ) -> RenderNode {
        let width = self.ctx.create_signal(Value::Float(0.0));
        let height = self.ctx.create_signal(Value::Float(0.0));
        self.window_pairs.push((uid, (width, height)));
        let outer = self.ctx_chain.take();
        self.ctx_chain = Some(Rc::new(super::CtxNode {
            parent: outer.clone(),
            name: Symbol::intern(WINDOW_CTX),
            fields: vec![
                (Symbol::intern("width"), Value::Signal(width)),
                (Symbol::intern("height"), Value::Signal(height)),
            ],
        }));
        let children = self.lower_members(members, known_views);
        self.ctx_chain = outer;
        RenderNode::Box {
            name: Symbol::intern(WINDOW),
            attrs,
            state_blocks: Vec::new(),
            anchor_name: el.anchor_name.clone(),
            children,
            action: None,
            bound_sig: None,
            env_snapshot: self.capture_env_snapshot(),
            measure: None,
            uid,
            inst: self.cur_inst,
            clip_path: None,
        }
    }

    // ── binding ─────────────────────────────────────────────────────────

    /// Swaps the interpreter's per-surface fields with `st`.
    fn swap_surface_state(&mut self, st: &mut SurfaceState) {
        swap_state!(
            self, st;
            atlas, router, viewport, retained, paint, scroll_targets, wheel_fields,
            native_targets, nav_targets, nav_swipe, scroll_item_bounds, end_reached_fired,
            scroll_page_last, scroll_quiet, snap_anims, pull_distance, pull_anims,
            refreshing_seen, scroll_vel, scroll_vel_last, scroll_drag, pending_scroll,
            ripples, ripple_spawned, frame_scopes, held_scopes, elem_uids, prev_elem_uids,
            entered_prev, entered_now, live_scopes, edit, composition, blink, caret,
        );
    }

    /// Takes surface `index`'s state out and binds it: until [`unbind`], the
    /// interpreter's atlas, router and the rest are that surface's.
    ///
    /// [`unbind`]: Self::unbind_surface
    fn bind_surface(&mut self, index: usize) -> Box<SurfaceState> {
        let mut st = self
            .surfaces
            .as_mut()
            .and_then(|s| s.live[index].state.take())
            .expect("a surface is bound once at a time");
        self.swap_surface_state(&mut st);
        st
    }

    /// Gives the surface's state back and restores the interpreter's own.
    fn unbind_surface(&mut self, index: usize, mut st: Box<SurfaceState>) {
        self.swap_surface_state(&mut st);
        if let Some(s) = self.surfaces.as_mut() {
            s.live[index].state = Some(st);
        }
    }

    // ── what hosts read and write ───────────────────────────────────────

    /// Turns on the surfaces of a program with windows. A host that has read
    /// that the program has a `Window` (`surface_shape`) calls this before the
    /// first tick, so input is routed by surface from the start.
    pub fn enable_surfaces(&mut self) {
        self.surfaces.get_or_insert_with(Box::default);
    }

    /// Whether this interpreter draws surfaces: a program with windows.
    #[must_use]
    pub fn has_surfaces(&self) -> bool {
        self.surfaces.is_some()
    }

    /// The title a window with no `title` has: the one the single window of the
    /// program has, which the host knows.
    pub fn set_default_window_title(&mut self, title: &str) {
        self.default_window_title = title.to_string();
    }

    /// The surfaces that are open, in the order they were mounted.
    #[must_use]
    pub fn surface_ids(&self) -> Vec<SurfaceId> {
        self.surfaces
            .as_ref()
            .map(|s| s.live.iter().map(|l| l.id).collect())
            .unwrap_or_default()
    }

    /// What surface `id` is made of now.
    #[must_use]
    pub fn surface_spec(&self, id: SurfaceId) -> Option<&SurfaceSpec> {
        let s = self.surfaces.as_ref()?;
        s.live.iter().find(|l| l.id == id).map(|l| &l.spec)
    }

    /// The size surface `id` is drawn at, in logical pixels.
    #[must_use]
    pub fn surface_size(&self, id: SurfaceId) -> Option<(f32, f32)> {
        let s = self.surfaces.as_ref()?;
        s.live.iter().find(|l| l.id == id).map(|l| l.size)
    }

    /// How many times surface `id` has been drawn: what the tests of "a surface
    /// none of whose elements changed is not drawn" count.
    #[must_use]
    pub fn surface_paints(&self, id: SurfaceId) -> u64 {
        self.surfaces
            .as_ref()
            .and_then(|s| s.live.iter().find(|l| l.id == id))
            .map_or(0, |l| l.paints)
    }

    /// The element that has keyboard focus on surface `id`, if any (for tests
    /// and hosts that mirror focus).
    #[must_use]
    pub fn focused_element_of(&self, id: SurfaceId) -> Option<u32> {
        let live = self.surfaces.as_ref()?.live.iter().find(|l| l.id == id)?;
        match &live.state {
            Some(st) => st.router.focused_element(),
            None => self.router.focused_element(),
        }
    }

    /// Tells the interpreter the size surface `id` is now, in logical pixels:
    /// the user resized the window, or the platform gave it its first size. A
    /// surface is drawn again at the next tick, and its `window.width` and
    /// `window.height` follow.
    pub fn set_surface_size(&mut self, id: SurfaceId, size: (f32, f32)) {
        if let Some(l) = self
            .surfaces
            .as_mut()
            .and_then(|s| s.live.iter_mut().find(|l| l.id == id))
        {
            l.size = size;
        }
    }

    /// Takes the changes to the set of surfaces the last tick made, in order:
    /// the host applies them on its main thread.
    pub fn take_surface_changes(&mut self) -> Vec<SurfaceChange> {
        self.surfaces
            .as_mut()
            .map(|s| std::mem::take(&mut s.changes))
            .unwrap_or_default()
    }

    /// Whether the program asked to end: a window closed that has no
    /// `close =>`, or the last window closed (RFC-0068 §2).
    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.surfaces.as_ref().is_some_and(|s| s.quit)
    }

    /// Whether any surface wants another tick to be drawn without waiting for
    /// an event: something is moving, or something it read has changed since.
    pub(super) fn surfaces_want_frame(&self) -> bool {
        self.surfaces.as_ref().is_some_and(|s| {
            s.force
                || s.live.iter().any(|l| {
                    l.wants_frame
                        || l.structure_moved
                        || l.input
                        || (l.drawn_size.is_some()
                            && self.ctx.reads_changed_since(&l.reads, l.drawn_seq))
                })
        })
    }

    // ── input ───────────────────────────────────────────────────────────

    /// Sets the clock of every surface's router: a press that is held, and the
    /// time between two taps, are counted by it.
    pub(super) fn set_surface_clocks(&mut self, ms: u64) {
        let Some(surfaces) = self.surfaces.as_mut() else {
            return;
        };
        for live in &mut surfaces.live {
            if let Some(st) = live.state.as_mut() {
                st.router.set_clock_ms(&mut self.ctx, ms);
            }
        }
    }

    /// Routes `events` by the surface each names (RFC-0068 §6): that surface's
    /// hit test, its hover, its pointer capture. An event for a surface that
    /// is not open is dropped, since there is nothing there to route it to.
    pub(super) fn dispatch_to_surfaces(&mut self, events: &[InputEvent]) {
        let mut groups: Vec<(SurfaceId, Vec<InputEvent>)> = Vec::new();
        for ev in events {
            match ev.kind {
                // The system's appearance is the program's, not a surface's.
                CoreKind::SystemScheme => {
                    if let (true, Some(InputPayload::Bool(dark))) =
                        (self.theme.follows_system, ev.payload.as_ref())
                    {
                        if self.theme_is_dark() != *dark {
                            self.set_theme_dark(*dark);
                        }
                    }
                }
                CoreKind::CloseRequest => self.close_requested(ev.surface),
                _ => match groups.iter_mut().find(|(id, _)| *id == ev.surface) {
                    Some((_, group)) => group.push(ev.clone()),
                    None => groups.push((ev.surface, vec![ev.clone()])),
                },
            }
        }
        for (id, group) in groups {
            let Some(index) = self.surfaces.as_ref().and_then(|s| s.index(id)) else {
                continue;
            };
            let st = self.bind_surface(index);
            self.dispatch_events_bound(&group);
            self.unbind_surface(index, st);
            if let Some(s) = self.surfaces.as_mut() {
                s.live[index].input = true;
            }
        }
    }

    /// The platform asked to close surface `id`: its `close =>` runs, and a
    /// window with none ends the program (RFC-0068 §2).
    fn close_requested(&mut self, id: SurfaceId) {
        let close = self
            .surfaces
            .as_ref()
            .and_then(|s| s.live.iter().find(|l| l.id == id))
            .map(|l| l.close.clone());
        match close {
            Some(Some(action)) => (action.borrow_mut())(&mut self.ctx, None),
            Some(None) => {
                if let Some(s) = self.surfaces.as_mut() {
                    s.quit = true;
                }
            }
            None => {}
        }
    }

    // ── the tick ────────────────────────────────────────────────────────

    /// One tick of a program with windows, after its input was routed
    /// ([`dispatch_events`](Self::dispatch_events)), the reactive graph pulled
    /// ([`tick`](Self::tick)) and the clock set: settles the structure of the
    /// program, opens and closes surfaces, and draws each surface that has
    /// something new into the frame `new_frame` gives it.
    ///
    /// `out` receives the frames of the surfaces that were drawn, each with its
    /// surface; a surface with nothing new is not drawn, not published and not
    /// presented (RFC-0068 §5). The set of surfaces that opened, changed or
    /// closed is in [`take_surface_changes`](Self::take_surface_changes).
    pub fn render_surfaces(
        &mut self,
        tree: &[RenderNode],
        new_frame: &mut dyn FnMut() -> RenderFrame,
        out: &mut Vec<(SurfaceId, RenderFrame)>,
    ) {
        self.enable_surfaces();
        if self.surfaces_are_idle() {
            return;
        }
        // The structure of the whole program, once: the mount and unmount edges
        // of every window's content fire whether or not the window is drawn.
        self.frame_seq = self.frame_seq.wrapping_add(1);
        self.leave_busy_prev = std::mem::take(&mut self.leave_busy);
        self.sync_surface_sizes();
        self.settle_structure(tree);
        // Windows that opened or closed. A window that opened has a size to
        // read (`window.width` in its content), which can settle more
        // structure, so that is done again with the size written.
        for _ in 0..3 {
            self.with_windows(tree, Self::reconcile_surfaces);
            if !self.sync_surface_sizes() {
                break;
            }
            self.settle_structure(tree);
        }

        let for_pools = std::mem::take(&mut self.for_pools);
        let when_pools = std::mem::take(&mut self.when_pools);
        let nav_pools = std::mem::take(&mut self.nav_pools);
        let pools = Pools {
            fors: &for_pools,
            whens: &when_pools,
            navs: &nav_pools,
        };
        let windows = self.mounted_windows(tree, pools);
        let live_now: Vec<(usize, &RenderNode)> = windows
            .iter()
            .filter_map(|&node| {
                let RenderNode::Box { uid, .. } = node else {
                    return None;
                };
                let index = self
                    .surfaces
                    .as_ref()?
                    .live
                    .iter()
                    .position(|l| l.uid == *uid)?;
                Some((index, node))
            })
            .collect();
        let force = self.surfaces.as_ref().is_some_and(|s| s.force);
        for (index, node) in live_now {
            let RenderNode::Box { children, .. } = node else {
                continue;
            };
            let Some(live) = self.surfaces.as_ref().map(|s| &s.live[index]) else {
                continue;
            };
            if !live.needs_drawing(&self.ctx, force) {
                continue;
            }
            let (size, structure_moved) = (live.size, live.structure_moved);
            let id = live.id;
            let mut frame = new_frame();
            let st = self.bind_surface(index);
            self.ctx.watch_reads();
            self.ctx.push_scope_tracker();
            self.render_frame(
                children,
                &mut frame,
                size.0,
                size.1,
                Some(SharedFrame {
                    pools,
                    structure_changed: structure_moved,
                }),
            );
            self.frame_scopes = self.ctx.pop_scope_tracker();
            let reads = self.ctx.take_reads();
            let wants = self.any_active
                || self.endless_seq == self.frame_seq
                || self.theme_transitioning()
                || self.measure_fired
                || !self.ripples.is_empty();
            let blink = self.blink.is_some();
            self.unbind_surface(index, st);
            let seq = self.ctx.seq();
            let caret_off = self.caret_off.get();
            if let Some(s) = self.surfaces.as_mut() {
                let l = &mut s.live[index];
                l.reads = reads;
                l.drawn_seq = seq;
                l.drawn_size = Some(size);
                l.wants_frame = wants;
                l.input = false;
                l.structure_moved = false;
                l.caret_seen = blink && caret_off;
                l.paints += 1;
            }
            out.push((id, frame));
        }
        self.for_pools = for_pools;
        self.when_pools = when_pools;
        self.nav_pools = nav_pools;
        let seq = self.ctx.seq();
        if let Some(s) = self.surfaces.as_mut() {
            s.force = false;
            s.idle_seq = Some(seq);
        }
    }

    /// The `Window` nodes mounted at the surface level of `tree` right now: the
    /// ones in the taken branch of a `when`, the live rows of a `for`.
    fn mounted_windows<'a>(&self, tree: &'a [RenderNode], pools: Pools<'a>) -> Vec<&'a RenderNode> {
        self.expand_concrete_in(tree, pools, super::animation::AnimSlot::default())
            .into_iter()
            .map(|c| c.node)
            .filter(|n| matches!(n, RenderNode::Box { name, .. } if name.as_str() == WINDOW))
            .collect()
    }

    /// Runs `f` over the windows mounted in `tree`, with the pools lent out
    /// for as long as it takes and put back after.
    fn with_windows(&mut self, tree: &[RenderNode], f: impl FnOnce(&mut Self, &[&RenderNode])) {
        let for_pools = std::mem::take(&mut self.for_pools);
        let when_pools = std::mem::take(&mut self.when_pools);
        let nav_pools = std::mem::take(&mut self.nav_pools);
        let pools = Pools {
            fors: &for_pools,
            whens: &when_pools,
            navs: &nav_pools,
        };
        let windows = self.mounted_windows(tree, pools);
        f(self, &windows);
        drop(windows);
        self.for_pools = for_pools;
        self.when_pools = when_pools;
        self.nav_pools = nav_pools;
    }

    /// One tick of a program with windows, handed to the relay (RFC-0068 §5):
    /// each surface that was drawn is published to its own slot, the surfaces
    /// that opened, changed or closed are queued for the main thread, and what
    /// the tick did is said in the result so the logic loop wakes the render
    /// thread only for a tick that gave it something.
    pub fn render_surfaces_to(
        &mut self,
        tree: &[RenderNode],
        relay: &byard_core::relay::Relay,
    ) -> byard_core::relay::SurfaceTick {
        let mut out = Vec::new();
        self.render_surfaces(tree, &mut || relay.acquire_recycled(), &mut out);
        let mut tick = byard_core::relay::SurfaceTick::default();
        for (id, frame) in out {
            // The accessibility tree of this surface, when it differs from the
            // one its adapter was last given.
            let hash = frame
                .access()
                .map(byard_core::access::AccessTree::content_hash);
            if let Some(live) = self
                .surfaces
                .as_mut()
                .and_then(|s| s.live.iter_mut().find(|l| l.id == id))
            {
                if hash.is_some() && hash != live.access_hash {
                    tick.access_changed = true;
                }
                live.access_hash = hash;
            }
            relay.publish_to(id, frame);
            tick.painted += 1;
        }
        for change in self.take_surface_changes() {
            let gone = match &change {
                SurfaceChange::Destroy { id } => Some(*id),
                _ => None,
            };
            relay.push_surface_change(change);
            if let Some(id) = gone {
                // Nothing publishes for it after its `Destroy`, so its slot goes.
                relay.remove_surface(id);
            }
        }
        tick
    }

    /// Whether a tick has nothing to do: no write, evaluation or new
    /// projection since the last tick ended, no input waiting, nothing moving.
    fn surfaces_are_idle(&self) -> bool {
        let Some(s) = self.surfaces.as_ref() else {
            return false;
        };
        s.idle_seq == Some(self.ctx.seq())
            && !s.force
            && !self.theme_transitioning()
            && s.live.iter().all(|l| {
                !l.wants_frame
                    && !l.input
                    && !l.structure_moved
                    && l.drawn_size == Some(l.size)
                    && (!self.caret_off_changed(l))
            })
    }

    /// Whether a surface's blinking caret has changed phase since it was drawn.
    fn caret_off_changed(&self, l: &Live) -> bool {
        let blinking = l.state.as_ref().is_some_and(|st| st.blink.is_some());
        blinking && l.caret_seen != self.caret_off.get()
    }

    /// Writes each surface's size into the signals behind its `window`, and
    /// pulls when one changed, so a `when window.width >= md` is reconciled in
    /// the tick the window changed in.
    fn sync_surface_sizes(&mut self) -> bool {
        let Some(surfaces) = self.surfaces.as_ref() else {
            return false;
        };
        let sizes: Vec<((SignalId, SignalId), (f32, f32))> =
            surfaces.live.iter().map(|l| (l.win, l.size)).collect();
        let mut changed = false;
        for ((w, h), (vw, vh)) in sizes {
            for (sig, now) in [(w, vw), (h, vh)] {
                let now = Value::Float(f64::from(now));
                if self.ctx.peek_signal(sig) != now {
                    self.ctx.write_signal(sig, now);
                    changed = true;
                }
            }
        }
        if changed {
            let epoch = self.ctx.begin_tick();
            self.ctx.pull(epoch);
        }
        changed
    }

    /// A window's own structure moved (a `when` or a `for` inside it): it has to
    /// be drawn again.
    pub(super) fn surface_structure_moved(&mut self, uid: u32) {
        if let Some(l) = self
            .surfaces
            .as_mut()
            .and_then(|s| s.live.iter_mut().find(|l| l.uid == uid))
        {
            l.structure_moved = true;
        }
    }

    // ── opening and closing ─────────────────────────────────────────────

    /// Compares the mounted windows with the surfaces that are open: a new
    /// window opens a surface, one that is gone closes its, and one whose
    /// reactive attributes moved is reconfigured.
    fn reconcile_surfaces(&mut self, windows: &[&RenderNode]) {
        let mut seen: Vec<u32> = Vec::with_capacity(windows.len());
        for &node in windows {
            let RenderNode::Box {
                attrs,
                anchor_name,
                env_snapshot,
                uid,
                ..
            } = node
            else {
                continue;
            };
            seen.push(*uid);
            let existing = self
                .surfaces
                .as_ref()
                .and_then(|s| s.live.iter().position(|l| l.uid == *uid));
            match existing {
                Some(index) => {
                    let prev = self.surfaces.as_ref().map(|s| s.live[index].spec.clone());
                    let spec =
                        self.window_spec(attrs, anchor_name.as_ref(), env_snapshot, prev.as_ref());
                    if let Some(s) = self.surfaces.as_mut() {
                        let l = &mut s.live[index];
                        if l.spec != spec {
                            l.spec = spec.clone();
                            s.changes.push(SurfaceChange::Configure { id: l.id, spec });
                        }
                    }
                }
                None => {
                    self.open_surface(node);
                }
            }
        }
        // A surface whose window is not mounted any more is closed.
        if let Some(s) = self.surfaces.as_mut() {
            let mut i = 0;
            while i < s.live.len() {
                if seen.contains(&s.live[i].uid) {
                    i += 1;
                } else {
                    let gone = s.live.remove(i);
                    s.changes.push(SurfaceChange::Destroy { id: gone.id });
                }
            }
            if s.live.is_empty() && s.had_any {
                // The last window closed, and a program of windows alone has
                // nothing left to show.
                s.quit = true;
            }
        }
    }

    /// Opens the surface of a window that was just mounted: an id that was
    /// never used, its spec, its `close =>`.
    fn open_surface(&mut self, node: &RenderNode) -> usize {
        let RenderNode::Box {
            attrs,
            anchor_name,
            env_snapshot,
            uid,
            ..
        } = node
        else {
            unreachable!("a window is a box");
        };
        let spec = self.window_spec(attrs, anchor_name.as_ref(), env_snapshot, None);
        let close = self.lower_close(attrs, env_snapshot);
        let win = self
            .window_pairs
            .iter()
            .find(|(u, _)| u == uid)
            .map_or_else(
                || {
                    (
                        self.ctx.create_signal(Value::Float(0.0)),
                        self.ctx.create_signal(Value::Float(0.0)),
                    )
                },
                |(_, pair)| *pair,
            );
        let mut spec = spec;
        spec.handles_close = close.is_some();
        let surfaces = self.surfaces.get_or_insert_with(Box::default);
        surfaces.next_id += 1;
        surfaces.had_any = true;
        let id = SurfaceId::new(surfaces.next_id);
        surfaces.changes.push(SurfaceChange::Create {
            id,
            spec: spec.clone(),
        });
        let size = spec.size;
        surfaces.live.push(Live {
            id,
            uid: *uid,
            spec,
            size,
            win,
            close,
            state: Some(Box::default()),
            reads: ReadSet::default(),
            drawn_seq: 0,
            drawn_size: None,
            wants_frame: false,
            input: false,
            structure_moved: false,
            caret_seen: false,
            access_hash: None,
            paints: 0,
        });
        surfaces.live.len() - 1
    }

    /// The `close =>` of a window, lowered once, among the names it was written
    /// in.
    fn lower_close(
        &mut self,
        attrs: &[Attr],
        env_snapshot: &[(Symbol, Value)],
    ) -> Option<Rc<RefCell<Action>>> {
        let attr = attrs.iter().find(|a| a.name.as_str() == "close")?;
        let AttrKind::Event { payload, action } = &attr.kind else {
            return None;
        };
        let env_base = self.env.len();
        for (k, v) in env_snapshot {
            self.env.push(k.clone(), v.clone());
        }
        let lowered = self.lower_action(action, payload.clone()).ok();
        self.env.truncate(env_base);
        lowered.map(|a| Rc::new(RefCell::new(a)))
    }

    /// What a window's attributes say it is, read now. `prev` is the spec it
    /// has: a window's `width`, `height` and `transparent` are only what it
    /// opens with (RFC-0068 §2), so they are not read again.
    fn window_spec(
        &mut self,
        attrs: &[Attr],
        name: Option<&Symbol>,
        env_snapshot: &[(Symbol, Value)],
        prev: Option<&SurfaceSpec>,
    ) -> SurfaceSpec {
        let env_base = self.env.len();
        for (k, v) in env_snapshot {
            self.env.push(k.clone(), v.clone());
        }
        let title = self
            .eval_str_prop(attrs, "title")
            .unwrap_or_else(|| self.default_window_title.clone());
        let mut spec = SurfaceSpec::window(title.clone());
        spec.name = name.map_or_else(|| title.clone(), |n| n.as_str().to_string());
        if let Some(prev) = prev {
            spec.size = prev.size;
            spec.transparent = prev.transparent;
            spec.handles_close = prev.handles_close;
        } else {
            if let Some(w) = self.eval_px_prop(attrs, "width") {
                spec.size.0 = w;
            }
            if let Some(h) = self.eval_px_prop(attrs, "height") {
                spec.size.1 = h;
            }
            spec.transparent = self.eval_bool_prop(attrs, "transparent").unwrap_or(false);
        }
        spec.min_size = (
            self.eval_px_prop(attrs, "min_width"),
            self.eval_px_prop(attrs, "min_height"),
        );
        spec.max_size = (
            self.eval_px_prop(attrs, "max_width"),
            self.eval_px_prop(attrs, "max_height"),
        );
        spec.resizable = self.eval_bool_prop(attrs, "resizable").unwrap_or(true);
        spec.decorations = self.eval_bool_prop(attrs, "decorations").unwrap_or(true);
        self.env.truncate(env_base);
        spec
    }
}
