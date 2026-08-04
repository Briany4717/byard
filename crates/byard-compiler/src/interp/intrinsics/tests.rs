
use super::*;
use crate::parser::ast::Member;
use crate::parser::parse;

fn first_element(src: &str) -> ElementNode {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    match parsed
        .views
        .into_iter()
        .next()
        .unwrap()
        .body
        .into_iter()
        .next()
        .unwrap()
    {
        Member::Element(e) => e,
        _ => panic!("expected element"),
    }
}

fn errs(src: &str) -> Vec<CompileError> {
    let el = first_element(src);
    validate_element(&el, &el.attrs, &[])
}

#[test]
fn valid_intrinsics_pass() {
    assert!(errs("View V() { Text(\"hi\") #[color: 0xFFFFFF, align: center] }").is_empty());
    assert!(errs("View V() { Column #[gap: 8, p: 16] { } }").is_empty());
    assert!(errs("View V() { Button(\"+\") #[bg: 0x3B82F6] => x }").is_empty());
}

#[test]
fn transform_props_are_accepted_on_containers_but_not_text_or_image() {
    assert!(
        errs(
            "View V() { Box #[translate: (0, 2), scale: 1.05, rotate: 90deg, origin: center] {} }"
        )
        .is_empty()
    );
    assert!(
        errs("View V() { Row #[scale.y: 1.2] {} }").is_empty(),
        "sub-property axis form"
    );

    // `Text`/`Image` don't have a `Transform` field on their engine
    // primitives yet (RFC-0011 engine-slice decision log), these must
    // still report `UnknownAttribute`, not silently accept and drop.
    let e = errs("View V() { Text(\"hi\") #[rotate: 90deg] }");
    assert!(matches!(&e[0], CompileError::UnknownAttribute { .. }));
    let e = errs("View V() { Image(\"x\") #[translate: (0, 2)] }");
    assert!(matches!(&e[0], CompileError::UnknownAttribute { .. }));
}

#[test]
fn rotate_rejects_a_bare_number_without_a_deg_or_rad_suffix() {
    let e = errs("View V() { Box #[rotate: 90] {} }");
    assert!(matches!(&e[0], CompileError::AttributeTypeMismatch { .. }));
}

#[test]
fn rotate_verbose_form_still_rejects_a_bare_number() {
    // The verbose `(angle: N)` wrapper must not let a bare number bypass the
    // deg/rad requirement, recurse into the field.
    let e = errs("View V() { Box #[rotate: (angle: 90)] {} }");
    assert!(matches!(&e[0], CompileError::AttributeTypeMismatch { .. }));
    // …but the properly-suffixed verbose form is accepted.
    assert!(errs("View V() { Box #[rotate: (angle: 90deg)] {} }").is_empty());
    // A verbose tuple with the wrong field name is a mismatch too.
    let e = errs("View V() { Box #[rotate: (deg: 90deg)] {} }");
    assert!(matches!(&e[0], CompileError::AttributeTypeMismatch { .. }));
}

#[test]
fn with_animation_on_a_paint_prop_is_accepted() {
    // RFC-0010: paint-time animatable props accept a `with` curve.
    assert!(errs("View V() { Box #[scale: 1 with anim.spring()] {} }").is_empty());
    assert!(errs("View V() { Box #[opacity: 0.5 with anim.linear(200ms)] {} }").is_empty());
}

#[test]
fn ripple_props_are_accepted_on_the_box_render_path() {
    // RFC-0023: the four ripple effect props, on containers and `Button`.
    assert!(
        errs(
            "View V() { Box #[ripple: 0x80FFFFFF, ripple_active: true, \
                 ripple_radius: 24.0, ripple_duration: 200] {} }"
        )
        .is_empty()
    );
    assert!(errs("View V() { Button(\"Save\") #[ripple: 0xFFFFFF] }").is_empty());
}

#[test]
fn blur_props_are_accepted_and_quality_is_a_closed_token_set() {
    // RFC-0023 §2: the four backdrop props on the box render path.
    assert!(
        errs(
            "View V() { Box #[blur: 20, backdrop_tint: 0x80FFFFFF, \
                 blur_saturation: 1.8, blur_quality: high] {} }"
        )
        .is_empty()
    );
    // An unknown quality token is rejected against the closed set.
    let e = errs("View V() { Box #[blur: 20, blur_quality: ultra] {} }");
    assert!(!e.is_empty(), "unknown `blur_quality` token must error");
}

#[test]
fn a_misspelled_ripple_prop_suggests_the_real_one() {
    let e = errs("View V() { Box #[ripple_activ: true] {} }");
    assert!(
        matches!(
            &e[0],
            CompileError::UnknownAttribute { hint: Some(h), .. } if h == "ripple_active"
        ),
        "got {e:?}"
    );
}

#[test]
fn with_animation_unknown_curve_is_an_error_with_a_hint() {
    let e = errs("View V() { Box #[scale: 1 with anim.sprng()] {} }");
    assert!(matches!(
        &e[0],
        CompileError::UnknownAnimation { hint: Some(h), .. } if h == "spring"
    ));
}

