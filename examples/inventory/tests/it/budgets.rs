//! The frame budget with a catalogue thirty times the sample's (2000 products):
//! opening the list, and typing in the search box. The ceilings are in
//! `ci/budgets.toml` under `[inventory]`, and the test prints what it measured
//! next to them (`--nocapture` shows it).
//!
//! They are far above a 16 ms frame on purpose: that is what the app costs
//! today, and the test keeps it from getting worse. See the gap log.

use std::time::Duration;

use crate::common::{self, App, Sandbox};

const PRODUCTS: usize = 2000;

#[test]
fn two_thousand_products_open_and_search_within_budget() {
    let open_ceiling = common::ceiling("inventory", "products_open_ms");
    let key_ceiling = common::ceiling("inventory", "search_keystroke_ms");

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

    app.tap("Search by name or SKU");
    let mut worst = Duration::ZERO;
    for c in "item 0123".chars() {
        worst = worst.max(app.keystroke(c));
        // Let the frame's follow-ups finish before the next key.
        for _ in 0..3 {
            app.step(&[]);
        }
    }
    app.settle();
    assert!(app.shows("10 of 2000"), "{:?}", app.starting("1"));

    eprintln!(
        "{PRODUCTS} products: opening the list {} ms (ceiling {open_ceiling}), \
         worst keystroke {} ms (ceiling {key_ceiling})",
        open.as_millis(),
        worst.as_millis()
    );
    assert!(
        u64::try_from(open.as_millis()).unwrap_or(u64::MAX) <= open_ceiling,
        "opening the list took {} ms, over [inventory] products_open_ms = {open_ceiling}",
        open.as_millis()
    );
    assert!(
        u64::try_from(worst.as_millis()).unwrap_or(u64::MAX) <= key_ceiling,
        "a keystroke took {} ms, over [inventory] search_keystroke_ms = {key_ceiling}",
        worst.as_millis()
    );
}
