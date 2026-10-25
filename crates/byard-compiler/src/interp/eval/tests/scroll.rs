//! Scrolling: offsets, snapping, paging, collapsing headers, pull-to-refresh,
//! windowed lists and hit-testing under a scroll.

use super::*;

/// RFC-0005: the mouse wheel over a `ScrollView` scrolls it by writing the
/// signal behind `offset.y`, clamped to `[0, content − viewport]`.
#[test]
fn wheel_over_a_scrollview_scrolls_and_clamps_the_offset() {
    // Content = 4 × 60px = 240 tall in a 100px viewport → max scroll 140.
    let src = "View V() { var off: Float = 0.0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Box #[bg: 0xFF0000, width: 180, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                     Box #[bg: 0xFFFF00, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    // Render once to record the scroll target (viewport at the top-left).
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    let peek_f = |interp: &Interpreter| -> f32 {
        match interp.peek(off) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => panic!("offset must be numeric"),
        }
    };
    let wheel = |interp: &mut Interpreter, dy: f32| {
        interp.dispatch_events(&[byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::Wheel,
            pos: (100.0, 50.0), // inside the 200×100 viewport
            delta: (0.0, dy),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        }]);
        interp.tick();
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 400.0, 300.0);
    };

    // Wheel forward by 2 lines (× 40px) → scroll down by 80.
    wheel(&mut interp, -2.0);
    let after_one = peek_f(&interp);
    assert!(
        (after_one - 80.0).abs() < 1.0,
        "one wheel notch scrolls by lines×40, got {after_one}"
    );

    // A big wheel clamps to the content extent (max = 240 − 100 = 140).
    wheel(&mut interp, -20.0);
    let clamped = peek_f(&interp);
    assert!(
        (clamped - 140.0).abs() < 1.0,
        "scroll must clamp to content−viewport, got {clamped}"
    );

    // Wheel back up past the top clamps at 0.
    wheel(&mut interp, 20.0);
    let top = peek_f(&interp);
    assert!(top.abs() < 1.0, "scroll must clamp at 0, got {top}");
}

/// RFC-0005 emission culling: a `ScrollView` child scrolled entirely out of
/// the viewport is never pushed to the frame (only its visible slice costs
/// anything), while the flat-id cursor stays aligned so siblings still paint.
#[test]
fn scrollview_culls_children_scrolled_out_of_view() {
    let src = "View V() { var off: Int = 0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Box #[bg: 0xFF0000, width: 180, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    // Present iff a box with the given dominant colour channel was emitted.
    let has = |interp: &mut Interpreter, chan: usize| -> bool {
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .any(|b| b.color[chan] > 0.8 && b.color[(chan + 1) % 3] < 0.3)
    };

    // At rest, the third box (y 120..180) sits fully below the 100px
    // viewport → culled. The first two overlap it → kept.
    assert!(has(&mut interp, 0), "red (top) is visible at rest");
    assert!(has(&mut interp, 1), "green (straddling) is visible at rest");
    assert!(!has(&mut interp, 2), "blue (below) is culled at rest");

    // Scroll down 120px: the first box (now y -120..-60) leaves the top →
    // culled; the third box scrolls into view.
    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    interp.write_var(off, Value::Int(120));
    interp.tick();
    assert!(
        !has(&mut interp, 0),
        "red is culled once scrolled past the top"
    );
    assert!(has(&mut interp, 2), "blue scrolls into view");
}

/// RFC-0005: dragging on inert `ScrollView` content scrolls it, the content
/// tracks the pointer between press and release, clamped to the extent.
#[test]
fn drag_on_scrollview_content_scrolls_and_clamps() {
    use byard_core::platform::EventKind as K;
    // Content = 4 × 60px = 240 tall in a 100px viewport → max scroll 140.
    let src = "View V() { var off: Float = 0.0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Box #[bg: 0xFF0000, width: 180, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                     Box #[bg: 0xFFFF00, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    let peek_f = |interp: &Interpreter| match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("offset must be numeric"),
    };
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };

    // Press on inert content, then drag up 50px → content scrolls down 50.
    interp.dispatch_events(&[ev(K::PointerDown, 100.0, 80.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, 30.0)]);
    assert!(
        (peek_f(&interp) - 50.0).abs() < 1.0,
        "drag up 50px scrolls down 50, got {}",
        peek_f(&interp)
    );

    // Dragging further up clamps at the content extent (140).
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, -200.0)]);
    assert!(
        (peek_f(&interp) - 140.0).abs() < 1.0,
        "drag clamps to content−viewport, got {}",
        peek_f(&interp)
    );

    // Releasing ends the gesture: a later stray move no longer scrolls.
    interp.dispatch_events(&[ev(K::PointerUp, 100.0, -200.0)]);
    let held = peek_f(&interp);
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, 300.0)]);
    assert!(
        (peek_f(&interp) - held).abs() < 0.01,
        "no drag is in flight after release, got {} (was {held})",
        peek_f(&interp)
    );
}

// ── RFC-0021: snap + pagination + on_end_reached ─────────────────────

