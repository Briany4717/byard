//! Animated values: `with`, loops, delays, staggers, keyframes and the
//! animation state of rows that come and go.

use super::*;

// ── RFC-0011: paint-time transform attribute surface ─────────────────

#[test]
fn transform_attrs_reach_the_box_instance() {
    let parsed = parse(
        "View C() { Box #[bg: 0xFF0000, width: 50, height: 50, \
             translate: (5, 10), scale: 1.5, rotate: 90deg] }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let instances = frame.instances();
    assert_eq!(instances.len(), 1);
    let t = instances[0].transform;
    assert_eq!(t.translate, [5.0, 10.0]);
    assert_eq!(t.scale, [1.5, 1.5]);
    assert!((t.rotate - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    // Unset `origin` defaults to the element's own center, not (0,0).
    assert_eq!(t.origin, [25.0, 25.0]);
}

#[test]
fn with_animation_interpolates_toward_the_target_and_settles() {
    // A linear ramp gives deterministic sample points to assert on.
    let parsed = parse(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, width: 10, height: 10, \
             scale: on ? 2.0 : 1.0 with anim.linear(1000)] }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();

    let render_scale = |interp: &mut Interpreter, now: u32| -> f32 {
        interp.set_now_ms(now);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.instances()[0].transform.scale[0]
    };

    // At rest: target is 1.0, nothing is animating.
    assert!((render_scale(&mut interp, 0) - 1.0).abs() < 1e-3);
    assert!(!interp.has_active_animations());

    // Flip the target to 2.0 at t=0, the motion retargets from the current
    // value (~1.0) and is now active.
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    assert!(
        (render_scale(&mut interp, 0) - 1.0).abs() < 1e-2,
        "starts where it was"
    );
    assert!(
        interp.has_active_animations(),
        "a just-retargeted motion is active"
    );

    // Halfway through the 1000 ms ramp → ~1.5.
    assert!((render_scale(&mut interp, 500) - 1.5).abs() < 5e-2);

    // Past the end → arrived at 2.0 and settled (idle again).
    assert!((render_scale(&mut interp, 1000) - 2.0).abs() < 1e-3);
    assert!(
        !interp.has_active_animations(),
        "settles once the ramp completes"
    );
}

/// Drives a `on ? … : …` paint prop through a 1000 ms linear ramp and
/// returns the value `sample` reads from the rendered frame at t = 0 (just
/// after the flip), 500, and 1000 ms, the shared body of the coverage tests
/// below, which each assert a different paint prop interpolates.
pub(super) fn ramp_paint_prop(
    src: &str,
    sample: impl Fn(&byard_core::frame::RenderFrame) -> f32,
) -> [f32; 3] {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    // Seed at rest, then flip the target on at t = 0 so the motion retargets.
    interp.set_now_ms(0);
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    let at = |interp: &mut Interpreter, now: u32| {
        interp.set_now_ms(now);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        sample(&frame)
    };
    [
        at(&mut interp, 0),
        at(&mut interp, 500),
        at(&mut interp, 1000),
    ]
}

#[test]
fn radius_animates_as_a_paint_prop() {
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, width: 40, height: 40, radius: on ? 20 : 4 with anim.linear(1000)] }",
        |f| f.instances()[0].radii[0],
    );
    assert!((a - 4.0).abs() < 0.5, "starts near 4, got {a}");
    assert!((b - 12.0).abs() < 1.5, "~halfway, got {b}");
    assert!((c - 20.0).abs() < 0.5, "arrives at 20, got {c}");
}

#[test]
fn border_width_animates_as_a_paint_prop() {
    let sample = |f: &byard_core::frame::RenderFrame| {
        f.decorated()
            .iter()
            .map(|d| d.border_width)
            .fold(0.0_f32, f32::max)
    };
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, border: 0xFFFFFF, width: 40, height: 40, \
             border_width: on ? 8 : 2 with anim.linear(1000)] }",
        sample,
    );
    assert!((a - 2.0).abs() < 0.5, "starts near 2, got {a}");
    assert!((b - 5.0).abs() < 1.0, "~halfway, got {b}");
    assert!((c - 8.0).abs() < 0.5, "arrives at 8, got {c}");
}

#[test]
fn a_shadow_field_animates() {
    let sample = |f: &byard_core::frame::RenderFrame| {
        f.decorated()
            .iter()
            .map(|d| d.shadow_dy)
            .fold(0.0_f32, f32::max)
    };
    let [a, b, c] = ramp_paint_prop(
        "View V() { var on: Bool = false \
             Box #[bg: 0x808080, width: 40, height: 40, \
             shadow: (y: (on ? 12 : 2) with anim.linear(1000), blur: 8, color: 0x80000000)] }",
        sample,
    );
    assert!((a - 2.0).abs() < 0.6, "starts near 2, got {a}");
    assert!((b - 7.0).abs() < 1.5, "~halfway, got {b}");
    assert!((c - 12.0).abs() < 0.6, "arrives at 12, got {c}");
}

