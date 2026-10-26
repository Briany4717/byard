//! Overlays: the z-layer, modal scrims, dismissal and anchored placement.

use super::*;

// ── RFC-0017: Overlay & z-layer system ───────────────────────────────

/// Finds the emitted solid box closest to the given colour channels.
fn find_solid_by_red(
    frame: &byard_core::frame::RenderFrame,
    red: f32,
) -> Option<(usize, byard_core::BoxInstance)> {
    frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - red).abs() < 0.05)
        .map(|(i, b)| (i, *b))
}

#[test]
fn overlay_takes_no_flow_space_and_paints_above_main() {
    // A main box (red) followed by an overlay whose scrim (blue, grow:1)
    // fills the viewport. The overlay must not displace the main box, and
    // its scrim must paint *above* the main box (nearer draw-order depth).
    let src = "View C() {\n \
            Box #[bg: 0xFF0000, width: 40, height: 40] {}\n \
            Overlay #[modal: false] {\n \
                Box #[bg: 0x0000FF, grow: 1] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let (red_i, red) = find_solid_by_red(&frame, 1.0).expect("main red box emitted");
    let (blue_i, blue) = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| b.color[2] > 0.95 && b.color[0] < 0.05)
        .map(|(i, b)| (i, *b))
        .expect("overlay scrim emitted");

    // Main box keeps its natural 40×40 at the origin, the overlay's 0×0
    // flow leaf did not push it down.
    assert!((red.rect[0]).abs() < 0.01 && (red.rect[1]).abs() < 0.01);
    assert!((red.rect[2] - 40.0).abs() < 0.01);
    // The scrim fills the whole viewport.
    assert!((blue.rect[2] - 400.0).abs() < 0.5 && (blue.rect[3] - 300.0).abs() < 0.5);
    // Painter's order: the overlay is emitted after the main tree, so its
    // depth is strictly nearer (smaller NDC-z) → it composites on top.
    assert!(
        frame.solid_depths()[blue_i] < frame.solid_depths()[red_i],
        "overlay scrim must paint above the main box"
    );
}

#[test]
fn overlay_center_anchor_positions_content_in_the_viewport() {
    let src = "View C() {\n \
            Overlay {\n \
                Column #[anchor: center, bg: 0x222222, width: 100, height: 60] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let dialog = frame
        .instances()
        .iter()
        .find(|b| (b.color[0] - linear(0x22)).abs() < 0.02)
        .expect("dialog emitted");
    // Centred: (400−100)/2 = 150, (300−60)/2 = 120.
    assert!(
        (dialog.rect[0] - 150.0).abs() < 1.0,
        "x centred, got {}",
        dialog.rect[0]
    );
    assert!(
        (dialog.rect[1] - 120.0).abs() < 1.0,
        "y centred, got {}",
        dialog.rect[1]
    );
}

#[test]
fn overlay_bottom_anchor_pins_content_to_the_viewport_bottom() {
    let src = "View C() {\n \
            Overlay {\n \
                Column #[anchor: bottom, bg: 0x333333, width: 200, height: 80] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let sheet = frame
        .instances()
        .iter()
        .find(|b| (b.color[0] - linear(0x33)).abs() < 0.02)
        .expect("sheet emitted");
    // Pinned to the bottom: y = 300 − 80 = 220; centred x = (400−200)/2 = 100.
    assert!(
        (sheet.rect[1] - 220.0).abs() < 1.0,
        "y bottom, got {}",
        sheet.rect[1]
    );
    assert!(
        (sheet.rect[0] - 100.0).abs() < 1.0,
        "x centred, got {}",
        sheet.rect[0]
    );
}

