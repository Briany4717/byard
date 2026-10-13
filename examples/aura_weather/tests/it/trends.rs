//! Weather Trends: the week's temperatures as a filled curve, drawn from the
//! response by a `for` inside the path, on a canvas as wide as its card.

use crate::common;

use byard_core::frame::CANVAS_SHAPE_LINE;
use common::{App, Server};

const FORECAST: &str = include_str!("../fixtures/forecast.json");
const TRENDS: &str = include_str!("../fixtures/trends.json");

fn server(week: &'static str) -> Server {
    Server::routes(vec![
        ("forecast_days=7", "200 OK", week),
        ("/v1/forecast", "200 OK", FORECAST),
    ])
}

/// The app, answered, on the Trends tab.
fn on_trends(week: &'static str) -> (App, Server) {
    let server = server(week);
    let mut app = App::start(&server);
    app.answer(); // the forecast, which opens first
    app.tap("Trends");
    app.answer();
    (app, server)
}

/// The chart's card: the box the area's fill sits inside.
fn card(app: &App) -> [f32; 4] {
    let fill = app.frame.fills()[0].mesh.bounds;
    app.frame
        .instances()
        .iter()
        .map(|b| b.rect)
        .chain(app.frame.decorated().iter().map(|d| d.base.rect))
        .filter(|r| {
            r[0] <= fill[0]
                && r[1] <= fill[1]
                && r[0] + r[2] >= fill[0] + fill[2]
                && r[1] + r[3] >= fill[1] + fill[3]
        })
        .min_by(|a, b| (a[2] * a[3]).total_cmp(&(b[2] * b[3])))
        .expect("a card round the chart")
}

#[test]
fn the_week_arrives_as_one_filled_curve_and_two_lines() {
    let (app, server) = on_trends(TRENDS);
    assert!(
        server
            .requests()
            .iter()
            .any(|r| r.contains("forecast_days=7") && r.contains("latitude=40.42")),
        "{:?}",
        server.requests()
    );
    // The summary reads the lists: the warmest high and the coldest low.
    for text in ["27°", "16°", "warmest", "coldest"] {
        assert!(app.shows(text), "{text} in {:?}", app.texts());
    }
    // One area under the highs. The two lines are six curves each, and a
    // curve is drawn as at least eight short segments.
    assert_eq!(app.frame.fills().len(), 1, "one filled area");
    let segments = app
        .frame
        .canvas_shapes()
        .iter()
        .filter(|s| s.kind == CANVAS_SHAPE_LINE)
        .count();
    assert!(
        segments >= 12 * 8,
        "two lines of six curves: {segments} segments"
    );
    // A day of the month under each reading.
    for day in ["30", "01", "06"] {
        assert!(app.shows(day), "{day} in {:?}", app.texts());
    }
}

/// The area is the data's shape: its top is the warmest high, and a colder
/// week draws a different curve in the same card.
#[test]
fn the_curve_is_the_data() {
    let (warm, _s1) = on_trends(TRENDS);
    let (cold, _s2) = on_trends(include_str!("../fixtures/trends_colder.json"));
    let (a, b) = (&warm.frame.fills()[0].mesh, &cold.frame.fills()[0].mesh);
    assert!((a.bounds[2] - b.bounds[2]).abs() < 0.5, "same width");
    assert_ne!(a.vertices.len(), 0);
    assert!(
        a.vertices
            .iter()
            .zip(&b.vertices)
            .any(|(p, q)| (p.pos[1] - q.pos[1]).abs() > 1.0)
            || a.vertices.len() != b.vertices.len(),
        "a different week is a different curve"
    );
    assert!(cold.shows("14°") && cold.shows("-2°"), "{:?}", cold.texts());
}

#[test]
fn the_chart_spans_its_card_at_every_width() {
    let (mut app, _server) = on_trends(TRENDS);
    let mut widths = Vec::new();
    for window in [440.0, 320.0, 900.0] {
        app.resize(window);
        let fill = app.frame.fills()[0].mesh.bounds;
        let card = card(&app);
        // The card pads 20 a side and the plot insets 16 more.
        let (left, right) = (fill[0] - card[0], card[0] + card[2] - (fill[0] + fill[2]));
        assert!(
            (left - 36.0).abs() < 1.0 && (right - 36.0).abs() < 1.0,
            "at {window}: {left} left and {right} right of the card {card:?}, fill {fill:?}"
        );
        widths.push(fill[2]);
    }
    assert!(
        widths[1] < widths[0] && widths[0] < widths[2],
        "narrower in a narrower window, wider in a wider one: {widths:?}"
    );
}

/// INV-23: a still chart is not tessellated again, and a resize, which moves
/// every point, is.
#[test]
fn the_curve_is_rebuilt_only_when_its_numbers_move() {
    let (mut app, _server) = on_trends(TRENDS);
    let settled = app.interp.tessellations();
    for _ in 0..30 {
        app.step(&[]);
    }
    assert_eq!(
        app.interp.tessellations(),
        settled,
        "thirty still frames tessellate nothing"
    );
    app.resize(700.0);
    assert!(
        app.interp.tessellations() > settled,
        "a new width is a new curve"
    );
    let resized = app.interp.tessellations();
    for _ in 0..30 {
        app.step(&[]);
    }
    assert_eq!(app.interp.tessellations(), resized);
}
