//! Editing in a `TextField` (RFC-0041): a caret that moves, a selection, the
//! clipboard and undo, driven through the interpreter with synthetic events,
//! which is how every other input is tested.
//!
//! The model has its own unit tests (`interp/edit.rs`). These are the ones
//! that need the whole path: a modifier travelling from the event to the field,
//! a press landing where the text was drawn, a value written from an action,
//! and what `on input` and `on change` see.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{Clipboard, EventKind, InputEvent, InputPayload, KeyModifiers, MemoryClipboard};

/// A field with the handlers the tests count (and Enter, which writes the
/// field from an action), a second field to take focus away, and a line that
/// shows what the field holds.
const SRC: &str = r#"
View Main() {
    var q: Str = ""
    var other: Str = ""
    var edits: Int = 0
    var last: Str = "-"
    Column #[p: 20, gap: 10, width: 400, height: 300] {
        TextField #[bind: q, placeholder: "first", width: 200, height: 40,
                    input => edits++, key_down(k) => last = k,
                    submit => q = "from an action"]
        TextField #[bind: other, placeholder: "second", width: 200, height: 40]
        Text("q={q}")
    }
}
"#;

/// Where the first field is: padding 20, then 40 tall. Its text starts 10
/// inside its left edge.
const FIRST: (f32, f32) = (100.0, 40.0);
const FIRST_TEXT_X: f32 = 30.0;
const SECOND: (f32, f32) = (100.0, 90.0);

/// The shortcut modifier on this platform, and shift.
fn modk() -> KeyModifiers {
    KeyModifiers::parse_chord("mod+a").unwrap().0
}

fn mod_shift() -> KeyModifiers {
    KeyModifiers {
        shift: true,
        ..modk()
    }
}

/// The word-movement modifier on this platform.
fn word() -> KeyModifiers {
    if cfg!(target_os = "macos") {
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        }
    } else {
        modk()
    }
}

fn shift() -> KeyModifiers {
    KeyModifiers {
        shift: true,
        ..KeyModifiers::default()
    }
}

fn none() -> KeyModifiers {
    KeyModifiers::default()
}

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    t: u64,
}

impl App {
    fn new() -> Self {
        Self::from_src(SRC)
    }

    fn from_src(src: &str) -> Self {
        let parsed = parse(src);
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

    /// An app whose first field is focused and holds `text`.
    fn with(text: &str) -> Self {
        let mut app = Self::new();
        app.tap(FIRST);
        app.type_text(text);
        app
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 300.0);
    }

