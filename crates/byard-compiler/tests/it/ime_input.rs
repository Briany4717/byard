//! IME composition in a `TextField` (RFC-0040), driven by synthetic events,
//! which is how it is tested without a platform input method (§9).
//!
//! The claim the RFC exists for is the first test: the preedit is drawn but is
//! never the value. The rest are the rules around it: a commit replaces the
//! preedit, the IME owns the editing keys while it composes, focus loss
//! discards rather than commits, a late commit lands in the field that owned
//! the composition, and the caret is reported in viewport space.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, ImeEvent, InputEvent, InputPayload};

/// Two fields, the first inside a translated box so its caret has to go
/// through a transform to reach viewport space, the second inside a scrolled
/// `ScrollView` so its caret has to go through a scroll offset.
const SRC: &str = r"
View Main() {
    var name = ''
    var other = ''
    var scroll = 30
    Column #[p: 20, gap: 20, width: 400, height: 400] {
        Box #[translate: (40, 0)] {
            TextField #[bind: name, width: 240, height: 40]
        }
        ScrollView #[width: 300, height: 100, offset: (0, scroll)] {
            Column #[gap: 0] {
                Box #[height: 50] {}
                TextField #[bind: other, width: 240, height: 40]
                Box #[height: 200] {}
            }
        }
    }
}
";

/// Centre of the first field, after its box's translate.
const A: (f32, f32) = (180.0, 40.0);
/// Centre of the second field: its layout y is 80 + 50, less the 30 scrolled.
const B: (f32, f32) = (140.0, 120.0);

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    t: u64,
}

