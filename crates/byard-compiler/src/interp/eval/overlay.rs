//! Overlays and anchored placement (RFC-0017, RFC-0036).
//!
//! An `Overlay` leaves its parent's layout and is built, laid out and emitted
//! after the main tree, above it, in declaration order. This is that second
//! pass: finding the mounted overlays, giving each its own layout, placing an
//! anchored one against the element it names (and flipping it to the other
//! side when it would leave the viewport), sizing it to its anchor, and
//! registering the scrim tap and `Escape` that dismiss it. The `anchor_to:`
//! names are checked here too, when the view is lowered.

use super::layout::Pools;
use super::{Interpreter, RenderNode, access};
use crate::diagnostics::{CompileError, Span};
use crate::parser::ast::{Attr, AttrKind, ElementNode, Expr, Member, ViewDecl};

/// One `Overlay`'s built layout (RFC-0017): its absolute wrapper node plus a
/// per-child emission slot. Holds borrows into the frozen render tree, so its
/// lifetime is scoped to a single [`Interpreter::render`] call.
pub(super) struct OverlayLayout<'a> {
    /// The `RenderNode::Overlay` this describes (source of `attrs`/`children`).
    node: &'a RenderNode,
    /// The absolute wrapper container in the atlas; its node index doubles as
    /// the modal scrim's element id.
    pub(super) wrapper_id: byard_core::atlas::layout::AtlasNodeId,
    /// One slot per built child, in declaration order.
    children: Vec<OverlayChildSlot<'a>>,
}

/// A single overlay child ready to emit (RFC-0017): the child render node, its
/// atlas id, and the flat-id list its render walk consumes.
struct OverlayChildSlot<'a> {
    node: &'a RenderNode,
    id: byard_core::atlas::layout::AtlasNodeId,
    flat_ids: Vec<byard_core::atlas::layout::AtlasNodeId>,
}

/// The absolute, inset-0 anchor wrapper style for an overlay child (RFC-0017
/// §Positioning). Direction is `Column`, so `justify` drives the vertical edge
/// and `align` the horizontal one. An unanchored child keeps the default
/// (`Start`/`Stretch`), so a `grow` scrim fills the viewport; an anchored child
/// is pinned to the requested edge/centre.
/// Where an anchored overlay's box goes, given its anchor's rect (RFC-0036).
///
/// Pure, and separated from the render walk on purpose: placement is the part
/// with the arithmetic and the flip rule, and it is the part worth testing
/// without a GPU, a frame, or a layout tree.
///
/// `edge` picks the side of `anchor` to sit on, `align` lines the overlay up
/// along the other axis, and `gap` is the space between the two boxes.
#[must_use]
fn anchor_placement(
    anchor: crate::interp::intrinsics::Rect,
    size: (f32, f32),
    edge: &str,
    align: &str,
    gap: f32,
) -> (f32, f32) {
    let (w, h) = size;
    // Along the anchor's cross axis: `start` lines the near edges up, `end`
    // the far ones, `center` the midpoints.
    let across = |anchor_start: f32, anchor_len: f32, own_len: f32| match align {
        "end" => anchor_start + anchor_len - own_len,
        "center" => anchor_start + (anchor_len - own_len) / 2.0,
        _ => anchor_start,
    };
    match edge {
        "above" => (across(anchor.x, anchor.w, w), anchor.y - gap - h),
        "before" => (anchor.x - gap - w, across(anchor.y, anchor.h, h)),
        "after" => (anchor.x + anchor.w + gap, across(anchor.y, anchor.h, h)),
        // `below` is the default, and the one an autocomplete wants.
        _ => (across(anchor.x, anchor.w, w), anchor.y + anchor.h + gap),
    }
}

/// The opposite side, for the flip (RFC-0036 §"Flip logic").
#[must_use]
fn flipped_edge(edge: &str) -> &'static str {
    match edge {
        "above" => "below",
        "before" => "after",
        "after" => "before",
        _ => "above",
    }
}

