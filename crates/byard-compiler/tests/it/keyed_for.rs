//! RFC-0068 §4b: `for … key` keeps an item's mount while its key is in the
//! list, and a plain `for` stays positional.

use byard_compiler::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, KeyModifiers};

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    t: u64,
}

impl App {
    fn new(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let main = parsed.views.iter().find(|v| v.name.as_str() == "Main");
        let tree = interp.lower_view(main.unwrap(), &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            t: 0,
        };
        app.send(&[]);
        app
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.set_now_ms(u32::try_from(self.t).unwrap());
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 400.0);
    }

    fn wait(&mut self, total: u64) {
        let end = self.t + total;
        while self.t < end {
            self.t += 16;
            self.send(&[]);
        }
    }

    fn ev(&mut self, kind: EventKind, pos: (f32, f32)) -> InputEvent {
        self.t += 10;
        InputEvent {
            surface: byard_core::SurfaceId::MAIN,
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: self.t,
            mods: KeyModifiers::default(),
        }
    }

    fn tap(&mut self, x: f32, y: f32) {
        let d = self.ev(EventKind::PointerDown, (x, y));
        let u = self.ev(EventKind::PointerUp, (x, y));
        self.send(&[d, u]);
        self.send(&[]);
    }

    /// Taps the `+` button of the row at `row` (rows are 30 high).
    fn bump(&mut self, row: usize) {
        self.tap(80.0, f32::from(u8::try_from(row).unwrap()) * 30.0 + 15.0);
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, value);
        self.send(&[]);
    }

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }
}

fn strs(items: &[&str]) -> Value {
    Value::List(items.iter().map(|s| Value::Str((*s).to_string())).collect())
}

/// Rows of a name, a count that is the row's own `var`, and a button that adds
/// one. `{HEAD}` is the `for` line.
fn counters(head: &str) -> String {
    format!(
        r#"
View Main() {{
    var items = ["a", "b", "c"]
    Column #[gap: 0] {{
        {head} {{
            Counter(s)
        }}
    }}
}}
View Counter(name: Str) {{
    var count = 0
    Row #[height: 30, gap: 4] {{
        Text("{{name}}={{count}}") #[width: 60]
        Button("+") #[width: 40, height: 30] => count += 1
    }}
}}
"#
    )
}

fn rows(app: &App) -> Vec<String> {
    app.texts()
        .into_iter()
        .filter(|t| t.contains('='))
        .collect()
}

#[test]
fn a_keyed_item_keeps_its_state_when_one_is_inserted_in_front() {
    let mut app = App::new(&counters("for s in items key s"));
    app.bump(0);
    app.bump(0);
    app.bump(2);
    assert_eq!(rows(&app), ["a=2", "b=0", "c=1"]);
    app.set("items", strs(&["z", "a", "b", "c"]));
    assert_eq!(rows(&app), ["z=0", "a=2", "b=0", "c=1"]);
    // The kept rows still run their own `var`.
    app.bump(1);
    assert_eq!(rows(&app), ["z=0", "a=3", "b=0", "c=1"]);
}

#[test]
fn without_key_a_for_is_positional_as_it_was() {
    let mut app = App::new(&counters("for s in items"));
    app.bump(0);
    app.bump(0);
    app.set("items", strs(&["z", "a", "b", "c"]));
    // The state stayed at the position, not with the item.
    assert_eq!(rows(&app), ["z=2", "a=0", "b=0", "c=0"]);
}

#[test]
fn removing_from_the_middle_and_reordering_keep_each_items_state() {
    let mut app = App::new(&counters("for s in items key s"));
    app.bump(0);
    app.bump(1);
    app.bump(1);
    app.bump(2);
    app.bump(2);
    app.bump(2);
    assert_eq!(rows(&app), ["a=1", "b=2", "c=3"]);
    app.set("items", strs(&["a", "c"]));
    assert_eq!(rows(&app), ["a=1", "c=3"]);
    app.set("items", strs(&["c", "a", "d"]));
    assert_eq!(rows(&app), ["c=3", "a=1", "d=0"]);
    // A key that went and came back is a new mount.
    app.set("items", strs(&["b", "c"]));
    assert_eq!(rows(&app), ["b=0", "c=3"]);
}

#[test]
fn the_index_follows_the_item() {
    let src = r#"
View Main() {
    var items = ["a", "b"]
    Column {
        for i, s in items key s {
            Text("{i}:{s}")
        }
    }
}
"#;
    let mut app = App::new(src);
    assert_eq!(app.texts(), ["0:a", "1:b"]);
    app.set("items", strs(&["z", "a", "b"]));
    assert_eq!(app.texts(), ["0:z", "1:a", "2:b"]);
    app.set("items", strs(&["b", "z"]));
    assert_eq!(app.texts(), ["0:b", "1:z"]);
}

