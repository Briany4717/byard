//! A text measurement that is already cached allocates nothing.
//!
//! Layout asks `TextMeasurer` for the size of every `Text` and `Button` on
//! every tick, and nearly every answer is one it already has. Looking one up
//! used to build a `String` and an `Arc<str>` for the key first, two heap
//! allocations per label per tick before the cache said "yes". The key is
//! borrowed now, and this file is what fails if that stops being true.
//!
//! # Why this is its own test binary
//!
//! Counting allocations takes a `#[global_allocator]`, and one allocator is
//! one per binary. Installing it in `tests/it` would put every integration
//! test of the crate behind a counter. The count is per thread, so the other
//! tests of this file running in parallel do not bill each other.
//!
//! Run `cargo test -p byard-core --test text_measure_alloc -- --nocapture` to
//! see the counts and the time a hit takes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::time::Instant;

use byard_core::text::TextMeasurer;

/// Text, size, wrap width, weight and family: one question to the measurer.
type Question<'a> = (&'a str, f32, Option<f32>, u16, Option<&'a str>);

const ROUNDS: usize = 10_000;

/// A `System` allocator that counts allocations on a thread while armed.
///
/// SAFETY: a thin pass-through wrapper around `System`. Every method forwards
/// its arguments unchanged to the corresponding `System` method, which is a
/// valid `GlobalAlloc`; the only addition is a counter bump. Same shape, and
/// the same justification, as the counting allocator in `byard-platform`'s
/// `frame_budget.rs`. Test-only.
#[allow(unsafe_code)]
struct CountingAllocator;

thread_local! {
    /// `const` initialisers keep the TLS non-lazy, so touching it from inside
    /// the allocator cannot itself allocate.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// `try_with` rather than `with`: during thread teardown the TLS slot is
/// gone, and a destructor that allocates must not panic the process.
fn note_allocation() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
    }
}

#[allow(unsafe_code)]
// SAFETY: see `CountingAllocator`'s doc comment.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: forwarding an unchanged `layout` to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` above with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        // SAFETY: same contract as `dealloc`, plus a non-zero `new_size`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

/// Runs `f` with counting armed and returns how many allocations it made.
fn count_allocations<R>(f: impl FnOnce() -> R) -> (R, usize) {
    ALLOCATIONS.set(0);
    COUNTING.set(true);
    let out = f();
    COUNTING.set(false);
    (out, ALLOCATIONS.get())
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn a_cache_hit_allocates_nothing() {
    let long = "the quick brown fox jumps over the lazy dog again and again";
    let mut m = TextMeasurer::new();
    // Every shape of question layout asks: plain, wrapped, weighted, with a
    // family, and the empty string.
    let questions: [Question<'static>; 6] = [
        ("Save changes", 16.0, None, 400, None),
        (long, 14.0, Some(120.0), 400, None),
        (long, 14.0, Some(240.0), 700, Some("Inter")),
        ("", 16.0, None, 400, None),
        ("日本語 🙂", 18.0, None, 400, None),
        ("Aura Weather", 28.0, None, 400, Some("Space Grotesk")),
    ];
    let asked = |m: &mut TextMeasurer| {
        for &(text, size, wrap, weight, family) in &questions {
            black_box(m.measure_wrapped(black_box(text), size, wrap, weight, family));
        }
    };

    // The first round fills the cache; it must allocate, or this test is not
    // counting anything.
    let ((), cold) = count_allocations(|| asked(&mut m));
    assert!(
        cold > 0,
        "shaping and filing six answers allocated nothing?"
    );

    let ((), hot) = count_allocations(|| {
        for _ in 0..ROUNDS {
            asked(&mut m);
        }
    });
    assert_eq!(
        hot,
        0,
        "{} cache hits made {hot} heap allocations",
        ROUNDS * questions.len()
    );

    // What a hit costs, printed next to the count. Not asserted: a timing on a
    // shared machine says how busy it was.
    let start = Instant::now();
    for _ in 0..ROUNDS {
        asked(&mut m);
    }
    let per_hit = start.elapsed().as_secs_f64() * 1e9 / (ROUNDS * questions.len()) as f64;
    println!("cold round: {cold} allocations; {ROUNDS} hot rounds: {hot}; {per_hit:.0} ns per hit");
}
