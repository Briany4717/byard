//! Aura Weather. See `lib.rs` for what it is made of.
//!
//! Run it from the repository root:
//!
//! ```text
//! cargo run -p aura-weather
//! ```
//!
//! Or draw one screen to a PNG without opening a window, after six seconds
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
//!
//! `--trace <file>` writes a trace of every frame, for either a window or a
//! shot; `--pan` drags the map across in a shot first, so its trace holds
//! panning frames:
//!
//! ```text
//! cargo run --release -p aura-weather -- --shot map.png map --pan --trace pan.json
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
    let mut app = aura_weather::app(aura_weather::TILE_SERVER);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };
    if let Some(path) = value("--trace") {
        app = app.trace(path);
    }
    if let Some(out) = value("--shot") {
        let shot_at = args.iter().position(|a| a == "--shot").unwrap_or(0);
        let tab = args
            .get(shot_at + 2)
            .filter(|a| !a.starts_with("--"))
            .map_or("now", String::as_str);
        // The app follows the system's appearance; a shot is dark unless
        // `--light` is given.
        let mut plan = byard::Shot::after(6000).system_dark(!flag("--light"));
        if let Some((_, x)) = TABS.iter().find(|(name, _)| *name == tab) {
            plan = plan.tap(*x, 850.0);
        }
        if flag("--pan") {
            // Across the map and back, a frame a step, once its tiles are in.
            plan = plan
                .wait(4000)
                .drag((300.0, 400.0), (100.0, 300.0), 60)
                .drag((100.0, 300.0), (300.0, 400.0), 60);
        }
        if let Some(path) = value("--trace") {
            plan = plan.trace(path);
        }
        return app.shot(out, &plan);
    }
    // Edit any `.byd` or `byard.toml` and the open window follows, map
    // included.
    app.hot_reload(flag("--dev")).run()
}
