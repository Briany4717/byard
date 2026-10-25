//! Style states: spreads, merges, specificity and the prop-driven state bits.

use super::*;
use crate::interp::eval::states::resolve_state_attrs;
use crate::parser::ast::AttrKind;
use crate::parser::ast::StyleStateKind;

#[test]
fn style_value_spreads_onto_an_element_and_inline_overrides() {
    // RFC-0016: a `let`-bound style is spliced by `..`, and inline attrs win.
    let parsed = parse(
        "View V() { \
             let btn = style { bg: 0x112233, radius: 8 } \
             Box #[..btn, width: 10, height: 10] {} \
             Box #[..btn, bg: 0x445566, width: 10, height: 10] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let insts = frame.instances();
    // First box takes `bg` from the spread (0x11 red channel).
    assert!(
        (insts[0].color[0] - linear(0x11)).abs() < 1e-3,
        "spread bg reaches the box, got {:?}",
        insts[0].color
    );
    // Second box: inline `bg` overrides the spread (0x44 red channel).
    assert!(
        (insts[1].color[0] - linear(0x44)).abs() < 1e-3,
        "inline bg overrides the spread, got {:?}",
        insts[1].color
    );
}

#[test]
fn merge_composes_two_styles_right_wins() {
    // RFC-0016: `base merge overrides`, the right style wins on conflicts,
    // the left's non-conflicting attributes survive.
    let parsed = parse(
        "View V() { \
             let base = style { bg: 0x111111, radius: 8 } \
             let hot = base merge style { bg: 0x445566 } \
             Box #[..hot, width: 10, height: 10] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let inst = frame.instances()[0];
    // `bg` comes from the right side of the merge (0x44 red channel)…
    assert!(
        (inst.color[0] - linear(0x44)).abs() < 1e-3,
        "right style's bg wins, got {:?}",
        inst.color
    );
    // …while `radius` (only on the base) survives (radii != 0).
    assert!(inst.radii[0] > 0.0, "base radius survives the merge");
}

#[test]
fn parent_scale_is_inherited_by_child_text_and_boxes() {
    // RFC-0011 group transforms: a scaled container carries its descendants,
    // the reported bug was that a scaled parent's *text* stayed the same size.
    let parsed = parse(
        "View V() {\n Column #[scale: 2.0, width: 100, height: 100, bg: 0x111111] {\n \
             Text(\"hi\") #[size: 10]\n Box #[bg: 0x222222, width: 20, height: 20]\n }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // The child text's font size doubled with the parent scale (the fix).
    let line = frame
        .texts()
        .iter()
        .find(|t| t.text == "hi")
        .expect("the child text line is emitted");
    assert!(
        (line.font_size - 20.0).abs() < 1e-3,
        "child text scaled 2× with its parent (10 → 20), got {}",
        line.font_size
    );

    // Both boxes carry a 2× scale: the parent's own, and the child's inherited.
    for inst in frame.instances() {
        assert!(
            (inst.transform.scale[0] - 2.0).abs() < 1e-3
                && (inst.transform.scale[1] - 2.0).abs() < 1e-3,
            "every box in the group inherits the 2× scale, got {:?}",
            inst.transform.scale
        );
    }
}

#[test]
fn resolve_state_attrs_applies_specificity_then_declaration_order() {
    // RFC-0024 §2: single-state blocks apply in declaration order (equal
    // specificity → later wins); a combined `on hover+focused` (higher
    // specificity) beats both single-state blocks.
    use crate::interp::events::StyleState;
    let sp = crate::diagnostics::Span::new(0, 0);
    let prop = |name: &str, v: i64| Attr {
        name: Symbol::intern(name),
        axis: None,
        kind: AttrKind::Prop {
            value: Expr::IntLit(v, sp),
        },
        span: sp,
    };
    let block = |states: Vec<StyleStateKind>, v: i64| StateBlock {
        states,
        attrs: vec![prop("bg", v)],
        viewport: None,
        span: sp,
    };
    let base = vec![prop("bg", 1)];
    let blocks = vec![
        block(vec![StyleStateKind::Hover], 2),
        block(vec![StyleStateKind::Disabled], 3),
        block(vec![StyleStateKind::Hover, StyleStateKind::Focused], 4),
    ];

    // No state active → base survives, and the borrow is cheap (no clone).
    let none = resolve_state_attrs(&base, &blocks, StyleState::empty(), &|_| false);
    assert!(matches!(none, std::borrow::Cow::Borrowed(_)));
    assert_eq!(find_int(&none, "bg"), Some(1));

    // Hover alone → the hover block overlays (the combined block needs focus).
    let hov = resolve_state_attrs(&base, &blocks, StyleState::HOVER, &|_| false);
    assert_eq!(find_int(&hov, "bg"), Some(2));

    // Hover + disabled (equal specificity) → disabled wins by declaration
    // order (declared after hover).
    let both = resolve_state_attrs(
        &base,
        &blocks,
        StyleState::HOVER.union(StyleState::DISABLED),
        &|_| false,
    );
    assert_eq!(find_int(&both, "bg"), Some(3));

    // Hover + focused → the combined `hover+focused` block (specificity 2)
    // beats the single-state `hover` block regardless of declaration order.
    let combined = resolve_state_attrs(
        &base,
        &blocks,
        StyleState::HOVER.union(StyleState::FOCUSED),
        &|_| false,
    );
    assert_eq!(find_int(&combined, "bg"), Some(4));
}

#[test]
fn checkbox_on_checked_state_recolours_the_accent() {
    // RFC-0024: a checked value drives the `checked` state, so `on checked`
    // overlays reach the checkbox's own filled-accent visual.
    let src = "View C() {\n var c = true\n \
                   let chk = style { bg: 0x111111 on checked { bg: 0x00FF00 } }\n \
                   Checkbox #[..chk, bind: c, width: 20, height: 20]\n}";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // The filled square is the first decorated box; its accent is the
    // `on checked` green.
    let fill = frame.decorated()[0].base.color;
    assert!(
        fill[1] > 0.9 && fill[0] < 0.1,
        "checked accent is the `on checked` green, got {fill:?}"
    );
}

#[test]
fn universal_selected_prop_drives_the_selected_state() {
    // RFC-0024: `selected: true` on any element activates the `selected`
    // state, so `on selected { bg }` recolours it.
    let src = "View C() {\n \
                   let s = style { bg: 0x111111 on selected { bg: 0x00FF00 } }\n \
                   Box #[..s, selected: true, width: 20, height: 20] {}\n}";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let fill = frame.instances()[0].color;
    assert!(
        fill[1] > 0.9 && fill[0] < 0.1,
        "selected box uses the `on selected` green, got {fill:?}"
    );
}

fn find_int(attrs: &[Attr], name: &str) -> Option<i64> {
    attrs
        .iter()
        .find(|a| a.name == Symbol::intern(name))
        .and_then(|a| match &a.kind {
            AttrKind::Prop {
                value: Expr::IntLit(n, _),
            } => Some(*n),
            _ => None,
        })
}

#[test]
fn disabled_state_block_recolours_in_the_same_frame() {
    // A `disabled:` box with an `on disabled { bg }` block resolves the
    // DISABLED state on the very frame it renders (the router is marked
    // before state styles resolve), so the disabled bg wins immediately.
    let parsed = parse(
        "View V() { \
             let btn = style { bg: 0x111111 on disabled { bg: 0x445566 } } \
             Box #[..btn, disabled: true, width: 40, height: 20] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let inst = frame.instances()[0];
    assert!(
        (inst.color[0] - linear(0x44)).abs() < 1e-3,
        "disabled bg overlays the base, got {:?}",
        inst.color
    );
}

#[test]
fn hover_state_block_recolours_after_pointer_enters() {
    // RFC-0016: an `on hover { bg }` block lights up once the pointer moves
    // over the element, even though the element registers no handler of its
    // own (it is tracked as a bare hover region).
    let parsed = parse(
        "View V() { \
             let btn = style { bg: 0x111111 on hover { bg: 0x445566 } } \
             Box #[..btn, width: 40, height: 20] {} }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    // First frame: pointer hasn't entered, base bg (0x11 red channel).
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (frame.instances()[0].color[0] - linear(0x11)).abs() < 1e-3,
        "base bg before hover, got {:?}",
        frame.instances()[0].color
    );

    // Move the pointer inside the box, then re-render.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerMove,
        pos: (10.0, 10.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    assert!(
        (frame2.instances()[0].color[0] - linear(0x44)).abs() < 1e-3,
        "hover bg overlays after the pointer enters, got {:?}",
        frame2.instances()[0].color
    );
}

#[test]
fn unknown_state_name_is_an_error_with_a_hint() {
    let parsed = parse("View V() { let s = style { bg: 1 on hoover { bg: 2 } } Box #[..s] {} }");
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| matches!(e, CompileError::UnknownStyleState { .. })),
        "an unknown state name must be an UnknownStyleState error, got {:?}",
        parsed.errors
    );
}

#[test]
fn spreading_a_non_style_is_an_error() {
    let parsed = parse("View V() { let x = 5 Box #[..x, width: 10, height: 10] {} }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(view, &[]);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::NotAStyle { .. })),
        "spreading a non-style must be a NotAStyle error, got {:?}",
        interp.errors()
    );
}

#[test]
fn no_transform_attrs_produces_identity() {
    let parsed = parse("View C() { Box #[bg: 0xFF0000, width: 50, height: 50] }");
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // `origin` alone isn't checked against `Transform::IDENTITY`'s [0,0]:
    // the compiler defaults an unset `origin` to the element's own
    // center (RFC-0011's stated default), which is a real but *inert*
    // difference from the engine's raw identity, pivot is irrelevant
    // when scale = 1 and rotate = 0, so the render is pixel-identical.
    let t = frame.instances()[0].transform;
    assert_eq!(t.translate, [0.0, 0.0]);
    assert_eq!(t.scale, [1.0, 1.0]);
    assert_eq!(t.rotate, 0.0);
    assert_eq!(t.opacity, 1.0);
}