#[test]
fn page_snap_settles_to_nearest_page_on_release() {
    use byard_core::platform::EventKind as K;
    // Horizontal `snap: page`: three 100px pages in a 100px viewport (max
    // 200). Drag left 60px → offset 60; release snaps to page 1 (offset 100),
    // reflects `page`, and would fire `page_change`.
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var pg: Int = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), page: pg, \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let pg = interp.var_signal(&Symbol::intern("pg")).unwrap();
    let peek_f = |interp: &Interpreter| match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };

    interp.dispatch_events(&[ev(K::PointerDown, 50.0, 50.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, -10.0, 50.0)]); // drag left 60 → offX 60
    assert!(
        (peek_f(&interp) - 60.0).abs() < 2.0,
        "mid-drag offset ~60, got {}",
        peek_f(&interp)
    );
    interp.dispatch_events(&[ev(K::PointerUp, -10.0, 50.0)]); // release → snap
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "snapped to page 1 (offset 100), got {}",
        peek_f(&interp)
    );
    assert_eq!(interp.peek(pg), Value::Int(1), "the reflected page is 1");
}

#[test]
fn on_end_reached_fires_once_past_threshold() {
    use byard_core::platform::EventKind as K;
    // 400px of content in a 100px viewport (max 300); `end_threshold: 0.8`.
    // Drag up 250 → offset 250 → visible bottom (250+100)/400 = 0.875 ≥ 0.8,
    // so `end_reached` fires and sets `loaded`.
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var loaded = false \
             ScrollView #[offset: (offX, offY), width: 100, height: 100, \
                          end_threshold: 0.8, end_reached => loaded = true] { \
                 Column { Box #[width: 100, height: 400] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let loaded = interp.var_signal(&Symbol::intern("loaded")).unwrap();
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };
    assert_eq!(
        interp.peek(loaded),
        Value::Bool(false),
        "not loaded at rest"
    );

    interp.dispatch_events(&[ev(K::PointerDown, 50.0, 50.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 50.0, -200.0)]); // drag up 250 → offY 250
    interp.tick();
    assert_eq!(
        interp.peek(loaded),
        Value::Bool(true),
        "end_reached fired past the 0.8 threshold"
    );
}

#[test]
fn setting_the_page_var_scrolls_to_that_page() {
    // Reflected `page:` (reverse): writing `page = 2` scrolls the horizontal
    // offset to page 2 (offset 200) on the next render.
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var pg: Int = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), page: pg, \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0); // records the scroll target

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let pg = interp.var_signal(&Symbol::intern("pg")).unwrap();

    interp.write_var(pg, Value::Int(2));
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 300.0); // sync scrolls to page 2

    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 200.0).abs() < 1.0,
        "page = 2 scrolled the offset to 200, got {got}"
    );
}

#[test]
fn page_reflects_on_wheel_scroll_without_a_release() {
    use byard_core::platform::EventKind as K;
    // A trackpad/wheel `Scroll` updates the reflected `page` continuously,
    // no drag release needed (the desktop scrolling case).
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 var pg: Int = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), page: pg, \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let pg = interp.var_signal(&Symbol::intern("pg")).unwrap();
    // Scroll right ~120px (a `Scroll` delta is pixels): offX → 120, page → 1.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-120.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    assert_eq!(
        interp.peek(pg),
        Value::Int(1),
        "page reflects the wheel scroll"
    );
}

#[test]
fn wheel_scroll_snaps_to_a_page_after_settling() {
    use byard_core::platform::EventKind as K;
    // Wheel-scroll 60px, then hold still: once the offset stops moving for a
    // few frames the settle fires and snaps to page 1, no release event, no
    // wall clock, just observed stillness (clock-independent settle).
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-60.0, 0.0), // offX → 60 (not yet a page boundary)
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    // Render successive idle frames with the offset held still; the settle
    // counts stable frames and snaps once it reaches its threshold.
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 100.0).abs() < 1.0,
        "wheel scroll settled and snapped to page 1 (offset 100), got {got}"
    );
}

/// RFC-0021: the stillness settle must not fire while a page is *actively*
/// being scrolled, the offset moving each frame restarts the settle count,
/// so a mid-scroll frame never snaps out from under the motion.
#[test]
fn wheel_scroll_does_not_snap_while_still_moving() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX: Float = 0.0 var offY: Float = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let wheel = |dx: f32| byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (dx, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };
    // Several small scrolls, each followed by a render (offset keeps moving),
    // so the settle count resets every frame and never snaps mid-motion.
    for _ in 0..3 {
        interp.dispatch_events(&[wheel(-20.0)]);
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 60.0).abs() < 1.0,
        "offset must track the in-progress scroll (60), not snap early, got {got}"
    );
}

/// RFC-0021: an enum keyword prop (`snap: page`) must keep working even when
/// the view declares a `var` of the *same name* as the keyword, the token is
/// read from the AST, so the variable can never shadow it. This is exactly the
/// `scroll_snap` example's shape (`var page` + `snap: page`), which silently
/// disabled snapping before enum props stopped resolving through the env.
#[test]
fn snap_page_keyword_is_not_shadowed_by_a_same_named_var() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX = 0.0 var offY = 0.0 var page = 0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          page: page, width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let pg = interp.var_signal(&Symbol::intern("page")).unwrap();
    // Wheel two-thirds of a page (past the snap midpoint) then hold still: the
    // stillness settle must snap forward to page 1, not stay put.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-70.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 100.0).abs() < 1.0,
        "`snap: page` must engage despite the `var page`; snapped to {got}, want 100"
    );
    assert_eq!(interp.peek(pg), Value::Int(1), "reflected page is 1");
}

