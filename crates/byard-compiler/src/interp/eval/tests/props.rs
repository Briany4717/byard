//! Resolving props to values: padding, radius, transforms, shadows, gradients,
//! colours with an alpha byte, the decorated box and the theme tokens.

use super::animation::sample_over_time;
use super::paint::rendered_frame;
use super::*;

// ── `Len` padding/margin forms ──────────────────────────────────

/// Lowers a single-`Box` view and returns the resolved padding plus any
/// errors raised during style resolution.
fn resolve_padding(src: &str) -> (byard_core::atlas::layout::Spacing, Vec<CompileError>) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    let RenderNode::Box { name, attrs, .. } = &tree[0] else {
        panic!("expected a Box");
    };
    let style = interp.eval_container_style(name.as_str(), attrs);
    (style.padding, interp.errors().to_vec())
}

#[test]
fn impl30_scalar_sets_all_sides() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: 5] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(p, Spacing::all(5.0));
}

#[test]
fn impl30_pair_is_vertical_horizontal() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (10, 5)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    // (vertical, horizontal): top=bottom=10, left=right=5.
    assert_eq!(
        p,
        Spacing {
            top: 10.0,
            right: 5.0,
            bottom: 10.0,
            left: 5.0
        }
    );
}

#[test]
fn impl30_quad_is_css_order() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (4, 6, 8, 7)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        p,
        Spacing {
            top: 4.0,
            right: 6.0,
            bottom: 8.0,
            left: 7.0
        }
    );
}

#[test]
fn impl30_named_single_side_defaults_rest_to_zero() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (bottom: 7)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        p,
        Spacing {
            top: 0.0,
            right: 0.0,
            bottom: 7.0,
            left: 0.0
        }
    );
}

#[test]
fn impl30_named_axis_shorthands() {
    use byard_core::atlas::layout::Spacing;
    let (p, errs) = resolve_padding("View C() { Column #[p: (horizontal: 10, vertical: 5)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        p,
        Spacing {
            top: 5.0,
            right: 10.0,
            bottom: 5.0,
            left: 10.0
        }
    );
}

#[test]
fn impl30_unknown_side_is_unknown_attribute_with_hint() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (tpo: 4)] {} }");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::UnknownAttribute { name, hint: Some(h), .. }
                if name == "tpo" && h == "top"
        )),
        "expected UnknownAttribute(tpo)->top, got {errs:?}"
    );
}

#[test]
fn impl30_axis_plus_component_conflicts() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (horizontal: 10, left: 3)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ConflictingSpacingField { .. })),
        "expected ConflictingSpacingField, got {errs:?}"
    );
}

#[test]
fn impl30_non_int_side_is_type_mismatch() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (top: 4, left: \"x\")] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::AttributeTypeMismatch { .. })),
        "expected AttributeTypeMismatch, got {errs:?}"
    );
}

#[test]
fn impl30_wrong_positional_arity_is_arity_mismatch() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (1, 2, 3)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { .. })),
        "expected ArityMismatch for a 3-tuple, got {errs:?}"
    );
}

#[test]
fn impl30_mixing_named_and_positional_errors() {
    let (_p, errs) = resolve_padding("View C() { Column #[p: (10, top: 4)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ConflictingSpacingField { .. })),
        "expected a conflict for mixed named/positional, got {errs:?}"
    );
}

#[test]
fn impl30_px_py_are_now_unknown_attributes() {
    let parsed = parse("View C() { Column #[px: 5] {} }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(view, &[]);
    assert!(
        interp.errors().iter().any(|e| matches!(
            e,
            CompileError::UnknownAttribute { name, .. } if name == "px"
        )),
        "px must now be UnknownAttribute, got {:?}",
        interp.errors()
    );
}

// ── Per-corner `radius` ──────────────────────────────────────────────

/// Lowers a single-element view and returns `resolve_radii`'s result for
/// its `radius` attribute alongside any errors raised.
fn resolve_radius_test(src: &str) -> ([f32; 4], Vec<CompileError>) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let attrs = element(&view.body[0]).attrs.clone();
    let radii = interp.resolve_radii(&attrs, "radius");
    (radii, interp.errors().to_vec())
}