    fn event(
        &mut self,
        kind: EventKind,
        pos: (f32, f32),
        payload: Option<InputPayload>,
        mods: KeyModifiers,
    ) -> InputEvent {
        self.t += 50;
        InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods,
        }
    }

    /// Starts a gesture a second after the last, so two taps are two taps and
    /// not one double press.
    fn pause(&mut self) {
        self.t += 1000;
    }

    fn tap(&mut self, pos: (f32, f32)) {
        self.pause();
        let down = self.event(EventKind::PointerDown, pos, None, none());
        let up = self.event(EventKind::PointerUp, pos, None, none());
        self.send(&[down, up]);
    }

    /// A press with `mods` held (Shift-click), released straight away.
    fn tap_with(&mut self, pos: (f32, f32), mods: KeyModifiers) {
        self.pause();
        let down = self.event(EventKind::PointerDown, pos, None, mods);
        let up = self.event(EventKind::PointerUp, pos, None, mods);
        self.send(&[down, up]);
    }

    /// Two quick taps: a double press.
    fn double_tap(&mut self, pos: (f32, f32)) {
        self.pause();
        let events: Vec<InputEvent> = [
            EventKind::PointerDown,
            EventKind::PointerUp,
            EventKind::PointerDown,
            EventKind::PointerUp,
        ]
        .into_iter()
        .map(|k| self.event(k, pos, None, none()))
        .collect();
        self.send(&events);
    }

    /// Press at `from`, move through `via` one event at a time, release at
    /// the last of them.
    fn drag(&mut self, from: (f32, f32), via: &[(f32, f32)]) {
        self.pause();
        let down = self.event(EventKind::PointerDown, from, None, none());
        self.send(&[down]);
        for &p in via {
            let mv = self.event(EventKind::PointerMove, p, None, none());
            self.send(&[mv]);
        }
        let end = via.last().copied().unwrap_or(from);
        let up = self.event(EventKind::PointerUp, end, None, none());
        self.send(&[up]);
    }

    /// Typed text, one character per event, as a keyboard sends it.
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let e = self.event(
                EventKind::TextInput,
                (0.0, 0.0),
                Some(InputPayload::Key(c.to_string())),
                none(),
            );
            self.send(&[e]);
        }
    }

    /// Text as one event, as an input method's commit or a long key repeat.
    fn insert(&mut self, text: &str) {
        let e = self.event(
            EventKind::TextInput,
            (0.0, 0.0),
            Some(InputPayload::Key(text.into())),
            none(),
        );
        self.send(&[e]);
    }

    fn key(&mut self, key: &str, mods: KeyModifiers) {
        let e = self.event(
            EventKind::KeyDown,
            (0.0, 0.0),
            Some(InputPayload::Key(key.into())),
            mods,
        );
        self.send(&[e]);
    }

    fn keys(&mut self, key: &str, mods: KeyModifiers, times: usize) {
        for _ in 0..times {
            self.key(key, mods);
        }
    }

    fn var(&self, name: &str) -> Value {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.peek(sig)
    }

    fn q(&self) -> String {
        match self.var("q") {
            Value::Str(s) => s,
            other => panic!("q is {other:?}"),
        }
    }

    fn count(&self, name: &str) -> i64 {
        self.var(name).as_int().unwrap()
    }

    /// `(anchor, caret)` of the focused field.
    fn sel(&self) -> (usize, usize) {
        self.interp
            .focused_selection()
            .expect("a focused field with editing state")
    }

    /// The field as read: `|` at the caret, `[` `]` around a selection.
    fn shown(&self) -> String {
        let (anchor, caret) = self.sel();
        let (lo, hi) = (anchor.min(caret), anchor.max(caret));
        let mut s = self.q();
        if lo == hi {
            s.insert(lo, '|');
        } else {
            s.insert(hi, ']');
            s.insert(lo, '[');
        }
        s
    }

    /// Where the caret is drawn, as the platform is told for its candidate
    /// window: the left edge of the caret rectangle, in viewport space.
    fn caret_x(&self) -> f32 {
        self.frame.text_input().expect("a focused field").caret.x
    }
}

/// How wide `text` is in the field's font, which is how far into the text a
/// caret after it sits. Measured the way the engine measures, independently of
/// the editing code.
fn width_of(text: &str) -> f32 {
    byard_core::text::TextMeasurer::new()
        .measure(text, 16.0, 400, None)
        .0
}

/// Where a press has to land to put the caret after `prefix`.
fn x_after(prefix: &str) -> f32 {
    FIRST_TEXT_X + width_of(prefix)
}

// ── the key table ────────────────────────────────────────────────────────