#[test]
fn every_attribute_carries_a_class_and_it_is_per_intrinsic() {
    // RFC-0032 §R2: the class is a required field of the attribute
    // definition, so this is really asserting that the definition
    // *compiles*, but it also pins the two answers that are easy to get
    // backwards, and the fact that the same name can differ per element.
    let col = lookup("Column").expect("Column is an intrinsic");
    assert_eq!(col.property_class("width"), Some(AttrClass::Layout));
    assert_eq!(col.property_class("bg"), Some(AttrClass::Paint));
    assert_eq!(
        col.property_class("rotate"),
        Some(AttrClass::Paint),
        "a transform is the *supported alternative* to animating layout \
             (RFC-0032 §Q8), so it had better not be layout-class itself"
    );
    let text = lookup("Text").expect("Text is an intrinsic");
    assert_eq!(
        text.property_class("size"),
        Some(AttrClass::Layout),
        "font size feeds the text measure protocol"
    );
    assert_eq!(text.property_class("color"), Some(AttrClass::Paint));
    assert_eq!(col.property_class("not_a_real_attribute"), None);
    // RFC-0031 §S1: `smooth` changes the corner *profile* the radius is
    // measured with, the same rect, different pixels, so it is
    // paint-class and therefore animatable. Classifying it as layout would
    // make `radius: 16, smooth: 0.6 with anim.spring()` a compile error for
    // no reason; classifying `radius` as paint is a separate question this
    // RFC does not reopen.
    assert_eq!(col.property_class("smooth"), Some(AttrClass::Paint));
    assert_eq!(
        lookup("Image")
            .expect("Image is an intrinsic")
            .property_class("smooth"),
        Some(AttrClass::Paint),
        "`smooth` goes wherever `radius` goes (RFC-0031 §S3)"
    );
}

/// RFC-0031 §S1 × RFC-0010: `smooth` reaching the animation chokepoint is
/// the whole point of it being paint-class. A layout classification would
/// have produced `LayoutPropNotAnimatable` here instead.
#[test]
fn corner_smoothing_animates() {
    let e = errs("View V() { Column #[radius: 16, smooth: 0.6 with anim.spring()] {} }");
    assert!(e.is_empty(), "smooth must animate: {e:?}");
}

