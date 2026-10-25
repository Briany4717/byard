//! The caret's blink (RFC-0041 §6), against the real timer driver.
//!
//! The claim worth testing is a cost: the blink is **not** an animation. It is
//! one repeating timer through the controller boundary's timer path, so a
//! focused, idle field wakes the logic thread twice a second and does nothing
//! in between, and a field that is not focused arms nothing at all. These
//! tests drive the real relay and Tokio's time driver, because a timer that
//! only fires under a fake clock proves nothing about the reactor.

use std::time::{Duration, Instant};

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};

/// A field, a second one to take the focus, and a branch that can close.
const SRC: &str = r#"
View Main() {
    var q: Str = ""
    var other: Str = ""
    var show: Bool = true
    Column #[p: 20, gap: 10, width: 400, height: 300] {
        TextField #[bind: q, width: 200, height: 40]
        TextField #[bind: other, width: 200, height: 40]
        when show {
            TextField #[bind: q, width: 200, height: 40]
        }
    }
}
"#;

const FIRST: (f32, f32) = (100.0, 40.0);
const SECOND: (f32, f32) = (100.0, 90.0);
const THIRD: (f32, f32) = (100.0, 140.0);

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    relay: Relay,
    frame: RenderFrame,
    t: u64,
}

impl App {
    fn new() -> Self {
        let parsed = parse(SRC);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let dispatcher = Dispatcher::new(
            ControllerRegistry::new(),
            relay.io_handle(),
            relay.io_result_sender(),
        );
        let mut interp = Interpreter::new();
        interp.set_dispatcher(dispatcher);
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &["Main"]);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            relay,
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
            mods: KeyModifiers::default(),
        }
    }

    fn tap(&mut self, pos: (f32, f32)) {
        self.t += 1000;
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

    fn window_focus(&mut self, focused: bool) {
        let e = self.event(
            EventKind::WindowFocus,
            (0.0, 0.0),
            Some(InputPayload::Bool(focused)),
        );
        self.send(&[e]);
    }

    /// What the logic thread does when a result arrives: apply what the pool
    /// completed, then tick and draw. Returns how many results it applied.
    fn wake(&mut self) -> usize {
        let mut results = Vec::new();
        while let Some(r) = self.relay.try_recv_io_result() {
            results.push(r);
        }
        let n = results.len();
        if n > 0 {
            let _ = self.interp.apply_io_results(results);
            self.send(&[]);
        }
        n
    }

    /// Whether this frame draws a caret: a box a pixel and a half wide as tall
    /// as the text.
    fn caret_drawn(&self) -> bool {
        self.frame
            .instances()
            .iter()
            .any(|b| (b.rect[2] - 1.5).abs() < 0.01)
    }

    /// Pumps until `done`, or fails after `within`.
    fn pump_until(&mut self, within: Duration, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            self.wake();
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{what}: not within {within:?}");
    }
}

#[test]
fn a_field_without_focus_arms_nothing_and_a_focused_one_arms_one_timer() {
    let mut app = App::new();
    assert!(!app.interp.caret_is_blinking(), "no field has focus");
    app.send(&[]);
    assert!(!app.interp.caret_is_blinking());
    app.tap(FIRST);
    assert!(app.interp.caret_is_blinking(), "focus starts it");
    assert!(app.caret_drawn(), "and it starts shown");
    app.tap(SECOND);
    assert!(app.interp.caret_is_blinking(), "now for the second field");
    app.tap((350.0, 250.0));
    // A press on nothing focusable leaves the focus where it was.
    assert!(app.interp.caret_is_blinking());
}

#[test]
fn the_blink_is_not_an_animation() {
    let mut app = App::new();
    app.tap(FIRST);
    app.type_text("a");
    assert!(app.interp.caret_is_blinking());
    assert!(
        !app.interp.needs_another_frame(),
        "a focused, idle field asks for no frame of its own: it is woken, \
         twice a second, by its timer"
    );
}