#[test]
fn left_and_right_move_the_caret_a_grapheme_whatever_it_is_made_of() {
    // An accent in two code points, a flag, an emoji with a skin tone.
    let text = "a\u{65}\u{301}\u{1F1FA}\u{1F1F8}\u{1F44D}\u{1F3FD}b";
    let mut app = App::with("");
    app.insert(text);
    app.keys("ArrowLeft", none(), 1);
    assert_eq!(app.shown(), format!("{}|b", &text[..text.len() - 1]));
    app.keys("ArrowLeft", none(), 1);
    assert_eq!(
        app.sel().1,
        "a\u{65}\u{301}\u{1F1FA}\u{1F1F8}".len(),
        "the emoji and its skin tone are one step"
    );
    app.keys("ArrowLeft", none(), 1);
    assert_eq!(app.sel().1, "a\u{65}\u{301}".len(), "so is the flag");
    app.keys("ArrowLeft", none(), 1);
    assert_eq!(app.sel().1, 1, "and the accented letter");
    app.keys("ArrowRight", none(), 2);
    assert_eq!(app.sel().1, "a\u{65}\u{301}\u{1F1FA}\u{1F1F8}".len());
    assert_eq!(app.q(), text, "moving edits nothing");
    assert_eq!(app.count("edits"), 1, "only the insert was an edit");
}

#[test]
fn home_and_end_go_to_either_end() {
    let mut app = App::with("hello");
    app.key("Home", none());
    assert_eq!(app.shown(), "|hello");
    app.key("End", none());
    assert_eq!(app.shown(), "hello|");
}

#[test]
fn shift_extends_the_selection_and_a_plain_arrow_collapses_it() {
    let mut app = App::with("hello world");
    app.keys("ArrowLeft", shift(), 5);
    assert_eq!(app.shown(), "hello [world]");
    app.key("ArrowLeft", none());
    assert_eq!(app.shown(), "hello |world", "Left collapses to the start");
    app.keys("ArrowRight", shift(), 3);
    assert_eq!(app.shown(), "hello [wor]ld");
    app.key("ArrowRight", none());
    assert_eq!(app.shown(), "hello wor|ld", "Right collapses to the end");
    app.key("Home", shift());
    assert_eq!(app.shown(), "[hello wor]ld");
}

#[test]
fn the_word_modifier_moves_and_selects_by_word_in_text_with_accents() {
    let mut app = App::with("naïve café au lait");
    app.key("ArrowLeft", word());
    assert_eq!(app.shown(), "naïve café au |lait");
    app.keys("ArrowLeft", word(), 2);
    assert_eq!(app.shown(), "naïve |café au lait");
    app.key("ArrowRight", word());
    assert_eq!(app.shown(), "naïve café| au lait");
    app.key(
        "ArrowRight",
        KeyModifiers {
            shift: true,
            ..word()
        },
    );
    assert_eq!(app.shown(), "naïve café[ au] lait");
}

#[test]
fn backspace_and_delete_remove_the_selection_or_a_grapheme() {
    let mut app = App::with("ab\u{1F1FA}\u{1F1F8}cd");
    app.keys("ArrowLeft", none(), 2);
    app.key("Backspace", none());
    assert_eq!(app.q(), "abcd", "the flag goes whole");
    app.key("Delete", none());
    assert_eq!(app.q(), "abd");
    app.keys("ArrowLeft", shift(), 2);
    assert_eq!(app.shown(), "[ab]d");
    app.key("Backspace", none());
    assert_eq!(app.q(), "d", "a selection goes at once");
    assert_eq!(app.shown(), "|d");
}

#[test]
fn delete_removes_the_character_after_the_caret_and_not_the_whole_field() {
    // The defect: Delete wrote an empty string.
    let mut app = App::with("keep me");
    app.key("Home", none());
    app.key("Delete", none());
    assert_eq!(app.q(), "eep me");
    app.key("Delete", none());
    assert_eq!(app.q(), "ep me");
    assert_eq!(app.shown(), "|ep me");
}

#[test]
fn the_word_modifier_with_backspace_and_delete_removes_to_the_word_boundary() {
    let mut app = App::with("one two three");
    app.key("Backspace", word());
    assert_eq!(app.q(), "one two ");
    app.key("Home", none());
    app.key("Delete", word());
    assert_eq!(app.q(), " two ");
}

#[test]
fn select_all_then_typing_replaces_the_value() {
    let mut app = App::with("hello");
    app.key("a", modk());
    assert_eq!(app.shown(), "[hello]");
    app.type_text("x");
    assert_eq!(app.q(), "x");
    assert_eq!(app.shown(), "x|");
}

