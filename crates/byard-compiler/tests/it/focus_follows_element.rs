//! Focus, caret, selection and undo belong to the element, not to the place
//! its layout node happens to take in the build order.
//!
//! A `when` or `for` before a field adds or removes nodes in front of it, and
//! the field's index moves with them. Typing must go on regardless: one
//! character per frame, which is what a keyboard does and what hides nothing.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};

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
        let tree = interp.lower_view(&parsed.views[0], &known);
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
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 500.0, 300.0);
    }

    fn event(&mut self, kind: EventKind, payload: Option<InputPayload>, shift: bool) -> InputEvent {
        self.t += 10;
        InputEvent {
            kind,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods: KeyModifiers {
                shift,
                ..KeyModifiers::default()
            },
        }
    }

    fn tap(&mut self, x: f32, y: f32) {
        let mut down = self.event(EventKind::PointerDown, None, false);
        down.pos = (x, y);
        let mut up = self.event(EventKind::PointerUp, None, false);
        up.pos = (x, y);
        self.send(&[down, up]);
    }

    /// One character, one frame.
    fn type_char(&mut self, c: char) {
        let e = self.event(
            EventKind::TextInput,
            Some(InputPayload::Key(c.to_string())),
            false,
        );
        self.send(&[e]);
    }

    fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            self.type_char(c);
        }
    }

    fn key(&mut self, name: &str, mods: KeyModifiers) {
        let mut down = self.event(
            EventKind::KeyDown,
            Some(InputPayload::Key(name.into())),
            false,
        );
        down.mods = mods;
        let mut up = self.event(
            EventKind::KeyUp,
            Some(InputPayload::Key(name.into())),
            false,
        );
        up.mods = mods;
        self.send(&[down, up]);
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, value);
        self.send(&[]);
    }

    fn get(&self, name: &str) -> Value {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.peek(sig)
    }

    fn text(&self, name: &str) -> String {
        match self.get(name) {
            Value::Str(s) => s,
            other => panic!("{name} is {other:?}"),
        }
    }
}

const FIELD: (f32, f32) = (150.0, 28.0);

/// The field after a `when` that turns true as soon as there is text: the
/// repro of "typing goes dead after the first character".
const WHEN_BEFORE: &str = r#"
View Main() {
    var a = ""
    Row #[p: 10, gap: 8] {
        Text("inline")
        when a.len > 0 { Text("has text") }
        TextField #[bind: a, width: 200, height: 36]
    }
}
"#;

#[test]
fn a_when_before_the_field_turning_true_keeps_the_focus() {
    let mut app = App::new(WHEN_BEFORE);
    app.tap(FIELD.0, FIELD.1);
    app.type_str("abc");
    assert_eq!(app.text("a"), "abc");
    assert!(app.interp.focused_selection().is_some(), "still focused");
}

#[test]
fn the_when_in_a_child_view_before_the_field_is_the_same() {
    let mut app = App::new(
        r#"
View Main() {
    var a = ""
    Row #[p: 10, gap: 8] {
        Text("inline")
        Msg(text: a)
        TextField #[bind: a, width: 200, height: 36]
    }
}
View Msg(text: String) {
    when text.len > 0 { Text("has text") }
}
"#,
    );
    app.tap(FIELD.0, FIELD.1);
    app.type_str("abc");
    assert_eq!(app.text("a"), "abc");
}

#[test]
fn a_for_before_the_field_gaining_and_losing_items_keeps_the_focus() {
    let mut app = App::new(
        r#"
View Main() {
    var a = ""
    var items = ["x"]
    Row #[p: 10, gap: 8] {
        for it in items { Text(it) }
        TextField #[bind: a, width: 200, height: 36]
    }
}
"#,
    );
    app.tap(150.0, 28.0);
    app.type_str("ab");
    let two = Value::List(vec![Value::Str("x".into()), Value::Str("y".into())]);
    app.set("items", two);
    app.type_char('c');
    app.set("items", Value::List(Vec::new()));
    app.type_char('d');
    assert_eq!(app.text("a"), "abcd");
}

