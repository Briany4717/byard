//! The frame budget with a catalogue thirty times the sample's (2000 products):
//! opening the list, and typing in the search box. The ceilings are in
//! `ci/budgets.toml` under `[inventory]`, and the test prints what it measured
//! next to them (`--nocapture` shows it).
//!
//! They are far above a 16 ms frame on purpose: that is what the app costs
//! today, and the test keeps it from getting worse. See the gap log.
//!
//! # Why this is its own test binary, and measures in a child process
//!
//! A time measured while the other tests of Stockroom run beside it (they run
//! in parallel, on every core) says how busy the machine was. Cargo runs one
//! test binary at a time, so this one has the machine to itself, and each
//! measurement is made in a process of its own, the way the Aura Weather
//! first-frame budget is (`examples/aura_weather/tests/first_frame.rs`). The
//! ceiling is on the best of [`RUNS`]: a slow run measures what else the
//! machine was doing, and a regression slows all of them.
//!
//! The times are held only on Linux, where the ceilings were measured, and
//! only by the ignored test: CI's budgets step runs it with `--ignored`, and
//! `cargo test -p stockroom --test budgets -- --ignored --nocapture` does it
//! by hand. The work a keystroke does is not a time, so it is held in every
//! run, everywhere.

#[path = "it/common/mod.rs"]
mod common;

use std::time::Duration;

use common::{App, Sandbox};

const PRODUCTS: usize = 2000;

/// How many measurements are made, each in a process of its own.
const RUNS: usize = 3;

/// Set in the child process that makes one measurement.
const CHILD: &str = "STOCKROOM_BUDGET_CHILD";
const REPORT: &str = "stockroom-budget:";

/// The work the list costs with 2000 products, as the interpreter counts it,
/// locked in at today's value so row virtualization can lower it.
///
/// A keystroke creates no signals today, so the first limit is 0 and a rise
/// means it started to. The second is the reactive scopes alive with the list
/// open (about 17 a row, all 2000 rows): the work every keystroke walks and
/// rebuilds, which building only the rows in view should cut by a large
/// factor. When it does, lower the limit in the same change.
const SIGNALS_PER_KEYSTROKE: usize = 0;
const LIVE_SCOPES_WITH_LIST_OPEN: usize = 34440;

