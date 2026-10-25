//! The idle-wakes budget (RFC-0046 section 1): an app with nothing animating
//! and nothing in flight does not wake its logic thread.
//!
//! The ceiling is `[idle] logic_wakes_per_second` in `ci/budgets.toml`, and
//! the test prints what it counted next to it every run (`--nocapture` shows
//! it). Like the frame budget in `byard-platform`, a ceiling may be lowered by
//! any change that improves things and raised only by one that says in its
//! description the old value, the new one and why. The other budget measured
//! with this app, the time to the first frame, is in `tests/first_frame.rs`.

use crate::common;

use std::time::{Duration, Instant};

use common::{App, Server};

const FORECAST: &str = include_str!("../fixtures/forecast.json");

// ── Idle wakes ─────────────────────────────────────────────────────────────

/// How long an idle app is watched, in real time: the claim is about the
/// reactor, and a fake clock would say nothing about it.
const WATCH: Duration = Duration::from_secs(1);

/// Counts the wakes of `app` over [`WATCH`], polling as the logic thread's
/// loop would.
fn wakes_over_a_second(app: &mut App) -> usize {
    let start = Instant::now();
    let mut wakes = 0;
    while start.elapsed() < WATCH {
        wakes += app.wakes();
        std::thread::sleep(Duration::from_millis(5));
    }
    wakes
}

#[test]
fn an_idle_app_does_not_wake_the_logic_thread() {
    let ceiling = common::ceiling("idle", "logic_wakes_per_second");

    let server = Server::serve("200 OK", FORECAST);
    let mut app = App::start(&server);
    // Past its first frames and its first data: the forecast has arrived and
    // the screen has settled.
    app.answer_all();
    app.settle();

    // The forecast, then the map, which is the screen with the most going on
    // (its tiles, its shader).
    let mut worst = 0;
    for tab in ["Now", "Map"] {
        if tab != "Now" {
            app.tap(tab);
            app.answer_all();
            app.settle();
        }
        let wakes = wakes_over_a_second(&mut app);
        eprintln!(
            "idle wakes, {tab} screen: {wakes} in {} s, ceiling {ceiling} a second",
            WATCH.as_secs()
        );
        worst = worst.max(wakes);
    }

    assert!(
        u64::try_from(worst).unwrap_or(u64::MAX) <= ceiling,
        "an idle Aura Weather woke the logic thread {worst} times in a second, over the \
         ceiling of {ceiling} in ci/budgets.toml [idle]. Nothing was animating and nothing \
         was in flight, so something is ticking or sending results that nobody asked for \
         (#336 was the logic thread ticking about 2,400 times a second in a hidden window). \
         Find what wakes it. Raise the ceiling only for a wake that is the point of a \
         feature, and say so in the PR."
    );
}
