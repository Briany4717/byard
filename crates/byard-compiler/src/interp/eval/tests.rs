use super::animation::hex_from_oklab;
use super::animation::oklab_from_hex;
use super::expr::eval_binary;
use super::expr::eval_compare;
use super::expr::eval_concat;
use super::expr::index_value;
use super::expr::structural_eq;
use super::instance::MAX_INSTANCE_DEPTH;
use super::states::resolve_state_attrs;
use super::*;
use crate::parser::ast::AttrKind;
use crate::parser::ast::BinOp;
use crate::parser::ast::ElementNode;
use crate::parser::ast::StyleStateKind;
use crate::parser::parse;
use animation::ramp_paint_prop;

mod animation;
mod canvas;
mod instance;
mod lowering;
mod props;

fn element(m: &Member) -> &ElementNode {
    match m {
        Member::Element(e) => e,
        _ => panic!("expected element"),
    }
}

// ── user-view registry & call-site recognition ──────────────────

/// Loads a multi-view file and lowers the named view to a render tree.
/// The linear value of an sRGB byte, for tests that identify a primitive by the
/// colour it was written with.
///
/// A colour literal is written encoded (`0x5B8DEF` is what the design tool
/// says) and reaches the frame decoded, because everything past the compiler
/// blends in linear light. A test comparing against `byte / 255` is comparing
/// the two spaces, which is exactly the mistake the double-encoding hid.
fn linear(byte: u8) -> f32 {
    crate::interp::intrinsics::srgb_to_linear(f32::from(byte) / 255.0)
}