impl App {
    fn new() -> Self {
        let parsed = parse(&SRC.replace('\'', "\""));
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            t: 0,
        };
        app.render();
        app
    }

    fn render(&mut self) {
        self.interp.tick();
        self.frame = RenderFrame::new();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 400.0);
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.dispatch_events(events);
        self.render();
    }

    fn event(
        &mut self,
        kind: EventKind,
        pos: (f32, f32),
        payload: Option<InputPayload>,
    ) -> InputEvent {
        self.t += 50;
        InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
        }
    }

    fn tap(&mut self, pos: (f32, f32)) {
        let down = self.event(EventKind::PointerDown, pos, None);
        let up = self.event(EventKind::PointerUp, pos, None);
        self.send(&[down, up]);
    }

    fn type_text(&mut self, text: &str) {
        let e = self.event(
            EventKind::TextInput,
            (0.0, 0.0),
            Some(InputPayload::Key(text.into())),
        );
        self.send(&[e]);
    }

    fn key(&mut self, key: &str) {
        let e = self.event(
            EventKind::KeyDown,
            (0.0, 0.0),
            Some(InputPayload::Key(key.into())),
        );
        self.send(&[e]);
    }

    /// What a host does with a platform IME event.
    fn ime(&mut self, events: Vec<ImeEvent>) {
        self.t += 50;
        let t = self.t;
        let inputs: Vec<InputEvent> = events.into_iter().filter_map(|e| e.into_input(t)).collect();
        self.send(&inputs);
    }

    fn preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        self.ime(vec![ImeEvent::Preedit {
            text: text.into(),
            cursor,
        }]);
    }

    /// Exactly what winit sends to commit: an empty preedit, then the text.
    fn commit(&mut self, text: &str) {
        self.ime(vec![
            ImeEvent::Preedit {
                text: String::new(),
                cursor: None,
            },
            ImeEvent::Commit(text.into()),
        ]);
    }

    fn var(&self, name: &str) -> String {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .expect("declared");
        match self.interp.peek(sig) {
            Value::Str(s) => s,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn drawn(&self, text: &str) -> Option<(f32, f32)> {
        self.frame
            .texts()
            .iter()
            .find(|l| l.text == text)
            .map(|l| (l.x, l.y))
    }

    fn caret(&self) -> Option<byard_core::frame::Rect> {
        self.frame.text_input().map(|t| t.caret)
    }
}

/// §1: the preedit is drawn, after the value, and the value does not change
/// until the IME commits; the commit replaces the preedit with the text.
#[test]
fn the_preedit_is_drawn_but_is_not_the_value_until_it_commits() {
    let mut app = App::new();
    app.tap(A);
    app.type_text("ab");
    app.preedit("にほん", Some((9, 9)));
    assert_eq!(app.var("name"), "ab", "a preedit never reaches the var");
    let (vx, _) = app.drawn("ab").expect("the value is drawn");
    let (px, _) = app.drawn("にほん").expect("the preedit is drawn");
    assert!(px > vx + 1.0, "the preedit sits after the value: {vx} {px}");

    app.preedit("日本", Some((6, 6)));
    assert_eq!(app.var("name"), "ab", "a conversion is still not the value");
    assert!(app.drawn("にほん").is_none() && app.drawn("日本").is_some());

    app.commit("日本");
    assert_eq!(app.var("name"), "ab日本");
    assert!(app.drawn("日本").is_none(), "a commit clears the preedit");
    assert!(app.drawn("ab日本").is_some());
}

/// §3: while a preedit shows, Backspace and Enter are the IME's.
#[test]
fn editing_keys_belong_to_the_ime_while_it_composes() {
    let mut app = App::new();
    app.tap(A);
    app.type_text("ab");
    app.preedit("か", Some((3, 3)));
    app.key("Backspace");
    assert_eq!(
        app.var("name"),
        "ab",
        "Backspace edited the preedit, not the value"
    );

    // With the preedit gone, Backspace is the field's again.
    app.commit("か");
    app.key("Backspace");
    assert_eq!(app.var("name"), "ab");
}

/// §6: focus moving away discards the preedit and commits nothing; a late
/// platform commit still lands in the field that owned the composition, and
/// typing in the newly focused field goes there.
#[test]
fn focus_loss_discards_and_a_late_commit_goes_to_the_owner() {
    let mut app = App::new();
    app.tap(A);
    app.type_text("x");
    app.preedit("かな", Some((6, 6)));
    app.tap(B);
    assert!(app.drawn("かな").is_none(), "the preedit is gone");
    assert_eq!(app.var("name"), "x", "and nothing was committed");
    assert_eq!(app.var("other"), "");

    // Discarded, not merely hidden: coming back does not bring it back.
    app.tap(A);
    assert!(
        app.drawn("かな").is_none(),
        "a discarded preedit stays gone"
    );
    app.tap(B);

    app.type_text("y");
    assert_eq!(app.var("other"), "y", "typing goes to the focused field");

    app.commit("仮名");
    assert_eq!(app.var("name"), "x仮名", "the commit goes to its owner");
    assert_eq!(app.var("other"), "y");

    // The owner is spent: the next commit goes to whatever has focus.
    app.preedit("は", Some((3, 3)));
    app.commit("葉");
    assert_eq!(app.var("other"), "y葉");
}

/// The IME turning off ends the composition with nothing committed.
#[test]
fn the_ime_turning_off_ends_the_composition() {
    let mut app = App::new();
    app.tap(A);
    app.preedit("かな", None);
    app.ime(vec![ImeEvent::Disabled]);
    assert!(app.drawn("かな").is_none());
    assert_eq!(app.var("name"), "");
}

/// §4: the caret is reported only while a field has focus, in viewport
/// space (through a transform and through a scroll offset), and it follows
/// the IME's cursor inside the preedit.
#[test]
fn the_caret_is_reported_in_viewport_space() {
    let mut app = App::new();
    assert!(app.caret().is_none(), "no focus, no caret, no IME");

    app.tap(A);
    let empty = app.caret().expect("a focused field reports its caret");
    // Layout x 20, the box's translate 40, the field's 10 of padding.
    assert!(
        (empty.x - 71.0).abs() < 1.0 && empty.y > 20.0 && empty.y < 60.0,
        "through the transform: {empty:?}"
    );

    app.preedit("かなかな", Some((12, 12)));
    let end = app.caret().unwrap();
    app.preedit("かなかな", Some((3, 3)));
    let inside = app.caret().unwrap();
    assert!(
        end.x > inside.x + 5.0 && inside.x > empty.x + 5.0,
        "the caret follows the IME cursor: {empty:?} {inside:?} {end:?}"
    );

    app.tap(B);
    let scrolled = app.caret().expect("the second field reports its caret");
    // Layout y 80 + 50 inside the scroll view, less the 30 scrolled.
    assert!(
        scrolled.y > 100.0 && scrolled.y < 140.0,
        "through the scroll offset: {scrolled:?}"
    );
}

/// §7: Backspace removes a grapheme cluster.
#[test]
fn backspace_removes_a_whole_grapheme() {
    let mut app = App::new();
    app.tap(A);
    app.type_text("e\u{301}");
    app.type_text("\u{1F1EF}\u{1F1F5}");
    app.key("Backspace");
    assert_eq!(app.var("name"), "e\u{301}", "the flag went whole");
    app.key("Backspace");
    assert_eq!(app.var("name"), "", "the accent went with its letter");
}