fn page_snap_view() -> &'static str {
    "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }"
}

/// RFC-0021 smooth snap: with an advancing clock the offset *glides* to the
/// page over several frames (a spring), rather than hard-jumping, some frame
/// must land strictly between the release offset and the page boundary.
#[test]
fn page_snap_glides_smoothly_when_a_clock_is_advancing() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(page_snap_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.set_now_ms(0);
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    // Wheel two-thirds of a page toward page 1, then let it settle: past the
    // quiet threshold it springs from 70 → 100.
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-70.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    let mut saw_intermediate = false;
    for step in 1..=60 {
        interp.set_now_ms(step * 16);
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let v = peek_f(&interp);
        if v > 70.5 && v < 99.5 {
            saw_intermediate = true; // mid-glide, neither the start nor the page
        }
    }
    assert!(
        saw_intermediate,
        "the offset must glide through intermediate positions, not hard-jump"
    );
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "the glide settles exactly on page 1 (100), got {}",
        peek_f(&interp)
    );
}

/// RFC-0021: a stream of shrinking scroll deltas (trackpad momentum) must not
/// trigger a snap while the fling is still delivering events, the offset
/// tracks the input and only snaps once the scroll goes quiet, so the snap and
/// the scroll never fight.
#[test]
fn momentum_scroll_does_not_snap_until_it_goes_quiet() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(page_snap_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    let wheel = |dx: f32| byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (dx, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };
    // Five momentum frames (input every frame) accumulate to offset 75 without
    // ever snapping, each event restarts the quiet countdown.
    let mut acc = 0.0;
    for _ in 0..5 {
        interp.dispatch_events(&[wheel(-15.0)]);
        interp.render(&tree, &mut frame, 400.0, 300.0);
        acc += 15.0;
        assert!(
            (peek_f(&interp) - acc).abs() < 1.0,
            "offset must track momentum ({acc}), never snap mid-fling; got {}",
            peek_f(&interp)
        );
    }
    // Fling over: a few quiet renders and it snaps to the nearest page (75 → 100).
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "once quiet, it snaps to page 1 (100), got {}",
        peek_f(&interp)
    );
}

/// RFC-0021 `snap: item`: with unequal-width items the offset settles to the
/// nearest *child boundary* (not a fixed page), so a carousel of varied cards
/// snaps each card to the viewport edge.
#[test]
fn item_snap_settles_to_the_nearest_child_boundary() {
    use byard_core::platform::EventKind as K;
    // Three items of widths 80/140/100 in a 120px viewport. Boundaries (item
    // starts) are 0, 80, 220; content 320, max 200. A wheel to offX 60 is
    // nearer boundary 80 than 0 → snaps to 80.
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: item, offset: (offX, offY), \
                          width: 120, height: 100] { \
                 Row { Box #[width: 80, height: 100] {} \
                       Box #[width: 140, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-60.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 80.0).abs() < 1.0,
        "snaps to the item-2 boundary (80), got {}",
        peek_f(&interp)
    );
}

/// RFC-0021 fling projection: a fast flick that stops short of the midpoint
/// still advances one page in the fling direction (the velocity carries it),
/// whereas the same offset with no velocity snaps back to the nearest page.
#[test]
fn fast_fling_projects_one_page_forward() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, offset: (offX, offY), \
                          width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    let wheel = |dx: f32, t: u64| byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (dx, 0.0),
        payload: None,
        time_ms: t,
        mods: byard_core::KeyModifiers::default(),
    };
    // Two quick flicks 20px apart in 40ms → ~500 px/s, well past 150 dp/s, but
    // the offset only reaches 40, short of the page-0→1 midpoint (50).
    interp.dispatch_events(&[wheel(-20.0, 0)]);
    interp.dispatch_events(&[wheel(-20.0, 40)]);
    assert!(
        (peek_f(&interp) - 40.0).abs() < 1.0,
        "offset reached 40 (short of the midpoint), got {}",
        peek_f(&interp)
    );
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 100.0).abs() < 1.0,
        "the fling projects forward to page 1 (100), got {}",
        peek_f(&interp)
    );
}

/// Finds the first `collapse_header` `ScrollView`'s `scroll_fraction` signal
/// (carried on `bound_sig`) in a lowered tree.
fn find_collapse_sig(nodes: &[RenderNode]) -> Option<SignalId> {
    for n in nodes {
        if let RenderNode::Box {
            name,
            bound_sig,
            children,
            ..
        } = n
        {
            if name.as_str() == "ScrollView" {
                if let Some(sig) = bound_sig {
                    return Some(*sig);
                }
            }
            if let Some(s) = find_collapse_sig(children) {
                return Some(s);
            }
        }
    }
    None
}