fn lower_named(src: &str, name: &str) -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let view = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == name)
        .unwrap();
    let tree = interp.lower_view(view, &known);
    (interp, tree)
}

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
fn checkbox_frame(src: &str) -> byard_core::frame::RenderFrame {
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

// ── RFC-0018: Grid ────────────────────────────────────────────────────

#[test]
fn grid_auto_places_children_into_columns() {
    // Two 1fr columns in a 200px-wide grid → 100px each; two auto-flowed
    // children land one per column (x≈0 and x≈100).
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr 1fr\", width: 200, height: 100] {\n \
               Box #[bg: 0xFF0000, height: 40] {}\n \
               Box #[bg: 0x00FF00, height: 40] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2, "two child fills");
    let mut xs: Vec<f32> = frame.instances().iter().map(|i| i.rect[0]).collect();
    xs.sort_by(f32::total_cmp);
    assert!(xs[0] < 5.0, "first column near x=0, got {xs:?}");
    assert!(
        (xs[1] - 100.0).abs() < 5.0,
        "second column near x=100, got {xs:?}"
    );
}

#[test]
fn grid_explicit_col_placement_pins_a_child() {
    // A lone child pinned to column 2 sits in the right column (x≈100),
    // proving `set_grid_item` wires `col:` through to Taffy.
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr 1fr\", width: 200, height: 100] {\n \
               Box #[bg: 0xFF0000, height: 40, col: 2] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 1, "one child fill");
    assert!(
        (frame.instances()[0].rect[0] - 100.0).abs() < 5.0,
        "pinned to column 2 (x≈100), got {:?}",
        frame.instances()[0].rect
    );
}

#[test]
fn grid_row_gap_offsets_the_second_row() {
    // One 1fr column, two explicit 40px rows, row_gap 20 → the second row
    // starts at y = 40 + 20 = 60. (Explicit row tracks so the assertion is
    // independent of grid `align-content`, which stretches *auto* rows to
    // fill a fixed-height container.)
    let frame = checkbox_frame(
        "View C() {\n Grid #[columns: \"1fr\", rows: \"40 40\", row_gap: 20, width: 100] {\n \
               Box #[bg: 0xFF0000] {}\n \
               Box #[bg: 0x00FF00] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2);
    let mut ys: Vec<f32> = frame.instances().iter().map(|i| i.rect[1]).collect();
    ys.sort_by(f32::total_cmp);
    assert!(ys[0] < 5.0, "first row at top, got {ys:?}");
    assert!(
        (ys[1] - 60.0).abs() < 5.0,
        "second row after 40px row + 20px gap, got {ys:?}"
    );
}

#[test]
fn grid_invalid_template_reports_a_diagnostic_and_still_renders() {
    let parsed = parse("View C() {\n Grid #[columns: \"1fr bogus\"] { Box #[bg: 0xFF0000] {} }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::InvalidGridTemplate { .. })),
        "expected InvalidGridTemplate, got {:?}",
        interp.errors()
    );
    // Non-fatal: the grid still lays out (falls back to a single auto column).
    assert_eq!(frame.instances().len(), 1, "the child still renders");
}

// ── RFC-0018: ZStack ──────────────────────────────────────────────────

#[test]
fn zstack_overlaps_children_at_the_same_origin() {
    // Two children with bg: the small one centres over the big one (they
    // overlap), unlike a Column which would stack them vertically.
    let frame = checkbox_frame(
        "View C() {\n ZStack #[width: 100, height: 100] {\n \
               Box #[bg: 0xFF0000, width: 100, height: 100] {}\n \
               Box #[bg: 0x00FF00, width: 40, height: 40] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2, "two child fills");
    // Declaration order: big first (bottom), small second (on top).
    let big = frame.instances()[0].rect;
    let small = frame.instances()[1].rect;
    assert!(
        big[0] < 5.0 && big[1] < 5.0,
        "big child at origin, got {big:?}"
    );
    // Small (40) centred in the 100 stack → (100 − 40) / 2 = 30.
    assert!(
        (small[0] - 30.0).abs() < 5.0 && (small[1] - 30.0).abs() < 5.0,
        "small child centred, got {small:?}"
    );
}

#[test]
fn zstack_alignment_pins_child_to_corner() {
    // `bottom_end` puts the small child at the bottom-right corner.
    let frame = checkbox_frame(
        "View C() {\n ZStack #[width: 100, height: 100, alignment: bottom_end] {\n \
               Box #[bg: 0xFF0000, width: 100, height: 100] {}\n \
               Box #[bg: 0x00FF00, width: 20, height: 20] {}\n \
             }\n}",
    );
    assert_eq!(frame.instances().len(), 2);
    let small = frame.instances()[1].rect;
    assert!(
        (small[0] - 80.0).abs() < 5.0 && (small[1] - 80.0).abs() < 5.0,
        "small child at bottom-right (80,80), got {small:?}"
    );
}

// ── RFC-0005: default text wrap ───────────────────────────────────────

#[test]
fn text_wraps_to_parent_width_by_default() {
    // A long line in a narrow fixed-width column wraps with NO explicit
    // `wrap`/`width`, default wrap reflows it to the column's width.
    let parsed = parse(
        "View C() {\n Column #[width: 120] {\n \
               Text(\"This is a fairly long sentence that must wrap within a narrow column.\")\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 1, "one text run");
    let wrap = frame.text_wraps()[0];
    assert!(wrap.is_some(), "default wrap is on");
    assert!(
        wrap.unwrap() <= 130.0,
        "wrapped to ~the 120px column (not the natural width), got {wrap:?}"
    );
}

#[test]
fn wrap_false_opts_out_to_a_single_line() {
    let parsed = parse(
        "View C() {\n Column #[width: 120] {\n \
               Text(\"A long unwrapped line that overflows the column.\") #[wrap: false]\n \
             }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 1);
    assert!(
        frame.text_wraps()[0].is_none(),
        "wrap: false → single line (no wrap width), got {:?}",
        frame.text_wraps()[0]
    );
}

#[test]
fn slider_drag_sets_float_value() {
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

    // PointerDown at ~50% of track (x=50 on a 100px track starting at x=0).
    interp.dispatch_events(&[byard_core::platform::InputEvent {
        kind: byard_core::platform::EventKind::PointerDown,
        pos: (50.0, 5.0),
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
    assert!(
        (val - 0.5).abs() < 0.1,
        "slider at 50% should be ~0.5, got {val}"
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

// ── Structural for/when in render tree ──────────────────────────

#[test]
fn when_true_includes_then_branch() {
    // RFC-0018: `when` lowers to one reactive `When` node; its taken branch is
    // expanded at paint time. With the condition true, the branch paints.
    let parsed = parse("View C() {\n var show = true\n when show { Text(\"visible\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(tree.len(), 1);
    assert!(
        matches!(tree[0], RenderNode::When { .. }),
        "when → When node"
    );
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts().len(), 1, "then-branch paints when true");
    assert_eq!(frame.texts()[0].text, "visible");
}

#[test]
fn when_false_emits_nothing_without_else() {
    let parsed = parse("View C() {\n var hide = false\n when hide { Text(\"hidden\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(frame.texts().is_empty(), "false, no else → nothing paints");
}

#[test]
fn when_reacts_to_a_var_flip_at_runtime() {
    // RFC-0018: the whole point, flipping the guard `var` mounts/unmounts the
    // subtree at runtime, with no re-lowering.
    let parsed = parse(
        "View C() {\n var show = false\n when show { Text(\"hi\") } else { Text(\"bye\") }\n}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let show = interp
        .var_signal(&crate::symbol::Symbol::intern("show"))
        .unwrap();
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(frame.texts()[0].text, "bye", "else branch first");

    interp.write_var(show, Value::Bool(true));
    interp.tick();
    let mut frame2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame2, 400.0, 300.0);
    assert_eq!(frame2.texts()[0].text, "hi", "then branch after flip");
}

#[test]
fn for_loop_emits_one_node_per_item() {
    // RFC-0018: `for` lowers to one reactive `For` node; the driver renders one
    // pooled body per element at paint time.
    let parsed =
        parse("View C() {\n var items = [1, 2, 3]\n for item in items { Text(\"{item}\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!(matches!(tree[0], RenderNode::For { .. }), "for → For node");
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let texts: Vec<&str> = frame.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["1", "2", "3"], "one node per item, in order");
}

#[test]
fn for_reacts_to_list_growth_and_element_change() {
    // RFC-0018: growing the list mounts more rows; changing an element updates
    // its row, all without re-lowering.
    let parsed = parse("View C() {\n var xs = [10, 20]\n for x in xs { Text(\"{x}\") }\n}");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    let xs = interp
        .var_signal(&crate::symbol::Symbol::intern("xs"))
        .unwrap();
    interp.tick();
    let mut f = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f, 400.0, 300.0);
    let t: Vec<&str> = f.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t, ["10", "20"]);

    // Grow + change: [10, 20] → [10, 99, 30].
    interp.write_var(
        xs,
        Value::List(vec![Value::Int(10), Value::Int(99), Value::Int(30)]),
    );
    interp.tick();
    let mut f2 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f2, 400.0, 300.0);
    let t2: Vec<&str> = f2.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t2, ["10", "99", "30"], "grew to 3 rows, element updated");

    // Shrink: [10, 99, 30] → [7].
    interp.write_var(xs, Value::List(vec![Value::Int(7)]));
    interp.tick();
    let mut f3 = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut f3, 400.0, 300.0);
    let t3: Vec<&str> = f3.texts().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(t3, ["7"], "shrank to 1 row");
}

// ── RFC-0005: default text wrap ──────────────────────────────────────

#[test]
fn explicit_width_pins_wrap_and_yields_a_taller_leaf() {
    let long = "the quick brown fox jumps over the lazy dog again and again";
    // Same text: the first wraps to the 400px root by default, the second is
    // pinned to width 120 (both wrap, wrap is default now).
    let src = format!("View C() {{\n Text(\"{long}\")\n Text(\"{long}\") #[width: 120]\n}}");
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    assert_eq!(frame.texts().len(), 2);
    assert_eq!(frame.text_wraps().len(), 2, "wrap slice parallel to texts");
    let w0 = frame.text_wraps()[0].expect("default wrap on the first line");
    let w1 = frame.text_wraps()[1].expect("explicit-width line still wraps");
    assert!(w0 > 200.0, "first wraps to the wide root, got {w0}");
    assert!(
        (w1 - 120.0).abs() < 1.0,
        "second wraps to its 120 width, got {w1}"
    );

    // The 120-wide leaf is narrower and at least as tall (more lines) as the
    // one that wrapped to the wide root.
    let wide = frame.rects()[1]; // C root is rects[0]; first Text is rects[1]
    let narrow = frame.rects()[2];
    assert!(
        (narrow.width - 120.0).abs() < 1.0,
        "the pinned leaf is 120 wide, got {}",
        narrow.width
    );
    assert!(
        narrow.height >= wide.height,
        "the narrower leaf wraps onto ≥ lines: {} vs {}",
        narrow.height,
        wide.height
    );
}

#[test]
fn reactive_demo_example_reacts_live() {
    // Ties the shipped RFC-0018 example to the suite: `when` toggles a subtree
    // and `for` grows a list, both at runtime.
    let src = include_str!("../../../examples/reactive_demo.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let show = interp
        .var_signal(&crate::symbol::Symbol::intern("showList"))
        .unwrap();
    let names = interp
        .var_signal(&crate::symbol::Symbol::intern("names"))
        .unwrap();
    let render = |interp: &mut Interpreter| {
        interp.tick();
        let mut f = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut f, 640.0, 520.0);
        f.texts().iter().map(|t| t.text.clone()).collect::<Vec<_>>()
    };

    // Initially: list shown → both names present.
    let t0 = render(&mut interp);
    assert!(t0.contains(&"Ada".to_string()) && t0.contains(&"Alan".to_string()));
    assert!(!t0.iter().any(|s| s.starts_with("List hidden")));

    // Hide the list live.
    interp.write_var(show, Value::Bool(false));
    let t1 = render(&mut interp);
    assert!(!t1.contains(&"Ada".to_string()), "list unmounted live");
    assert!(
        t1.iter().any(|s| s.starts_with("List hidden")),
        "else branch"
    );

    // Show again + grow the list live → four rows.
    interp.write_var(show, Value::Bool(true));
    interp.write_var(
        names,
        Value::List(vec![
            Value::Str("Ada".into()),
            Value::Str("Alan".into()),
            Value::Str("Grace".into()),
            Value::Str("Katherine".into()),
        ]),
    );
    let t2 = render(&mut interp);
    for n in ["Ada", "Alan", "Grace", "Katherine"] {
        assert!(t2.contains(&n.to_string()), "{n} row mounted after growth");
    }
}

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
    let src = include_str!("../../../examples/overlay_demo.byd");
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

// ── RFC-0023 ripple ink ─────────────────────────────────────────────

/// A 200×100 box at the layout origin with a semi-transparent white
/// ripple, triggered by `on pressed`, the RFC-0023 guide example shape.
const RIPPLE_SRC: &str = "View V() {
        let btn = style {
            bg: 0x6750A4, radius: 20, ripple: 0x80FFFFFF
            on pressed { ripple_active: true }
        }
        Box #[..btn, width: 200, height: 100] {}
    }";

fn pointer(
    kind: byard_core::platform::EventKind,
    x: f32,
    y: f32,
    t: u64,
) -> byard_core::platform::InputEvent {
    byard_core::platform::InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
        mods: byard_core::KeyModifiers::default(),
    }
}

/// Lowers `src`, renders once at `t = 0` (registering the hit region),
/// presses at `(x, y)` with press identity `press_t`, and renders once at
/// `t = 10`, the frame that spawns the ripple, so its `start_ms` is 10.
fn pressed_ripple_named(src: &str, x: f32, y: f32, press_t: u64) -> (Interpreter, Vec<RenderNode>) {
    use byard_core::platform::EventKind as K;
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[pointer(K::PointerDown, x, y, press_t)]);
    let spawn_frame = render_at(&mut interp, &tree, 10);
    assert_eq!(
        spawn_frame.ripples().len(),
        1,
        "the press spawns one ripple"
    );
    (interp, tree)
}

/// [`pressed_ripple_named`] over the canonical [`RIPPLE_SRC`] view.
fn pressed_ripple_interp(x: f32, y: f32, press_t: u64) -> (Interpreter, Vec<RenderNode>) {
    pressed_ripple_named(RIPPLE_SRC, x, y, press_t)
}

/// Renders `tree` at `ms` and returns the frame.
fn render_at(
    interp: &mut Interpreter,
    tree: &[RenderNode],
    ms: u32,
) -> byard_core::frame::RenderFrame {
    interp.set_now_ms(ms);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 300.0);
    frame
}

#[test]
fn a_press_spawns_a_ripple_at_the_tap_point() {
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    let frame = render_at(&mut interp, &tree, 50);

    assert_eq!(frame.ripples().len(), 1, "one press spawns one ripple");
    let r = frame.ripples()[0];
    // The circle is centred on the tap point, in absolute coordinates.
    assert!(
        (r.params[0] - 30.0).abs() < 0.01,
        "center x: {:?}",
        r.params
    );
    assert!(
        (r.params[1] - 40.0).abs() < 0.01,
        "center y: {:?}",
        r.params
    );
    // Mid-animation: expanding and still visible.
    assert!(r.params[2] > 0.0, "radius has started expanding");
    assert!(r.params[3] > 0.0 && r.params[3] < 1.0, "fade in flight");
    // The ink colour is the resolved `ripple:` colour (50% white).
    assert!((r.color[3] - 0.5).abs() < 0.01, "ink alpha: {:?}", r.color);
    // Clipped to the element's rounded rect: rect and radii carried over.
    assert_eq!(r.rect, [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(r.radii, [20.0; 4]);
    assert!(
        interp.has_active_animations(),
        "a live ripple keeps requesting frames"
    );
}

#[test]
fn ripple_expands_monotonically_and_retires_after_its_duration() {
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    let r1 = render_at(&mut interp, &tree, 50).ripples()[0].params[2];
    let f2 = render_at(&mut interp, &tree, 150);
    let r2 = f2.ripples()[0].params[2];
    assert!(r2 > r1, "radius grows with time ({r1} → {r2})");

    // Past the 300 ms default duration the ripple is gone, and with it the
    // frame demand (no other animation is in flight in this view).
    let f3 = render_at(&mut interp, &tree, 400);
    assert!(f3.ripples().is_empty(), "faded ripple retires");
    assert!(!interp.has_active_animations(), "no lingering frame demand");
}

#[test]
fn the_auto_max_radius_reaches_the_farthest_corner() {
    // Tap at the top-left origin of the 200×100 box: the farthest corner
    // is (200, 100), so the ink must grow to cover hypot(200, 100).
    let (mut interp, tree) = pressed_ripple_interp(0.0, 0.0, 1);
    let expected = 200.0_f32.hypot(100.0);
    // One ms before retirement the ease-out ramp has essentially arrived.
    let frame = render_at(&mut interp, &tree, 299);
    let r = frame.ripples()[0].params[2];
    assert!(
        r > expected * 0.99,
        "radius {r} must reach the farthest corner ({expected})"
    );
}

#[test]
fn a_hold_spawns_once_while_rapid_taps_spawn_one_ripple_each() {
    use byard_core::platform::EventKind as K;
    let (mut interp, tree) = pressed_ripple_interp(30.0, 40.0, 1);
    // Held press across two renders: still exactly one ripple.
    let _ = render_at(&mut interp, &tree, 20);
    let held = render_at(&mut interp, &tree, 40);
    assert_eq!(held.ripples().len(), 1, "a hold never respawns");

    // Release and tap again (a fresh press identity): a second ripple
    // joins the first, still fading, their ink pools on the GPU.
    interp.dispatch_events(&[pointer(K::PointerUp, 30.0, 40.0, 60)]);
    interp.dispatch_events(&[pointer(K::PointerDown, 80.0, 50.0, 80)]);
    let both = render_at(&mut interp, &tree, 100);
    assert_eq!(both.ripples().len(), 2, "each tap spawns its own ripple");
}

#[test]
fn ripple_radius_and_duration_props_override_the_defaults() {
    let src = "View V() {
            let btn = style {
                bg: 0x6750A4, ripple: 0x80FFFFFF
                ripple_radius: 10, ripple_duration: 100
                on pressed { ripple_active: true }
            }
            Box #[..btn, width: 200, height: 100] {}
        }";
    // Spawned at t = 10 by the helper.
    let (mut interp, tree) = pressed_ripple_named(src, 30.0, 40.0, 1);

    let mid = render_at(&mut interp, &tree, 100);
    assert_eq!(mid.ripples().len(), 1);
    assert!(
        mid.ripples()[0].params[2] <= 10.0,
        "`ripple_radius` caps the expansion: {:?}",
        mid.ripples()[0].params
    );
    let done = render_at(&mut interp, &tree, 120);
    assert!(
        done.ripples().is_empty(),
        "`ripple_duration: 100` retires the ink after 100 ms"
    );
}

#[test]
fn a_ripple_without_an_active_trigger_never_spawns() {
    use byard_core::platform::EventKind as K;
    // `ripple:` set but no `ripple_active` anywhere: pressing must not ink.
    let src = "View V() {
            Box #[bg: 0x6750A4, ripple: 0x80FFFFFF, width: 200, height: 100] {}
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[pointer(K::PointerDown, 30.0, 40.0, 1)]);
    let frame = render_at(&mut interp, &tree, 50);
    assert!(frame.ripples().is_empty(), "no trigger, no ink");
}

#[test]
fn ripple_depth_sits_between_the_background_and_the_children() {
    // A box with a child label: the ink must draw above the box fill but
    // beneath the text (RFC-0023 compositing order).
    let src = "View V() {
            let btn = style {
                bg: 0x6750A4, ripple: 0x80FFFFFF
                on pressed { ripple_active: true }
            }
            Box #[..btn, width: 200, height: 100] {
                Text(\"Save\") #[color: 0xFFFFFF]
            }
        }";
    let (mut interp, tree) = pressed_ripple_named(src, 30.0, 40.0, 1);
    let frame = render_at(&mut interp, &tree, 50);
    let ripple_depth = frame.ripples()[0].depth;
    let bg_depth = frame.solid_depths()[0];
    let text_depth = frame.text_depths()[0];
    // Later emission = nearer = smaller NDC-z.
    assert!(
        ripple_depth < bg_depth,
        "ink above the background ({ripple_depth} vs {bg_depth})"
    );
    assert!(
        text_depth < ripple_depth,
        "children above the ink ({text_depth} vs {ripple_depth})"
    );
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

// ── RFC-0023 §2 backdrop blur / vibrancy ────────────────────────────

fn rendered_frame(src: &str) -> (Interpreter, byard_core::frame::RenderFrame) {
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    (interp, frame)
}

#[test]
fn blur_props_emit_a_backdrop_with_the_resolved_fields() {
    let (_interp, frame) = rendered_frame(
        "View V() { Box #[width: 200, height: 100, radius: 16, blur: 20, \
             backdrop_tint: 0x80FFFFFF, blur_saturation: 2.0, blur_quality: high] {} }",
    );
    assert_eq!(frame.backdrops().len(), 1);
    let b = frame.backdrops()[0];
    assert_eq!(b.rect, [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(b.radii, [16.0; 4]);
    assert!((b.blur - 20.0).abs() < f32::EPSILON);
    assert!((b.tint[3] - 0.5).abs() < 0.01, "tint alpha: {:?}", b.tint);
    assert!((b.saturation - 2.0).abs() < f32::EPSILON);
    assert_eq!(b.quality, byard_core::frame::BLUR_QUALITY_HIGH);
}

#[test]
fn blur_clamps_to_the_max_radius_and_defaults_saturation() {
    let (_interp, frame) =
        rendered_frame("View V() { Box #[width: 100, height: 100, blur: 500] {} }");
    let b = frame.backdrops()[0];
    assert!(
        (b.blur - byard_core::frame::BLUR_MAX_RADIUS).abs() < f32::EPSILON,
        "500 clamps to the max, got {}",
        b.blur
    );
    assert!((b.saturation - 1.8).abs() < 0.01, "vibrancy default 1.8");
    assert_eq!(b.quality, byard_core::frame::BLUR_QUALITY_AUTO);
}

#[test]
fn a_tint_without_blur_lowers_to_a_plain_translucent_fill() {
    // No blur → no barrier, no off-screen work: the identical composite
    // is a translucent fill over the content behind.
    let (_interp, frame) =
        rendered_frame("View V() { Box #[width: 100, height: 100, backdrop_tint: 0x80336699] {} }");
    assert!(frame.backdrops().is_empty(), "no blur, no backdrop barrier");
    let wash = frame
        .decorated()
        .iter()
        .find(|d| (d.base.color[3] - 0.5).abs() < 0.01)
        .expect("the tint lowers to a translucent decorated fill");
    assert_eq!(wash.base.rect, [0.0, 0.0, 100.0, 100.0]);
}

// ── Spacer flexes (RFC-0005) ─────────────────────────────────────────

#[test]
fn a_spacer_absorbs_the_free_space_of_its_row() {
    // RFC-0005: `Spacer` is a *flexible* gap (`grow`, default 1). Before it
    // flexed, a trailing item sat glued to the leading one instead of at the
    // far end of the row.
    let (_interp, frame) = rendered_frame(
        "View V() { Row #[width: 300, height: 20] { \
                 Box #[bg: 0xFF0000, width: 20, height: 20] {} \
                 Spacer \
                 Box #[bg: 0x00FF00, width: 20, height: 20] {} \
             } }",
    );
    let x_of = |red: bool| {
        frame
            .instances()
            .iter()
            .find(|b| (b.color[0] > 0.8) == red && (b.color[1] > 0.8) != red)
            .map(|b| b.rect[0])
            .expect("both boxes are emitted")
    };
    assert!((x_of(true) - 0.0).abs() < 0.5, "the first box stays put");
    assert!(
        (x_of(false) - 280.0).abs() < 0.5,
        "the second is pushed to the far end, got {}",
        x_of(false)
    );
}

#[test]
fn a_spacer_honours_grow_and_basis() {
    // `grow: 0` degenerates to a fixed `basis`-sized gap; two spacers with
    // different `grow` split the free space in proportion.
    let (_interp, frame) = rendered_frame(
        "View V() { Row #[width: 300, height: 20] { \
                 Spacer #[grow: 0, basis: 40] \
                 Box #[bg: 0xFF0000, width: 20, height: 20] {} \
                 Spacer #[grow: 1] \
                 Box #[bg: 0x00FF00, width: 20, height: 20] {} \
                 Spacer #[grow: 3] \
             } }",
    );
    let x_of = |red: bool| {
        frame
            .instances()
            .iter()
            .find(|b| (b.color[0] > 0.8) == red && (b.color[1] > 0.8) != red)
            .map(|b| b.rect[0])
            .unwrap()
    };
    assert!((x_of(true) - 40.0).abs() < 0.5, "the fixed 40px gap held");
    // 300 − 40 − 20 − 20 = 220 free, split 1 : 3 → 55 then 165.
    assert!(
        (x_of(false) - (40.0 + 20.0 + 55.0)).abs() < 1.0,
        "the free space split 1:3, got {}",
        x_of(false)
    );
}

#[test]
fn the_backdrop_barrier_snapshots_the_content_behind_it() {
    // A solid card behind, then a bg-less glass pane with a child label:
    // the barrier must capture the card (and nothing later), and the
    // pane's depth must sit between the card and the label.
    let src = "View V() {
            Column #[width: 300, height: 200] {
                Box #[bg: 0xFF3366, width: 300, height: 80] {}
                Box #[blur: 12, width: 300, height: 80] {
                    Text(\"glass\") #[color: 0x000000]
                }
            }
        }";
    let (_interp, frame) = rendered_frame(src);
    assert_eq!(frame.backdrops().len(), 1);
    let mark = frame.backdrop_marks()[0];
    assert_eq!(mark.solid, 1, "the card was emitted behind the barrier");
    assert_eq!(mark.text, 0, "the pane's own label comes after it");
    let d = frame.backdrops()[0].depth;
    assert!(d < frame.solid_depths()[0], "pane above the card");
    assert!(frame.text_depths()[0] < d, "label above the pane");
}

#[test]
fn blur_animates_as_a_paint_prop() {
    let sample = |f: &byard_core::frame::RenderFrame| f.backdrops().first().map_or(0.0, |b| b.blur);
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[width: 100, height: 100, blur: on ? 16.0 : 4.0 with anim.linear(1000)] }",
        sample,
    );
    assert!((a - 4.0).abs() < 0.5, "starts near 4, got {a}");
    assert!((b - 10.0).abs() < 1.5, "~halfway, got {b}");
    assert!((c - 16.0).abs() < 0.5, "arrives at 16, got {c}");
}

