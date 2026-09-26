//! Light dismiss for an anchored overlay (RFC-0036 `on dismiss`).
//!
//! The claim has two halves and the second is the one worth writing tests for.
//! A press outside the panel closes it; a press on the *anchor* does not, and
//! everything under the panel keeps every event it would otherwise have had.
//!
//! That second half is what separates this from RFC-0017's modal dismissal,
//! which exists, works, and would be the wrong thing to reuse: a scrim covers
//! the viewport and swallows the scroll and hover of the page beneath it,
//! which is right for a dialog and wrong for an autocomplete.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload};

const W: f32 = 400.0;
const H: f32 = 300.0;

/// A field at (20, 20) 120×30, a panel anchored under it, and a button
/// elsewhere in the page whose counter says whether events still reach it.
const SRC: &str = r#"
View Main() {
    var open = true
    var page_taps = 0

    Column #[bg: 0x101010, p: 20, gap: 10, width: 400, height: 300] {
        Box #[bg: 0x223344, width: 120, height: 30] as field {}
        Box #[bg: 0x334455, width: 120, height: 30, m: (top: 120)] => page_taps++
    }
    when open {
        Overlay #[modal: false] {
            Box #[bg: 0xAA3344, width: 140, height: 60, anchor_to: "field",
                  dismiss => open = false] {}
        }
    }
}
"#;

fn ev(kind: EventKind, pos: (f32, f32), t: u64) -> InputEvent {
    InputEvent {
        kind,
        pos,
        delta: (1.0, 1.0),
        payload: None,
        time_ms: t,
    }
}

struct App {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
}

impl App {
    fn new() -> Self {
        let parsed = parse(SRC);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self { interp, tree };
        app.render();
        app
    }

    fn render(&mut self) {
        self.interp.tick();
        let mut frame = RenderFrame::new();
        self.interp.render(&self.tree, &mut frame, W, H);
    }

    fn press(&mut self, pos: (f32, f32)) {
        self.interp
            .dispatch_events(&[ev(EventKind::PointerDown, pos, 0)]);
        self.render();
    }

    fn tap(&mut self, pos: (f32, f32)) {
        self.interp.dispatch_events(&[
            ev(EventKind::PointerDown, pos, 0),
            ev(EventKind::PointerUp, pos, 8),
        ]);
        self.render();
    }

    fn escape(&mut self) {
        self.interp.dispatch_events(&[InputEvent {
            kind: EventKind::KeyDown,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload: Some(InputPayload::Str("Escape".into())),
            time_ms: 0,
        }]);
        self.render();
    }

    fn var(&self, name: &str) -> Value {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .unwrap_or_else(|| panic!("`{name}` is declared"));
        self.interp.peek(sig)
    }

    fn open(&self) -> bool {
        self.var("open").as_bool().unwrap_or(false)
    }
}

/// The field's centre, the panel's centre, and a point in neither.
const FIELD: (f32, f32) = (80.0, 35.0);
const PANEL: (f32, f32) = (90.0, 80.0);
const AWAY: (f32, f32) = (340.0, 260.0);
/// The button lower down the page, which is neither the panel nor the anchor.
const PAGE_BUTTON: (f32, f32) = (80.0, 195.0);

#[test]
fn a_press_inside_the_panel_does_not_dismiss_it() {
    let mut app = App::new();
    assert!(app.open(), "the panel starts open");
    app.press(PANEL);
    assert!(app.open(), "pressing the panel itself must not close it");
}

/// The half that is easy to leave out, and the one whose absence looks like a
/// rendering glitch: without the anchor's rect in the keep list, the press
/// that opens a dropdown is also the press that closes it.
#[test]
fn a_press_on_the_anchor_does_not_dismiss() {
    let mut app = App::new();
    app.press(FIELD);
    assert!(
        app.open(),
        "pressing the field the panel hangs from must not close it: that press \
         is the one that opens it, and closing on it reads as a flicker"
    );
}

#[test]
fn a_press_anywhere_else_dismisses() {
    let mut app = App::new();
    app.press(AWAY);
    assert!(
        !app.open(),
        "a press outside both rects must close the panel"
    );
}

#[test]
fn escape_dismisses() {
    let mut app = App::new();
    app.escape();
    assert!(!app.open());
}

/// The page under an open panel keeps its events.
///
/// This is the assertion that fails if somebody reaches for RFC-0017's modal
/// scrim to implement this: the scrim raises the router's modal floor and
/// every handler beneath it stops firing, so the tap below would be swallowed
/// and the count would stay at zero while the panel was up.
#[test]
fn the_page_under_the_panel_still_receives_its_events() {
    let mut app = App::new();
    assert!(app.open(), "the panel is up for this whole test");
    app.tap(PAGE_BUTTON);
    assert_eq!(
        app.var("page_taps").as_int(),
        Some(1),
        "a tap outside the panel must reach the page as well as dismissing: \
         a light dismiss observes, it does not block"
    );
}

/// And a dismissed panel stops dismissing: the registration is rebuilt every
/// render, like every other hit rect, so a closed panel leaves nothing behind.
#[test]
fn a_closed_panel_registers_nothing() {
    let mut app = App::new();
    app.press(AWAY);
    assert!(!app.open());
    // A second press with the panel gone must not fire anything; if the
    // registration leaked, this would run an action against a scope that is
    // no longer mounted.
    app.press(AWAY);
    assert!(!app.open());
}
