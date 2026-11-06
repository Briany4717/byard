//! The first frame, and the dashboard's way into a product.

use crate::common::{App, Sandbox};

#[test]
fn the_first_frame_shows_the_numbers_of_the_sample_data() {
    let sb = Sandbox::new();
    let app = App::start(&sb);
    // Sixty products, 4,444 units worth $193,440.70, eight low, seven out.
    for text in ["60", "4444", "$193,440.70", "8", "7"] {
        assert!(app.shows(text), "{text} in {:?}", app.texts());
    }
    assert!(app.shows("Stock value by category"));
    assert!(app.shows("Packaging"));
    assert!(
        app.shows("$70,988.80"),
        "the biggest bar: {:?}",
        app.texts()
    );
    // The three movements of the sample, newest first.
    assert_eq!(
        app.order_of(&["Hex bolt M8", "Hex bolt M6", "Flat washer M6"]),
        ["Hex bolt M8", "Hex bolt M6", "Flat washer M6"]
    );
}

#[test]
fn the_first_run_saves_the_sample_data() {
    let sb = Sandbox::new();
    let _app = App::start(&sb);
    let saved = sb.saved();
    assert!(saved.contains("HW-001"), "{}", saved.len());
    assert!(saved.contains("\"seeded\""));
}

#[test]
fn a_product_that_needs_attention_opens_from_the_dashboard() {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Claw hammer");
    // The tab changed and the product's page is up.
    assert!(app.shows("Claw hammer"), "{:?}", app.texts());
    assert!(app.shows("On hand"), "{:?}", app.texts());
    assert!(app.shows("Out"), "{:?}", app.texts());
    assert!(app.shows("Receive") && app.shows("Ship") && app.shows("Adjust"));
}