/// RFC-0021 collapsing header: `scroll_fraction` runs 0 → 1 over the header's
/// collapsible range (natural height − `collapse_min`) and clamps past it.
#[test]
fn collapse_header_drives_scroll_fraction() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             ScrollView #[collapse_header: true, collapse_min: 40, offset: (sx, sy), \
                          width: 200, height: 150] { \
                 Box #[bg: 0xAABBCC, width: 200] { Box #[width: 200, height: 100] {} } \
                 Column { Box #[bg: 0x112233, width: 200, height: 120] {} \
                          Box #[bg: 0x223344, width: 200, height: 120] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let frac = find_collapse_sig(&tree).expect("scroll_fraction signal exists");
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let mut frame = byard_core::frame::RenderFrame::new();
    let read = |i: &Interpreter| match i.peek(frac) {
        Value::Float(f) => f as f32,
        _ => -1.0,
    };
    // Range is 100 − 40 = 60px.
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        read(&interp).abs() < 1e-3,
        "expanded → 0, got {}",
        read(&interp)
    );
    interp.write_var(sy, Value::Float(30.0));
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (read(&interp) - 0.5).abs() < 1e-2,
        "halfway → 0.5, got {}",
        read(&interp)
    );
    interp.write_var(sy, Value::Float(90.0));
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (read(&interp) - 1.0).abs() < 1e-3,
        "past range → clamped 1, got {}",
        read(&interp)
    );
}

/// RFC-0021 collapsing header: the header (first child) stays pinned to the
/// viewport top as the content scrolls under it.
#[test]
fn collapse_header_pins_header_while_content_scrolls() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             ScrollView #[collapse_header: true, offset: (sx, sy), width: 200, height: 150] { \
                 Box #[bg: 0xAABBCC, width: 200, height: 80] {} \
                 Box #[bg: 0x112233, width: 200, height: 200] {} \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let header_rgba = crate::interp::intrinsics::color_to_rgba(0x00AA_BBCC, false);
    let content_rgba = crate::interp::intrinsics::color_to_rgba(0x0011_2233, false);
    // Scroll is applied as a paint-time translate, not baked into the layout
    // rect, so the painted y is `rect.y + transform.translate.y`.
    let y_of = |frame: &byard_core::frame::RenderFrame, rgba: [f32; 4]| -> Option<f32> {
        frame
            .instances()
            .iter()
            .find(|b| b.color == rgba)
            .map(|b| b.rect[1] + b.transform.translate[1])
    };
    let mut f0 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f0, 400.0, 300.0);
    let (h0, c0) = (y_of(&f0, header_rgba), y_of(&f0, content_rgba));
    // Scroll down 50px.
    interp.write_var(sy, Value::Float(50.0));
    let mut f1 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f1, 400.0, 300.0);
    let (h1, c1) = (y_of(&f1, header_rgba), y_of(&f1, content_rgba));
    let (h0, h1) = (h0.expect("header painted"), h1.expect("header painted"));
    let (c0, c1) = (c0.expect("content painted"), c1.expect("content painted"));
    assert!((h1 - h0).abs() < 0.5, "header stays pinned: {h0} → {h1}");
    assert!(
        (c1 - (c0 - 50.0)).abs() < 0.5,
        "content scrolls up by 50: {c0} → {c1}"
    );
    // The header must paint *on top* of the content that scrolls under it,
    // draw-order depth is emission order and `draw_depth` decreases with it, so
    // the (last-emitted) header's depth is strictly nearer than the content's.
    let depth_of = |frame: &byard_core::frame::RenderFrame, rgba: [f32; 4]| -> f32 {
        let i = frame
            .instances()
            .iter()
            .position(|b| b.color == rgba)
            .expect("painted");
        frame.solid_depths()[i]
    };
    assert!(
        depth_of(&f1, header_rgba) < depth_of(&f1, content_rgba),
        "the pinned header paints over the scrolling content"
    );
}

/// RFC-0021 collapsing header: the pin + `scroll_fraction` work with the
/// example's real shape, the ScrollView nested in an outer `Column`, and a
/// header that is a `Box` wrapping a `Column` of `Text`s (not a bare box).
#[test]
fn collapse_header_works_nested_like_the_example() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             Column #[bg: 0x14141C, width: 200] { \
                 ScrollView #[collapse_header: true, collapse_min: 30, offset: (sx, sy), \
                              width: 200, height: 120] { \
                     Box #[bg: 0xAABBCC, width: 200, p: 10] { \
                         Column { Text(\"Title\") Text(\"Sub\") } \
                     } \
                     Column #[p: 8] { \
                         Box #[bg: 0x112233, width: 180, height: 90] {} \
                         Box #[bg: 0x223344, width: 180, height: 90] {} \
                     } \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    // The `scroll_fraction` signal must be minted even when nested.
    let frac = find_collapse_sig(&tree).expect("nested collapse header is detected");
    interp.tick();
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let header_rgba = crate::interp::intrinsics::color_to_rgba(0x00AA_BBCC, false);
    let y_of = |frame: &byard_core::frame::RenderFrame| -> Option<f32> {
        frame
            .instances()
            .iter()
            .find(|b| b.color == header_rgba)
            .map(|b| b.rect[1] + b.transform.translate[1])
    };
    let mut f0 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f0, 400.0, 300.0);
    let h0 = y_of(&f0).expect("header painted");
    interp.write_var(sy, Value::Float(40.0));
    let mut f1 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f1, 400.0, 300.0);
    let h1 = y_of(&f1).expect("header painted");
    assert!(
        (h1 - h0).abs() < 0.5,
        "the header stays pinned when nested: {h0} → {h1}"
    );
    assert!(
        matches!(interp.peek(frac), Value::Float(f) if f > 0.5),
        "scroll_fraction advanced past 0.5 after scrolling 40px, got {:?}",
        interp.peek(frac)
    );
}

