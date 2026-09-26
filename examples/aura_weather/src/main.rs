//! Aura Weather, the reference application: every screen is `byld`, and this
//! is its one Rust file.
//!
//! Run it from the repository root:
//!
//! ```text
//! cargo run -p aura-weather
//! ```
//!
//! Or, for the `byld` half with hot reload:
//!
//! ```text
//! cargo run -p byard-cli -- dev examples/aura_weather
//! ```
//!
//! The app is the project in this directory: `byard.toml` declares the theme,
//! its two typefaces and the type scale, and every `.byd` under `src/` is part
//! of the program. `App::new` reads it exactly as `byard dev` does.

fn main() -> Result<(), byard::ByardError> {
    byard::App::new(env!("CARGO_MANIFEST_DIR"))
        .title("Aura Weather")
        .size(440, 880)
        .run()
}
