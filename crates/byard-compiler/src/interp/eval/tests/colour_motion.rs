//! RFC-0057: motion on colours and gradients.

use super::animation::ramp_paint_prop;
use super::*;

/// The channel of the first decorated box's border, shadow or gradient, read
/// at 0, 500 and 1000 ms of a 1000 ms linear ramp.
fn ramp(src: &str, sample: impl Fn(&byard_core::frame::RenderFrame) -> f32) -> [f32; 3] {
    ramp_paint_prop(src, sample)
}

/// The fill of the first box drawn, translucent (decorated) or opaque.
fn fill(f: &byard_core::frame::RenderFrame) -> [f32; 4] {
    f.decorated()
        .first()
        .map(|d| d.base.color)
        .or_else(|| f.instances().first().map(|i| i.color))
        .unwrap_or([0.0; 4])
}

#[test]
fn a_border_colour_blends() {
    let [a, b, c] = ramp(
        "View V() { var on: Bool = false \
         Box #[bg: 0x202020, width: 40, height: 40, border_width: 2, \
         border: on ? 0x0000FF : 0xFF0000 with anim.linear(1000)] }",
        |f| f.decorated()[0].border_color[0],
    );
    assert!(a > 0.9, "starts red, got {a}");
    assert!((0.05..0.95).contains(&b), "mid-way, got {b}");
    assert!(c < 0.02, "ends blue, got {c}");
}

#[test]
fn a_shadow_colour_blends_and_is_not_an_integer_sweep() {
    // Before RFC-0057 the shadow's colour went through the scalar path: the
    // packed integer ran 0xFF0000 → 0x0000FF as a number.
    let [a, b, c] = ramp(
        "View V() { var on: Bool = false \
         Box #[bg: 0x808080, width: 40, height: 40, \
         shadow: (y: 4, blur: 8, color: on ? 0xFF0000FF : 0xFFFF0000 with anim.linear(1000))] }",
        |f| f.decorated()[0].shadow_color[0],
    );
    assert!(a > 0.9, "starts red, got {a}");
    assert!((0.05..0.95).contains(&b), "mid-way, got {b}");
    assert!(c < 0.02, "ends blue, got {c}");
    // The blue channel is the mirror, not garbage from the integer's bits.
    let [_, mid_blue, end_blue] = ramp(
        "View V() { var on: Bool = false \
         Box #[bg: 0x808080, width: 40, height: 40, \
         shadow: (y: 4, blur: 8, color: on ? 0xFF0000FF : 0xFFFF0000 with anim.linear(1000))] }",
        |f| f.decorated()[0].shadow_color[2],
    );
    assert!(
        (0.05..0.95).contains(&mid_blue),
        "blue mid-way, got {mid_blue}"
    );
    assert!(end_blue > 0.9, "ends blue, got {end_blue}");
}

#[test]
fn a_canvas_fill_and_stroke_blend() {
    let src = "View V() { var on: Bool = false \
         Canvas #[width: 60, height: 60] { \
           circle(cx: 30, cy: 30, r: 10, \
                  fill: on ? 0xFF0000FF : 0xFFFF0000 with anim.linear(1000), \
                  stroke: on ? 0xFF0000FF : 0xFFFF0000 with anim.linear(1000), \
                  stroke_width: 2) } }";
    let [fa, fb, fc] = ramp(src, |f| f.canvas_shapes()[0].fill_color[0]);
    assert!(
        fa > 0.9 && (0.05..0.95).contains(&fb) && fc < 0.02,
        "{fa} {fb} {fc}"
    );
    let [sa, sb, sc] = ramp(src, |f| f.canvas_shapes()[0].stroke_color[0]);
    assert!(
        sa > 0.9 && (0.05..0.95).contains(&sb) && sc < 0.02,
        "{sa} {sb} {sc}"
    );
}

