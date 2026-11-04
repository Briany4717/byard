//! The value controls and the text field: write-back, `Checkbox`,
//! `RadioButton`, `Slider`, keyboard delivery and Tab traversal.

use super::*;

// ── Toggle/Slider/TextField write-back ──────────────────────────

#[test]
fn toggle_with_bg_has_no_background_slab() {
    // Regression: `bg` on a Toggle is the ON accent, not a full-rect fill
    // painted behind the control (that stray slab made widgets look "off").
    let parsed = parse(
        "View C() {\n var on = true\n Toggle #[bind: on, bg: 0x10B981, width: 52, height: 30]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // Exactly track + thumb, no extra background rectangle, no DecoratedBox.
    assert_eq!(
        frame.instances().len(),
        2,
        "toggle should emit only track + thumb"
    );
    assert_eq!(frame.decorated().len(), 0);
}

#[test]
fn slider_with_bg_has_no_background_slab() {
    // Regression: `bg` on a Slider is the fill accent, not a full-rect fill.
    let parsed = parse(
        "View C() {\n var v = 0.5\n Slider #[bind: v, bg: 0xEF4444, min: 0.0, max: 1.0, width: 200, height: 24]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // track + fill + thumb(accent disc) + thumb(white inner) = 4; no slab.
    assert_eq!(
        frame.instances().len(),
        4,
        "slider should emit track + fill + thumb (2 discs), no slab"
    );
}

#[test]
fn toggle_tap_flips_bound_var() {
    let parsed = parse("View C() {\n var on = false\n Toggle #[bind: on]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("on"))
        .unwrap();
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(false));

    // Simulate a render so handlers are registered.
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap inside the Toggle rect.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Bool(true),
        "toggle flipped to true"
    );

    // Second tap flips back, gap > DOUBLE_TAP_MS (300ms) so it's a plain tap.
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 400,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 450,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(false), "toggle flipped back");
}

// ── RFC-0018: Checkbox ────────────────────────────────────────────────

/// Renders `src`'s first view and returns `(instances, decorated)` counts,
/// plus the first decorated box, for the Checkbox visual tests.
pub(super) fn checkbox_frame(src: &str) -> byard_core::frame::RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame
}

#[test]
fn unchecked_checkbox_is_a_single_borderless_slot() {
    // Unchecked with no styled border: one decorated slot, no mark. (The
    // container is a DecoratedBox so it *can* carry a border when styled; with
    // none, it's a borderless muted fill.)
    let frame = checkbox_frame("View C() {\n var c = false\n Checkbox #[bind: c]\n}");
    assert_eq!(frame.instances().len(), 0, "no solid instances");
    assert_eq!(frame.decorated().len(), 1, "just the muted slot, no mark");
    let slot = frame.decorated()[0];
    assert!(
        slot.base.color[3] > 0.0,
        "the slot has an opaque muted fill"
    );
    assert_eq!(slot.border_width, 0.0, "no border when none is styled");
}

#[test]
fn checked_checkbox_fills_and_draws_a_two_stroke_check() {
    // Checked: a filled accent square + two checkmark stroke quads on top,
    // three decorated boxes (all on the decorated pipeline, in push order, so
    // the check is never hidden behind the fill).
    let frame = checkbox_frame("View C() {\n var c = true\n Checkbox #[bind: c]\n}");
    assert_eq!(frame.instances().len(), 0, "no solid instances");
    assert_eq!(
        frame.decorated().len(),
        3,
        "filled square + two rotated stroke quads"
    );
    assert!(
        frame.decorated()[0].base.color[3] > 0.0,
        "the checked square has an opaque accent fill"
    );
    // The two strokes rotate in opposite senses about their midpoints, proof
    // the mark is angled geometry, not two axis-aligned bars.
    let r1 = frame.decorated()[1].base.transform.rotate;
    let r2 = frame.decorated()[2].base.transform.rotate;
    assert!(
        (r1 - r2).abs() > 0.1,
        "the two strokes are at different angles"
    );
}

#[test]
fn indeterminate_checkbox_fills_and_draws_a_single_dash() {
    // Mixed state: filled square + one horizontal bar, no checkmark.
    let frame =
        checkbox_frame("View C() {\n var c = false\n Checkbox #[bind: c, indeterminate: true]\n}");
    assert_eq!(frame.instances().len(), 0, "no solid instances");
    assert_eq!(frame.decorated().len(), 2, "filled square + one dash");
    assert!(
        frame.decorated()[0].base.color[3] > 0.0,
        "filled accent square"
    );
}

#[test]
fn checkbox_bg_is_the_accent_not_a_full_rect_slab() {
    // Regression parity with Toggle/Slider: `bg` on a Checkbox is the checked
    // accent (the box fill), never a background slab, a checked box is the
    // filled square plus the two mark strokes, nothing more.
    let frame = checkbox_frame(
        "View C() {\n var c = true\n Checkbox #[bind: c, bg: 0x10B981, width: 24, height: 24]\n}",
    );
    assert_eq!(
        frame.decorated().len(),
        3,
        "filled square + the two checkmark strokes"
    );
    let fill = frame.decorated()[0].base.color;
    let accent = crate::interp::intrinsics::color_to_rgba(0x0010_B981, false);
    assert!(
        (fill[0] - accent[0]).abs() < 0.01 && (fill[1] - accent[1]).abs() < 0.01,
        "the filled square carries the `bg` accent"
    );
}