// ── RFC-0025: looping & indefinite animations ────────────────────────

/// Renders `src` at each of `times` (ms on the engine clock) and returns
/// what `sample` reads from the frame, plus whether the animation was still
/// in the active set at that instant, the two things every looping test
/// asks about.
pub(super) fn sample_over_time(
    src: &str,
    times: &[u32],
    sample: impl Fn(&byard_core::frame::RenderFrame) -> f32,
) -> Vec<(f32, bool)> {
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    times
        .iter()
        .map(|ms| {
            interp.set_now_ms(*ms);
            let mut frame = byard_core::frame::RenderFrame::new();
            interp.render(&tree, &mut frame, 400.0, 300.0);
            (sample(&frame), interp.has_active_animations())
        })
        .collect()
}

/// The first emitted box's paint-time `translate.x`.
fn translate_x(frame: &byard_core::frame::RenderFrame) -> f32 {
    frame.instances()[0].transform.translate[0]
}

#[test]
fn an_infinite_repeat_wraps_and_keeps_the_frames_coming() {
    // A 400 ms linear ramp from 0 → 100, repeating forever: it must wrap at
    // the period and never let the app idle (that is what a spinner needs).
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(400ms, from: 0, repeat: infinite)] }";
    let seen = sample_over_time(src, &[0, 200, 400, 600, 4_000], translate_x);
    let x: Vec<f32> = seen.iter().map(|(v, _)| *v).collect();
    assert!((x[0] - 0.0).abs() < 1.0, "starts at `from`, got {}", x[0]);
    assert!((x[1] - 50.0).abs() < 2.0, "halfway, got {}", x[1]);
    assert!((x[2] - 0.0).abs() < 1.0, "wrapped, got {}", x[2]);
    assert!((x[3] - 50.0).abs() < 2.0, "second play, got {}", x[3]);
    assert!(
        seen.iter().all(|(_, active)| *active),
        "an infinite animation never settles"
    );
}

#[test]
fn reverse_oscillates_between_the_two_endpoints() {
    // The RFC's pulsing badge: `from` and the target are the two ends, and
    // alternating plays turn it into a continuous oscillation.
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(400ms, from: 0, \
                   repeat: infinite, reverse: true)] }";
    let x: Vec<f32> = sample_over_time(src, &[0, 200, 400, 600, 800], translate_x)
        .into_iter()
        .map(|(v, _)| v)
        .collect();
    assert!((x[0] - 0.0).abs() < 1.0, "out from `from`, got {}", x[0]);
    assert!((x[1] - 50.0).abs() < 2.0);
    assert!((x[2] - 100.0).abs() < 1.0, "at the far end, got {}", x[2]);
    assert!((x[3] - 50.0).abs() < 2.0, "coming back, got {}", x[3]);
    assert!((x[4] - 0.0).abs() < 1.0, "home again, got {}", x[4]);
}

#[test]
fn a_counted_repeat_stops_and_lets_the_app_idle() {
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(400ms, from: 0, repeat: 2)] }";
    let seen = sample_over_time(src, &[0, 500, 799, 900, 5_000], translate_x);
    assert!(seen[1].1, "still playing the second time");
    assert!((seen[3].0 - 100.0).abs() < 1.0, "holds the final value");
    assert!(
        !seen[3].1 && !seen[4].1,
        "a finite repeat leaves the active set, so the app idles"
    );
}

#[test]
fn a_delay_holds_the_start_value_then_animates() {
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: (100, 0) with anim.linear(200ms, from: 0, delay: 300ms)] }";
    let seen = sample_over_time(src, &[0, 200, 400, 600], translate_x);
    assert!((seen[0].0).abs() < 0.01, "held at `from` during the delay");
    assert!((seen[1].0).abs() < 0.01, "still held at 200ms");
    assert!(
        seen[1].1,
        "a pending delay keeps requesting frames, otherwise it would never start"
    );
    assert!((seen[2].0 - 50.0).abs() < 2.0, "halfway, got {}", seen[2].0);
    assert!((seen[3].0 - 100.0).abs() < 0.5, "arrived");
}

#[test]
fn stagger_offsets_each_item_by_its_index() {
    // RFC-0025 §"Stagger": one written animation, a different start per item.
    let src = "View V() { let xs = [1, 2, 3] \
                   Column { for i, x in xs { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with \
                             anim.stagger(linear(200ms, from: 0), 100ms, i)] {} \
                   } } }";
    // Every reading starts from a render at t = 0: an animation's timeline
    // begins when it first appears, exactly as it does in a running app.
    let at = |ms: u32| {
        *sample_over_time(src, &[0, ms], |f| {
            // Each row's translate, in row order.
            f.instances()
                .iter()
                .map(|b| b.transform.translate[0])
                .sum::<f32>()
        })
        .last()
        .map(|(v, _)| v)
        .unwrap()
    };
    // At 100 ms the first row is halfway (50), the second just starting (0),
    // the third still waiting (0).
    assert!((at(100) - 50.0).abs() < 3.0, "only row 0 has moved");
    // At 200 ms: row 0 arrived (100), row 1 halfway (50), row 2 starting (0).
    assert!((at(200) - 150.0).abs() < 4.0, "the wave has advanced");
    // Once every row has played out, they are all at the target.
    assert!((at(1_000) - 300.0).abs() < 0.5, "all rows arrived");
}

