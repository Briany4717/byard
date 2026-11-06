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
//! or `settings`. `--light` draws the light scheme. After the screen, `--tap
//! x,y`, `--type text` and `--key Tab` act first, in the order given.

/// Where each tab's button is in the tab bar, in logical pixels.
const TABS: [(&str, f32); 4] = [
    ("dashboard", 163.0),
    ("products", 264.0),
    ("movements", 367.0),
    ("settings", 468.0),
];

fn main() -> Result<(), byard::ByardError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let app = if args.iter().any(|a| a == "--shot") {
        stockroom::shot_app()
    } else {
        stockroom::app()
    };
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
        // A fixed clock, so a record made in the picture is dated the same way
        // every time it is drawn.
        let mut plan = byard::Shot::after(1500)
            .system_dark(!flag("--light"))
            .now("2026-11-05T09:00:00");
        if let Some((_, x)) = TABS.iter().find(|(name, _)| *name == tab) {
            plan = plan.tap(*x, 30.0).wait(500);
        }
        // Then what a person would do next, in the order given: `--tap x,y`,
        // `--type text`, `--key Tab`.
        let mut rest = args.iter().skip(shot_at + 2);
        while let Some(a) = rest.next() {
            match (a.as_str(), rest.clone().next()) {
                ("--tap", Some(xy)) => {
                    let (x, y) = xy.split_once(',').unwrap_or(("0", "0"));
                    plan = plan.tap(x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
                }
                ("--type", Some(text)) => plan = plan.type_text(text),
                ("--key", Some(key)) => plan = plan.key(key),
                _ => continue,
            }
            plan = plan.wait(700);
            rest.next();
        }
        return app.shot(out, &plan);
    }
    app.hot_reload(flag("--dev")).run()
}
