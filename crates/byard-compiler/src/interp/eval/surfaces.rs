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
use byard_core::surface::placement::{Anchors, Layer, Margin};
use byard_core::surface::{
    Exclusive, InputMode, InputRegion, KeyboardMode, LayerSpec, Rect as IRect,
};
use byard_core::{InputEvent, ScreenInfo, SurfaceChange, SurfaceId, SurfaceSpec};

use super::layout::{Pools, SharedFrame};
use super::{Interpreter, RenderNode};
use crate::interp::env::{SignalId, Value};
use crate::interp::events::Action;
use crate::interp::reactive::ReadSet;
use crate::parser::ast::{Attr, AttrKind, ElementNode, Expr, Member};
use crate::symbol::Symbol;

/// The element that is a window.
pub(super) const WINDOW: &str = "Window";

/// The element that is a layer surface.
pub(super) const PANEL: &str = "Panel";

/// Whether a node name is a surface element.
pub(super) fn is_surface_name(name: &str) -> bool {
    name == WINDOW || name == PANEL
}

/// How far a measure lets an unbounded axis run.
const UNBOUNDED: f32 = 100_000.0;

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

/// Where each `Window` of `views` is written, by source span: the view, and
/// its place among that view's windows in source order.
pub(super) fn index_windows(
    views: &[crate::parser::ast::ViewDecl],
) -> std::collections::HashMap<(u32, u32), (Symbol, u32)> {
    fn walk(
        members: &[Member],
        view: &Symbol,
        n: &mut u32,
        out: &mut std::collections::HashMap<(u32, u32), (Symbol, u32)>,
    ) {
        for m in members {
            match m {
                Member::Element(el) => {
                    if is_surface_name(el.name.as_str()) {
                        out.insert((el.span.start, el.span.end), (view.clone(), *n));
                        *n += 1;
                    }
                    walk(&el.children, view, n, out);
                }
                Member::When { then, els, .. } => {
                    walk(then, view, n, out);
                    if let Some(els) = els {
                        walk(els, view, n, out);
                    }
                }
                Member::For { body, .. } => walk(body, view, n, out),
                _ => {}
            }
        }
    }
    let mut out = std::collections::HashMap::new();
    for v in views {
        let mut n = 0;
        walk(&v.body, &v.name, &mut n, &mut out);
    }
    out
}

/// What says which window a `Window` is when the tree is lowered again: the
/// element's place in the program, which an edit elsewhere does not move.
#[derive(Clone, Debug, Default, PartialEq)]
struct Site {
    view: Option<Symbol>,
    ordinal: u32,
    inst: u32,
    /// The `as` name.
    name: Option<String>,
    /// The title when it is written as a text or a name, which a reload that
    /// adds a window before this one leaves as it was.
    title: String,
}

/// The title of a window as written, without its span: its text, or the name
/// it reads.
fn title_key(attrs: &[Attr]) -> String {
    use crate::parser::ast::{Expr, StrPart};
    let Some(AttrKind::Prop { value }) = attrs
        .iter()
        .find(|a| a.name.as_str() == "title")
        .map(|a| &a.kind)
    else {
        return String::new();
    };
    match value {
        Expr::StrLit(parts, _) => parts
            .iter()
            .map(|p| match p {
                StrPart::Text(t) => t.clone(),
                StrPart::Interp(..) => "{}".to_string(),
            })
            .collect(),
        Expr::Ident(n, _) => format!("@{}", n.as_str()),
        _ => String::new(),
    }
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
    site: Site,
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
    /// What a panel adds to a window; `None` for a window.
    panel: Option<PanelLive>,
}

/// What a `Panel` asks, read from its attributes (RFC-0068 §3).
#[derive(Clone, Debug, PartialEq)]
struct PanelAsk {
    layer: LayerSpec,
    /// `width` and `height` as written; `None` is the content's.
    size: (Option<f32>, Option<f32>),
    /// The `as` name, else the namespace.
    name: String,
}

/// The state of a panel that a window has no use for.
struct PanelLive {
    ask: PanelAsk,
    /// The content's measured size along the axes that take it.
    measured: (f32, f32),
    /// Whether the host has been told it exists (`Create` sent). A panel has
    /// no size to ask for until it has been measured.
    announced: bool,
    /// The last input region sent.
    region: InputRegion,
}