#[test]
fn copy_cut_and_paste_go_through_the_interpreters_clipboard() {
    let mut app = App::with("hello world");
    app.keys("ArrowLeft", shift(), 5);
    app.key("c", modk());
    assert_eq!(app.q(), "hello world", "copy leaves the text");
    app.key("ArrowLeft", none());
    app.key("v", modk());
    assert_eq!(app.q(), "hello worldworld", "paste at the caret");
    app.key("a", modk());
    app.key("x", modk());
    assert_eq!(app.q(), "", "cut removes what it copied");
    app.key("v", modk());
    assert_eq!(app.q(), "hello worldworld", "and it was copied whole");
}

#[test]
fn a_clipboard_the_host_supplies_is_the_one_used() {
    let mut app = App::with("abc");
    let mut mine = MemoryClipboard::new();
    mine.write_text("from the host");
    app.interp.set_clipboard(Box::new(mine));
    app.key("a", modk());
    app.key("v", modk());
    assert_eq!(app.q(), "from the host");
}

#[test]
fn a_pasted_value_with_newlines_arrives_as_one_line() {
    let mut app = App::with("");
    let mut mine = MemoryClipboard::new();
    mine.write_text("line one\r\nline two\nline three");
    app.interp.set_clipboard(Box::new(mine));
    app.key("v", modk());
    assert_eq!(app.q(), "line one line two line three");
    assert!(!app.q().contains(['\n', '\r']));
}

#[test]
fn copy_with_nothing_selected_and_paste_of_nothing_do_nothing() {
    let mut app = App::with("abc");
    let mut mine = MemoryClipboard::new();
    mine.write_text("kept");
    app.interp.set_clipboard(Box::new(mine));
    let edits = app.count("edits");
    app.key("c", modk());
    app.key("x", modk());
    assert_eq!(app.q(), "abc");
    assert_eq!(app.count("edits"), edits, "nothing was edited");
    // The clipboard still holds what it did: paste it to see.
    app.key("v", modk());
    assert_eq!(app.q(), "abckept");
    // An empty clipboard pastes nothing, and nothing fires.
    app.interp.set_clipboard(Box::new(MemoryClipboard::new()));
    let edits = app.count("edits");
    app.key("v", modk());
    assert_eq!(app.q(), "abckept");
    assert_eq!(app.count("edits"), edits);
}

#[test]
fn a_shortcut_types_no_text() {
    // The platform sends the key, and a control character it filters out; a
    // host that sent the letter as text too must not get a `v` in the field.
    let mut app = App::with("ab");
    let e = app.event(
        EventKind::TextInput,
        (0.0, 0.0),
        Some(InputPayload::Key("v".into())),
        modk(),
    );
    app.send(&[e]);
    assert_eq!(app.q(), "ab");
}

#[test]
fn typing_hello_world_then_undo_leaves_hello_and_a_space() {
    let mut app = App::with("hello world");
    assert_eq!(app.count("edits"), 11, "typing fired once per letter");
    app.key("z", modk());
    assert_eq!(app.q(), "hello ");
    app.key("z", modk());
    assert_eq!(app.q(), "");
    app.key("z", mod_shift());
    assert_eq!(app.q(), "hello ", "redo, with Shift");
    app.key("y", modk());
    assert_eq!(app.q(), "hello world", "redo, with Y");
}

#[test]
fn undo_and_redo_write_the_value_so_input_and_change_fire_for_them() {
    let mut app = App::with("ab");
    let before = app.count("edits");
    app.key("z", modk());
    assert_eq!(app.q(), "");
    assert_eq!(app.count("edits"), before + 1);
    app.key("y", modk());
    assert_eq!(app.q(), "ab");
    assert_eq!(app.count("edits"), before + 2);
    // Nothing to redo: nothing fires.
    app.key("y", modk());
    assert_eq!(app.count("edits"), before + 2);
}