#[test]
fn animating_a_text_size_is_rejected_and_names_transform() {
    // The class table is what makes this reachable at all: `size` is not
    // in the historical layout-name list, so before RFC-0032 an animated
    // font size compiled and quietly relayed out the tree every frame,
    // the exact thing RFC-0010 INV-8 forbids in prose and nothing checked.
    let e = errs(r#"View V() { Text("hi") #[size: 20 with anim.spring()] {} }"#);
    assert!(
        matches!(&e[0], CompileError::LayoutPropNotAnimatable { prop, .. } if prop == "size"),
        "expected LayoutPropNotAnimatable on `size`, got {e:?}"
    );
    let message = e[0].headline();
    assert!(
        message.contains("transform"),
        "the diagnostic must name the supported alternative; got: {message}"
    );
}

#[test]
fn animating_a_paint_prop_is_still_allowed() {
    // The other half: making the rule stricter must not make it universal.
    assert!(
        errs("View V() { Box #[bg: 0xFF0000 with anim.spring()] {} }").is_empty(),
        "a colour is paint-class and animates on the GPU"
    );
}

#[test]
fn with_animation_on_a_layout_prop_is_rejected() {
    // Animating `width` would relayout every frame, a compile error, not a
    // silent slowdown (RFC-0010 §"Layout properties").
    let e = errs("View V() { Box #[width: 100 with anim.spring()] {} }");
    assert!(matches!(
        &e[0],
        CompileError::LayoutPropNotAnimatable { .. }
    ));
}

#[test]
fn nested_animated_values_still_check_the_innermost_value_and_every_curve() {
    // A parenthesised `(x with a) with b` must not let its inner value or
    // curve slip past the checker.
    let e =
        errs("View V() { Box #[radius: (\"hi\" with anim.spring()) with anim.linear(200ms)] {} }");
    assert!(
        e.iter()
            .any(|err| matches!(err, CompileError::AttributeTypeMismatch { .. })),
        "innermost `\"hi\"` must be type-checked against `radius`"
    );
    let e = errs("View V() { Box #[radius: (3 with anim.sprng()) with anim.linear(200ms)] {} }");
    assert!(
        e.iter()
            .any(|err| matches!(err, CompileError::UnknownAnimation { .. })),
        "a bad nested curve must still be reported"
    );
}

/// The layout-property prohibition is about the *expression*, not its
/// outermost node, otherwise a parenthesis steps around it.
///
/// `width: (x with anim.linear(200ms)) + 0` animates a layout property just
/// as plainly as `width: x with …`; it merely writes the `with` one level in.
/// Letting it through was not a lenient reading of the rule, it was a
/// silently broken element: the value reached the layout pass, relaid out
/// every frame, and resolved to a float the integer-valued dimension reader
/// drops, so the box lost its width entirely and stretched to fill.
#[test]
fn an_animation_nested_inside_a_layout_expression_is_still_rejected() {
    for src in [
        "View V() { Box #[width: ((true ? 200 : 20) with anim.linear(200ms)) + 0] {} }",
        "View V() { Box #[height: 2 * (100 with anim.spring())] {} }",
        "View V() { Box #[gap: (4 with anim.spring()) + 1] {} }",
        "View V() { Box #[width: (anim.keyframes(0%: 0, 100%: 9, duration: 1s)) + 0] {} }",
    ] {
        let e = errs(src);
        assert!(
            e.iter()
                .any(|err| matches!(err, CompileError::LayoutPropNotAnimatable { .. })),
            "{src}\nmust be rejected, got {e:?}"
        );
    }
    // The rule is about layout, not about nesting: a paint property takes an
    // animation wherever it is written, and still type-checks what it wraps.
    assert!(
        errs("View V() { Box #[radius: (4 with anim.spring()) + 1] {} }").is_empty(),
        "a nested animation on a paint prop is ordinary"
    );
}

#[test]
fn keyframes_check_their_steps_and_are_rejected_on_a_layout_prop() {
    // RFC-0025 §3: keyframes are a value, checked like any other value.
    assert!(
        errs(
            "View V() { Box #[translate: anim.keyframes(0%: (-100, 0), 100%: (300, 0), \
                 duration: 2s, loop: true)] {} }"
        )
        .is_empty(),
        "a well-formed sequence on a paint prop checks clean"
    );
    // …on a layout property it would relayout every frame (INV-8).
    let e = errs("View V() { Box #[width: anim.keyframes(0%: 0, 100%: 200, duration: 1s)] {} }");
    assert!(
        matches!(&e[0], CompileError::LayoutPropNotAnimatable { prop, .. } if prop == "width"),
        "got {e:?}"
    );
    // A malformed sequence is reported, not silently dropped.
    let e = errs("View V() { Box #[radius: anim.keyframes(0%: 4, 100%: 12)] {} }");
    assert!(
        matches!(&e[0], CompileError::InvalidAnimation { .. }),
        "a missing `duration:` is an error, got {e:?}"
    );
    // Each step's value is type-checked against the property.
    let e =
        errs("View V() { Box #[radius: anim.keyframes(0%: 4, 100%: \"big\", duration: 1s)] {} }");
    assert!(
        e.iter()
            .any(|err| matches!(err, CompileError::AttributeTypeMismatch { .. })),
        "got {e:?}"
    );
}

#[test]
fn a_bad_animation_modifier_is_reported_through_the_element_checker() {
    // RFC-0025 §4 modifiers are validated with the curve, so a typo never
    // silently degrades a looping animation into a one-shot.
    let e = errs("View V() { Box #[scale: 1.2 with anim.spring(repeat: often)] {} }");
    assert!(
        matches!(&e[0], CompileError::InvalidAnimation { .. }),
        "got {e:?}"
    );
    assert!(
        errs("View V() { Box #[scale: 1.2 with anim.spring(repeat: infinite, reverse: true)] {} }")
            .is_empty(),
        "the well-formed modifiers check clean"
    );
}

#[test]
fn rule1_unknown_view_suggests() {
    let e = errs("View V() { Colunm #[gap: 8] {} }");
    assert!(matches!(
        &e[0],
        CompileError::UnknownView { hint: Some(h), .. } if h == "Column"
    ));
}

#[test]
fn rule1_known_user_view_is_ok() {
    // `Card` is not an intrinsic but is a known view in scope.
    let el = first_element("View V() { Card #[gap: 8] {} }");
    assert!(validate_element(&el, &el.attrs, &["Card"]).is_empty());
}

#[test]
fn rule2_arity_mismatch() {
    // Text takes exactly one content arg.
    let e = errs("View V() { Text(\"a\", \"b\") }");
    assert!(matches!(
        &e[0],
        CompileError::ArityMismatch {
            expected: 1,
            found: 2,
            ..
        }
    ));
    // Column takes none.
    let e = errs("View V() { Column(\"x\") }");
    assert!(
        e.iter()
            .any(|d| matches!(d, CompileError::ArityMismatch { expected: 0, .. }))
    );
}

#[test]
fn rule4_unknown_attribute_suggests_gap() {
    let e = errs("View V() { Column #[gp: 1] {} }");
    assert!(matches!(
        &e[0],
        CompileError::UnknownAttribute { hint: Some(h), .. } if h == "gap"
    ));
}

#[test]
fn rule4_value_on_box_is_unknown_attribute() {
    let e = errs("View V() { Box #[value: 1] {} }");
    assert!(matches!(&e[0], CompileError::UnknownAttribute { .. }));
}

#[test]
fn rule5_wrong_separator() {
    // `gap` is a property; using `=>` is a separator error.
    let e = errs("View V() { Column #[gap => 1] {} }");
    assert!(matches!(
        &e[0],
        CompileError::WrongAttributeSeparator {
            expected_property: true,
            ..
        }
    ));
    // `tap` is an event; using `:` is a separator error.
    let e = errs("View V() { Button(\"x\") #[tap: 1] }");
    assert!(matches!(
        &e[0],
        CompileError::WrongAttributeSeparator {
            expected_property: false,
            ..
        }
    ));
}

#[test]
fn rule6_type_and_enum_token() {
    // A string where a color is expected.
    let e = errs("View V() { Column #[bg: \"red\"] {} }");
    assert!(matches!(&e[0], CompileError::AttributeTypeMismatch { .. }));
    // An unknown enum token.
    let e = errs("View V() { Column #[align: centr] {} }");
    assert!(matches!(&e[0], CompileError::AttributeTypeMismatch { .. }));
}

#[test]
fn rule8_children_on_childless_intrinsic() {
    let e = errs("View V() { Text(\"hi\") { Text(\"no\") } }");
    assert!(
        e.iter()
            .any(|d| matches!(d, CompileError::UnexpectedChildren { .. }))
    );
}

#[test]
fn hit_rect_inflates_small_button_clamped_to_parent() {
    let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
    let inflated = inflate_hit_rect(Rect::new(0.0, 0.0, 10.0, 10.0), parent);
    assert!(inflated.w >= HIT_MIN && inflated.h >= HIT_MIN);
    // Stays within the parent scissor.
    assert!(inflated.x >= parent.x && inflated.y >= parent.y);
    assert!(inflated.x + inflated.w <= parent.x + parent.w);
    assert!(inflated.y + inflated.h <= parent.y + parent.h);
}

#[test]
fn vector_icon_validates_like_an_asset_handle_intrinsic() {
    // Valid: arity-1 asset handle + size/color props.
    assert!(
        errs("View V() { VectorIcon(\"icons/gear.svg\") #[size: 24, color: 0xFFFFFF] }").is_empty()
    );
    // Arity 0 and 2 → ArityMismatch.
    assert!(
        errs("View V() { VectorIcon() }")
            .iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { expected: 1, .. }))
    );
    assert!(
        errs("View V() { VectorIcon(\"a.svg\", \"b.svg\") }")
            .iter()
            .any(|e| matches!(
                e,
                CompileError::ArityMismatch {
                    expected: 1,
                    found: 2,
                    ..
                }
            ))
    );
    // A child block → UnexpectedChildren.
    assert!(
        errs("View V() { VectorIcon(\"a.svg\") { Text(\"no\") } }")
            .iter()
            .any(|e| matches!(e, CompileError::UnexpectedChildren { .. }))
    );
    // An unknown attribute (e.g. gradient) → UnknownAttribute.
    assert!(
        errs("View V() { VectorIcon(\"a.svg\") #[gradient: 0x00FF00] }")
            .iter()
            .any(|e| matches!(e, CompileError::UnknownAttribute { .. }))
    );
}