#[test]
fn impl44_radius_scalar_broadcasts_to_all_four_corners() {
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: 16] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(radii, [16.0, 16.0, 16.0, 16.0]);
}

#[test]
fn impl44_radius_quad_sets_independent_corners_in_css_order() {
    // top_left, top_right, bottom_right, bottom_left (frame.rs / WGSL convention).
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: (4, 8, 12, 16)] {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(radii, [4.0, 8.0, 12.0, 16.0]);
}

#[test]
fn impl44_radius_missing_attribute_defaults_to_zero() {
    let (radii, errs) = resolve_radius_test("View C() { Column {} }");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(radii, [0.0; 4]);
}

#[test]
fn impl44_radius_wrong_arity_is_arity_mismatch() {
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: (4, 8)] {} }");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            CompileError::ArityMismatch {
                expected: 4,
                found: 2,
                ..
            }
        )),
        "expected ArityMismatch(4, found 2), got {errs:?}"
    );
    assert_eq!(radii, [0.0; 4]);
}

#[test]
fn impl44_radius_named_corner_field_is_rejected() {
    let (radii, errs) = resolve_radius_test(
        "View C() { Column #[radius: (top_left: 4, top_right: 8, bottom_right: 12, bottom_left: 16)] {} }",
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::ConflictingSpacingField { .. })),
        "expected ConflictingSpacingField for named corners, got {errs:?}"
    );
    assert_eq!(radii, [0.0; 4]);
}

#[test]
fn impl44_radius_non_numeric_corner_is_type_mismatch() {
    let (radii, errs) = resolve_radius_test("View C() { Column #[radius: (4, \"x\", 12, 16)] {} }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, CompileError::AttributeTypeMismatch { .. })),
        "expected AttributeTypeMismatch, got {errs:?}"
    );
    // Valid corners still resolve; the bad one is left at the [0.0;4]
    // default rather than aborting the whole tuple.
    assert_eq!(radii, [4.0, 0.0, 12.0, 16.0]);
}

#[test]
fn impl44_decorated_box_carries_independent_corner_radii_into_box_instance() {
    // End-to-end: a quad `radius` on a Box that also has `bg` (so it's a
    // plain BoxInstance push, not a DecoratedBox) reaches the GPU instance
    // with all four corners intact rather than being collapsed to a scalar.
    let parsed =
        parse("View C() { Box #[bg: 0xFF0000, radius: (4, 8, 12, 16), width: 50, height: 50] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let instances = frame.instances();
    assert_eq!(instances.len(), 1, "expected exactly one BoxInstance");
    assert_eq!(instances[0].radii, [4.0, 8.0, 12.0, 16.0]);
}

#[test]
fn sub_property_axis_sets_one_axis_and_leaves_the_other_default() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50, translate.y: 7] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(frame.instances()[0].transform.translate, [0.0, 7.0]);
}

#[test]
fn named_tuple_scale_sets_one_axis_and_leaves_the_other_at_one() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50, scale: (y: 2.0)] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(frame.instances()[0].transform.scale, [1.0, 2.0]);
}

#[test]
fn origin_token_resolves_relative_to_the_laid_out_rect() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 40, height: 20, origin: top_left] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    // The Box lays out at the view's origin (0,0) by default.
    assert_eq!(frame.instances()[0].transform.origin, [0.0, 0.0]);
}

#[test]
fn unknown_origin_token_is_a_compile_error_with_a_hint() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50, origin: centre] }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(matches!(
        &interp.errors()[0],
        CompileError::UnknownAttribute { hint: Some(h), .. } if h.contains("center")
    ));
}

// ── DecoratedBox / TextureSampler ───────────────────────────────

#[test]
fn image_lowers_to_texture_sampler_in_frame() {
    let parsed =
        parse("View C() {\n Image(\"photo.jpg\") #[fit: \"cover\", width: 200, height: 150]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!(
        matches!(tree[0], RenderNode::Image { .. }),
        "Image element lowers to RenderNode::Image"
    );

    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.textures().len(), 1, "one TextureSampler in frame");
    let tex = &frame.textures()[0];
    assert_eq!(tex.src, "photo.jpg");
    assert_eq!(tex.fit, byard_core::frame::ImageFit::Cover);
}

