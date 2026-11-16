//! Popups: an anchored `Overlay` in a `Panel` is a surface of its own
//! (RFC-0068 §7).
//!
//! A bar is 40 pixels high, so a menu that hangs from it cannot be drawn inside
//! it. In a `Panel` (and in a popup, for a submenu), an `Overlay` whose first
//! child has `anchor_to` is not built into the surface it was written in: it is
//! a [`SurfaceKind::Popup`](byard_core::SurfaceKind::Popup) whose parent is
//! that surface, with the positioner data a compositor needs
//! ([`PopupSpec`]) and a size that is what its content measured, unbounded.
//!
//! Everything else about it is a surface like the others: its own layout,
//! hit-test data, focus and accessibility tree (a [`SurfaceState`]), drawn into
//! its own frame only when something it showed has changed. What is particular:
//!
//! - **It is created when its overlay mounts and destroyed after its `on leave`
//!   motion has settled.** The overlay stays in the tree while it leaves, so the
//!   popup stays open and is drawn inert until then.
//! - **It is placed against an element of its parent**, whose bounds the parent
//!   recorded the last time it was drawn. When the anchor moves or the content's
//!   size changes it is a `Configure`.
//! - **It may have the grab.** A popup that is a focus scope (RFC-0066: modal, or
//!   holding a `menu` or a `dialog`) takes the keyboard, and the compositor
//!   sends [`PopupDone`](byard_core::EventKind::PopupDone) when the user clicks
//!   outside it, which runs the overlay child's `dismiss`, as a press outside
//!   does in a window.
//!
//! An `Overlay` in a panel with no `anchor_to`, and every overlay in a `Window`,
//! are drawn inside their surface as they always were.
//!
//! [`SurfaceState`]: super::surfaces::SurfaceState

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use byard_core::frame::RenderFrame;
use byard_core::surface::popup::{Align, Side};
use byard_core::surface::{PopupSpec, Rect as IRect};
use byard_core::{SurfaceChange, SurfaceId, SurfaceSpec};

use super::animation::AnimSlot;
use super::layout::Pools;
use super::overlay::AnchorTarget;
use super::surfaces::{Live, Site};
use super::{Interpreter, RenderNode};
use crate::interp::events::Action;
use crate::interp::intrinsics::Rect;
use crate::interp::reactive::ReadSet;
use crate::parser::ast::{Attr, AttrKind};
use crate::symbol::Symbol;

/// The name of a popup whose content names none (`as`).
const DEFAULT_NAME: &str = "popup";

/// A popup that is open.
pub(super) struct PopupLive {
    /// The surface it hangs from.
    pub(super) parent: SurfaceId,
    /// Whether the host has been told it exists (`Create` sent).
    pub(super) announced: bool,
    /// Whether the overlay is on its way out: it stays open, inert, until its
    /// `on leave` has settled.
    pub(super) leaving: bool,
    /// The animation slot its overlay was expanded under.
    pub(super) slot: AnimSlot,
    /// `Some(is_menu)` when it is a focus scope.
    pub(super) scope: Option<bool>,
    /// What runs when the compositor dismisses it, or Escape does.
    pub(super) dismiss: Option<Rc<RefCell<Action>>>,
}

/// What a popup's frame needs to know while it is drawn: it is a scope, it
/// dismisses, it leaves.
#[derive(Clone)]
pub(super) struct PopupFrame {
    /// The overlay's identity, which is what a focus scope is from one frame to
    /// the next.
    pub(super) uid: u32,
    /// The slot its content is expanded under, the leaving group when it leaves.
    pub(super) slot: AnimSlot,
    pub(super) leaving: bool,
    pub(super) scope: Option<bool>,
    pub(super) dismiss: Option<Rc<RefCell<Action>>>,
}

/// What the interpreter knows of popups while it draws a surface.
#[derive(Default)]
pub(super) struct PopupCtx {
    /// Whether the surface being drawn is one an anchored `Overlay` becomes a
    /// popup of: a panel, or a popup. Such an overlay is not drawn inside the
    /// surface, it is a surface of its own.
    pub(super) host: bool,
    /// What the popup being drawn is, while its frame is.
    pub(super) frame: Option<PopupFrame>,
}

/// Where the elements a popup can anchor to landed the last time their surface
/// was drawn, in that surface's coordinates.
#[derive(Default)]
pub(super) struct HostAnchors {
    uids: HashMap<u32, Rect>,
    names: HashMap<Symbol, Rect>,
}

impl HostAnchors {
    /// What the frame that was just drawn recorded. Empty, and nothing is
    /// copied, for a surface that has no `as` name to anchor to.
    pub(super) fn of(it: &Interpreter) -> Self {
        if it.anchor_uid_rects.is_empty() && it.anchor_rects.is_empty() {
            return Self::default();
        }
        Self {
            uids: it.anchor_uid_rects.clone(),
            names: it.anchor_rects.clone(),
        }
    }