#[test]
fn a_when_after_the_field_changing_keeps_the_focus() {
    let mut app = App::new(
        r#"
View Main() {
    var a = ""
    Row #[p: 10, gap: 8] {
        TextField #[bind: a, width: 200, height: 36]
        when a.len > 0 { Text("has text") }
    }
}
"#,
    );
    app.tap(60.0, 28.0);
    app.type_str("abc");
    assert_eq!(app.text("a"), "abc");
}

#[test]
fn a_field_inside_a_when_that_stays_true_follows_a_sibling_when_before_it() {
    let mut app = App::new(
        r#"
View Main() {
    var a = ""
    var open = true
    when open {
        Row #[p: 10, gap: 8] {
            Text("inline")
            when a.len > 0 { Text("has text") }
            TextField #[bind: a, width: 200, height: 36]
        }
    }
}
"#,
    );
    app.tap(FIELD.0, FIELD.1);
    app.type_str("abc");
    assert_eq!(app.text("a"), "abc");
}

#[test]
fn a_field_in_an_overlay_keeps_its_focus_when_the_page_before_it_changes() {
    let mut app = App::new(
        r#"
View Main() {
    var a = ""
    var more = false
    Row #[p: 10, gap: 8] {
        Text("page")
        when more { Text("more") }
    }
    Overlay #[modal: false] {
        Row #[anchor: center, p: 10, gap: 8] {
            Text("inline")
            when a.len > 0 { Text("has text") }
            TextField #[bind: a, width: 200, height: 36]
        }
    }
}
"#,
    );
    // The row is centred; its field is the right-hand part of it.
    app.tap(300.0, 150.0);
    app.type_char('a');
    app.set("more", Value::Bool(true));
    app.type_char('b');
    app.set("more", Value::Bool(false));
    app.type_char('c');
    assert_eq!(app.text("a"), "abc");
}

#[test]
fn the_caret_and_the_selection_survive_a_relayout() {
    let mut app = App::new(WHEN_BEFORE);
    app.tap(FIELD.0, FIELD.1);
    app.type_str("hello");
    // The `when` is open by now; shift+Left twice selects "lo".
    let shift = KeyModifiers {
        shift: true,
        ..KeyModifiers::default()
    };
    app.key("ArrowLeft", shift);
    app.key("ArrowLeft", shift);
    assert_eq!(app.interp.focused_selection(), Some((5, 3)));
    // Clearing the field closes the `when` and opens it again as it is typed
    // into; here the selection must still be the same after each relayout.
    app.interp.write_var(
        app.interp.var_signal(&Symbol::intern("a")).unwrap(),
        Value::Str("hello".into()),
    );
    app.send(&[]);
    assert_eq!(app.interp.focused_selection(), Some((5, 3)));
    app.type_char('X');
    assert_eq!(app.text("a"), "helX");
    assert_eq!(app.interp.focused_selection(), Some((4, 4)));
}

#[test]
fn undo_history_goes_with_the_field_through_a_relayout() {
    let mut app = App::new(WHEN_BEFORE);
    app.tap(FIELD.0, FIELD.1);
    app.type_str("abc");
    let modk = KeyModifiers::parse_chord("mod+a").unwrap().0;
    app.key("z", modk);
    // One undo takes the typed run back to where it began, whatever the
    // layout did in between: the `when` opened after the first character.
    assert_eq!(app.text("a"), "");
}

#[test]
fn tab_still_walks_the_fields_in_order_after_a_relayout() {
    let mut app = App::new(
        r#"
View Main() {
    var a = ""
    var b = ""
    var fa = false
    var fb = false
    Row #[p: 10, gap: 8] {
        when a.len > 0 { Text("has text") }
        TextField #[bind: a, focused: fa, width: 100, height: 36]
        TextField #[bind: b, focused: fb, width: 100, height: 36]
    }
}
"#,
    );
    app.tap(60.0, 28.0);
    app.type_str("ab");
    assert_eq!(app.get("fa"), Value::Bool(true));
    app.key("Tab", KeyModifiers::default());
    assert_eq!(app.get("fa"), Value::Bool(false));
    assert_eq!(app.get("fb"), Value::Bool(true));
    app.type_str("xy");
    assert_eq!(app.text("a"), "ab");
    assert_eq!(app.text("b"), "xy");
}

