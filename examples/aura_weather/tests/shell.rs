//! The app shell (the tab host, the tab bar, the theme and its two faces),
//! checked the way the app runs: the project is read from this directory and
//! the program is driven through the interpreter, with taps where a person
//! would tap.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent};

const W: f32 = 440.0;
const H: f32 = 852.0;

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    now: u32,
}

impl App {
    fn start() -> Self {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = byard_project::manifest::Manifest::discover(Some(dir)).unwrap();
        let (program, _) = byard_project::deps::resolve_project(&manifest).unwrap();
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        let mut interp = Interpreter::new();
        interp.set_theme(manifest.theme);
        interp.load_views(&program.views);
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        let root = byard_compiler::parser::ast::root_view(&program.views).unwrap();
        let tree = interp.lower_view(root, &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            now: 0,
        };
        app.step(&[]);
        app
    }

    fn step(&mut self, events: &[InputEvent]) {
        self.interp.set_now_ms(self.now);
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame = RenderFrame::new();
        self.interp.render(&self.tree, &mut self.frame, W, H);
        self.now += 16;
    }

    /// Runs long enough for the tab transition to finish.
    fn settle(&mut self) {
        for _ in 0..60 {
            self.step(&[]);
        }
    }

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    /// Taps the tab labelled `label`, wherever the layout put it.
    fn tap_tab(&mut self, label: &str) {
        let line = self
            .frame
            .texts()
            .iter()
            .rev() // the tab bar is drawn after the screen
            .find(|t| t.text == label)
            .unwrap_or_else(|| panic!("a tab labelled {label}: {:?}", self.texts()));
        let pos = (line.x + 4.0, line.y + line.font_size * 0.5);
        let at = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
        };
        let t = u64::from(self.now);
        self.step(&[
            at(EventKind::PointerDown, t),
            at(EventKind::PointerUp, t + 8),
        ]);
        self.settle();
    }
}

#[test]
fn the_app_starts_on_the_forecast_and_every_tab_reaches_its_screen() {
    let mut app = App::start();
    app.settle();
    assert!(
        app.texts().contains(&"Now".to_string()),
        "{:?}",
        app.texts()
    );
    assert!(
        app.texts()
            .contains(&"Current conditions and the next hours".to_string())
    );

    for (tab, subtitle) in [
        ("10 days", "The outlook, day by day"),
        ("Air", "What you are breathing"),
        ("Trends", "Temperature over the week"),
        ("Now", "Current conditions and the next hours"),
    ] {
        app.tap_tab(tab);
        assert!(
            app.texts().contains(&subtitle.to_string()),
            "tapping {tab} shows its screen: {:?}",
            app.texts()
        );
    }
}

/// Headings are set in the display face and reading text in the UI face,
/// through the theme's type scale: two families on one frame.
#[test]
fn the_frame_carries_both_typefaces() {
    let mut app = App::start();
    app.settle();
    let family = |text: &str| {
        app.frame
            .texts()
            .iter()
            .find(|t| t.text == text)
            .and_then(|t| t.family.as_deref().map(str::to_string))
    };
    assert_eq!(
        family("Now").as_deref(),
        Some("Space Grotesk"),
        "the heading"
    );
    assert_eq!(
        family("Current conditions and the next hours").as_deref(),
        Some("Manrope"),
        "the caption"
    );
}
