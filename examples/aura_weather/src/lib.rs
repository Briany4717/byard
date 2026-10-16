//! Aura Weather, the reference application.
//!
//! Every screen is `byld`. The one thing that is not is the map: an element
//! of the app's own, with the app's own shader, in [`map`].
//!
//! The app is the project in this directory: `byard.toml` declares the theme,
//! its two typefaces and the type scale, and every `.byd` under `src/` is part
//! of the program. `App::new` reads it exactly as `byard dev` does.

pub mod map;

/// Where the map's tiles come from, used under OpenStreetMap's tile usage
/// policy (its attribution is on the map screen).
pub const TILE_SERVER: &str = "https://tile.openstreetmap.org";

/// The app, with its map drawing tiles from `tiles`.
#[must_use]
pub fn app(tiles: &str) -> byard::App {
    byard::App::new(env!("CARGO_MANIFEST_DIR"))
        .title("Aura Weather")
        .size(440, 880)
        .shader::<map::MapTiles>()
        .native_view::<map::Map>()
        .provide(map::Tiles::at(tiles))
}