#[test]
fn a_paste_is_one_undo_step() {
    let mut app = App::with("ab");
    let mut mine = MemoryClipboard::new();
    mine.write_text("12345");
    app.interp.set_clipboard(Box::new(mine));
    app.key("v", modk());
    assert_eq!(app.q(), "ab12345");
    app.key("z", modk());
    assert_eq!(app.q(), "ab", "the paste, whole");
}

#[test]
fn redo_after_a_new_edit_is_empty() {
    let mut app = App::with("ab");
    app.key("z", modk());
    assert_eq!(app.q(), "");
    app.insert("x");
    assert_eq!(app.q(), "x");
    app.key("y", modk());
    assert_eq!(app.q(), "x", "the redo stack went with the new edit");
}

// ── events ───────────────────────────────────────────────────────────────

#[test]
fn an_edit_that_changes_nothing_fires_nothing() {
    let mut app = App::with("ab");
    app.key("End", none());
    let edits = app.count("edits");
    app.key("Delete", none());
    assert_eq!(app.count("edits"), edits, "Delete at the end");
    app.key("Home", none());
    app.key("Backspace", none());
    assert_eq!(app.count("edits"), edits, "Backspace at the start");
    app.key("Delete", none());
    assert_eq!(app.count("edits"), edits + 1, "but this one deletes");
}

#[test]
fn key_down_still_sees_the_key_name_and_nothing_else() {
    let mut app = App::with("ab");
    app.key("ArrowLeft", shift());
    assert_eq!(
        app.var("last"),
        Value::Str("ArrowLeft".into()),
        "the payload is the key's name, whatever was held"
    );
    app.key("a", modk());
    assert_eq!(app.var("last"), Value::Str("a".into()));
}

// ── the value written from outside ───────────────────────────────────────

#[test]
fn a_var_written_from_an_action_moves_the_caret_to_the_end() {
    let mut app = App::with("hello world");
    app.keys("ArrowLeft", shift(), 5);
    app.key("Home", none());
    assert_eq!(app.sel(), (0, 0));
    // Enter submits, and `submit` writes the field from an action.
    app.key("Enter", none());
    assert_eq!(app.q(), "from an action");
    assert_eq!(
        app.sel(),
        ("from an action".len(), "from an action".len()),
        "the caret is at the end and nothing is selected"
    );
    app.type_text("!");
    assert_eq!(app.q(), "from an action!");
    // Undo starts over too: what was typed before the write is not ours to undo.
    app.key("z", modk());
    assert_eq!(app.q(), "from an action");
}

#[test]
fn the_caret_is_at_the_end_after_a_write_and_the_selection_is_gone() {
    let mut app = App::with("hello world");
    app.key("a", modk());
    assert_eq!(app.sel(), (0, 11));
    let sig = app.interp.var_signal(&Symbol::intern("q")).unwrap();
    app.interp.write_var(sig, Value::Str("brand new".into()));
    app.send(&[]);
    assert_eq!(app.sel(), (9, 9), "the caret is at the end");
    app.type_text("!");
    assert_eq!(app.q(), "brand new!");
}

#[test]
fn undo_starts_over_when_the_value_was_written_from_outside() {
    let mut app = App::with("abc");
    let sig = app.interp.var_signal(&Symbol::intern("q")).unwrap();
    app.interp.write_var(sig, Value::Str("elsewhere".into()));
    app.send(&[]);
    app.key("z", modk());
    assert_eq!(app.q(), "elsewhere", "there is nothing of ours to undo");
}

// ── the pointer ──────────────────────────────────────────────────────────

