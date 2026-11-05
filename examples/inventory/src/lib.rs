//! Stockroom, a stock-keeping app and Byard's biggest test of itself.
//!
//! Every screen is `byld`, under `src/`. What `byld` cannot do is in
//! [`controller`]: read the clock, and write the CSV export.

pub mod controller;

pub use controller::Stockroom;

/// The app, keeping its files under the platform's data directory.
#[must_use]
pub fn app() -> byard::App {
    app_with(env!("CARGO_MANIFEST_DIR"), Stockroom::for_app(APP_ID))
}

/// The identity the saved data is filed under.
pub const APP_ID: &str = "dev.byard.stockroom";

/// The app as [`app`] builds it, reading its project from `project` and using
/// `room` as its controller.
#[must_use]
pub fn app_with(project: impl AsRef<std::path::Path>, room: Stockroom) -> byard::App {
    byard::App::new(project)
        .title("Stockroom")
        .size(1100, 720)
        .app_id(APP_ID)
        .provide(room)
}
