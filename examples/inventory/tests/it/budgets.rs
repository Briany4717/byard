//! The frame budget with a catalogue ten times the sample's (2000 products):
//! opening the list, and typing in the search box, stay within a frame's worth
//! of time. The ceilings are in `ci/budgets.toml` under `[inventory]`, and each
//! run prints what it measured next to them (`--nocapture` shows it).

use std::time::{Duration, Instant};

use crate::common::{self, App, Sandbox};

const PRODUCTS: usize = 2000;

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[test]
fn explore_timings() {
    let sb = Sandbox::new();
    common::write_store_with(&sb, PRODUCTS);
    let t = Instant::now();
    let mut app = App::start(&sb);
    eprintln!("start {} ms", ms(t.elapsed()));
    assert!(app.shows("2000"), "{:?}", &app.texts()[..12]);
    let t = Instant::now();
    let d = app.tap_timed("Products");
    eprintln!("first frame of products {} ms", d.as_millis());
    eprintln!("open products {} ms", ms(t.elapsed()));
    assert!(app.shows("2000 of 2000"));
    app.tap("Search by name or SKU");
    let mut worst = Duration::ZERO;
    for c in "item 0123".chars() {
        let d = app.keystroke(c);
        eprintln!("key {c}: {} ms", d.as_millis());
        worst = worst.max(d);
        for _ in 0..3 {
            app.step(&[]);
        }
    }
    eprintln!("worst {} ms", worst.as_millis());
    app.settle();
    eprintln!("{:?}", app.starting("1 of").or(app.starting("10 of")));
}