#[test]
fn overlay_validates_as_a_childful_layout_intrinsic() {
    // Valid: modal overlay with a scrim + content, and a `dismiss` event.
    assert!(
        errs(
            "View V() { Overlay #[modal: true] { Box #[bg: 0x000000, opacity: 0.3, grow: 1] {} \
                 Column #[anchor: center, bg: 0xFFFFFF] { Text(\"hi\") } } }"
        )
        .is_empty()
    );
    // `dismiss` is an event, so `=>` is correct.
    assert!(errs("View V() { Overlay #[dismiss => x] { Box {} } }").is_empty());
    // Content args are rejected (arity 0).
    assert!(
        errs("View V() { Overlay(\"x\") { Box {} } }")
            .iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { expected: 0, .. }))
    );
    // A stray prop → UnknownAttribute.
    assert!(
        errs("View V() { Overlay #[z_index: 3] { Box {} } }")
            .iter()
            .any(|e| matches!(e, CompileError::UnknownAttribute { .. }))
    );
    // `dismiss` with `:` instead of `=>` is a separator error (it's an event).
    assert!(
        errs("View V() { Overlay #[dismiss: 1] { Box {} } }")
            .iter()
            .any(|e| matches!(e, CompileError::WrongAttributeSeparator { .. }))
    );
}

#[test]
fn checkbox_validates_as_a_focusable_bool_widget() {
    // Valid: `value` (Bool) with the mixed-state flag and a `change` event.
    assert!(errs("View V() { Checkbox #[value: false, indeterminate: true] }").is_empty());
    assert!(errs("View V() { Checkbox #[value: true, change => f()] }").is_empty());
    // Focusable by default → key events are in-vocabulary.
    assert!(errs("View V() { Checkbox #[value: false, key_down => f()] }").is_empty());
    // `value` must be a Bool: a string is a type mismatch.
    assert!(
        errs("View V() { Checkbox #[value: \"x\"] }")
            .iter()
            .any(|e| matches!(e, CompileError::AttributeTypeMismatch { .. }))
    );
    // Content args are rejected (arity 0).
    assert!(
        errs("View V() { Checkbox(\"x\") }")
            .iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { expected: 0, .. }))
    );
    // Children are rejected (childless).
    assert!(
        errs("View V() { Checkbox { Box {} } }")
            .iter()
            .any(|e| matches!(e, CompileError::UnexpectedChildren { .. }))
    );
    // A stray prop → UnknownAttribute (`selected`/`invalid` are now universal
    // RFC-0024 props, so pick a genuinely-unknown name).
    assert!(
        errs("View V() { Checkbox #[bogus: true] }")
            .iter()
            .any(|e| matches!(e, CompileError::UnknownAttribute { .. }))
    );
    // `change` is an event, so `:` instead of `=>` is a separator error.
    assert!(
        errs("View V() { Checkbox #[change: 1] }")
            .iter()
            .any(|e| matches!(e, CompileError::WrongAttributeSeparator { .. }))
    );
}

