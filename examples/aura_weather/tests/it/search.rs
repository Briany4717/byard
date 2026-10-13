//! Location search: typing in the field opens the places found, hanging from
//! the field, and picking one moves every screen there.

use crate::common;

use common::{App, Server};

const FORECAST: &str = include_str!("../fixtures/forecast.json");
const LISBON: &str = include_str!("../fixtures/geocode.json");
const LIMA: &str = include_str!("../fixtures/geocode_lima.json");
const NOTHING: &str = include_str!("../fixtures/geocode_none.json");

/// The search field's height, as `main.byd` writes it.
const FIELD_H: f32 = 44.0;
/// The gap `main.byd` leaves between the field and its results.
const GAP: f32 = 6.0;

fn server(places: &'static str) -> Server {
    Server::routes(vec![
        ("/v1/search", "200 OK", places),
        ("", "200 OK", FORECAST),
    ])
}

/// Starts the app, lets the forecast arrive, and puts the caret in the field.
fn searching(server: &Server) -> App {
    let mut app = App::start(server);
    app.answer();
    app.tap("Search for a city");
    app
}

/// The field and the panel under it, as painted. The field is the box of the
/// field's height spanning the window less its margins; the panel is the
/// other box exactly as wide.
fn field_and_panel(app: &App, window: f32) -> ([f32; 4], [f32; 4]) {
    let boxes = app.boxes();
    let wide = |b: &&[f32; 4]| (b[2] - (window - 48.0)).abs() < 0.5;
    let field = *boxes
        .iter()
        .filter(wide)
        .find(|b| (b[3] - FIELD_H).abs() < 0.5)
        .unwrap_or_else(|| panic!("the field in {boxes:?}"));
    let panel = *boxes
        .iter()
        .filter(wide)
        .find(|b| b[1] > field[1] && b[1] < field[1] + FIELD_H + 40.0 && b[3] > FIELD_H + 1.0)
        .unwrap_or_else(|| panic!("a panel under {field:?} in {boxes:?}"));
    (field, panel)
}

#[test]
fn typing_opens_the_places_found() {
    let server = server(LISBON);
    let mut app = searching(&server);
    assert!(!app.shows("Portugal"), "closed until something is typed");
    app.type_text("lis");
    app.answer();
    assert!(app.shows("Lisbon"), "{:?}", app.texts());
    assert!(app.shows("Portugal"), "{:?}", app.texts());
    assert!(app.shows("Ukraine"), "{:?}", app.texts());
    assert!(
        server
            .requests()
            .iter()
            .any(|r| r.contains("/v1/search?name=lis&")),
        "{:?}",
        server.requests()
    );
}

/// One letter is not a search: nothing is asked and nothing opens.
#[test]
fn one_letter_asks_nothing() {
    let server = server(LISBON);
    let mut app = searching(&server);
    app.type_text("l");
    app.settle();
    assert!(
        !server.requests().iter().any(|r| r.contains("/v1/search")),
        "{:?}",
        server.requests()
    );
    assert!(!app.shows("Looking for l"), "{:?}", app.texts());
}

/// The panel hangs from the field: its top one gap under the field's bottom,
/// their left edges aligned, and as wide as the field, at any window width.
#[test]
fn the_results_hang_from_the_field_at_two_window_sizes() {
    let server = server(LISBON);
    let mut app = searching(&server);
    app.type_text("lis");
    app.answer();
    for window in [common::W, 900.0] {
        app.resize(window);
        let (field, panel) = field_and_panel(&app, window);
        assert!(
            (panel[1] - (field[1] + field[3] + GAP)).abs() < 0.5,
            "at {window}: one gap under the field: field {field:?} panel {panel:?}"
        );
        assert!(
            (panel[0] - field[0]).abs() < 0.5,
            "at {window}: left edges aligned: field {field:?} panel {panel:?}"
        );
        assert!(
            (panel[2] - field[2]).abs() < 0.5,
            "at {window}: as wide as the field: field {field:?} panel {panel:?}"
        );
    }
}

#[test]
fn picking_a_place_moves_every_screen_there() {
    let server = server(LISBON);
    let mut app = searching(&server);
    app.type_text("lis");
    app.answer();
    app.tap("Portugal");
    // The forecast asks again, for the new place.
    app.answer();
    assert!(app.shows("Lisbon"), "the heading: {:?}", app.texts());
    assert!(!app.shows("Madrid"), "{:?}", app.texts());
    assert!(!app.shows("Portugal"), "the list closed: {:?}", app.texts());
    assert!(
        app.shows("Search for a city"),
        "the field is empty again: {:?}",
        app.texts()
    );
    let asked = server.requests();
    assert!(
        asked
            .iter()
            .any(|r| r.contains("/v1/forecast?latitude=38.72509&longitude=-9.1498")),
        "{asked:?}"
    );

    // A screen opened afterwards asks for the new place too.
    app.tap("10 days");
    let asked = server.requests();
    assert!(
        asked
            .last()
            .is_some_and(|r| r.contains("latitude=38.72509")),
        "{asked:?}"
    );
}

/// The geocoder leaves `results` out when it finds nothing.
#[test]
fn a_name_nobody_has_says_so() {
    let server = server(NOTHING);
    let mut app = searching(&server);
    app.type_text("zzqq");
    app.answer();
    assert!(app.shows("No place by that name"), "{:?}", app.texts());
}

#[test]
fn a_press_elsewhere_closes_the_results() {
    let server = server(LISBON);
    let mut app = searching(&server);
    app.type_text("lis");
    app.answer();
    assert!(app.shows("Portugal"));
    app.tap_at((common::W / 2.0, 600.0));
    assert!(!app.shows("Portugal"), "{:?}", app.texts());
}

/// A reply for what the field no longer says is dropped. The answer to "li"
/// is held back so it lands after the answer to "lis".
#[test]
fn an_answer_to_an_older_question_is_dropped() {
    let server = Server::slow_routes(vec![
        ("/v1/search?name=lis&", "200 OK", LISBON, 0),
        ("/v1/search?name=li&", "200 OK", LIMA, 300),
        ("", "200 OK", FORECAST, 0),
    ]);
    let mut app = searching(&server);
    app.type_text("lis");
    app.answer();
    assert!(app.shows("Lisbon"), "{:?}", app.texts());
    // The late answer, to a question nobody is asking any more.
    app.answer();
    assert!(app.shows("Lisbon"), "{:?}", app.texts());
    assert!(!app.shows("Lima"), "{:?}", app.texts());
}
