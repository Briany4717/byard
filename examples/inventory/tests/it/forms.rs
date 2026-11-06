//! The product form: what it checks, the order Tab goes in, saving, and the
//! store keeping what was saved.

use crate::common::{App, Sandbox};

fn new_form() -> (Sandbox, App) {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Products");
    app.tap("New product");
    assert!(app.shows("Create product"), "{:?}", app.texts());
    (sb, app)
}

#[test]
fn nothing_is_wrong_until_a_field_is_touched_and_save_waits() {
    let (sb, mut app) = new_form();
    // The form opens with its defaults: quantity 0, minimum from Settings.
    assert!(app.shows("5"), "the default minimum: {:?}", app.texts());
    assert!(!app.shows("Required"), "{:?}", app.texts());
    // Save does nothing while name, SKU and price are empty.
    app.tap("Create product");
    assert!(app.shows("New product") && app.shows("Create product"));
    assert!(!sb.saved().contains("Created"), "nothing saved");
    assert_eq!(app.starting("61 of"), None);
}

#[test]
fn each_field_says_what_is_wrong() {
    let (_sb, mut app) = new_form();

    // A SKU that exists, in any case.
    app.tap("HW-061");
    app.type_text("hw-001");
    assert!(
        app.shows("Already used by another product"),
        "{:?}",
        app.texts()
    );

    // Leaving an empty name says it is required.
    app.tap("Hex bolt M10");
    app.tap("0");
    assert!(app.shows("Required"), "{:?}", app.texts());

    // The quantity: -1 and abc are refused, 12 is fine.
    app.replace_text("-1");
    assert!(app.shows("A whole number, 0 or more"), "{:?}", app.texts());
    app.replace_text("abc");
    assert!(app.shows("A whole number, 0 or more"));
    assert!(
        app.shows("abc"),
        "the whole word was typed: {:?}",
        app.texts()
    );
    app.replace_text("12");
    assert!(!app.shows("A whole number, 0 or more"), "{:?}", app.texts());

    // The price: letters, a bare point and a trailing point are refused.
    app.tap("0.00");
    app.type_text("abc");
    assert!(app.shows("A price, 0 or more"), "{:?}", app.texts());
    app.replace_text("1.2.3");
    assert!(app.shows("A price, 0 or more"));
    app.replace_text("-1");
    assert!(app.shows("A price, 0 or more"));
    // ".5" is half, "4." is four: the engine reads them.
    app.replace_text(".5");
    assert!(!app.shows("A price, 0 or more"), "{:?}", app.texts());
    app.replace_text("4.50");
    assert!(!app.shows("A price, 0 or more"), "{:?}", app.texts());
    // A number too large to hold is not one either: no wrapping.
    app.replace_text("99999999999999999999");
    assert!(app.shows("A price, 0 or more"), "{:?}", app.texts());
}

#[test]
fn tab_goes_through_the_form_in_the_order_it_is_written() {
    let (_sb, mut app) = new_form();
    app.tap("HW-061");
    app.type_text("ZZ-1");
    app.key("Tab");
    app.type_text("Washer");
    // Six category chips are buttons, and Tab visits each of them.
    for _ in 0..7 {
        app.key("Tab");
    }
    app.replace_text("12");
    app.key("Tab");
    app.replace_text("3");
    app.key("Tab");
    app.type_text("4.5");
    app.key("Tab");
    app.type_text("Shelf Q");
    let order = app.order_of(&["ZZ-1", "Washer", "12", "3", "4.5", "Shelf Q"]);
    assert_eq!(
        order,
        ["ZZ-1", "Washer", "12", "3", "4.5", "Shelf Q"],
        "{:?}",
        app.texts()
    );
}

/// Fills a valid form, with `location` last, and leaves the caret in it.
fn fill(app: &mut App, sku: &str, name: &str) {
    app.tap("HW-061");
    app.type_text(sku);
    app.key("Tab");
    app.type_text(name);
    for _ in 0..7 {
        app.key("Tab");
    }
    app.replace_text("7");
    app.key("Tab");
    app.key("Tab");
    app.type_text("2.5");
    app.key("Tab");
    app.type_text("D4");
}

#[test]
fn enter_in_the_last_field_saves_and_back_does_not_return_to_the_form() {
    let (sb, mut app) = new_form();
    fill(&mut app, "NEW-001", "Brass hinge");
    app.key("Enter");
    // The product's page, not the form.
    assert!(!app.shows("Create product"), "{:?}", app.texts());
    assert!(
        app.shows("Brass hinge") && app.shows("NEW-001") && app.shows("$2.50"),
        "{:?}",
        app.texts()
    );
    assert!(app.shows("D4") && app.shows("5 Nov 2026, 10:30"));
    // Opening stock was logged as a movement.
    assert!(
        app.shows("+7") && app.shows("Opening stock"),
        "{:?}",
        app.texts()
    );
    assert!(sb.saved().contains("NEW-001"), "saved to the store");
    // Back goes to the list, one step, and the form is not in the way.
    app.tap("< Products");
    assert!(app.shows("61 of 61"), "{:?}", app.texts());
}

#[test]
fn a_second_run_loads_what_the_first_saved() {
    let (sb, mut app) = new_form();
    fill(&mut app, "NEW-002", "Copper pipe");
    app.tap("Create product");
    assert!(app.shows("Copper pipe"));
    drop(app);

    let mut again = App::start(&sb);
    again.tap("Products");
    assert!(again.shows("61 of 61"), "{:?}", again.texts());
    again.tap("Search by name or SKU");
    again.type_text("copper");
    assert!(
        again.shows("1 of 61") && again.shows("Copper pipe"),
        "{:?}",
        again.texts()
    );
    // The dashboard counts it too.
    again.tap("Dashboard");
    assert!(again.shows("61"), "{:?}", again.texts());
}

#[test]
fn editing_keeps_the_quantity_and_allows_the_products_own_sku() {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Products");
    app.tap("9V battery");
    app.tap("Edit");
    assert!(app.shows("Edit product"), "{:?}", app.texts());
    // Its own SKU is not a duplicate; the form is valid as it opens.
    assert!(!app.shows("Already used by another product"));
    // The name field holds "9V battery".
    app.tap("9V battery");
    app.replace_text("9V alkaline battery");
    app.tap("Save changes");
    assert!(app.shows("9V alkaline battery"), "{:?}", app.texts());
    assert!(app.shows("29"), "the quantity is untouched");
    assert!(!app.shows("Edit product"));
    // Back is the list, not the form.
    app.tap("< Products");
    assert!(app.shows("60 of 60"), "{:?}", app.texts());
}