#[test]
fn a_state_override_retargets_the_base_with_animation() {
    use byard_core::platform::EventKind as K;
    // RFC-0010 × RFC-0012/0016: the state changes the target, the base
    // owns the curve, `on hover { blur: 16 }` over
    // `blur: 4 with anim.linear(1000)` must ramp, not pop.
    let src = "View V() {
            let glass = style {
                blur: 4.0 with anim.linear(1000)
                on hover { blur: 16 }
            }
            Box #[..glass, width: 200, height: 100] {}
        }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.set_now_ms(0);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Hover the pane; the next renders sample the retargeted ramp.
    interp.dispatch_events(&[pointer(K::PointerMove, 50.0, 50.0, 1)]);
    let blur_at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.backdrops().first().map_or(-1.0, |b| b.blur)
    };
    let a = blur_at(&mut interp, 0);
    let b = blur_at(&mut interp, 500);
    let c = blur_at(&mut interp, 1000);
    assert!((a - 4.0).abs() < 0.5, "starts at the base value, got {a}");
    assert!(
        (b - 10.0).abs() < 1.5,
        "~halfway to the hover target, got {b}"
    );
    assert!(
        (c - 16.0).abs() < 0.5,
        "arrives at the hover target, got {c}"
    );
}

#[test]
fn an_animated_color_ramps_its_alpha_channel_too() {
    // RFC-0023 regression: a translucent `backdrop_tint` fades in, the
    // alpha byte animates alongside the OKLab channels. The old 3-channel
    // path dropped alpha entirely, collapsing `0x00FFFFFF → 0x80FFFFFF`
    // into an instant opaque white.
    let sample =
        |f: &byard_core::frame::RenderFrame| f.backdrops().first().map_or(-1.0, |b| b.tint[3]);
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[width: 100, height: 100, blur: 8, \
             backdrop_tint: on ? 0x80FFFFFF : 0x00FFFFFF with anim.linear(1000)] }",
        sample,
    );
    assert!(a.abs() < 0.02, "starts fully transparent, got {a}");
    assert!(
        (b - 0.25).abs() < 0.05,
        "~half the 0.5 target mid-ramp, got {b}"
    );
    assert!((c - 0.502).abs() < 0.02, "arrives at 0x80 alpha, got {c}");
}