#[test]
fn keyframes_walk_their_steps_and_loop() {
    // RFC-0025 §3: a three-step sequence over 400 ms, looping forever.
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: anim.keyframes(0%: 0, 50%: 80, 100%: 0, \
                   duration: 400ms, loop: true)] }";
    let seen = sample_over_time(src, &[0, 100, 200, 300, 400, 500], translate_x);
    let x: Vec<f32> = seen.iter().map(|(v, _)| *v).collect();
    assert!((x[0] - 0.0).abs() < 0.5, "the 0% step");
    assert!(
        (x[1] - 40.0).abs() < 2.0,
        "midway to the 50% step, got {}",
        x[1]
    );
    assert!((x[2] - 80.0).abs() < 0.5, "the 50% step, got {}", x[2]);
    assert!((x[3] - 40.0).abs() < 2.0, "coming back down, got {}", x[3]);
    assert!((x[4] - 0.0).abs() < 0.5, "wrapped to the start");
    assert!((x[5] - 40.0).abs() < 2.0, "second play");
    assert!(
        seen.iter().all(|(_, active)| *active),
        "a loop never settles"
    );
}

#[test]
fn keyframe_pairs_interpolate_component_wise() {
    // A coordinate pair per step (the RFC's indeterminate-progress shape).
    let src = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                   translate: anim.keyframes(0%: (-100, 10), 100%: (300, 30), duration: 400ms)] }";
    let seen = sample_over_time(src, &[0, 200], |f| {
        let t = f.instances()[0].transform.translate;
        // Encode both axes in one sample: x + y * 1000.
        t[0] + t[1] * 1000.0
    });
    let (x, y) = (seen[1].0 % 1000.0, (seen[1].0 / 1000.0).floor());
    assert!(
        (x - 100.0).abs() < 2.0,
        "x halfway from -100 to 300, got {x}"
    );
    assert!((y - 20.0).abs() < 0.5, "y halfway from 10 to 30, got {y}");
}

#[test]
fn a_keyframe_segment_uses_its_own_easing() {
    // Same two-segment shape, but the first arrives with `ease_out`: at the
    // quarter mark it must be further along than the linear version.
    let linear = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                      translate: anim.keyframes(0%: 0, 50%: 100, 100%: 0, duration: 400ms)] }";
    let eased = "View V() { Box #[bg: 0x808080, width: 10, height: 10, \
                     translate: anim.keyframes(0%: 0, 50%: 100 ease_out, 100%: 0, \
                     duration: 400ms)] }";
    let at_quarter = |src: &str| sample_over_time(src, &[0, 100], translate_x)[1].0;
    assert!(
        at_quarter(eased) > at_quarter(linear) + 10.0,
        "ease_out leads linear: {} vs {}",
        at_quarter(eased),
        at_quarter(linear)
    );
}

#[test]
fn a_keyframed_colour_blends_in_oklab() {
    // A colour sequence samples its two surrounding steps perceptually,
    // never by lerping the packed integer.
    let src = "View V() { Box #[width: 10, height: 10, \
                   bg: anim.keyframes(0%: 0x000000, 100%: 0xFFFFFF, duration: 400ms)] }";
    let grey = |f: &byard_core::frame::RenderFrame| f.instances()[0].color[0];
    let seen = sample_over_time(src, &[0, 200, 400], grey);
    assert!(seen[0].0 < 0.01, "starts black, got {}", seen[0].0);
    // The frame carries linear light, so the number read here is the *luminance*
    // of the midpoint. OKLab's midpoint of black→white is `L = 0.5`, the
    // perceptual middle, which is a luminance of about 0.125 and encodes to
    // roughly `0x63`. The mistake this rules out is lerping the two colours in
    // linear light, which lands on 0.5 luminance, a grey most of a stop too
    // bright and the reason a naive fade looks like it washes out in the middle.
    assert!(
        seen[1].0 > 0.09 && seen[1].0 < 0.17,
        "the OKLab midpoint of black→white is the perceptual middle \
             (luminance ≈ 0.125), not the linear-light midpoint (0.5), got {}",
        seen[1].0
    );
    assert!(seen[2].0 > 0.99, "ends white, got {}", seen[2].0);
}

#[test]
fn a_looping_colour_oscillates_between_two_colours() {
    let src = "View V() { Box #[width: 10, height: 10, \
                   bg: 0xFFFFFF with anim.linear(400ms, from: 0x000000, \
                   repeat: infinite, reverse: true)] }";
    let grey = |f: &byard_core::frame::RenderFrame| f.instances()[0].color[0];
    let seen = sample_over_time(src, &[0, 400, 800], grey);
    assert!(seen[0].0 < 0.01, "starts black");
    assert!(seen[1].0 > 0.99, "reaches white");
    assert!(seen[2].0 < 0.01, "and comes back");
    assert!(seen.iter().all(|(_, active)| *active));
}

