//! The paint walk (RFC-0005, RFC-0011).
//!
//! After layout, each concrete node is walked once with its resolved rect, the
//! transform and opacity it inherits, and its scroll shift: it emits its
//! boxes, text, images, vectors, canvases, native views, navigation screens
//! and the rest to the frame, culls what a `ScrollView` has scrolled out of
//! view, and registers the hit regions and handlers on the router under its
//! ancestors' transform. The glass backdrop, the ripples and the decorated
//! box with its layered shadows are emitted from here too.

use super::canvas::{ShapeGroupSink, attr_span, transformed_rrect_outline};
use super::layout::{Concrete, Pools};
use super::navigation::NavKind;
use super::props::ShadowSpec;
use super::scroll::{
    PULL_THRESHOLD, ScrollAxis, ScrollTarget, SnapAlign, SnapMode, WindowSpec, scrolled_hit_rect,
};
use super::states::{resolve_state_attrs, viewport_holds};
use super::wiring::NativeTarget;
use super::{Interpreter, RenderNode, access};
use crate::interp::env::Value;
use crate::parser::ast::{Attr, AttrKind, StyleStateKind};
use crate::symbol::Symbol;

/// A shadow-only [`DecoratedBox`](byard_core::frame::DecoratedBox): the box's
/// geometry (rect/radii/transform) with a transparent fill and no border, so it
/// casts `sh` beneath the surface. Emitted per shadow (RFC-0011 layered shadows).
fn shadow_decorated(
    base: byard_core::BoxInstance,
    opacity: f32,
    sh: &ShadowSpec,
) -> byard_core::frame::DecoratedBox {
    byard_core::frame::DecoratedBox {
        base: byard_core::BoxInstance {
            color: [0.0; 4],
            ..base
        },
        border_width: 0.0,
        border_color: [0.0; 4],
        shadow_dx: sh.dx,
        shadow_dy: sh.dy,
        shadow_blur: sh.blur,
        shadow_spread: sh.spread,
        shadow_color: sh.color,
        opacity,
        // A shadow cast is geometry, not fill: the element's own ramp belongs to
        // its surface, never to the blurred silhouette beneath it.
        gradient: None,
        dirty: true,
    }
}

/// One live ripple ink reveal (RFC-0023): spawned on a press gesture over an
/// element whose resolved `ripple_active` is true, expanded and faded over
/// `duration_ms`, and retired once fully faded. The snapshot model mirrors the
/// RFC's `RippleInstance` reference: colour/duration/radius are resolved once
/// at the spawn (the ink is a launched entity, a later style change doesn't
/// recolour ink already in flight), while the element rect and clip radii are
/// re-read at emission each frame so the ink tracks a moving/resizing element.
#[derive(Clone, Copy)]
pub(super) struct ActiveRipple {
    /// The stable element index this ripple belongs to (the router's key).
    /// The press identity that spawned it is not kept here, the rising-edge
    /// latch ([`Interpreter::ripple_spawned`]) is the only reader of gesture
    /// identity, and a launched ripple's lifecycle is purely time-based.
    elem: u32,
    /// Tap point relative to the element rect's origin (RFC-0023 §1),
    /// relative, so the ink stays anchored if layout shifts the element.
    center_rel: [f32; 2],
    /// Ink colour, resolved at spawn.
    color: [f32; 4],
    /// Engine time at activation.
    pub(super) start_ms: u32,
    /// Total expand/fade duration in ms (`ripple_duration`, default 300).
    pub(super) duration_ms: f32,
    /// `ripple_radius` override; `None` auto-expands to the farthest corner.
    max_radius: Option<f32>,
}

/// Default ripple fade-out duration in ms (RFC-0023 §"Ripple properties").
const RIPPLE_DEFAULT_DURATION_MS: f32 = 300.0;

/// Default `blur_saturation` boost (RFC-0023 §"Blur properties"): the iOS
/// vibrancy look.
const BLUR_DEFAULT_SATURATION: f32 = 1.8;

pub(super) fn dim_alpha(mut color: [f32; 4], opacity: f32) -> [f32; 4] {
    color[3] *= opacity;
    color
}

/// Pushes a plain fill onto the pass its alpha belongs to: an opaque one onto
/// `SolidBox`, which writes depth and occludes, and a translucent one onto the
/// blended decorated pass, which only tests it. A translucent fill on the solid
/// pass would stamp a nearer depth over its whole rect and cull whatever later
/// passes draw beneath it.
pub(super) fn push_fill(frame: &mut byard_core::frame::RenderFrame, fill: byard_core::BoxInstance) {
    if fill.color[3] < 1.0 {
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: fill,
            opacity: 1.0,
            dirty: true,
            ..Default::default()
        });
    } else {
        frame.push_instance(fill);
    }
}

impl Interpreter {
    /// Renders one node, with its hit regions registered under its
    /// ancestors' paint transform (RFC-0011 hierarchical transforms).
    ///
    /// A wrapper rather than a line at the top of the body because the body has
    /// a dozen return paths, and a hit frame that leaked from one node into its
    /// next sibling would be the kind of bug that only shows under a rotated
    /// parent. Set on the way in and restored on the way out, unconditionally.
    ///
    /// The frame is the inherited transform **minus the scroll displacement**:
    /// scrolling already reaches every hit rect through `scroll_shift`, and
    /// applying it a second time through the transform would put a scrolled
    /// button's target one scroll offset away from the button.
    #[allow(clippy::too_many_arguments, clippy::similar_names)]
    pub(super) fn render_node_with_atlas(
        &mut self,
        node: &RenderNode,
        atlas_node: byard_core::atlas::layout::AtlasNodeId,
        frame: &mut byard_core::frame::RenderFrame,
        flat_ids: &[byard_core::atlas::layout::AtlasNodeId],
        flat_idx: &mut usize,
        parent_rect: crate::interp::intrinsics::Rect,
        inherited_opacity: f32,
        inherited_transform: byard_core::frame::Transform,
        cull_clip: Option<byard_core::frame::Rect>,
        scroll_shift: (f32, f32),
        window: Option<WindowSpec>,
        pools: Pools<'_>,
    ) {
        let mut hit = inherited_transform;
        hit.translate[0] -= scroll_shift.0;
        hit.translate[1] -= scroll_shift.1;
        // Opacity decides nothing about where a press lands.
        hit.opacity = 1.0;
        let previous = self.router.set_hit_frame(hit);
        self.render_node_with_atlas_inner(
            node,
            atlas_node,
            frame,
            flat_ids,
            flat_idx,
            parent_rect,
            inherited_opacity,
            inherited_transform,
            cull_clip,
            scroll_shift,
            window,
            pools,
        );
        self.router.restore_hit_frame(previous);
    }