#[test]
fn the_caret_hides_and_shows_again_on_the_timer() {
    let mut app = App::new();
    app.tap(FIRST);
    assert!(app.caret_drawn());
    app.pump_until(Duration::from_secs(3), "the caret hides", |a| {
        !a.caret_drawn()
    });
    app.pump_until(Duration::from_secs(3), "and shows again", |a| {
        a.caret_drawn()
    });
}

#[test]
fn an_edit_or_a_caret_move_starts_the_blink_over_with_the_caret_showing() {
    let mut app = App::new();
    app.tap(FIRST);
    app.pump_until(Duration::from_secs(3), "the caret hides", |a| {
        !a.caret_drawn()
    });
    app.type_text("x");
    assert!(app.caret_drawn(), "typing shows it at once");
    assert!(app.interp.caret_is_blinking(), "and it goes on blinking");
    app.pump_until(Duration::from_secs(3), "it hides again", |a| {
        !a.caret_drawn()
    });
    let e = app.event(
        EventKind::KeyDown,
        (0.0, 0.0),
        Some(InputPayload::Key("ArrowLeft".into())),
    );
    app.send(&[e]);
    assert!(app.caret_drawn(), "so does moving the caret");
}

#[test]
fn a_field_that_is_torn_down_under_focus_stops_blinking() {
    let mut app = App::new();
    app.tap(THIRD);
    assert!(app.interp.caret_is_blinking());
    // The focused field lives in a branch that closes.
    let show = app.interp.var_signal(&Symbol::intern("show")).unwrap();
    app.interp.write_var(show, Value::Bool(false));
    app.send(&[]);
    assert!(
        !app.interp.caret_is_blinking(),
        "its field is gone, and nothing blinks for it"
    );
    assert_eq!(
        app.interp.edit_state_count(),
        0,
        "and its state went with it"
    );
}

#[test]
fn a_window_without_focus_does_not_blink_and_draws_no_caret() {
    let mut app = App::new();
    app.tap(FIRST);
    assert!(app.caret_drawn());
    app.window_focus(false);
    assert!(
        !app.interp.caret_is_blinking(),
        "no timer for a window nobody is in"
    );
    assert!(!app.caret_drawn(), "and the caret is hidden");
    assert!(
        app.frame.text_input().is_some(),
        "the IME is still told where it is"
    );
    app.window_focus(true);
    assert!(app.interp.caret_is_blinking(), "back in, it blinks again");
    assert!(app.caret_drawn(), "and shows");
}

#[test]
fn an_embedding_with_no_timer_driver_shows_a_steady_caret() {
    // `byard check` and a headless run wire no dispatcher: no blink, caret on.
    let parsed = parse(SRC);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let press = |t: u64, kind| InputEvent {
        kind,
        pos: FIRST,
        delta: (0.0, 0.0),
        payload: None,
        time_ms: t,
        mods: KeyModifiers::default(),
    };
    interp.dispatch_events(&[
        press(10, EventKind::PointerDown),
        press(20, EventKind::PointerUp),
    ]);
    interp.tick();
    frame.clear();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert!(!interp.caret_is_blinking());
    assert!(
        frame
            .instances()
            .iter()
            .any(|b| (b.rect[2] - 1.5).abs() < 0.01)
    );
}

#[test]
fn a_held_caret_does_not_blink() {
    let mut app = App::new();
    app.interp.set_caret_blink(false);
    app.tap(FIRST);
    assert!(!app.interp.caret_is_blinking());
    assert!(app.caret_drawn());
}

/// A harness that waits for "nothing outstanding" (Aura Weather's tests do, to
/// let every reply land) must not wait on a caret: it never stops blinking
/// while a field has focus, and it is not a reply anybody is owed.
#[test]
fn the_blink_is_not_an_outstanding_reply() {
    let mut app = App::new();
    app.tap(FIRST);
    assert!(app.interp.caret_is_blinking());
    assert_eq!(app.interp.outstanding_continuations(), 0);
}
