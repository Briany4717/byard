//! A modal overlay's panel, its scrim, and presses that do not bubble.
//!
//! `dismiss` runs for a press on the scrim, meaning outside the overlay's
//! content. A press on the panel's padding, on a `Text` or on a `TextField`
//! inside it belongs to the panel, and a `tap` on the panel is not run when
//! the press landed on something above it that listens.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};

const W: f32 = 600.0;
const H: f32 = 300.0;

/// The dialog of the bug report: a 300 wide panel centred in 600x300, with its
/// padding, a title, a 200x40 field and a button.
const DIALOG: &str = r#"
View Main() {
    var open = true
    var text = ""
    var panel_taps = 0
    Column #[grow: 1, bg: 0xFFFFFF] {
        Text("open={open} text={text}")
        when open { Overlay #[dismiss => { open = false }] {
            Column #[anchor: center, bg: 0x333344, p: 20, gap: 10, width: 300] {
                Text("A dialog")
                TextField #[bind: text, width: 200, height: 40,
                            placeholder: "Order number, delivery, reason"]
                Button("Ok") => open = false } } }
    }
}
"#;

struct App {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    frame: RenderFrame,
}

fn ev(kind: EventKind, pos: (f32, f32), t: u64, payload: Option<InputPayload>) -> InputEvent {
    InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload,
        time_ms: t,
        mods: KeyModifiers::default(),
    }
}

impl App {
    fn new(src: &str) -> Self {
        let parsed = parse(src);
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
        };
        app.render();
        app
    }

    fn render(&mut self) {
        self.interp.tick();
        self.frame = RenderFrame::new();
        self.interp.render(&self.tree, &mut self.frame, W, H);
    }

    fn tap(&mut self, pos: (f32, f32)) {
        self.interp.dispatch_events(&[
            ev(EventKind::PointerDown, pos, 0, None),
            ev(EventKind::PointerUp, pos, 8, None),
        ]);
        self.render();
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.interp.dispatch_events(&[ev(
                EventKind::TextInput,
                (0.0, 0.0),
                0,
                Some(InputPayload::Key(c.to_string())),
            )]);
            self.render();
        }
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

/// Panel padding, the title, the field, and the scrim (top-left corner).
const PADDING: (f32, f32) = (160.0, 150.0);
const TITLE: (f32, f32) = (200.0, 85.0);
const FIELD: (f32, f32) = (300.0, 140.0);
const SCRIM: (f32, f32) = (10.0, 10.0);

#[test]
fn a_press_on_the_panels_padding_does_not_dismiss() {
    let mut app = App::new(DIALOG);
    app.tap(PADDING);
    assert!(app.open(), "padding is the panel's, not the scrim's");
}

#[test]
fn a_press_on_a_text_in_the_panel_does_not_dismiss() {
    let mut app = App::new(DIALOG);
    app.tap(TITLE);
    assert!(app.open());
}

#[test]
fn a_press_on_the_scrim_dismisses() {
    let mut app = App::new(DIALOG);
    app.tap(SCRIM);
    assert!(!app.open());
}

#[test]
fn a_press_on_the_field_focuses_it_and_typing_goes_into_it() {
    let mut app = App::new(DIALOG);
    app.tap(FIELD);
    app.type_text("hello");
    assert!(app.open(), "the dialog stays open");
    assert_eq!(app.var("text"), Value::Str("hello".into()));
}

/// The workaround apps used for the bug above, `tap => {}` on the panel. It
/// must not turn the panel into something that also hears the field's press.
#[test]
fn a_tap_on_the_panel_is_not_run_by_a_press_on_the_field_inside_it() {
    let src = r#"
View Main() {
    var open = true
    var text = ""
    var panel_taps = 0
    Column #[grow: 1] {
        when open { Overlay #[dismiss => { open = false }] {
            Column #[anchor: center, bg: 0x333344, p: 20, gap: 10, width: 300,
                     tap => panel_taps++] {
                Text("A dialog")
                TextField #[bind: text, width: 200, height: 40] } } }
    }
}
"#;
    let mut app = App::new(src);
    // Without the button the panel is shorter, so the field sits lower.
    app.tap((300.0, 160.0));
    assert_eq!(
        app.var("panel_taps"),
        Value::Int(0),
        "the field has the press"
    );
    app.tap(PADDING);
    assert_eq!(
        app.var("panel_taps"),
        Value::Int(1),
        "padding is the panel's"
    );
    assert!(app.open());
}

