//! Aura Weather. See `lib.rs` for what it is made of.
//!
//! Run it from the repository root:
//!
//! ```text
//! cargo run -p aura-weather
//! ```
//!
//! Or draw one screen to a PNG without opening a window, after two seconds
//! of running (so its data has arrived):
//!
//! ```text
//! cargo run -p aura-weather -- --shot map.png map
//! ```
//!
//! The next word is the tab: `now`, `days`, `air`, `trends` or `map`, and
//! `--light` after it draws the light scheme.
//!
//! Or run it following its own files, so an edit to a screen applies to the
//! open window:
//!
//! ```text
//! cargo run -p aura-weather -- --dev
//! ```

/// Where each tab's button is in the tab bar, in logical pixels.
const TABS: [(&str, f32); 5] = [
    ("now", 38.0),
    ("days", 106.0),
    ("air", 169.0),
    ("trends", 231.0),
    ("map", 296.0),
];

fn main() -> Result<(), byard::ByardError> {
    let app = aura_weather::app(aura_weather::TILE_SERVER);
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        // Edit any `.byd` or `byard.toml` and the open window follows, map
        // included.
        (Some("--dev"), _) => app.hot_reload(true).run(),
        (Some("--shot"), Some(out)) => {
            let tab = args.next().unwrap_or_else(|| "now".to_string());
            // The app follows the system's appearance; a shot is dark unless
            // `--light` is given.
            let dark = !args.any(|a| a == "--light");
            let mut plan = byard::Shot::after(2000).system_dark(dark);
            if let Some((_, x)) = TABS.iter().find(|(name, _)| *name == tab) {
                plan = plan.tap(*x, 850.0);
            }
            app.shot(out, &plan)
        }
        _ => app.run(),
    }
}