#[test]
fn radiobutton_validates_as_a_focusable_group_member() {
    // Valid: a value + a group bind, and a `change` event.
    assert!(errs("View V() { RadioButton #[value: \"home\", bind: \"home\"] }").is_empty());
    assert!(
        errs("View V() { RadioButton #[value: \"a\", bind: \"a\", change => f()] }").is_empty()
    );
    // Focusable by default → key events are in-vocabulary (arrow keys).
    assert!(
        errs("View V() { RadioButton #[value: \"a\", bind: \"a\", key_down => f()] }").is_empty()
    );
    // `value` must be a Str: an int is a type mismatch.
    assert!(
        errs("View V() { RadioButton #[value: 5, bind: \"a\"] }")
            .iter()
            .any(|e| matches!(e, CompileError::AttributeTypeMismatch { .. }))
    );
    // Content args are rejected (arity 0).
    assert!(
        errs("View V() { RadioButton(\"x\") }")
            .iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { expected: 0, .. }))
    );
    // Children are rejected (childless).
    assert!(
        errs("View V() { RadioButton { Box {} } }")
            .iter()
            .any(|e| matches!(e, CompileError::UnexpectedChildren { .. }))
    );
    // A stray prop → UnknownAttribute (`selected`/`invalid` are now universal
    // RFC-0024 props, so pick a genuinely-unknown name).
    assert!(
        errs("View V() { RadioButton #[bogus: true] }")
            .iter()
            .any(|e| matches!(e, CompileError::UnknownAttribute { .. }))
    );
    // `change` is an event, so `:` instead of `=>` is a separator error.
    assert!(
        errs("View V() { RadioButton #[change: 1] }")
            .iter()
            .any(|e| matches!(e, CompileError::WrongAttributeSeparator { .. }))
    );
}

#[test]
fn grid_validates_as_a_childful_container() {
    use byard_core::atlas::GridTrack;
    // Valid: templates, gaps, and children.
    assert!(
        errs("View V() { Grid #[columns: \"1fr 1fr\", rows: \"auto\", gap: 8] { Box {} Box {} } }")
            .is_empty()
    );
    // Child placement props are accepted on a grid child.
    assert!(
        errs("View V() { Grid #[columns: \"1fr 1fr\"] { Box #[col: 1, row: 2, col_span: 2] {} } }")
            .is_empty()
    );
    // Content args are rejected (arity 0).
    assert!(
        errs("View V() { Grid(\"x\") { Box {} } }")
            .iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { expected: 0, .. }))
    );
    // A stray prop → UnknownAttribute.
    assert!(
        errs("View V() { Grid #[cols: \"1fr\"] {} }")
            .iter()
            .any(|e| matches!(e, CompileError::UnknownAttribute { .. }))
    );

    // The template parser itself.
    assert_eq!(
        parse_grid_template("1fr 2fr 100"),
        Some(vec![
            GridTrack::Fr(1.0),
            GridTrack::Fr(2.0),
            GridTrack::Px(100.0)
        ])
    );
    assert_eq!(
        parse_grid_template("repeat(3, 1fr)"),
        Some(vec![
            GridTrack::Fr(1.0),
            GridTrack::Fr(1.0),
            GridTrack::Fr(1.0)
        ])
    );
    assert_eq!(
        parse_grid_template("auto 1fr"),
        Some(vec![GridTrack::Auto, GridTrack::Fr(1.0)])
    );
    assert_eq!(parse_grid_template(""), None);
    assert_eq!(parse_grid_template("1fr bogus"), None);
    assert_eq!(parse_grid_template("repeat(0, 1fr)"), None);
}

#[test]
fn zstack_validates_as_a_childful_container() {
    // Valid: an alignment token and overlapping children.
    assert!(errs("View V() { ZStack #[alignment: top_end] { Box {} Box {} } }").is_empty());
    assert!(errs("View V() { ZStack { Box {} } }").is_empty());
    // Content args are rejected (arity 0).
    assert!(
        errs("View V() { ZStack(\"x\") { Box {} } }")
            .iter()
            .any(|e| matches!(e, CompileError::ArityMismatch { expected: 0, .. }))
    );
    // An unknown alignment token is a type mismatch.
    let e = errs("View V() { ZStack #[alignment: middle] {} }");
    assert!(matches!(&e[0], CompileError::AttributeTypeMismatch { .. }));
    // A stray prop → UnknownAttribute.
    assert!(
        errs("View V() { ZStack #[foo: 1] {} }")
            .iter()
            .any(|e| matches!(e, CompileError::UnknownAttribute { .. }))
    );
}

#[test]
fn anchor_enum_is_accepted_on_containers_and_rejects_unknown_tokens() {
    assert!(errs("View V() { Column #[anchor: bottom] {} }").is_empty());
    assert!(errs("View V() { Box #[anchor: center] {} }").is_empty());
    let e = errs("View V() { Box #[anchor: middle] {} }");
    assert!(matches!(&e[0], CompileError::AttributeTypeMismatch { .. }));
}

#[test]
fn color_parsing() {
    let green = color_to_rgba(0x00_FF_00, false);
    assert!(green[0] < 0.01 && green[1] > 0.99 && green[2] < 0.01 && green[3] > 0.99);
    let c = color_to_rgba(0x80_00_00_00, true);
    assert!((c[3] - 0.5019).abs() < 0.01, "alpha-first 0x80… ≈ 0.5");
}

// ── Canvas & shape commands (RFC-0020) ─────────────────────────────────────

