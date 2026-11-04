//! A `Button`, a `Toggle` and a `Slider` are in the Tab order, and the keyboard
//! works them (RFC-0042 §5, issue #347).
//!
//! Tab and Shift+Tab walk the controls in the order they are written, skipping
//! the ones that are disabled; Space or Enter presses a focused button or flips
//! a focused switch, once however long the key is held; the arrow keys step a
//! focused slider. A text field keeps what is its own: Space types a space and
//! Enter submits, with nothing else pressed.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
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
            .render(&self.tree, &mut self.frame, 400.0, 400.0);
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

    /// A key pressed and let go, as `byard shot --key` sends it.
    fn key(&mut self, name: &str) {
        self.key_with(name, false);
    }

    fn key_with(&mut self, name: &str, shift: bool) {
        let down = self.event(
            EventKind::KeyDown,
            Some(InputPayload::Key(name.into())),
            shift,
        );
        let up = self.event(
            EventKind::KeyUp,
            Some(InputPayload::Key(name.into())),
            shift,
        );
        self.send(&[down, up]);
    }

    /// Only the press, as a held key repeats it.
    fn key_down(&mut self, name: &str) {
        let down = self.event(
            EventKind::KeyDown,
            Some(InputPayload::Key(name.into())),
            false,
        );
        self.send(&[down]);
    }

    fn key_up(&mut self, name: &str) {
        let up = self.event(
            EventKind::KeyUp,
            Some(InputPayload::Key(name.into())),
            false,
        );
        self.send(&[up]);
    }

    fn typed(&mut self, text: &str) {
        let e = self.event(
            EventKind::TextInput,
            Some(InputPayload::Key(text.into())),
            false,
        );
        self.send(&[e]);
    }

    fn tap(&mut self, x: f32, y: f32) {
        let mut down = self.event(EventKind::PointerDown, None, false);
        down.pos = (x, y);
        let mut up = self.event(EventKind::PointerUp, None, false);
        up.pos = (x, y);
        self.send(&[down, up]);
    }

    fn line(&self) -> String {
        self.frame
            .texts()
            .last()
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }
}

const CONTROLS: &str = r#"
View Main() {
    var count = 0
    var wifi = false
    var vol = 0.5
    Column {
        Button("Add") #[width: 100, height: 40] => { count += 1 }
        Button("Off") #[width: 100, height: 40, disabled: true] => { count += 100 }
        Toggle #[bind: wifi, width: 52, height: 30]
        Slider #[bind: vol, min: 0.0, max: 1.0, step: 0.1, width: 200, height: 28]
        Text("{count}:{wifi}:{vol}")
    }
}
"#;

#[test]
fn tab_walks_button_toggle_and_slider_and_the_keys_work_them() {
    let mut app = App::new(CONTROLS);
    assert_eq!(app.line(), "0:false:0.5");

    // Tab reaches the button; Space presses it, Enter again.
    app.key("Tab");
    app.key(" ");
    assert_eq!(app.line(), "1:false:0.5");
    app.key("Enter");
    assert_eq!(app.line(), "2:false:0.5");

    // The disabled button is skipped: the next stop is the toggle.
    app.key("Tab");
    app.key(" ");
    assert_eq!(app.line(), "2:true:0.5", "Space flipped the toggle");
    app.key("Enter");
    assert_eq!(app.line(), "2:false:0.5", "Enter flips it back");

    // Then the slider: arrows step it by `step`, and stop at the ends.
    app.key("Tab");
    app.key("ArrowRight");
    assert_eq!(app.line(), "2:false:0.6");
    app.key("ArrowLeft");
    app.key("ArrowLeft");
    assert_eq!(app.line(), "2:false:0.4");
    for _ in 0..20 {
        app.key("ArrowRight");
    }
    assert_eq!(app.line(), "2:false:1", "clamped to max");

    // Tab wraps to the button; Shift+Tab goes back the way it came.
    app.key("Tab");
    app.key_with("Tab", true);
    app.key(" ");
    assert_eq!(
        app.line(),
        "2:false:1",
        "back on the slider, Space does nothing"
    );
    app.key_with("Tab", true);
    app.key(" ");
    assert_eq!(app.line(), "2:true:1", "Shift+Tab reached the toggle");
}

#[test]
fn a_disabled_control_is_not_reached_by_a_click_either() {
    let mut app = App::new(CONTROLS);
    // Click the disabled button (second, y 40..80), then press Space: it would
    // be pressed if the click had focused it.
    app.tap(50.0, 60.0);
    app.key(" ");
    assert_eq!(app.line(), "0:false:0.5");
    // A click on the enabled one focuses it, and Space presses it as well.
    app.tap(50.0, 20.0);
    assert_eq!(app.line(), "1:false:0.5");
    app.key(" ");
    assert_eq!(app.line(), "2:false:0.5");
}