struct Measured {
    open_ms: u64,
    key_ms: u64,
    signals: usize,
    scopes: usize,
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// One measurement, in this process.
fn measure() -> Measured {
    let sb = Sandbox::new();
    common::write_store_with(&sb, PRODUCTS);
    let mut app = App::start(&sb);
    assert!(
        app.shows("2000"),
        "the dashboard counts them: {:?}",
        app.texts()
    );

    let open = app.tap_timed("Products");
    assert!(app.shows("2000 of 2000"), "{:?}", app.texts());
    app.settle();
    let live = app.interp.live_scope_count();

    app.tap("Search by name or SKU");
    let mut worst = Duration::ZERO;
    let mut signals = 0;
    for c in "item 0123".chars() {
        let s0 = app.interp.signal_count();
        worst = worst.max(app.keystroke(c));
        // Let the frame's follow-ups finish before the next key.
        for _ in 0..3 {
            app.step(&[]);
        }
        signals = signals.max(app.interp.signal_count() - s0);
    }
    app.settle();
    assert!(app.shows("10 of 2000"), "{:?}", app.starting("1"));
    Measured {
        open_ms: ms(open),
        key_ms: ms(worst),
        signals,
        scopes: live,
    }
}

/// Makes one measurement in a process of its own, as this test binary.
fn measure_in_a_new_process() -> Measured {
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "two_thousand_products_open_and_search_within_budget",
        ])
        .args(["--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .expect("start the test binary again");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some((_, rest)) = line.split_once(REPORT) {
            let n: Vec<u64> = rest
                .split_whitespace()
                .map(|v| v.parse().expect("a number"))
                .collect();
            return Measured {
                open_ms: n[0],
                key_ms: n[1],
                signals: usize::try_from(n[2]).expect("a count"),
                scopes: usize::try_from(n[3]).expect("a count"),
            };
        }
    }
    panic!(
        "a measurement reported nothing ({}):\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The process one timed measurement is made in: this test, run again by
/// itself with [`CHILD`] set. Without it, it does nothing.
#[test]
fn two_thousand_products_open_and_search_within_budget() {
    if std::env::var_os(CHILD).is_some() {
        let m = measure();
        println!(
            "{REPORT}{} {} {} {}",
            m.open_ms, m.key_ms, m.signals, m.scopes
        );
    }
}

/// Work, not time: the same on any machine and under any load, so it is part
/// of every run. One measurement, in this process.
#[test]
fn the_list_does_the_work_it_did() {
    let m = measure();
    check_work(&m);
}

fn check_work(m: &Measured) {
    assert!(
        m.signals == SIGNALS_PER_KEYSTROKE,
        "a keystroke in the search box created {} signals; it used to create \
         {SIGNALS_PER_KEYSTROKE}. Find what it now builds.",
        m.signals
    );
    assert!(
        m.scopes <= LIVE_SCOPES_WITH_LIST_OPEN,
        "{} reactive scopes are alive with {PRODUCTS} products listed, over \
         {LIVE_SCOPES_WITH_LIST_OPEN}: each row costs more than it did. If row \
         virtualization lowered the work, lower the constant instead.",
        m.scopes
    );
}

/// The times. Ignored in the normal run: a time says how busy the machine was
/// as much as what the app costs, and under a loaded runner it failed a
/// change that touched nothing here (411 ms against a 400 ms ceiling). The CI
/// budgets step on Linux runs it by itself, `-- --ignored`, where the machine
/// is quiet and the ceilings were measured.
#[test]
#[ignore = "timed, and a time flakes when the machine is busy; CI's budgets step runs it with --ignored"]
fn two_thousand_products_open_and_search_within_time_budget() {
    let open_ceiling = common::ceiling("inventory", "products_open_ms");
    let key_ceiling = common::ceiling("inventory", "search_keystroke_ms");

    let runs: Vec<Measured> = (0..RUNS).map(|_| measure_in_a_new_process()).collect();
    let best_open = runs.iter().map(|m| m.open_ms).min().expect("a run");
    let best_key = runs.iter().map(|m| m.key_ms).min().expect("a run");
    let all = |f: fn(&Measured) -> u64| {
        runs.iter()
            .map(|m| format!("{} ms", f(m)))
            .collect::<Vec<_>>()
            .join(", ")
    };

    // Timing is only enforced where the ceilings were measured. Another system
    // has its own graphics stack and its own runner, and a ceiling nobody has
    // measured there is a guess that would flake.
    let enforced = cfg!(target_os = "linux");
    eprintln!(
        "{PRODUCTS} products: opening the list {best_open} ms best of {RUNS} ({}), ceiling \
         {open_ceiling} ms; worst keystroke {best_key} ms best of {RUNS} ({}), ceiling \
         {key_ceiling} ms{}",
        all(|m| m.open_ms),
        all(|m| m.key_ms),
        if enforced {
            ""
        } else {
            " (not enforced on this system)"
        }
    );
    eprintln!(
        "a keystroke creates {} signals (limit {SIGNALS_PER_KEYSTROKE}); {} scopes alive with the \
         list open (limit {LIVE_SCOPES_WITH_LIST_OPEN})",
        runs[0].signals, runs[0].scopes
    );
    for m in &runs {
        check_work(m);
    }
    assert!(
        !enforced || best_open <= open_ceiling,
        "opening the list took {best_open} ms at best (every run: {}), over [inventory] \
         products_open_ms = {open_ceiling}",
        all(|m| m.open_ms)
    );
    assert!(
        !enforced || best_key <= key_ceiling,
        "a keystroke took {best_key} ms at best (every run: {}), over [inventory] \
         search_keystroke_ms = {key_ceiling}",
        all(|m| m.key_ms)
    );
}