/// `validate_element` + `validate_canvas` on the first element of `src`,
/// the exact pair the evaluator's `Canvas` lowering runs.
fn canvas_errs(src: &str) -> Vec<CompileError> {
    let el = first_element(src);
    let mut e = validate_element(&el, &el.attrs, &[]);
    e.extend(validate_canvas(&el, &el.attrs));
    e
}

// ── RFC-0031 §S9–§S10: `ngon` and sequence morphing ───────────────────

#[test]
fn ngon_is_a_shape_command_with_its_own_parameter_contract() {
    assert!(is_shape_command("ngon"));
    let e = canvas_errs(
        "View V() { Canvas #[width: 48, height: 48] { \
               ngon(cx: 24, cy: 24, r: 20, n: 7, corner: 5, inner: 0.75, \
                    rotate: 15deg, fill: 0x6750A4) } }",
    );
    assert!(
        e.is_empty(),
        "a fully-specified ngon must check clean: {e:?}"
    );

    // `n` and `r` are required; the rest have defaults.
    let missing =
        canvas_errs("View V() { Canvas #[width: 48, height: 48] { ngon(cx: 1, cy: 1) } }");
    let names: Vec<&str> = missing
        .iter()
        .filter_map(|x| match x {
            CompileError::MissingShapeParam { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"r") && names.contains(&"n"), "{missing:?}");
}

/// §Q10. The diagnostic must be `NotAnimatable`, **not**
/// `LayoutPropNotAnimatable`: `n` moves no geometry and costs no relayout,
/// it simply has no value between a pentagon and a hexagon. Conflating the
/// two would tell the author to reach for a transform, which is the wrong
/// advice, the right one is `morph`.
#[test]
fn animating_ngons_vertex_count_is_refused_and_names_morph() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 48, height: 48] { \
               ngon(cx: 24, cy: 24, r: 20, n: 5 with anim.spring(), fill: 0x6750A4) } }",
    );
    let found = e
        .iter()
        .find(|x| matches!(x, CompileError::NotAnimatable { .. }))
        .unwrap_or_else(|| panic!("expected NotAnimatable, got {e:?}"));
    assert!(
        found.headline().contains("morph"),
        "the error must teach the right construct: {}",
        found.headline()
    );
    assert!(
        !e.iter()
            .any(|x| matches!(x, CompileError::LayoutPropNotAnimatable { .. })),
        "`n` is paint-class; the layout diagnostic would be the wrong reason"
    );
}

/// The other side: everything else about an `ngon` animates, because the
/// shape's *proportions* are continuous even though its vertex count is not.
#[test]
fn an_ngons_continuous_parameters_still_animate() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 48, height: 48] { \
               ngon(cx: 24, cy: 24, r: 20, n: 5, inner: 0.4 with anim.spring(), \
                    corner: 2 with anim.spring(), fill: 0x6750A4) } }",
    );
    assert!(e.is_empty(), "inner/corner must animate: {e:?}");
}

/// §S5/§Q3. The cap is diagnosed at the **ninth shape**, not at the canvas:
/// the author needs to know which one to move.
#[test]
fn a_ninth_shape_in_a_group_is_an_error_naming_that_shape() {
    let members = (0..9)
        .map(|i| format!("ngon(cx: {}, cy: 24, r: 8, n: 5, fill: 0x6750A4)", i * 10))
        .collect::<Vec<_>>()
        .join(" ");
    let src =
        format!("View V() {{ Canvas #[width: 200, height: 48, morph: 0.0] {{ {members} }} }}");
    let el = first_element(&src);
    let e = validate_canvas(&el, &el.attrs);
    let CompileError::TooManyGroupMembers { span, max, found } = e
        .iter()
        .find(|x| matches!(x, CompileError::TooManyGroupMembers { .. }))
        .unwrap_or_else(|| panic!("expected TooManyGroupMembers, got {e:?}"))
    else {
        unreachable!()
    };
    assert_eq!((*max, *found), (8, 9));
    // The span is the ninth shape's, which begins after the eighth ends.
    let ninth = src.rfind("ngon(").expect("nine ngons were written");
    assert_eq!(
        span.start as usize, ninth,
        "the error must point at the ninth shape"
    );

    // Eight is fine, and the same body without a combine mode is fine at
    // any count, the cap belongs to the group, not to the canvas.
    let eight = members.rsplit_once(" ngon(").expect("nine members").0;
    let ok = format!("View V() {{ Canvas #[width: 200, height: 48, morph: 0.0] {{ {eight} }} }}");
    let el = first_element(&ok);
    assert!(validate_canvas(&el, &el.attrs).is_empty());
    let ungrouped = format!("View V() {{ Canvas #[width: 200, height: 48] {{ {members} }} }}");
    let el = first_element(&ungrouped);
    assert!(
        validate_canvas(&el, &el.attrs).is_empty(),
        "an ungrouped canvas has no member cap"
    );
}