    /// The body of [`render_node_with_atlas`](Self::render_node_with_atlas),
    /// which owns the hit-frame bookkeeping around it.
    #[allow(clippy::similar_names)]
    #[allow(clippy::too_many_arguments)]
    fn render_node_with_atlas_inner(
        &mut self,
        node: &RenderNode,
        atlas_node: byard_core::atlas::layout::AtlasNodeId,
        frame: &mut byard_core::frame::RenderFrame,
        flat_ids: &[byard_core::atlas::layout::AtlasNodeId],
        flat_idx: &mut usize,
        parent_rect: crate::interp::intrinsics::Rect,
        // Opacity inherited from ancestors (RFC-0011 T4 approximation): folded
        // into this element's own `opacity` and multiplied into the alpha of
        // every primitive it emits, so a translucent parent dims its text and
        // widgets too, not only its own background.
        inherited_opacity: f32,
        // Paint-time transform inherited from ancestors (RFC-0011 group
        // transforms): composed with this element's own transform so a scaled or
        // translated container carries its children, text, and widgets with it,
        // not only its own background box. `IDENTITY` at the root.
        inherited_transform: byard_core::frame::Transform,
        // The nearest enclosing `ScrollView` viewport, in screen space (RFC-0005
        // emission culling). A node whose scroll-shifted rect falls entirely
        // outside it is skipped, the scissor already hides such fragments, so
        // this only spares the CPU the emission. `None` outside any scroll
        // container (the whole viewport is live).
        cull_clip: Option<byard_core::frame::Rect>,
        // Accumulated scroll displacement from every enclosing `ScrollView`
        // (RFC-0005), in screen px. Paint applies it through the inherited
        // transform; **hit-testing** cannot ride that path, RFC-0011
        // deliberately keeps paint transforms out of hit rects (a hover-scale
        // must not move its own hit target), so the scroll displacement
        // travels separately and shifts every hit rect registered inside the
        // scrolled subtree. This is what keeps a scrolled button interactive
        // at its *on-screen* position, not its laid-out one.
        scroll_shift: (f32, f32),
        // Set only on a windowed `ScrollView`'s list child (RFC-0005 windowed
        // layout): this node renders just rows `start..end`, bracketed by the two
        // spacer leaves the build pass emitted, so the flat-id cursor stays
        // aligned. `None` everywhere else, the ordinary full child walk.
        window: Option<WindowSpec>,
        // RFC-0018: the reactive `for` pools, for expanding `when`/`for` children.
        pools: Pools<'_>,
    ) {
        debug_assert_eq!(flat_ids[*flat_idx], atlas_node);
        *flat_idx += 1;

        match node {
            // Reactive `when`/`for` are expanded to concrete children before the
            // walk reaches them (RFC-0018), so they never arrive as a paint node.
            RenderNode::When { .. } | RenderNode::For { .. } => {
                unreachable!("when/for are expanded before render_node_with_atlas")
            }
            RenderNode::Effect { .. } => {
                unreachable!("effects are dropped by expand_concrete before the paint walk")
            }
            // A `Spacer` is layout-only. An `Overlay` renders nothing in the main
            // flow, its 0×0 leaf holds a slot in the flat-id cursor (already
            // advanced above) while its children are emitted separately in the
            // deferred overlay phase (RFC-0017).
            RenderNode::Spacer { .. } | RenderNode::Overlay { .. } => {}
            RenderNode::Text {
                attrs,
                state_blocks,
                content,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    // RFC-0016: overlay any active `on <state>` block against the
                    // live engine mask before reading paint properties. RFC-0024:
                    // fold in the universal `selected:`/`invalid:` states.
                    let elem_idx = self.atlas.node_index(atlas_node);
                    let state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
                    let attrs = attrs.as_ref();
                    let text = match self.binding_value(*content) {
                        Some(Value::Str(s)) => s,
                        other => other.map_or_else(String::new, |v| format!("{v:?}")),
                    };
                    // Fall back to theme on-surface color when unset.
                    let color = self
                        .eval_color_prop(attrs, "color")
                        .unwrap_or(self.theme.on_surface());
                    // Resolve `typo:` token to font size; inline `size:` overrides
                    // (RFC-0005 `Typo`, completed by RFC-0022).
                    let typo_size = self.eval_typo_size(attrs);
                    let size =
                        self.eval_int_prop(attrs, "size")
                            .or(typo_size)
                            .unwrap_or(self.theme.font_size as i64) as f32;
                    let mut rgba = super::intrinsics::color_rgba_auto(color);
                    rgba[3] *= inherited_opacity;
                    // RFC-0011: the run's own transform, composed with its
                    // ancestors'. Scale and translate are baked into the baseline
                    // anchor and the font size (glyph extents scale from the
                    // anchor, so this scales the run about the pivot), which
                    // keeps it shaped at its drawn size and sharp. Rotation is
                    // not something glyphon can draw, so a rotated run of its
                    // own opens a rotated group: the frame stores the run
                    // upright and the composite turns it. Inside a rotated
                    // ancestor, that ancestor's group already turns it.
                    let own = self.resolve_transform(
                        attrs,
                        crate::interp::intrinsics::Rect::new(
                            rect.x,
                            rect.y,
                            rect.width,
                            rect.height,
                        ),
                    );
                    let transform = inherited_transform.compose(&own);
                    let own_group = own.rotate.abs() > f32::EPSILON
                        && frame.begin_group_rotated(
                            1.0,
                            transform.rotate,
                            [
                                transform.origin[0] + transform.translate[0],
                                transform.origin[1] + transform.translate[1],
                            ],
                        );
                    if self.access.on {
                        self.access_text(
                            frame,
                            attrs,
                            elem_idx,
                            &text,
                            crate::interp::intrinsics::Rect::new(
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ),
                            &transform,
                        );
                    }
                    let anchor = transform.apply_point([rect.x, rect.y]);
                    let scaled_size = size * transform.uniform_scale();
                    // RFC-0005 default text wrap: shape the run to the width layout
                    // resolved for this leaf (its parent-offered width), scaled by
                    // any ancestor scale (the run's glyphs scale about the pivot).
                    // `wrap: false` opts out to a single-line run. This mirrors the
                    // atlas's measure pass, so the rendered line breaks match the
                    // laid-out height.
                    let weight = self.resolve_weight(attrs);
                    let family = self.resolve_family(attrs);
                    let wrap_w = if self.eval_bool_prop(attrs, "wrap") == Some(false) {
                        None
                    } else {
                        Some(rect.width * transform.uniform_scale())
                    };
                    frame.push_text_wrapped(
                        byard_core::TextLine {
                            x: anchor[0],
                            y: anchor[1],
                            text,
                            font_size: scaled_size,
                            weight,
                            family,
                            color: rgba,
                            dirty: true,
                        },
                        wrap_w,
                    );
                    if own_group {
                        frame.end_group();
                    }

                    let has_events = attrs
                        .iter()
                        .any(|a| matches!(a.kind, AttrKind::Event { .. }));
                    if has_events {
                        let self_rect = crate::interp::intrinsics::Rect::new(
                            rect.x,
                            rect.y,
                            rect.width,
                            rect.height,
                        );
                        let hit_rect = scrolled_hit_rect(
                            crate::interp::intrinsics::inflate_hit_rect(self_rect, parent_rect),
                            scroll_shift,
                            cull_clip,
                        );
                        self.register_event_attrs(attrs, hit_rect, elem_idx);
                    }
                }
            }
            RenderNode::Box {
                name,
                attrs,
                state_blocks,
                children,
                action,
                bound_sig,
                env_snapshot,
                anchor_name,
                measure: _,
                clip_path,
            } => {
                // RFC-0019 §2: restore the instance environment captured at lower
                // time so event actions re-lowered below (a forwarded callback,
                // or any param reference in `attrs`) resolve against the scope
                // this box was instantiated in. Empty at the top level (no-op).
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let mut current_rect = parent_rect;
                // Opacity children inherit from this box: its effective opacity
                // when it has a resolved rect (set below), else whatever it
                // inherited unchanged.
                let mut child_opacity = inherited_opacity;
                // Whether this box opened an opacity group (RFC-0011 T4), so the
                // group is closed on the way out of exactly the box that opened it.
                let mut grouped = false;
                // The accessibility scope this box opened (RFC-0042), closed on
                // the way out with the node it described.
                let mut access_scope = access::Scope::default();
                // Likewise the composed paint transform children inherit (RFC-0011
                // group transforms): this box's own transform ∘ its ancestors',
                // set once the rect is known, else passed through unchanged.
                let mut child_transform = inherited_transform;
                // And the accumulated scroll displacement (RFC-0005): grown by
                // this box when it is a `ScrollView`, passed through otherwise.
                let mut child_scroll_shift = scroll_shift;
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    current_rect = crate::interp::intrinsics::Rect::new(
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                    );
                    // RFC-0036: this element is an anchor, so record where it
                    // landed. Recorded from the *painted* rect, transform and
                    // scroll shift included, because an overlay has to point at
                    // where the user sees the element, not where it would have
                    // been without them.
                    if let Some(name) = anchor_name {
                        let tl = inherited_transform.apply_point([rect.x, rect.y]);
                        self.anchor_rects.insert(
                            name.clone(),
                            crate::interp::intrinsics::Rect::new(
                                tl[0] + scroll_shift.0,
                                tl[1] + scroll_shift.1,
                                rect.width * inherited_transform.scale[0],
                                rect.height * inherited_transform.scale[1],
                            ),
                        );
                    }
                    let elem_idx = self.atlas.node_index(atlas_node);
                    // RFC-0012 S5: a `disabled:` element still lays out and paints,
                    // but the router gates every handler it registers below and
                    // reports the `DISABLED` interaction state. Marked here, before
                    // resolving state styles, so an `on disabled { … }` block takes
                    // effect on the very frame the element becomes disabled.
                    if self.eval_bool_prop(attrs, "disabled") == Some(true) {
                        if let Some(idx) = elem_idx {
                            self.router.set_disabled(idx);
                        }
                    }
                    // RFC-0016: overlay any active `on <state>` block over the
                    // base attributes against the live engine `StyleState` mask
                    // *before* reading paint properties. Stateless boxes borrow
                    // `attrs` unchanged (no clone). The base `attrs` still drive
                    // event/handler registration below so hit targets are stable.
                    // RFC-0024: fold the prop/value-driven states (checked,
                    // selected, invalid, indeterminate) into the router's
                    // pointer/focus/drag mask so `on checked { … }` etc. resolve.
                    let paint_state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, *bound_sig, name.as_str()));
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, paint_state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
                    let paint_attrs = paint_attrs.as_ref();
                    // Resolve the paint-time transform once, up front, so it can
                    // be applied both to a plain container's `bg` fill *and* to
                    // the self-owned visuals of `Toggle`/`Slider`/`TextField`
                    // (their track/fill/thumb/underline/caret are the element's
                    // own quads, so RFC-0011's element-local transform applies to
                    // them exactly as it does to a `Box` fill).
                    // The element's own transform, then composed with the one
                    // inherited from its ancestors (RFC-0011 group transforms) so
                    // this box's fill, its widget visuals, its children, and its
                    // text all move/scale as a group. Passed on to children below.
                    let own_transform = self.resolve_transform(paint_attrs, current_rect);
                    let transform = inherited_transform.compose(&own_transform);
                    child_transform = transform;
                    let bg = self.eval_color_prop(paint_attrs, "bg");
                    let radii = self.resolve_radii(paint_attrs, "radius");
                    // RFC-0031 §S1: the corner profile `radii` are measured
                    // with. Paint-class, so it never touches layout, and it
                    // reaches the fill, the border, every shadow, the backdrop
                    // pane and the ripple clip from this one read, §Q2's
                    // "a shadow with a different corner profile than its caster
                    // reads as a rendering bug", applied to the whole element.
                    let smooth = self.resolve_smooth(paint_attrs);
                    // `border` is a Color (catalog DECORATION); a present border
                    // draws a 2px ring of that colour.
                    let border_color = self.eval_color_prop(paint_attrs, "border");
                    // `border_width` is an animatable paint prop (RFC-0010): it
                    // resolves through `eval_pure`, so `border_width: n with
                    // anim.*` interpolates like any other scalar. Defaults to 2px
                    // when a border colour is present, 0 when there is no border.
                    let border_width = if border_color.is_some() {
                        self.eval_float_prop(paint_attrs, "border_width")
                            .map_or(2.0, |v| v as f32)
                    } else {
                        0.0
                    };
                    // `shadow` is a token (`sm`/`md`/`lg`) → an offset+blur drop
                    // shadow; any other non-empty value falls back to `md`.
                    // `shadow` (RFC-0011 custom shadows): a preset token, a
                    // single (named/positional) tuple, or an array of tuples for
                    // CSS-style layered shadows. Each becomes its own shadow-only
                    // decorated box beneath the surface.
                    let shadows = self.resolve_shadows(paint_attrs);
                    // The element's *effective* opacity: its own `opacity` prop
                    // folded with whatever it inherited (RFC-0011 T4). Used for
                    // this box's own fill and passed down so children (a Button's
                    // label, a widget's visuals) dim with it.
                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(paint_attrs, "opacity")
                            .map_or(1.0, |v| v as f32);
                    // RFC-0011 T4: a translucent box with children is faded as
                    // one picture rather than by multiplying its alpha into
                    // every primitive it contains. The difference is overlap:
                    // its text over its own background, two children crossing,
                    // each darkening the other where they meet. Inside the
                    // group everything draws opaque and the composite applies
                    // the alpha once. A leaf has nothing to overlap, and a box
                    // inside an open group falls back to per-instance, so both
                    // keep the path they always had.
                    //
                    // RFC-0011 text transforms: a rotated box with children is
                    // grouped too. Its subtree is stored upright and the
                    // composite turns the picture about the transform's pivot,
                    // which is how its text, which glyphon cannot rotate, turns
                    // with it. Scale and translate stay per primitive, where
                    // text is already shaped at its scaled size, so a rotated
                    // label is as sharp as an upright one of the same size.
                    let rotated = transform.rotate.abs() > f32::EPSILON;
                    let pivot = [
                        transform.origin[0] + transform.translate[0],
                        transform.origin[1] + transform.translate[1],
                    ];
                    grouped = ((opacity - 1.0).abs() > f32::EPSILON || rotated)
                        && !children.is_empty()
                        && frame.begin_group_rotated(
                            opacity,
                            if rotated { transform.rotate } else { 0.0 },
                            pivot,
                        );
                    let opacity = if grouped { 1.0 } else { opacity };
                    child_opacity = opacity;
                    // An 8-digit `bg` carries its own alpha byte (RFC-0005 §1).
                    let bg_rgba = bg.map_or([0.0; 4], super::intrinsics::color_rgba_auto);
                    // A background below full alpha is as translucent as a
                    // faded box: on the depth-writing SolidBox pass it would
                    // cull whatever later passes draw beneath it.
                    let translucent =
                        (opacity - 1.0).abs() > f32::EPSILON || (bg.is_some() && bg_rgba[3] < 1.0);
                    // RFC-0001 §3.1: a gradient is a `DecoratedBox` feature, so
                    // its presence promotes the box off the flat SolidBox path
                    // exactly as a border/shadow/opacity does.
                    let gradient = self.resolve_gradient(paint_attrs);
                    // `Toggle`/`Slider` own their visuals (track/fill/thumb) and
                    // treat `bg` as the *accent* colour, not a full-rect fill,
                    // painting the rect here would draw a slab behind the control.
                    let owns_visuals = matches!(
                        name.as_str(),
                        "Toggle" | "Slider" | "Checkbox" | "RadioButton"
                    );
                    // A gradient is a fill in its own right, so a box with a ramp
                    // and no `bg` still has a surface to paint (the ramp over a
                    // transparent base).
                    if !owns_visuals && (bg.is_some() || gradient.is_some()) {
                        let base = byard_core::BoxInstance {
                            rect: [rect.x, rect.y, rect.width, rect.height],
                            color: bg_rgba,
                            radii,
                            transform,
                            smooth,
                        };
                        let border_rgba =
                            border_color.map_or([0.0; 4], super::intrinsics::color_rgba_auto);
                        // Cast the shadows first so they sit *beneath* the fill.
                        // Reversed: first-listed is pushed last → nearest z → on
                        // top of later shadows (CSS box-shadow order), all still
                        // behind the surface pushed after them.
                        for sh in shadows.iter().rev() {
                            frame.push_decorated(shadow_decorated(base, opacity, sh));
                        }
                        if translucent || gradient.is_some() {
                            // A translucent or gradient-filled box blends its fill
                            // as one unit on the decorated pipeline; keep it whole.
                            frame.push_decorated(byard_core::frame::DecoratedBox {
                                base,
                                border_width,
                                border_color: border_rgba,
                                opacity,
                                gradient,
                                // Re-walked and re-emitted every tick;
                                // mirror Text's always-dirty lowering.
                                dirty: true,
                                ..Default::default()
                            });
                        } else if border_color.is_some() {
                            // Paint the opaque fill on the SolidBox pass so it stays
                            // *behind* this container's children (they also paint as
                            // solids, pushed after it, and the decorated pass runs
                            // after every solid). Then add the border as a decorated
                            // overlay whose interior is transparent: it only strokes
                            // the edge, so it can never occlude the children drawn
                            // beneath it (fixes the parent-card-over-child-widget
                            // z-order bug).
                            frame.push_instance(base);
                            frame.push_decorated(byard_core::frame::DecoratedBox {
                                base: byard_core::BoxInstance {
                                    color: [0.0; 4],
                                    ..base
                                },
                                border_width,
                                border_color: border_rgba,
                                opacity: 1.0,
                                dirty: true,
                                ..Default::default()
                            });
                        } else {
                            frame.push_instance(base);
                        }
                    }

