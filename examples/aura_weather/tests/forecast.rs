//! The forecast screen over a local server: the request it makes, the three
//! states the request has, and every reading reaching the screen.

mod common;

use common::{App, Server};

const FORECAST: &str = include_str!("fixtures/forecast.json");

#[test]
fn the_screen_asks_for_the_forecast_by_path_when_it_opens() {
    let server = Server::serve("200 OK", FORECAST);
    let mut app = App::start(&server);
    assert!(app.shows("Asking the sky"), "{:?}", app.texts());
    app.answer();
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "one request on open: {requests:?}");
    assert!(
        requests[0].starts_with("GET /v1/forecast?latitude=40.42&longitude=-3.7"),
        "a relative path, resolved against the base URL: {requests:?}"
    );
}

#[test]
fn the_answer_reaches_the_hero_the_details_and_the_hours() {
    let server = Server::serve("200 OK", FORECAST);
    let mut app = App::start(&server);
    app.answer();
    let shown = app.texts();

    // The hero, rounded; the WMO code 3 in words; the day's range.
    for text in ["30°", "Overcast", "High 33°  ·  Low 17°"] {
        assert!(app.shows(text), "{text} in {shown:?}");
    }
    // The details, each rounded the way it is read.
    for text in ["28°", "21%", "6 km/h"] {
        assert!(app.shows(text), "{text} in {shown:?}");
    }
    // The hours: the first is "Now", the rest are cut out of the timestamp,
    // and each hour keeps its own temperature.
    let hours: Vec<&str> = shown
        .iter()
        .map(String::as_str)
        .skip_while(|t| *t != "Next hours")
        .skip(1)
        .collect();
    assert_eq!(&hours[..6], ["Now", "28°", "14:00", "30°", "15:00", "31°"]);
    assert!(!app.shows("Asking the sky"), "the loading state is gone");
}

#[test]
fn a_failed_request_says_so_and_trying_again_asks_again() {
    let server = Server::serve("503 Service Unavailable", "{}");
    let mut app = App::start(&server);
    app.answer();
    assert!(
        app.shows("The forecast did not arrive"),
        "{:?}",
        app.texts()
    );
    assert!(!app.shows("Asking the sky"));

    app.tap("Try again");
    assert!(
        app.shows("Asking the sky"),
        "retrying shows the loading state"
    );
    app.answer();
    assert_eq!(server.requests().len(), 2, "and asks the server again");
    assert!(app.shows("The forecast did not arrive"));
}
