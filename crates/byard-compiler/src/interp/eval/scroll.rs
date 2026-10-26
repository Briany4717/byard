//! Scrolling: `ScrollView` offsets, snapping, paging and pull-to-refresh
//! (RFC-0005, RFC-0021).
//!
//! A scroll position is a `var` the app owns or an offset the interpreter
//! keeps for it. This is reading and writing that offset, the visible window
//! of a windowed list and the culling of rows outside it, the drag and wheel
//! bookkeeping, the snap points a release settles to (and the spring that
//! carries it there), paging, the end-of-list event, and the elastic
//! pull-to-refresh indicator.

use super::Interpreter;
use super::animation::pack_curve;
use crate::interp::env::{SignalId, Value};
use crate::parser::ast::{Attr, AttrKind};

/// RFC-0021 pull-to-refresh geometry, in logical px. The pull region resists with
/// a diminishing-returns curve that asymptotes to [`PULL_MAX`]; releasing past
/// [`PULL_THRESHOLD`] triggers a refresh and rests the indicator at [`PULL_REST`].
const PULL_MAX: f32 = 120.0;

pub(super) const PULL_THRESHOLD: f32 = 56.0;

const PULL_REST: f32 = 44.0;

/// One scrollable axis of a [`ScrollTarget`]: the `var` behind `offset.x` or
/// `offset.y` and how far it may travel (content extent − viewport, ≥ 0).
#[derive(Clone, Copy)]
pub(super) struct ScrollAxis {
    /// The signal backing this axis's offset component; the wheel/drag writes it.
    pub(super) sig: SignalId,
    /// Maximum scroll distance on this axis (content − viewport), clamped ≥ 0.
    pub(super) max: f32,
}

/// RFC-0021 `snap` mode: where a scroll settles after a fling/scroll goes quiet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapMode {
    /// Free scrolling (`snap: none`, the default), never snaps.
    None,
    /// `snap: page`, settle to the nearest viewport-sized page.
    Page,
    /// `snap: item`, settle to the nearest direct-child boundary (the child
    /// offsets are precomputed into [`Interpreter::scroll_item_bounds`]).
    Item,
}

/// RFC-0021 `snap_align`: where the snapped item aligns within the viewport.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapAlign {
    /// Item's leading edge at the viewport's leading edge (default).
    Start,
    /// Item centred in the viewport.
    Center,
    /// Item's trailing edge at the viewport's trailing edge.
    End,
}

/// A wheel/drag-scrollable region recorded during render (RFC-0005
/// `ScrollView`). `dispatch_events` turns a wheel or a drag over `rect` into a
/// clamped write to whichever of `offset.x`/`offset.y` is a writable `var`.
#[derive(Clone, Copy)]
pub(super) struct ScrollTarget {
    /// Viewport rect in logical screen px (the wheel/drag hit region).
    pub(super) rect: crate::interp::intrinsics::Rect,
    /// Horizontal axis, present when `offset.x` is a writable `var`.
    pub(super) x: Option<ScrollAxis>,
    /// Vertical axis, present when `offset.y` is a writable `var`.
    pub(super) y: Option<ScrollAxis>,
    /// The `ScrollView` element index, for firing engine scroll events
    /// (`end_reached`/`page_change`/`scroll_end`), RFC-0021.
    pub(super) elem: Option<u32>,
    /// RFC-0021 `snap` mode (`none`/`page`/`item`). For `item`, the
    /// `snap_align`-adjusted child boundaries live in
    /// [`Interpreter::scroll_item_bounds`], keyed by [`elem`](Self::elem).
    pub(super) snap: SnapMode,
    /// RFC-0021 `snap_spring` override, the packed curve driving the snap glide,
    /// or `None` for the engine default spring.
    pub(super) snap_spring: Option<byard_core::frame::MotionCurve>,
    /// RFC-0021 reflected `page:` var, written the current page index on a
    /// page-snap settle; a `page_change` fires when it changes.
    pub(super) page_sig: Option<SignalId>,
    /// RFC-0021 `end_threshold` (0..1): the fraction of the scrollable extent at
    /// which `end_reached` fires. `None` when no `end_reached` handler exists.
    pub(super) end_threshold: Option<f32>,
    /// RFC-0021 `pull_refresh`: overscrolling past the top drags an elastic pull
    /// region (see [`Interpreter::pull_distance`]); releasing past the threshold
    /// fires `refresh`. A pull view needs no `offset` var, the pull region is
    /// engine state, not the scroll offset.
    pub(super) pull_refresh: bool,
    /// RFC-0021 reflected `refreshing:` var, set `true` by the engine when a pull
    /// triggers a refresh; the app sets it back to `false` when its load finishes,
    /// which springs the indicator away.
    pub(super) refreshing_sig: Option<SignalId>,
}

/// An in-flight smooth snap (RFC-0021 §2): a spring driving one `ScrollView`
/// axis's offset signal to an exact page boundary. Seeded when scrolling settles
/// (drag release or scroll-quiet), advanced each `render` from the shared engine
/// clock, and dropped once [`Motion::is_settled_with_eps`] reports it has
/// arrived, at which point the offset is pinned to `target` and `scroll_end`
/// fires. Cancelled outright by any fresh scroll/drag on the same elem.
#[derive(Clone, Copy)]
pub(super) struct SnapAnim {
    /// The offset-axis signal the spring writes each frame.
    sig: SignalId,
    /// The spring itself (`from` = offset at settle, `to` = `target`).
    motion: byard_core::frame::Motion,
    /// The exact page boundary to pin the offset to on settle.
    target: f32,
}

