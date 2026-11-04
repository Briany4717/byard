//! Performance benchmarks for text measurement.
//!
//! Run with `cargo bench -p byard-core --bench text`.
//!
//! Layout asks `TextMeasurer` for the size of every `Text` and `Button` on
//! every tick, and almost every answer is already in its cache, so what a
//! *hit* costs is what a steady-state frame pays per label. A *miss* is what a
//! counter, a clock or a streamed line pays each time its string changes.

#![allow(clippy::cast_precision_loss)]

use std::hint::black_box;
use std::time::Instant;

use byard_core::text::TextMeasurer;

fn bench(name: &str, iters: u64, mut f: impl FnMut(u64)) {
    for i in 0..iters.min(1000) {
        f(i);
    }
    let start = Instant::now();
    for i in 0..iters {
        f(i);
    }
    let per_op = start.elapsed().as_nanos() as f64 / iters as f64;
    println!("{name:55} {per_op:>10.2} ns/op   ({iters} iters)");
}

fn main() {
    println!("\n── text measurement ──\n");
    let mut m = TextMeasurer::new();

    let _ = m.measure("Save changes", 16.0, 400, None);
    bench("text: measure, cache hit (12 chars)", 1_000_000, |_| {
        black_box(m.measure(black_box("Save changes"), 16.0, 400, None));
    });

    let long =
        "The quick brown fox jumps over the lazy dog, again and again, until the line has to wrap.";
    let _ = m.measure_wrapped(long, 14.0, Some(240.0), 400, Some("Inter"));
    bench(
        "text: measure, cache hit (90 chars, wrapped, family)",
        1_000_000,
        |_| {
            black_box(m.measure_wrapped(
                black_box(long),
                14.0,
                Some(240.0),
                400,
                Some(black_box("Inter")),
            ));
        },
    );

    // The labels of a screen, measured again each tick: all hits.
    let labels: Vec<String> = (0..200).map(|i| format!("Row {i} label")).collect();
    for l in &labels {
        let _ = m.measure(l, 14.0, 400, None);
    }
    bench("text: a 200-label tick, all hits", 20_000, |_| {
        for l in &labels {
            black_box(m.measure(l, 14.0, 400, None));
        }
    });

    // A string that is new every time: a counter, a clock, a progress line.
    bench(
        "text: measure, cache miss (a new string each time)",
        20_000,
        |i| {
            black_box(m.measure(&format!("Downloaded {i} of 99999 bytes"), 14.0, 400, None));
        },
    );
    println!();
}
