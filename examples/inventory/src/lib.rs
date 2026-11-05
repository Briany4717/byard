//! Stockroom, a stock-keeping app and Byard's biggest test of itself.
//!
//! Every screen is `byld`, under `src/`. What `byld` cannot do is in
//! [`controller`]: read the clock, and write the CSV export.

pub mod controller;

pub use controller::Stockroom;

/// The app, keeping its files under the platform's data directory.
#[must_use]
pub fn app() -> byard::App {
    app_with(env!("CARGO_MANIFEST_DIR"), Stockroom::for_app(APP_ID), APP_ID)
}

/// The identity the saved data is filed under.
pub const APP_ID: &str = "dev.byard.stockroom";

/// The app as [`app`] builds it, reading its project from `project` and using
/// `room` as its controller, and its saved data filed under `app_id`.
#[must_use]
pub fn app_with(
    project: impl AsRef<std::path::Path>,
    room: Stockroom,
    app_id: &str,
) -> byard::App {
    byard::App::new(project)
        .title("Stockroom")
        .size(1100, 720)
        .app_id(app_id)
        .provide(room)
}

/// The app for a headless shot: its saved data and its export go to a folder
/// of their own under the system's temporary directory, so a shot never reads
/// or changes the real catalogue. The folder starts empty (the sample data).
#[must_use]
pub fn shot_app() -> byard::App {
    let id = format!("{APP_ID}.shot-{}", std::process::id());
    let dir = std::env::temp_dir().join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    app_with(
        env!("CARGO_MANIFEST_DIR"),
        Stockroom::at(&dir).with_fixed_now("2026-11-05T10:30"),
        &id,
    )
}