/// RFC-0021 collapsing header: a header child's `opacity: 1.0 -
/// scroll_fraction` actually fades its text as the header collapses.
#[test]
fn collapse_header_fades_child_via_scroll_fraction() {
    let src = "View V() { var sx = 0.0 var sy = 0.0 \
             ScrollView #[collapse_header: true, collapse_min: 20, offset: (sx, sy), \
                          width: 200, height: 100] { \
                 Box #[bg: 0xAABBCC, width: 200, p: 8] { \
                     Box #[opacity: 1.0 - scroll_fraction] { Text(\"Sub\") #[color: 0xC7BFDE] } \
                 } \
                 Column #[p: 8] { Box #[bg: 0x112233, width: 180, height: 120] {} \
                                  Box #[bg: 0x223344, width: 180, height: 120] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let sy = interp.var_signal(&Symbol::intern("sy")).unwrap();
    let sub_rgb = crate::interp::intrinsics::color_to_rgba(0x00C7_BFDE, false);
    // The alpha the subtitle reaches the screen with: its own, times the
    // opacity of any group it is faded inside (RFC-0011 T4). A fading box
    // with children is one picture now, so the fade lives on the group.
    let sub_alpha = |frame: &byard_core::frame::RenderFrame| -> Option<f32> {
        frame
            .texts()
            .iter()
            .enumerate()
            .find(|(_, t)| t.color[..3] == sub_rgb[..3])
            .map(|(i, t)| t.color[3] * frame.composite_opacity(|m| m.text, i))
    };
    let mut f0 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f0, 400.0, 300.0);
    let a0 = sub_alpha(&f0).expect("subtitle painted");
    interp.write_var(sy, Value::Float(80.0));
    let mut f1 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f1, 400.0, 300.0);
    let a1 = sub_alpha(&f1).expect("subtitle painted");
    assert!(a0 > 0.9, "expanded subtitle is opaque, got {a0}");
    assert!(a1 < 0.1, "collapsed subtitle has faded out, got {a1}");
}

/// RFC-0021 `snap_align: center`: the snapped item is centred in the viewport,
/// so its rest offset is its start minus half the slack `(viewport − item)`.
#[test]
fn item_snap_align_center_centres_the_item() {
    use byard_core::platform::EventKind as K;
    // Uniform 100px items in a 140px viewport. Centring item 1 (start 100)
    // rests at 100 − (140 − 100)/2 = 80. A wheel near there settles to 80.
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: item, snap_align: center, \
                          offset: (offX, offY), width: 140, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    let peek_f = |i: &Interpreter| match i.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-75.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    assert!(
        (peek_f(&interp) - 80.0).abs() < 1.0,
        "centres item 1 at offset 80, got {}",
        peek_f(&interp)
    );
}

/// RFC-0021 `snap_spring`: a custom spring override checks clean and still
/// snaps to the page (the override changes the glide feel, not the target).
#[test]
fn snap_spring_override_checks_and_snaps() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, \
                          snap_spring: anim.spring(stiffness: 320, damping: 18), \
                          offset: (offX, offY), width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} \
                       Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(
        interp.errors().is_empty(),
        "snap_spring resolves cleanly: {:?}",
        interp.errors()
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let off = interp.var_signal(&Symbol::intern("offX")).unwrap();
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: K::Scroll,
        pos: (50.0, 50.0),
        delta: (-70.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    for _ in 0..8 {
        interp.render(&tree, &mut frame, 400.0, 300.0);
    }
    let got = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => 0.0,
    };
    assert!(
        (got - 100.0).abs() < 1.0,
        "still snaps to page 1 with the custom spring, got {got}"
    );
}

/// RFC-0021 `snap_spring`: a malformed curve is reported (and falls back to the
/// default), not silently accepted.
#[test]
fn snap_spring_malformed_is_reported() {
    let src = "View V() { var offX = 0.0 var offY = 0.0 \
             ScrollView #[axis: horizontal, snap: page, snap_spring: anim.wobble, \
                          offset: (offX, offY), width: 100, height: 100] { \
                 Row { Box #[width: 100, height: 100] {} } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    // `snap_spring` is resolved during render (scroll-target build), so drive a
    // frame to surface the diagnostic.
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        !interp.errors().is_empty(),
        "an unknown curve name is diagnosed"
    );
}

fn pull_refresh_view() -> &'static str {
    "View V() { var refreshing = false var refreshed = false \
             ScrollView #[pull_refresh: true, refreshing: refreshing, \
                          refresh => refreshed = true, width: 200, height: 120] { \
                 Column { Box #[width: 180, height: 50] {} \
                          Box #[width: 180, height: 50] {} } \
             } }"
}