#[test]
fn a_press_puts_the_caret_between_the_graphemes_it_landed_nearest() {
    let mut app = App::with("hello world");
    for k in [0, 3, 5, 11] {
        app.tap((x_after(&"hello world"[..k]), FIRST.1));
        assert_eq!(app.sel(), (k, k), "a press at the boundary {k}");
    }
    // Between two boundaries it is the nearer one that wins: a press past the
    // middle of a letter lands after it.
    let (a, b) = (x_after("hel"), x_after("hell"));
    app.tap((a + (b - a) * 0.3, FIRST.1));
    assert_eq!(app.sel(), (3, 3));
    app.tap((a + (b - a) * 0.7, FIRST.1));
    assert_eq!(app.sel(), (4, 4));
    // Left of the text and right of it.
    app.tap((22.0, FIRST.1));
    assert_eq!(app.sel(), (0, 0));
    app.tap((210.0, FIRST.1));
    assert_eq!(app.sel(), (11, 11));
}

#[test]
fn a_press_lands_between_graphemes_that_are_more_than_one_code_point() {
    let flag = "\u{1F1FA}\u{1F1F8}";
    let mut app = App::with("");
    app.insert(&format!("a{flag}b"));
    // Wherever the flag is pressed, the caret is before it or after it.
    let (from, to) = (x_after("a"), x_after(&format!("a{flag}")));
    let mut x = from - 1.0;
    while x < to + 1.0 {
        app.tap((x, FIRST.1));
        let caret = app.sel().1;
        assert!(
            caret == 0 || caret == 1 || caret == 1 + flag.len() || caret == 2 + flag.len(),
            "x = {x} put the caret inside the flag at {caret}"
        );
        x += 1.5;
    }
}

#[test]
fn the_press_that_focuses_the_field_places_the_caret_too() {
    let mut app = App::new();
    let sig = app.interp.var_signal(&Symbol::intern("q")).unwrap();
    app.interp.write_var(sig, Value::Str("abcdef".into()));
    app.send(&[]);
    // Not focused: the text is drawn from the field's left padding.
    app.tap((x_after("abc"), FIRST.1));
    assert_eq!(app.sel(), (3, 3));
    app.type_text("X");
    assert_eq!(app.q(), "abcXdef");
}

#[test]
fn shift_with_a_press_extends_the_selection_from_where_it_was() {
    let mut app = App::with("hello world");
    app.tap((x_after("hello"), FIRST.1));
    app.tap_with((x_after("hello wor"), FIRST.1), shift());
    assert_eq!(app.sel(), (5, 9));
}

#[test]
fn a_drag_selects_from_the_press_to_the_pointer_and_may_leave_the_field() {
    let mut app = App::with("hello world");
    let y = FIRST.1;
    app.drag((x_after("he"), y), &[(x_after("hello"), y)]);
    assert_eq!(app.shown(), "he[llo] world");
    // Back across the press point: the anchor stays.
    app.drag((x_after("hello"), y), &[(x_after("he"), y)]);
    assert_eq!(app.sel(), (5, 2));
    // Out of the field to the right, and below it, with the press still held:
    // the field keeps the drag.
    app.drag((x_after("hello "), y), &[(300.0, y), (390.0, 250.0)]);
    assert_eq!(app.sel(), (6, 11));
    // And a drag begun elsewhere does not reach into it.
    app.tap(FIRST);
    app.key("Home", none());
    app.drag(
        (300.0, 250.0),
        &[(x_after("hello"), y), (x_after("hello w"), y)],
    );
    assert_eq!(app.sel(), (0, 0));
}

#[test]
fn a_double_press_selects_the_word_under_the_pointer() {
    let mut app = App::with("big red dog");
    app.double_tap((x_after("big r") + 2.0, FIRST.1));
    assert_eq!(app.shown(), "big [red] dog");
    app.double_tap((x_after("big red d") + 2.0, FIRST.1));
    assert_eq!(app.shown(), "big red [dog]");
}

// ── composition keeps its priority ───────────────────────────────────────