#[test]
fn morph_is_a_paint_class_canvas_attribute() {
    let canvas = lookup("Canvas").expect("Canvas is an intrinsic");
    assert_eq!(canvas.property_class("morph"), Some(AttrClass::Paint));
    // §S10 × INV-8: the whole point is that the Material 3 loader is one
    // animated scalar. A layout classification would refuse it.
    let e = canvas_errs(
        "View V() { Canvas #[width: 48, height: 48, \
               morph: 7.0 with anim.linear(4550ms, from: 0.0, repeat: infinite)] { \
               ngon(cx: 24, cy: 24, r: 20, n: 4, fill: 0x6750A4) \
               ngon(cx: 24, cy: 24, r: 20, n: 7, fill: 0x6750A4) } }",
    );
    assert!(e.is_empty(), "morph must animate: {e:?}");
}

// ── RFC-0031 §S7–§S8: organic fusion ──────────────────────────────────

#[test]
fn fuse_and_morph_are_mutually_exclusive() {
    // §Q4. Not a preference: morphing between fused sub-groups needs a
    // member to itself be a group head, which turns a flat contiguous range
    // into a tree and the unrolled fragment loop into recursion.
    let e = canvas_errs(
        "View V() { Canvas #[width: 140, height: 48, fuse: 16, morph: 0.5] { \
               circle(cx: 24, cy: 24, r: 18, fill: 0x6750A4) } }",
    );
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::ConflictingGroupMode { .. })),
        "expected ConflictingGroupMode, got {e:?}"
    );
    // Either one alone is fine.
    for mode in ["fuse: 16", "morph: 0.5"] {
        let ok = canvas_errs(&format!(
            "View V() {{ Canvas #[width: 140, height: 48, {mode}] {{ \
                   circle(cx: 24, cy: 24, r: 18, fill: 0x6750A4) }} }}"
        ));
        assert!(ok.is_empty(), "`{mode}` alone must check clean: {ok:?}");
    }
}

/// §Q5. A *warning*, not an error: the shape still renders correctly and
/// the property is merely inert, so failing a build over it would be
/// disproportionate, but saying nothing is how a developer spends an
/// afternoon on an outline that was never going to appear.
#[test]
fn a_per_member_stroke_inside_a_fusion_group_warns_without_failing() {
    // The *first* shape's stroke is the group's outline, so it is silent.
    let first_only = canvas_errs(
        "View V() { Canvas #[width: 140, height: 48, fuse: 16] { \
               circle(cx: 24, cy: 24, r: 18, stroke: 0xFFFFFF, stroke_width: 2) \
               circle(cx: 60, cy: 24, r: 14, fill: 0x6750A4) } }",
    );
    assert!(
        first_only.is_empty(),
        "the group's outline comes from the first shape: {first_only:?}"
    );

    // A *later* shape's stroke is the one that is genuinely inert.
    let e = canvas_errs(
        "View V() { Canvas #[width: 140, height: 48, fuse: 16] { \
               circle(cx: 24, cy: 24, r: 18, fill: 0x6750A4) \
               circle(cx: 60, cy: 24, r: 14, fill: 0x6750A4, stroke: 0xFFFFFF, \
                      stroke_width: 2) } }",
    );
    let strokes: Vec<&CompileError> = e
        .iter()
        .filter(|x| matches!(x, CompileError::StrokeInFusionGroup { .. }))
        .collect();
    assert_eq!(strokes.len(), 2, "both stroke params are inert: {e:?}");
    assert!(
        strokes.iter().all(|x| x.is_warning()),
        "an inert property must not fail the build"
    );
    assert!(
        e.iter().all(CompileError::is_warning),
        "nothing here is fatal: {e:?}"
    );
    // The same shape outside a fusion group is silent, the stroke works.
    let ungrouped = canvas_errs(
        "View V() { Canvas #[width: 140, height: 48] { \
               circle(cx: 24, cy: 24, r: 18, stroke: 0xFFFFFF, stroke_width: 2) } }",
    );
    assert!(ungrouped.is_empty(), "{ungrouped:?}");
}

/// §Q6. An error, not an approximation: there is no closed form for arc
/// length along the union of arbitrary SDFs, and any approximation makes
/// dashes crawl as the fusion animates, for no reason the author can see.
#[test]
fn dashes_on_a_fused_stroke_are_refused() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 140, height: 48, fuse: 16] { \
               circle(cx: 24, cy: 24, r: 18, dash: (6, 4), fill: 0x6750A4) } }",
    );
    let dash = e
        .iter()
        .find(|x| matches!(x, CompileError::DashOnFusedStroke { .. }))
        .unwrap_or_else(|| panic!("expected DashOnFusedStroke, got {e:?}"));
    assert!(!dash.is_warning(), "a dash that cannot be drawn is fatal");
}

#[test]
fn fuse_is_a_paint_class_canvas_attribute() {
    let canvas = lookup("Canvas").expect("Canvas is an intrinsic");
    assert_eq!(canvas.property_class("fuse"), Some(AttrClass::Paint));
    // §S7's cost model: an animating `k` is new per-instance data, never a
    // re-tessellation, so it belongs on the ordinary animation chokepoint.
    let e = canvas_errs(
        "View V() { Canvas #[width: 140, height: 48, \
               fuse: 16 with anim.spring()] { \
               circle(cx: 24, cy: 24, r: 18, fill: 0x6750A4) } }",
    );
    assert!(e.is_empty(), "fuse must animate: {e:?}");
}