/// RFC-0021 pull-to-refresh: a downward over-drag past the threshold fires
/// `refresh`, sets the reflected `refreshing` var, and rests the indicator;
/// the app clearing `refreshing` retracts it.
#[test]
fn pull_past_threshold_fires_refresh_and_retracts_when_cleared() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(pull_refresh_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let refreshing = interp.var_signal(&Symbol::intern("refreshing")).unwrap();
    let refreshed = interp.var_signal(&Symbol::intern("refreshed")).unwrap();
    let ev = |kind, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (100.0, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };
    // Press at the top, drag down 130px (past the elastic threshold), release.
    interp.dispatch_events(&[ev(K::PointerDown, 12.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 142.0)]);
    interp.dispatch_events(&[ev(K::PointerUp, 142.0)]);
    assert_eq!(
        interp.peek(refreshed),
        Value::Bool(true),
        "release past the threshold fires `refresh`"
    );
    assert_eq!(
        interp.peek(refreshing),
        Value::Bool(true),
        "the engine sets `refreshing` while it loads"
    );
    // The controller finishes: clearing `refreshing` retracts the indicator.
    interp.write_var(refreshing, Value::Bool(false));
    interp.render(&tree, &mut frame, 400.0, 300.0);
    // (no panic / clean retract, with no clock the spring resolves instantly)
}

/// RFC-0021 pull-to-refresh: a short pull that never reaches the threshold
/// retracts on release without firing `refresh`.
#[test]
fn short_pull_retracts_without_refreshing() {
    use byard_core::platform::EventKind as K;
    let parsed = parse(pull_refresh_view());
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let refreshing = interp.var_signal(&Symbol::intern("refreshing")).unwrap();
    let refreshed = interp.var_signal(&Symbol::intern("refreshed")).unwrap();
    let ev = |kind, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (100.0, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };
    // Only a 20px pull, well under the threshold.
    interp.dispatch_events(&[ev(K::PointerDown, 12.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 32.0)]);
    interp.dispatch_events(&[ev(K::PointerUp, 32.0)]);
    assert_eq!(
        interp.peek(refreshed),
        Value::Bool(false),
        "a short pull does not fire `refresh`"
    );
    assert_eq!(interp.peek(refreshing), Value::Bool(false));
}

/// RFC-0005: a press that lands on an interactive child (here a `Button`)
/// is that child's gesture, drag-to-scroll defers and the list stays put.
#[test]
fn drag_defers_to_interactive_children() {
    use byard_core::platform::EventKind as K;
    let src = "View V() { var off: Float = 0.0 var c: Int = 0 \
             ScrollView #[width: 200, height: 100, offset: (0, off)] { \
                 Column { \
                     Button(\"tap\") #[width: 180, height: 60] => c++ \
                     Box #[bg: 0x00FF00, width: 180, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 180, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };

    // Press on the Button (top 60px), then drag: the button owns the press,
    // so the list must not scroll.
    interp.dispatch_events(&[ev(K::PointerDown, 100.0, 30.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 100.0, -60.0)]);
    let scrolled = match interp.peek(off) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    assert!(
        scrolled.abs() < 0.01,
        "a press on a Button must not drag-scroll the list, got {scrolled}"
    );
}

/// RFC-0005 `axis: horizontal`: content overflows on the inline axis and the
/// wheel's x delta scrolls `offset.x`, clamped to the horizontal extent.
#[test]
fn horizontal_scrollview_scrolls_offset_x_by_wheel() {
    // A Row of 4 × 100px cards = 400 wide in a 200px viewport → max x 200.
    let src = "View V() { var panX: Float = 0.0 \
             ScrollView #[width: 200, height: 80, axis: horizontal, offset: (panX, 0)] { \
                 Row { \
                     Box #[bg: 0xFF0000, width: 100, height: 60] {} \
                     Box #[bg: 0x00FF00, width: 100, height: 60] {} \
                     Box #[bg: 0x0000FF, width: 100, height: 60] {} \
                     Box #[bg: 0xFFFF00, width: 100, height: 60] {} \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let pan = interp.var_signal(&Symbol::intern("panX")).unwrap();
    let peek = |interp: &Interpreter| match interp.peek(pan) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    // Sample the red card's paint translate.x, so we prove the content
    // actually shifts left (−offset), not just that the signal moved.
    let red_tx = |interp: &mut Interpreter| -> f32 {
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 400.0, 300.0);
        f.instances()
            .iter()
            .find(|b| b.color[0] > 0.8 && b.color[1] < 0.3 && b.color[2] < 0.3)
            .map_or(0.0, |b| b.transform.translate[0])
    };
    let tx0 = red_tx(&mut interp);

    let wheel_x = |interp: &mut Interpreter, dx: f32| {
        interp.dispatch_events(&[byard_core::platform::InputEvent {
            kind: byard_core::platform::EventKind::Wheel,
            pos: (100.0, 40.0),
            delta: (dx, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        }]);
        interp.tick();
    };

    // Wheel 2 lines right (×40) → scroll right 80; red card shifts left 80.
    wheel_x(&mut interp, -2.0);
    assert!(
        (peek(&interp) - 80.0).abs() < 1.0,
        "x wheel scrolls, got {}",
        peek(&interp)
    );
    assert!(
        (tx0 - red_tx(&mut interp) - 80.0).abs() < 0.5,
        "content shifts left by the x offset"
    );

    // A big wheel clamps to content−viewport (400 − 200 = 200).
    wheel_x(&mut interp, -20.0);
    assert!(
        (peek(&interp) - 200.0).abs() < 1.0,
        "x clamps to extent, got {}",
        peek(&interp)
    );
}

