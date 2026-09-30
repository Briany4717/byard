//! An `if` statement inside an action.
//!
//! It used to parse, check clean and never run: the language had no `if`
//! production, so `if n > 3.0 { n = 0.0 }` read as three unrelated statements
//! and the guard clause every action needs to stay inside its bounds did
//! nothing. Each test drives a real view and reads what it renders.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent};

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    /// Advances per tap so consecutive taps are distinct gestures.
    clock: u64,
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
            clock: 0,
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
        self.clock += 1000;
        let base = self.clock;
        let at = |kind, time_ms| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: base + time_ms,
        };
        self.step(&[at(EventKind::PointerDown, 0), at(EventKind::PointerUp, 10)]);
        self.step(&[]);
    }
}

/// A view with one text and one button `bump` whose handler is `action`.
fn view(vars: &str, action: &str) -> String {
    format!(
        r#"
View Main() {{
    {vars}
    Column {{
        Text("n = {{n}}")
        Button("bump") #[width: 80, height: 30] => {{
            {action}
        }}
    }}
}}
"#
    )
}

fn taps(app: &mut App, times: usize) -> Vec<String> {
    (0..times)
        .map(|_| {
            app.tap("bump");
            app.texts()[0].clone()
        })
        .collect()
}

#[test]
fn an_if_guard_clamps_a_value_after_the_statement_before_it() {
    let mut app = App::start(&view("var n = 1.0", "n = n * 2.0\n if n > 3.0 { n = 0.0 }"));
    assert_eq!(taps(&mut app, 4), ["n = 2", "n = 0", "n = 0", "n = 0"]);
}

#[test]
fn an_if_else_runs_exactly_one_branch() {
    let mut app = App::start(&view(
        "var n = 0",
        "if n < 2 { n = n + 1 } else { n = n + 10 }",
    ));
    assert_eq!(taps(&mut app, 4), ["n = 1", "n = 2", "n = 12", "n = 22"]);
}

#[test]
fn an_else_if_chain_picks_the_first_true_arm() {
    let mut app = App::start(&view(
        "var n = 0",
        "n++\n if n == 1 { n = 10 } else if n == 11 { n = 20 } else if n == 21 { n = 30 } else { n = 0 }",
    ));
    assert_eq!(
        taps(&mut app, 5),
        ["n = 10", "n = 20", "n = 30", "n = 0", "n = 10"]
    );
}

#[test]
fn a_nested_if_runs_only_when_both_conditions_hold() {
    let mut app = App::start(&view(
        "var n = 0",
        "n++\n if n > 1 { if n < 4 { n = n + 100 } }",
    ));
    assert_eq!(taps(&mut app, 3), ["n = 1", "n = 102", "n = 103"]);
}

#[test]
fn an_if_without_an_else_leaves_the_value_alone_when_false() {
    let mut app = App::start(&view("var n = 5", "if n > 100 { n = 0 }"));
    assert_eq!(taps(&mut app, 2), ["n = 5", "n = 5"]);
}

#[test]
fn an_if_runs_inside_on_mount() {
    let app = App::start(
        r#"
View Main() {
    var n = 7
    on mount => { if n > 5 { n = 1 } else { n = 2 } }
    Text("n = {n}")
}
"#,
    );
    assert_eq!(app.texts(), ["n = 1"]);
}

#[test]
fn an_if_runs_inside_a_let_action() {
    let mut app = App::start(
        r#"
View Main() {
    var n = 1
    let step = () => {
        n = n * 2
        if n > 3 { n = 0 }
    }
    Column {
        Text("n = {n}")
        Button("bump") #[width: 80, height: 30] => step()
    }
}
"#,
    );
    assert_eq!(taps(&mut app, 4), ["n = 2", "n = 0", "n = 0", "n = 0"]);
}

#[test]
fn an_if_reads_a_list_length_as_a_guard() {
    let mut app = App::start(&view(
        "var n = 0\n var xs = [1, 2, 3]",
        "if xs.len > 0 { n = n + 1 }\n xs = xs.filter(x => x > 1)",
    ));
    // xs: [1,2,3] -> [2,3] -> [2,3]; the guard holds every time.
    assert_eq!(taps(&mut app, 2), ["n = 1", "n = 2"]);
}

#[test]
fn if_is_still_an_ordinary_name_where_no_condition_follows() {
    let parsed = parse("View Main() { var if = 3\n Text(\"{if}\") }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
}