#[test]
fn valid_canvas_with_shapes_passes() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 48, height: 48, bg: 0x1E1E2A] { \
               arc(cx: 24, cy: 24, r: 20, start: -90, sweep: 270, \
                   stroke: 0x6750A4, stroke_width: 4, cap: round) \
               circle(cx: 24, cy: 24, r: 8, fill: 0xE8DEF8) \
               line(x1: 0, y1: 0, x2: 48, y2: 48, stroke: 0xFFFFFF, dash: (4, 4)) \
               rect(x: 4, y: 4, w: 12, h: 8, radius: 2, fill: 0x334155) \
               bezier(x1: 0, y1: 40, cx1: 16, cy1: 0, cx2: 32, cy2: 0, x2: 48, y2: 40, \
                      stroke: 0x00FF00) \
               path(d: \"M4 4 L20 4 L20 20 Z\", fill: 0xFF0000) \
               text(\"75%\", x: 24, y: 24, align: center, size: 12) } }",
    );
    assert!(e.is_empty(), "{e:?}");
}

#[test]
fn canvas_requires_width_and_height() {
    let e = canvas_errs("View V() { Canvas #[width: 48] { circle(cx: 1, cy: 1, r: 1) } }");
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::CanvasMissingSize { .. })),
        "{e:?}"
    );
}

#[test]
fn shape_command_outside_canvas_is_a_precise_error() {
    let e = errs("View V() { arc(cx: 24, cy: 24, r: 20) }");
    assert!(
        matches!(&e[0], CompileError::ShapeOutsideCanvas { name, .. } if name == "arc"),
        "{e:?}"
    );
}

#[test]
fn non_shape_children_inside_canvas_are_rejected() {
    // An intrinsic view child is not a shape command.
    let e = canvas_errs("View V() { Canvas #[width: 10, height: 10] { Text(\"no\") } }");
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::UnknownShapeCommand { name, .. } if name == "Text")),
        "{e:?}"
    );
    // Nor is a declaration: there is nothing for a `var` inside a canvas
    // to mean, and silently ignoring one is how a developer spends an
    // afternoon on a shape that was never going to appear.
    let e = canvas_errs("View V() { Canvas #[width: 10, height: 10] { var n = 1 } }");
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::UnknownShapeCommand { name, .. } if name == "var")),
        "{e:?}"
    );
    let e = canvas_errs("View V() { Canvas #[width: 10, height: 10] { let n = 1 } }");
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::UnknownShapeCommand { name, .. } if name == "let")),
        "{e:?}"
    );
}

/// RFC-0020 §1 as amended: `for` and `when` are shape *generators*, so a
/// canvas body admits them.
///
/// Without this a drawing surface cannot draw a chart, the one thing a
/// drawing surface is for, because the shape count cannot come from data.
#[test]
fn control_flow_inside_a_canvas_is_accepted_and_its_body_still_validated() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
             for b in bars { rect(x: b.x, y: 0, w: 2, h: b.h) } } }",
    );
    assert!(e.is_empty(), "a data-driven canvas must validate: {e:?}");

    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
             when on { circle(cx: 1, cy: 1, r: 1) } else { line(x1: 0, y1: 0, x2: 1, y2: 1) } } }",
    );
    assert!(e.is_empty(), "{e:?}");

    // Admitting the control flow must not stop validating what is inside
    // it: a bad shape in a loop body is still a bad shape.
    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
             for b in bars { Text(\"no\") } } }",
    );
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::UnknownShapeCommand { name, .. } if name == "Text")),
        "a loop body is not an escape hatch: {e:?}"
    );

    // Including in an `else` branch, which is the one a recursive walk
    // written in a hurry forgets.
    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
             when on { circle(cx: 1, cy: 1, r: 1) } else { Text(\"no\") } } }",
    );
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::UnknownShapeCommand { name, .. } if name == "Text")),
        "{e:?}"
    );
}

#[test]
fn unknown_shape_param_gets_a_levenshtein_hint() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
               arc(cx: 1, cy: 1, r: 5, stroke_widht: 2) } }",
    );
    assert!(
        e.iter().any(|x| matches!(
            x,
            CompileError::UnknownShapeParam { name, hint: Some(h), .. }
                if name == "stroke_widht" && h == "stroke_width"
        )),
        "{e:?}"
    );
}

#[test]
fn missing_required_geometry_is_reported() {
    let e = canvas_errs("View V() { Canvas #[width: 10, height: 10] { arc(cx: 1, cy: 1) } }");
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::MissingShapeParam { name, .. } if name == "r")),
        "{e:?}"
    );
}

#[test]
fn stroking_a_path_is_rejected_in_v1() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
               path(d: \"M0 0 L5 5\", stroke: 0xFF0000) } }",
    );
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::PathStrokeUnsupported { .. })),
        "{e:?}"
    );
}

#[test]
fn bezier_accepts_the_terse_positional_form() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
               bezier(0, 40, 16, 0, 32, 0, 48, 40, stroke: 0xFFFFFF) } }",
    );
    assert!(e.is_empty(), "{e:?}");
}

#[test]
fn bad_cap_token_is_flagged_with_a_hint() {
    let e = canvas_errs(
        "View V() { Canvas #[width: 10, height: 10] { \
               circle(cx: 1, cy: 1, r: 1, stroke: 0xFFFFFF, cap: rounded) } }",
    );
    assert!(
        e.iter()
            .any(|x| matches!(x, CompileError::AttributeTypeMismatch { .. })),
        "{e:?}"
    );
}
