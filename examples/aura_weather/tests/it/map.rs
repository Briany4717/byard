//! The map: the app's own element, drawn by the app's own shader.
//!
//! The view runs here as it does in the app, with its tile controller
//! answering from a local server. The claims are the ones a native view has
//! to keep: it asks for the tiles its rectangle shows and for nothing twice,
//! a tile that never arrives stalls nothing, however many tiles there are
//! they are one batch, and nothing is drawn outside the card.

use crate::common;

use byard_core::render::{ClipShape, NativeBatch};
use common::{App, Server};

const FORECAST: &str = include_str!("../fixtures/forecast.json");

fn server() -> Server {
    Server::serve("200 OK", FORECAST)
}

/// Opens the app on its map and lets every tile arrive.
fn on_the_map(server: &Server) -> App {
    let mut app = App::start(server);
    app.answer_all();
    app.tap("Map");
    app.answer_all();
    app
}

/// The request lines for tiles, as `z/x/y`.
fn tiles_asked(server: &Server) -> Vec<String> {
    server
        .requests()
        .iter()
        .filter_map(|r| {
            let path = r.split_whitespace().nth(1)?;
            Some(
                path.strip_prefix("/tiles/")?
                    .trim_end_matches(".png")
                    .to_string(),
            )
        })
        .collect()
}

/// The batches the map's own shader draws this frame.
fn tile_batches(app: &App) -> Vec<&NativeBatch> {
    app.frame
        .native_batches()
        .iter()
        .filter(|b| b.pipeline.name() == "aura_map_tiles")
        .collect()
}

#[test]
fn the_map_asks_for_the_tiles_around_the_place() {
    let server = server();
    let app = on_the_map(&server);
    assert!(app.shows("Where it is"), "{:?}", app.texts());
    assert!(app.shows("© OpenStreetMap contributors"), "the attribution");

    let asked = tiles_asked(&server);
    // Madrid at zoom 10 is in column 501, row 385.
    assert!(asked.contains(&"10/501/385".to_string()), "{asked:?}");
    assert!(asked.iter().all(|t| t.starts_with("10/")), "{asked:?}");
    let mut unique = asked.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        asked.len(),
        "no tile is asked for twice: {asked:?}"
    );
}

/// Every tile on screen is in one batch, at this width and at twice it:
/// more tiles, the same one draw.
#[test]
fn however_many_tiles_they_are_one_batch() {
    let server = server();
    let mut app = on_the_map(&server);
    let narrow = tile_batches(&app);
    assert_eq!(narrow.len(), 1, "one batch");
    let few = narrow[0].count;
    assert_eq!(
        few as usize,
        tiles_asked(&server).len(),
        "every tile that arrived is drawn"
    );

    app.resize(900.0);
    app.answer_all();
    let wide = tile_batches(&app);
    assert_eq!(wide.len(), 1, "still one batch");
    assert!(wide[0].count > few, "{} tiles, then {}", few, wide[0].count);
}

/// A tile the server does not have leaves a gap, and nothing else: the rest
/// are drawn, the screen is drawn, and the tile is not asked for again on
/// every frame.
#[test]
fn a_tile_that_never_arrives_stalls_nothing() {
    let server = Server::routes(vec![
        ("/tiles/10/501/385", "404 Not Found", "{}"),
        ("", "200 OK", FORECAST),
    ]);
    let mut app = on_the_map(&server);
    let asked = tiles_asked(&server).len();
    let drawn = tile_batches(&app)[0].count as usize;
    assert_eq!(drawn, asked - 1, "every tile but the missing one");
    assert!(app.shows("Where it is"), "{:?}", app.texts());

    // A second of frames later nothing more has been asked.
    app.settle();
    assert_eq!(
        tiles_asked(&server).len(),
        asked,
        "{:?}",
        tiles_asked(&server)
    );
}

/// The tiles reach past the card. The batch carries the card's rectangle as
/// its clip, so none of that is drawn.
#[test]
fn nothing_is_drawn_outside_the_card() {
    let server = server();
    let app = on_the_map(&server);
    let batch = tile_batches(&app)[0];
    let Some(ClipShape::Rect(clip)) = batch.clip else {
        panic!("the map clips its tiles: {:?}", batch.clip);
    };
    // The card: the window's width less the screen's 24 of padding each side,
    // and the 520 the screen gives it.
    assert!((clip[2] - (common::W - 48.0)).abs() < 0.5, "{clip:?}");
    assert!((clip[3] - 520.0).abs() < 0.5, "{clip:?}");
    assert!((clip[0] - 24.0).abs() < 0.5, "{clip:?}");
}

#[test]
fn zooming_in_asks_for_the_next_level() {
    let server = server();
    let mut app = on_the_map(&server);
    app.tap("+");
    app.answer_all();
    let asked = tiles_asked(&server);
    // Each level doubles the columns and rows: the place is in 1003, 771.
    assert!(asked.contains(&"11/1003/771".to_string()), "{asked:?}");
}

