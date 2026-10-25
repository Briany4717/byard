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
use controls::checkbox_frame;

mod animation;
mod canvas;
mod controls;
mod instance;
mod layout;
mod lowering;
mod overlay;
mod props;
mod scroll;
mod states;

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