#[test]
fn a_focused_field_that_goes_away_gives_up_the_focus() {
    let mut app = App::new(
        r#"
View Main() {
    var a = ""
    var fa = false
    var open = true
    Row #[p: 10, gap: 8] {
        when open { TextField #[bind: a, focused: fa, width: 200, height: 36] }
    }
}
"#,
    );
    app.tap(60.0, 28.0);
    assert_eq!(app.get("fa"), Value::Bool(true));
    app.set("open", Value::Bool(false));
    app.send(&[]);
    assert_eq!(app.get("fa"), Value::Bool(false));
}

// ── The rest of what is keyed by an element: scroll, press, hover ──────────

impl App {
    fn pointer(&mut self, kind: EventKind, x: f32, y: f32) {
        let mut e = self.event(kind, None, false);
        e.pos = (x, y);
        self.send(&[e]);
    }

    fn wheel(&mut self, x: f32, y: f32, dy: f32) {
        let mut e = self.event(EventKind::Wheel, None, false);
        e.pos = (x, y);
        e.delta = (0.0, dy);
        self.send(&[e]);
    }

    fn int(&self, name: &str) -> i64 {
        match self.get(name) {
            Value::Int(n) => n,
            #[allow(clippy::cast_possible_truncation)]
            Value::Float(f) => f as i64,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn drawn(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }
}

const SCROLLED: &str = r#"
View Main() {
    var more = false
    var off = 0
    Row {
        when more { Text("has text") }
        ScrollView #[width: 200, height: 100, offset: (0, off)] {
            Column {
                for i in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19] { Box #[height: 30] { Text("row {i}") } }
            }
        }
    }
}
"#;

#[test]
fn a_scroll_offset_and_the_rows_in_view_survive_a_when_before_the_view() {
    let mut app = App::new(SCROLLED);
    app.set("off", Value::Int(300));
    let before = app.drawn();
    assert!(before.iter().any(|t| t == "row 10"), "{before:?}");
    app.set("more", Value::Bool(true));
    assert_eq!(app.int("off"), 300);
    let after: Vec<String> = app
        .drawn()
        .into_iter()
        .filter(|t| t.starts_with("row"))
        .collect();
    let before: Vec<String> = before
        .into_iter()
        .filter(|t| t.starts_with("row"))
        .collect();
    assert_eq!(after, before, "the same rows stay visible");
}
const PRESSABLE: &str = r"
View Main() {
    var more = false
    var count = 0
    let hot = style { bg: 0x111111 on hover { bg: 0x00FF00 } }
    Row {
        when more { Box #[width: 30, height: 40] {} }
        Box #[..hot, width: 100, height: 40] => count++
    }
}
";

#[test]
fn a_press_in_flight_still_taps_its_element_after_a_when_before_it_opens() {
    let mut app = App::new(PRESSABLE);
    app.pointer(EventKind::PointerDown, 90.0, 20.0);
    app.set("more", Value::Bool(true));
    app.pointer(EventKind::PointerUp, 90.0, 20.0);
    assert_eq!(app.int("count"), 1);
}

#[test]
fn the_hovered_element_stays_hovered_after_a_when_before_it_opens() {
    let mut app = App::new(PRESSABLE);
    app.pointer(EventKind::PointerMove, 90.0, 20.0);
    let green = |app: &App| app.frame.instances().iter().any(|i| i.color[1] > 0.5);
    assert!(green(&app), "hover applies");
    app.set("more", Value::Bool(true));
    assert!(green(&app), "and goes on applying to the same element");
}

#[test]
fn end_reached_does_not_fire_again_when_a_when_before_the_view_opens() {
    let mut app = App::new(
        r"
View Main() {
    var more = false
    var off = 0
    var loads = 0
    Row {
        when more { Box #[width: 30, height: 100] {} }
        ScrollView #[offset: (0, off), width: 100, height: 100,
                     end_threshold: 0.8, end_reached => loads++] {
            Column { Box #[width: 100, height: 400] {} }
        }
    }
}
",
    );
    app.wheel(50.0, 50.0, -8.0);
    assert_eq!(app.int("loads"), 1, "fired once past the threshold");
    app.set("more", Value::Bool(true));
    app.wheel(80.0, 50.0, -0.1);
    assert_eq!(app.int("loads"), 1, "still the same view, still fired");
}
