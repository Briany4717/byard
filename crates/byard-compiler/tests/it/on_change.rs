//! `on change(a, b) => action`: an action run when a value it names changes.
//!
//! The case it exists for is a screen that loads in `on mount` and has to
//! load again when what it was asked for changes. The claims: it does not run
//! on mount, it runs once per change and not per frame, a value written to
//! what it already was is not a change, and it stops with its scope.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    now: u32,
}

impl App {
    fn start(source: &str) -> Self {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let checked = byard_compiler::infer::check_views(&parsed.views).errors;
        assert!(checked.is_empty(), "the type checker: {checked:?}");
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let root = byard_compiler::parser::ast::root_view(&parsed.views).unwrap();
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
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 300.0);
        self.now += 16;
    }

    fn tap(&mut self, label: &str) {
        let line = self
            .frame
            .texts()
            .iter()
            .find(|t| t.text == label)
            .unwrap_or_else(|| panic!("a text {label}: {:?}", self.texts()));
        let pos = (line.x + 2.0, line.y + line.font_size * 0.5);
        let at = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
            mods: byard_core::KeyModifiers::default(),
        };
        let t = u64::from(self.now);
        self.step(&[
            at(EventKind::PointerDown, t),
            at(EventKind::PointerUp, t + 8),
        ]);
        // Idle frames: a change must not go on running once it has run.
        for _ in 0..5 {
            self.step(&[]);
        }
    }

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn shows(&self, text: &str) -> bool {
        self.frame.texts().iter().any(|t| t.text == text)
    }
}

const COUNTER: &str = r#"
View Main() {
    var city = "Madrid"
    var runs = 0
    on change(city) => runs++

    Column #[p: 20, gap: 12] {
        Text("ran {runs}")
        Button("Lisbon") => city = "Lisbon"
        Button("Madrid again") => city = "Madrid"
    }
}
"#;

#[test]
fn it_does_not_run_on_mount() {
    let mut app = App::start(COUNTER);
    for _ in 0..5 {
        app.step(&[]);
    }
    assert!(app.shows("ran 0"), "{:?}", app.texts());
}

#[test]
fn it_runs_once_for_each_change() {
    let mut app = App::start(COUNTER);
    app.tap("Lisbon");
    assert!(app.shows("ran 1"), "{:?}", app.texts());
    app.tap("Madrid again");
    assert!(app.shows("ran 2"), "{:?}", app.texts());
}

#[test]
fn writing_the_value_it_already_has_is_not_a_change() {
    let mut app = App::start(COUNTER);
    app.tap("Madrid again");
    assert!(app.shows("ran 0"), "{:?}", app.texts());
    app.tap("Lisbon");
    app.tap("Lisbon");
    assert!(app.shows("ran 1"), "{:?}", app.texts());
}

/// The case it was written for: the value belongs to the parent and reaches
/// the screen as a parameter.
#[test]
fn a_view_sees_its_parameter_change() {
    let mut app = App::start(
        r#"
View Main() {
    var city = "Madrid"
    Column #[p: 20, gap: 12] {
        Screen(place: city)
        Button("Lisbon") => city = "Lisbon"
    }
}

View Screen(place: Str) {
    var loaded = "nothing"
    on mount => loaded = place
    on change(place) => loaded = place
    Text("loaded {loaded}")
}
"#,
    );
    assert!(app.shows("loaded Madrid"), "{:?}", app.texts());
    app.tap("Lisbon");
    assert!(app.shows("loaded Lisbon"), "{:?}", app.texts());
}

#[test]
fn two_values_written_together_run_it_once() {
    let mut app = App::start(
        r#"
View Main() {
    var lat = 40.4
    var lon = -3.7
    var runs = 0
    on change(lat, lon) => runs++

    Column #[p: 20, gap: 12] {
        Text("ran {runs}")
        Button("Move") => {
            lat = 38.7
            lon = -9.1
        }
        Button("North") => lat = 60.0
    }
}
"#,
    );
    app.tap("Move");
    assert!(app.shows("ran 1"), "{:?}", app.texts());
    app.tap("North");
    assert!(app.shows("ran 2"), "{:?}", app.texts());
}

/// A scope that is not on screen does not run, and coming back is a mount,
/// not a change: what moved while it was away is what it starts from.
#[test]
fn it_stops_with_its_scope() {
    let mut app = App::start(
        r#"
View Main() {
    var city = "Madrid"
    var open = true
    var runs = 0

    Column #[p: 20, gap: 12] {
        Text("ran {runs}")
        when open {
            Watcher(place: city, ran: { runs++ })
        }
        Button("Lisbon") => city = "Lisbon"
        Button("Porto") => city = "Porto"
        Button("Close") => open = false
        Button("Open") => open = true
    }
}

View Watcher(place: Str, ran: Fn()) {
    on change(place) => ran()
    Text("watching {place}")
}
"#,
    );
    app.tap("Close");
    app.tap("Lisbon");
    assert!(
        app.shows("ran 0"),
        "away, it does not run: {:?}",
        app.texts()
    );
    app.tap("Open");
    assert!(app.shows("watching Lisbon"), "{:?}", app.texts());
    assert!(
        app.shows("ran 0"),
        "coming back is not a change: {:?}",
        app.texts()
    );
    app.tap("Porto");
    assert!(app.shows("ran 1"), "{:?}", app.texts());
}

/// The type checker reads the watched values, not only the action.
#[test]
fn the_checker_reads_the_watched_values() {
    let parsed = parse(
        "View Main() {
    var city = \"Madrid\"
    var runs = 0
    on change(city.uper()) => runs++
    Text(\"{runs}\")
}",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let errors = format!(
        "{:?}",
        byard_compiler::infer::check_views(&parsed.views).errors
    );
    assert!(errors.contains("uper"), "{errors}");
}

#[test]
fn the_watched_values_need_their_parentheses() {
    let parsed = parse(
        "View Main() {
    var city = \"Madrid\"
    var runs = 0
    on change city => runs++
    Text(\"{runs}\")
}",
    );
    assert!(
        !parsed.errors.is_empty(),
        "it must not parse as something else"
    );
}