/// Another place moves the map there.
#[test]
fn the_map_follows_the_place() {
    let server = Server::routes(vec![
        (
            "/v1/search",
            "200 OK",
            include_str!("../fixtures/geocode.json"),
        ),
        ("", "200 OK", FORECAST),
    ]);
    let mut app = on_the_map(&server);
    app.tap("Search for a city");
    app.type_text("lis");
    app.answer_all();
    app.tap("Portugal");
    app.answer_all();
    let asked = tiles_asked(&server);
    // Lisbon at zoom 10: column 486, row 391.
    assert!(asked.contains(&"10/486/391".to_string()), "{asked:?}");
}

/// Dragging the map moves it: the tiles shift with the pointer, and the ones
/// that come into view are asked for.
#[test]
fn dragging_pans_the_map() {
    use byard_core::platform::{EventKind, InputEvent};

    let server = server();
    let mut app = on_the_map(&server);
    let before = tiles_asked(&server).len();
    let at = |kind, pos, t| InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
        mods: byard_core::KeyModifiers::default(),
    };
    // Press in the middle of the card and pull a tile and a half to the right.
    let (x, y) = (220.0, 400.0);
    app.step(&[at(EventKind::PointerDown, (x, y), 5000)]);
    for i in 1..=8_u16 {
        let moved = x + f32::from(i) * 48.0;
        app.step(&[at(
            EventKind::PointerMove,
            (moved, y),
            5000 + u64::from(i) * 16,
        )]);
    }
    app.step(&[at(EventKind::PointerUp, (x + 384.0, y), 5200)]);
    app.answer_all();

    let asked = tiles_asked(&server);
    assert!(
        asked.len() > before,
        "tiles to the west came into view: {asked:?}"
    );
    // Pulled right, the map shows what is to the west: column 499 and below.
    assert!(
        asked
            .iter()
            .any(|t| t.starts_with("10/499/") || t.starts_with("10/498/")),
        "{asked:?}"
    );
}

/// Panning over tiles the map already has costs a frame and nothing else:
/// no tile is asked for again, no layer is written again, the tiles stay one
/// batch, and the frame time is printed for a person to read (shared CI
/// runners are too noisy to hold to a number).
#[test]
fn panning_over_tiles_it_has_writes_and_asks_nothing() {
    use byard_core::platform::{EventKind, InputEvent};

    let server = server();
    let mut app = on_the_map(&server);
    let asked = tiles_asked(&server).len();
    let at = |kind, pos, t| InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
        mods: byard_core::KeyModifiers::default(),
    };

    // Small moves back and forth: everything on screen stays among the
    // tiles already loaded.
    let (x, y) = (220.0, 400.0);
    app.step(&[at(EventKind::PointerDown, (x, y), 9000)]);
    let mut slowest = std::time::Duration::ZERO;
    let mut total = std::time::Duration::ZERO;
    let frames = 60_u16;
    for i in 1..=frames {
        let dx = if i % 2 == 0 { 6.0 } else { -6.0 };
        let started = std::time::Instant::now();
        app.step(&[at(
            EventKind::PointerMove,
            (x + dx, y),
            9000 + u64::from(i) * 16,
        )]);
        let took = started.elapsed();
        slowest = slowest.max(took);
        total += took;

        assert!(
            app.frame.native_textures().is_empty(),
            "frame {i} wrote a layer again"
        );
        assert_eq!(tile_batches(&app).len(), 1, "frame {i}: one batch");
    }
    app.step(&[at(EventKind::PointerUp, (x, y), 10_200)]);
    assert_eq!(tiles_asked(&server).len(), asked, "nothing asked again");
    eprintln!(
        "map pan, logic per frame (tick + render, whole app): mean {:?}, slowest {:?}",
        total / u32::from(frames),
        slowest
    );
}

/// The `night` lane of every tile drawn this frame: the last of the
/// instance's eleven floats.
fn night_lanes(app: &App) -> Vec<f32> {
    let batch = tile_batches(app)[0];
    assert_eq!(batch.instance_size, 44, "rect, card, layer, radius, night");
    batch
        .bytes
        .chunks(batch.instance_size)
        .map(|tile| f32::from_ne_bytes(tile[40..44].try_into().unwrap()))
        .collect()
}

fn system(dark: bool) -> byard_core::InputEvent {
    byard_core::InputEvent {
        kind: byard_core::EventKind::SystemScheme,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::InputPayload::Bool(dark)),
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }
}

/// The app follows the system's appearance, and the map with it: the tiles
/// are drawn in their own daytime colours on a light system and in the
/// app's night ones on a dark one.
#[test]
fn the_map_follows_the_systems_appearance() {
    let server = server();
    let mut app = on_the_map(&server);
    app.step(&[system(false)]);
    app.settle();
    let day = night_lanes(&app);
    assert!(
        !day.is_empty() && day.iter().all(|&n| n.abs() < f32::EPSILON),
        "{day:?}"
    );

    app.step(&[system(true)]);
    app.settle();
    let night = night_lanes(&app);
    assert!(
        night.iter().all(|&n| (n - 1.0).abs() < f32::EPSILON),
        "{night:?}"
    );
}
