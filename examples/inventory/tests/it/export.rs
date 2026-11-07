//! Settings: the CSV export, the sample-data reset and the new-product minimum.

use crate::common::{App, Sandbox};

#[test]
fn export_writes_the_catalogue_as_csv_and_says_where() {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Settings");
    app.tap("Export to CSV");
    let path = sb.dir.join(stockroom::controller::EXPORT_FILE);
    let csv = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 61, "a header and sixty products");
    assert_eq!(
        lines[0],
        "id,sku,name,category,qty,min,price,location,updated"
    );
    assert_eq!(
        lines[1], "1,HW-001,Hex bolt M6,Hardware,0,10,7.90,,2026-10-01T09:00",
        "a product with no location leaves it empty"
    );
    assert_eq!(
        lines[2],
        "2,HW-002,Hex bolt M8,Hardware,8,5,34.50,B1,2026-10-01T09:00"
    );
    let said = format!("Wrote 60 products to {}", path.display());
    assert!(app.shows(&said), "{said:?} in {:?}", app.texts());
}

#[test]
fn reset_asks_first_and_puts_the_sample_data_back() {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    // Delete one product so there is something to put back.
    app.tap("Products");
    app.tap("9V battery");
    app.tap("Delete");
    app.tap("Delete");
    assert!(app.shows("59 of 59"), "{:?}", app.texts());

    app.tap("Settings");
    app.tap("Reset to sample data");
    assert!(app.shows("Reset to the sample data?"), "{:?}", app.texts());
    assert!(app.shows("This removes 59 products and their movements, and cannot be undone."));
    // Cancel changes nothing.
    app.tap("Cancel");
    assert!(!app.shows("Reset to the sample data?"));
    app.tap("Products");
    assert!(app.shows("59 of 59"));

    app.tap("Settings");
    app.tap("Reset to sample data");
    app.tap("Reset");
    app.tap("Products");
    assert!(app.shows("60 of 60"), "{:?}", app.texts());
    assert!(sb.saved().contains("EL-013"), "saved");
}

#[test]
fn the_default_minimum_starts_new_products() {
    let sb = Sandbox::new();
    let mut app = App::start(&sb);
    app.tap("Settings");
    app.tap("5");
    app.replace_text("abc");
    assert!(app.shows("A whole number, 0 or more"), "{:?}", app.texts());
    app.replace_text("12");
    app.tap("Save");
    assert!(sb.saved().contains("\"minDefault\": 12"), "{}", sb.saved());
    app.tap("Products");
    app.tap("New product");
    assert!(
        app.shows("12"),
        "the form starts with it: {:?}",
        app.texts()
    );
    drop(app);
    // And it is still 12 on the next run.
    let mut again = App::start(&sb);
    again.tap("Products");
    again.tap("New product");
    assert!(again.shows("12"), "{:?}", again.texts());
}