#[test]
fn image_fit_defaults_to_fill() {
    let parsed = parse("View C() {\n Image(\"img.png\")\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.textures()[0].fit, byard_core::frame::ImageFit::Fill);
}

#[test]
fn box_with_border_becomes_decorated_box() {
    // `border` is the catalog Color attr; it yields a 2px ring.
    let parsed = parse("View C() {\n Box #[bg: 0xffffff, border: 0x000000]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // A bordered container splits into an opaque SolidBox fill
    // (so it stays behind its children, which also paint as solids) plus a
    // decorated *overlay* whose interior is transparent and only strokes the
    // 2px border, it can't occlude the children drawn beneath it.
    assert_eq!(
        frame.instances().len(),
        1,
        "opaque fill on the SolidBox pass"
    );
    assert_eq!(
        frame.instances()[0].color,
        [1.0, 1.0, 1.0, 1.0],
        "the fill carries the bg colour"
    );
    assert_eq!(frame.decorated().len(), 1, "border overlay → DecoratedBox");
    assert!((frame.decorated()[0].border_width - 2.0).abs() < f32::EPSILON);
    assert_eq!(
        frame.decorated()[0].base.color,
        [0.0; 4],
        "the overlay interior is transparent so children stay visible"
    );
}

#[test]
fn bordered_container_paints_fill_before_its_child_widget() {
    // The regression behind the "widgets invisible" report: an opaque,
    // bordered card must NOT paint over the solid boxes of the widgets it
    // contains. The card's fill is a SolidBox pushed *before*
    // the child's, and the only decorated primitive is a transparent-interior
    // border overlay, so the child's fill is never occluded.
    let parsed = parse(
        "View C() {\n Column #[bg: 0x222233, border: 0x445566] {\n Box #[bg: 0xFF0000, width: 20, height: 20]\n }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Two solid fills: the card, then the child, in that paint order, so
    // the child (drawn second) lands on top of the card, not under it.
    assert_eq!(frame.instances().len(), 2, "card fill + child fill");
    assert_ne!(
        frame.instances()[0].color,
        [1.0, 0.0, 0.0, 1.0],
        "the first solid fill is the card, not the child"
    );
    assert_eq!(
        frame.instances()[1].color,
        [1.0, 0.0, 0.0, 1.0],
        "the child's red fill paints last (on top)"
    );
    // Every decorated primitive in this frame is a transparent-interior
    // overlay, so nothing opaque is layered above the child.
    assert!(
        frame.decorated().iter().all(|d| d.base.color[3] == 0.0),
        "all decorated overlays have transparent interiors"
    );
}

#[test]
fn box_with_shadow_token_becomes_decorated_box() {
    let parsed = parse("View C() {\n Box #[bg: 0x222222, shadow: \"md\"]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.decorated().len(), 1, "shadowed box → DecoratedBox");
    assert!(frame.decorated()[0].shadow_blur > 0.0);
    assert!(
        frame.decorated()[0].shadow_color[3] > 0.0,
        "shadow is translucent"
    );
}

/// Renders `src`'s first view and returns the frame's decorated boxes'
/// shadow triples `(dy, blur, spread)`, for the custom-shadow tests below.
fn shadow_params(src: &str) -> Vec<(f32, f32, f32)> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
        .decorated()
        .iter()
        .map(|d| (d.shadow_dy, d.shadow_blur, d.shadow_spread))
        .collect()
}

#[test]
fn named_custom_shadow_sets_offset_blur_and_spread() {
    let got = shadow_params(
        "View C() { Box #[bg: 0x222222, shadow: (y: 6, blur: 12, spread: 3, color: 0x80000000)] {} }",
    );
    assert_eq!(got.len(), 1, "one shadow instance beneath the fill");
    let (dy, blur, spread) = got[0];
    assert!((dy - 6.0).abs() < 0.01, "dy={dy}");
    assert!((blur - 12.0).abs() < 0.01, "blur={blur}");
    assert!((spread - 3.0).abs() < 0.01, "spread={spread}");
}

#[test]
fn positional_shadow_maps_x_y_blur_spread_color_by_slot() {
    let got =
        shadow_params("View C() { Box #[bg: 0x222222, shadow: (0, 4, 8, 2, 0x80000000)] {} }");
    assert_eq!(got.len(), 1);
    let (dy, blur, spread) = got[0];
    assert!((dy - 4.0).abs() < 0.01 && (blur - 8.0).abs() < 0.01 && (spread - 2.0).abs() < 0.01);
}

#[test]
fn layered_shadows_emit_one_instance_each() {
    let got = shadow_params(
        "View C() { Box #[bg: 0x222222, shadow: [(y: 2, blur: 4), (y: 8, blur: 16)]] {} }",
    );
    assert_eq!(got.len(), 2, "two layered shadows → two instances");
    let mut blurs: Vec<f32> = got.iter().map(|s| s.1).collect();
    blurs.sort_by(f32::total_cmp);
    assert!((blurs[0] - 4.0).abs() < 0.01 && (blurs[1] - 16.0).abs() < 0.01);
}

#[test]
fn shadow_none_and_absent_emit_no_shadow() {
    assert!(
        shadow_params("View C() { Box #[bg: 0x222222] {} }").is_empty(),
        "{:?}",
        shadow_params("View C() { Box #[bg: 0x222222] {} }")
    );
    assert!(
        shadow_params("View C() { Box #[bg: 0x222222, shadow: \"none\"] {} }").is_empty(),
        "{:?}",
        shadow_params("View C() { Box #[bg: 0x222222, shadow: \"none\"] {} }")
    );
}

// ── Theme system ────────────────────────────────────────────────

#[test]
fn text_without_color_uses_theme_on_surface() {
    let parsed = parse("View C() {\n Text(\"hi\")\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let expected_color = crate::interp::intrinsics::color_to_rgba(interp.theme.on_surface(), false);
    assert_eq!(
        frame.texts()[0].color,
        expected_color,
        "no-color Text gets theme on_surface"
    );
}

#[test]
fn typo_token_resolves_to_concrete_size() {
    let parsed = parse("View C() {\n Text(\"hi\") #[typo: \"titleLarge\"]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert!(
        (frame.texts()[0].font_size - 22.0).abs() < f32::EPSILON,
        "titleLarge → 22pt, got {}",
        frame.texts()[0].font_size
    );
}

// ── RFC-0022: theme runtime (injected reactive tokens) ────────────────

/// Lowers `src`'s first view against `theme` (installed as the ambient
/// `Theme`, RFC-0022), ticks, and renders one frame.
fn theme_render(interp: &mut Interpreter, src: &str) -> byard_core::frame::RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
}

#[test]
fn injected_theme_color_token_paints_and_flips_with_scheme() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let src = "View C() {\n inject Theme as t\n Column #[bg: t.primary] {}\n}";

    let frame = theme_render(&mut interp, src);
    let light = frame.instances()[0].color;
    let expected_light = crate::interp::intrinsics::color_to_rgba(
        interp.theme.color("primary", false).unwrap(),
        false,
    );
    assert_eq!(
        light, expected_light,
        "light scheme paints the light primary"
    );

    // Flip the scheme, a single reactive write, and re-render.
    interp.set_theme_dark(true);
    let mut frame2 = byard_core::frame::RenderFrame::new();
    // Re-lower against the same env so the injected `t` still resolves.
    let tree = interp.lower_view(&parse(src).views[0], &[]);
    interp.tick();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    let dark = frame2.instances()[0].color;
    let expected_dark = crate::interp::intrinsics::color_to_rgba(
        interp.theme.color("primary", true).unwrap(),
        false,
    );
    assert_eq!(dark, expected_dark, "dark scheme paints the dark primary");
    assert_ne!(light, dark, "flipping the scheme recolours the box");
}

#[test]
fn theme_typo_accessor_sets_font_size() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Text(\"hi\") #[typo: t.titleLarge]\n}",
    );
    assert!(
        (frame.texts()[0].font_size - 22.0).abs() < f32::EPSILON,
        "t.titleLarge → 22pt, got {}",
        frame.texts()[0].font_size
    );
}

#[test]
fn theme_shape_accessor_sets_corner_radius() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Box #[bg: 0x222222, radius: t.cornerLg] {}\n}",
    );
    assert!(
        (frame.instances()[0].radii[0] - 16.0).abs() < f32::EPSILON,
        "t.cornerLg → 16px radius, got {:?}",
        frame.instances()[0].radii
    );
}