#[test]
fn checkbox_tap_flips_bound_var() {
    let parsed = parse("View C() {\n var c = false\n Checkbox #[bind: c]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("c"))
        .unwrap();
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(false));

    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Bool(true), "tap checked the box");
}

#[test]
fn checkbox_space_key_toggles_when_focused() {
    let parsed = parse("View C() {\n var c = false\n Checkbox #[bind: c]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("c"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // Click to focus the box, then press Space.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::KeyDown,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload: Some(byard_core::platform::InputPayload::Key(" ".to_string())),
            time_ms: 10,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Bool(true),
        "Space toggled the focused checkbox"
    );
}

// ── RFC-0018: RadioButton ─────────────────────────────────────────────

#[test]
fn unselected_radio_is_a_ring_only() {
    // `choice != value` → the ring only, no inner dot.
    let frame = checkbox_frame(
        "View C() {\n var choice = \"work\"\n RadioButton #[value: \"home\", bind: choice]\n}",
    );
    assert_eq!(frame.instances().len(), 0, "no inner dot when unselected");
    assert_eq!(frame.decorated().len(), 1, "just the outer ring");
    assert!(
        frame.decorated()[0].border_width > 0.0,
        "the ring is a bordered decorated box"
    );
    assert_eq!(
        frame.decorated()[0].base.color[3],
        0.0,
        "the ring interior is transparent"
    );
}

#[test]
fn selected_radio_draws_ring_plus_inner_dot() {
    // `choice == value` → the ring plus a filled inner dot.
    let frame = checkbox_frame(
        "View C() {\n var choice = \"home\"\n RadioButton #[value: \"home\", bind: choice]\n}",
    );
    assert_eq!(frame.decorated().len(), 1, "the outer ring");
    assert_eq!(frame.instances().len(), 1, "the inner dot");
    assert!(
        frame.instances()[0].color[3] > 0.0,
        "the dot has an opaque accent fill"
    );
}

#[test]
fn radio_tap_selects_its_value() {
    let parsed = parse(
        "View C() {\n var choice = \"home\"\n RadioButton #[value: \"work\", bind: choice]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("choice"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Str("work".to_string()),
        "tapping the radio wrote its value to the group var"
    );
}