#[test]
fn a_fade_from_transparent_keeps_the_hue_of_the_other_end() {
    // 0x00000000 → 0xFF5B8DEF. Mixed in straight alpha, the middle was a dark
    // blue-grey (lightness halfway to black); premultiplied, it is the blue at
    // half strength, so the ratio of its channels is the target's all the way.
    let target = [linear(0x5B), linear(0x8D), linear(0xEF)];
    let ratio = |c: [f32; 4]| c[2] / (c[0] + c[1] + c[2]);
    let want = target[2] / (target[0] + target[1] + target[2]);
    let sample = |f: &byard_core::frame::RenderFrame| ratio(fill(f));
    let [_, mid, end] = ramp(
        "View V() { var on: Bool = false \
         Box #[width: 40, height: 40, \
         bg: on ? 0xFF5B8DEF : 0x00000000 with anim.linear(1000)] }",
        sample,
    );
    assert!((end - want).abs() < 0.01, "ends on the target, got {end}");
    assert!(
        (mid - want).abs() < 0.03,
        "mid-way keeps the target's hue ({want}), got {mid}"
    );
    let alpha = |f: &byard_core::frame::RenderFrame| fill(f)[3];
    let [_, mid_alpha, _] = ramp(
        "View V() { var on: Bool = false \
         Box #[width: 40, height: 40, \
         bg: on ? 0xFF5B8DEF : 0x00000000 with anim.linear(1000)] }",
        alpha,
    );
    assert!(
        (mid_alpha - 0.5).abs() < 0.05,
        "half the alpha, got {mid_alpha}"
    );
    // The colour itself is the target's, at half the alpha: no grey fringe.
    let [_, mid_blue, _] = ramp(
        "View V() { var on: Bool = false \
         Box #[width: 40, height: 40, \
         bg: on ? 0xFF5B8DEF : 0x00000000 with anim.linear(1000)] }",
        |f| fill(f)[2],
    );
    assert!(
        (mid_blue - target[2]).abs() < 0.02,
        "mid-way blue is the target's ({}), got {mid_blue}",
        target[2]
    );
}

#[test]
fn a_fade_to_transparent_keeps_its_hue_too() {
    let target = [linear(0x5B), linear(0x8D), linear(0xEF)];
    let want = target[2] / (target[0] + target[1] + target[2]);
    let [start, mid, _] = ramp(
        "View V() { var on: Bool = false \
         Box #[width: 40, height: 40, \
         bg: on ? 0x00FFFFFF : 0xFF5B8DEF with anim.linear(1000)] }",
        |f| {
            let c = fill(f);
            c[2] / (c[0] + c[1] + c[2]).max(1e-6)
        },
    );
    assert!(
        (start - want).abs() < 0.01,
        "starts on the blue, got {start}"
    );
    // Blue towards a transparent *white*: the hue is the blue's until alpha
    // is gone, with no lift towards white from the invisible end.
    assert!(
        (mid - want).abs() < 0.03,
        "mid-way is still the blue, got {mid}"
    );
}

#[test]
fn alpha_on_a_colour_expression_still_animates() {
    let [a, b, c] = ramp(
        "View V() { var on: Bool = false \
         Box #[width: 40, height: 40, \
         bg: (0x5B8DEF).alpha(on ? 0.5 : 0.0 with anim.linear(1000))] }",
        |f| fill(f)[3],
    );
    assert!(a < 0.02, "starts clear, got {a}");
    assert!((0.15..0.35).contains(&b), "mid-way, got {b}");
    assert!((c - 0.5).abs() < 0.02, "ends at half, got {c}");
}

const GRADIENT: &str = "View V() { var on: Bool = false \
     Box #[bg: 0x000000, width: 80, height: 40, \
     gradient: on ? (from: 0xFF0000, to: 0x0000FF) \
                  : (from: 0x00FF00, to: 0xFFFF00) with anim.linear(1000)] }";

