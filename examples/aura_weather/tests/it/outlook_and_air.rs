//! The 10-day outlook and the air quality screen, over a local server that
//! answers both hosts the app declares.

use crate::common;

use byard_core::frame::{CANVAS_SHAPE_ARC, GradientKind};
use common::{App, Server};

const FORECAST: &str = include_str!("../fixtures/forecast.json");
const OUTLOOK: &str = include_str!("../fixtures/outlook.json");
const AIR: &str = include_str!("../fixtures/air.json");

fn server() -> Server {
    Server::routes(vec![
        ("/v1/air-quality", "200 OK", AIR),
        ("forecast_days=10", "200 OK", OUTLOOK),
        ("/v1/forecast", "200 OK", FORECAST),
    ])
}

/// The app, answered, on the tab labelled `tab`.
fn on_tab(tab: &str) -> (App, Server) {
    let server = server();
    let mut app = App::start(&server);
    app.answer(); // the forecast, which opens first
    app.tap(tab);
    app.answer();
    (app, server)
}

#[test]
fn the_outlook_lists_ten_days_each_with_its_own_readings() {
    let (app, server) = on_tab("10 days");
    assert!(
        server
            .requests()
            .iter()
            .any(|r| r.contains("forecast_days=10") && r.contains("latitude=40.42")),
        "{:?}",
        server.requests()
    );
    let shown = app.texts();
    // Today, then day and month cut out of the date.
    for text in ["Today", "01 Oct", "09 Oct"] {
        assert!(app.shows(text), "{text} in {shown:?}");
    }
    // A row keeps its own values: the third day is a storm, 63% rain,
    // 16 to 23.
    let row: Vec<&str> = shown
        .iter()
        .map(String::as_str)
        .skip_while(|t| *t != "02 Oct")
        .take(5)
        .collect();
    assert_eq!(row, ["02 Oct", "Storms", "63%", "16°", "23°"]);
    let days = shown
        .iter()
        .filter(|t| t.ends_with(" Oct") || *t == "Today")
        .count();
    assert_eq!(days, 10, "{shown:?}");
}

#[test]
fn the_air_screen_asks_the_air_host_and_names_the_reading() {
    let (app, server) = on_tab("Air");
    assert!(
        server
            .requests()
            .iter()
            .any(|r| r.starts_with("GET /v1/air-quality?latitude=40.42")),
        "the named host, resolved to the local server: {:?}",
        server.requests()
    );
    let shown = app.texts();
    for text in [
        "55",
        "Moderate",
        "12.8 µg/m³",
        "18.7 µg/m³",
        "13 µg/m³",
        "42 µg/m³",
    ] {
        assert!(app.shows(text), "{text} in {shown:?}");
    }
}

#[test]
fn the_glow_behind_the_gauge_is_radial() {
    let (app, _server) = on_tab("Air");
    let glows: Vec<_> = app
        .frame
        .decorated()
        .iter()
        .filter_map(|d| d.gradient)
        .filter(|g| g.kind == GradientKind::Radial)
        .collect();
    assert_eq!(glows.len(), 1, "one radial glow, not a linear ramp");
    assert!(
        (glows[0].center[0] - 0.5).abs() < 1e-6,
        "centred: {:?}",
        glows[0].center
    );
}

/// The gauge's arcs, as `(radius, sweep in degrees)`.
fn arcs(app: &App) -> Vec<(f32, f32)> {
    app.frame
        .canvas_shapes()
        .iter()
        .filter(|s| s.kind == CANVAS_SHAPE_ARC)
        .map(|s| (s.params[2], s.params[4].to_degrees()))
        .collect()
}

/// The gauge's card: the one box with the radial glow.
fn gauge_card(app: &App) -> [f32; 4] {
    app.frame
        .decorated()
        .iter()
        .find(|d| d.gradient.is_some_and(|g| g.kind == GradientKind::Radial))
        .map(|d| d.base.rect)
        .expect("the gauge's card")
}

#[test]
fn the_gauge_is_drawn_inside_its_card_and_centred() {
    let (mut app, _server) = on_tab("Air");
    for width in [440.0, 300.0, 1280.0] {
        app.resize(width);
        let card = gauge_card(&app);
        let (left, top, card_w, card_h) = (card[0], card[1], card[2], card[3]);
        for s in app
            .frame
            .canvas_shapes()
            .iter()
            .filter(|s| s.kind == CANVAS_SHAPE_ARC)
        {
            let (cx, cy, r) = (s.params[0], s.params[1], s.params[2]);
            assert!(
                (cx - (left + card_w / 2.0)).abs() < 1.0,
                "at {width}: centred in the card {card:?}, arc at {cx}"
            );
            assert!(
                cy - r >= top && cy + r <= top + card_h,
                "at {width}: inside the card {card:?}, arc {cx},{cy} r {r}"
            );
        }
    }
}

#[test]
fn the_gauge_reads_the_aqi_and_follows_its_card() {
    let (mut app, _server) = on_tab("Air");
    let wide = arcs(&app);
    assert_eq!(wide.len(), 2, "a track and a value: {wide:?}");
    assert!(
        (wide[0].1 - 270.0).abs() < 0.01,
        "the track is the full 270°"
    );
    assert!(
        (wide[1].1 - 55.0 * 0.9).abs() < 0.01,
        "55 on a 300 scale over 270°: {wide:?}"
    );

    // Narrow enough that the card, not the 260 cap, decides the size.
    app.resize(300.0);
    let narrow = arcs(&app);
    assert!(
        narrow[0].0 < wide[0].0 - 10.0,
        "the gauge shrank with its card: {wide:?} then {narrow:?}"
    );
    let card = app
        .frame
        .decorated()
        .iter()
        .find(|d| d.gradient.is_some_and(|g| g.kind == GradientKind::Radial))
        .map(|d| d.base.rect[2])
        .expect("the gauge's card");
    assert!(
        (narrow[0].0 - (card - 40.0) * 0.38).abs() < 0.5,
        "drawn from the measured card ({card}), not from a literal: {narrow:?}"
    );
}
