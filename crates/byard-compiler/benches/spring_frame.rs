#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
//! What a spring costs the logic thread, and so what evaluating springs on the
//! GPU could save (RFC-0010).
//!
//! Run with `cargo bench -p byard-compiler --bench spring_frame`.
//!
//! Two numbers. The first is the closed-form spring itself, per sample. The
//! second is a whole logic frame (`tick` + `render`) of a scene with N boxes
//! whose springs never settle, against the same scene with the same props and
//! no animation. The difference is the *upper bound* on a GPU move: moving the
//! evaluation into the vertex shader would remove the sampling, and at best the
//! re-render that carries each sampled value to the frame, but nothing else.

use std::hint::black_box;
use std::time::Instant;

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::{Motion, MotionCurve, RenderFrame};

/// `n` boxes, each carrying `prop`, in rows of 40.
fn scene(n: usize, prop: &str) -> String {
    let mut s = String::from("View Main() {\n    Column #[gap: 2] {\n");
    for row in 0..n.div_ceil(40) {
        s.push_str("        Row #[gap: 2] {\n");
        for _ in 0..40.min(n - row * 40) {
            s.push_str("            Box #[width: 8, height: 8, ");
            s.push_str(prop);
            s.push_str("] {}\n");
        }
        s.push_str("        }\n");
    }
    s.push_str("    }\n}\n");
    s
}

/// Mean microseconds per logic frame over `frames` frames at 60 Hz, and
/// whether the painted boxes moved between the last two frames.
fn frame_us(src: &str, frames: u32) -> (f64, bool) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = RenderFrame::new();
    let mut now = 0_u32;
    let mut step = |interp: &mut Interpreter, frame: &mut RenderFrame| {
        now += 16;
        interp.set_now_ms(now);
        interp.tick();
        frame.clear();
        interp.render(&tree, frame, 1280.0, 800.0);
        black_box(frame.instances().len());
    };
    for _ in 0..30 {
        step(&mut interp, &mut frame);
    }
    let start = Instant::now();
    for _ in 0..frames {
        step(&mut interp, &mut frame);
    }
    let us = start.elapsed().as_secs_f64() * 1e6 / f64::from(frames);
    let before = format!("{:?}", frame.instances());
    step(&mut interp, &mut frame);
    (us, before != format!("{:?}", frame.instances()))
}

fn main() {
    // The closed form alone: an under-damped spring, sampled across its flight.
    let m = Motion {
        from: 0.0,
        to: 40.0,
        start_ms: 0,
        curve: MotionCurve {
            kind: MotionCurve::SPRING,
            params: [170.0, 0.7, 0.0],
        },
    };
    let samples = 10_000_000_u32;
    let start = Instant::now();
    let mut acc = 0.0_f32;
    for i in 0..samples {
        acc += black_box(&m).sample(black_box(i % 2_000));
    }
    black_box(acc);
    let ns = start.elapsed().as_nanos() as f64 / f64::from(samples);
    println!(
        "{:52} {ns:>8.2} ns/sample",
        "Motion::sample (spring closed form)"
    );

    println!();
    println!(
        "{:>6}  {:>12}  {:>12}  {:>12}  {:>12}",
        "boxes", "static us", "spring us", "colour us", "per spring ns"
    );
    for n in [100, 500, 1000, 2000] {
        let (still, still_moved) = frame_us(&scene(n, "bg: 0x6750A4, translate: (40, 0)"), 200);
        let (moving, moved) = frame_us(
            &scene(
                n,
                "bg: 0x6750A4, translate: (40, 0) with anim.spring(from: 0, repeat: infinite, reverse: true)",
            ),
            200,
        );
        let (colour, recoloured) = frame_us(
            &scene(
                n,
                "bg: 0x6750A4 with anim.spring(from: 0xEF5350, repeat: infinite, reverse: true)",
            ),
            200,
        );
        // The measurement means nothing unless the springs were in flight
        // for the whole run and the static scene really was static.
        assert!(moved && recoloured && !still_moved, "n={n}");
        let per = (moving - still) * 1000.0 / n as f64;
        println!("{n:>6}  {still:>12.1}  {moving:>12.1}  {colour:>12.1}  {per:>12.1}");
    }
}