#[test]
fn a_commit_goes_in_at_the_caret_and_replaces_the_selection() {
    let mut app = App::with("hello world");
    app.keys("ArrowLeft", shift(), 5);
    let commit = byard_core::ImeEvent::Commit("there".into())
        .into_input(900)
        .unwrap();
    app.send(&[commit]);
    assert_eq!(app.q(), "hello there");
    app.keys("ArrowLeft", none(), 5);
    let commit = byard_core::ImeEvent::Commit("big ".into())
        .into_input(950)
        .unwrap();
    app.send(&[commit]);
    assert_eq!(app.q(), "hello big there", "in the middle, not at the end");
}

#[test]
fn a_commit_is_not_a_shortcut_even_after_one_was_pressed() {
    let mut app = App::with("ab");
    app.key("c", modk());
    let commit = byard_core::ImeEvent::Commit("x".into())
        .into_input(900)
        .unwrap();
    app.send(&[commit]);
    assert_eq!(app.q(), "abx", "the modifiers of the last key are not its");
}

#[test]
fn the_preedit_is_drawn_at_the_caret_and_the_text_after_it_makes_room() {
    let mut app = App::with("hello world");
    app.keys("ArrowLeft", none(), 6);
    let preedit = byard_core::ImeEvent::Preedit {
        text: "にほん".into(),
        cursor: Some((9, 9)),
    }
    .into_input(900)
    .unwrap();
    app.send(&[preedit]);
    let texts: Vec<(String, f32)> = app
        .frame
        .texts()
        .iter()
        .map(|t| (t.text.clone(), t.x))
        .collect();
    let x_of = |s: &str| {
        texts
            .iter()
            .find(|(t, _)| t == s)
            .unwrap_or_else(|| panic!("{s:?} in {texts:?}"))
            .1
    };
    assert_eq!(x_of("hello"), FIRST_TEXT_X);
    assert_eq!(x_of("にほん"), FIRST_TEXT_X + width_of("hello"));
    let tail = x_of(" world");
    assert!(
        (tail - (FIRST_TEXT_X + width_of("hello") + width_of("にほん"))).abs() < 0.01,
        "the text after the caret starts where the preedit ends: {tail}"
    );
    assert_eq!(app.q(), "hello world", "a preedit is not the value");
    // The IME owns the caret keys while it composes.
    app.key("ArrowLeft", none());
    assert_eq!(app.sel(), (5, 5));
}

// ── painting ─────────────────────────────────────────────────────────────

#[test]
fn the_caret_is_drawn_at_the_carets_place_and_not_at_the_end() {
    let mut app = App::with("hello world");
    let at_end = app.caret_x();
    app.keys("ArrowLeft", none(), 5);
    let mid = app.caret_x();
    assert!(
        (mid - (x_after("hello ") - 0.5)).abs() < 0.5,
        "the caret after `hello ` is at {mid}, expected {}",
        x_after("hello ") - 0.5
    );
    assert!(at_end - mid > 20.0, "and not where the end is ({at_end})");
    // The caret is a box in the frame at that x.
    let drawn = app
        .frame
        .instances()
        .iter()
        .any(|b| (b.rect[0] - mid).abs() < 0.01 && b.rect[2] < 2.0);
    assert!(drawn, "a caret-sized box at x = {mid}");
}

#[test]
fn a_selection_is_a_translucent_box_behind_the_text_and_the_caret_is_at_its_moving_end() {
    let mut app = App::with("hello world");
    app.keys("ArrowLeft", none(), 5);
    app.keys("ArrowLeft", shift(), 3);
    assert_eq!(app.shown(), "hel[lo ]world");
    let (x0, x1) = (x_after("hel"), x_after("hello "));
    let selection: Vec<_> = app
        .frame
        .decorated()
        .iter()
        .filter(|d| (d.base.rect[0] - x0).abs() < 0.01)
        .collect();
    assert_eq!(selection.len(), 1, "one selection box starting at {x0}");
    let rect = selection[0].base.rect;
    assert!(
        (rect[2] - (x1 - x0)).abs() < 0.01,
        "as wide as the text it covers"
    );
    let alpha = selection[0].base.color[3];
    assert!(
        (alpha - 0.3).abs() < 0.01,
        "the theme's primary at 30 %, not {alpha}"
    );
    // The caret is at the end that moved, which is the left one.
    assert!((app.caret_x() - (x0 - 0.5)).abs() < 0.5);
}