/// The visible slice of a windowed `ScrollView` list (RFC-0005 windowed layout):
/// only rows `start..end` of a uniform-height list are built, laid out, and
/// emitted, with a leading/trailing [`Spacer`] standing in for the elided rows so
/// the content extent (and thus the scroll clamp) and every visible row's
/// position stay exact. Computed identically in the build and render passes from
/// the same offset, so the parallel flat-id cursor stays aligned.
#[derive(Clone, Copy)]
pub(super) struct WindowSpec {
    /// Index of the first materialised row.
    pub(super) start: usize,
    /// One past the last materialised row (`start..end` is the live slice).
    pub(super) end: usize,
    /// Fixed per-row extent in logical px (spacing folded in).
    pub(super) row_height: f32,
    /// Total row count, so the trailing spacer covers `n − end` rows.
    pub(super) n: usize,
}

/// Shifts a hit rect by the accumulated scroll displacement and clips it to
/// the enclosing scroll viewport (RFC-0005): interaction follows the content
/// to its *on-screen* position, and content scrolled out of the viewport
/// stops being hittable, it is not visible, so it must not be tappable.
pub(super) fn scrolled_hit_rect(
    r: crate::interp::intrinsics::Rect,
    shift: (f32, f32),
    viewport: Option<byard_core::frame::Rect>,
) -> crate::interp::intrinsics::Rect {
    let shifted = crate::interp::intrinsics::Rect::new(r.x + shift.0, r.y + shift.1, r.w, r.h);
    let Some(v) = viewport else {
        return shifted;
    };
    let x0 = shifted.x.max(v.x);
    let y0 = shifted.y.max(v.y);
    let x1 = (shifted.x + shifted.w).min(v.x + v.width);
    let y1 = (shifted.y + shifted.h).min(v.y + v.height);
    crate::interp::intrinsics::Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// One axis of a live [`ScrollDrag`]: the signal to write and its value at the
/// press, so the live offset is a pure function of the pointer travel.
#[derive(Clone, Copy)]
pub(super) struct ScrollDragAxis {
    /// The signal backing this axis's offset component, written as it moves.
    pub(super) sig: SignalId,
    /// The offset at the press; the live offset is this minus the pointer travel.
    start_offset: f32,
    /// Maximum scroll distance on this axis (content − viewport), clamped ≥ 0.
    pub(super) max: f32,
    /// Whether `sig` holds an `Int` (write back rounded) or a `Float`.
    is_int: bool,
}

/// A live drag-to-scroll gesture (RFC-0005 `ScrollView`): the content follows
/// the pointer between press and release. Captured at press so the offset is a
/// pure function of the pointer travel, no accumulated drift.
#[derive(Clone, Copy)]
pub(super) struct ScrollDrag {
    /// Pointer position at the press, in logical screen px.
    pub(super) start_pos: (f32, f32),
    /// Horizontal axis, present when `offset.x` is a writable `var`.
    pub(super) x: Option<ScrollDragAxis>,
    /// Vertical axis, present when `offset.y` is a writable `var`.
    pub(super) y: Option<ScrollDragAxis>,
    /// The dragged `ScrollView` element (for RFC-0021 snap-settle activity).
    pub(super) elem: Option<u32>,
    /// RFC-0021 `pull_refresh`: whether the dragged view enables pull-to-refresh,
    /// so a downward over-drag at the top grows the pull region.
    pub(super) pull_refresh: bool,
    /// RFC-0021 reflected `refreshing:` var of the dragged view, set `true` when a
    /// release past the threshold triggers a refresh.
    pub(super) refreshing_sig: Option<SignalId>,
}

/// An in-flight pull-region spring (RFC-0021 pull-to-refresh): drives a
/// `ScrollView`'s [`pull_distance`](Interpreter::pull_distance) to `target`,
/// the indicator's rest height while refreshing, or `0` to retract it, advanced
/// each `render` from the engine clock like a [`SnapAnim`].
#[derive(Clone, Copy)]
pub(super) struct PullAnim {
    /// The spring (`from` = pull at release, `to` = `target`).
    motion: byard_core::frame::Motion,
    /// The exact pull distance to pin on settle (`0` retracts the indicator).
    target: f32,
}

impl Interpreter {
    /// The signals backing a `ScrollView`'s `offset.x` and `offset.y` (RFC-0005),
    /// each present when that tuple component is a writable `var`, e.g.
    /// `offset: (panX, scrollY)` yields both, `offset: (0, scrollY)` only the y.
    /// A component that is a literal or computed value yields `None` (that axis
    /// is inert to wheel/drag; the app drives it). Returned as `(x, y)`.
    pub(super) fn resolve_offset_sigs(
        &self,
        attrs: &[Attr],
    ) -> (Option<super::env::SignalId>, Option<super::env::SignalId>) {
        use crate::parser::ast::Expr;
        let Some(value) = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "offset" => Some(value),
            _ => None,
        }) else {
            return (None, None);
        };
        // `offset: (x, y)`, a component is scrollable iff it names a `var`.
        let sig_at = |i: usize| -> Option<super::env::SignalId> {
            let Expr::Tuple(args, _) = value else {
                return None;
            };
            let Some(Expr::Ident(name, _)) = args.get(i).map(|a| &a.value) else {
                return None;
            };
            match self.env.lookup(name) {
                Some(super::env::Value::Signal(sig)) => Some(*sig),
                _ => None,
            }
        };
        (sig_at(0), sig_at(1))
    }

    /// The visible row window of a windowed `ScrollView` (RFC-0005), or `None`
    /// when it is not `windowed`, its `row_height` is unset/≤ 0, or it has no
    /// uniform list child. The window brackets the viewport with a couple of
    /// overscan rows so a partially-scrolled row is always materialised. Computed
    /// from the *current* `offset.y`, and, because both passes read the same
    /// offset within one render, identically in build and render.
    pub(super) fn scroll_window(
        &mut self,
        sv_attrs: &[Attr],
        child_count: usize,
    ) -> Option<WindowSpec> {
        // Overscan rows on each side keep a row that is only partly scrolled into
        // view fully materialised, and hide the one-frame lag between an input
        // and the re-render that follows it.
        const OVERSCAN: usize = 2;
        if self.eval_bool_prop(sv_attrs, "windowed") != Some(true) {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let row_height = self.eval_px_prop(sv_attrs, "row_height").unwrap_or(0.0);
        if row_height <= 0.0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let viewport_h = self.eval_px_prop(sv_attrs, "height").unwrap_or(0.0);
        let (_, offset_y) = self.resolve_axis_pair(sv_attrs, "offset", (0.0, 0.0));

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first = (offset_y / row_height).floor().max(0.0) as usize;
        let start = first.saturating_sub(OVERSCAN);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let span = (viewport_h / row_height).ceil() as usize + 2 * OVERSCAN + 1;
        let end = start.saturating_add(span).min(child_count);
        Some(WindowSpec {
            start,
            end,
            row_height,
            n: child_count,
        })
    }

    /// Whether a `ScrollView` child's laid-out rectangle, mapped through the
    /// scroll-shifted `transform`, falls entirely outside `clip`, the emission-
    /// culling test (RFC-0005 §3.3). All four corners are transformed so a scaled
    /// ancestor is handled; an unknown rect is conservatively kept (never culled).
    pub(super) fn child_fully_clipped(
        &self,
        child_id: byard_core::atlas::layout::AtlasNodeId,
        transform: byard_core::frame::Transform,
        clip: byard_core::frame::Rect,
    ) -> bool {
        let Ok(Some(r)) = self.atlas.resolved_rect(child_id) else {
            return false;
        };
        let corners = [
            transform.apply_point([r.x, r.y]),
            transform.apply_point([r.x + r.width, r.y]),
            transform.apply_point([r.x, r.y + r.height]),
            transform.apply_point([r.x + r.width, r.y + r.height]),
        ];
        let min_x = corners.iter().map(|c| c[0]).fold(f32::INFINITY, f32::min);
        let max_x = corners
            .iter()
            .map(|c| c[0])
            .fold(f32::NEG_INFINITY, f32::max);
        let min_y = corners.iter().map(|c| c[1]).fold(f32::INFINITY, f32::min);
        let max_y = corners
            .iter()
            .map(|c| c[1])
            .fold(f32::NEG_INFINITY, f32::max);
        max_x <= clip.x
            || min_x >= clip.x + clip.width
            || max_y <= clip.y
            || min_y >= clip.y + clip.height
    }

    /// Resolves the RFC-0021 `snap_spring` prop into a packed spring curve, or
    /// `None` for the engine default. Reuses the `with`-animation curve grammar
    /// (`anim.spring(stiffness: …, damping: …)`); a malformed curve is reported
    /// and falls back to the default.
    pub(super) fn resolve_snap_spring(
        &mut self,
        attrs: &[Attr],
    ) -> Option<byard_core::frame::MotionCurve> {
        let value = attrs.iter().find_map(|a| match &a.kind {
            AttrKind::Prop { value } if a.name.as_str() == "snap_spring" => Some(value),
            _ => None,
        })?;
        match crate::interp::anim::resolve_curve(value) {
            Ok(curve) => Some(pack_curve(curve)),
            Err(e) => {
                self.errors.push(e);
                None
            }
        }
    }

    /// Reads a scroll offset signal as an `f32` (an `Int` or `Float` `var`);
    /// anything else reads as the origin.
    pub(super) fn peek_scroll(&self, sig: SignalId) -> f32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        match self.peek(sig) {
            Value::Int(n) => n as f32,
            Value::Float(f) => f as f32,
            _ => 0.0,
        }
    }

    /// Writes `value` back to a scroll offset signal, preserving its `Int`/`Float`
    /// kind so a whole-pixel `var off: Int` never becomes a `Float` mid-scroll.
    fn write_scroll(&mut self, sig: SignalId, value: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let v = match self.peek(sig) {
            Value::Int(_) => Value::Int(value.round() as i64),
            _ => Value::Float(f64::from(value)),
        };
        self.write_var(sig, v);
    }

    /// Nudges one scroll axis by `delta` logical px (wheel/trackpad), clamped to
    /// `[0, max]`. A forward delta reveals earlier content, so the offset shrinks.
    pub(super) fn nudge_scroll(&mut self, axis: ScrollAxis, delta: f32) {
        let next = (self.peek_scroll(axis.sig) - delta).clamp(0.0, axis.max);
        self.write_scroll(axis.sig, next);
    }

    /// Estimates and records the signed scroll velocity for `elem` (RFC-0021 fling
    /// projection): the offset change since the last input over its `time_ms`
    /// (px/s, positive = offset increasing). Skipped when the clock does not
    /// advance (`dt == 0`, e.g. unit tests) so velocity stays 0 and a settle snaps
    /// to the nearest boundary.
    pub(super) fn record_scroll_velocity(&mut self, elem: u32, offset: f32, time_ms: u64) {
        if let Some((prev_off, prev_t)) = self.scroll_vel_last.get(&elem).copied() {
            let dt = time_ms.saturating_sub(prev_t);
            if dt > 0 {
                self.scroll_vel
                    .insert(elem, (offset - prev_off) / (dt as f32 / 1000.0));
            }
        }
        self.scroll_vel_last.insert(elem, (offset, time_ms));
    }

    /// The scrollable axis of a target and its viewport extent (the one with
    /// travel, `max > 0`), vertical preferred. RFC-0021 snap/pagination helper.
    pub(super) fn scrollable_axis(t: &ScrollTarget) -> Option<(ScrollAxis, f32)> {
        t.y.filter(|a| a.max > 0.0)
            .map(|a| (a, t.rect.h))
            .or_else(|| t.x.filter(|a| a.max > 0.0).map(|a| (a, t.rect.w)))
    }

    /// Reflects the current page for `t` from its offset (`round(offset /
    /// viewport)`), writing the `page:` var and firing `page_change` on a change.
    /// Runs continuously (every scroll) so pagination tracks wheel/trackpad
    /// scrolling, not just snap settles (RFC-0021).
    fn reflect_page(&mut self, t: &ScrollTarget) {
        let (Some(psig), Some(elem)) = (t.page_sig, t.elem) else {
            return;
        };
        let Some((axis, vp)) = Self::scrollable_axis(t) else {
            return;
        };
        if vp <= 0.0 {
            return;
        }
        #[allow(clippy::cast_possible_truncation)]
        let page = (self.peek_scroll(axis.sig) / vp).round() as i64;
        if self.ctx.peek_signal(psig).as_int() != Some(page) {
            self.ctx.write_signal(psig, Value::Int(page));
            self.router.fire_event(
                &mut self.ctx,
                elem,
                super::events::EventKind::PageChange,
                Some(&Value::Int(page)),
            );
        }
        // Treat our reflection as the synced value so `page`→offset never fights.
        self.scroll_page_last.insert(elem, page);
    }

    /// The offset `t`'s scrollable `axis` should rest at, snapping the current
    /// offset to the mode's nearest boundary (RFC-0021): a viewport multiple for
    /// [`SnapMode::Page`], or the nearest precomputed direct-child boundary for
    /// [`SnapMode::Item`] (already `snap_align`-adjusted in
    /// [`scroll_item_bounds`](Self::scroll_item_bounds)). `None` for `snap: none`
    /// or when an item view has no measured children yet.
    fn snap_target_offset(&self, t: &ScrollTarget, axis: ScrollAxis, vp: f32) -> Option<f32> {
        let cur = self.peek_scroll(axis.sig);
        match t.snap {
            SnapMode::None => None,
            SnapMode::Page => Some(((cur / vp).round() * vp).clamp(0.0, axis.max)),
            SnapMode::Item => {
                let bounds = self.scroll_item_bounds.get(&t.elem?)?;
                bounds
                    .iter()
                    .copied()
                    .min_by(|a, b| (a - cur).abs().total_cmp(&(b - cur).abs()))
                    .map(|v| v.clamp(0.0, axis.max))
            }
        }
    }

    /// RFC-0021 `snap: item`: the rest offset for each direct child of the
    /// `ScrollView` atlas node, on the scrolling axis, aligned per `snap_align`
    /// and clamped to `[0, axis_max]`. Read once per render from the laid-out
    /// child rects (offset is a paint-time translate, so layout positions are the
    /// natural, offset-free content coordinates). A child whose start is `s` and
    /// extent `w` rests at `s` (start), `s − (vp − w)/2` (centre), or `s − (vp − w)`
    /// (end).
    pub(super) fn item_snap_offsets(
        &self,
        sv_node: byard_core::atlas::layout::AtlasNodeId,
        horizontal: bool,
        vp: f32,
        axis_max: f32,
        align: SnapAlign,
    ) -> Vec<f32> {
        let Ok(Some(sv)) = self.atlas.resolved_rect(sv_node) else {
            return Vec::new();
        };
        // The items are the flex children that lay out along the scroll axis. A
        // scrolling `ScrollView` almost always wraps them in one `Row`/`Column`
        // (needed to flow on the scroll axis), so when there is a single content
        // child, descend into it and snap to *its* children; otherwise the direct
        // children are the items.
        let direct = self.atlas.children(sv_node);
        let items = match direct.as_slice() {
            [only] => {
                let inner = self.atlas.children(*only);
                if inner.is_empty() { direct } else { inner }
            }
            _ => direct,
        };
        let mut offsets: Vec<f32> = items
            .into_iter()
            .filter_map(|child| self.atlas.resolved_rect(child).ok().flatten())
            .map(|r| {
                let (start, extent) = if horizontal {
                    (r.x - sv.x, r.width)
                } else {
                    (r.y - sv.y, r.height)
                };
                let aligned = match align {
                    SnapAlign::Start => start,
                    SnapAlign::Center => start - (vp - extent) / 2.0,
                    SnapAlign::End => start - (vp - extent),
                };
                aligned.clamp(0.0, axis_max)
            })
            .collect();
        // Ascending, so the fling-projection ±1 clamp indexes adjacent items.
        offsets.sort_by(f32::total_cmp);
        offsets
    }

    /// The offset a settle should target, applying RFC-0021 fling projection: above
    /// the fling velocity threshold it advances one boundary in the fling direction
    /// (clamped ±1 of the nearest, no multi-item skip); otherwise the nearest.
    /// Shares boundary geometry with [`snap_target_offset`](Self::snap_target_offset).
    fn snap_settle_target(&self, t: &ScrollTarget, axis: ScrollAxis, vp: f32) -> Option<f32> {
        /// Fling velocity (px/s) above which the settle projects rather than snaps
        /// to nearest (RFC-0021 resolved question: 150 dp/s).
        const FLING: f32 = 150.0;
        let cur = self.peek_scroll(axis.sig);
        let vel = t
            .elem
            .and_then(|e| self.scroll_vel.get(&e).copied())
            .unwrap_or(0.0);
        // The ordered boundary offsets on this axis.
        let bounds: Vec<f32> = match t.snap {
            SnapMode::None => return None,
            SnapMode::Page => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let last = (axis.max / vp).ceil().max(0.0) as usize;
                #[allow(clippy::cast_precision_loss)]
                (0..=last)
                    .map(|i| ((i as f32) * vp).min(axis.max))
                    .collect()
            }
            SnapMode::Item => self.scroll_item_bounds.get(&t.elem?)?.clone(),
        };
        if bounds.is_empty() {
            return None;
        }
        let nearest = bounds
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (**a - cur).abs().total_cmp(&(**b - cur).abs()))
            .map(|(i, _)| i)?;
        let target = if vel > FLING {
            // first boundary strictly ahead of the current offset
            bounds
                .iter()
                .position(|&b| b > cur + 0.5)
                .unwrap_or(nearest)
        } else if vel < -FLING {
            // last boundary strictly behind
            bounds
                .iter()
                .rposition(|&b| b < cur - 0.5)
                .unwrap_or(nearest)
        } else {
            nearest
        };
        // Clamp to ±1 of the nearest so a fling never skips more than one boundary.
        let target = target.clamp(nearest.saturating_sub(1), nearest + 1);
        Some(bounds[target].clamp(0.0, axis.max))
    }

    /// Begins a smooth snap of `t`'s scrollable axis to its nearest boundary: reflects
    /// `page` right away (the indicator jumps to the destination as the glide
    /// starts) and seeds a spring from the current offset to the page boundary
    /// (RFC-0021 §2, RFC-0010 spring). [`advance_snap_anims`](Self::advance_snap_anims)
    /// drives it to rest and fires `scroll_end`. When no clock is advancing (a
    /// non-animating host or a test), it resolves to the boundary instantly so the
    /// behaviour is identical minus the animation. Shared by drag-release and the
    /// scroll-quiet settle; a no-op if already resting on a page.
    fn begin_snap(&mut self, t: &ScrollTarget) {
        /// Sub-pixel tolerance for "already on a boundary".
        const EPS: f32 = 0.5;
        if t.snap == SnapMode::None {
            return;
        }
        let (Some((axis, vp)), Some(elem)) = (Self::scrollable_axis(t), t.elem) else {
            return;
        };
        if vp <= 0.0 {
            return;
        }
        let cur = self.peek_scroll(axis.sig);
        // A fling above the velocity threshold projects one boundary ahead; a slow
        // settle snaps to the nearest. Consume the velocity so the next gesture
        // starts fresh.
        let Some(target) = self.snap_settle_target(t, axis, vp) else {
            return;
        };
        self.scroll_vel.remove(&elem);
        self.scroll_vel_last.remove(&elem);
        if (cur - target).abs() < EPS {
            self.snap_anims.remove(&elem);
            return; // already on a boundary, nothing to glide
        }
        // Destination page is known now, reflect it immediately (fires
        // `page_change`) so pagination leads the glide, and settle it exactly once
        // the spring (or the instant fallback) arrives.
        self.reflect_page(t);
        if self.clock_set {
            let curve = t
                .snap_spring
                .unwrap_or_else(|| pack_curve(crate::interp::anim::Curve::DEFAULT_SPRING));
            self.snap_anims.insert(
                elem,
                SnapAnim {
                    sig: axis.sig,
                    motion: byard_core::frame::Motion {
                        from: cur,
                        to: target,
                        start_ms: self.now_ms,
                        curve,
                    },
                    target,
                },
            );
            self.any_active = true;
        } else {
            self.finish_snap(elem, axis.sig, target);
        }
    }

    /// Pins `sig` to the exact page `target`, clears any pending glide, and fires
    /// `scroll_end` for `elem` (RFC-0021 snap completion).
    fn finish_snap(&mut self, elem: u32, sig: SignalId, target: f32) {
        self.snap_anims.remove(&elem);
        self.write_scroll(sig, target);
        self.router.fire_event(
            &mut self.ctx,
            elem,
            super::events::EventKind::ScrollEnd,
            None,
        );
    }

    /// Advances every in-flight snap spring one `render` (RFC-0021 smooth snap):
    /// samples each [`SnapAnim`](SnapAnim) at the engine clock, writes the offset,
    /// and, once the spring settles, pins the offset exactly on the page and
    /// fires `scroll_end`. A live drag on an elem cancels its glide (the finger
    /// takes over). Keeps `any_active` set while any spring is still moving so the
    /// host keeps presenting frames until it rests.
    pub(super) fn advance_snap_anims(&mut self) {
        /// Pixel/velocity settle gates for a scroll offset (sub-pixel is
        /// imperceptible; the exact target is pinned on settle regardless).
        const EPS_POS: f32 = 0.5;
        const EPS_VEL: f32 = 2.0;
        if self.snap_anims.is_empty() {
            return;
        }
        let drag_elem = self.scroll_drag.and_then(|d| d.elem);
        let now = self.now_ms;
        for (elem, anim) in self
            .snap_anims
            .iter()
            .map(|(e, a)| (*e, *a))
            .collect::<Vec<_>>()
        {
            if drag_elem == Some(elem) {
                self.snap_anims.remove(&elem); // the finger reclaimed this view
                continue;
            }
            if anim.motion.is_settled_with_eps(now, EPS_POS, EPS_VEL) {
                self.finish_snap(elem, anim.sig, anim.target);
            } else {
                self.write_scroll(anim.sig, anim.motion.sample(now));
                self.any_active = true;
            }
        }
    }

    /// On drag release, snap the `ScrollView` under the press point (RFC-0021).
    pub(super) fn snap_scroll_on_release(&mut self, start_pos: (f32, f32)) {
        let (px, py) = start_pos;
        let Some(t) = self
            .scroll_targets
            .iter()
            .rev()
            .find(|t| {
                px >= t.rect.x
                    && px < t.rect.x + t.rect.w
                    && py >= t.rect.y
                    && py < t.rect.y + t.rect.h
            })
            .copied()
        else {
            return;
        };
        self.begin_snap(&t);
    }

    /// Reflects `page` for every scroll target after this tick's scroll writes
    /// (RFC-0021 continuous pagination).
    pub(super) fn reflect_pages(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            self.reflect_page(&t);
        }
    }

    /// RFC-0021 snap settle: once a `snap`-enabled `ScrollView` has gone quiet,
    /// no wheel/trackpad scroll input for [`SETTLE_FRAMES`] renders (so trackpad
    /// momentum, a stream of shrinking deltas, cannot trigger a snap mid-fling
    /// that fights the next event), glide its offset to the nearest page via
    /// [`begin_snap`](Self::begin_snap). Frame-counted, not clock-based, so it
    /// settles identically whether or not the host advances `now_ms`. A live drag
    /// never settles on stillness (it snaps on release). Runs each `render`, over
    /// the previous frame's targets.
    pub(super) fn settle_snaps(&mut self) {
        /// Renders of quiet (no scroll input) before a `snap: page` view settles.
        const SETTLE_FRAMES: u64 = 4;
        /// Sub-pixel tolerance for "already on a page".
        const EPS: f32 = 0.5;
        let drag_elem = self.scroll_drag.and_then(|d| d.elem);
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let Some(elem) = t.elem else { continue };
            // A drag in progress, or a glide already running, owns the offset.
            if t.snap == SnapMode::None
                || drag_elem == Some(elem)
                || self.snap_anims.contains_key(&elem)
            {
                continue;
            }
            let Some((axis, vp)) = Self::scrollable_axis(&t) else {
                continue;
            };
            if vp <= 0.0 {
                continue;
            }
            let cur = self.peek_scroll(axis.sig);
            let Some(boundary) = self.snap_target_offset(&t, axis, vp) else {
                continue;
            };
            if (cur - boundary).abs() < EPS {
                continue; // already resting on a boundary
            }
            // Off a boundary: wait for the scroll to go quiet, then glide. `quiet`
            // is how many renders since the last scroll input touched this elem;
            // momentum keeps resetting it, so we only snap once the fling ends.
            let quiet = self
                .scroll_quiet
                .get(&elem)
                .map_or(SETTLE_FRAMES, |last| self.frame_seq.saturating_sub(*last));
            if quiet >= SETTLE_FRAMES {
                self.begin_snap(&t);
            } else {
                self.any_active = true; // keep frames coming until it goes quiet
            }
        }
    }

    /// RFC-0021 reflected `page:` (the reverse direction): when the app sets the
    /// `page` var, scroll the `ScrollView`'s offset to that page. Edge-triggered
    /// against [`scroll_page_last`](Self::scroll_page_last) so it fires only on an
    /// external change (a drag never writes `page` mid-gesture, and our own snap
    /// updates the tracker), never level-triggered against the live offset, so it
    /// can't fight scrolling. Runs at the top of `render` over the previous
    /// frame's targets.
    pub(super) fn sync_page_offsets(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let (Some(psig), Some(elem)) = (t.page_sig, t.elem) else {
                continue;
            };
            let Some(page) = self.ctx.peek_signal(psig).as_int() else {
                continue;
            };
            if self.scroll_page_last.get(&elem) == Some(&page) {
                continue; // no external change since we last synced
            }
            let axis_vp =
                t.y.filter(|a| a.max > 0.0)
                    .map(|a| (a, t.rect.h))
                    .or_else(|| t.x.filter(|a| a.max > 0.0).map(|a| (a, t.rect.w)));
            if let Some((axis, vp)) = axis_vp {
                #[allow(clippy::cast_precision_loss)]
                let target = (page.max(0) as f32 * vp).clamp(0.0, axis.max);
                self.write_scroll(axis.sig, target);
            }
            self.scroll_page_last.insert(elem, page);
        }
    }

    /// RFC-0021 `on_end_reached`: fires `end_reached` once for any `ScrollView`
    /// whose visible bottom has crossed `end_threshold` of its content, debounced
    /// via [`end_reached_fired`](Self::end_reached_fired) until the offset falls
    /// back below the threshold (so appending items re-arms it).
    pub(super) fn fire_end_reached(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let (Some(threshold), Some(elem)) = (t.end_threshold, t.elem) else {
                continue;
            };
            let axis_vp =
                t.y.filter(|a| a.max > 0.0)
                    .map(|a| (a, t.rect.h))
                    .or_else(|| t.x.filter(|a| a.max > 0.0).map(|a| (a, t.rect.w)));
            let Some((axis, vp)) = axis_vp else { continue };
            if axis.max <= 0.0 {
                continue;
            }
            let frac = (self.peek_scroll(axis.sig) + vp) / (axis.max + vp);
            if frac >= threshold {
                if self.end_reached_fired.insert(elem) {
                    self.router.fire_event(
                        &mut self.ctx,
                        elem,
                        super::events::EventKind::EndReached,
                        None,
                    );
                }
            } else {
                self.end_reached_fired.remove(&elem);
            }
        }
    }

    /// Snapshots one axis of a drag at the press: its live offset becomes the
    /// baseline the pointer travel is subtracted from (RFC-0005).
    pub(super) fn capture_drag_axis(&self, axis: ScrollAxis) -> ScrollDragAxis {
        let is_int = matches!(self.peek(axis.sig), Value::Int(_));
        ScrollDragAxis {
            sig: axis.sig,
            start_offset: self.peek_scroll(axis.sig),
            max: axis.max,
            is_int,
        }
    }

    /// Applies a drag's pointer `travel` (current − press) to one axis: the
    /// content follows the pointer, so the offset is the press offset minus the
    /// travel, clamped to `[0, max]` (RFC-0005 drag-to-scroll).
    pub(super) fn write_drag_axis(&mut self, axis: ScrollDragAxis, travel: f32) {
        let next = (axis.start_offset - travel).clamp(0.0, axis.max);
        let value = if axis.is_int {
            #[allow(clippy::cast_possible_truncation)]
            Value::Int(next.round() as i64)
        } else {
            Value::Float(f64::from(next))
        };
        self.write_var(axis.sig, value);
    }

    /// Draws the default pull-to-refresh indicator (RFC-0021): a light ring
    /// centred in the revealed gap that grows and fades in with `progress`
    /// (0 → threshold) and holds solid while a refresh is `active`. `gap_px` is the
    /// pull region's on-screen height; `clip` the viewport rect. Drawn as a
    /// border-only [`DecoratedBox`] (transparent fill + full radii = a ring).
    pub(super) fn push_pull_indicator(
        frame: &mut byard_core::frame::RenderFrame,
        clip: byard_core::frame::Rect,
        gap_px: f32,
        progress: f32,
        active: bool,
    ) {
        let r = (gap_px * 0.28).clamp(3.0, 11.0);
        let cx = clip.x + clip.width / 2.0;
        let cy = clip.y + gap_px / 2.0;
        let opacity = if active { 1.0 } else { progress };
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: byard_core::BoxInstance {
                rect: [cx - r, cy - r, 2.0 * r, 2.0 * r],
                color: [0.0; 4],
                radii: [r; 4],
                transform: byard_core::frame::Transform::IDENTITY,
                smooth: 0.0,
            },
            border_width: 2.0,
            border_color: [0.85, 0.84, 0.88, 1.0],
            opacity,
            dirty: true,
            ..Default::default()
        });
    }

    /// The elastic pull height for a raw over-drag, in logical px (RFC-0021
    /// pull-to-refresh): a diminishing-returns curve that asymptotes to
    /// [`PULL_MAX`], so the region resists ever harder the further it is pulled.
    fn pull_elastic(raw: f32) -> f32 {
        if raw <= 0.0 {
            0.0
        } else {
            PULL_MAX * raw / (raw + PULL_MAX)
        }
    }

    /// RFC-0021 pull-to-refresh: a downward over-drag past the top grows the pull
    /// region for `d`'s elem. `pointer_y` is the live pointer; the over-drag is the
    /// travel beyond whatever upward scroll the press had available (`0` for a
    /// non-scrolling pull view). A live drag cancels any retract spring.
    pub(super) fn drive_pull_drag(&mut self, d: ScrollDrag, pointer_y: f32) {
        let Some(elem) = d.elem else { return };
        let start_y = d.y.map_or(0.0, |a| a.start_offset);
        let over = (pointer_y - d.start_pos.1 - start_y).max(0.0);
        if over > 0.0 {
            self.pull_anims.remove(&elem);
            self.pull_distance.insert(elem, Self::pull_elastic(over));
            self.any_active = true;
        } else {
            self.pull_distance.remove(&elem);
        }
    }

    /// RFC-0021 pull-to-refresh release: past [`PULL_THRESHOLD`] fire `refresh`,
    /// set `refreshing = true`, and rest the indicator at [`PULL_REST`]; otherwise
    /// retract the pull region to `0`.
    pub(super) fn release_pull(&mut self, d: ScrollDrag) {
        let Some(elem) = d.elem else { return };
        let pull = self.pull_distance.get(&elem).copied().unwrap_or(0.0);
        if pull >= PULL_THRESHOLD {
            self.router
                .fire_event(&mut self.ctx, elem, super::events::EventKind::Refresh, None);
            if let Some(sig) = d.refreshing_sig {
                // The app owns the `refreshing` lifecycle: hold the indicator at
                // rest until it clears the flag (retracts via `sync_refreshing`).
                self.ctx.write_signal(sig, Value::Bool(true));
                self.refreshing_seen.insert(elem, true);
                self.begin_pull_settle(elem, PULL_REST);
            } else {
                // No `refreshing` binding, a momentary trigger; retract now.
                self.begin_pull_settle(elem, 0.0);
            }
        } else {
            self.begin_pull_settle(elem, 0.0);
        }
    }

    /// Springs `elem`'s pull region toward `target` (RFC-0021): the indicator rest
    /// height while refreshing, or `0` to retract it. Resolves instantly with no
    /// advancing clock (tests / non-animating hosts), mirroring `begin_snap`.
    fn begin_pull_settle(&mut self, elem: u32, target: f32) {
        if !self.clock_set {
            self.pull_anims.remove(&elem);
            if target <= 0.0 {
                self.pull_distance.remove(&elem);
            } else {
                self.pull_distance.insert(elem, target);
            }
            return;
        }
        let from = self.pull_distance.get(&elem).copied().unwrap_or(0.0);
        let curve = pack_curve(crate::interp::anim::Curve::DEFAULT_SPRING);
        self.pull_anims.insert(
            elem,
            PullAnim {
                motion: byard_core::frame::Motion {
                    from,
                    to: target,
                    start_ms: self.now_ms,
                    curve,
                },
                target,
            },
        );
        self.any_active = true;
    }

    /// Advances every in-flight pull-region spring one `render` (RFC-0021): writes
    /// the sampled height into [`pull_distance`](Self::pull_distance) and, on
    /// settle, pins the exact target (`0` retracts the indicator entirely).
    pub(super) fn advance_pull_anims(&mut self) {
        const EPS_POS: f32 = 0.5;
        const EPS_VEL: f32 = 2.0;
        if self.pull_anims.is_empty() {
            return;
        }
        let now = self.now_ms;
        for (elem, anim) in self
            .pull_anims
            .iter()
            .map(|(e, a)| (*e, *a))
            .collect::<Vec<_>>()
        {
            if anim.motion.is_settled_with_eps(now, EPS_POS, EPS_VEL) {
                self.pull_anims.remove(&elem);
                if anim.target <= 0.0 {
                    self.pull_distance.remove(&elem);
                } else {
                    self.pull_distance.insert(elem, anim.target);
                }
            } else {
                self.pull_distance.insert(elem, anim.motion.sample(now));
                self.any_active = true;
            }
        }
    }

    /// RFC-0021 reflected `refreshing:`: honour an app-driven change (edge-triggered
    /// against [`refreshing_seen`](Self::refreshing_seen), never fighting the
    /// engine's own set on trigger). Clearing it (`true → false`) retracts the
    /// indicator; setting it programmatically (`false → true`) shows it at rest.
    /// Runs at the top of `render` over the previous frame's targets.
    pub(super) fn sync_refreshing(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let (Some(sig), Some(elem)) = (t.refreshing_sig, t.elem) else {
                continue;
            };
            let r = matches!(self.ctx.peek_signal(sig), Value::Bool(true));
            if self.refreshing_seen.get(&elem) == Some(&r) {
                continue;
            }
            self.begin_pull_settle(elem, if r { PULL_REST } else { 0.0 });
            self.refreshing_seen.insert(elem, r);
        }
    }
}