                    // RFC-0023 §2: backdrop blur, emitted right after this
                    // element's background (the §4 compositing slot), so the
                    // pane samples everything painted behind it, its own
                    // background included, and its children render on top.
                    self.emit_backdrop(
                        paint_attrs,
                        current_rect,
                        radii,
                        smooth,
                        transform,
                        opacity,
                        frame,
                    );

                    // RFC-0023: ripple ink, emitted after this element's
                    // background and before its children, which stamps its
                    // draw-order depth into exactly the RFC's compositing slot
                    // (background → ripple → children).
                    self.emit_ripples(
                        paint_attrs,
                        elem_idx,
                        current_rect,
                        radii,
                        smooth,
                        transform,
                        opacity,
                        scroll_shift,
                        frame,
                    );

                    let element_name = name.as_str();
                    // Inflate to the 44×44 slop, then move the target to where
                    // the content actually is on screen (RFC-0005 scroll shift)
                    // and clip it to the scroll viewport.
                    let hit_rect = scrolled_hit_rect(
                        crate::interp::intrinsics::inflate_hit_rect(current_rect, parent_rect),
                        scroll_shift,
                        cull_clip,
                    );

                    // RFC-0016: an element that styles `on hover`/`on pressed` but
                    // registers no handler still needs the engine to track the
                    // pointer over it, so register a bare hover/press hit region.
                    // RFC-0023: same for a `ripple:` element, the press gesture
                    // must resolve to this element for the ink to spawn, even
                    // when it registers no handler of its own.
                    if let Some(idx) = elem_idx {
                        let tracks_pointer_states = state_blocks.iter().any(|sb| {
                            sb.states.iter().any(|s| {
                                matches!(s, StyleStateKind::Hover | StyleStateKind::Pressed)
                            })
                        });
                        let has_ripple = paint_attrs.iter().any(|a| a.name.as_str() == "ripple");
                        if tracks_pointer_states || has_ripple {
                            self.router.track_region(idx, hit_rect);
                        }
                    }

