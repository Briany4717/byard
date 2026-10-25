//! The events written on a `TextField` run.
//!
//! None of them did: the field registered its own editing and nothing the
//! author wrote, so `input =>`, `change =>`, `submit =>`, `key_down =>`,
//! `focus =>` and `blur =>` were accepted and never ran.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload};

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    t: u64,
}

impl App {
    /// A field with `events` written on it, a second field to take the focus
    /// away, and a line that shows what the handlers recorded.
    fn start(events: &str) -> Self {
        let src = format!(
            "View Main() {{
    var q: Str = \"\"
    var other: Str = \"\"
    var seen: Str = \"-\"
    var count: Int = 0
    Column #[p: 20, gap: 10] {{
        TextField #[bind: q, placeholder: \"first\", width: 200, height: 40, {events}]
        TextField #[bind: other, placeholder: \"second\", width: 200, height: 40]
        Text(\"seen={{seen}} count={{count}} q={{q}}\")
    }}
}}"
        );
        let parsed = parse(&src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &["Main"]);
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
            .render(&self.tree, &mut self.frame, 400.0, 300.0);
    }

    fn event(&mut self, kind: EventKind, pos: (f32, f32), payload: Option<InputPayload>) {
        self.t += 50;
        let e = InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods: byard_core::KeyModifiers::default(),
        };
        self.send(&[e]);
    }

    /// Clicks the field showing `placeholder`.
    fn focus(&mut self, placeholder: &str) {
        let line = self
            .frame
            .texts()
            .iter()
            .find(|t| t.text == placeholder)
            .unwrap_or_else(|| panic!("{placeholder}: {:?}", self.line()));
        let pos = (line.x + 2.0, line.y + 4.0);
        self.event(EventKind::PointerDown, pos, None);
        self.event(EventKind::PointerUp, pos, None);
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.event(
                EventKind::TextInput,
                (0.0, 0.0),
                Some(InputPayload::Key(c.to_string())),
            );
        }
    }

    fn key(&mut self, key: &str) {
        self.event(
            EventKind::KeyDown,
            (0.0, 0.0),
            Some(InputPayload::Key(key.into())),
        );
    }

    /// The line the handlers write to.
    fn line(&self) -> String {
        self.frame
            .texts()
            .iter()
            .map(|t| t.text.clone())
            .find(|t| t.starts_with("seen="))
            .unwrap_or_default()
    }
}

#[test]
fn input_runs_on_every_keystroke_and_sees_the_new_value() {
    let mut app = App::start("input => { seen = q count++ }");
    app.focus("first");
    app.type_text("lis");
    assert_eq!(app.line(), "seen=lis count=3 q=lis");
}

#[test]
fn input_is_told_the_new_value() {
    let mut app = App::start("input(text) => seen = text");
    app.focus("first");
    app.type_text("ab");
    assert_eq!(app.line(), "seen=ab count=0 q=ab");
}

#[test]
fn change_runs_on_every_edit_too() {
    let mut app = App::start("change => { seen = q count++ }");
    app.focus("first");
    app.type_text("ab");
    assert_eq!(app.line(), "seen=ab count=2 q=ab");
}

#[test]
fn deleting_is_an_edit_and_an_empty_backspace_is_not() {
    let mut app = App::start("input => { seen = q count++ }");
    app.focus("first");
    app.type_text("ab");
    app.key("Backspace");
    assert_eq!(app.line(), "seen=a count=3 q=a");
    app.key("Backspace");
    assert_eq!(app.line(), "seen= count=4 q=");
    app.key("Backspace");
    assert_eq!(app.line(), "seen= count=4 q=", "nothing left to delete");
}

#[test]
fn submit_runs_on_enter_and_only_on_enter() {
    let mut app = App::start("submit => { seen = q count++ }");
    app.focus("first");
    app.type_text("go");
    assert_eq!(app.line(), "seen=- count=0 q=go");
    app.key("Enter");
    assert_eq!(app.line(), "seen=go count=1 q=go");
}

/// A `key_down` written on the field does not take the field's own editing
/// away: Backspace still deletes.
#[test]
fn key_down_runs_and_the_field_still_edits() {
    let mut app = App::start("key_down(key) => { seen = key count++ }");
    app.focus("first");
    app.type_text("ab");
    app.key("Backspace");
    assert_eq!(app.line(), "seen=Backspace count=1 q=a");
    app.key("ArrowLeft");
    assert_eq!(app.line(), "seen=ArrowLeft count=2 q=a");
}

#[test]
fn focus_and_blur_run() {
    let mut app = App::start("focus => seen = \"in\", blur => { seen = \"out\" count++ }");
    app.focus("first");
    assert_eq!(app.line(), "seen=in count=0 q=");
    app.focus("second");
    assert_eq!(app.line(), "seen=out count=1 q=");
}

/// Typing into another field runs nothing here.
#[test]
fn only_the_focused_field_hears_it() {
    let mut app = App::start("input => count++, submit => count++");
    app.focus("second");
    app.type_text("xy");
    app.key("Enter");
    assert_eq!(app.line(), "seen=- count=0 q=");
}