#[test]
fn a_field_can_choose_the_colour_its_selection_is_painted_in() {
    let mut app = App::from_src(
        r#"
View Main() {
    var q: Str = "hello"
    TextField #[bind: q, width: 200, height: 40, selection: 0x66E4A11B]
}
"#,
    );
    app.tap((100.0, 20.0));
    app.key("a", modk());
    let boxes = app.frame.decorated();
    assert_eq!(boxes.len(), 1, "one selection box");
    let [red, green, blue, alpha] = boxes[0].base.color;
    // The colour as written, alpha included, not the theme's at 30 %.
    assert!((alpha - 0.4).abs() < 0.01, "alpha {alpha}");
    assert!(
        red > green && green > blue,
        "an amber, not the theme's blue: {red} {green} {blue}"
    );
}

#[test]
fn a_field_that_never_had_focus_has_no_editing_state() {
    let mut app = App::new();
    app.tap(SECOND);
    app.key("a", none());
    assert_eq!(app.interp.edit_state_count(), 1, "only the focused one");
    let mut idle = App::new();
    idle.send(&[]);
    idle.send(&[]);
    assert_eq!(idle.interp.edit_state_count(), 0, "nothing without focus");
}

#[test]
fn text_wider_than_the_field_scrolls_to_keep_the_caret_inside() {
    let mut app = App::with("");
    let long = "the quick brown fox jumps over the lazy dog again and again";
    app.type_text(long);
    // The field is 200 wide, text starts 10 in: the caret stays inside it.
    let x = app.caret_x();
    assert!(
        (30.0..=220.0).contains(&x),
        "the caret at the end is inside the field: {x}"
    );
    let line = app
        .frame
        .texts()
        .iter()
        .find(|t| t.text == long)
        .expect("the value is drawn");
    assert!(
        line.x < FIRST_TEXT_X,
        "the text is scrolled left: {}",
        line.x
    );
    app.key("Home", none());
    let line = app.frame.texts().iter().find(|t| t.text == long).unwrap();
    assert_eq!(line.x, FIRST_TEXT_X, "and back to the start with the caret");
    assert!((app.caret_x() - (FIRST_TEXT_X - 0.5)).abs() < 0.5);
}

#[test]
fn a_drag_past_the_edge_of_a_long_text_scrolls_it() {
    let mut app = App::with("");
    app.type_text("the quick brown fox jumps over the lazy dog again and again");
    app.key("Home", none());
    let y = FIRST.1;
    // Press near the start and drag out to the right of the field, moving a
    // few times: the caret steps along and the text scrolls after it.
    let down = app.event(EventKind::PointerDown, (40.0, y), None, none());
    app.send(&[down]);
    let before = app.sel().1;
    for i in 0..6_u8 {
        let mv = app.event(
            EventKind::PointerMove,
            (260.0 + f32::from(i), y),
            None,
            none(),
        );
        app.send(&[mv]);
    }
    let after = app.sel().1;
    assert_eq!(after, before + 6, "one grapheme per move past the edge");
    let up = app.event(EventKind::PointerUp, (266.0, y), None, none());
    app.send(&[up]);
}

#[test]
fn editing_keys_do_nothing_in_a_field_that_does_not_have_focus() {
    let mut app = App::new();
    app.tap(SECOND);
    app.key("Delete", none());
    app.key("a", modk());
    assert_eq!(app.q(), "");
    assert_eq!(app.count("edits"), 0);
}
