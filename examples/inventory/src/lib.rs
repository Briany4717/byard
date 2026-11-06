//! Stockroom, a stock-keeping app and Byard's biggest test of itself.
//!
//! Every screen is `byld`, under `src/`. What `byld` cannot do is in
//! [`controller`]: write the CSV export.

pub mod controller;

pub use controller::Stockroom;

/// The identity the saved data is filed under.
pub const APP_ID: &str = "dev.byard.stockroom";

fn base() -> byard::App {
    byard::App::new(env!("CARGO_MANIFEST_DIR"))
        .title("Stockroom")
        .size(1100, 720)
}

/// The app, keeping its files under the platform's data directory.
#[must_use]
pub fn app() -> byard::App {
    base().provide(Stockroom::for_app(APP_ID)).app_id(APP_ID)
}

/// The app for a headless shot: its saved data and its export go to a folder
/// of their own under the system's temporary directory, so a shot never reads
/// or changes the real catalogue. The folder starts empty (the sample data).
#[must_use]
pub fn shot_app() -> byard::App {
    let id = format!("{APP_ID}.shot-{}", std::process::id());
    let dir = std::env::temp_dir().join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    base().provide(Stockroom::at(&dir)).app_id(&id)
}