    fn rect(&self, target: &AnchorTarget) -> Option<Rect> {
        match target {
            AnchorTarget::Element(uid) => self.uids.get(uid).copied(),
            AnchorTarget::Name(name) => self
                .names
                .iter()
                .find(|(k, _)| k.as_str() == name)
                .map(|(_, r)| *r),
        }
    }
}

/// Whole pixels around `r`, at least one wide: a positioner wants an integer
/// anchor rectangle with a width.
#[allow(clippy::cast_possible_truncation)]
fn whole(r: Rect) -> IRect {
    let (x0, y0) = (r.x.floor() as i32, r.y.floor() as i32);
    let (x1, y1) = ((r.x + r.w).ceil() as i32, (r.y + r.h).ceil() as i32);
    IRect::new(x0, y0, (x1 - x0).max(1), (y1 - y0).max(0))
}

impl Interpreter {
    /// Whether the overlay holding `children` is a popup: its first child has
    /// `anchor_to` (RFC-0068 §7).
    pub(super) fn is_popup_overlay(
        &self,
        children: &[RenderNode],
        pools: Pools<'_>,
        slot: AnimSlot,
    ) -> bool {
        self.expand_concrete_in(children, pools, slot)
            .first()
            .is_some_and(|c| {
                matches!(c.node, RenderNode::Box { attrs, .. }
                    if attrs.iter().any(|a| a.name.as_str() == "anchor_to"))
            })
    }

