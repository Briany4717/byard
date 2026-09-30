//! The app shell: the tab host, the tab bar, the theme and its two faces.

mod common;

use common::{App, Server};

/// What the forecast screen asks for, trimmed to the fields it reads.
const FORECAST: &str = include_str!("fixtures/forecast.json");

#[test]
fn the_app_starts_on_the_forecast_and_every_tab_reaches_its_screen() {
    let server = Server::serve("200 OK", FORECAST);
    let mut app = App::start(&server);
    app.settle();
    assert!(app.shows("Now"), "{:?}", app.texts());
    assert!(app.shows("Current conditions and the next hours"));

    for (tab, subtitle) in [
        ("10 days", "The outlook, day by day"),
        ("Air", "What you are breathing"),
        ("Trends", "Temperature over the week"),
        ("Now", "Current conditions and the next hours"),
    ] {
        app.tap(tab);
        assert!(
            app.shows(subtitle),
            "tapping {tab} shows its screen: {:?}",
            app.texts()
        );
    }
}

/// Headings are set in the display face and reading text in the UI face,
/// through the theme's type scale: two families on one frame.
#[test]
fn the_frame_carries_both_typefaces() {
    let server = Server::serve("200 OK", FORECAST);
    let mut app = App::start(&server);
    app.settle();
    let family = |text: &str| {
        app.frame
            .texts()
            .iter()
            .find(|t| t.text == text)
            .and_then(|t| t.family.as_deref().map(str::to_string))
    };
    assert_eq!(
        family("Madrid").as_deref(),
        Some("Space Grotesk"),
        "the heading"
    );
    assert_eq!(
        family("Current conditions and the next hours").as_deref(),
        Some("Manrope"),
        "the caption"
    );
}
