//! Receive, ship, adjust and delete.

use crate::common::{App, Sandbox};

/// The page of "9V battery": 29 on hand, minimum 10.
fn battery() -> (Sandbox, App) {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Products");
    app.tap("9V battery");
    assert!(app.shows("On hand"), "{:?}", app.texts());
    (sb, app)
}

fn on_hand(app: &App) -> String {
    // The fact is "On hand", then its value.
    let texts = app.texts();
    let at = texts.iter().position(|t| t == "On hand").expect("On hand");
    texts[at + 1].clone()
}

#[test]
fn receiving_adds_to_the_quantity_and_the_log() {
    let (_sb, mut app) = battery();
    app.tap("Receive");
    assert!(app.shows("Receive 9V battery"), "{:?}", app.texts());
    app.tap("0");
    app.type_text("6");
    app.tap("Order number, delivery, reason");
    app.type_text("Delivery 77");
    app.tap("Confirm");
    assert!(!app.shows("Receive 9V battery"), "the dialog closes");
    assert_eq!(on_hand(&app), "35");
    // The movement is on the product's page, stamped by the clock.
    assert!(app.shows("+6") && app.shows("Received"), "{:?}", app.texts());
    assert!(app.shows("Delivery 77"));
    assert!(app.shows("5 Nov 2026, 10:30"), "{:?}", app.texts());
}

#[test]
fn shipping_takes_from_the_quantity_and_shows_in_the_log() {
    let (sb, mut app) = battery();
    app.tap("Ship");
    app.tap("0");
    app.type_text("29");
    // Everything on hand is allowed; that makes it out of stock.
    app.tap("Confirm");
    assert_eq!(on_hand(&app), "0");
    assert!(app.shows("-29") && app.shows("Shipped"), "{:?}", app.texts());
    assert!(app.shows("Out"), "the badge: {:?}", app.texts());
    assert!(sb.saved().contains("\"kind\": \"out\""), "saved");
}

#[test]
fn shipping_more_than_is_on_hand_is_refused() {
    let (_sb, mut app) = battery();
    app.tap("Ship");
    app.tap("0");
    app.type_text("30");
    assert!(app.shows("Only 29 on hand"), "{:?}", app.texts());
    // Confirm does nothing while the quantity is wrong.
    app.tap("Confirm");
    assert!(app.shows("Ship 9V battery"), "the dialog stays: {:?}", app.texts());
    assert_eq!(on_hand(&app), "29");
    assert!(!app.shows("Shipped"));
    // A smaller number is fine.
    app.tap("30");
    app.replace_text("3");
    assert!(!app.shows("Only 29 on hand"));
    app.tap("Confirm");
    assert_eq!(on_hand(&app), "26");
}

#[test]
fn a_quantity_must_be_a_whole_number() {
    let (_sb, mut app) = battery();
    app.tap("Receive");
    app.tap("0");
    app.type_text("abc");
    assert!(app.shows("Use a whole number, 0 or more"), "{:?}", app.texts());
    app.key("Backspace");
    app.key("Backspace");
    app.key("Backspace");
    app.type_text("-1");
    assert!(app.shows("Use a whole number, 0 or more"), "{:?}", app.texts());
}

#[test]
fn adjusting_sets_the_count_and_records_the_difference() {
    let (_sb, mut app) = battery();
    app.tap("Adjust");
    // The new count starts as what is on hand.
    assert!(app.shows("That is what is on hand already"), "{:?}", app.texts());
    app.tap("29");
    app.replace_text("20");
    app.tap("Confirm");
    assert_eq!(on_hand(&app), "20");
    assert!(app.shows("-9") && app.shows("Adjusted"), "{:?}", app.texts());
}

#[test]
fn a_dialog_blocks_the_page_under_it() {
    let (_sb, mut app) = battery();
    app.tap("Ship");
    // "Edit" is still drawn under the dimmed page; pressing where it is does
    // not reach it.
    let at = app.find("Edit");
    app.tap_at(at);
    assert!(!app.shows("Edit product"), "{:?}", app.texts());
    // The press on the dimmed part closed the dialog, as `dismiss` says; a
    // second press now reaches the page.
    assert!(!app.shows("Ship 9V battery"), "{:?}", app.texts());
}

#[test]
fn escape_closes_a_dialog() {
    let (_sb, mut app) = battery();
    app.tap("Ship");
    assert!(app.shows("Ship 9V battery"));
    app.key("Escape");
    assert!(!app.shows("Ship 9V battery"), "{:?}", app.texts());
}

#[test]
fn delete_asks_first_then_removes_and_keeps_the_movements_readable() {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Products");
    app.tap("Search by name or SKU");
    app.type_text("Hex bolt M8");
    app.tap("Hex bolt M8");
    assert!(app.shows("HW-002"), "{:?}", app.texts());
    // Asks first, and says what stays.
    app.tap("Delete");
    assert!(app.shows("Delete Hex bolt M8?"), "{:?}", app.texts());
    assert!(app.shows("Its 1 movements stay in the log."));
    assert!(sb.saved().contains("HW-002"), "nothing is deleted yet");
    // Keep it: nothing changed.
    app.tap("Keep it");
    assert!(!app.shows("Delete Hex bolt M8?"));
    assert!(app.shows("HW-002"));
    // Now delete: the page is left for the list, and the product is gone.
    app.tap("Delete");
    app.tap("Delete");
    assert!(app.shows("59 of 59") || app.shows("0 of 59"), "{:?}", app.texts());
    assert!(!sb.saved().contains("HW-002"), "removed from the store");
    // The log still reads.
    app.tap("Movements");
    assert!(app.shows("Deleted product #2"), "{:?}", app.texts());
    assert!(app.shows("Order 1042"));
    assert!(app.shows("3 of 3"));
}