impl PanelLive {
    /// Whether the compositor stretches this panel along each axis (anchored
    /// at both ends), which is size 0 on the wire.
    fn stretched(&self) -> (bool, bool) {
        let a = self.ask.layer.anchors;
        (
            a.contains(Anchors::LEFT) && a.contains(Anchors::RIGHT),
            a.contains(Anchors::TOP) && a.contains(Anchors::BOTTOM),
        )
    }

    /// The size the panel asks for: 0 where it is stretched, else what is
    /// written, else what the content measured.
    fn requested(&self) -> (f32, f32) {
        let (sw, sh) = self.stretched();
        (
            if sw {
                0.0
            } else {
                self.ask.size.0.unwrap_or(self.measured.0)
            },
            if sh {
                0.0
            } else {
                self.ask.size.1.unwrap_or(self.measured.1)
            },
        )
    }

    /// Which axes take the content's size and so have to be measured.
    fn measures(&self) -> (bool, bool) {
        let (sw, sh) = self.stretched();
        (
            !sw && self.ask.size.0.is_none(),
            !sh && self.ask.size.1.is_none(),
        )
    }
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
    /// The reloads that have been matched with the windows.
    reloads_seen: u32,
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
        self.ready()
            && (self.drawn_size != Some(self.size)
                || force
                || self.input
                || self.wants_frame
                || self.structure_moved
                || ctx.reads_changed_since(&self.reads, self.drawn_seq))
    }

    /// Whether the surface has a size to be drawn at. A panel has none until
    /// the compositor has configured it along every axis it stretches
    /// (RFC-0068 §5: it draws its first frame when the platform gave it a size).
    fn ready(&self) -> bool {
        self.panel.is_none() || (self.size.0 > 0.0 && self.size.1 > 0.0)
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
        if let Some(site) = self.window_sites.get(&(el.span.start, el.span.end)) {
            self.window_keys.insert(uid, site.clone());
        }
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
            name: el.name.clone(),
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
            if self
                .surfaces
                .as_ref()
                .is_some_and(|s| s.live[index].panel.is_some())
            {
                self.settle_panel(index, children, pools, force);
            }
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
            if force {
                // A reload or a theme change: this surface's last layout says
                // nothing about the tree it is drawn from now.
                self.retained.invalidated = true;
                self.paint.reset();
            }
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
            let region = self.panel_region(index);
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
                if let (Some(p), Some(region)) = (l.panel.as_mut(), region) {
                    if p.region != region {
                        p.region = region.clone();
                        s.changes.push(SurfaceChange::InputRegion { id, region });
                    }
                }
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
            .filter(|n| matches!(n, RenderNode::Box { name, .. } if is_surface_name(name.as_str())))
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
                    && (!l.ready() || l.drawn_size == Some(l.size))
                    && l.panel.as_ref().is_none_or(|p| p.announced)
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

    /// The place in the program a `Window` node is written.
    fn site_of(&self, node: &RenderNode) -> Site {
        let RenderNode::Box {
            attrs,
            anchor_name,
            uid,
            inst,
            ..
        } = node
        else {
            return Site::default();
        };
        let at = self.window_keys.get(uid);
        Site {
            view: at.map(|(v, _)| v.clone()),
            ordinal: at.map_or(0, |(_, n)| *n),
            inst: *inst,
            name: anchor_name.as_ref().map(|n| n.as_str().to_string()),
            title: title_key(attrs),
        }
    }

    /// After a reload the tree was lowered again, so every window is a new node.
    /// The ones that are still written where they were keep their surface
    /// (RFC-0068 §2, identity; Q12): matched, in turn, by the `as` name, by the
    /// title as written, and by their place among the view's windows, so an
    /// edit to a window's content, its title, or a window added before it
    /// leaves the others as they were. Whatever is left over opens or closes.
    fn rematch_after_reload(&mut self, windows: &[&RenderNode]) {
        let Some(surfaces) = self.surfaces.as_ref() else {
            return;
        };
        let mounted: Vec<u32> = windows
            .iter()
            .filter_map(|n| match n {
                RenderNode::Box { uid, .. } => Some(*uid),
                _ => None,
            })
            .collect();
        let mut stale: Vec<usize> = (0..surfaces.live.len())
            .filter(|&i| !mounted.contains(&surfaces.live[i].uid))
            .collect();
        let live_uids: Vec<u32> = surfaces.live.iter().map(|l| l.uid).collect();
        let mut fresh: Vec<&RenderNode> = windows
            .iter()
            .copied()
            .filter(|n| matches!(n, RenderNode::Box { uid, .. } if !live_uids.contains(uid)))
            .collect();
        for pass in 0..3 {
            let mut k = 0;
            while k < fresh.len() {
                let site = self.site_of(fresh[k]);
                let hit = stale.iter().position(|&i| {
                    let have = &self.surfaces.as_ref().expect("surfaces").live[i].site;
                    have.view == site.view
                        && have.inst == site.inst
                        && match pass {
                            0 => site.name.is_some() && have.name == site.name,
                            1 => {
                                have.name == site.name
                                    && !site.title.is_empty()
                                    && have.title == site.title
                            }
                            _ => have.ordinal == site.ordinal,
                        }
                });
                let Some(h) = hit else {
                    k += 1;
                    continue;
                };
                let index = stale.remove(h);
                let node = fresh.remove(k);
                let RenderNode::Box {
                    attrs,
                    env_snapshot,
                    uid,
                    ..
                } = node
                else {
                    continue;
                };
                let win = self
                    .window_pairs
                    .iter()
                    .find(|(u, _)| u == uid)
                    .map(|(_, pair)| *pair);
                let close = self.lower_close(attrs, env_snapshot);
                if let Some(l) = self.surfaces.as_mut().map(|s| &mut s.live[index]) {
                    l.uid = *uid;
                    if let Some(win) = win {
                        l.win = win;
                    }
                    l.site = site;
                    l.close = close;
                    l.structure_moved = true;
                }
            }
        }
    }

    /// Compares the mounted windows with the surfaces that are open: a new
    /// window opens a surface, one that is gone closes its, and one whose
    /// reactive attributes moved is reconfigured.
    fn reconcile_surfaces(&mut self, windows: &[&RenderNode]) {
        let reloads = self.reloads;
        if self
            .surfaces
            .as_mut()
            .is_some_and(|s| std::mem::replace(&mut s.reloads_seen, reloads) != reloads)
        {
            self.rematch_after_reload(windows);
        }
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
                Some(index) if node_is_panel(node) => {
                    let ask = self.panel_ask(attrs, anchor_name.as_ref(), env_snapshot);
                    let Some(s) = self.surfaces.as_mut() else {
                        continue;
                    };
                    let l = &mut s.live[index];
                    let Some(p) = l.panel.as_mut() else {
                        continue;
                    };
                    // Fixed when the surface is made: a change makes it again.
                    if p.ask.layer.namespace != ask.layer.namespace
                        || p.ask.layer.screen != ask.layer.screen
                    {
                        let gone = s.live.remove(index);
                        if gone.panel.as_ref().is_some_and(|p| p.announced) {
                            s.changes.push(SurfaceChange::Destroy { id: gone.id });
                        }
                        self.open_surface(node);
                    } else if p.ask != ask {
                        p.ask = ask;
                        l.structure_moved = true;
                    }
                }
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
                    if gone.panel.as_ref().is_none_or(|p| p.announced) {
                        s.changes.push(SurfaceChange::Destroy { id: gone.id });
                    }
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
        let panel = node_is_panel(node)
            .then(|| self.panel_ask(attrs, anchor_name.as_ref(), env_snapshot))
            .map(|ask| PanelLive {
                ask,
                measured: (0.0, 0.0),
                announced: false,
                region: InputRegion::All,
            });
        let spec = match &panel {
            Some(p) => compose_panel_spec(p),
            None => self.window_spec(attrs, anchor_name.as_ref(), env_snapshot, None),
        };
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
        let site = self.site_of(node);
        let surfaces = self.surfaces.get_or_insert_with(Box::default);
        surfaces.next_id += 1;
        surfaces.had_any = true;
        let id = SurfaceId::new(surfaces.next_id);
        // A panel is announced once it has been measured (`settle_panel`).
        if panel.is_none() {
            surfaces.changes.push(SurfaceChange::Create {
                id,
                spec: spec.clone(),
            });
        }
        let opening = spec.size;
        surfaces.live.push(Live {
            id,
            uid: *uid,
            spec,
            size: opening,
            win,
            site,
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
            panel,
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

/// Whether a surface node is a `Panel`.
fn node_is_panel(node: &RenderNode) -> bool {
    matches!(node, RenderNode::Box { name, .. } if name.as_str() == PANEL)
}

/// What a panel is made of, from what it asks and what its content measured.
fn compose_panel_spec(p: &PanelLive) -> SurfaceSpec {
    let mut spec = SurfaceSpec::panel(p.ask.layer.namespace.clone());
    spec.name.clone_from(&p.ask.name);
    spec.size = p.requested();
    spec.layer = Some(p.ask.layer.clone());
    spec
}

/// The edges a keyword or a tuple of keywords names.
fn edges_of(expr: &Expr) -> Anchors {
    fn one(word: &str) -> Anchors {
        match word {
            "top" => Anchors::TOP,
            "bottom" => Anchors::BOTTOM,
            "left" => Anchors::LEFT,
            "right" => Anchors::RIGHT,
            _ => Anchors::NONE,
        }
    }
    match expr {
        Expr::Tuple(args, _) => args
            .iter()
            .fold(Anchors::NONE, |acc, a| acc | edges_of(&a.value)),
        e => Interpreter::enum_token(e).map_or(Anchors::NONE, one),
    }
}

impl Interpreter {
    // ── panels ──────────────────────────────────────────────────────────

    /// Whether `name` is the built-in `name` and nothing the program binds.
    pub(super) fn is_builtin_name(&self, name: &Symbol, payload_name: Option<&Symbol>) -> bool {
        payload_name != Some(name)
            && !self.lambda_params.contains(name)
            && self.env.lookup(name).is_none()
            && !self.consts.contains_key(name)
    }

    /// The signal behind `screens`, made on the first read (RFC-0068 §4).
    pub(super) fn screens_signal(&mut self) -> SignalId {
        if let Some(sig) = self.screens_sig {
            return sig;
        }
        let sig = self.ctx.create_signal(screens_value(&self.screens));
        self.screens_sig = Some(sig);
        sig
    }

    /// Tells the program which screens are connected (RFC-0068 §4). The host
    /// calls it when an output is added, removed or changes; the list is read in
    /// the order given (see [`ScreenInfo::sort`]). Nothing happens, and nothing
    /// is allocated, for a program that never reads `screens`.
    pub fn set_screens(&mut self, screens: Vec<ScreenInfo>) {
        if screens == self.screens {
            return;
        }
        self.screens = screens;
        if let Some(sig) = self.screens_sig {
            let value = screens_value(&self.screens);
            if self.ctx.peek_signal(sig) != value {
                self.ctx.write_signal(sig, value);
            }
        }
    }

    /// Whether a program has read `screens`, which is what makes its signal.
    #[must_use]
    pub fn has_screens_signal(&self) -> bool {
        self.screens_sig.is_some()
    }

    /// The screens the host last said.
    #[must_use]
    pub fn screens(&self) -> &[ScreenInfo] {
        &self.screens
    }

    /// The namespace of a `Panel` that writes none: the program's app id.
    pub fn set_default_namespace(&mut self, namespace: &str) {
        self.default_namespace = namespace.to_string();
    }

    /// What a panel's attributes ask for, read now.
    fn panel_ask(
        &mut self,
        attrs: &[Attr],
        name: Option<&Symbol>,
        env_snapshot: &[(Symbol, Value)],
    ) -> PanelAsk {
        let env_base = self.env.len();
        for (k, v) in env_snapshot {
            self.env.push(k.clone(), v.clone());
        }
        let find = |n: &str| {
            attrs.iter().find_map(|a| match &a.kind {
                AttrKind::Prop { value } if a.name.as_str() == n => Some(value.clone()),
                _ => None,
            })
        };
        // A keyword, or the keyword a conditional of keywords picks.
        let word = |this: &mut Self, n: &str| -> Option<String> {
            let value = find(n)?;
            let leaf = this.pick_gradient_leaf(&value).clone();
            Self::enum_token(&leaf).map(str::to_string)
        };
        let edge = word(self, "edge");
        let mut anchors = match edge.as_deref() {
            Some("top") => Anchors::TOP | Anchors::LEFT | Anchors::RIGHT,
            Some("bottom") => Anchors::BOTTOM | Anchors::LEFT | Anchors::RIGHT,
            Some("left") => Anchors::LEFT | Anchors::TOP | Anchors::BOTTOM,
            Some("right") => Anchors::RIGHT | Anchors::TOP | Anchors::BOTTOM,
            _ => Anchors::NONE,
        };
        if edge.is_none() {
            if let Some(a) = find("anchor") {
                anchors = edges_of(&a);
            }
        }
        let namespace = self.eval_str_prop(attrs, "namespace").unwrap_or_else(|| {
            if self.default_namespace.is_empty() {
                "byard".to_string()
            } else {
                self.default_namespace.clone()
            }
        });
        let mut layer = LayerSpec::new(namespace.clone());
        layer.anchors = anchors;
        layer.layer = match word(self, "layer").as_deref() {
            Some("background") => Layer::Background,
            Some("bottom") => Layer::Bottom,
            Some("overlay") => Layer::Overlay,
            _ => Layer::Top,
        };
        layer.keyboard = match word(self, "keyboard").as_deref() {
            Some("on_demand") => KeyboardMode::OnDemand,
            Some("exclusive") => KeyboardMode::Exclusive,
            _ => KeyboardMode::None,
        };
        layer.input = match word(self, "input").as_deref() {
            Some("all") => InputMode::All,
            Some("none") => InputMode::None,
            _ => InputMode::Auto,
        };
        layer.exclusive = match find("exclusive") {
            None if edge.is_some() => Exclusive::Thickness,
            None => Exclusive::Off,
            Some(Expr::Ident(w, _)) if w.as_str() == "ignore" => Exclusive::Ignore,
            Some(value) => match self.eval_pure(&value) {
                Value::Bool(true) => Exclusive::Thickness,
                Value::Int(n) => Exclusive::Pixels(i32::try_from(n).unwrap_or(0)),
                #[allow(clippy::cast_possible_truncation)]
                Value::Float(f) => Exclusive::Pixels(f.round() as i32),
                _ => Exclusive::Off,
            },
        };
        layer.screen = find("screen").and_then(|v| match self.eval_pure(&v) {
            Value::Record(fields) => fields.iter().find_map(|(k, v)| match v {
                Value::Str(s) if k.as_str() == "name" => Some(s.clone()),
                _ => None,
            }),
            _ => None,
        });
        #[allow(clippy::cast_possible_truncation)]
        let px = |this: &mut Self, n: &str| this.eval_px_prop(attrs, n).map(|v| v.round() as i32);
        let mut margin = Margin::default();
        if let Some(m) = px(self, "m") {
            margin = Margin::all(m);
        }
        if let Some(v) = px(self, "mx") {
            (margin.left, margin.right) = (v, v);
        }
        if let Some(v) = px(self, "my") {
            (margin.top, margin.bottom) = (v, v);
        }
        if let Some(v) = px(self, "mt") {
            margin.top = v;
        }
        if let Some(v) = px(self, "mr") {
            margin.right = v;
        }
        if let Some(v) = px(self, "mb") {
            margin.bottom = v;
        }
        if let Some(v) = px(self, "ml") {
            margin.left = v;
        }
        layer.margin = margin;
        let size = (
            self.eval_px_prop(attrs, "width"),
            self.eval_px_prop(attrs, "height"),
        );
        self.env.truncate(env_base);
        PanelAsk {
            layer,
            size,
            name: name.map_or(namespace, |n| n.as_str().to_string()),
        }
    }

    /// Measures what a panel's content needs along the axes that take it, tells
    /// the host about the panel (`Create` once, `Configure` when what it asks
    /// moved) and gives it the size it asked for (RFC-0068 §3, "Size").
    ///
    /// The measure is one layout of the content with the measured axis
    /// unbounded, in an atlas of its own, so the surface's retained layout is
    /// left as it was. It runs when the surface would be drawn anyway, never on
    /// an idle tick.
    // Sizes are whole logical pixels here, compared as the values that were stored.
    #[allow(clippy::float_cmp)]
    fn settle_panel(
        &mut self,
        index: usize,
        children: &[RenderNode],
        pools: Pools<'_>,
        force: bool,
    ) {
        let Some(live) = self.surfaces.as_ref().map(|s| &s.live[index]) else {
            return;
        };
        let Some(panel) = live.panel.as_ref() else {
            return;
        };
        let (mw, mh) = panel.measures();
        let want = (mw || mh) && (!panel.announced || live.needs_drawing(&self.ctx, force));
        let known = {
            let (sw, sh) = panel.stretched();
            let axis = |stretched: bool, written: Option<f32>, host: f32| {
                if stretched {
                    (host > 0.0).then_some(host)
                } else {
                    written
                }
            };
            (
                axis(sw, panel.ask.size.0, live.size.0),
                axis(sh, panel.ask.size.1, live.size.1),
            )
        };
        let before = panel.measured;
        let measured = if want {
            let (w, h) = self.measure_content(children, pools, known);
            (
                if mw { w.ceil() } else { before.0 },
                if mh { h.ceil() } else { before.1 },
            )
        } else {
            before
        };
        let Some(s) = self.surfaces.as_mut() else {
            return;
        };
        let l = &mut s.live[index];
        let Some(p) = l.panel.as_mut() else {
            return;
        };
        p.measured = measured;
        let spec = compose_panel_spec(p);
        let (sw, sh) = p.stretched();
        // An axis the program sizes follows what it asks for; a stretched one is
        // the compositor's to say.
        if !sw && spec.size.0 != l.spec.size.0 {
            l.size.0 = spec.size.0;
        }
        if !sh && spec.size.1 != l.spec.size.1 {
            l.size.1 = spec.size.1;
        }
        if !p.announced {
            p.announced = true;
            s.changes.push(SurfaceChange::Create {
                id: l.id,
                spec: spec.clone(),
            });
            l.spec = spec;
        } else if spec != l.spec {
            s.changes.push(SurfaceChange::Configure {
                id: l.id,
                spec: spec.clone(),
            });
            l.spec = spec;
        }
    }

    /// The size `children` take when laid out with each axis that is `None`
    /// unbounded and each that is `Some` as long as that.
    fn measure_content(
        &mut self,
        children: &[RenderNode],
        pools: Pools<'_>,
        known: (Option<f32>, Option<f32>),
    ) -> (f32, f32) {
        use byard_core::frame::Viewport;
        let space = (known.0.unwrap_or(UNBOUNDED), known.1.unwrap_or(UNBOUNDED));
        let saved = self.viewport;
        self.viewport = space;
        let mut scratch = byard_core::atlas::layout::LayoutAtlas::default();
        std::mem::swap(&mut self.atlas, &mut scratch);
        let mut flat = Vec::new();
        let mut none = Vec::new();
        let root =
            self.build_layout_pass_sized(children, pools, space, known, &mut flat, &[], &mut none);
        let mut size = (0.0, 0.0);
        if let Some((Some(main), _)) = root {
            {
                let mut measurer = self.text_measurer.borrow_mut();
                let measurer = measurer.get_or_insert_with(byard_core::text::TextMeasurer::new);
                let _ = self
                    .atlas
                    .compute_with_text(Viewport::new(space.0, space.1), measurer);
            }
            if let Ok(Some(r)) = self.atlas.resolved_rect(main) {
                size = (r.width, r.height);
            }
        }
        std::mem::swap(&mut self.atlas, &mut scratch);
        self.viewport = saved;
        self.elem_uids.clear();
        size
    }

    /// The part of the bound panel that takes the pointer, after it was drawn
    /// (RFC-0068 §6). `None` for a window.
    fn panel_region(&self, index: usize) -> Option<InputRegion> {
        let mode = self
            .surfaces
            .as_ref()?
            .live
            .get(index)?
            .panel
            .as_ref()?
            .ask
            .layer
            .input;
        Some(match mode {
            InputMode::All => InputRegion::All,
            InputMode::None => InputRegion::None,
            InputMode::Auto => {
                let mut rects: Vec<IRect> = self
                    .router
                    .pointer_rects()
                    .into_iter()
                    .chain(self.scroll_targets.iter().map(|t| t.rect))
                    .filter(|r| r.w > 0.0 && r.h > 0.0)
                    .map(|r| {
                        #[allow(clippy::cast_possible_truncation)]
                        let (x0, y0, x1, y1) = (
                            r.x.floor() as i32,
                            r.y.floor() as i32,
                            (r.x + r.w).ceil() as i32,
                            (r.y + r.h).ceil() as i32,
                        );
                        IRect::new(x0, y0, x1 - x0, y1 - y0)
                    })
                    .collect();
                rects.sort_by_key(|r| (r.x, r.y, r.width, r.height));
                rects.dedup();
                InputRegion::Rects(rects)
            }
        })
    }
}

/// `screens` as the list of records a program reads.
fn screens_value(screens: &[ScreenInfo]) -> Value {
    let float = |v: f32| Value::Float(f64::from(v));
    Value::List(
        screens
            .iter()
            .map(|s| {
                Value::Record(vec![
                    (Symbol::intern("name"), Value::Str(s.name.clone())),
                    (
                        Symbol::intern("description"),
                        Value::Str(s.description.clone()),
                    ),
                    (Symbol::intern("x"), float(s.x)),
                    (Symbol::intern("y"), float(s.y)),
                    (Symbol::intern("width"), float(s.width)),
                    (Symbol::intern("height"), float(s.height)),
                    (Symbol::intern("scale"), float(s.scale)),
                ])
            })
            .collect(),
    )
}