#[test]
fn an_unmounted_branch_starts_its_animation_over() {
    // RFC-0025: "no separate stop-animation API, the animation lives and
    // dies with its element". A collapsed `when` branch really *unmounts*,
    // so its animation state goes with it: when the branch comes back the
    // spinner starts its turn again instead of resuming a stale phase.
    let src = "View V() { var shown: Bool = true \
                   when shown { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with anim.linear(400ms, from: 0, \
                             repeat: infinite)] {} \
                   } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        (
            frame.instances().first().map(|b| b.transform.translate[0]),
            interp.has_active_animations(),
        )
    };
    render(&mut interp, 0);
    let (x, active) = render(&mut interp, 100);
    assert!((x.unwrap() - 25.0).abs() < 2.0, "a quarter in, got {x:?}");
    assert!(active);

    // Hide it: nothing is drawn, and the app can idle.
    let shown = interp.var_signal(&Symbol::intern("shown")).unwrap();
    interp.write_var(shown, Value::Bool(false));
    interp.tick();
    let (x, active) = render(&mut interp, 900);
    assert_eq!(x, None, "nothing is drawn while hidden");
    assert!(!active, "an unmounted loop costs no frames");

    // Show it again: a fresh mount, so a fresh turn from `from`.
    interp.write_var(shown, Value::Bool(true));
    interp.tick();
    let (x, _) = render(&mut interp, 900);
    assert!(x.unwrap().abs() < 2.0, "a remount starts over, got {x:?}");
    let (x, _) = render(&mut interp, 1_000);
    assert!(
        (x.unwrap() - 25.0).abs() < 3.0,
        "…and runs from there, got {x:?}"
    );
}

#[test]
fn an_animation_that_stops_being_drawn_pauses_and_resumes_in_phase() {
    // RFC-0025 §2: the element is still mounted, it just is not being
    // *painted*, here a windowed `ScrollView` row scrolled out of the
    // window and back. That is a **pause**: the app idles while the row is
    // away and the motion continues exactly where it stopped, with no jump.
    //
    // The mechanism matters, and this test used to use the wrong one. It
    // emptied the list, which is an *unmount*, not an element that is
    // merely off-screen, and RFC-0025 is explicit that an animation lives
    // and dies with its element. Scrolling is the case §2 describes: the
    // pool keeps its rows and their state, only the emission stops.
    let src = "View V() { var y: Float = 0.0 \
                   ScrollView #[width: 40, height: 20, row_height: 20, \
                                windowed: true, offset: (0, y)] { \
                     Column { for row in [1, 2, 3, 4, 5, 6, 7, 8] { \
                       Box #[bg: 0x808080, width: 10, height: 20, \
                             translate: (100, 0) with anim.linear(400ms, from: 0, \
                             repeat: infinite)] {} \
                     } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.instances().first().map(|b| b.transform.translate[0])
    };
    let scroll_to = |interp: &mut Interpreter, y: f64| {
        let sig = interp.var_signal(&Symbol::intern("y")).unwrap();
        interp.write_var(sig, Value::Float(y));
        interp.tick();
    };

    render(&mut interp, 0);
    let x = render(&mut interp, 100);
    assert!((x.unwrap() - 25.0).abs() < 2.0, "a quarter in, got {x:?}");

    // Scroll the first row out of the window. It is still mounted; it is
    // simply not drawn, so its timeline pauses.
    scroll_to(&mut interp, 120.0);
    for step in 2..10 {
        render(&mut interp, step * 100);
    }

    // Back to the top: the row resumes where it stopped rather than
    // jumping to where a wall clock would have carried it.
    scroll_to(&mut interp, 0.0);
    let x = render(&mut interp, 900);
    assert!(
        (x.unwrap() - 25.0).abs() < 4.0,
        "resumes in phase, got {x:?}"
    );
}

/// **The defect the instance half of the key exists for.** Two `for` rows
/// heading for *different* targets each reach their own.
///
/// Before the animation key carried an instance, the two rows shared one
/// `Motion`: every frame, row A retargeted it to A's goal and row B
/// immediately retargeted it to B's, each reseeding `from` to the current
/// sampled value and restarting the clock. The pair stalled near `t = 0`
/// and neither arrived. Anyone with a list of animated components was
/// affected, and the symptom, "my springs are sluggish in a list", points
/// nowhere near the cause.
#[test]
fn two_for_rows_with_different_targets_each_reach_their_own() {
    let src = "View V() { var rows = [{ w: 40 }, { w: 200 }] \
                   Column { for row in rows { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (row.w, 0) with anim.linear(200ms, from: 0)] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };

    render(&mut interp, 0);
    // Mid-flight the two rows are already apart, in proportion to their
    // own targets, a shared `Motion` could only ever produce one value.
    let mid = render(&mut interp, 100);
    assert_eq!(mid.len(), 2, "two rows");
    assert!(
        mid[1] > mid[0] * 2.0,
        "the rows must animate independently, got {mid:?}"
    );
    // …and each settles on its own target rather than fighting to a stall.
    let settled = render(&mut interp, 400);
    assert!(
        (settled[0] - 40.0).abs() < 1.0 && (settled[1] - 200.0).abs() < 1.0,
        "each row must reach its own target, got {settled:?}"
    );
    assert!(
        !interp.has_active_animations(),
        "both arrived, so nothing is still in motion"
    );
}