/// Places an anchored overlay, flipping to the opposite edge when the first
/// choice would leave the viewport (RFC-0036).
///
/// Default-on, because an autocomplete that renders off the bottom of the
/// window is a bug in almost every case; `anchor_flip: false` opts out.
///
/// A flip happens at most once. If both sides overflow — a viewport too small
/// to hold the overlay either way — the side with more room wins and the box is
/// clamped into view rather than rendered where nobody can reach it.
#[must_use]
fn anchor_placement_flipped(
    anchor: crate::interp::intrinsics::Rect,
    size: (f32, f32),
    edge: &str,
    align: &str,
    gap: f32,
    flip: bool,
    viewport: (f32, f32),
) -> (f32, f32) {
    let (w, h) = size;
    let (vw, vh) = viewport;
    let fits = |(x, y): (f32, f32)| x >= 0.0 && y >= 0.0 && x + w <= vw && y + h <= vh;

    let first = anchor_placement(anchor, size, edge, align, gap);
    if !flip || fits(first) {
        return clamp_into(first, size, viewport);
    }
    let other = anchor_placement(anchor, size, flipped_edge(edge), align, gap);
    if fits(other) {
        return other;
    }
    // Neither fits. Keep whichever leaves more of the overlay on screen, then
    // clamp; a half-visible dropdown over its own field is the documented
    // last resort and beats one that is entirely off-screen.
    let visible = |(x, y): (f32, f32)| {
        let vx = (x + w).min(vw) - x.max(0.0);
        let vy = (y + h).min(vh) - y.max(0.0);
        vx.max(0.0) * vy.max(0.0)
    };
    let best = if visible(other) > visible(first) {
        other
    } else {
        first
    };
    clamp_into(best, size, viewport)
}

/// Slides a box back inside the viewport without resizing it.
#[must_use]
fn clamp_into(pos: (f32, f32), size: (f32, f32), viewport: (f32, f32)) -> (f32, f32) {
    let (w, h) = size;
    let (vw, vh) = viewport;
    // `max(0.0)` last, so a box larger than the viewport pins to the top-left
    // rather than to a negative coordinate.
    (pos.0.min(vw - w).max(0.0), pos.1.min(vh - h).max(0.0))
}

/// The nearest declared anchor name to `name`, for the diagnostic's hint.
fn closest_anchor(name: &str, declared: &[String]) -> Option<String> {
    declared
        .iter()
        .filter(|d| {
            // Cheap and good enough for a hint: same first letter and a
            // similar length is what a typo usually looks like.
            d.chars().next() == name.chars().next() && d.len().abs_diff(name.len()) <= 2
        })
        .min_by_key(|d| d.len().abs_diff(name.len()))
        .cloned()
}

fn anchor_wrapper_style(anchor: Option<&str>) -> byard_core::atlas::layout::ContainerStyle {
    use byard_core::atlas::layout::{Align, ContainerStyle, FlexDir, Justify};
    let mut style = ContainerStyle::default()
        .with_absolute(true)
        .with_direction(FlexDir::Column);
    let (justify, align) = match anchor {
        Some("center") => (Some(Justify::Center), Some(Align::Center)),
        Some("top") => (Some(Justify::Start), Some(Align::Center)),
        Some("bottom") => (Some(Justify::End), Some(Align::Center)),
        Some("start") => (Some(Justify::Center), Some(Align::Start)),
        // RFC-0036: an element-anchored child sizes to its content and is
        // moved into place afterwards, so it pins to the origin and stretches
        // on neither axis.
        Some("__anchored") => (Some(Justify::Start), Some(Align::Start)),
        Some("end") => (Some(Justify::Center), Some(Align::End)),
        // No anchor (a scrim): keep flow defaults so `grow` fills the viewport.
        _ => (None, None),
    };
    if let Some(j) = justify {
        style = style.with_justify(j);
    }
    if let Some(a) = align {
        style = style.with_align(a);
    }
    style
}

