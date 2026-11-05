//! Stockroom. Run it from the repository root:
//!
//! ```text
//! cargo run -p stockroom
//! ```
//!
//! Or draw a screen to a PNG without opening a window:
//!
//! ```text
//! cargo run -p stockroom -- --shot dashboard.png dashboard
//! ```
//!
//! The word after the file is the screen: `dashboard`, `products`, `movements`
//! or `settings`. `--light` draws the light scheme.

/// Where each tab's button is in the tab bar, in logical pixels.
const TABS: [(&str, f32); 4] = [
    ("dashboard", 80.0),
    ("products", 200.0),
    ("movements", 320.0),
    ("settings", 440.0),
];

fn main() -> Result<(), byard::ByardError> {
    let app = stockroom::app();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };
    if let Some(out) = value("--shot") {
        let shot_at = args.iter().position(|a| a == "--shot").unwrap_or(0);
        let tab = args
            .get(shot_at + 2)
            .filter(|a| !a.starts_with("--"))
            .map_or("dashboard", String::as_str);
        let mut plan = byard::Shot::after(1500).system_dark(!flag("--light"));
        if let Some((_, x)) = TABS.iter().find(|(name, _)| *name == tab) {
            plan = plan.tap(*x, 30.0).wait(500);
        }
        return app.shot(out, &plan);
    }
    app.hot_reload(flag("--dev")).run()
}