/// Nested loops distinguish instances too, and without a path to fold: a
/// `for` inside a `for` body is lowered once per *outer* row, so its pool,
/// and every element signal in it, belongs to that row alone.
#[test]
fn nested_for_rows_animate_independently() {
    let src = "View V() { var groups = [{ items: [{ w: 30 }] }, { items: [{ w: 180 }] }] \
                   Column { for g in groups { \
                     Column { for item in g.items { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (item.w, 0) with anim.linear(200ms, from: 0)] {} \
                     } } \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .filter(|x| *x > 0.0)
            .collect()
    };
    render(&mut interp, 0);
    render(&mut interp, 100);
    let settled = render(&mut interp, 400);
    assert_eq!(settled.len(), 2, "two innermost rows, got {settled:?}");
    assert!(
        (settled[0] - 30.0).abs() < 1.0 && (settled[1] - 180.0).abs() < 1.0,
        "each nested row must reach its own target, got {settled:?}"
    );
}

/// The other side of the same coin, and what the instance key changed: a row
/// that leaves the *list* is unmounted, and an unmount takes its animation
/// with it (RFC-0025: "no separate stop-animation API, the animation lives and
/// dies with its element").
///
/// This is not a preference between two readings. A `for` pool's bodies are
/// grow-only, so index 0 keeps its lowered nodes and its element signal
/// across a shrink; without dropping the state, a re-grown row would resume
/// **a different element's** timeline. Starting fresh is the only answer
/// that describes something real.
#[test]
fn a_row_that_leaves_the_list_drops_its_animation() {
    let src = "View V() { var rows: List<Int> = [1] \
                   Column { for row in rows { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with anim.linear(400ms, from: 0, \
                             repeat: infinite)] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        (
            frame.instances().first().map(|b| b.transform.translate[0]),
            interp.has_active_animations(),
        )
    };
    render(&mut interp, 0);
    let (x, _) = render(&mut interp, 100);
    assert!((x.unwrap() - 25.0).abs() < 2.0, "a quarter in, got {x:?}");

    let rows = interp.var_signal(&Symbol::intern("rows")).unwrap();
    interp.write_var(rows, Value::List(vec![]));
    interp.tick();
    let (x, active) = render(&mut interp, 900);
    assert_eq!(x, None, "nothing is drawn while it is gone");
    assert!(!active, "and it costs no frames");

    // A new row in the same pool slot is a new element, not the old one
    // resuming: it starts at the beginning of its own timeline.
    interp.write_var(rows, Value::List(vec![Value::Int(1)]));
    interp.tick();
    let (x, _) = render(&mut interp, 900);
    assert!(x.unwrap() < 5.0, "a re-grown row starts fresh, got {x:?}");
}

/// A `when` inside a `for`, the shape every real list has, because rows are
/// filtered, must not cost the row its identity.
///
/// A `when` branch is lowered **lazily, at render time**, with the scope it
/// was written in restored from a snapshot and truncated again immediately.
/// Anything inside it that resolves later, an animated attribute reading
/// `row`, an event action, a nested pool, has to capture that scope while it
/// is briefly back. Without it, `row` resolves to nothing at render time and
/// every animated target in the branch silently reads `0`: the list looks
/// mounted and correct, and none of it moves.
#[test]
fn a_row_inside_a_when_still_knows_which_row_it_is() {
    let src = "View V() { var count = 2 let rows = [{ w: 40 }, { w: 200 }] \
                   Column { for i, row in rows { when i < count { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (row.w, 0) with anim.linear(200ms, from: 0)] {} \
                   } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };
    render(&mut interp, 0);
    let settled = render(&mut interp, 400);
    assert_eq!(settled.len(), 2, "two rows, got {settled:?}");
    assert!(
        (settled[0] - 40.0).abs() < 1.0 && (settled[1] - 200.0).abs() < 1.0,
        "each filtered row must reach its own target, got {settled:?}"
    );
}