impl Interpreter {
    /// Collects every mounted `Overlay` in `nodes` in pre-order (RFC-0017 mount =
    /// declaration order), expanding reactive `when`/`for` (RFC-0018) so an
    /// overlay inside a live branch/body is found. Recurses through `Box` and an
    /// overlay's own children, so a nested overlay is collected as its own later
    ///, hence higher, stack entry.
    pub(super) fn collect_overlays<'a>(
        &self,
        nodes: &'a [RenderNode],
        pools: Pools<'a>,
        out: &mut Vec<&'a RenderNode>,
    ) {
        for concrete in self.expand_concrete(nodes, pools) {
            match concrete.node {
                RenderNode::Overlay { children, .. } => {
                    out.push(concrete.node);
                    self.collect_overlays(children, pools, out);
                }
                RenderNode::Box { children, .. } => {
                    self.collect_overlays(children, pools, out);
                }
                // RFC-0026 × RFC-0017: a modal opened by a live screen floats
                // over the whole app, exactly as one opened anywhere else.
                RenderNode::Nav { pool, .. } => {
                    if let Some(p) = pools.navs.get(*pool) {
                        for screen in &p.live {
                            self.collect_overlays(&p.entries[screen.entry].nodes, pools, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Builds one `Overlay`'s layout into the atlas (RFC-0017): each child is
    /// laid out at its natural size, then wrapped in an absolute, inset-0
    /// container whose `justify`/`align` realise the child's `anchor` within the
    /// viewport. All the anchor wrappers hang off one absolute overlay wrapper.
    /// Returns the wrapper id and per-child emission slots. `None` if `ov` is not
    /// an `Overlay` or the atlas rejects the nodes.
    pub(super) fn build_overlay_layout<'a>(
        &mut self,
        ov: &'a RenderNode,
        pools: Pools<'a>,
    ) -> Option<OverlayLayout<'a>> {
        let RenderNode::Overlay { children, .. } = ov else {
            return None;
        };
        // RFC-0018: an overlay's direct children may be reactive `when`/`for`;
        // expand them to concrete anchor targets before laying each out.
        let concrete = self.expand_concrete(children, pools);
        let mut anchor_ids = Vec::with_capacity(concrete.len());
        let mut slots = Vec::with_capacity(concrete.len());
        // An overlay is emitted away from where it was written, and the render
        // pass resets the slot to 0 for that reason; its *own* rows still have
        // one, so lay each out as itself.
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            let child = c.node;
            let mut cflat = Vec::new();
            let Ok(cid) = self.build_layout_tree(child, pools, &mut cflat) else {
                continue;
            };
            // RFC-0036: an anchored child is positioned by the overlay pass,
            // so its wrapper must not stretch it. It has to arrive at its own
            // content size, because that size is exactly what the placement
            // measures against — a panel stretched to the viewport would be
            // "placed" correctly and still cover the screen.
            let style = if self.is_pass_placed(child) {
                anchor_wrapper_style(Some("__anchored"))
            } else {
                anchor_wrapper_style(self.anchor_token(child).as_deref())
            };
            let Ok(anchor_id) = self.atlas.add_container(style, &[cid]) else {
                continue;
            };
            anchor_ids.push(anchor_id);
            slots.push(OverlayChildSlot {
                node: child,
                id: cid,
                flat_ids: cflat,
            });
        }
        self.anim_slot = enclosing;
        let wrapper_style =
            byard_core::atlas::layout::ContainerStyle::default().with_absolute(true);
        let wrapper_id = self.atlas.add_container(wrapper_style, &anchor_ids).ok()?;
        Some(OverlayLayout {
            node: ov,
            wrapper_id,
            children: slots,
        })
    }

    /// The `anchor_to:` name of an overlay child (RFC-0036), if it anchors to
    /// an element rather than to the viewport.
    fn anchor_ref(&mut self, child: &RenderNode) -> Option<String> {
        match child {
            RenderNode::Box { attrs, .. } => {
                let attrs = attrs.clone();
                self.eval_str_prop(&attrs, "anchor_to")
            }
            _ => None,
        }
    }

    /// Whether the overlay pass, rather than the viewport wrapper, decides
    /// where this child goes.
    ///
    /// True for an element-relative anchor (RFC-0036) and for an absolute
    /// `at:` (RFC-0017 §Positioning). Both are placed from a resolved rect, so
    /// both need the child to arrive at its own content size: a stretched
    /// panel would be "placed" correctly and still cover the screen.
    fn is_pass_placed(&mut self, child: &RenderNode) -> bool {
        if self.anchor_ref(child).is_some() {
            return true;
        }
        match child {
            RenderNode::Box { attrs, .. } => {
                let attrs = attrs.clone();
                attrs.iter().any(|a| a.name.as_str() == "at")
            }
            _ => false,
        }
    }

    /// The `anchor:` token of an overlay child (RFC-0017), or `None` for an
    /// unanchored child (a scrim, which fills the viewport via `grow`).
    fn anchor_token(&mut self, child: &RenderNode) -> Option<String> {
        match child {
            RenderNode::Box { attrs, .. } => Self::enum_prop(attrs, "anchor").map(str::to_string),
            _ => None,
        }
    }

    /// Emits one overlay's children on top of the main scene (RFC-0017 overlay
    /// phase). Clips them to the viewport, installs a modal scrim first when
    /// `modal` (the input barrier + `dismiss` target), then walks each child
    /// through the ordinary render path so it uses every existing pipeline.
    pub(super) fn emit_overlay(
        &mut self,
        ol: &OverlayLayout<'_>,
        frame: &mut byard_core::frame::RenderFrame,
        width: f32,
        height: f32,
        pools: Pools<'_>,
    ) {
        let RenderNode::Overlay {
            attrs,
            env_snapshot,
            ..
        } = ol.node
        else {
            return;
        };
        let viewport = crate::interp::intrinsics::Rect::new(0.0, 0.0, width, height);
        // `modal` defaults true (RFC-0017 §Modality); `dismiss_on_outside`
        // defaults to whatever `modal` is.
        let modal = self.eval_bool_prop(attrs, "modal").unwrap_or(true);
        let dismiss_on_outside = self
            .eval_bool_prop(attrs, "dismiss_on_outside")
            .unwrap_or(modal);

        // Clamp everything the overlay paints to the viewport (RFC-0017
        // resolved-question: scissor interaction).
        frame.begin_clip(byard_core::frame::Rect::new(0.0, 0.0, width, height));

        // RFC-0042: a modal overlay is a dialog, and whatever it holds is
        // inside it in the tree.
        let dialog = if self.access.on {
            let elem = self.atlas.node_index(ol.wrapper_id);
            self.access_enter_overlay(frame, attrs, modal, elem, viewport)
        } else {
            access::Scope::default()
        };

        // A modal overlay installs its scrim *before* its content so the content
        // wins hit-testing where it overlaps, while the scrim blocks (and
        // optionally dismisses) everything beneath the overlay.
        if modal {
            // Restore the overlay's instance environment so a `dismiss` action
            // referencing an instance `var`/param resolves correctly (RFC-0019).
            let env_base = self.env.len();
            for (k, v) in env_snapshot {
                self.env.push(k.clone(), v.clone());
            }
            let dismiss = if dismiss_on_outside {
                self.lower_overlay_dismiss(attrs)
            } else {
                None
            };
            self.env.truncate(env_base);
            let elem = self.atlas.node_index(ol.wrapper_id).unwrap_or(u32::MAX);
            self.router.push_modal_scrim(elem, viewport, dismiss);
        }

        for slot in &ol.children {
            // RFC-0036: an overlay child naming an anchor is moved from where
            // the viewport wrapper put it onto the anchor's rect. The shift
            // rides both channels — the paint transform and the hit-rect shift
            // — because a dropdown you can see but not click is not placed.
            let shift = self.anchor_shift(slot, width, height);
            let mut transform = byard_core::frame::Transform::IDENTITY;
            transform.translate[0] += shift.0;
            transform.translate[1] += shift.1;

            let mut flat_idx = 0;
            self.render_node_with_atlas(
                slot.node,
                slot.id,
                frame,
                &slot.flat_ids,
                &mut flat_idx,
                viewport,
                1.0,
                transform,
                None,
                shift,
                None,
                pools,
            );

            // RFC-0036 `on dismiss`: registered after the child has painted,
            // because the rects it keeps are the ones the child just landed
            // on.
            self.register_light_dismiss(slot, shift);
        }

        self.access_leave(frame, dialog);
        frame.end_clip();
    }

    /// Registers an anchored overlay child's `on dismiss` with the router
    /// (RFC-0036).
    ///
    /// **Not** the RFC-0017 scrim, which is a different feature that happens
    /// to share a word. A modal's scrim covers the viewport and swallows every
    /// event beneath it, which is right for a dialog and wrong for a dropdown:
    /// an autocomplete must not stop the page under it from scrolling. This
    /// registers an observer that blocks nothing.
    ///
    /// The anchor's own rect is kept alongside the panel's, and that is the
    /// half worth stating: without it, pressing the field that opened the
    /// panel dismisses it and the same press reopens it, which reads as a
    /// flicker with no cause.
    fn register_light_dismiss(&mut self, slot: &OverlayChildSlot<'_>, shift: (f32, f32)) {
        let RenderNode::Box { attrs, .. } = slot.node else {
            return;
        };
        // `on dismiss` is spelled as an ordinary event, so it arrives as one.
        let has_dismiss = attrs
            .iter()
            .any(|a| a.name.as_str() == "dismiss" && matches!(a.kind, AttrKind::Event { .. }));
        if !has_dismiss {
            return;
        }
        let Some(name) = self.eval_str_prop(attrs, "anchor_to") else {
            return;
        };
        let Ok(Some(own)) = self.atlas.resolved_rect(slot.id) else {
            return;
        };
        let mut keep = vec![crate::interp::intrinsics::Rect::new(
            own.x + shift.0,
            own.y + shift.1,
            own.width,
            own.height,
        )];
        if let Some(anchor) = self
            .anchor_rects
            .iter()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, r)| *r)
        {
            keep.push(anchor);
        }
        if let Some(action) = self.lower_overlay_dismiss(attrs) {
            self.router.push_light_dismiss(keep, action);
        }
    }

    /// Gives every overlay child that asked to `match` an anchor's width that
    /// width, and re-runs layout once if any of them changed (RFC-0036).
    ///
    /// Runs after the main tree has painted, which is when `anchor_rects`
    /// holds where each `as`-tagged element actually landed. The width cannot
    /// be a layout *input* on the first pass, because on that pass the anchor
    /// has no rect; and it cannot be applied by widening the finished rect
    /// either, because the panel's own children were laid out against the
    /// width it had, so its rows would sit in a box they no longer fill.
    ///
    /// Whether the second pass runs at all is decided by the widths, not by
    /// the presence of the property: on a steady frame every width is already
    /// what it should be and nothing is recomputed. That is what keeps a
    /// screen with a dropdown open from paying a full layout every frame.
    pub(super) fn apply_match_widths(
        &mut self,
        layouts: &[OverlayLayout<'_>],
        width: f32,
        height: f32,
    ) {
        let mut changed = false;
        for layout in layouts {
            for slot in &layout.children {
                let RenderNode::Box { attrs, .. } = slot.node else {
                    continue;
                };
                let Some((name, _)) = Self::match_width_ref(attrs) else {
                    continue;
                };
                // Unknown at render time is silence, exactly as a missing
                // anchor is: the compile check is what reports a misspelt
                // name, and a frame is the wrong place to raise it again.
                let Some(anchor) = self
                    .anchor_rects
                    .iter()
                    .find(|(k, _)| k.as_str() == name)
                    .map(|(_, r)| *r)
                else {
                    continue;
                };
                if self
                    .atlas
                    .set_fixed_width(slot.id, anchor.w)
                    .unwrap_or(false)
                {
                    changed = true;
                }
            }
        }
        if !changed {
            return;
        }
        let viewport = byard_core::frame::Viewport::new(width, height);
        let mut measurer = self.text_measurer.borrow_mut();
        let measurer = measurer.get_or_insert_with(byard_core::text::TextMeasurer::new);
        let _ = self.atlas.recompute_dirty_with_text(viewport, measurer);
    }

    /// The anchor name in `width: match(<name>)`, if the attributes carry one
    /// (RFC-0036).
    ///
    /// Recognised structurally rather than evaluated. `match(field)` parses as
    /// an ordinary call, and evaluating it would either invent a number or
    /// report an unknown function; neither is what the author asked for. It is
    /// a *layout relationship*, and the only place that can be resolved is
    /// after the anchor's rect is a fact.
    pub(super) fn match_width_ref(attrs: &[Attr]) -> Option<(String, Span)> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() != "width" {
                return None;
            }
            let AttrKind::Prop { value } = &a.kind else {
                return None;
            };
            let Expr::Call { callee, args, span } = value else {
                return None;
            };
            let Expr::Ident(name, _) = callee.as_ref() else {
                return None;
            };
            if name.as_str() != "match" {
                return None;
            }
            match args.as_slice() {
                [arg] => match &arg.value {
                    Expr::Ident(target, _) => Some((target.as_str().to_string(), *span)),
                    Expr::StrLit(parts, _) => match parts.as_slice() {
                        [crate::parser::ast::StrPart::Text(t)] => Some((t.clone(), *span)),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            }
        })
    }

    /// How far to move an overlay child so it sits against its anchor
    /// (RFC-0036), or `(0, 0)` for a child that names none.
    ///
    /// Returns zero rather than an error when the name is unknown at render
    /// time: the *compile* check is what reports a misspelt or forward
    /// reference, and a frame is the wrong place to raise it again.
    fn anchor_shift(&mut self, slot: &OverlayChildSlot<'_>, width: f32, height: f32) -> (f32, f32) {
        let RenderNode::Box { attrs, .. } = slot.node else {
            return (0.0, 0.0);
        };
        // RFC-0017 §Positioning: an absolute offset from the viewport's
        // top-left, unconditionally. It does not flip and it does not clamp: a
        // panel placed off-screen stays off-screen, because silently pulling
        // it back would make a wrong coordinate look like a layout decision.
        if attrs.iter().any(|a| a.name.as_str() == "at") {
            let (ax, ay) = self.resolve_axis_pair(attrs, "at", (0.0, 0.0));
            let Ok(Some(own)) = self.atlas.resolved_rect(slot.id) else {
                return (0.0, 0.0);
            };
            return (ax - own.x, ay - own.y);
        }
        let Some(name) = self.eval_str_prop(attrs, "anchor_to") else {
            return (0.0, 0.0);
        };
        // Compared by text rather than interned. `anchor_to` may be computed,
        // and the interner is process-global and append-only for the life of
        // the process, so interning a per-frame string would grow memory
        // without bound and take the write lock on the logic thread every time
        // a new one appeared. The table holds a handful of entries — one per
        // `as` tag in the view — so the scan is cheaper than the hash would be.
        let Some(anchor) = self
            .anchor_rects
            .iter()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, r)| *r)
        else {
            return (0.0, 0.0);
        };
        let Ok(Some(own)) = self.atlas.resolved_rect(slot.id) else {
            return (0.0, 0.0);
        };
        let edge = Self::enum_prop(attrs, "anchor_edge")
            .unwrap_or("below")
            .to_string();
        let align = Self::enum_prop(attrs, "anchor_align")
            .unwrap_or("start")
            .to_string();
        let gap = self.eval_px_prop(attrs, "anchor_gap").unwrap_or(0.0);
        let flip = self.eval_bool_prop(attrs, "anchor_flip") != Some(false);

        let (tx, ty) = anchor_placement_flipped(
            anchor,
            (own.width, own.height),
            &edge,
            &align,
            gap,
            flip,
            (width, height),
        );
        (tx - own.x, ty - own.y)
    }

    /// Lowers an `Overlay`'s `dismiss =>` action to a router [`Action`], if
    /// present (RFC-0017 §Dismissal). The action runs on scrim tap and on
    /// `Escape`.
    ///
    /// [`Action`]: super::events::Action
    fn lower_overlay_dismiss(&mut self, attrs: &[Attr]) -> Option<super::events::Action> {
        for attr in attrs {
            if attr.name.as_str() == "dismiss" {
                if let AttrKind::Event { payload, action } = &attr.kind {
                    return self.lower_action(action, payload.clone()).ok();
                }
            }
        }
        None
    }

    /// Checks every `anchor_to:` against the `as <name>` tags declared before
    /// it, in lexical order (RFC-0036).
    ///
    /// Lexical order is the whole guarantee: an overlay may only anchor to a
    /// name that appears earlier in the same view, which makes an anchor cycle
    /// impossible to write rather than something to detect at runtime, and
    /// means the anchor's rect is already a fact by the time the overlay pass
    /// places against it.
    pub(super) fn check_anchor_refs(&mut self, view: &ViewDecl) {
        let mut declared: Vec<String> = Vec::new();
        Self::walk_anchor_members(&view.body, &mut declared, &mut self.errors);
    }

    /// Walks members in written order, collecting `as` tags and checking
    /// `anchor_to:` against what has been collected so far.
    fn walk_anchor_members(
        members: &[Member],
        declared: &mut Vec<String>,
        errors: &mut Vec<CompileError>,
    ) {
        for member in members {
            match member {
                Member::Element(el) => Self::walk_anchor_element(el, declared, errors),
                Member::For { body, .. } | Member::Route { body, .. } => {
                    Self::walk_anchor_members(body, declared, errors);
                }
                Member::When { then, els, .. } => {
                    Self::walk_anchor_members(then, declared, errors);
                    if let Some(e) = els {
                        Self::walk_anchor_members(e, declared, errors);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk_anchor_element(
        el: &ElementNode,
        declared: &mut Vec<String>,
        errors: &mut Vec<CompileError>,
    ) {
        // RFC-0036 `width: match(ref)`: checked here rather than in its own
        // walk, because it asks the same two questions `anchor_to` does and
        // wants the same answers. A width matched against a name nobody
        // declared, or written on an element that anchors to nothing, is a
        // `width:` that quietly does nothing.
        // RFC-0017 §Positioning: `at:` is viewport-space and unconditional;
        // `anchor_to:` is element-relative and flips. Writing both is not a
        // precedence question to settle in the docs, it is two answers to one
        // question, so it is refused.
        if let Some(at) = el.attrs.iter().find(|a| a.name.as_str() == "at") {
            if el.attrs.iter().any(|a| a.name.as_str() == "anchor_to") {
                errors.push(CompileError::MisplacedAnchorTail {
                    span: at.span,
                    prop: "at:".to_string(),
                    reason: "cannot be combined with `anchor_to:`: one places the \
                             overlay in the viewport and the other against an \
                             element, and there is no sensible order for both"
                        .to_string(),
                    hint: None,
                });
            }
        }
        // RFC-0036: `dismiss =>` on a container is the light-dismiss of an
        // anchored overlay, so it needs an anchor for the same reason a
        // matched width does: the press it ignores is the one on the anchor,
        // and without one the panel would close the moment the user pressed
        // the thing that opened it.
        if el.name.as_str() != "Overlay" {
            let dismiss = el
                .attrs
                .iter()
                .find(|a| a.name.as_str() == "dismiss" && matches!(a.kind, AttrKind::Event { .. }));
            if let Some(attr) = dismiss {
                if !el.attrs.iter().any(|a| a.name.as_str() == "anchor_to") {
                    errors.push(CompileError::MisplacedAnchorTail {
                        span: attr.span,
                        prop: "dismiss =>".to_string(),
                        reason: "needs an `anchor_to:` on the same element: a light \
                                 dismiss ignores presses on the anchor, and an element \
                                 that anchors to nothing has none to ignore"
                            .to_string(),
                        hint: None,
                    });
                }
            }
        }
        if let Some((name, span)) = Self::match_width_ref(&el.attrs) {
            let anchors = el.attrs.iter().any(|a| a.name.as_str() == "anchor_to");
            if !anchors {
                errors.push(CompileError::MisplacedAnchorTail {
                    span,
                    prop: format!("width: match({name})"),
                    reason: "needs an `anchor_to:` on the same element: the width \
                             comes from the anchor's resolved rect, and an element \
                             that anchors to nothing has no rect to read"
                        .to_string(),
                    hint: None,
                });
            } else if !declared.contains(&name) {
                errors.push(CompileError::MisplacedAnchorTail {
                    span,
                    prop: format!("width: match({name})"),
                    reason: "names no element tagged `as` before this overlay".to_string(),
                    hint: closest_anchor(&name, declared),
                });
            }
        }
        for attr in &el.attrs {
            if attr.name.as_str() != "anchor_to" {
                continue;
            }
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            // Only a literal name can be checked. A computed one is allowed
            // through rather than rejected: the check exists to catch typos,
            // not to forbid the (rare, deliberate) dynamic case.
            let Expr::StrLit(parts, span) = value else {
                continue;
            };
            let [crate::parser::ast::StrPart::Text(name)] = parts.as_slice() else {
                continue;
            };
            if !declared.iter().any(|d| d == name) {
                errors.push(CompileError::UnknownAnchor {
                    span: *span,
                    name: name.clone(),
                    hint: closest_anchor(name, declared),
                });
            }
        }
        // Recorded *after* its own attrs are checked, so an element cannot
        // anchor to itself.
        if let Some(name) = &el.anchor_name {
            declared.push(name.as_str().to_string());
        }
        Self::walk_anchor_members(&el.children, declared, errors);
    }
}
