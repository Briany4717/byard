//! The value controls: `Toggle`, `Checkbox`, `RadioButton` and `Slider`
//! (RFC-0005, RFC-0024).
//!
//! Each one is painted from its bound `var` during the paint walk and registers
//! the handlers that write it back: a tap flips a toggle or checkbox, selects
//! a radio, and a press or drag sets a slider (quantised to its `step`, or to
//! a few decimals without one). The `bind:` and `value:` attributes are
//! resolved to the `var` they name here.

use super::Interpreter;
use super::paint::dim_alpha;
use crate::interp::env::Value;
use crate::parser::ast::{Attr, AttrKind};

/// Decimal places a `Slider` without an explicit `step` quantises its value to.
///
/// A continuous slider otherwise emits the full `f64` precision of a
/// pixel-derived ratio (e.g. `0.6035294…`); rounding keeps the bound value
/// readable. Authors who need a specific granularity set `step:` instead.
const SLIDER_DEFAULT_DECIMALS: i32 = 3;

/// Decimal places implied by `step`, via its shortest round-trip form
/// (`0.1 → 1`, `0.25 → 2`, `1.0 → 0`). Used so a stepped slider never emits a
/// value with more decimal places than the step itself, e.g. `step: 0.1`
/// landing on `6 × 0.1 = 0.6000000000000001` is rounded back to `0.6`. Capped
/// at 10 places (any real step is far coarser).
fn step_decimals(step: f64) -> i32 {
    match format!("{}", step.abs()).split_once('.') {
        Some((_, frac)) => i32::try_from(frac.len().min(10)).unwrap_or(0),
        None => 0,
    }
}

/// Rounds `val` to `decimals` decimal places (half-away-from-zero).
fn round_to_decimals(val: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    (val * factor).round() / factor
}

/// Pushes a single rounded stroke quad from `a` to `b` of thickness `t`,
/// rotated to the segment angle about its midpoint and composed under
/// `transform` (so an element/group transform carries the mark with it). Backs
/// the RFC-0018 `Checkbox` checkmark. Emitted on the **decorated** pipeline so it
/// paints above the checkbox's decorated (bordered) container, which is pushed
/// first.
fn push_stroke_quad(
    frame: &mut byard_core::frame::RenderFrame,
    a: [f32; 2],
    b: [f32; 2],
    t: f32,
    color: [f32; 4],
    transform: byard_core::frame::Transform,
) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();
    let cx = f32::midpoint(a[0], b[0]);
    let cy = f32::midpoint(a[1], b[1]);
    let seg = byard_core::frame::Transform {
        rotate: dy.atan2(dx),
        origin: [cx, cy],
        ..byard_core::frame::Transform::IDENTITY
    };
    frame.push_decorated(byard_core::frame::DecoratedBox {
        base: byard_core::BoxInstance {
            rect: [cx - len / 2.0, cy - t / 2.0, len, t],
            color,
            radii: [t / 2.0; 4],
            transform: transform.compose(&seg),
            smooth: 0.0,
        },
        dirty: true,
        ..Default::default()
    });
}

impl Interpreter {
    /// Resolves the `bind:` or `value:` attribute of a value widget to a
    /// `SignalId`. Returns `None` if no such attribute exists or it doesn't
    /// name a `var`.
    pub(super) fn resolve_bind_sig(&self, attrs: &[Attr]) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if matches!(attr.name.as_str(), "bind" | "value") {
                match &attr.kind {
                    AttrKind::Prop {
                        value: Expr::Ident(name, _),
                    } => {
                        if let Some(super::env::Value::Signal(sig)) = self.env.lookup(name) {
                            return Some(*sig);
                        }
                    }
                    // `bind: theme.dark` binds a toggle straight to the reactive
                    // scheme flag (RFC-0022 §1), tapping it recolors the tree.
                    AttrKind::Prop { value } => {
                        if let Some(sig) = self.resolve_theme_scheme_target(value) {
                            return Some(sig);
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// Resolves *only* the `bind:` attribute of a `RadioButton` (RFC-0018) to the
    /// group `var`'s `SignalId`. Unlike [`resolve_bind_sig`], it never inspects
    /// `value:`, for a radio, `value` is the button's literal identity string,
    /// not a signal binding. Returns `None` if `bind:` is absent or doesn't name
    /// a `var`.
    pub(super) fn resolve_group_bind_sig(&self, attrs: &[Attr]) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if attr.name.as_str() == "bind" {
                if let AttrKind::Prop {
                    value: Expr::Ident(name, _),
                } = &attr.kind
                {
                    if let Some(super::env::Value::Signal(sig)) = self.env.lookup(name) {
                        return Some(*sig);
                    }
                }
            }
        }
        None
    }

    /// Resolves the attribute `name`'s value to a writable `var`'s `SignalId`
    /// when it is a bare identifier bound to a `var` (else `None`). Backs
    /// RFC-0021's reflected `page:` prop; a small generalization of
    /// [`resolve_group_bind_sig`](Self::resolve_group_bind_sig).
    pub(super) fn resolve_named_var_sig(
        &self,
        attrs: &[Attr],
        name: &str,
    ) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if attr.name.as_str() == name {
                if let AttrKind::Prop {
                    value: Expr::Ident(n, _),
                } = &attr.kind
                {
                    if let Some(super::env::Value::Signal(sig)) = self.env.lookup(n) {
                        return Some(*sig);
                    }
                }
            }
        }
        None
    }