#[test]
fn a_changed_field_of_a_kept_item_is_read_anew() {
    let src = r#"
type Task = { id: Int, title: Str }
View Main() {
    var tasks: List<Task> = []
    Column #[gap: 0] {
        for t in tasks key t.id {
            Counter(t.title)
        }
    }
}
View Counter(name: Str) {
    var count = 0
    Row #[height: 30] {
        Text("{name}={count}") #[width: 60]
        Button("+") #[width: 40, height: 30] => count += 1
    }
}
"#;
    let task = |id: i64, title: &str| {
        Value::Record(vec![
            (Symbol::intern("id"), Value::Int(id)),
            (Symbol::intern("title"), Value::Str(title.to_string())),
        ])
    };
    let mut app = App::new(src);
    app.set("tasks", Value::List(vec![task(1, "one"), task(2, "two")]));
    app.bump(1);
    assert_eq!(rows(&app), ["one=0", "two=1"]);
    app.set(
        "tasks",
        Value::List(vec![task(2, "two, renamed"), task(1, "one")]),
    );
    assert_eq!(rows(&app), ["two, renamed=1", "one=0"]);
}

#[test]
fn a_removed_item_plays_on_leave_and_is_then_gone() {
    let src = r#"
View Main() {
    var items = ["a", "b", "c"]
    Column #[gap: 0] {
        for s in items key s {
            Box #[width: 100, height: 30, bg: 0xFF0000, opacity: 1.0 with anim.linear(200ms)] {
                on leave { opacity: 0.0 }
                Text("{s}")
            }
        }
    }
}
"#;
    let mut app = App::new(src);
    app.wait(300);
    assert_eq!(app.texts(), ["a", "b", "c"]);
    app.set("items", strs(&["a", "c"]));
    app.wait(64);
    // Mid-leave: `b` is still drawn, after the live rows.
    assert_eq!(app.texts(), ["a", "c", "b"]);
    app.wait(500);
    assert_eq!(app.texts(), ["a", "c"]);
}

#[test]
fn a_leaving_item_that_returns_is_the_same_mount() {
    let src = r#"
View Main() {
    var items = ["a", "b"]
    Column #[gap: 0] {
        for s in items key s {
            Seven(s)
        }
    }
}
View Seven(name: Str) {
    var n = 7
    Box #[width: 100, height: 30, bg: 0xFF0000, opacity: 1.0 with anim.linear(400ms)] {
        on leave { opacity: 0.0 }
        Text("{name}{n}")
    }
}
"#;
    let mut app = App::new(src);
    app.wait(300);
    app.set("items", strs(&["a"]));
    app.wait(32);
    assert_eq!(app.texts(), ["a7", "b7"]);
    app.set("items", strs(&["a", "b"]));
    app.wait(700);
    assert_eq!(app.texts(), ["a7", "b7"]);
}

#[test]
fn two_items_with_one_key_are_reported_and_the_second_is_not_drawn() {
    let mut app = App::new(&counters("for s in items key s"));
    app.set("items", strs(&["a", "b", "a"]));
    assert_eq!(rows(&app), ["a=0", "b=0"]);
    let dups: Vec<&CompileError> = app
        .interp
        .errors()
        .iter()
        .filter(|e| matches!(e, CompileError::DuplicateForKey { .. }))
        .collect();
    assert_eq!(dups.len(), 1, "told once: {:?}", app.interp.errors());
    assert!(
        dups[0].headline().contains("\"a\""),
        "{}",
        dups[0].headline()
    );
    // Frame after frame, still once.
    app.wait(100);
    let again = app
        .interp
        .errors()
        .iter()
        .filter(|e| matches!(e, CompileError::DuplicateForKey { .. }))
        .count();
    assert_eq!(again, 1);
}

#[test]
fn a_binding_called_key_still_works() {
    let src = r#"
View Main() {
    var key = 3
    let doubled = key * 2
    var items = ["a", "b"]
    Column {
        Text("{key} {doubled}")
        for key in items { Text("k{key}") }
        for s in items key s { Text("s{s}") }
    }
}
"#;
    let app = App::new(src);
    assert_eq!(app.texts(), ["3 6", "ka", "kb", "sa", "sb"]);
}

