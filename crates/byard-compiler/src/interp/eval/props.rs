//! Resolving an element's props to values (RFC-0011, RFC-0016).
//!
//! A prop is an expression, so reading one evaluates it. First the typed
//! readers every intrinsic goes through (a colour, an integer, a pixel
//! extent, a float, a bool, a string, an image fit; `None` for an absent or
//! mistyped prop), and the two that read a bareword keyword straight from the
//! syntax tree so no `var` of the same name can shadow it. Then the box
//! style built from them: the container's layout style, spacing, shadows,
//! gradients, corner radii and the paint-time transform.

use super::Interpreter;
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::Value;
use crate::parser::ast::{Arg, Attr, AttrKind, Expr, StrPart};
use crate::symbol::Symbol;

/// One resolved drop shadow (RFC-0011 custom shadows): offset, blur, spread, and
/// resolved RGBA colour. A box may carry several, CSS-style layered shadows,
/// each emitted as its own shadow-only `DecoratedBox` beneath the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ShadowSpec {
    pub(super) dx: f32,
    pub(super) dy: f32,
    pub(super) blur: f32,
    pub(super) spread: f32,
    pub(super) color: [f32; 4],
}

/// Default drop-shadow colour (`0xAARRGGBB`, ~33% black) when a shadow omits its
/// own `color`.
const DEFAULT_SHADOW_COLOR: i64 = 0x5500_0000;

/// Renders a value for string interpolation (`"Count: {count}"`).
/// Coerces a spacing side/scalar value to `f32`; only numeric values are valid
/// `Len`s (a non-numeric side is a `TypeMismatch`).
pub(super) fn spacing_value(v: &Value) -> Option<f32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    match v {
        Value::Int(n) => Some(*n as f32),
        Value::Float(f) => Some(*f as f32),
        _ => None,
    }
}

/// Reads an already-evaluated two-axis value (`(x, y)` positional or
/// `(x: …, y: …)` named) as a pair, leaving unset axes at `default`.
///
/// The value-level counterpart of `resolve_axis_pair_value`'s syntactic tuple
/// handling, for pairs that only exist after evaluation, a keyframed
/// `translate` (RFC-0025 §3) being the case that needs it.
fn axis_pair_of_value(value: &Value, default: (f32, f32)) -> Option<(f32, f32)> {
    let items = value.as_tuple()?;
    if items.iter().all(|(name, _)| name.is_some()) {
        let axis = |want: &str| {
            items
                .iter()
                .find(|(name, _)| name.as_ref().is_some_and(|n| n.as_str() == want))
                .and_then(|(_, v)| spacing_value(v))
        };
        return Some((
            axis("x").unwrap_or(default.0),
            axis("y").unwrap_or(default.1),
        ));
    }
    let [(_, x), (_, y)] = items else {
        return None;
    };
    Some((spacing_value(x)?, spacing_value(y)?))
}

/// Assigns one resolved side of a named spacing tuple, recording a
/// [`CompileError::ConflictingSpacingField`] if the side was already set (either
/// directly or via an axis shorthand).
fn assign_side(
    slot: &mut Option<f32>,
    v: f32,
    side: &str,
    span: Span,
    errors: &mut Vec<CompileError>,
) {
    if slot.is_some() {
        errors.push(CompileError::ConflictingSpacingField {
            span,
            message: format!("spacing side `{side}` was set more than once"),
        });
    } else {
        *slot = Some(v);
    }
}