/// The same loss, seen through the other thing a row knows about itself: its
/// index. A stagger's delay is `step × i`, so a branch that forgot `i` gives
/// every row a delay of zero and the cascade arrives as one flash.
#[test]
fn a_stagger_inside_a_when_still_cascades_by_index() {
    let src = "View V() { var count = 3 let rows = [1, 2, 3] \
                   Column { for i, row in rows { when i < count { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with anim.stagger(linear(100ms, from: 0), \
                             200ms, i)] {} \
                   } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };
    render(&mut interp, 0);
    // 250ms in: row 0 has played out, row 1 is halfway through its own play,
    // row 2 has not started. One clause, three phases.
    let mid = render(&mut interp, 250);
    assert_eq!(mid.len(), 3, "three rows, got {mid:?}");
    assert!(
        (mid[0] - 100.0).abs() < 1.0 && (mid[1] - 50.0).abs() < 5.0 && mid[2] < 1.0,
        "the cascade must run in index order, got {mid:?}"
    );
}

/// A row's *size* comes from the same scope its colour does.
///
/// Layout runs one pass ahead of paint over the same tree, and paint
/// restores each box's captured instance environment before reading its
/// attrs. The layout pass did not, so every dimension written in terms of
/// the element it belongs to, `width: row.w` in a list, `width: size` in a
/// user view, resolved to nothing there. Nothing reported it: a `width`
/// that resolves to nothing is a box with no width, which is a box that
/// fills its parent. So the list laid out as one full-width column and
/// painted the colours perfectly.
#[test]
fn a_row_is_laid_out_from_its_own_scope() {
    let src = "View V() { let rows = [{ w: 60 }, { w: 220 }] \
                   Column { for row in rows { \
                       Row #[bg: 0x808080, height: 10, width: row.w] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 700.0, 300.0);
    let widths: Vec<f32> = frame.instances().iter().map(|b| b.rect[2]).collect();
    assert_eq!(
        widths,
        vec![60.0, 220.0],
        "each row is laid out at its own width, not stretched to the viewport"
    );
}

/// The loss compounds: a pool lowered *inside* a lazily-lowered branch
/// inherits that branch's scope, so a nested `for` whose body reads the
/// **outer** row, a per-row detail on each of a row's children, needs the
/// capture to have happened one level up before it can capture it in turn.
#[test]
fn a_nested_for_inside_a_when_still_reads_its_outer_row() {
    let src = "View V() { var count = 2 \
                   let groups = [{ w: 30, items: [1] }, { w: 180, items: [1] }] \
                   Column { for i, g in groups { when i < count { \
                     Column { for item in g.items { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (g.w, 0) with anim.linear(200ms, from: 0)] {} \
                     } } \
                   } } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let render = |interp: &mut Interpreter, ms: u32| -> Vec<f32> {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect()
    };
    render(&mut interp, 0);
    let settled = render(&mut interp, 400);
    assert_eq!(settled.len(), 2, "one box per nested row, got {settled:?}");
    assert!(
        (settled[0] - 30.0).abs() < 1.0 && (settled[1] - 180.0).abs() < 1.0,
        "each nested row must reach its own target, got {settled:?}"
    );
}

#[test]
fn a_retarget_cancels_a_pending_delay_but_never_a_stagger() {
    // RFC-0025 §5: a delayed transition must not overwrite a newer target,
    // so a target change restarts it immediately, while a stagger's
    // entrance offset survives (it is sequencing, not a response).
    let delayed = "View V() { var on: Bool = false \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: ((on ? 100 : 0), 0) with anim.linear(200ms, from: 0, \
                             delay: 400ms)] }";
    let (mut interp, tree) = lower_named(delayed, "V");
    interp.tick();
    let at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        translate_x(&frame)
    };
    at(&mut interp, 0);
    // Flip the target at 300 ms, inside the original delay window.
    let on = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(on, Value::Bool(true));
    interp.tick();
    at(&mut interp, 300);
    // The pending delay is cancelled, so the property heads for the new
    // target at once rather than sitting still for another 400 ms.
    assert!(
        (at(&mut interp, 400) - 50.0).abs() < 3.0,
        "halfway 100 ms after the retarget, got {}",
        at(&mut interp, 400)
    );
    assert!((at(&mut interp, 500) - 100.0).abs() < 1.0, "arrived");

    // A stagger, by contrast, honours its offset again, the cascade
    // *replays* in item order instead of snapping.
    let staggered = "View V() { var on: Bool = false \
                         Column { for i, row in [1, 2] { \
                             Box #[bg: 0x808080, width: 10, height: 10, \
                                   translate: ((on ? 100 : 0), 0) with \
                                   anim.stagger(linear(100ms, from: 0), 200ms, i)] {} \
                         } } }";
    let (mut interp, tree) = lower_named(staggered, "V");
    interp.tick();
    let xs = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect::<Vec<_>>()
    };
    xs(&mut interp, 0);
    let on = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(on, Value::Bool(true));
    interp.tick();
    xs(&mut interp, 0);
    // 150 ms in: row 0 (no offset) has arrived, row 1 is still waiting out
    // its 200 ms offset.
    let seen = xs(&mut interp, 150);
    assert!((seen[0] - 100.0).abs() < 1.0, "row 0 played, got {seen:?}");
    assert!(seen[1].abs() < 1.0, "row 1 still waiting, got {seen:?}");
    let seen = xs(&mut interp, 400);
    assert!(
        seen.iter().all(|x| (x - 100.0).abs() < 1.0),
        "the whole cascade has played, got {seen:?}"
    );
}