#[test]
fn manifest_custom_token_resolves_over_base() {
    let mut theme = super::super::theme::Theme::byard_base();
    theme.set_color("light", "primary", 0x0012_3456);
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Column #[bg: t.primary] {}\n}",
    );
    assert_eq!(
        frame.instances()[0].color,
        crate::interp::intrinsics::color_to_rgba(0x0012_3456, false),
        "a manifest-overridden token wins over byard-base"
    );
}

#[test]
fn unknown_theme_token_is_a_compile_error() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let parsed = parse("View C() {\n inject Theme as t\n Column #[bg: t.nope] {}\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    // The bad token surfaces when the `bg` prop is evaluated at render time.
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::UnknownThemeToken { field, .. } if field == "nope")),
        "t.nope → UnknownThemeToken, got {:?}",
        interp.errors()
    );
}

#[test]
fn theme_dark_is_assignable_and_bindable() {
    // `t.dark = …` (assign) and `bind: t.dark` (Toggle) must both resolve to
    // the scheme signal, neither is `NotAssignable` (RFC-0022 §1).
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let _ = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n \
             Column {\n Button(\"x\") => t.dark = true\n \
             Toggle #[bind: t.dark]\n }\n}",
    );
    assert!(
        interp.errors().is_empty(),
        "assignable/bindable theme.dark should not error: {:?}",
        interp.errors()
    );
}

#[test]
fn theme_mode_string_reflects_active_scheme() {
    let mut interp = Interpreter::new();
    interp.set_theme(super::super::theme::Theme::byard_base());
    let frame = theme_render(
        &mut interp,
        "View C() {\n inject Theme as t\n Text(\"{t.mode}\")\n}",
    );
    assert_eq!(frame.texts()[0].text, "light");
    assert!(!interp.theme_is_dark());
    interp.set_theme_dark(true);
    assert!(interp.theme_is_dark());
}

#[test]
fn inline_size_overrides_typo_token() {
    let parsed = parse("View C() {\n Text(\"hi\") #[typo: \"titleLarge\", size: 30]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert!(
        (frame.texts()[0].font_size - 30.0).abs() < f32::EPSILON,
        "inline size: 30 overrides typo token"
    );
}

#[test]
fn plain_box_stays_as_box_instance() {
    let parsed = parse("View C() {\n Box #[bg: 0x111111]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.instances().len(), 1, "plain box → BoxInstance");
    assert_eq!(frame.decorated().len(), 0);
}

// ── Gradient fills (RFC-0001 §3.1) ──────────────────────────────────

#[test]
fn a_gradient_promotes_the_box_and_resolves_its_ramp() {
    let (_interp, frame) = rendered_frame(
        "View V() { Box #[width: 200, height: 40, bg: 0x2B2930, \
             gradient: (angle: 90deg, from: 0x00FFFFFF, mid: 0x40FFFFFF, to: 0x00FFFFFF, \
             mid_pos: 0.25), gradient_offset: 0.5] {} }",
    );
    assert!(
        frame.instances().is_empty(),
        "a gradient promotes the box off the flat SolidBox path"
    );
    let g = frame.decorated()[0]
        .gradient
        .expect("the ramp reaches the frame");
    assert!(
        (g.angle - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
        "90deg is canonicalized to radians by the lexer, got {}",
        g.angle
    );
    assert!((g.mid_pos - 0.25).abs() < 1e-6);
    assert!((g.offset - 0.5).abs() < 1e-6);
    // Straight-alpha stops: a transparent white end and a 25 %-alpha middle.
    assert!(
        g.from[3] < 0.01 && g.to[3] < 0.01,
        "the ends are transparent"
    );
    assert!(
        (g.mid[3] - 0.25).abs() < 0.01,
        "the band's alpha, got {:?}",
        g.mid
    );
    assert!(g.mid[0] > 0.99, "…and it is white");
}

#[test]
fn an_omitted_mid_stop_is_the_midpoint_of_the_two_ends() {
    let (_interp, frame) = rendered_frame(
        "View V() { Box #[width: 100, height: 20, \
             gradient: (from: 0xFF000000, to: 0xFFFFFFFF)] {} }",
    );
    let g = frame.decorated()[0].gradient.unwrap();
    // A two-stop ramp: the implicit middle is exactly halfway, so the ramp
    // is indistinguishable from a plain linear gradient.
    for channel in 0..3 {
        assert!(
            (g.mid[channel] - f32::midpoint(g.from[channel], g.to[channel])).abs() < 1e-6,
            "channel {channel} must be the midpoint"
        );
    }
    assert!(
        !frame.decorated()[0].base.color[3].is_nan(),
        "a gradient with no `bg` still paints a surface"
    );
}

#[test]
fn a_gradient_offset_animates_like_any_other_number() {
    // The travelling-sweep case: `gradient_offset` is an ordinary numeric
    // prop, so RFC-0025's looping keyframes drive it with no extra plumbing.
    let src = "View V() { Box #[width: 100, height: 20, bg: 0x222222, \
                   gradient: (from: 0x00FFFFFF, mid: 0x40FFFFFF, to: 0x00FFFFFF), \
                   gradient_offset: anim.keyframes(0%: 0.0, 100%: 1.0, \
                   duration: 400ms, loop: true)] {} }";
    let seen = sample_over_time(src, &[0, 200, 400], |f| {
        f.decorated()[0].gradient.map_or(-1.0, |g| g.offset)
    });
    assert!((seen[0].0).abs() < 0.01, "starts at 0, got {}", seen[0].0);
    assert!((seen[1].0 - 0.5).abs() < 0.05, "halfway, got {}", seen[1].0);
    assert!((seen[2].0).abs() < 0.01, "wrapped, got {}", seen[2].0);
    assert!(
        seen.iter().all(|(_, active)| *active),
        "a loop keeps frames coming"
    );
}

#[test]
fn a_malformed_gradient_is_reported_not_ignored() {
    let errs = |src: &str| {
        let parsed = parse(src);
        let mut interp = Interpreter::new();
        let tree = interp.lower_view(&parsed.views[0], &[]);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        interp.errors().to_vec()
    };
    // A single end is a flat wash the author could have written as `bg`.
    assert!(matches!(
        errs("View V() { Box #[width: 10, height: 10, gradient: (from: 0xFF0000)] {} }").first(),
        Some(CompileError::AttributeTypeMismatch { .. })
    ));
    // A misspelt field names the fix instead of silently doing nothing.
    assert!(matches!(
        errs("View V() { Box #[width: 10, height: 10, \
              gradient: (from: 0xFF0000, to: 0x00FF00, angel: 90deg)] {} }")
            .first(),
        Some(CompileError::UnknownAttribute { hint: Some(h), .. }) if h == "gradient.angle"
    ));
    // Not a tuple at all.
    assert!(matches!(
        errs("View V() { Box #[width: 10, height: 10, gradient: 0xFF0000] {} }").first(),
        Some(CompileError::AttributeTypeMismatch { .. })
    ));
}

// ── 8-digit colours: the alpha byte reaches the fill (RFC-0005 §1) ──────

/// Renders `src` and returns every box fill it emitted, solid and decorated,
/// each paired with whether it went to the blended (decorated) pass.
fn box_fills(src: &str) -> Vec<([f32; 4], bool)> {
    let mut interp = Interpreter::new();
    let frame = theme_render(&mut interp, src);
    frame
        .instances()
        .iter()
        .map(|b| (b.color, false))
        .chain(frame.decorated().iter().map(|d| {
            let mut c = d.base.color;
            c[3] *= d.opacity;
            (c, true)
        }))
        .collect()
}

#[test]
fn eight_digit_bg_on_a_box_keeps_its_alpha_and_blends() {
    let fills = box_fills("View C() {\n Box #[bg: 0x801C2430, width: 40, height: 40] {}\n}");
    let [(color, blended)] = fills.as_slice() else {
        panic!("one fill for one box, got {fills:?}");
    };
    assert!(
        (color[3] - 128.0 / 255.0).abs() < 1e-3,
        "0x80RRGGBB paints at ~half alpha, got {color:?}"
    );
    let rgb = crate::interp::intrinsics::color_to_rgba(0x001C_2430, false);
    assert!(
        color[..3]
            .iter()
            .zip(&rgb[..3])
            .all(|(a, b)| (a - b).abs() < 1e-6),
        "the RGB is the low three bytes, got {color:?} want {rgb:?}"
    );
    // A translucent fill on the depth-writing solid pass would cull whatever
    // later passes draw beneath it; it has to take the blended path.
    assert!(blended, "a translucent bg goes to the decorated pass");
}

#[test]
fn opaque_bg_stays_on_the_solid_path() {
    // The control: 6-digit and an explicit `FF` alpha byte both stay flat.
    for src in [
        "View C() {\n Box #[bg: 0x1C2430, width: 40, height: 40] {}\n}",
        "View C() {\n Box #[bg: 0xFF1C2430, width: 40, height: 40] {}\n}",
    ] {
        let fills = box_fills(src);
        let [(color, blended)] = fills.as_slice() else {
            panic!("one fill for one box, got {fills:?}");
        };
        assert!(
            (color[3] - 1.0).abs() < 1e-6,
            "{src}: opaque, got {color:?}"
        );
        assert!(!blended, "{src}: an opaque bg stays on SolidBox");
    }
}

#[test]
fn zero_alpha_byte_is_transparent_not_opaque() {
    // `0x00RRGGBB` written with eight digits is alpha 0; the lexer's tag is
    // what tells it apart from the 6-digit `0xRRGGBB`.
    let fills = box_fills("View C() {\n Box #[bg: 0x001C2430, width: 40, height: 40] {}\n}");
    assert!(
        fills.iter().all(|(c, _)| c[3].abs() < 1e-6),
        "an 8-digit 00 alpha is fully transparent, got {fills:?}"
    );
}

#[test]
fn eight_digit_border_keeps_its_alpha() {
    let mut interp = Interpreter::new();
    let frame = theme_render(
        &mut interp,
        "View C() {\n Box #[bg: 0x1C2430, border: 0x40FFFFFF, border_width: 2, \
             width: 40, height: 40] {}\n}",
    );
    let border = frame
        .decorated()
        .iter()
        .find(|d| d.border_width > 0.0)
        .expect("a border overlay")
        .border_color;
    assert!(
        (border[3] - 64.0 / 255.0).abs() < 1e-3,
        "0x40RRGGBB border at ~quarter alpha, got {border:?}"
    );
}

#[test]
fn eight_digit_text_color_keeps_its_alpha() {
    let mut interp = Interpreter::new();
    let frame = theme_render(
        &mut interp,
        "View C() {\n Text(\"hi\") #[color: 0x80FFFFFF]\n}",
    );
    let color = frame.texts()[0].color;
    assert!(
        (color[3] - 128.0 / 255.0).abs() < 1e-3,
        "0x80RRGGBB text at ~half alpha, got {color:?}"
    );
}

#[test]
fn eight_digit_canvas_bg_keeps_its_alpha_and_blends() {
    let fills = box_fills("View C() {\n Canvas #[bg: 0x801C2430, width: 40, height: 40] {}\n}");
    let [(color, blended)] = fills.as_slice() else {
        panic!("one fill for one canvas, got {fills:?}");
    };
    assert!(
        (color[3] - 128.0 / 255.0).abs() < 1e-3,
        "0x80RRGGBB canvas bg at ~half alpha, got {color:?}"
    );
    assert!(
        blended,
        "a translucent canvas bg goes to the decorated pass"
    );
}

#[test]
fn eval_pure_answers_literals_with_the_value_lowering_would_give() {
    // `eval_pure` short-circuits numeric literals and all-literal tuples; the
    // value must be the one the lowered closure produces.
    let parsed = parse(
        "View V() { Box #[scale: 2, opacity: 0.5, rotate: 90deg, translate: (x: 4, y: -2.5), \
         width: (1, 2 + 3)] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let el = element(&parsed.views[0].body[0]);
    let mut interp = Interpreter::new();
    for attr in &el.attrs {
        let AttrKind::Prop { value } = &attr.kind else {
            panic!("expected a property");
        };
        let fast = interp.eval_pure(value);
        let mut lowered = interp.lower_expr(value, None);
        let slow = lowered(&mut interp.ctx);
        assert_eq!(
            format!("{fast:?}"),
            format!("{slow:?}"),
            "{}",
            attr.name.as_str()
        );
    }
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
}