impl Interpreter {
    pub(super) fn eval_color_prop(&mut self, attrs: &[Attr], name: &str) -> Option<i64> {
        // Resolve the matching attribute value; a `with` colour animation
        // (RFC-0010 A3) is driven through the OKLab path rather than the scalar
        // one, since a packed `0xRRGGBB` can't be interpolated component-wise.
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == name => Some(value),
            _ => None,
        })?;
        if let Expr::Animated {
            value: target,
            anim,
            span,
        } = value
        {
            return Some(self.eval_animated_color(target, anim, self.anim_key(*span)));
        }
        // A keyframed colour (RFC-0025 §3) blends its two surrounding steps in
        // the same perceptual space, for the same reason.
        if crate::interp::anim::is_keyframes_call(value) {
            return self.eval_keyframe_color(value);
        }
        self.eval_pure(value).as_int()
    }

    pub(super) fn eval_int_prop(&mut self, attrs: &[Attr], name: &str) -> Option<i64> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    let val = self.eval_pure(value);
                    return val.as_int();
                }
            }
            None
        })
    }

    /// Reads a **pixel extent** prop (`width`, `height`) as an `f32`.
    ///
    /// Separate from [`eval_int_prop`](Self::eval_int_prop) because a pixel
    /// extent is not an integer quantity: layout is `f32` throughout, a
    /// measured rect (RFC-0038) is fractional, and `800.0 / 3.0` is a perfectly
    /// ordinary width to write. Read through the integer path, every one of
    /// those resolved to `None` and the element silently fell back to its
    /// default size, which is exactly the silent failure the engine forbids.
    pub(super) fn eval_px_prop(&mut self, attrs: &[Attr], name: &str) -> Option<f32> {
        // RFC-0036: `width: match(ref)` is a layout relationship, not a
        // number. It is resolved once the anchor's rect exists, and
        // evaluating it here would call a function nobody wrote.
        if name == "width" && Self::match_width_ref(attrs).is_some() {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    return match self.eval_pure(value) {
                        Value::Int(n) => Some(n as f32),
                        Value::Float(f) => Some(f as f32),
                        _ => None,
                    };
                }
            }
            None
        })
    }

    pub(super) fn eval_float_prop(&mut self, attrs: &[Attr], name: &str) -> Option<f64> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    let val = self.eval_pure(value);
                    return match val {
                        Value::Float(f) => Some(f),
                        Value::Int(n) => Some(n as f64),
                        _ => None,
                    };
                }
            }
            None
        })
    }

    pub(super) fn eval_bool_prop(&mut self, attrs: &[Attr], name: &str) -> Option<bool> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    if let Value::Bool(b) = self.eval_pure(value) {
                        return Some(b);
                    }
                }
            }
            None
        })
    }

    pub(super) fn eval_str_prop(&mut self, attrs: &[Attr], name: &str) -> Option<String> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    let val = self.eval_pure(value);
                    return match val {
                        Value::Str(s) => Some(s),
                        _ => None,
                    };
                }
            }
            None
        })
    }

    /// The literal keyword token of an *enum* (keyword) prop value, read straight
    /// from the AST bareword, e.g. the `page` in `snap: page`.
    ///
    /// Enum props (`PropType::Enum`) are a closed keyword set validated by the
    /// checker as a bare [`Expr::Ident`] (`intrinsics.rs`, `check_attr_value`),
    /// never as a reactive expression. Reading them with [`eval_pure`] would
    /// instead resolve the identifier through the environment, so a same-named
    /// `var` silently *shadows* the keyword: `snap: page` next to a `var page`
    /// evaluates to the variable's `Int`, not the token `"page"`, and the
    /// behaviour turns off (RFC-0021). Taking the token from the AST matches the
    /// checker exactly, can never be shadowed, and skips lowering an expression
    /// for a value that is always a compile-time keyword.
    pub(super) fn enum_token(value: &Expr) -> Option<&str> {
        match value {
            Expr::Ident(sym, _) => Some(sym.as_str()),
            // A keyword may also be written as a plain string literal
            // (`fit: "cover"`). A single, non-interpolated run is a token; an
            // interpolated string is not.
            Expr::StrLit(parts, _) => match parts.as_slice() {
                [StrPart::Text(s)] => Some(s.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    /// [`enum_token`](Self::enum_token) for a named attribute: the keyword token
    /// of enum prop `name`, or `None` if absent or not a bareword keyword.
    pub(super) fn enum_prop<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a str> {
        attrs.iter().find_map(|a| match &a.kind {
            AttrKind::Prop { value } if a.name.as_str() == name => Self::enum_token(value),
            _ => None,
        })
    }

    pub(super) fn eval_fit_prop(&mut self, attrs: &[Attr]) -> byard_core::frame::ImageFit {
        match Self::enum_prop(attrs, "fit") {
            Some("contain") => byard_core::frame::ImageFit::Contain,
            Some("cover") => byard_core::frame::ImageFit::Cover,
            Some("none") => byard_core::frame::ImageFit::None,
            _ => byard_core::frame::ImageFit::Fill,
        }
    }

    pub(super) fn eval_container_style(
        &mut self,
        element_name: &str,
        attrs: &[Attr],
    ) -> byard_core::atlas::layout::ContainerStyle {
        use byard_core::atlas::layout::{Align, ContainerStyle, FlexDir, Justify};

        let val_to_f32 = |v: &Value| -> Option<f32> {
            match v {
                Value::Int(n) => Some(*n as f32),
                Value::Float(f) => Some(*f as f32),
                _ => None,
            }
        };

        let mut style = ContainerStyle::default();
        style.direction = match element_name {
            "Row" => FlexDir::Row,
            _ => FlexDir::Column,
        };
        // RFC-0005 `ScrollView`: a scroll container, content is measured at
        // natural size and overflows the fixed viewport (clipped + scrolled by
        // the renderer), rather than flex-shrunk to fit. `axis` (default
        // `vertical`) picks the overflowing axes; `both` scrolls in 2D.
        if element_name == "ScrollView" {
            style = style.with_scroll_axes(false, true);
        }
        // A `Button` starts from the theme's defaults: padding around its
        // label, the label centred on both axes, and a minimum touch size.
        // They are seeded before the attribute loop, so anything the author
        // wrote (`p:`, one side, `align:`, `justify:`) simply overwrites them.
        if element_name == "Button" {
            let (v, h) = self.theme.button_padding;
            style.padding = byard_core::atlas::layout::Spacing::symmetric(v, h);
            style.align = Align::Center;
            style.justify = Justify::Center;
            let min = Some(self.theme.button_min_size);
            style = style.with_min_size(min, min);
        }
        for attr in attrs {
            if let AttrKind::Prop { value } = &attr.kind {
                // Evaluate only the layout props this resolver consumes.
                // Paint/effect attrs (`bg`, `blur`, `opacity`, …) must NOT be
                // evaluated here: layout runs in the build phase against the
                // *base* attrs, and driving a paint prop through the RFC-0010
                // `with` chokepoint from here would fight the paint pass's
                // state-resolved evaluation of the same `Motion`, a
                // retarget ping-pong that freezes state-driven animations
                // short of their target. Layout props themselves are never
                // animatable (`LayoutPropNotAnimatable`), so skipping the
                // rest removes wasted work without changing behaviour.
                let val = match attr.name.as_str() {
                    "width" | "height" | "gap" | "grow" | "basis" | "pt" | "padding_top"
                    | "padding-top" | "pr" | "padding_right" | "padding-right" | "pb"
                    | "padding_bottom" | "padding-bottom" | "pl" | "padding_left"
                    | "padding-left" | "mt" | "margin_top" | "margin-top" | "mr"
                    | "margin_right" | "margin-right" | "mb" | "margin_bottom"
                    | "margin-bottom" | "ml" | "margin_left" | "margin-left" | "mx"
                    | "margin_x" | "margin_horizontal" | "margin-horizontal" | "my"
                    | "margin_y" | "margin_vertical" | "margin-vertical" => self.eval_pure(value),
                    _ => Value::Unit,
                };
                match attr.name.as_str() {
                    "axis" if element_name == "ScrollView" => {
                        if let Some(s) = Self::enum_token(value) {
                            let (x, y) = match s {
                                "horizontal" => (true, false),
                                "both" => (true, true),
                                _ => (false, true),
                            };
                            style = style.with_scroll_axes(x, y);
                            // A ScrollView is `Column`, so its cross axis is x. To
                            // scroll horizontally, content must keep its natural
                            // width instead of being stretched to the viewport, or
                            // Taffy caps the content extent at the viewport and
                            // there is nothing to scroll. A `stretch` on the block
                            // axis (vertical-only) is still what fills row width.
                            if x {
                                style.align = Align::Start;
                            }
                        }
                    }
                    "width" => style.width = val_to_f32(&val),
                    "height" => style.height = val_to_f32(&val),
                    "direction" => {
                        if let Some(s) = Self::enum_token(value) {
                            style.direction = match s {
                                "column" => FlexDir::Column,
                                _ => FlexDir::Row,
                            };
                        }
                    }
                    "gap" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.gap = v;
                        }
                    }
                    "p" | "padding" => {
                        style.padding = self.resolve_spacing(value, "p");
                    }
                    "pt" | "padding_top" | "padding-top" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.top = v;
                        }
                    }
                    "pr" | "padding_right" | "padding-right" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.right = v;
                        }
                    }
                    "pb" | "padding_bottom" | "padding-bottom" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.bottom = v;
                        }
                    }
                    "pl" | "padding_left" | "padding-left" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.left = v;
                        }
                    }
                    "m" | "margin" => {
                        style.margin = self.resolve_spacing(value, "m");
                    }
                    "mt" | "margin_top" | "margin-top" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.top = v;
                        }
                    }
                    "mr" | "margin_right" | "margin-right" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.right = v;
                        }
                    }
                    "mb" | "margin_bottom" | "margin-bottom" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.bottom = v;
                        }
                    }
                    "ml" | "margin_left" | "margin-left" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.left = v;
                        }
                    }
                    "mx" | "margin_x" | "margin_horizontal" | "margin-horizontal" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.left = v;
                            style.margin.right = v;
                        }
                    }
                    "my" | "margin_y" | "margin_vertical" | "margin-vertical" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.top = v;
                            style.margin.bottom = v;
                        }
                    }
                    "align" => {
                        if let Some(s) = Self::enum_token(value) {
                            style.align = match s {
                                "start" => Align::Start,
                                "center" => Align::Center,
                                "end" => Align::End,
                                _ => Align::Stretch,
                            };
                        }
                    }
                    "justify" => {
                        if let Some(s) = Self::enum_token(value) {
                            style.justify = match s {
                                "center" => Justify::Center,
                                "end" => Justify::End,
                                "between" => Justify::Between,
                                "around" => Justify::Around,
                                "evenly" => Justify::Evenly,
                                _ => Justify::Start,
                            };
                        }
                    }
                    "grow" => {
                        if let Some(n) = val.as_int() {
                            style.grow = n as f32;
                        }
                    }
                    _ => {}
                }
            }
        }
        // An explicit `width:`/`height:` is the author's call; the touch floor
        // never overrides it.
        if style.width.is_some() {
            style.min_width = None;
        }
        if style.height.is_some() {
            style.min_height = None;
        }
        style
    }

    /// Resolves a `Len`-typed `p`/`m` attribute value into a `Spacing` quad
    /// (RFC-0005 §1 erratum), emitting span-anchored `CompileError`s
    /// for the four error classes:
    ///
    /// - an unknown side name → [`CompileError::UnknownAttribute`] with a hint;
    /// - a side set twice, an axis shorthand plus one of its component sides, or
    ///   a tuple mixing named and positional fields →
    ///   [`CompileError::ConflictingSpacingField`];
    /// - a non-numeric side value → [`CompileError::AttributeTypeMismatch`];
    /// - a positional tuple of arity 3 or > 4 → [`CompileError::ArityMismatch`].
    ///
    /// Accepted forms: scalar (`p: 5`), inferred pair (`p: (vertical, horizontal)`),
    /// inferred quad CSS `(top, right, bottom, left)`, and the verbose named form
    /// (`p: (top: 4, horizontal: 8)`). A single parenthesized value parses to the
    /// inner expression, so it arrives as a scalar.
    fn resolve_spacing(&mut self, expr: &Expr, prop: &str) -> byard_core::atlas::layout::Spacing {
        use byard_core::atlas::layout::Spacing;
        match expr {
            Expr::Tuple(args, span) => {
                let any_named = args.iter().any(|a| a.name.is_some());
                let all_named = args.iter().all(|a| a.name.is_some());
                if any_named && !all_named {
                    self.errors.push(CompileError::ConflictingSpacingField {
                        span: *span,
                        message: "a spacing tuple cannot mix named and positional fields"
                            .to_string(),
                    });
                    return Spacing::default();
                }
                if all_named {
                    self.resolve_named_spacing(args)
                } else {
                    self.resolve_positional_spacing(args, *span, prop)
                }
            }
            other => {
                let val = self.eval_pure(other);
                if let Some(v) = spacing_value(&val) {
                    Spacing::all(v)
                } else {
                    self.errors.push(CompileError::AttributeTypeMismatch {
                        span: other.span(),
                        expected: "a length (an integer)".to_string(),
                    });
                    Spacing::default()
                }
            }
        }
    }

    /// Verbose named spacing form (`p: (top: 4, horizontal: 8)`).
    fn resolve_named_spacing(&mut self, args: &[Arg]) -> byard_core::atlas::layout::Spacing {
        use byard_core::atlas::layout::Spacing;
        const SIDES: &[&str] = &["top", "bottom", "left", "right", "horizontal", "vertical"];

        let (mut top, mut right, mut bottom, mut left) = (None, None, None, None);
        for arg in args {
            // `all_named` guarantees a name is present.
            let Some(name) = &arg.name else { continue };
            let span = arg.value.span();
            let val = self.eval_pure(&arg.value);
            let Some(v) = spacing_value(&val) else {
                self.errors.push(CompileError::AttributeTypeMismatch {
                    span,
                    expected: "a length (an integer)".to_string(),
                });
                continue;
            };
            match name.as_str() {
                "top" => assign_side(&mut top, v, "top", span, &mut self.errors),
                "bottom" => assign_side(&mut bottom, v, "bottom", span, &mut self.errors),
                "left" => assign_side(&mut left, v, "left", span, &mut self.errors),
                "right" => assign_side(&mut right, v, "right", span, &mut self.errors),
                "horizontal" => {
                    assign_side(&mut left, v, "left", span, &mut self.errors);
                    assign_side(&mut right, v, "right", span, &mut self.errors);
                }
                "vertical" => {
                    assign_side(&mut top, v, "top", span, &mut self.errors);
                    assign_side(&mut bottom, v, "bottom", span, &mut self.errors);
                }
                unknown => {
                    let hint = crate::util::closest_match(unknown, SIDES.iter().copied())
                        .map(str::to_string);
                    self.errors.push(CompileError::UnknownAttribute {
                        span,
                        name: unknown.to_string(),
                        hint,
                    });
                }
            }
        }
        Spacing {
            top: top.unwrap_or(0.0),
            right: right.unwrap_or(0.0),
            bottom: bottom.unwrap_or(0.0),
            left: left.unwrap_or(0.0),
        }
    }

    /// Inferred positional spacing forms: pair `(vertical, horizontal)` or quad
    /// CSS `(top, right, bottom, left)`. Any other arity is an error.
    fn resolve_positional_spacing(
        &mut self,
        args: &[Arg],
        span: Span,
        prop: &str,
    ) -> byard_core::atlas::layout::Spacing {
        use byard_core::atlas::layout::Spacing;
        let mut vals = Vec::with_capacity(args.len());
        for arg in args {
            let val = self.eval_pure(&arg.value);
            if let Some(v) = spacing_value(&val) {
                vals.push(v);
            } else {
                self.errors.push(CompileError::AttributeTypeMismatch {
                    span: arg.value.span(),
                    expected: "a length (an integer)".to_string(),
                });
                vals.push(0.0);
            }
        }
        match vals.len() {
            2 => Spacing::symmetric(vals[0], vals[1]),
            4 => Spacing {
                top: vals[0],
                right: vals[1],
                bottom: vals[2],
                left: vals[3],
            },
            n => {
                self.errors.push(CompileError::ArityMismatch {
                    span,
                    name: prop.to_string(),
                    expected: 4,
                    found: n,
                });
                Spacing::default()
            }
        }
    }

    /// Resolves a `radius`-typed attribute into per-corner radii
    /// `[top_left, top_right, bottom_right, bottom_left]`, the exact order
    /// `BoxInstance::radii`/`TextureSampler::radii` expect (`frame.rs`).
    ///
    /// RFC-0005 §"Decoration" documents `radius: Len` as "scalar = all, quad =
    /// per-corner". Accepted forms: a scalar (`radius: 16`, all four
    /// corners) and the positional CSS-order quad (`radius: (4, 8, 12, 16)`).
    /// Unlike `p`/`m`'s generic `Len` contract, there is no pair shorthand and
    /// no named-field form for `radius`, the RFC documents only scalar/quad,
    /// so this resolver doesn't invent additional surface. A non-4 tuple
    /// arity is a `CompileError::ArityMismatch`; a non-numeric corner is an
    /// `AttributeTypeMismatch`; a named field is a `ConflictingSpacingField`
    /// (reusing the existing diagnostic, the message states the real cause).
    /// Resolves the `shadow` attribute into zero or more drop shadows
    /// (RFC-0011 custom shadows). Accepts a preset token (`sm`/`md`/`lg`/`none`),
    /// a single tuple, named `(y: 4, blur: 8, spread: 0, color: 0x…)` or
    /// positional `(x, y, blur, spread, color)`, or an array of tuples for
    /// CSS-style layered shadows.
    pub(super) fn resolve_shadows(&mut self, attrs: &[Attr]) -> Vec<ShadowSpec> {
        let Some(value) = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "shadow" => Some(value),
            _ => None,
        }) else {
            return Vec::new();
        };
        match value {
            // Layered shadows: first-listed paints on top (CSS order), so the
            // caller emits them reversed to sit nearest.
            Expr::Array(items, _) => items
                .iter()
                .filter_map(|e| self.shadow_from_expr(e))
                .collect(),
            other => self.shadow_from_expr(other).into_iter().collect(),
        }
    }

    /// One shadow from a tuple, or a preset token; `None` for `none`/unknown.
    fn shadow_from_expr(&mut self, value: &Expr) -> Option<ShadowSpec> {
        if let Expr::Tuple(args, _) = value {
            return Some(self.shadow_from_tuple(args));
        }
        // A preset token (`sm`/`md`/`lg`); `none`/anything else → no shadow.
        let (dy, blur) = match self.eval_pure(value) {
            Value::Str(t) => match t.as_str() {
                "sm" => (1.0, 3.0),
                "md" => (3.0, 8.0),
                "lg" => (6.0, 16.0),
                _ => return None,
            },
            _ => return None,
        };
        // Preset alpha scales gently with size (sm 0x44 → lg 0x66).
        #[allow(clippy::cast_possible_truncation)]
        let alpha = (0x44 + (blur as i64 - 3) * 2).clamp(0x44, 0x66);
        Some(ShadowSpec {
            dx: 0.0,
            dy,
            blur,
            spread: 0.0,
            color: super::intrinsics::color_to_rgba(alpha << 24, true),
        })
    }

    /// Builds a [`ShadowSpec`] from a `shadow` tuple. Named fields (`x`/`dx`,
    /// `y`/`dy`, `blur`, `spread`, `color`) take any order; a positional tuple
    /// maps by slot `(x, y, blur, spread, color)`, each optional (later slots
    /// default), with `color` always the fifth slot so it is unambiguous.
    fn shadow_from_tuple(&mut self, args: &[Arg]) -> ShadowSpec {
        let mut s = ShadowSpec {
            dx: 0.0,
            dy: 0.0,
            blur: 0.0,
            spread: 0.0,
            color: super::intrinsics::color_to_rgba(DEFAULT_SHADOW_COLOR, true),
        };
        if args.iter().any(|a| a.name.is_some()) {
            for a in args {
                let Some(field) = a.name.as_ref().map(crate::Symbol::as_str) else {
                    continue;
                };
                match field {
                    "x" | "dx" => s.dx = self.eval_num(&a.value),
                    "y" | "dy" => s.dy = self.eval_num(&a.value),
                    "blur" => s.blur = self.eval_num(&a.value),
                    "spread" => s.spread = self.eval_num(&a.value),
                    "color" => s.color = self.eval_shadow_color(&a.value),
                    _ => {}
                }
            }
        } else {
            for (i, a) in args.iter().enumerate() {
                match i {
                    0 => s.dx = self.eval_num(&a.value),
                    1 => s.dy = self.eval_num(&a.value),
                    2 => s.blur = self.eval_num(&a.value),
                    3 => s.spread = self.eval_num(&a.value),
                    4 => s.color = self.eval_shadow_color(&a.value),
                    _ => {}
                }
            }
        }
        s
    }

    /// Resolves the `gradient` prop into a [`Gradient`](byard_core::frame::Gradient)
    ///, a linear colour ramp painted over the element's fill (RFC-0001 §3.1's
    /// `DecoratedBox` remit).
    ///
    /// Surface (named fields, any order, all optional):
    /// `gradient: (angle: 90deg, from: 0x00FFFFFF, mid: 0x33FFFFFF, to: 0x00FFFFFF, mid_pos: 0.5)`.
    /// A bare `mid` is what makes a *band* (transparent → bright → transparent)
    /// expressible; omit it and the ramp is an ordinary two-stop fade. The
    /// separate `gradient_offset` prop shifts the ramp along its axis and, being
    /// an ordinary numeric prop, animates through the RFC-0010/RFC-0025
    /// chokepoints, a looping offset is a travelling sweep.
    ///
    /// RFC-0035 adds a `kind`, and with it the two fields the other shapes
    /// need:
    ///
    /// ```text
    /// gradient: (kind: radial, center: (1.0, 0.0), radius: 0.9, from: …, to: …)
    /// gradient: (kind: conic, center: (0.5, 0.5), start: -90deg, from: …, mid: …, to: …)
    /// ```
    ///
    /// `kind` is absent in every file written before this, and absent means
    /// `linear`, so nothing already written changes meaning, which is the one
    /// thing RFC-0035's resolved question about the existing surface asks for.
    pub(super) fn resolve_gradient(
        &mut self,
        attrs: &[Attr],
    ) -> Option<byard_core::frame::Gradient> {
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "gradient" => Some(value),
            _ => None,
        })?;
        let value = value.clone();
        let offset = self
            .eval_float_prop(attrs, "gradient_offset")
            .map_or(0.0, |v| v as f32);
        self.resolve_gradient_expr(&value, offset)
    }

    /// The gradient a `(kind: …, from: …, to: …)` tuple describes.
    ///
    /// Split out from [`resolve_gradient`](Self::resolve_gradient) so a filled
    /// path reads its gradient through the *same* parser a box does
    /// (RFC-0037 resolved question: share the descriptor, do not fork it). The
    /// alternative is two parsers that agree until one of them learns
    /// something, which is how a path gradient and a box gradient end up
    /// meaning different things by the same words.
    pub(super) fn resolve_gradient_expr(
        &mut self,
        value: &Expr,
        offset: f32,
    ) -> Option<byard_core::frame::Gradient> {
        let Expr::Tuple(args, span) = value else {
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: value.span(),
                expected: "a gradient, e.g. `(angle: 90deg, from: 0x00FFFFFF, to: 0xFFFFFFFF)`"
                    .to_string(),
            });
            return None;
        };
        let mut angle = 0.0_f32;
        let (mut from, mut to, mut mid) = (None, None, None);
        let mut mid_pos = 0.5_f32;
        let mut kind = byard_core::frame::GradientKind::Linear;
        let mut center = [0.5_f32, 0.5_f32];
        let mut radius = 0.5_f32;
        let mut radius_written = false;
        let mut start_span = None;
        for arg in args {
            let Some(field) = arg.name.as_ref().map(crate::Symbol::as_str) else {
                self.errors.push(CompileError::ConflictingSpacingField {
                    span: *span,
                    message: "`gradient` takes named fields \
                              (angle / from / mid / to / mid_pos)"
                        .to_string(),
                });
                return None;
            };
            match field {
                "angle" => angle = self.eval_num(&arg.value),
                "from" => from = Some(self.eval_gradient_stop(&arg.value)),
                "to" => to = Some(self.eval_gradient_stop(&arg.value)),
                "mid" => mid = Some(self.eval_gradient_stop(&arg.value)),
                "mid_pos" => mid_pos = self.eval_num(&arg.value).clamp(0.0, 1.0),
                // RFC-0035. `start` is the conic sweep's own name for the
                // angle it begins at; it writes the same field, because a
                // gradient has exactly one angle and two spellings for it
                // would be two things to keep in step.
                "start" => {
                    angle = self.eval_num(&arg.value);
                    start_span = Some(arg.value.span());
                }
                "kind" => {
                    kind = match Self::enum_token(&arg.value) {
                        Some("radial") => byard_core::frame::GradientKind::Radial,
                        Some("conic") => byard_core::frame::GradientKind::Conic,
                        Some("linear") => byard_core::frame::GradientKind::Linear,
                        other => {
                            let hint = other
                                .and_then(|o| {
                                    crate::util::closest_match(o, ["linear", "radial", "conic"])
                                })
                                .map(String::from);
                            self.errors.push(CompileError::UnknownAttribute {
                                span: arg.value.span(),
                                name: format!("gradient.kind = {}", other.unwrap_or("?")),
                                hint,
                            });
                            byard_core::frame::GradientKind::Linear
                        }
                    };
                }
                "center" => {
                    let (x, y) = self.resolve_axis_pair_value(&arg.value, (center[0], center[1]));
                    center = [x, y];
                }
                "radius" => {
                    radius = self.eval_num(&arg.value);
                    radius_written = true;
                }
                unknown => {
                    let hint = crate::util::closest_match(
                        unknown,
                        [
                            "angle", "from", "mid", "to", "mid_pos", "kind", "center", "radius",
                            "start",
                        ],
                    )
                    .map(String::from);
                    self.errors.push(CompileError::UnknownAttribute {
                        span: arg.value.span(),
                        name: format!("gradient.{unknown}"),
                        hint: hint.map(|h| format!("gradient.{h}")),
                    });
                }
            }
        }
        // A ramp needs two ends; a single stop is a flat wash the caller could
        // have written as `bg`, so it is a mistake worth naming.
        let (Some(from), Some(to)) = (from, to) else {
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: *span,
                expected: "a gradient with both `from:` and `to:` colours".to_string(),
            });
            return None;
        };
        // RFC-0035 §Compiler validation, all three of it.
        if kind == byard_core::frame::GradientKind::Radial && radius <= 0.0 {
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: *span,
                expected: if radius_written {
                    "a radial gradient with a positive `radius:` (it is a \
                     fraction of the element's half-diagonal, so `0` paints \
                     nothing but the last stop)"
                        .to_string()
                } else {
                    "a radial gradient with a `radius:`".to_string()
                },
            });
        }
        // An angle outside one turn is not wrong, it is just written the long
        // way; the shader wraps anyway, and normalising here keeps the encoded
        // bytes of `start: -90deg` and `start: 270deg` identical, so the paint
        // digest sees one value rather than two spellings of it.
        if kind == byard_core::frame::GradientKind::Conic {
            let tau = std::f32::consts::TAU;
            angle = angle.rem_euclid(tau);
            let _ = start_span;
        }
        Some(byard_core::frame::Gradient {
            kind,
            angle,
            center,
            radius,
            from,
            mid: mid.unwrap_or_else(|| std::array::from_fn(|i| f32::midpoint(from[i], to[i]))),
            to,
            mid_pos,
            offset,
        })
    }

    /// Evaluates one gradient stop colour to linear RGBA. A `with`-animated stop
    /// is driven through the OKLab colour path (RFC-0010 A3) like any other
    /// animated colour, so a gradient can crossfade.
    fn eval_gradient_stop(&mut self, expr: &Expr) -> [f32; 4] {
        let packed = match expr {
            Expr::Animated {
                value: target,
                anim,
                span,
            } => self.eval_animated_color(target, anim, self.anim_key(*span)),
            other if crate::interp::anim::is_keyframes_call(other) => {
                self.eval_keyframe_color(other).unwrap_or(0)
            }
            other => self.eval_pure(other).as_int().unwrap_or(0),
        };
        super::intrinsics::color_rgba_auto(packed)
    }

    /// Evaluates a numeric shadow field (offset/blur/spread) to `f32`.
    #[allow(clippy::cast_possible_truncation)]
    pub(super) fn eval_num(&mut self, e: &Expr) -> f32 {
        match self.eval_pure(e) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => 0.0,
        }
    }

    /// Evaluates a shadow `color` field (a `0xAARRGGBB` literal) to RGBA.
    fn eval_shadow_color(&mut self, e: &Expr) -> [f32; 4] {
        let packed = self.eval_pure(e).as_int().unwrap_or(DEFAULT_SHADOW_COLOR);
        super::intrinsics::color_to_rgba(packed, true)
    }

    pub(super) fn resolve_radii(&mut self, attrs: &[Attr], name: &str) -> [f32; 4] {
        let Some(attr) = attrs.iter().find(|a| a.name.as_str() == name) else {
            return [0.0; 4];
        };
        let AttrKind::Prop { value } = &attr.kind else {
            return [0.0; 4];
        };
        match value {
            Expr::Tuple(args, span) => {
                if args.iter().any(|a| a.name.is_some()) {
                    self.errors.push(CompileError::ConflictingSpacingField {
                        span: *span,
                        message: format!(
                            "`{name}` does not accept named corner fields; use a \
                             positional quad (top_left, top_right, bottom_right, \
                             bottom_left)"
                        ),
                    });
                    return [0.0; 4];
                }
                if args.len() != 4 {
                    self.errors.push(CompileError::ArityMismatch {
                        span: *span,
                        name: name.to_string(),
                        expected: 4,
                        found: args.len(),
                    });
                    return [0.0; 4];
                }
                let mut radii = [0.0_f32; 4];
                for (slot, arg) in radii.iter_mut().zip(args) {
                    let val = self.eval_pure(&arg.value);
                    if let Some(v) = spacing_value(&val) {
                        *slot = v;
                    } else {
                        self.errors.push(CompileError::AttributeTypeMismatch {
                            span: arg.value.span(),
                            expected: "a length (an integer)".to_string(),
                        });
                    }
                }
                radii
            }
            other => {
                let val = self.eval_pure(other);
                if let Some(v) = spacing_value(&val) {
                    [v; 4]
                } else {
                    self.errors.push(CompileError::AttributeTypeMismatch {
                        span: other.span(),
                        expected: "a length (an integer)".to_string(),
                    });
                    [0.0; 4]
                }
            }
        }
    }

    /// Resolves the paint-time transform attributes (RFC-0011:
    /// `translate`/`scale`/`rotate`/`origin`; `opacity` stays on its own
    /// existing path, see the doc comment on `DecoratedBox`/`Transform` in
    /// `frame.rs` for why). `rect` is the element's own laid-out rect,
    /// logical pixels, needed to resolve a token/fractional `origin` into an
    /// absolute pivot.
    pub(super) fn resolve_transform(
        &mut self,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
    ) -> byard_core::frame::Transform {
        let translate = self.resolve_axis_pair(attrs, "translate", (0.0, 0.0));
        let scale = self.resolve_axis_pair(attrs, "scale", (1.0, 1.0));
        let rotate = self.resolve_rotate(attrs).unwrap_or(0.0);
        let origin = self.resolve_origin(attrs, rect);
        byard_core::frame::Transform {
            translate: [translate.0, translate.1],
            scale: [scale.0, scale.1],
            rotate,
            origin: [origin.0, origin.1],
            opacity: 1.0,
        }
    }

    /// Resolves a two-axis prop (`translate`/`scale`) to `(x, y)`, RFC-0011's
    /// dual surface: a bare scalar fills both axes; `(a, b)` binds positionally;
    /// `(x: a, y: b)` sets any subset, order-independent, leaving the rest at
    /// `default`. The sub-property form (`name.x: v` / `name.y: v`, a separate
    /// `Attr` with `axis: Some(_)`) then overrides individual axes on top of
    /// whatever the base `name: value` attribute (if any) already resolved,
    /// so `translate.y: 2` alone is exactly `translate: (y: 2)`.
    pub(super) fn resolve_axis_pair(
        &mut self,
        attrs: &[Attr],
        name: &str,
        default: (f32, f32),
    ) -> (f32, f32) {
        let mut result = default;
        if let Some(attr) = attrs
            .iter()
            .find(|a| a.name.as_str() == name && a.axis.is_none())
        {
            if let AttrKind::Prop { value } = &attr.kind {
                result = self.resolve_axis_pair_value(value, default);
            }
        }
        for attr in attrs
            .iter()
            .filter(|a| a.name.as_str() == name && a.axis.is_some())
        {
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            let span = value.span();
            let val = self.eval_pure(value);
            let Some(v) = spacing_value(&val) else {
                self.errors.push(CompileError::AttributeTypeMismatch {
                    span,
                    expected: "a number".to_string(),
                });
                continue;
            };
            let Some(axis) = attr.axis.as_ref() else {
                continue;
            };
            match axis.as_str() {
                "x" => result.0 = v,
                "y" => result.1 = v,
                unknown => {
                    let hint = crate::util::closest_match(unknown, ["x", "y"]).map(String::from);
                    self.errors.push(CompileError::UnknownAttribute {
                        span: attr.span,
                        name: format!("{name}.{unknown}"),
                        hint: hint.map(|h| format!("{name}.{h}")),
                    });
                }
            }
        }
        result
    }

    /// Parses one `translate`/`scale`-shaped [`Expr`] (scalar, positional
    /// tuple, or named tuple) into `(x, y)`, the value-shape half of
    /// [`Self::resolve_axis_pair`], factored out so [`Self::resolve_origin`]
    /// can reuse the exact same tuple grammar for its own fractional pair.
    fn resolve_axis_pair_value(&mut self, value: &Expr, default: (f32, f32)) -> (f32, f32) {
        match value {
            Expr::Tuple(args, span) => {
                let any_named = args.iter().any(|a| a.name.is_some());
                let all_named = args.iter().all(|a| a.name.is_some());
                if any_named && !all_named {
                    self.errors.push(CompileError::ConflictingSpacingField {
                        span: *span,
                        message: "cannot mix named and positional fields".to_string(),
                    });
                    return default;
                }
                if all_named {
                    let (mut x, mut y) = (None, None);
                    for arg in args {
                        let Some(name) = &arg.name else { continue };
                        let span = arg.value.span();
                        let val = self.eval_pure(&arg.value);
                        let Some(v) = spacing_value(&val) else {
                            self.errors.push(CompileError::AttributeTypeMismatch {
                                span,
                                expected: "a number".to_string(),
                            });
                            continue;
                        };
                        match name.as_str() {
                            "x" => assign_side(&mut x, v, "x", span, &mut self.errors),
                            "y" => assign_side(&mut y, v, "y", span, &mut self.errors),
                            unknown => {
                                let hint = crate::util::closest_match(unknown, ["x", "y"])
                                    .map(String::from);
                                self.errors.push(CompileError::UnknownAttribute {
                                    span,
                                    name: unknown.to_string(),
                                    hint,
                                });
                            }
                        }
                    }
                    (x.unwrap_or(default.0), y.unwrap_or(default.1))
                } else if args.len() == 2 {
                    let x = self.eval_pure(&args[0].value);
                    let y = self.eval_pure(&args[1].value);
                    let x = spacing_value(&x).unwrap_or_else(|| {
                        self.errors.push(CompileError::AttributeTypeMismatch {
                            span: args[0].value.span(),
                            expected: "a number".to_string(),
                        });
                        default.0
                    });
                    let y = spacing_value(&y).unwrap_or_else(|| {
                        self.errors.push(CompileError::AttributeTypeMismatch {
                            span: args[1].value.span(),
                            expected: "a number".to_string(),
                        });
                        default.1
                    });
                    (x, y)
                } else {
                    self.errors.push(CompileError::ArityMismatch {
                        span: *span,
                        name: "translate/scale/origin".to_string(),
                        expected: 2,
                        found: args.len(),
                    });
                    default
                }
            }
            other => {
                let val = self.eval_pure(other);
                if let Some(v) = spacing_value(&val) {
                    (v, v)
                } else if let Some(pair) = axis_pair_of_value(&val, default) {
                    // A *computed* pair, what keyframed coordinates resolve to
                    // (RFC-0025: `translate: anim.keyframes(0%: (-100, 0), …)`
                    // blends component-wise and arrives here as a tuple value).
                    pair
                } else {
                    self.errors.push(CompileError::AttributeTypeMismatch {
                        span: other.span(),
                        expected: "a number".to_string(),
                    });
                    default
                }
            }
        }
    }

    /// Resolves `rotate` (RFC-0011): the terse `rotate: 90deg` form or the
    /// verbose `rotate: (angle: 90deg)` single-field tuple, both already
    /// canonicalized to radians by the lexer's `Expr::AngleLit`. Absent →
    /// `None` (caller defaults to `0.0`, no rotation).
    fn resolve_rotate(&mut self, attrs: &[Attr]) -> Option<f32> {
        let attr = attrs
            .iter()
            .find(|a| a.name.as_str() == "rotate" && a.axis.is_none())?;
        let AttrKind::Prop { value } = &attr.kind else {
            return None;
        };
        let inner = match value {
            Expr::Tuple(args, _)
                if args.len() == 1
                    && args[0].name.as_ref().map(Symbol::as_str) == Some("angle") =>
            {
                &args[0].value
            }
            other => other,
        };
        let val = self.eval_pure(inner);
        let Some(rad) = spacing_value(&val) else {
            // A non-numeric `rotate` (e.g. `rotate: center`, or a reactive var
            // that didn't resolve to a number) is a real mistake, not a no-op,
            // flag it the same way `translate`/`scale` flag theirs instead of
            // silently painting with no rotation.
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: inner.span(),
                expected: "an angle (e.g. 90deg, 1.5rad)".to_string(),
            });
            return None;
        };
        Some(rad)
    }

    /// Resolves `origin` (RFC-0011 T2) to an absolute logical-pixel pivot in
    /// the same coordinate space as `rect`: a named token (`center` and the
    /// four corners/edges), or a fractional `(fx, fy)` tuple relative to
    /// `rect` (positional or named, reusing [`Self::resolve_axis_pair_value`]'s
    /// tuple grammar). Absent, or an unrecognized token, defaults to `center`
    ///, RFC-0011's own stated default, rather than hard-failing.
    ///
    /// Deliberately out of scope for now: the `px` absolute-origin suffix
    /// (T2's third form) needs a new lexer literal this slice doesn't add;
    /// only the token and fractional forms are implemented.
    fn resolve_origin(
        &mut self,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
    ) -> (f32, f32) {
        let center = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let Some(attr) = attrs
            .iter()
            .find(|a| a.name.as_str() == "origin" && a.axis.is_none())
        else {
            return center;
        };
        let AttrKind::Prop { value } = &attr.kind else {
            return center;
        };
        if let Expr::Ident(sym, span) = value {
            const TOKENS: &[&str] = &[
                "center",
                "top_left",
                "top_right",
                "bottom_left",
                "bottom_right",
                "top",
                "bottom",
                "left",
                "right",
            ];
            return match sym.as_str() {
                "center" => center,
                "top_left" => (rect.x, rect.y),
                "top_right" => (rect.x + rect.w, rect.y),
                "bottom_left" => (rect.x, rect.y + rect.h),
                "bottom_right" => (rect.x + rect.w, rect.y + rect.h),
                "top" => (rect.x + rect.w * 0.5, rect.y),
                "bottom" => (rect.x + rect.w * 0.5, rect.y + rect.h),
                "left" => (rect.x, rect.y + rect.h * 0.5),
                "right" => (rect.x + rect.w, rect.y + rect.h * 0.5),
                unknown => {
                    let hint = crate::util::closest_match(unknown, TOKENS.iter().copied())
                        .map(String::from);
                    self.errors.push(CompileError::UnknownAttribute {
                        span: *span,
                        name: format!("origin: {unknown}"),
                        hint,
                    });
                    center
                }
            };
        }
        let (fx, fy) = self.resolve_axis_pair_value(value, (0.5, 0.5));
        (rect.x + fx * rect.w, rect.y + fy * rect.h)
    }
}