                    // ── Widget-specific visual lowering & handler registration ──
                    match element_name {
                        "Toggle" => {
                            self.render_toggle(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "Slider" => {
                            self.render_slider(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "Checkbox" => {
                            self.render_checkbox(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "RadioButton" => {
                            self.render_radio(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "TextField" => {
                            self.render_text_field(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        _ => {
                            // General interactive elements: register event-attr handlers.
                            let has_event_attrs = attrs
                                .iter()
                                .any(|a| matches!(a.kind, AttrKind::Event { .. }));
                            let is_interactive = matches!(element_name, "Button")
                                || has_event_attrs
                                || action.is_some();

                            if is_interactive {
                                self.register_event_attrs(attrs, hit_rect, elem_idx);

                                if let Some(action_expr) = action {
                                    if let Ok(action_closure) = self.lower_action(action_expr, None)
                                    {
                                        if let Some(idx) = elem_idx {
                                            self.router.on(
                                                idx,
                                                hit_rect,
                                                crate::interp::events::EventKind::Tap,
                                                action_closure,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // ── `focused:` reflected prop → register as focusable ──
                    // TextField, Checkbox, and RadioButton register their own
                    // focusable inside their render fns (they are focusable *by
                    // default*, RFC-0018), so exclude them here to avoid
                    // double-registration.
                    if !matches!(element_name, "TextField" | "Checkbox" | "RadioButton") {
                        self.register_focusable(attrs, hit_rect, elem_idx);
                    }

                    // RFC-0042: describe the box now that its handlers and
                    // focus target are registered, which is what its actions
                    // and state are read from.
                    if self.access.on {
                        let tapped = elem_idx.is_some_and(|i| {
                            self.router
                                .has_handler(i, crate::interp::events::EventKind::Tap)
                        });
                        access_scope = self.access_enter_box(
                            frame,
                            &access::BoxFacts {
                                name: element_name,
                                attrs,
                                paint_attrs,
                                bound_sig: *bound_sig,
                                tapped,
                                elem: elem_idx,
                                rect: current_rect,
                                atlas_node,
                                transform,
                            },
                        );
                    }
                }
                // RFC-0021 collapsing header: when set, the first child is pinned
                // to the viewport top by this many screen px (undoing the scroll
                // translate), so it stays put while the content scrolls under it.
                let mut header_pin: Option<f32> = None;
                // RFC-0005 `ScrollView`: clip children to this viewport and
                // translate the content by `−offset`. The overflow is scissored
                // by the encoder (an off-viewport child costs no fragments), and
                // the content was measured unbounded (layout `scroll`). `offset`
                // is a two-way `Vec2` the app can read or drive on either axis;
                // wheel and drag write it below. Rotation of a scroll viewport is
                // out of scope (the clip is an axis-aligned screen rect).
                // RFC-0037 clip masks: `Clip #[rrect: r]` clips its subtree
                // to its own laid-out box. A zero (or absent) radius is a
                // plain rectangular clip, which costs exactly what a
                // `ScrollView`'s does — a scissor — so wrapping content in a
                // square `Clip` is not a new expense.
                // Clips are cut where their content is stored: inside a rotated
                // group that is upright, and the composite turns clip and content
                // together (RFC-0011 text transforms).
                let clip_transform = frame.upright(inherited_transform);
                let mask_clip = if name.as_str() == "Clip" && clip_transform.rotate != 0.0 {
                    // RFC-0011: under a rotated ancestor the clip's outline is
                    // a rotated rectangle, which neither the scissor nor an
                    // axis-aligned clip entry can express. It becomes a path
                    // mask instead, so the content is cut along the edges it
                    // is drawn with. Its own `path` child, if it has one, nests
                    // inside that as it would anywhere else.
                    let mut radii = self.resolve_radii(attrs, "rrect");
                    for r in &mut radii {
                        *r *= clip_transform.scale[0];
                    }
                    let outline = transformed_rrect_outline(current_rect, radii, &clip_transform);
                    let opened = self.begin_clip_commands(&outline, frame);
                    let pathed = clip_path.as_ref().is_some_and(|p| {
                        let tl = clip_transform.apply_point([current_rect.x, current_rect.y]);
                        let rect = byard_core::frame::Rect::new(
                            tl[0],
                            tl[1],
                            current_rect.w * clip_transform.scale[0],
                            current_rect.h * clip_transform.scale[1],
                        );
                        self.begin_clip_path_mask(p, rect, frame)
                    });
                    match (opened, pathed) {
                        (true, pathed) => Some(pathed),
                        (false, true) => Some(false),
                        (false, false) => None,
                    }
                } else if name.as_str() == "Clip" {
                    let tl = clip_transform.apply_point([current_rect.x, current_rect.y]);
                    let rect = byard_core::frame::Rect::new(
                        tl[0],
                        tl[1],
                        current_rect.w * clip_transform.scale[0],
                        current_rect.h * clip_transform.scale[1],
                    );
                    // Read the same way a box's `radius` is, so `rrect: 16`
                    // and `rrect: (16, 0, 0, 16)` both mean here what they
                    // already mean there. The radius scales with the subtree,
                    // as a decorated box's corners do: a scaled card's clip
                    // has to keep matching the card.
                    let mut radii = self.resolve_radii(attrs, "rrect");
                    for r in &mut radii {
                        *r *= clip_transform.scale[0];
                    }
                    frame.begin_clip_rounded(rect, radii);
                    // RFC-0037 `clip(path)`: the mask opens *inside* the
                    // rounded clip rather than replacing it, so the two
                    // coverages multiply and `rrect` still means what it says
                    // on a clip that also has a path. Two entries, closed in
                    // reverse below.
                    let pathed = clip_path
                        .as_ref()
                        .is_some_and(|p| self.begin_clip_path_mask(p, rect, frame));
                    Some(pathed)
                } else {
                    None
                };
                let scroll_clip = if name.as_str() == "ScrollView" {
                    let (ox, oy) = self.resolve_axis_pair(attrs, "offset", (0.0, 0.0));
                    let tl = clip_transform.apply_point([current_rect.x, current_rect.y]);
                    let clip = byard_core::frame::Rect::new(
                        tl[0],
                        tl[1],
                        current_rect.w * clip_transform.scale[0],
                        current_rect.h * clip_transform.scale[1],
                    );
                    frame.begin_clip(clip);
                    child_transform.translate[0] -= ox * inherited_transform.scale[0];
                    child_transform.translate[1] -= oy * inherited_transform.scale[1];
                    // Hit rects travel with the content (paint rides the
                    // transform above; hit-testing rides this, see the
                    // `scroll_shift` parameter docs).
                    child_scroll_shift.0 -= ox * inherited_transform.scale[0];
                    child_scroll_shift.1 -= oy * inherited_transform.scale[1];
                    // RFC-0021 pull-to-refresh: shift the content down by the pull
                    // region and draw the default indicator in the revealed gap.
                    let pull_elem = self.atlas.node_index(atlas_node);
                    let pull = pull_elem
                        .and_then(|e| self.pull_distance.get(&e).copied())
                        .unwrap_or(0.0);
                    if pull > 0.0 {
                        let gap_px = pull * inherited_transform.scale[1];
                        child_transform.translate[1] += gap_px;
                        child_scroll_shift.1 += gap_px;
                        let progress = (pull / PULL_THRESHOLD).clamp(0.0, 1.0);
                        let active = pull_elem
                            .and_then(|e| self.refreshing_seen.get(&e).copied())
                            .unwrap_or(false);
                        Self::push_pull_indicator(frame, clip, gap_px, progress, active);
                    }
                    // RFC-0021 collapsing header: drive `scroll_fraction` (0 →
                    // expanded, 1 → collapsed) from the vertical scroll over the
                    // header's collapsible range (its natural height minus
                    // `collapse_min`), and pin the header to the viewport top so it
                    // stays while the content scrolls under it. The header keeps its
                    // laid-out height; its descendants read `scroll_fraction` to
                    // interpolate their own size/opacity (RFC-0021 §4 mechanism).
                    if let Some(frac_sig) = *bound_sig {
                        let h_max = self
                            .atlas
                            .children(atlas_node)
                            .first()
                            .and_then(|h| self.atlas.resolved_rect(*h).ok().flatten())
                            .map_or(0.0, |r| r.height);
                        #[allow(clippy::cast_possible_truncation)]
                        let c_min = self
                            .eval_float_prop(attrs, "collapse_min")
                            .map_or(56.0, |v| v as f32);
                        let dist = (h_max - c_min).max(1.0);
                        let frac = (oy / dist).clamp(0.0, 1.0);
                        if !matches!(
                            self.ctx.peek_signal(frac_sig),
                            Value::Float(f) if (f - f64::from(frac)).abs() < 1e-4
                        ) {
                            self.ctx
                                .write_signal(frac_sig, Value::Float(f64::from(frac)));
                        }
                        header_pin = Some(oy * inherited_transform.scale[1]);
                    }
                    // Record a wheel/drag scroll target for whichever of
                    // `offset.x`/`offset.y` is a writable signal (e.g.
                    // `offset: (panX, scrollY)`): the input scrolls by writing it,
                    // clamped to the content extent. `dispatch_events` consumes
                    // these next tick (render-then-dispatch handshake).
                    let (sig_x, sig_y) = self.resolve_offset_sigs(attrs);
                    // A `pull_refresh` view needs a target even with no `offset`
                    // var, its pull region is engine state, driven by the drag.
                    let pull_refresh = self.eval_bool_prop(attrs, "pull_refresh").unwrap_or(false);
                    if sig_x.is_some() || sig_y.is_some() || pull_refresh {
                        let (content_w, content_h) = self
                            .atlas
                            .content_size(atlas_node)
                            .ok()
                            .flatten()
                            .unwrap_or((current_rect.w, current_rect.h));
                        let refreshing_sig = self.resolve_named_var_sig(attrs, "refreshing");
                        // RFC-0021 behaviours resolved from props.
                        let snap = match Self::enum_prop(attrs, "snap") {
                            Some("page") => SnapMode::Page,
                            Some("item") => SnapMode::Item,
                            _ => SnapMode::None,
                        };
                        let snap_align = match Self::enum_prop(attrs, "snap_align") {
                            Some("center") => SnapAlign::Center,
                            Some("end") => SnapAlign::End,
                            _ => SnapAlign::Start,
                        };
                        let snap_spring = self.resolve_snap_spring(attrs);
                        let page_sig = self.resolve_named_var_sig(attrs, "page");
                        let has_end = attrs.iter().any(|a| {
                            a.name.as_str() == "end_reached"
                                && matches!(a.kind, AttrKind::Event { .. })
                        });
                        #[allow(clippy::cast_possible_truncation)]
                        let end_threshold = has_end.then(|| {
                            self.eval_float_prop(attrs, "end_threshold")
                                .map_or(0.8, |v| v as f32)
                        });
                        let sv_elem = self.atlas.node_index(atlas_node);
                        let x_max = (content_w - current_rect.w).max(0.0);
                        let y_max = (content_h - current_rect.h).max(0.0);
                        // RFC-0021 `snap: item`: precompute the rest offset for each
                        // direct child on the scrolling axis (vertical preferred,
                        // matching `scrollable_axis`), aligned per `snap_align`.
                        if let (Some(elem), SnapMode::Item) = (sv_elem, snap) {
                            let horizontal = y_max <= 0.0 && x_max > 0.0;
                            let (vp, axis_max) = if horizontal {
                                (current_rect.w, x_max)
                            } else {
                                (current_rect.h, y_max)
                            };
                            let bounds = self.item_snap_offsets(
                                atlas_node, horizontal, vp, axis_max, snap_align,
                            );
                            self.scroll_item_bounds.insert(elem, bounds);
                        }
                        self.scroll_targets.push(ScrollTarget {
                            rect: crate::interp::intrinsics::Rect::new(
                                clip.x,
                                clip.y,
                                clip.width,
                                clip.height,
                            ),
                            x: sig_x.map(|sig| ScrollAxis { sig, max: x_max }),
                            y: sig_y.map(|sig| ScrollAxis { sig, max: y_max }),
                            elem: sv_elem,
                            snap,
                            snap_spring,
                            page_sig,
                            end_threshold,
                            pull_refresh,
                            refreshing_sig,
                        });
                    }
                    Some(clip)
                } else {
                    None
                };
                // Children cull against this box's own scroll viewport when it is
                // a `ScrollView`, otherwise against whatever viewport an ancestor
                // `ScrollView` established, so rows nested under an inner `Column`
                // are culled too, not just the `ScrollView`'s direct child.
                let child_clip = scroll_clip.or(cull_clip);
                // A windowed `ScrollView` hands its computed row window to its
                // single list child (mirrors the build pass); nothing else
                // propagates one.
                let child_window = if name.as_str() == "ScrollView" {
                    match children.as_slice() {
                        [RenderNode::Box { children: rows, .. }] => {
                            // RFC-0018: count the expanded rows (a reactive `for`
                            // is one node that expands to N), mirroring the build.
                            let n = self.expand_concrete(rows, pools).len();
                            self.scroll_window(attrs, n)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let render_child = |this: &mut Self,
                                    child: Concrete<'_>,
                                    frame: &mut byard_core::frame::RenderFrame,
                                    flat_idx: &mut usize| {
                    // The instance this child belongs to, so every `with`
                    // it evaluates keys to its own row rather than to the
                    // written span shared by all of them.
                    this.anim_slot = child.slot;
                    let child = child.node;
                    let child_id = flat_ids[*flat_idx];
                    // RFC-0005 emission culling (north star): a child the
                    // scroll has pushed entirely out of the viewport is never
                    // pushed to the frame, a long list costs only its visible
                    // slice. Advance the cursor past the skipped subtree so the
                    // remaining children stay aligned.
                    if let Some(clip) = child_clip {
                        if this.child_fully_clipped(child_id, child_transform, clip) {
                            // RFC-0042: its words still name the button it is in.
                            this.access_culled(child, pools);
                            *flat_idx += this.flat_len(child, pools);
                            return;
                        }
                    }
                    this.render_node_with_atlas(
                        child,
                        child_id,
                        frame,
                        flat_ids,
                        flat_idx,
                        current_rect,
                        child_opacity,
                        child_transform,
                        child_clip,
                        child_scroll_shift,
                        child_window,
                        pools,
                    );
                };
                if let Some(win) = window {
                    // This box is a windowed list child (RFC-0005): the build pass
                    // wrapped its rows in a leading + trailing spacer leaf. Consume
                    // the leading spacer, render only rows `start..end`, then the
                    // trailing spacer, keeping the flat-id cursor in lockstep.
                    *flat_idx += 1;
                    // RFC-0018: expand a reactive `for` (or literal rows) and
                    // paint only the windowed slice, mirroring the build pass.
                    let rows = self.expand_concrete(children, pools);
                    for &row in &rows[win.start..win.end] {
                        render_child(self, row, frame, flat_idx);
                    }
                    *flat_idx += 1;
                } else if let Some(pin) = header_pin {
                    // RFC-0021 collapsing header: the first child (the header) is
                    // pinned to the viewport top (its scroll translate undone on Y)
                    // *and* drawn last so it paints on top of the content that
                    // scrolls up under it, draw-order depth is emission order, so a
                    // header emitted first would sit behind the content. Skip the
                    // header's flat ids, paint the rest, then rewind and paint the
                    // header over them.
                    let concrete = self.expand_concrete(children, pools);
                    if let Some((&header, rest)) = concrete.split_first() {
                        let header_start = *flat_idx;
                        *flat_idx += self.flat_len(header.node, pools);
                        for &child in rest {
                            render_child(self, child, frame, flat_idx);
                        }
                        let after = *flat_idx;
                        *flat_idx = header_start;
                        let mut pin_tf = child_transform;
                        pin_tf.translate[1] += pin;
                        let child_id = flat_ids[*flat_idx];
                        // Bind `scroll_fraction` in the *render* env so the header's
                        // prop expressions (`opacity: 1.0 - scroll_fraction`, …)
                        // resolve it to the live signal, `eval_pure` re-lowers
                        // against the current env each frame, so the lower-time scope
                        // alone isn't enough. Truncated right after (header-only).
                        let fenv = self.env.len();
                        if let Some(fs) = *bound_sig {
                            self.env
                                .push(Symbol::intern("scroll_fraction"), Value::Signal(fs));
                        }
                        self.anim_slot = header.slot;
                        self.render_node_with_atlas(
                            header.node,
                            child_id,
                            frame,
                            flat_ids,
                            flat_idx,
                            current_rect,
                            child_opacity,
                            pin_tf,
                            child_clip,
                            // The pin undoes the vertical scroll translate, so
                            // the header's hit rects stay at the viewport top
                            // exactly like its pixels do.
                            (child_scroll_shift.0, child_scroll_shift.1 + pin),
                            child_window,
                            pools,
                        );
                        self.env.truncate(fenv);
                        *flat_idx = after;
                    }
                } else {
                    // RFC-0018: expand reactive `when`/`for` into concrete children
                    // in the same order the layout pass did.
                    for child in self.expand_concrete(children, pools) {
                        render_child(self, child, frame, flat_idx);
                    }
                }
                if scroll_clip.is_some() {
                    frame.end_clip();
                }
                if let Some(pathed) = mask_clip {
                    if pathed {
                        frame.end_clip();
                    }
                    frame.end_clip();
                }
                // After the clips it contains, so the group's picture includes
                // everything its box drew, clipped the way it was drawn.
                if grouped {
                    frame.end_group();
                }
                self.access_leave(frame, access_scope);
                // Close the RFC-0019 instance-env scope opened at the top of this
                // arm (balanced with `env_base`), restoring the caller's env for
                // the remaining siblings.
                self.env.truncate(env_base);
            }
            // RFC-0026: a navigation container. Its own surface is an ordinary
            // background fill; the interesting part is the per-screen transform
            //, the transition's whole cost is two `f32` offsets and an alpha,
            // composed into the transform every subtree already inherits, so a
            // screen sliding in costs no relayout and no extra pass (a
            // paint-class change never relayouts).
            RenderNode::Nav { pool, .. } => {
                let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) else {
                    return;
                };
                let Some(p) = pools.navs.get(*pool) else {
                    return;
                };
                let nav_rect =
                    crate::interp::intrinsics::Rect::new(rect.x, rect.y, rect.width, rect.height);
                let elem_idx = self.atlas.node_index(atlas_node);
                let state = elem_idx
                    .map_or_else(crate::interp::events::StyleState::empty, |i| {
                        self.router.style_state(i)
                    })
                    .union(self.prop_style_state(&p.attrs, None, ""));
                let paint_attrs = resolve_state_attrs(&p.attrs, &p.state_blocks, state, &|c| {
                    viewport_holds(c, self.viewport, &self.theme)
                });
                let paint_attrs = paint_attrs.as_ref();
                let own_transform = self.resolve_transform(paint_attrs, nav_rect);
                let transform = inherited_transform.compose(&own_transform);
                let opacity = inherited_opacity
                    * self
                        .eval_float_prop(paint_attrs, "opacity")
                        .map_or(1.0, |v| v as f32);
                if let Some(bg) = self.eval_color_prop(paint_attrs, "bg") {
                    push_fill(
                        frame,
                        byard_core::BoxInstance {
                            rect: [rect.x, rect.y, rect.width, rect.height],
                            color: dim_alpha(super::intrinsics::color_rgba_auto(bg), opacity),
                            radii: self.resolve_radii(paint_attrs, "radius"),
                            transform,
                            smooth: self.resolve_smooth(paint_attrs),
                        },
                    );
                }
                // `route_change` and any pointer handlers on the container.
                let hit_rect = scrolled_hit_rect(nav_rect, scroll_shift, cull_clip);
                self.register_event_attrs(&p.attrs, hit_rect, elem_idx);
                // RFC-0042: tabs are a tab list; a stack's screens have no
                // role of their own and their content is simply the tree's.
                let host = p.kind == NavKind::Host;
                let nav_scope = if host && self.access.on {
                    self.access_enter_tabs(frame, &p.attrs, elem_idx, nav_rect, &transform)
                } else {
                    access::Scope::default()
                };

                // A screen mid-transition is partly outside the container, so
                // everything it paints is clipped to the container's bounds,
                // otherwise a sliding screen would smear across its siblings.
                let clip = byard_core::frame::Rect::new(
                    transform.apply_point([nav_rect.x, nav_rect.y])[0],
                    transform.apply_point([nav_rect.x, nav_rect.y])[1],
                    nav_rect.w * transform.scale[0],
                    nav_rect.h * transform.scale[1],
                );
                frame.begin_clip(clip);
                for screen in &p.live {
                    let motion = p.transition.screen_motion(
                        p.progress,
                        p.popping,
                        screen.incoming,
                        nav_rect.w,
                        nav_rect.h,
                    );
                    let mut screen_transform = transform;
                    screen_transform.translate[0] += motion.dx * transform.scale[0];
                    screen_transform.translate[1] += motion.dy * transform.scale[1];
                    // Hit rects ride their own channel (RFC-0011 keeps
                    // paint transforms out of hit-testing), so the same offset
                    // travels separately, a half-slid screen is tappable
                    // exactly where it is drawn.
                    let screen_shift = (
                        scroll_shift.0 + motion.dx * transform.scale[0],
                        scroll_shift.1 + motion.dy * transform.scale[1],
                    );
                    let screen_id = flat_ids[*flat_idx];
                    *flat_idx += 1;
                    let screen_rect = self
                        .atlas
                        .resolved_rect(screen_id)
                        .ok()
                        .flatten()
                        .map_or(nav_rect, |r| {
                            crate::interp::intrinsics::Rect::new(r.x, r.y, r.width, r.height)
                        });
                    let tab_scope = if host && self.access.on {
                        self.access_enter_tab(
                            frame,
                            &p.entries[screen.entry].path,
                            screen.entry == p.current,
                            self.atlas.node_index(screen_id),
                            screen_rect,
                            &screen_transform,
                        )
                    } else {
                        access::Scope::default()
                    };
                    for child in self.expand_concrete(&p.entries[screen.entry].nodes, pools) {
                        let child_id = flat_ids[*flat_idx];
                        self.anim_slot = child.slot;
                        self.render_node_with_atlas(
                            child.node,
                            child_id,
                            frame,
                            flat_ids,
                            flat_idx,
                            screen_rect,
                            opacity * motion.opacity,
                            screen_transform,
                            Some(clip),
                            screen_shift,
                            None,
                            pools,
                        );
                    }
                    self.access_leave(frame, tab_scope);
                }
                frame.end_clip();
                self.access_leave(frame, nav_scope);
                // The element index a settled `route_change` fires against.
                if let Some(nav) = self.nav_elems.get_mut(*pool) {
                    *nav = elem_idx;
                }
                if p.swipe_back {
                    self.nav_targets.push((hit_rect, *pool));
                }
            }
            RenderNode::Image {
                attrs,
                state_blocks,
                src,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    // RFC-0016: overlay active `on <state>` blocks before reading
                    // paint properties (fit/radius/opacity). RFC-0024: fold in the
                    // universal `selected:`/`invalid:` states.
                    let state = self
                        .atlas
                        .node_index(atlas_node)
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
                    let attrs = attrs.as_ref();
                    let src_val = self
                        .binding_value(*src)
                        .and_then(|v| if let Value::Str(s) = v { Some(s) } else { None })
                        .unwrap_or_default();
                    let fit = self.eval_fit_prop(attrs);
                    let radii = self.resolve_radii(attrs, "radius");
                    let smooth = self.resolve_smooth(attrs);
                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(attrs, "opacity")
                            .map_or(1.0, |v| v as f32);
                    // RFC-0011 group transforms: an `Image` carries no transform
                    // field, so an ancestor's scale/translate is baked into its
                    // rect (top-left through the transform, extents scaled per
                    // axis). Rotation isn't representable here and is left to box
                    // primitives, same limitation as `Text`.
                    let tl = inherited_transform.apply_point([rect.x, rect.y]);
                    let tw = rect.width * inherited_transform.scale[0];
                    let th = rect.height * inherited_transform.scale[1];
                    if self.access.on {
                        let elem = self.atlas.node_index(atlas_node);
                        self.access_image(
                            frame,
                            attrs,
                            elem,
                            crate::interp::intrinsics::Rect::new(
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ),
                            &inherited_transform,
                            false,
                        );
                    }
                    frame.push_texture(byard_core::frame::TextureSampler {
                        rect: [tl[0], tl[1], tw, th],
                        src: src_val,
                        fit,
                        radii,
                        // RFC-0031 §S3: an image's rounded clip follows the same
                        // corner profile as the boxes around it.
                        smooth,
                        opacity,
                        // Re-emitted every tick; mirror Text's
                        // always-dirty lowering.
                        dirty: true,
                    });
                }
            }
            // RFC-0039: hand the view its rect and let it draw. Props are
            // re-evaluated every tick through the same `eval_*` chokepoints an
            // intrinsic's are, so a native view animates and reacts for free
            // (RFC-0010), and the view itself never learns that a value came
            // from a signal.
            RenderNode::Native {
                name,
                attrs,
                state_blocks,
                action,
                env_snapshot,
                slot,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    let elem_idx = self.atlas.node_index(atlas_node);
                    let state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
                    let paint_attrs = paint_attrs.as_ref();

                    // RFC-0019 §2: prop expressions resolve against the scope
                    // the element was written in.
                    let env_base = self.env.len();
                    for (k, v) in env_snapshot {
                        self.env.push(k.clone(), v.clone());
                    }
                    self.apply_native_props(name.as_str(), *slot, paint_attrs);
                    self.env.truncate(env_base);

                    let view_rect = crate::interp::intrinsics::Rect::new(
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                    );
                    if let Some(view) = self.native_views.get_mut(*slot) {
                        let outcome = frame.render_native(
                            view.as_mut(),
                            byard_core::render::Layout::new([
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ]),
                        );
                        if outcome.repaint {
                            // The view is animating from state the engine
                            // cannot see, so it says so once per frame rather
                            // than holding a subscription open (RFC-0032).
                            frame.request_full_redraw();
                        }
                        // Whose calls these are is known here and nowhere
                        // else: a view is handed a context, never an identity
                        // (RFC-0039).
                        if !outcome.calls.is_empty() {
                            self.native_call_owners.push((*slot, outcome.calls));
                        }
                    }

                    // Events: the view's rect, under the same hit-rect rules an
                    // intrinsic follows (RFC-0003 E8 inflation included), and
                    // recorded so `dispatch_events` can offer it the raw event
                    // its `on_event` expects.
                    let hit_rect = scrolled_hit_rect(
                        crate::interp::intrinsics::inflate_hit_rect(view_rect, parent_rect),
                        scroll_shift,
                        cull_clip,
                    );
                    self.native_targets.push(NativeTarget {
                        slot: *slot,
                        rect: hit_rect,
                        drawn_at: self.router.handler_count(),
                    });
                    if self.access.on {
                        self.access_native(
                            frame,
                            name.as_str(),
                            attrs,
                            elem_idx,
                            view_rect,
                            &inherited_transform,
                        );
                    }
                    if let Some(idx) = elem_idx {
                        if state_blocks.iter().any(|sb| {
                            sb.states.iter().any(|st| {
                                matches!(st, StyleStateKind::Hover | StyleStateKind::Pressed)
                            })
                        }) {
                            self.router.track_region(idx, hit_rect);
                        }
                    }
                    let has_events = attrs
                        .iter()
                        .any(|a| matches!(a.kind, AttrKind::Event { .. }))
                        || action.is_some();
                    if has_events {
                        let env_base = self.env.len();
                        for (k, v) in env_snapshot {
                            self.env.push(k.clone(), v.clone());
                        }
                        self.register_event_attrs(attrs, hit_rect, elem_idx);
                        if let Some(action_expr) = action {
                            if let (Ok(closure), Some(idx)) =
                                (self.lower_action(action_expr, None), elem_idx)
                            {
                                self.router.on(
                                    idx,
                                    hit_rect,
                                    crate::interp::events::EventKind::Tap,
                                    closure,
                                );
                            }
                        }
                        self.env.truncate(env_base);
                    }
                }
            }
            RenderNode::Vector { attrs, src } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    if self.access.on {
                        let elem = self.atlas.node_index(atlas_node);
                        self.access_image(
                            frame,
                            attrs,
                            elem,
                            crate::interp::intrinsics::Rect::new(
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ),
                            &inherited_transform,
                            false,
                        );
                    }
                    let handle = self
                        .binding_value(*src)
                        .and_then(|v| if let Value::Str(s) = v { Some(s) } else { None })
                        .unwrap_or_default();
                    let base_rgb = self
                        .eval_color_prop(attrs, "color")
                        .map_or([1.0, 1.0, 1.0, 1.0], super::intrinsics::color_rgba_auto);
                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(attrs, "opacity")
                            .map_or(1.0, |v| v as f32);

                    // Cache hit: a resident glyph, tinted and opacity-applied.
                    // Cache miss: a zero-opacity placeholder so the frame ships
                    // without stalling (the render thread never blocks on
                    // generation); the dispatch itself happened
                    // inside `lookup_or_dispatch`.
                    let (uv_rect, layer, px_range, alpha) =
                        match self.vector_jit.lookup_or_dispatch(&handle) {
                            Some(glyph) => (
                                glyph.uv_rect,
                                glyph.layer,
                                glyph.px_range,
                                base_rgb[3] * opacity,
                            ),
                            None => (
                                byard_core::frame::Rect::new(0.0, 0.0, 0.0, 0.0),
                                0,
                                super::intrinsics::VECTOR_DEFAULT_PX_RANGE,
                                0.0,
                            ),
                        };
                    let rgb = [base_rgb[0], base_rgb[1], base_rgb[2], alpha];

                    frame.push_vector(byard_core::frame::VectorInstance::new(
                        byard_core::frame::Rect::new(rect.x, rect.y, rect.width, rect.height),
                        uv_rect,
                        rgb,
                        px_range,
                        layer,
                    ));
                }
            }
            // RFC-0020: the `Canvas` drawing surface. Shape parameters are
            // re-evaluated every tick through `eval_pure`, so a reactive or
            // `with`-animated `sweep`/`dash_offset`/color animates with zero
            // extra plumbing (RFC-0010's single evaluation chokepoint).
            RenderNode::Canvas {
                attrs,
                state_blocks,
                shapes,
                action,
                env_snapshot,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    let elem_idx = self.atlas.node_index(atlas_node);
                    // RFC-0016: overlay active `on <state>` blocks before
                    // reading paint properties. RFC-0024: fold in universal states.
                    let state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
                    let paint_attrs = paint_attrs.as_ref();

                    // RFC-0019 §2: restore the instance environment so shape
                    // parameter expressions and event actions resolve against
                    // the scope the canvas was instantiated in.
                    let env_base = self.env.len();
                    for (k, v) in env_snapshot {
                        self.env.push(k.clone(), v.clone());
                    }

                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(paint_attrs, "opacity")
                            .map_or(1.0, |v| v as f32);
                    let canvas_rect = crate::interp::intrinsics::Rect::new(
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                    );

                    if self.access.on {
                        self.access_image(
                            frame,
                            attrs,
                            elem_idx,
                            canvas_rect,
                            &inherited_transform,
                            true,
                        );
                    }

                    // Background fill: a plain solid behind every shape.
                    if let Some(bg) = self.eval_color_prop(paint_attrs, "bg") {
                        push_fill(
                            frame,
                            byard_core::BoxInstance {
                                rect: [rect.x, rect.y, rect.width, rect.height],
                                color: dim_alpha(super::intrinsics::color_rgba_auto(bg), opacity),
                                radii: [0.0; 4],
                                transform: inherited_transform,
                                smooth: 0.0,
                            },
                        );
                    }

                    // Shape commands, in declaration order (painter's order,
                    // each `push_canvas_shape` advances the global emission
                    // depth, RFC-0011). A `Canvas` that declares a combine mode
                    // (RFC-0031 §S4) collects them into one group instead, and
                    // pushes its head once at the end.
                    let combine = self.resolve_group_mode(paint_attrs);
                    let mut sink = combine.map(|(mode, _)| ShapeGroupSink {
                        morph_paths: (mode == byard_core::frame::GROUP_MORPH).then(Vec::new),
                        ..ShapeGroupSink::default()
                    });
                    self.emit_canvas_items(
                        shapes,
                        canvas_rect,
                        opacity,
                        inherited_transform,
                        sink.as_mut(),
                        frame,
                    );
                    if let (Some((mode, param)), Some(mut sink)) = (combine, sink) {
                        if let Some(paths) = sink.morph_paths.take() {
                            let resample = paint_attrs
                                .iter()
                                .find(|a| a.name.as_str() == "morph_mode")
                                .and_then(|a| match &a.kind {
                                    AttrKind::Prop { value } => Self::enum_token(value),
                                    _ => None,
                                })
                                == Some("resample");
                            self.push_morph_paths(
                                &paths,
                                param,
                                resample,
                                canvas_rect,
                                opacity,
                                inherited_transform,
                                frame,
                            );
                        }
                        self.push_shape_group(
                            mode,
                            param,
                            sink,
                            opacity,
                            inherited_transform,
                            attr_span(attrs),
                            frame,
                        );
                    }

                    // Events: the canvas rect only, individual shapes are not
                    // hit-testable (RFC-0020 resolved question). Shifted to
                    // the on-screen position like every hit rect (RFC-0005).
                    let hit_rect = scrolled_hit_rect(
                        crate::interp::intrinsics::inflate_hit_rect(canvas_rect, parent_rect),
                        scroll_shift,
                        cull_clip,
                    );
                    // RFC-0016: an `on hover`/`on pressed` block with no handler
                    // still needs pointer tracking, mirroring the Box path.
                    if let Some(idx) = elem_idx {
                        if state_blocks.iter().any(|sb| {
                            sb.states.iter().any(|s| {
                                matches!(s, StyleStateKind::Hover | StyleStateKind::Pressed)
                            })
                        }) {
                            self.router.track_region(idx, hit_rect);
                        }
                    }
                    let has_events = attrs
                        .iter()
                        .any(|a| matches!(a.kind, AttrKind::Event { .. }))
                        || action.is_some();
                    if has_events {
                        self.register_event_attrs(attrs, hit_rect, elem_idx);
                        if let Some(action_expr) = action {
                            if let Ok(closure) = self.lower_action(action_expr, None) {
                                if let Some(idx) = elem_idx {
                                    self.router.on(
                                        idx,
                                        hit_rect,
                                        crate::interp::events::EventKind::Tap,
                                        closure,
                                    );
                                }
                            }
                        }
                    }
                    self.env.truncate(env_base);
                }
            }
        }
    }

    /// Emits the RFC-0023 §2 backdrop-blur pane for one element, called from
    /// the `Box` paint arm right after the background push, the RFC-0023 §4
    /// compositing slot (background → blur → tint → ripple → children): the
    /// pane samples everything already emitted, its own background included.
    ///
    /// `blur` (clamped to [`byard_core::frame::BLUR_MAX_RADIUS`]) enables the
    /// pane; both it and `backdrop_tint` resolve through the RFC-0010
    /// animation chokepoints, so `blur: 0 with anim.spring()` +
    /// `on hover { blur: 16 }` animates the glass for free. A tint *without*
    /// blur needs no barrier or off-screen work at all, it lowers to a plain
    /// translucent fill over the content behind, which is the identical
    /// composite at zero cost.
    #[allow(clippy::too_many_arguments)]
    fn emit_backdrop(
        &mut self,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        radii: [f32; 4],
        smooth: f32,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{
            BLUR_MAX_RADIUS, BLUR_QUALITY_AUTO, BLUR_QUALITY_HIGH, BLUR_QUALITY_LOW,
            BackdropInstance,
        };

        let blur = self
            .eval_float_prop(attrs, "blur")
            .map_or(0.0, |v| (v as f32).clamp(0.0, BLUR_MAX_RADIUS));
        let tint = self.eval_color_prop(attrs, "backdrop_tint");
        // Alpha auto-detect (lexer tag or magnitude, RFC-0011): a tint is
        // translucent by nature, `0x00FFFFFF` included.
        let tint_rgba = tint.map_or([0.0; 4], super::intrinsics::color_rgba_auto);

        if blur <= 0.0 {
            // Tint-only: a translucent wash over the (unblurred) content
            // behind, a plain alpha-blended fill composites identically.
            if tint_rgba[3] > 0.0 {
                frame.push_decorated(byard_core::frame::DecoratedBox {
                    base: byard_core::BoxInstance {
                        rect: [rect.x, rect.y, rect.w, rect.h],
                        color: tint_rgba,
                        radii,
                        transform,
                        smooth,
                    },
                    opacity,
                    dirty: true,
                    ..Default::default()
                });
            }
            return;
        }

        let saturation = self
            .eval_float_prop(attrs, "blur_saturation")
            .map_or(BLUR_DEFAULT_SATURATION, |v| (v as f32).max(0.0));
        let quality = attrs
            .iter()
            .find(|a| a.name.as_str() == "blur_quality")
            .and_then(|a| match &a.kind {
                AttrKind::Prop { value } => Self::enum_token(value),
                _ => None,
            })
            .map_or(BLUR_QUALITY_AUTO, |token| match token {
                "high" => BLUR_QUALITY_HIGH,
                "low" => BLUR_QUALITY_LOW,
                _ => BLUR_QUALITY_AUTO,
            });

        frame.push_backdrop(BackdropInstance {
            rect: [rect.x, rect.y, rect.w, rect.h],
            radii,
            smooth,
            blur,
            tint: tint_rgba,
            saturation,
            quality,
            opacity,
            transform,
            depth: 0.0, // stamped by `push_backdrop`
        });
    }

    /// Spawns and emits the RFC-0023 ripple ink for one element, called from
    /// the `Box` paint arm between the background push and the child walk,
    /// which is exactly the RFC's compositing slot (background → ripple →
    /// children), resolved by the emission-order draw depth.
    ///
    /// Enabled by a `ripple:` colour on the element. A ripple *spawns* on the
    /// rising edge of a press gesture while the resolved `ripple_active` is
    /// true (typically flipped by `on pressed { ripple_active: true }`), at the
    /// pointer-down point; the gesture's press timestamp is the spawn latch, so
    /// a hold spawns once while each rapid tap spawns its own (their ink
    /// pools in the pipeline where the circles overlap).
    /// Colour/duration/radius snapshot at spawn;
    /// the element rect, clip radii, transform, and opacity are re-read each
    /// frame so ink tracks a moving element. Expansion is an ease-out ramp and
    /// fade a linear one, both sampled through the shared [`Motion`] closed
    /// forms (RFC-0010 as landed).
    ///
    /// [`Motion`]: byard_core::frame::Motion
    #[allow(clippy::too_many_arguments)]
    fn emit_ripples(
        &mut self,
        attrs: &[Attr],
        elem_idx: Option<u32>,
        rect: crate::interp::intrinsics::Rect,
        radii: [f32; 4],
        smooth: f32,
        transform: byard_core::frame::Transform,
        opacity: f32,
        // Accumulated scroll displacement (RFC-0005): the press position is
        // in screen space while `rect` is layout space, so the tap point maps
        // back through the shift.
        scroll_shift: (f32, f32),
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{Motion, MotionCurve, RippleInstance};

        let Some(color) = self.eval_color_prop(attrs, "ripple") else {
            return;
        };
        let Some(elem) = elem_idx else {
            return;
        };

        // Spawn on a fresh press gesture. Gated on an advancing clock, without
        // one a ripple could never expand, fade, or retire (mirrors
        // `eval_animated`'s inert-host rule).
        if self.clock_set && self.eval_bool_prop(attrs, "ripple_active") == Some(true) {
            if let Some((pos, press_ms)) = self.router.press_gesture(elem) {
                if self.ripple_spawned != Some((elem, press_ms)) {
                    self.ripple_spawned = Some((elem, press_ms));
                    let duration_ms = self
                        .eval_float_prop(attrs, "ripple_duration")
                        .map_or(RIPPLE_DEFAULT_DURATION_MS, |v| v as f32)
                        .max(1.0);
                    let max_radius = self
                        .eval_float_prop(attrs, "ripple_radius")
                        .map(|v| v as f32);
                    self.ripples.push(ActiveRipple {
                        elem,
                        center_rel: [
                            (pos.0 - scroll_shift.0 - rect.x).clamp(0.0, rect.w),
                            (pos.1 - scroll_shift.1 - rect.y).clamp(0.0, rect.h),
                        ],
                        // Alpha auto-detect (lexer tag or magnitude), ripple
                        // ink is typically translucent.
                        color: super::intrinsics::color_rgba_auto(color),
                        start_ms: self.now_ms,
                        duration_ms,
                        max_radius,
                    });
                }
            }
        }

        // Emit every live ripple owned by this element; `any_active` is set
        // after the loop so the iteration's borrow of `self.ripples` never
        // overlaps the mutation.
        let now = self.now_ms;
        let mut emitted_any = false;
        for r in &self.ripples {
            if r.elem != elem {
                continue;
            }
            // Auto max radius: the distance from the tap point to the farthest
            // element corner, so the ink always covers the whole surface
            // (RFC-0023 §"Ripple properties").
            let max_r = r.max_radius.unwrap_or_else(|| {
                let dx = r.center_rel[0].max(rect.w - r.center_rel[0]);
                let dy = r.center_rel[1].max(rect.h - r.center_rel[1]);
                dx.hypot(dy)
            });
            let curve = |kind: u32| MotionCurve {
                kind,
                params: [r.duration_ms, 0.0, 0.0],
            };
            let expand = Motion {
                from: 0.0,
                to: max_r,
                start_ms: r.start_ms,
                curve: curve(MotionCurve::EASE_OUT),
            };
            let fade = Motion {
                from: 1.0,
                to: 0.0,
                start_ms: r.start_ms,
                curve: curve(MotionCurve::LINEAR),
            };
            frame.push_ripple(RippleInstance {
                rect: [rect.x, rect.y, rect.w, rect.h],
                params: [
                    rect.x + r.center_rel[0],
                    rect.y + r.center_rel[1],
                    expand.sample(now),
                    fade.sample(now) * opacity,
                ],
                color: r.color,
                radii,
                smooth,
                t_translate: transform.translate,
                t_scale: transform.scale,
                t_rotate: transform.rotate,
                t_origin: transform.origin,
                depth: 0.0, // stamped by `push_ripple`
            });
            emitted_any = true;
        }
        if emitted_any {
            self.any_active = true;
        }
    }
}
