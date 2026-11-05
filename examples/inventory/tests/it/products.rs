//! The product list: search, category, low stock and sorting.

use crate::common::{App, Sandbox};

fn products() -> (Sandbox, App) {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Products");
    (sb, app)
}

fn backspace(app: &mut App, n: usize) {
    for _ in 0..n {
        app.key("Backspace");
    }
}

#[test]
fn search_finds_by_name_and_by_sku() {
    let (_sb, mut app) = products();
    assert!(app.shows("60 of 60"));
    app.tap("Search by name or SKU");
    app.type_text("bolt");
    assert!(app.shows("2 of 60"), "{:?}", app.texts());
    assert!(app.shows("Hex bolt M6") && app.shows("Hex bolt M8"));
    assert!(!app.shows("Cabinet knob"));

    backspace(&mut app, 4);
    assert!(app.shows("60 of 60"), "{:?}", app.texts());
    // By SKU, in any case: the nine Packaging products PK-051 to PK-059 match.
    app.type_text("pk-05");
    assert!(app.shows("9 of 60"), "{:?}", app.texts());
    assert!(app.shows("Packing tape") || app.shows("Pallet strap"), "{:?}", app.texts());

    backspace(&mut app, 5);
    app.type_text("nothing like this");
    assert!(app.shows("0 of 60"));
    assert!(app.shows("No product matches"));
}

#[test]
fn a_category_chip_narrows_the_list() {
    let (_sb, mut app) = products();
    app.tap_first("Safety");
    assert!(app.shows("10 of 60"), "{:?}", app.texts());
    assert!(!app.shows("Cabinet knob"));
    app.tap_first("All");
    assert!(app.shows("60 of 60"));
}

#[test]
fn low_stock_only_keeps_what_is_low_or_out() {
    let (_sb, mut app) = products();
    // The switch is the control; the words beside it are only words.
    let (x, y) = app.find("Low stock only");
    app.tap_at((x - 30.0, y));
    // Eight low and seven out.
    assert!(app.shows("15 of 60"), "{:?}", app.texts());
    assert!(!app.shows("Cabinet knob"));
    app.tap_at((x - 30.0, y));
    assert!(app.shows("60 of 60"));
}

#[test]
fn the_headings_sort() {
    let (_sb, mut app) = products();
    app.tap_first("Hardware");
    // By name to start with.
    assert_eq!(
        app.order_of(&["Cabinet knob", "Anchor plug 8mm"]),
        ["Anchor plug 8mm", "Cabinet knob"]
    );
    // By quantity: 0, then 117, then 210.
    app.tap("Quantity");
    assert_eq!(
        app.order_of(&["Hex bolt M6", "Anchor plug 8mm", "Cabinet knob"]),
        ["Hex bolt M6", "Anchor plug 8mm", "Cabinet knob"]
    );
    assert!(app.shows("Quantity ^"));
    // Again turns it round.
    app.tap("Quantity ^");
    assert_eq!(
        app.order_of(&["Hex bolt M6", "Anchor plug 8mm", "Cabinet knob"]),
        ["Cabinet knob", "Anchor plug 8mm", "Hex bolt M6"]
    );
    assert!(app.shows("Quantity v"));
    // By name, back to the start.
    app.tap("Name");
    assert_eq!(
        app.order_of(&["Cabinet knob", "Anchor plug 8mm"]),
        ["Anchor plug 8mm", "Cabinet knob"]
    );
}

#[test]
fn a_row_opens_the_product_and_back_returns_to_the_list() {
    let (_sb, mut app) = products();
    app.tap("9V battery");
    assert!(app.shows("EL-013") && app.shows("Electronics"), "{:?}", app.texts());
    app.tap("< Products");
    assert!(app.shows("60 of 60"), "{:?}", app.texts());
}