#[test]
fn a_restart_witness_replays_the_animation_in_order() {
    // RFC-0025 §5's replay case: an entrance's endpoints never change, so
    // nothing would ever retarget it. `restart:` is the reference-free "play
    // that again", and because a replay is intentional sequencing, the
    // stagger offsets are honoured again rather than cancelled.
    let src = "View V() { var attempt: Int = 0 \
                   Column { for i, row in [1, 2] { \
                       Box #[bg: 0x808080, width: 10, height: 10, \
                             translate: (100, 0) with \
                             anim.stagger(linear(100ms, from: 0), 200ms, i, restart: attempt)] {} \
                   } } }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let xs = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.transform.translate[0])
            .collect::<Vec<_>>()
    };
    // The first cascade plays out.
    xs(&mut interp, 0);
    let seen = xs(&mut interp, 600);
    assert!(
        seen.iter().all(|x| (x - 100.0).abs() < 1.0),
        "both rows arrived, got {seen:?}"
    );

    // Bump the witness: the cascade runs again, in item order.
    let attempt = interp.var_signal(&Symbol::intern("attempt")).unwrap();
    interp.write_var(attempt, Value::Int(1));
    interp.tick();
    let seen = xs(&mut interp, 600);
    assert!(
        seen[0].abs() < 1.0 && seen[1].abs() < 1.0,
        "the replay starts both rows over, got {seen:?}"
    );
    let seen = xs(&mut interp, 700);
    assert!(
        (seen[0] - 100.0).abs() < 1.0,
        "row 0 has replayed, got {seen:?}"
    );
    assert!(
        seen[1].abs() < 1.0,
        "row 1 is still waiting out its offset, got {seen:?}"
    );
    let seen = xs(&mut interp, 950);
    assert!(
        seen.iter().all(|x| (x - 100.0).abs() < 1.0),
        "the whole cascade replayed, got {seen:?}"
    );
}

#[test]
fn a_restart_witness_also_replays_a_keyframe_sequence() {
    let src = "View V() { var attempt: Int = 0 \
                   Box #[bg: 0x808080, width: 10, height: 10, \
                         translate: anim.keyframes(0%: 0, 100%: 100, duration: 400ms, \
                         restart: attempt)] {} }";
    let (mut interp, tree) = lower_named(src, "V");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        translate_x(&frame)
    };
    at(&mut interp, 0);
    assert!((at(&mut interp, 400) - 100.0).abs() < 1.0, "played out");
    let attempt = interp.var_signal(&Symbol::intern("attempt")).unwrap();
    interp.write_var(attempt, Value::Int(1));
    interp.tick();
    assert!(at(&mut interp, 400).abs() < 1.0, "the sequence starts over");
    assert!((at(&mut interp, 600) - 50.0).abs() < 3.0, "and runs again");
}

#[test]
fn stopping_the_looping_example_empties_the_active_set() {
    // Ties the shipped RFC-0025 example to the promise its header makes: with
    // `loading` off, *every* endless animation is unmounted, so the active set
    // empties and a host may park its event loop instead of spinning. A
    // stray infinite animation left outside the `when` would show up here as
    // an app that can never idle.
    let src = include_str!("../../../../../byard-cli/examples/looping_animations/src/main.byd");
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let at = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 700.0, 750.0);
        interp.has_active_animations()
    };
    assert!(at(&mut interp, 0), "the loaders animate on mount");
    assert!(at(&mut interp, 2_000), "and keep animating");

    let loading = interp.var_signal(&Symbol::intern("loading")).unwrap();
    interp.write_var(loading, Value::Bool(false));
    interp.tick();
    for ms in [2_100_u32, 3_000, 9_000] {
        assert!(
            !at(&mut interp, ms),
            "nothing may still be animating at t={ms} after Stop"
        );
    }
    // Starting again brings the motion back (a fresh mount, from the top).
    interp.write_var(loading, Value::Bool(true));
    interp.tick();
    assert!(at(&mut interp, 9_100), "Start resumes the motion");
}

#[test]
fn a_layout_property_cannot_be_keyframed() {
    // RFC-0025 §3 defers to RFC-0010: keyframes on a layout prop would relayout
    // every frame (animation is paint-time only), so they are rejected like
    // `with` is.
    let parsed = parse(
        "View V() { Box #[bg: 0x808080, height: 10, \
             width: anim.keyframes(0%: 0, 100%: 200, duration: 1s)] }",
    );
    let mut interp = Interpreter::new();
    let _ = interp.lower_view(&parsed.views[0], &[]);
    assert!(
        interp.errors().iter().any(
            |e| matches!(e, CompileError::LayoutPropNotAnimatable { prop, .. } if prop == "width")
        ),
        "expected LayoutPropNotAnimatable, got {:?}",
        interp.errors()
    );
}