    /// The popups mounted in `children`, in the surface they are drawn in: the
    /// overlays that are anchored, with the slot each was expanded under.
    fn find_popups<'a>(
        &mut self,
        children: &'a [RenderNode],
        pools: Pools<'a>,
        slot: AnimSlot,
    ) -> Vec<(&'a RenderNode, AnimSlot)> {
        let mut popups = Vec::new();
        let mut inside = Vec::new();
        self.popup.host = true;
        self.collect_overlays_in(children, pools, slot, &mut inside, Some(&mut popups));
        self.popup.host = false;
        popups
    }

    /// Opens, configures, draws and closes the popups of surface `host`, whose
    /// content is `children`, after the host itself was drawn (or not).
    ///
    /// `drew` says whether the host was drawn this tick, which is when popups can
    /// have come or gone; a host that was not drawn is only looked at while it
    /// has popups, whose content can change on its own.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn sync_popups(
        &mut self,
        host: usize,
        children: &[RenderNode],
        pools: Pools<'_>,
        slot: AnimSlot,
        drew: bool,
        force: bool,
        new_frame: &mut dyn FnMut() -> RenderFrame,
        out: &mut Vec<(SurfaceId, RenderFrame)>,
    ) {
        let Some(surfaces) = self.surfaces.as_ref() else {
            return;
        };
        let live = &surfaces.live[host];
        // Its anchors are known once it has been drawn, and a popup can only
        // hang from a surface the host has been told about.
        let announced = live.panel.as_ref().is_some_and(|p| p.announced)
            || live.popup.as_ref().is_some_and(|p| p.announced);
        if !announced || live.drawn_size.is_none() {
            return;
        }
        let host_id = live.id;
        let has_popups = surfaces
            .live
            .iter()
            .any(|l| l.popup.as_ref().is_some_and(|p| p.parent == host_id));
        if !drew && !has_popups {
            return;
        }
        let found = self.find_popups(children, pools, slot);
        let mut seen = Vec::with_capacity(found.len());
        for (ov, ov_slot) in found {
            let RenderNode::Overlay {
                uid,
                children: content,
                ..
            } = ov
            else {
                continue;
            };
            seen.push(*uid);
            let Some(index) = self.settle_popup(host_id, ov, ov_slot, pools, force) else {
                continue;
            };
            let frame = self.surfaces.as_ref().and_then(|s| {
                let p = s.live[index].popup.as_ref()?;
                Some(PopupFrame {
                    uid: *uid,
                    slot: ov_slot,
                    leaving: p.leaving,
                    scope: p.scope,
                    dismiss: p.dismiss.clone(),
                })
            });
            let drew = self.draw_surface(index, content, pools, force, frame, new_frame, out);
            self.sync_popups(index, content, pools, ov_slot, drew, force, new_frame, out);
        }
        // A popup whose overlay is not mounted any more (its `when` went false
        // and its `on leave`, if any, has settled) is closed.
        if let Some(s) = self.surfaces.as_mut() {
            let gone: Vec<SurfaceId> = s
                .live
                .iter()
                .filter(|l| {
                    l.popup.as_ref().is_some_and(|p| p.parent == host_id) && !seen.contains(&l.uid)
                })
                .map(|l| l.id)
                .collect();
            for id in gone {
                s.close_popups_of(id);
                s.close_popup(id);
            }
        }
    }

    /// Makes sure the popup of `ov` is open and says what it is now: measures
    /// its content, finds its anchor, and tells the host (`Create` once,
    /// `Configure` when its anchor or its size moved). Returns its place among
    /// the live surfaces, or `None` while its anchor has not been drawn.
    fn settle_popup(
        &mut self,
        host_id: SurfaceId,
        ov: &RenderNode,
        slot: AnimSlot,
        pools: Pools<'_>,
        force: bool,
    ) -> Option<usize> {
        let RenderNode::Overlay { children, uid, .. } = ov else {
            return None;
        };
        let host = self.surfaces.as_ref()?.index(host_id)?;
        let existing = self
            .surfaces
            .as_ref()?
            .live
            .iter()
            .position(|l| l.uid == *uid && l.popup.is_some());
        // The first child says where it goes.
        let first = self
            .expand_concrete_in(children, pools, slot)
            .into_iter()
            .next()?
            .node;
        let RenderNode::Box {
            attrs: child_attrs,
            env_snapshot: child_env,
            anchor_name,
            ..
        } = first
        else {
            return None;
        };
        // Nothing to be placed against, the anchor not drawn: there is no popup
        // yet; one that is open keeps the place it had.
        let previous = existing
            .and_then(|i| self.surfaces.as_ref()?.live[i].spec.popup)
            .map(|p| p.anchor_rect);
        let anchor = self.popup_anchor(host, first).or(previous)?;
        // How it is placed, read where the child was written.
        let base = self.env.len();
        for (k, v) in child_env {
            self.env.push(k.clone(), v.clone());
        }
        let side = Side::from_word(Self::enum_prop(child_attrs, "anchor_edge").unwrap_or("below"));
        let align =
            Align::from_word(Self::enum_prop(child_attrs, "anchor_align").unwrap_or("start"));
        #[allow(clippy::cast_possible_truncation)]
        let gap = self
            .eval_px_prop(child_attrs, "anchor_gap")
            .unwrap_or(0.0)
            .round() as i32;
        let flip = self.eval_bool_prop(child_attrs, "anchor_flip") != Some(false);
        self.env.truncate(base);

        let index = match existing {
            Some(i) => i,
            None => self.open_popup(host_id, ov, first, slot, pools)?,
        };
        let s = self.surfaces.as_mut()?;
        let l = &mut s.live[index];
        let (grab, was_announced) = {
            let p = l.popup.as_mut()?;
            // It starts to leave, or comes back before it was gone: what it
            // plays changes, though nothing it read did.
            if p.leaving != (slot.leave != 0) {
                l.structure_moved = true;
            }
            p.leaving = slot.leave != 0;
            p.slot = slot;
            (l.spec.popup.is_some_and(|p| p.grab), p.announced)
        };
        let want_size = !was_announced || l.needs_drawing(&self.ctx, force);

        let before = l.spec.size;
        let measured = if want_size {
            let saved = std::mem::replace(&mut self.anim_slot, slot);
            let (w, h) = self.measure_content(children, pools, (None, None));
            self.anim_slot = saved;
            (w.ceil().max(1.0), h.ceil().max(1.0))
        } else {
            before
        };
        let name = anchor_name
            .as_ref()
            .map_or(DEFAULT_NAME, |n| n.as_str())
            .to_string();
        let mut spec = SurfaceSpec::popup(
            name,
            PopupSpec::anchored(host_id, anchor, side, align, gap, flip, grab),
        );
        spec.size = measured;
        let s = self.surfaces.as_mut()?;
        let l = &mut s.live[index];
        l.size = measured;
        let announce = !l.popup.as_ref()?.announced;
        if announce {
            if let Some(p) = l.popup.as_mut() {
                p.announced = true;
            }
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
        Some(index)
    }

    /// Where `child`'s `anchor_to` landed in surface `host`, as the last drawing
    /// of that surface recorded it.
    fn popup_anchor(&mut self, host: usize, child: &RenderNode) -> Option<IRect> {
        let target = self.anchor_target(child)?;
        let live = &self.surfaces.as_ref()?.live[host];
        live.anchors.rect(&target).map(whole)
    }

    /// Opens the surface of an overlay that just mounted as a popup, not yet
    /// announced: whether it has the grab, what dismisses it.
    fn open_popup(
        &mut self,
        host_id: SurfaceId,
        ov: &RenderNode,
        first: &RenderNode,
        slot: AnimSlot,
        pools: Pools<'_>,
    ) -> Option<usize> {
        let RenderNode::Overlay {
            attrs,
            children,
            env_snapshot,
            uid,
            ..
        } = ov
        else {
            return None;
        };
        let RenderNode::Box {
            attrs: child_attrs,
            env_snapshot: child_env,
            ..
        } = first
        else {
            return None;
        };
        // RFC-0066 §1: modal, or holding a menu or a dialog, is a focus scope,
        // and a scope has the grab.
        let base = self.env.len();
        for (k, v) in env_snapshot {
            self.env.push(k.clone(), v.clone());
        }
        let modal = self.eval_bool_prop(attrs, "modal").unwrap_or(true);
        self.env.truncate(base);
        let scope = self.overlay_scope(modal, children, pools);
        // The child's `dismiss` is the light dismiss of an anchored overlay; the
        // overlay's own is what a modal's scrim runs.
        let dismiss = self
            .lower_event(child_attrs, child_env, "dismiss")
            .or_else(|| self.lower_event(attrs, env_snapshot, "dismiss"));
        let surfaces = self.surfaces.as_mut()?;
        surfaces.next_id += 1;
        let id = SurfaceId::new(surfaces.next_id);
        let mut spec = SurfaceSpec::popup(
            DEFAULT_NAME,
            PopupSpec::anchored(
                host_id,
                IRect::new(0, 0, 1, 0),
                Side::Below,
                Align::Start,
                0,
                true,
                scope.is_some(),
            ),
        );
        spec.size = (1.0, 1.0);
        surfaces.live.push(Live {
            id,
            uid: *uid,
            spec,
            size: (0.0, 0.0),
            win: None,
            site: Site::default(),
            close: None,
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
            panel: None,
            popup: Some(PopupLive {
                parent: host_id,
                announced: false,
                leaving: slot.leave != 0,
                slot,
                scope,
                dismiss,
            }),
            anchors: HostAnchors::default(),
        });
        Some(surfaces.live.len() - 1)
    }

    /// Lowers the event `name` among `attrs`, once, in the names it was
    /// written among, as a surface keeps it.
    pub(super) fn lower_event(
        &mut self,
        attrs: &[Attr],
        env_snapshot: &[(Symbol, crate::interp::env::Value)],
        name: &str,
    ) -> Option<Rc<RefCell<Action>>> {
        let attr = attrs.iter().find(|a| a.name.as_str() == name)?;
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

    /// What a popup's frame registers before anything is drawn: a leaving one
    /// takes no input, one that is a scope opens it, and Escape dismisses it.
    pub(super) fn begin_popup_frame(&mut self, width: f32, height: f32) {
        let Some(p) = self.popup.frame.as_ref() else {
            return;
        };
        if p.leaving {
            self.router.set_inert(true);
            return;
        }
        if let Some(menu) = p.scope {
            self.router.begin_scope(p.uid, menu);
        }
        if let Some(dismiss) = p.dismiss.clone() {
            // A press cannot land outside the surface, so only Escape ever
            // reaches this: a popup is dismissed by the compositor on a click
            // outside it.
            let keep = crate::interp::intrinsics::Rect::new(0.0, 0.0, width, height);
            self.router.push_light_dismiss(
                vec![keep],
                Box::new(move |ctx, payload| (dismiss.borrow_mut())(ctx, payload)),
            );
        }
    }

    /// Closes the scope a popup frame opened, once everything it holds is drawn.
    pub(super) fn end_popup_frame(&mut self) {
        if self
            .popup
            .frame
            .as_ref()
            .is_some_and(|p| p.scope.is_some() && !p.leaving)
        {
            self.router.end_scope();
        }
    }

    /// The compositor dismissed popup `id`: its overlay runs its `dismiss`
    /// (RFC-0068 §7).
    pub(super) fn popup_done(&mut self, id: SurfaceId) {
        let dismiss = self
            .surfaces
            .as_ref()
            .and_then(|s| s.live.iter().find(|l| l.id == id))
            .and_then(|l| l.popup.as_ref())
            .and_then(|p| p.dismiss.clone());
        if let Some(dismiss) = dismiss {
            (dismiss.borrow_mut())(&mut self.ctx, None);
        }
    }

    /// The popup that has the grab, if any: the innermost popup that is a focus
    /// scope and is not leaving. It has the keyboard, and a click outside it is
    /// the compositor's `popup_done` (RFC-0068 §7).
    #[must_use]
    pub fn grab_surface(&self) -> Option<SurfaceId> {
        self.surfaces
            .as_ref()?
            .live
            .iter()
            .rev()
            .find(|l| {
                l.popup.as_ref().is_some_and(|p| p.announced && !p.leaving)
                    && l.spec.popup.is_some_and(|p| p.grab)
            })
            .map(|l| l.id)
    }
}
