//! A function a view defines once and calls from several places.
//!
//! `let load = () => { ... }` names an action, so `on mount` and a Retry
//! button can both run the same request rather than each spelling it out.
//! It used to check clean and then do nothing when called. Hot reload
//! defines view members through the same code, so a `fn` with parameters is
//! checked across one too.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::reload::diff_view;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent};

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
}

impl App {
    fn start(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
        };
        app.step(&[]);
        app.step(&[]);
        app
    }

    fn step(&mut self, events: &[InputEvent]) {
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame = RenderFrame::new();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 400.0);
    }

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn tap(&mut self, label: &str) {
        let line = self
            .frame
            .texts()
            .iter()
            .find(|t| t.text == label)
            .unwrap_or_else(|| panic!("{label} in {:?}", self.texts()));
        let pos = (line.x + 2.0, line.y + 2.0);
        let at = |kind, time_ms| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms,
        };
        self.step(&[at(EventKind::PointerDown, 0), at(EventKind::PointerUp, 10)]);
        self.step(&[]);
    }

    /// What `byard dev` does on save: reload, reload the view registry,
    /// lower the root again.
    fn reload(&mut self, src: &str, old: &str) {
        let (old, new) = (parse(old), parse(src));
        self.interp
            .reload(&new.views[0], diff_view(&old.views[0], &new.views[0]));
        self.interp.load_views(&new.views);
        let known: Vec<&str> = new.views.iter().map(|v| v.name.as_str()).collect();
        self.tree = self.interp.lower_view(&new.views[0], &known);
        assert!(
            self.interp.errors().is_empty(),
            "{:?}",
            self.interp.errors()
        );
        self.step(&[]);
    }
}

const SHARED: &str = r#"
View Main() {
    var runs = 0
    var state = "idle"
    let load = () => {
        runs = runs + 1
        state = "loaded"
    }
    on mount => load()
    Column {
        Text("{state} {runs}")
        Button("again") #[width: 80, height: 30] => load()
    }
}
"#;

#[test]
fn a_let_action_runs_from_on_mount_and_from_a_button() {
    let mut app = App::start(SHARED);
    assert_eq!(app.texts()[0], "loaded 1", "on mount ran the block");
    app.tap("again");
    assert_eq!(app.texts()[0], "loaded 2", "and so did the button");
}

#[test]
fn a_let_function_takes_its_arguments() {
    let app = App::start(
        r#"
View Main() {
    let label = (n, unit) => "{n} {unit}"
    let twice = x => x * 2
    Column { Text(label(3, "km")) Text("{twice(21)}") }
}
"#,
    );
    assert_eq!(app.texts(), ["3 km", "42"]);
}

const WITH_FN: &str = r#"
View Main() {
    var n = 20
    fn plus(x) => n + x
    Text("{plus(1)} {plus(2)}")
}
"#;

#[test]
fn a_fn_with_parameters_survives_a_hot_reload() {
    let mut app = App::start(WITH_FN);
    assert_eq!(app.texts(), ["21 22"]);
    app.reload(&WITH_FN.replace("n + x", "n + x * 10"), WITH_FN);
    assert_eq!(app.texts(), ["30 40"], "the edited body, still a function");
}