    // ── Widget rendering helpers ─────────────────────────────

    /// Renders a `Toggle` widget: track + thumb, and registers a Tap
    /// handler to flip the bound bool.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_toggle(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let is_on = bound_sig.is_some_and(|s| self.ctx.peek_signal(s).as_bool().unwrap_or(false));

        // The full-height pill track. `bg` is the ON accent (default: theme
        // primary); OFF is a muted surface tint.
        let accent = self
            .eval_color_prop(attrs, "bg")
            .unwrap_or(self.theme.primary());
        // The accent reads as opaque on purpose, here and on every control
        // that owns its visuals: the layered pieces (thumb over track, dot
        // under ring, disc under disc) are composed for an opaque accent and
        // sit on the SolidBox pass. Fading a control is `opacity:`'s job.
        let track_color = if is_on {
            super::intrinsics::color_to_rgba(accent, false)
        } else {
            [0.40_f32, 0.42, 0.48, 1.0]
        };
        let radius = rect.h / 2.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [rect.x, rect.y, rect.w, rect.h],
            color: dim_alpha(track_color, opacity),
            radii: [radius; 4],
            transform,
            smooth: 0.0,
        });

        // Thumb: a white circle inset from the track edges, sliding L↔R.
        let pad = (rect.h * 0.12).max(2.0);
        let thumb_size = (rect.h - pad * 2.0).max(2.0);
        let thumb_y = rect.y + pad;
        let thumb_x = if is_on {
            rect.x + rect.w - thumb_size - pad
        } else {
            rect.x + pad
        };
        frame.push_instance(byard_core::BoxInstance {
            rect: [thumb_x, thumb_y, thumb_size, thumb_size],
            color: dim_alpha([1.0, 1.0, 1.0, 1.0], opacity),
            radii: [thumb_size / 2.0; 4],
            transform,
            smooth: 0.0,
        });

        // Tap handler to flip the bool.
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let flip: super::events::Action = Box::new(move |ctx, _| {
                let cur = ctx.peek_signal(sig).as_bool().unwrap_or(false);
                ctx.write_signal(sig, Value::Bool(!cur));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::Tap, flip);
        }
    }

    /// Renders a `Checkbox` widget (RFC-0018): a rounded square that fills with
    /// the accent colour and shows an engine-drawn checkmark when checked, a
    /// muted filled slot when unchecked, and a horizontal dash for the
    /// `indeterminate` mixed state, all borderless SolidBoxes (a 2px ring reads
    /// as a heavy dark outline at control sizes). Registers Tap + Space (KeyDown)
    /// to flip the bound bool, a `change` write-back (RFC-0003 E1), and a focus
    /// target so Tab and click reach it. Like `Toggle`, it owns its visuals,
    /// `bg` is the checked accent, never a full-rect slab.
    ///
    /// The checkmark is drawn as two rounded stroke quads rotated to each
    /// segment's angle through the paint-transform system, so it stays crisp at
    /// any size/DPI and needs no atlas asset for the leaf mark. (RFC-0018's
    /// resolved MSDF-baked checkmark is a rendering refinement tracked for the
    /// vector subsystem; the geometry and interaction contract here are final.)
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_checkbox(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let is_on = bound_sig.is_some_and(|s| self.ctx.peek_signal(s).as_bool().unwrap_or(false));
        let indeterminate = self.eval_bool_prop(attrs, "indeterminate").unwrap_or(false);
        // A checkbox is square: use the smaller side so a non-square rect still
        // yields crisp, proportional geometry, centred in the laid-out box.
        let side = rect.w.min(rect.h);
        let ox = rect.x + (rect.w - side) / 2.0;
        let oy = rect.y + (rect.h - side) / 2.0;
        let radius = (side * 0.18).max(2.0);
        let filled = is_on || indeterminate;

        // The container is a rounded `DecoratedBox` so it can carry a `border`
        // (RFC-0024 lets a style set `border`/`on checked { border }`). `bg` is
        // the checked accent (default: theme primary); when unchecked, a styled
        // `border` yields an outlined box with a transparent interior (the M3
        // look), otherwise a muted filled slot (`Toggle`'s OFF tint). The
        // container is pushed *before* the mark, both on the decorated pipeline,
        // so the white mark lands on top.
        let bg = self.eval_color_prop(attrs, "bg");
        let border = self.eval_color_prop(attrs, "border");
        let border_width = if border.is_some() {
            self.eval_float_prop(attrs, "border_width")
                .map_or(2.0, |v| v as f32)
        } else {
            0.0
        };
        // A border is a plain stroke on the blended pass, so its alpha byte
        // counts, as on any `Box`.
        let border_rgba = border.map_or([0.0; 4], |c| {
            dim_alpha(super::intrinsics::color_rgba_auto(c), opacity)
        });
        // Opaque accent, as for `Toggle`.
        let accent = bg.unwrap_or(self.theme.primary());
        let fill = if filled {
            dim_alpha(super::intrinsics::color_to_rgba(accent, false), opacity)
        } else if border.is_some() {
            [0.0; 4]
        } else {
            dim_alpha([0.40, 0.42, 0.48, 1.0], opacity)
        };
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: byard_core::BoxInstance {
                rect: [ox, oy, side, side],
                color: fill,
                radii: [radius; 4],
                transform,
                smooth: 0.0,
            },
            border_width,
            border_color: border_rgba,
            dirty: true,
            ..Default::default()
        });

        // The mark, in white, on the filled square (also decorated, so it paints
        // above the container).
        let mark = dim_alpha([1.0, 1.0, 1.0, 1.0], opacity);
        if indeterminate {
            // Mixed state: a single horizontal bar (checked overrides mixed only
            // in the container fill; the dash renders whenever `indeterminate`).
            let bar_w = side * 0.5;
            let bar_h = (side * 0.12).max(2.0);
            frame.push_decorated(byard_core::frame::DecoratedBox {
                base: byard_core::BoxInstance {
                    rect: [
                        ox + (side - bar_w) / 2.0,
                        oy + (side - bar_h) / 2.0,
                        bar_w,
                        bar_h,
                    ],
                    color: mark,
                    radii: [bar_h / 2.0; 4],
                    transform,
                    smooth: 0.0,
                },
                dirty: true,
                ..Default::default()
            });
        } else if is_on {
            // A two-stroke checkmark: canonical vertices in the unit square,
            // each segment a rounded quad rotated to its angle.
            let t = (side * 0.13).max(2.0);
            let pts = [
                [ox + side * 0.26, oy + side * 0.52], // short-stroke start
                [ox + side * 0.44, oy + side * 0.70], // bottom vertex
                [ox + side * 0.76, oy + side * 0.32], // long-stroke end
                [ox + side * 0.41, oy + side * 0.70], // long-stroke start
            ];
            for (a, b) in [(pts[0], pts[1]), (pts[3], pts[2])] {
                push_stroke_quad(frame, a, b, t, mark, transform);
            }
        }

        // Handlers: Tap + Space flip the bool; `change` write-back; focusable.
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let flip: super::events::Action = Box::new(move |ctx, _| {
                let cur = ctx.peek_signal(sig).as_bool().unwrap_or(false);
                ctx.write_signal(sig, Value::Bool(!cur));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::Tap, flip);

            // Space toggles when focused (WAI-ARIA checkbox keyboard pattern).
            let key_flip: super::events::Action = Box::new(move |ctx, payload| {
                if let Some(Value::Str(key)) = payload {
                    if matches!(key.as_str(), " " | "Space" | "Spacebar") {
                        let cur = ctx.peek_signal(sig).as_bool().unwrap_or(false);
                        ctx.write_signal(sig, Value::Bool(!cur));
                    }
                }
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::KeyDown, key_flip);

            // Change write-back from the platform (RFC-0003 E1).
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );

            // Focus target so Tab and click reach the box. Uses the element's own
            // `focused:` var when given, else a private signal for the registry.
            let focused_sig = self.resolve_focused_sig(attrs);
            let fsig = focused_sig.unwrap_or_else(|| self.ctx.create_signal(Value::Bool(false)));
            self.router.focusable(idx, hit_rect, fsig);
        }
    }

    /// Renders a `RadioButton` widget (RFC-0018): an outer ring plus an inner
    /// filled dot when selected. Selection is `bind == value`: the bound group
    /// `var`'s current string equals this button's `value`. Tapping writes this
    /// button's `value` to the group var, so the previously selected sibling
    /// deselects reactively (automatic mutual exclusion, every radio in the
    /// group reads the same var). Registers the group ordering for arrow-key
    /// navigation, a Tap handler, arrow KeyDown handlers (move selection within
    /// the group, wrapping), a `change` write-back (RFC-0003 E1), and a focus
    /// target so Tab/click reach it. Owns its visuals, `bg` is the selected
    /// accent. The ring is the radio's defining affordance (unlike `Checkbox`,
    /// whose square border was dropped); its interior is transparent so the dot
    /// shows through and it composes over any background.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_radio(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let value = self.eval_str_prop(attrs, "value").unwrap_or_default();
        let selected = bound_sig.is_some_and(|s| match self.ctx.peek_signal(s) {
            Value::Str(v) => v == value,
            _ => false,
        });

        // A radio is circular: use the smaller side, centred in the laid-out box.
        let side = rect.w.min(rect.h);
        let ox = rect.x + (rect.w - side) / 2.0;
        let oy = rect.y + (rect.h - side) / 2.0;
        let r = side / 2.0;

        let accent = self
            .eval_color_prop(attrs, "bg")
            .unwrap_or(self.theme.primary());
        // Opaque accent, as for `Toggle`.
        let accent_rgba = super::intrinsics::color_to_rgba(accent, false);
        let ring_color = if selected {
            accent_rgba
        } else {
            [0.55, 0.57, 0.62, 1.0]
        };

        // Inner dot FIRST (solid pipeline) so it sits beneath the ring; the ring's
        // interior is transparent, so the dot shows through regardless. Only drawn
        // when selected.
        if selected {
            let dot = side * 0.5;
            let inset = (side - dot) / 2.0;
            frame.push_instance(byard_core::BoxInstance {
                rect: [ox + inset, oy + inset, dot, dot],
                color: dim_alpha(accent_rgba, opacity),
                radii: [dot / 2.0; 4],
                transform,
                smooth: 0.0,
            });
        }

        // Outer ring: a transparent-interior DecoratedBox with a full-radius
        // border. Width scales with size so it reads as a ring, not a hairline.
        let ring_w = (side * 0.12).max(2.0);
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: byard_core::BoxInstance {
                rect: [ox, oy, side, side],
                color: [0.0; 4],
                radii: [r; 4],
                transform,
                smooth: 0.0,
            },
            border_width: ring_w,
            border_color: dim_alpha(ring_color, opacity),
            dirty: true,
            ..Default::default()
        });

        // ── Group registration + handlers ────────────────────────────────────
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            // Record this button's value in its group's ordered list (shared via
            // an Rc so the arrow handlers below see the whole group once the
            // render has finished populating it).
            let group = self.radio_groups.entry(sig).or_default().clone();
            group.borrow_mut().push(value.clone());

            // Tap → select this button (write its value to the group var).
            let val = value.clone();
            let select: super::events::Action = Box::new(move |ctx, _| {
                ctx.write_signal(sig, Value::Str(val.clone()));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::Tap, select);

            // Arrow keys → move selection to the next/previous value in the group,
            // wrapping at both ends (WAI-ARIA radio group). Down/Right advance;
            // Up/Left retreat.
            let grp = group.clone();
            let arrows: super::events::Action = Box::new(move |ctx, payload| {
                let Some(Value::Str(key)) = payload else {
                    return;
                };
                let forward = match key.as_str() {
                    "ArrowDown" | "ArrowRight" => true,
                    "ArrowUp" | "ArrowLeft" => false,
                    _ => return,
                };
                let vals = grp.borrow();
                let n = vals.len();
                if n == 0 {
                    return;
                }
                let cur = match ctx.peek_signal(sig) {
                    Value::Str(s) => s,
                    _ => String::new(),
                };
                let here = vals.iter().position(|v| *v == cur).unwrap_or(0);
                // Wrap-around in unsigned arithmetic (no signed casts): forward is
                // `+1`, backward is `+ (n − 1)`, both mod `n`.
                let next = if forward {
                    (here + 1) % n
                } else {
                    (here + n - 1) % n
                };
                ctx.write_signal(sig, Value::Str(vals[next].clone()));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::KeyDown, arrows);

            // Change write-back from the platform (RFC-0003 E1).
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );

            // Focus target so Tab and click reach the button.
            let focused_sig = self.resolve_focused_sig(attrs);
            let fsig = focused_sig.unwrap_or_else(|| self.ctx.create_signal(Value::Bool(false)));
            self.router.focusable(idx, hit_rect, fsig);
        }
    }

    /// Renders a `Slider` widget: track + fill + thumb, and registers
    /// PointerDown + PointerDrag handlers to write the value.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_slider(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        // Keep the authored `f64` values for the value-write path: computing the
        // emitted value in `f64` avoids the `f32`→`f64` widening artifact (a drag
        // landing on 0.6 was stored as `f64::from(0.6_f32)` =
        // 0.6000000238418579). The `f32` casts below are only for pixel-space
        // visual layout (track/fill/thumb), where the noise is invisible.
        let min_f = self.eval_float_prop(attrs, "min").unwrap_or(0.0);
        let max_f = self.eval_float_prop(attrs, "max").unwrap_or(1.0);
        let step_f = self.eval_float_prop(attrs, "step");
        let min = min_f as f32;
        let max = max_f as f32;
        let cur_val = bound_sig.map_or(min, |s| match self.ctx.peek_signal(s) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => min,
        });
        let t = if (max - min).abs() > f32::EPSILON {
            ((cur_val - min) / (max - min)).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // `bg` is the fill accent (default: theme primary); the unfilled track
        // is a muted tint.
        let accent = self
            .eval_color_prop(attrs, "bg")
            .unwrap_or(self.theme.primary());
        // Opaque accent, as for `Toggle`.
        let accent_rgba = super::intrinsics::color_to_rgba(accent, false);

        // Track (unfilled remainder).
        let track_h = (rect.h * 0.28).clamp(4.0, 8.0);
        let track_y = rect.y + (rect.h - track_h) / 2.0;
        let track_r = track_h / 2.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [rect.x, track_y, rect.w, track_h],
            color: dim_alpha([0.40, 0.42, 0.48, 1.0], opacity),
            radii: [track_r; 4],
            transform,
            smooth: 0.0,
        });

        // Fill up to the thumb.
        let fill_w = t * rect.w;
        if fill_w > 0.0 {
            frame.push_instance(byard_core::BoxInstance {
                rect: [rect.x, track_y, fill_w, track_h],
                color: dim_alpha(accent_rgba, opacity),
                radii: [track_r; 4],
                transform,
                smooth: 0.0,
            });
        }

        // Thumb: white circle with a thin accent ring (drawn as accent disc
        // under a slightly smaller white disc).
        let thumb_size = (rect.h * 0.85).clamp(14.0, 22.0);
        let thumb_x = rect.x + t * (rect.w - thumb_size);
        let thumb_y = rect.y + (rect.h - thumb_size) / 2.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [thumb_x, thumb_y, thumb_size, thumb_size],
            color: dim_alpha(accent_rgba, opacity),
            radii: [thumb_size / 2.0; 4],
            transform,
            smooth: 0.0,
        });
        let inner = thumb_size - 5.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [thumb_x + 2.5, thumb_y + 2.5, inner, inner],
            color: dim_alpha([1.0, 1.0, 1.0, 1.0], opacity),
            radii: [inner / 2.0; 4],
            transform,
            smooth: 0.0,
        });

        // Handlers: PointerDown + PointerDrag.
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let track_x = rect.x;
            let track_w = rect.w;
            let make_drag_action =
                |min: f64, max: f64, step: Option<f64>| -> super::events::Action {
                    Box::new(move |ctx, _| {
                        let pos = super::events::CURRENT_EVENT_POS.with(std::cell::Cell::get);
                        // Pixel positions are `f32`; widen before the value math so
                        // the stored value never carries `f32` rounding noise.
                        let t = ((f64::from(pos.0) - f64::from(track_x)) / f64::from(track_w))
                            .clamp(0.0, 1.0);
                        let raw = min + t * (max - min);
                        // Quantise so the value never carries more decimals than
                        // the step (or, with no step, a readable default), see
                        // `step_decimals`/`SLIDER_DEFAULT_DECIMALS`.
                        let val = match step {
                            Some(s) => round_to_decimals((raw / s).round() * s, step_decimals(s)),
                            None => round_to_decimals(raw, SLIDER_DEFAULT_DECIMALS),
                        };
                        ctx.write_signal(sig, Value::Float(val));
                    })
                };
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::PointerDown,
                make_drag_action(min_f, max_f, step_f),
            );
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::PointerDrag,
                make_drag_action(min_f, max_f, step_f),
            );
            // Change write-back (RFC-0003 E1), as the other value widgets
            // have: the way a value set from outside the pointer, an
            // assistive technology (RFC-0042), reaches the `var`.
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );
        }
    }
}