/// RFC-0005 `ScrollView`: content is clipped to the viewport and translated
/// by `−offset`, so scrolling moves the content up without relayout.
#[test]
fn scrollview_clips_and_translates_content_by_offset() {
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

    // The red content box's paint-time translate.y (where the scroll offset
    // lives, the shader applies it, so the layout rect is untouched, i.e.
    // no relayout on scroll), plus whether a content clip was emitted.
    let sample = |interp: &mut Interpreter| -> (f32, f32, usize) {
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        let red = *frame
            .instances()
            .iter()
            .find(|b| b.color[0] > 0.8 && b.color[1] < 0.3 && b.color[2] < 0.3)
            .expect("the red content box is emitted");
        (red.rect[1], red.transform.translate[1], frame.clips().len())
    };

    let (rect_y0, tx0, clips0) = sample(&mut interp);
    assert!(clips0 >= 1, "the ScrollView must emit a content clip");

    // Scroll down by 40 logical px → the content's paint translate moves up
    // by 40, while its layout rect is unchanged (no relayout).
    let off = interp.var_signal(&Symbol::intern("off")).unwrap();
    interp.write_var(off, Value::Int(40));
    interp.tick();
    let (rect_y1, tx1, clips1) = sample(&mut interp);
    assert!(clips1 >= 1);
    assert!(
        (rect_y0 - rect_y1).abs() < 0.01,
        "layout rect must not move on scroll (no relayout): {rect_y0} vs {rect_y1}"
    );
    assert!(
        (tx0 - tx1 - 40.0).abs() < 0.5,
        "content must translate up by the offset: tx0={tx0} tx1={tx1}"
    );
}

#[test]
fn with_animation_lerps_color_in_oklab_and_settles() {
    let parsed = parse(
        "View V() { var on: Bool = false \
             Box #[width: 10, height: 10, \
             bg: on ? 0x000000 : 0xFFFFFF with anim.linear(1000)] }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();

    let render_r = |interp: &mut Interpreter, now: u32| -> f32 {
        interp.set_now_ms(now);
        let mut frame = byard_core::frame::RenderFrame::new();
        interp.render(&tree, &mut frame, 400.0, 300.0);
        frame.instances()[0].color[0]
    };

    // At rest the target is white; nothing is animating.
    assert!((render_r(&mut interp, 0) - 1.0).abs() < 1e-2);
    assert!(!interp.has_active_animations());

    // Flip toward black: starts near white and is active.
    let sig = interp.var_signal(&Symbol::intern("on")).unwrap();
    interp.write_var(sig, Value::Bool(true));
    interp.tick();
    let start = render_r(&mut interp, 0);
    assert!(start > 0.9, "starts near white, got {start}");
    assert!(interp.has_active_animations());

    // Mid-flight it's a grey between the endpoints, still moving.
    let mid = render_r(&mut interp, 500);
    assert!((0.05..0.95).contains(&mid), "mid-flight grey, got {mid}");

    // Arrives at black and settles (idle again).
    assert!(render_r(&mut interp, 1000) < 1e-2, "arrives black");
    assert!(!interp.has_active_animations());
}

#[test]
fn animation_is_inert_until_the_clock_is_advanced() {
    // A host that never advances the clock must resolve the value to its
    // target and never mark it active, otherwise a wait-based runner would
    // spin forever redrawing a motion pinned at t=0.
    let parsed = parse(
        "View V() { var on: Bool = true \
             Box #[bg: 0x808080, width: 10, height: 10, \
             scale: on ? 2.0 : 1.0 with anim.spring()] }",
    );
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(
        (frame.instances()[0].transform.scale[0] - 2.0).abs() < 1e-6,
        "with no clock the value jumps straight to its target"
    );
    assert!(
        !interp.has_active_animations(),
        "an un-advanced clock must never leave an animation active"
    );
}

#[test]
fn opacity_dims_descendant_text_not_only_the_background() {
    // Regression: a translucent Button dims its *label* too, not just its
    // background, `opacity` folds into the alpha of every primitive the
    // element and its descendants emit.
    let parsed = parse(
        "View V() { var c: Int = 0 \
             Button(\"x\") #[bg: 0x6495ED, opacity: 0.4, width: 100, height: 44] => c++ }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let view = &parsed.views[0];
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(view, &[]);
    interp.tick();
    let mut frame = byard_core::frame::RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let (i, label) = frame
        .texts()
        .iter()
        .enumerate()
        .find(|(_, t)| t.text == "x")
        .expect("the button's label was emitted");
    // Effective alpha: a translucent Button with a label is faded as one
    // picture (RFC-0011 T4), so the label draws opaque inside a group and the
    // 0.4 is the group's. What must hold is that it reaches the screen at 0.4.
    let effective = label.color[3] * frame.composite_opacity(|m| m.text, i);
    assert!(
        (effective - 0.4).abs() < 1e-3,
        "label should reach the screen at the 0.4 opacity, got {effective}"
    );
}