#[test]
fn a_key_that_is_a_record_matches_by_its_fields() {
    let src = r#"
type Pos = { x: Int, y: Int }
View Main() {
    var cells: List<Pos> = []
    Column #[gap: 0] {
        for c in cells key c {
            Counter("{c.x},{c.y}")
        }
    }
}
View Counter(name: Str) {
    var count = 0
    Row #[height: 30] {
        Text("{name}={count}") #[width: 60]
        Button("+") #[width: 40, height: 30] => count += 1
    }
}
"#;
    let pos = |x: i64, y: i64| {
        Value::Record(vec![
            (Symbol::intern("x"), Value::Int(x)),
            (Symbol::intern("y"), Value::Int(y)),
        ])
    };
    let mut app = App::new(src);
    app.set("cells", Value::List(vec![pos(0, 0), pos(1, 0)]));
    app.bump(1);
    app.set("cells", Value::List(vec![pos(5, 5), pos(0, 0), pos(1, 0)]));
    assert_eq!(rows(&app), ["5,5=0", "0,0=0", "1,0=1"]);
}

const SLIDES: &str = r#"
View Main() {
    var items = [30, 50]
    Column #[gap: 0] {
        for n in items KEY {
            Slide(n)
        }
    }
}
View Slide(h: Int) {
    var wide = false
    Row #[gap: 0] {
        Button("go") #[width: 40, height: 20] => wide = !wide
        Box #[width: 100, height: h, bg: wide ? 0xFF0000 : 0x0000FF with anim.linear(1000ms)]
    }
}
"#;

/// How red the box `h` high is (it is blue at rest, red once it has grown).
fn opacity_of(app: &App, h: f32) -> f32 {
    app.frame
        .instances()
        .iter()
        .find(|b| (b.rect[3] - h).abs() < 0.5 && (b.rect[2] - 100.0).abs() < 0.5)
        .map_or(-1.0, |b| b.color[0])
}

#[test]
fn a_keyed_items_animation_keeps_going_across_a_reorder() {
    let mut app = App::new(&SLIDES.replace("KEY", "key n"));
    app.wait(100);
    let rest = opacity_of(&app, 50.0);
    // Start the 50-high item's animation (its button is in the second row).
    app.tap(20.0, 40.0);
    app.wait(300);
    let before = opacity_of(&app, 50.0);
    assert!(
        before > rest + 0.1 && before < 0.95,
        "mid-flight: {rest} then {before}"
    );
    app.set("items", Value::List(vec![Value::Int(50), Value::Int(30)]));
    let after = opacity_of(&app, 50.0);
    assert!(
        (after - before).abs() < 0.15,
        "the same animation, from where it was: {before} then {after}"
    );
    app.wait(200);
    assert!(opacity_of(&app, 50.0) > after, "and it keeps going");
    // The other item never moved.
    assert!((opacity_of(&app, 30.0) - rest).abs() < 0.01);
}

#[test]
fn a_plain_fors_animation_stays_with_the_position_under_a_reorder() {
    // What a plain `for` has always done: the animation belongs to the slot,
    // so after the reorder the slot's new item (the 30-high one) is the one
    // mid-flight, and the 50-high item shows its slot's resting value.
    let mut app = App::new(&SLIDES.replace("KEY", ""));
    app.wait(100);
    let rest = opacity_of(&app, 50.0);
    app.tap(20.0, 40.0);
    app.wait(300);
    let before = opacity_of(&app, 50.0);
    assert!(before > rest + 0.1, "mid-flight: {rest} then {before}");
    app.set("items", Value::List(vec![Value::Int(50), Value::Int(30)]));
    assert!(
        (opacity_of(&app, 50.0) - rest).abs() < 0.01,
        "{}",
        opacity_of(&app, 50.0)
    );
    assert!(
        opacity_of(&app, 30.0) > rest + 0.1,
        "{}",
        opacity_of(&app, 30.0)
    );
}

#[test]
fn a_key_on_a_canvas_or_path_for_is_an_error_not_a_silent_no_op() {
    for body in [
        "Canvas #[width: 100, height: 100] { for i in [1, 2] key i { circle(10, 10, 4) } }",
        "Canvas #[width: 100, height: 100] { path(fill: 0xFF0000) { move(0, 0)\n for i in [1, 2] key i { line(i, 5) } } }",
    ] {
        let parsed = parse(&format!("View Main() {{ {body} }}"));
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let _ = interp.lower_view(&parsed.views[0], &["Main"]);
        let found = interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::KeyInCanvasFor { .. }));
        assert!(found, "{body}: {:?}", interp.errors());
    }
}