#[test]
fn a_held_key_presses_once_and_is_armed_again_by_its_release() {
    let mut app = App::new(CONTROLS);
    app.key("Tab");
    app.key_down(" ");
    app.key_down(" ");
    app.key_down(" ");
    assert_eq!(app.line(), "1:false:0.5", "key repeat is not three presses");
    app.key_up(" ");
    app.key_down(" ");
    assert_eq!(app.line(), "2:false:0.5");
    // A different activation key does not re-arm it mid-hold.
    app.key_down("Enter");
    assert_eq!(app.line(), "2:false:0.5");
}

#[test]
fn a_text_field_still_types_a_space_and_submits_on_enter() {
    let mut app = App::new(
        r#"
View Main() {
    var q: Str = ""
    var n = 0
    var sent = 0
    Column {
        TextField #[bind: q, width: 200, height: 40, submit => { sent += 1 }]
        Button("Add") #[width: 100, height: 40] => { n += 1 }
        Text("[{q}] n={n} sent={sent}")
    }
}
"#,
    );
    app.key("Tab");
    app.typed("a");
    // A real keyboard sends the space as a key and as text.
    app.key_down(" ");
    app.typed(" ");
    app.key_up(" ");
    app.typed("b");
    assert_eq!(
        app.line(),
        "[a b] n=0 sent=0",
        "the space was typed, nothing pressed"
    );
    app.key("Enter");
    assert_eq!(app.line(), "[a b] n=0 sent=1", "Enter submitted the field");

    // And the button next to it is a stop of its own.
    app.key("Tab");
    app.key(" ");
    assert_eq!(app.line(), "[a b] n=1 sent=1");
}

#[test]
fn a_button_inside_another_focusable_is_its_own_stop() {
    let mut app = App::new(
        r#"
View Main() {
    var n = 0
    var outer = false
    Column {
        Box #[width: 200, height: 80, focused: outer, bg: 0x333333] {
            Button("In") #[width: 100, height: 40] => { n += 1 }
        }
        Text("n={n} outer={outer}")
    }
}
"#,
    );
    app.key("Tab");
    assert_eq!(app.line(), "n=0 outer=true", "the box comes first");
    app.key(" ");
    assert_eq!(
        app.line(),
        "n=0 outer=true",
        "Space does nothing to a plain box"
    );
    app.key("Tab");
    assert_eq!(app.line(), "n=0 outer=false", "then the button inside it");
    app.key(" ");
    assert_eq!(app.line(), "n=1 outer=false");
}

#[test]
fn a_button_whose_tap_is_a_callback_prop_is_pressed_by_the_keyboard() {
    let mut app = App::new(
        r#"
View Main() {
    var n = 0
    Column {
        Pressable(on_press: { n += 1 })
        Text("n={n}")
    }
}
View Pressable(on_press: Fn()) {
    Button("Go") #[width: 100, height: 40] => on_press()
}
"#,
    );
    app.key("Tab");
    app.key(" ");
    assert_eq!(app.line(), "n=1");
}

#[test]
fn a_button_with_nothing_to_press_is_not_a_stop() {
    let mut app = App::new(
        r#"
View Main() {
    var n = 0
    Column {
        Button("Inert") #[width: 100, height: 40]
        Button("Add") #[width: 100, height: 40] => { n += 1 }
        Text("n={n}")
    }
}
"#,
    );
    app.key("Tab");
    app.key(" ");
    assert_eq!(
        app.line(),
        "n=1",
        "the first stop is the button that does something"
    );
}

#[test]
fn focusable_controls_add_no_signals_frame_after_frame() {
    // Every focusable without `focused:` used to make a signal of its own each
    // frame it was registered.
    let mut app = App::new(
        r#"
View Main() {
    var a = false
    var q: Str = ""
    var plan = "x"
    var v = 0.5
    var n = 0
    Column {
        Button("Go") #[width: 80, height: 30] => { n += 1 }
        Toggle #[bind: a, width: 52, height: 30]
        Slider #[bind: v, width: 100, height: 28]
        Checkbox #[bind: a, width: 24, height: 24]
        RadioButton #[bind: plan, value: "x", width: 24, height: 24]
        TextField #[bind: q, width: 100, height: 30]
        Box #[width: 10, height: 10, focus => { n += 1 }] {}
    }
}
"#,
    );
    app.send(&[]);
    let before = app.interp.signal_count();
    for _ in 0..100 {
        app.send(&[]);
    }
    assert_eq!(app.interp.signal_count(), before);
}