#[test]
fn radio_group_is_mutually_exclusive_via_the_shared_var() {
    // Two radios on one `var`: tapping the second selects it, which
    // deselects the first (they read the same var, no explicit exclusion).
    let parsed = parse(
        "View C() {\n var choice = \"home\"\n \
             Column #[gap: 40] {\n \
               RadioButton #[value: \"home\", bind: choice, width: 44, height: 44]\n \
               RadioButton #[value: \"work\", bind: choice, width: 44, height: 44]\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("choice"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // Tap the SECOND radio (below the first, gap 40 keeps hit rects apart).
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (20.0, 106.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (20.0, 106.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(sig),
        Value::Str("work".to_string()),
        "the group var now holds the second radio's value"
    );
}

#[test]
fn radio_arrow_keys_move_selection_with_wrap() {
    let parsed = parse(
        "View C() {\n var choice = \"home\"\n \
             Column #[gap: 40] {\n \
               RadioButton #[value: \"home\", bind: choice, width: 44, height: 44]\n \
               RadioButton #[value: \"work\", bind: choice, width: 44, height: 44]\n \
               RadioButton #[value: \"other\", bind: choice, width: 44, height: 44]\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("choice"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();

    // Focus the FIRST radio with a press (no release → no tap, so `choice`
    // stays "home").
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (20.0, 20.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();
    assert_eq!(interp.peek(sig), Value::Str("home".to_string()));

    // A helper: re-render (repopulates the group ordering + handlers), press
    // the arrow, tick, and read back the group var.
    let press = |interp: &mut Interpreter, key: &str| -> String {
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 400.0, 300.0);
        interp.dispatch_events(&[byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::KeyDown,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload: Some(byard_core::platform::InputPayload::Key(key.to_string())),
            time_ms: 10,
            mods: byard_core::KeyModifiers::default(),
        }]);
        interp.tick();
        match interp.peek(sig) {
            Value::Str(s) => s,
            other => panic!("expected Str, got {other:?}"),
        }
    };

    assert_eq!(press(&mut interp, "ArrowDown"), "work", "home → work");
    assert_eq!(press(&mut interp, "ArrowDown"), "other", "work → other");
    assert_eq!(
        press(&mut interp, "ArrowDown"),
        "home",
        "other → home (forward wrap)"
    );
    assert_eq!(
        press(&mut interp, "ArrowUp"),
        "other",
        "home → other (backward wrap)"
    );
}

#[test]
fn slider_value_has_no_f32_widening_tail() {
    // Regression: a drag landing on 0.6 used to be stored as
    // `f64::from(0.6_f32)` = 0.6000000238418579 because the value math ran
    // in f32 and was only widened at the end. The value path now stays in
    // f64, so a pixel-aligned 60% drag round-trips to a clean "0.6".
    let parsed =
        parse("View C() {\n var vol = 0.0\n Slider #[bind: vol, min: 0, max: 1, width: 100]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("vol"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // x = 60 on a 100px track starting at x = 0 → exactly 60%.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (60.0, 5.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();

    let val = match interp.peek(sig) {
        Value::Float(f) => f,
        other => panic!("expected Float, got {other:?}"),
    };
    assert_eq!(
        format!("{val}"),
        "0.6",
        "slider value must not carry an f32 widening tail"
    );
}

#[test]
fn slider_with_step_does_not_emit_more_decimals_than_the_step() {
    // step: 0.1 landing on 60% used to store 6 * 0.1 = 0.6000000000000001.
    // The value is now rounded to the step's precision → a clean "0.6".
    let parsed = parse(
        "View C() {\n var vol = 0.0\n Slider #[bind: vol, min: 0, max: 1, step: 0.1, width: 100]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("vol"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (60.0, 5.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();

    let val = match interp.peek(sig) {
        Value::Float(f) => f,
        other => panic!("expected Float, got {other:?}"),
    };
    assert_eq!(format!("{val}"), "0.6");
}

#[test]
fn text_field_change_event_round_trips() {
    let parsed = parse("View C() {\n var query = \"\"\n TextField #[bind: query]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let sig = interp
        .var_signal(&crate::symbol::Symbol::intern("query"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Change event with new value.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::Change,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Str("hello".to_string())),
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    assert_eq!(interp.peek(sig), Value::Str("hello".to_string()));

    // Re-delivering the same value is deduped (E1).
    let before = interp.peek(sig);
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::Change,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Str("hello".to_string())),
        time_ms: 1,
        mods: byard_core::KeyModifiers::default(),
    }]);
    assert_eq!(interp.peek(sig), before, "equal value deduped");
}

#[test]
fn bind_to_non_var_produces_no_bound_sig() {
    // `let y = 0` is not a var → resolve_bind_sig returns None.
    let parsed = parse("View C() {\n let y = 0\n Toggle #[bind: y]\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    // No error expected at lowering; just bound_sig is None (non-var silently ignored).
    let RenderNode::Box { bound_sig, .. } = &tree[0] else {
        panic!("expected Box");
    };
    assert!(bound_sig.is_none(), "let binding yields no bound_sig");
}

// ── Keyboard delivery ───────────────────────────────────────────

#[test]
fn text_field_receives_keyboard_text_input() {
    let parsed2 = parse("View C() {\n var text = \"\"\n TextField #[bind: text]\n}");
    assert!(parsed2.errors.is_empty(), "{:?}", parsed2.errors);
    let view2 = &parsed2.views[0];
    let mut interp2 = Interpreter::new();
    let tree2 = interp2.lower_view(view2, &[]);
    let sig = interp2
        .var_signal(&crate::symbol::Symbol::intern("text"))
        .unwrap();
    interp2.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp2.render(&tree2, &mut frame, 400.0, 300.0);

    // Focus the TextField by tapping it first.
    interp2.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (5.0, 5.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 50,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp2.tick();
    interp2.render(&tree2, &mut frame, 400.0, 300.0);

    // Type "ab".
    interp2.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::TextInput,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("a".to_string())),
        time_ms: 100,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp2.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::TextInput,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("b".to_string())),
        time_ms: 200,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp2.tick();
    assert_eq!(
        interp2.peek(sig),
        Value::Str("ab".to_string()),
        "typed 'ab'"
    );

    // Backspace removes last char.
    interp2.render(&tree2, &mut frame, 400.0, 300.0);
    interp2.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (5.0, 5.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key(
            "Backspace".to_string(),
        )),
        time_ms: 300,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp2.tick();
    assert_eq!(
        interp2.peek(sig),
        Value::Str("a".to_string()),
        "backspace deleted 'b'"
    );
}

// ── Tab focus traversal ─────────────────────────────────────────

#[test]
fn tab_key_advances_focus_through_text_fields() {
    // Two TextFields, Tab should cycle between them.
    let parsed = parse(
        "View C() {\n var fa = false\n var fb = false\n TextField #[bind: fa, focused: fa]\n TextField #[bind: fb, focused: fb]\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let fa = interp
        .var_signal(&crate::symbol::Symbol::intern("fa"))
        .unwrap();
    let fb = interp
        .var_signal(&crate::symbol::Symbol::intern("fb"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tab: should focus the first field (none focused yet → index 0).
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("Tab".to_string())),
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();
    assert_eq!(
        interp.peek(fa),
        Value::Bool(true),
        "first field focused after Tab"
    );
    assert_eq!(interp.peek(fb), Value::Bool(false));

    // Second Tab: advances to second field.
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("Tab".to_string())),
        time_ms: 100,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();
    assert_eq!(interp.peek(fa), Value::Bool(false), "first field blurred");
    assert_eq!(interp.peek(fb), Value::Bool(true), "second field focused");
}