/// RFC-0005 `axis: both`: a single drag pans the content in 2D, each axis
/// clamped independently.
#[test]
fn both_axis_scrollview_pans_in_two_dimensions_by_drag() {
    use byard_core::platform::EventKind as K;
    // A 400×400 content grid in a 200×200 viewport → max 200 on each axis.
    let src = "View V() { var panX: Float = 0.0 var panY: Float = 0.0 \
             ScrollView #[width: 200, height: 200, axis: both, offset: (panX, panY)] { \
                 Column { \
                     Row { Box #[bg: 0xFF0000, width: 200, height: 200] {} \
                           Box #[bg: 0x00FF00, width: 200, height: 200] {} } \
                     Row { Box #[bg: 0x0000FF, width: 200, height: 200] {} \
                           Box #[bg: 0xFFFF00, width: 200, height: 200] {} } \
                 } \
             } }";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let px = interp.var_signal(&Symbol::intern("panX")).unwrap();
    let py = interp.var_signal(&Symbol::intern("panY")).unwrap();
    let peek = |interp: &Interpreter, s| match interp.peek(s) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    let ev = |kind, x: f32, y: f32| byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    };

    // Press mid-viewport, drag up-left 60px each → pan right 60, down 60.
    interp.dispatch_events(&[ev(K::PointerDown, 100.0, 100.0)]);
    interp.dispatch_events(&[ev(K::PointerMove, 40.0, 40.0)]);
    assert!(
        (peek(&interp, px) - 60.0).abs() < 1.0,
        "panX, got {}",
        peek(&interp, px)
    );
    assert!(
        (peek(&interp, py) - 60.0).abs() < 1.0,
        "panY, got {}",
        peek(&interp, py)
    );
}