/// The same rule without an overlay: a press does not bubble to a listener
/// beneath the element that has it.
#[test]
fn a_press_does_not_bubble_to_an_element_under_the_one_pressed() {
    let src = r"
View Main() {
    var outer = 0
    var down = 0
    Column #[p: 20, width: 300, height: 200, tap => outer++] {
        Box #[width: 100, height: 100, pointer_down => down++] {}
    }
}
";
    let mut app = App::new(src);
    app.tap((50.0, 50.0));
    assert_eq!(app.var("down"), Value::Int(1));
    assert_eq!(
        app.var("outer"),
        Value::Int(0),
        "the box listens and has the press"
    );
    app.tap((250.0, 150.0));
    assert_eq!(app.var("outer"), Value::Int(1));
}

#[test]
fn a_modal_overlay_dims_the_page_with_a_scrim() {
    let app = App::new(DIALOG);
    let scrim = app
        .frame
        .instances()
        .iter()
        .find(|b| b.rect == [0.0, 0.0, W, H] && b.color[3] > 0.0 && b.color[3] < 1.0)
        .expect("a translucent box covers the viewport");
    assert!((scrim.color[3] - 0.32).abs() < 0.01, "{:?}", scrim.color);
}

#[test]
fn the_scrim_can_be_replaced_or_turned_off() {
    let own = DIALOG.replace("Overlay #[", "Overlay #[scrim: 0x80FF0000, ");
    let app = App::new(&own);
    assert!(
        app.frame
            .instances()
            .iter()
            .any(|b| b.rect == [0.0, 0.0, W, H]
                && b.color[0] > 0.9
                && b.color[1] < 0.1
                && b.color[3] < 0.9),
        "the overlay's own scrim colour"
    );
    let off = DIALOG.replace("Overlay #[", "Overlay #[scrim: 0x00000000, ");
    let app = App::new(&off);
    assert!(
        !app.frame
            .instances()
            .iter()
            .any(|b| b.rect == [0.0, 0.0, W, H] && b.color[3] < 1.0),
        "a transparent scrim draws nothing"
    );
}

#[test]
fn a_non_modal_overlay_draws_no_scrim() {
    let src = DIALOG.replace("Overlay #[", "Overlay #[modal: false, ");
    let app = App::new(&src);
    assert!(
        !app.frame
            .instances()
            .iter()
            .any(|b| b.rect == [0.0, 0.0, W, H] && b.color[3] < 1.0),
        "no translucent box over the page"
    );
}

/// The placeholder is cut at the field's content box, as typed text is.
#[test]
fn a_long_placeholder_is_clipped_to_the_field() {
    let app = App::new(DIALOG);
    let clips = app.frame.clips();
    assert!(
        clips.iter().any(|c| (c.rect.width - 180.0).abs() < 0.5),
        "a clip the width of the field's content box: {clips:?}"
    );
}

/// RFC-0017's way of writing a scrim, a first child that fills the viewport,
/// still works and is not dimmed a second time by the default one.
#[test]
fn an_overlay_that_draws_its_own_scrim_gets_no_second_one() {
    let src = r"
View Main() {
    var open = true
    when open { Overlay #[dismiss => { open = false }] {
        Box #[bg: 0x1D1B20, opacity: 0.32, grow: 1] {}
        Column #[anchor: center, bg: 0x333344, width: 100, height: 60] {} } }
}
";
    let mut app = App::new(src);
    let full = |app: &App| {
        app.frame
            .decorated()
            .iter()
            .map(|d| d.base.rect)
            .chain(app.frame.instances().iter().map(|b| b.rect))
            .filter(|r| *r == [0.0, 0.0, W, H])
            .count()
    };
    assert_eq!(full(&app), 1, "only the authored scrim covers the page");
    app.tap(SCRIM);
    assert!(!app.open(), "a press on it dismisses");
}