#[test]
fn a_gradient_animates_stop_by_stop() {
    let from = |ch: usize| {
        move |f: &byard_core::frame::RenderFrame| f.decorated()[0].gradient.unwrap().from[ch]
    };
    let to = |ch: usize| {
        move |f: &byard_core::frame::RenderFrame| f.decorated()[0].gradient.unwrap().to[ch]
    };
    // `from` goes green → red; `to` goes yellow → blue.
    let [g0, g1, g2] = ramp(GRADIENT, from(1));
    assert!(
        g0 > 0.9 && (0.05..0.95).contains(&g1) && g2 < 0.02,
        "{g0} {g1} {g2}"
    );
    let [r0, r1, r2] = ramp(GRADIENT, from(0));
    assert!(
        r0 < 0.02 && (0.05..0.95).contains(&r1) && r2 > 0.9,
        "{r0} {r1} {r2}"
    );
    let [b0, b1, b2] = ramp(GRADIENT, to(2));
    assert!(
        b0 < 0.02 && (0.05..0.95).contains(&b1) && b2 > 0.9,
        "{b0} {b1} {b2}"
    );
}

#[test]
fn a_conditional_of_gradients_is_the_one_its_condition_picks() {
    let (mut interp, tree) = lower_named(
        "View V() { var on: Bool = true \
         Box #[bg: 0x000000, width: 80, height: 40, \
         gradient: on ? (from: 0xFF0000, to: 0x0000FF) : (from: 0x00FF00, to: 0xFFFF00)] }",
        "V",
    );
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let gradient = frame.decorated()[0].gradient.expect("a gradient");
    assert!(gradient.from[0] > 0.9 && gradient.from[1] < 0.02);
}

#[test]
fn a_gradient_reversed_in_flight_continues_from_where_it_is() {
    let parsed = parse(GRADIENT);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    let red_at = |interp: &mut Interpreter, now: u32| {
        interp.set_now_ms(now);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.decorated()[0].gradient.unwrap().from[0]
    };
    red_at(&mut interp, 0);
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    red_at(&mut interp, 0);
    let before = red_at(&mut interp, 500);
    interp.write_var(sig, Value::Bool(false));
    interp.tick();
    let at_flip = red_at(&mut interp, 500);
    assert!(
        (at_flip - before).abs() < 0.02,
        "no jump at the reversal: {before} then {at_flip}"
    );
    assert!(red_at(&mut interp, 1500) < 0.02, "heads back to green");
    assert!(!interp.has_active_animations(), "settled");
}

#[test]
fn a_gradient_animation_allocates_once_and_costs_nothing_without_with() {
    // No `with`: no state at all.
    let (mut interp, tree) = lower_named(
        "View V() { Box #[bg: 0x000000, width: 80, height: 40, \
         gradient: (from: 0xFF0000, to: 0x0000FF)] }",
        "V",
    );
    interp.tick();
    interp.set_now_ms(0);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(interp.gradient_animations.capacity(), 0);
    assert_eq!(interp.color_animations.capacity(), 0);

    // With one: a single record, and the map never grows while it runs.
    let parsed = parse(GRADIENT);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    interp.tick();
    interp.set_now_ms(0);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    let capacity = interp.gradient_animations.capacity();
    for ms in (0..1200).step_by(16) {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        assert_eq!(interp.gradient_animations.len(), 1);
        assert_eq!(interp.gradient_animations.capacity(), capacity);
    }
}

#[test]
fn gradients_of_another_shape_jump_instead_of_blending() {
    // The evaluator's own guard: a record whose goal changes kind is
    // reseeded at the goal, never blended.
    let (mut interp, tree) = lower_named(
        "View V() { var on: Bool = false \
         Box #[bg: 0x000000, width: 80, height: 40, \
         gradient: (kind: linear, from: 0xFF0000, to: 0x0000FF) with anim.linear(1000)] }",
        "V",
    );
    interp.tick();
    interp.set_now_ms(0);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let key = *interp.gradient_animations.keys().next().expect("a record");
    let stored = interp.gradient_animations.get_mut(&key).unwrap();
    // As a program edited under a running animation would leave it.
    stored.stops = 3;
    interp.set_now_ms(16);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let shown = frame.decorated()[0].gradient.unwrap();
    assert!(
        shown.from[0] > 0.99 && shown.to[2] > 0.99,
        "jumped to the goal"
    );
    assert!(!interp.has_active_animations());
}