/// RFC-0005 windowed layout: a `windowed` ScrollView lays out only the
/// visible slice of a long uniform list, O(visible), not O(list), while a
/// plain ScrollView over the same list lays out every row.
#[test]
fn windowed_scrollview_lays_out_only_the_visible_window() {
    // 1000 rows × 20px in a 100px viewport. A windowed pass should build only
    // a handful of rows (viewport/row + overscan) + 2 spacers + containers.
    let list = |windowed: &str| {
        format!(
            "View V() {{ var y: Float = 0.0 \
                 ScrollView #[width: 200, height: 100, row_height: 20, {windowed} offset: (0, y)] {{ \
                     Column {{ \
                         for i in [{}] {{ \
                             Box #[bg: 0x6495ED, width: 180, height: 20] {{}} \
                         }} \
                     }} \
                 }} }}",
            (0..1000)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let node_count = |src: String| {
        let parsed = parse(&src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let tree = interp.lower_view(&parsed.views[0], &[]);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        (interp.atlas_node_count(), frame.instances().len())
    };
    let (windowed_nodes, windowed_boxes) = node_count(list("windowed: true,"));
    let (plain_nodes, _) = node_count(list(""));

    assert!(
        windowed_nodes < 40,
        "a windowed 1000-row list lays out O(visible), got {windowed_nodes} nodes"
    );
    assert!(
        plain_nodes > 1000,
        "a plain list lays out every row, got {plain_nodes} nodes"
    );
    assert!(
        windowed_boxes < 30,
        "only the visible rows are emitted, got {windowed_boxes}"
    );
}

/// RFC-0005 windowed layout: the two spacer leaves preserve the full content
/// extent, so the scroll clamp still reaches the true bottom of the list.
#[test]
fn windowed_scrollview_preserves_scroll_extent() {
    // 500 rows × 20 = 10 000 tall in a 100px viewport → max scroll 9 900.
    let src = format!(
        "View V() {{ var y: Float = 0.0 \
             ScrollView #[width: 200, height: 100, row_height: 20, windowed: true, offset: (0, y)] {{ \
                 Column {{ for i in [{}] {{ Box #[bg: 0x6495ED, width: 180, height: 20] {{}} }} }} \
             }} }}",
        (0..500)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let y = interp.var_signal(&Symbol::intern("y")).unwrap();
    // A huge wheel must clamp to content − viewport = 9 900, proving the
    // elided rows still count toward the extent (spacers, not shrinkage).
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::Wheel,
        pos: (100.0, 50.0),
        delta: (0.0, -10_000.0),
        payload: None,
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }]);
    let clamped = match interp.peek(y) {
        Value::Float(f) => f as f32,
        Value::Int(n) => n as f32,
        _ => panic!("numeric"),
    };
    assert!(
        (clamped - 9_900.0).abs() < 1.0,
        "windowed extent must span the whole list, clamped at {clamped}"
    );
}

/// RFC-0005 windowed layout: as the offset grows the window slides, so a row
/// deep in the list becomes visible while the atlas stays O(visible).
#[test]
fn windowed_scrollview_slides_the_window_on_scroll() {
    let src = format!(
        "View V() {{ var y: Float = 0.0 \
             ScrollView #[width: 200, height: 100, row_height: 20, windowed: true, offset: (0, y)] {{ \
                 Column {{ for i in [{}] {{ Box #[bg: 0x6495ED, width: 180, height: 20] {{}} }} }} \
             }} }}",
        (0..500)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    // The emitted rows' *layout* Y band (their true list positions, not the
    // scrolled screen position), and the atlas size, at the current offset.
    let sample = |interp: &mut Interpreter| -> (f32, f32, usize) {
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let ys: Vec<f32> = frame.instances().iter().map(|b| b.rect[1]).collect();
        let min = ys.iter().copied().fold(f32::INFINITY, f32::min);
        let max = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (min, max, interp.atlas_node_count())
    };
    let (_, at_rest_bottom, nodes0) = sample(&mut interp);
    assert!(
        at_rest_bottom < 300.0,
        "at rest the window sits at the top of the list, got bottom {at_rest_bottom}"
    );

    // Jump near the bottom (offset 8000 → row ~400 of 500). The window must
    // slide to the deep rows: they lay out at y ≈ 8000 (row_index × 20).
    let y = interp.var_signal(&Symbol::intern("y")).unwrap();
    interp.write_var(y, Value::Float(8000.0));
    interp.tick();
    let (deep_top, _, nodes1) = sample(&mut interp);
    assert!(
        deep_top > 7000.0,
        "the window slid to the deep rows (laid out near y≈8000), got top {deep_top}"
    );
    assert!(nodes0 < 40, "the window is O(visible) at rest: {nodes0}");
    assert!(
        nodes1 < 40,
        "the window stays O(visible) after scrolling deep: {nodes1}"
    );
}

/// RFC-0005 windowed layout regression: with uniform rows whose stride equals
/// `row_height`, the materialised rows must stay on an exact `row_height` grid
/// at every offset, including across a window-slide boundary. A spacer sized
/// off-grid would shift the whole content when `start` ticks (the "small
/// jumps" bug), so this pins the invariant that a scroll of 1px moves the
/// content by exactly 1px, never a row.
#[test]
fn windowed_rows_stay_on_an_exact_grid_across_slides() {
    // 500 rows laid out at exactly row_height (height 20, no gap → stride 20).
    let src = format!(
        "View V() {{ var y: Float = 0.0 \
             ScrollView #[width: 200, height: 100, windowed: true, row_height: 20, offset: (0, y)] {{ \
                 Column {{ for i in [{}] {{ Box #[bg: 0x6495ED, width: 180, height: 20] {{}} }} }} \
             }} }}",
        (0..500)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let y = interp.var_signal(&Symbol::intern("y")).unwrap();

    // Sweep offsets straddling several window-slide boundaries (start ticks
    // every 20px). At each, the emitted rows must be exactly 20px apart.
    for off in [0.0, 19.0, 20.0, 21.0, 79.0, 80.0, 81.0, 200.0, 205.0] {
        interp.write_var(y, Value::Float(off));
        interp.tick();
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let mut ys: Vec<f32> = frame.instances().iter().map(|b| b.rect[1]).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in ys.windows(2) {
            let stride = w[1] - w[0];
            assert!(
                (stride - 20.0).abs() < 0.01,
                "rows must stay on the 20px grid at offset {off}, got stride {stride} in {ys:?}"
            );
        }
    }
}

#[test]
fn oklab_hex_round_trips_within_one_lsb() {
    for hex in [
        0x00_0000_i64,
        0xFF_FFFF,
        0x64_95ED,
        0xEF_4444,
        0x10_B981,
        0x80_8080,
    ] {
        let back = hex_from_oklab(oklab_from_hex(hex));
        for shift in [16, 8, 0] {
            let a = (hex >> shift) & 0xFF;
            let b = (back >> shift) & 0xFF;
            assert!(
                (a - b).abs() <= 1,
                "channel drift for {hex:#08x}: {a} vs {b}"
            );
        }
    }
}

// ── RFC-0005 scroll × hit-testing ───────────────────────────────────

#[test]
fn hit_targets_follow_the_scroll_offset() {
    use byard_core::platform::EventKind as K;
    // A tappable card that starts at y 80..160 inside a 100-high
    // viewport. Scrolling down 60 shows it at y 20..100 on screen.
    let src = "View V() { var n = 0 var off = 0
            ScrollView #[width: 200, height: 100, offset: (0, off)] {
                Column {
                    Box #[bg: 0x111111, width: 200, height: 80] {}
                    Box #[bg: 0x222222, width: 200, height: 80, tap => n = n + 1] {}
                }
            }
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    let n = interp.var_signal(&Symbol::intern("n")).unwrap();
    let taps = |interp: &Interpreter| match interp.peek(n) {
        Value::Int(v) => v,
        other => panic!("n must be an Int, got {other:?}"),
    };
    let tap_at = |interp: &mut Interpreter, x: f32, y: f32, t: u64| {
        interp.dispatch_events(&[pointer(K::PointerDown, x, y, t)]);
        interp.dispatch_events(&[pointer(K::PointerUp, x, y, t + 20)]);
    };

    // Scroll down 60 and re-render (handlers re-register shifted).
    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    interp.write_var(off, Value::Int(60));
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Tapping where the card now IS on screen fires its action…
    tap_at(&mut interp, 100.0, 30.0, 100);
    assert_eq!(taps(&interp), 1, "the on-screen position must be tappable");

    // …and tapping where layout *placed* it (y 110, scrolled away and
    // outside the viewport) must not, the hit rect moved with the
    // content and is clipped to the scroll viewport.
    tap_at(&mut interp, 100.0, 110.0, 200);
    assert_eq!(taps(&interp), 1, "the stale laid-out position is inert");
}