#[test]
fn modal_overlay_blocks_the_main_tree_and_dismisses_on_outside_tap() {
    // A main button sits behind a modal overlay. Its scrim fills the
    // viewport; a small confirm button is centred. Tapping the scrim (an
    // outside tap) fires `dismiss` and must NOT reach the button behind.
    let src = "View C() {\n \
            var open = true\n \
            var behind = false\n \
            var confirmed = false\n \
            Button(\"behind\") #[width: 400, height: 300] => behind = true\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[bg: 0x000000, opacity: 0.3, grow: 1] {}\n \
                Column #[anchor: center, width: 80, height: 40] {\n \
                    Button(\"ok\") #[width: 80, height: 40] => confirmed = true\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    let behind = interp
        .var_signal(&crate::symbol::Symbol::intern("behind"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap the top-left corner, over the scrim, outside the centred content.
    let tap = |t: u64, p: (f32, f32)| {
        [
            byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::PointerDown,
                pos: p,
                delta: (0.0, 0.0),
                payload: None,
                time_ms: t,
                mods: byard_core::KeyModifiers::default(),
            },
            byard_core::platform::InputEvent {
                kind: byard_core::platform::EventKind::PointerUp,
                pos: p,
                delta: (0.0, 0.0),
                payload: None,
                time_ms: t + 20,
                mods: byard_core::KeyModifiers::default(),
            },
        ]
    };
    interp.dispatch_events(&tap(0, (10.0, 10.0)));
    interp.tick();

    assert_eq!(
        interp.peek(open),
        Value::Bool(false),
        "outside tap dismissed"
    );
    assert_eq!(
        interp.peek(behind),
        Value::Bool(false),
        "modal scrim blocked the button behind it"
    );
}

#[test]
fn modal_overlay_content_wins_over_the_scrim() {
    // A tap on the centred confirm button fires its action, not the scrim's
    // dismiss, the content is registered after the scrim, so it wins.
    let src = "View C() {\n \
            var open = true\n \
            var confirmed = false\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[bg: 0x000000, grow: 1] {}\n \
                Column #[anchor: center, width: 80, height: 40] {\n \
                    Button(\"ok\") #[width: 80, height: 40] => confirmed = true\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    let confirmed = interp
        .var_signal(&crate::symbol::Symbol::intern("confirmed"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Centre of the viewport = centre of the confirm button.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (200.0, 150.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (200.0, 150.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();

    assert_eq!(
        interp.peek(confirmed),
        Value::Bool(true),
        "content button fired"
    );
    assert_eq!(
        interp.peek(open),
        Value::Bool(true),
        "scrim dismiss did NOT fire"
    );
}

#[test]
fn escape_dismisses_the_topmost_modal_overlay() {
    let src = "View C() {\n \
            var open = true\n \
            Overlay #[modal: true, dismiss => open = false] {\n \
                Box #[grow: 1] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let open = interp
        .var_signal(&crate::symbol::Symbol::intern("open"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::KeyDown,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::platform::InputPayload::Key("Escape".into())),
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    interp.tick();
    assert_eq!(
        interp.peek(open),
        Value::Bool(false),
        "Escape dismissed the modal"
    );
}

#[test]
fn when_gated_overlay_unmounts_live_on_dismiss() {
    // RFC-0018 × RFC-0017: a `when`-gated modal overlay now dismisses at
    // runtime, tapping the scrim flips the guard `var`, and the very next
    // render unmounts the overlay (no hot-reload needed). This is the headline
    // reactivity win: overlays are live.
    let src = "View C() {\n \
            var open = true\n \
            when open {\n \
                Overlay #[modal: true, dismiss => open = false] {\n \
                    Box #[bg: 0x000000, opacity: 0.4, grow: 1] {}\n \
                    Column #[anchor: center, bg: 0xFFFFFF, width: 100, height: 60] {}\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // The overlay is mounted: the white dialog surface is present.
    assert!(
        frame.instances().iter().any(|b| b.color[0] > 0.95),
        "overlay mounted initially"
    );

    // Tap the scrim (top-left, outside the centred content).
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
            time_ms: 20,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    // `open` flipped false → the `when` unmounts the overlay entirely.
    assert!(
        !frame2.instances().iter().any(|b| b.color[0] > 0.95),
        "overlay unmounted live after dismiss"
    );
    assert!(frame2.instances().is_empty(), "nothing left on screen");
}

#[test]
fn non_modal_overlay_lets_taps_fall_through() {
    // A non-modal overlay (a snackbar-style surface) must not block the main
    // tree: a tap on the button behind still fires.
    let src = "View C() {\n \
            var behind = false\n \
            Button(\"behind\") #[width: 400, height: 300] => behind = true\n \
            Overlay #[modal: false] {\n \
                Column #[anchor: bottom, width: 100, height: 20] {}\n \
            }\n\
        }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let behind = interp
        .var_signal(&crate::symbol::Symbol::intern("behind"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tap top-left, away from the bottom-anchored surface.
    interp.dispatch_events(&[
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerDown,
            pos: (10.0, 10.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        },
        byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::PointerUp,
            pos: (10.0, 10.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 20,
            mods: byard_core::KeyModifiers::default(),
        },
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(behind),
        Value::Bool(true),
        "non-modal overlay let the tap fall through"
    );
}

#[test]
fn overlay_demo_example_renders_dialog_above_the_base_app() {
    // Ties the shipped visual example to the test suite: it must parse,
    // lower, and render, with the modal dialog surface compositing above the
    // base app (RFC-0017). Guards the demo against silent breakage.
    let src = include_str!("../../../../examples/overlay_demo.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 900.0, 560.0);

    // The base app background (0x14141C) is emitted early; the dialog surface
    // (0xECE6F0) is an overlay emitted later, so it sits at a nearer depth than
    // the base app. Both are matched by their written colour, decoded: the
    // frame is in linear light.
    let base = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x14)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("base app background emitted");
    let (dialog, dialog_box) = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| {
            (b.color[0] - linear(0xEC)).abs() < 0.02 && (b.color[1] - linear(0xE6)).abs() < 0.02
        })
        .map(|(i, b)| (i, *b))
        .expect("dialog surface emitted");
    assert!(
        frame.solid_depths()[dialog] < frame.solid_depths()[base],
        "the modal dialog must composite above the base app"
    );

    // No dialog text line may overflow the dialog surface, line wrap is not
    // built yet, so the example is authored to fit. Guards the reported
    // overflow against regression: every dark-on-light label painted inside
    // the surface must end before the surface's right edge.
    let mut measurer = byard_core::text::TextMeasurer::new();
    let surf_left = dialog_box.rect[0];
    let surf_right = dialog_box.rect[0] + dialog_box.rect[2];
    for (line, wrap) in frame.texts().iter().zip(frame.text_wraps()) {
        let inside = line.x >= surf_left && line.x < surf_right && line.color[0] < 0.5;
        if inside {
            // Honour the wrap width (RFC-0018): a wrapped label's laid-out
            // width is bounded to it, not the full one-line measurement.
            let (w, _) = measurer.measure_wrapped(&line.text, line.font_size, *wrap, 400, None);
            assert!(
                line.x + w <= surf_right + 0.5,
                "dialog text {:?} overflows the surface: {} + {} > {}",
                line.text,
                line.x,
                w,
                surf_right
            );
        }
    }
}

#[test]
fn nested_overlays_stack_in_mount_order() {
    // An overlay whose content mounts a second overlay: both are collected,
    // and the inner one is emitted later (on top).
    let src = "View C() {\n \
            Overlay #[modal: false] {\n \
                Box #[bg: 0x111111, grow: 1] {}\n \
                Overlay #[modal: false] {\n \
                    Box #[bg: 0x222222, grow: 1] {}\n \
                }\n \
            }\n\
        }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let outer = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x11)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("outer overlay box");
    let inner = frame
        .instances()
        .iter()
        .enumerate()
        .find(|(_, b)| (b.color[0] - linear(0x22)).abs() < 0.01)
        .map(|(i, _)| i)
        .expect("inner overlay box");
    assert!(
        frame.solid_depths()[inner] < frame.solid_depths()[outer],
        "nested overlay stacks above its parent overlay"
    );
}