#[test]
fn deepest_rect_overlap_counts_the_tallest_stack() {
    let r = |x: f32| [x, 0.0, 100.0, 100.0];
    assert_eq!(deepest_rect_overlap(&[]), 0);
    assert_eq!(deepest_rect_overlap(&[r(0.0)]), 1);
    // Disjoint panes never stack.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(200.0), r(400.0)]), 1);
    // Two overlapping panes plus a distant third: the cluster is 2.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(80.0), r(400.0)]), 2);
    // A chain a∩b, b∩c clusters to 3 around b, deliberately
    // conservative (see the fn docs), erring toward the diagnostic.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(80.0), r(160.0)]), 3);
    // Three co-located panes: a genuine 3-deep stack.
    assert_eq!(deepest_rect_overlap(&[r(0.0), r(10.0), r(20.0)]), 3);
}

#[test]
fn three_stacked_glass_panes_raise_the_overlap_warning() {
    let (interp, frame) = rendered_frame(
        "View V() { ZStack #[width: 200, height: 200] { \
             Box #[blur: 8, width: 180, height: 180] {} \
             Box #[blur: 8, width: 160, height: 160] {} \
             Box #[blur: 8, width: 140, height: 140] {} } }",
    );
    assert_eq!(frame.backdrops().len(), 3);
    assert_eq!(
        interp.perf_warnings(),
        &[PerfWarning::OverlappingBlurs { count: 3 }]
    );

    // Two panes are fine, stacked glass only warns at three.
    let (interp, _frame) = rendered_frame(
        "View V() { ZStack #[width: 200, height: 200] { \
             Box #[blur: 8, width: 180, height: 180] {} \
             Box #[blur: 8, width: 160, height: 160] {} } }",
    );
    assert!(
        interp.perf_warnings().is_empty(),
        "{:?}",
        interp.perf_warnings()
    );
}
