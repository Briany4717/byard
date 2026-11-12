//! RFC-0061: layout motion. A size, a gap, a padding or a margin moves on a
//! `with` (also inside `on <state>`, for a pointer and for a held key), a
//! radius or spacing tuple takes expressions and moves slot by slot, and a
//! `let` in a `for` body feeds a length.

#![allow(clippy::float_cmp)]

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::{BoxInstance, RenderFrame};
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};

const W: f32 = 400.0;
const H: f32 = 400.0;

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
        assert!(app.interp.errors().is_empty(), "{:?}", app.interp.errors());
        app
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.set_now_ms(u32::try_from(self.t).unwrap());
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp.render(&self.tree, &mut self.frame, W, H);
    }

    fn wait(&mut self, ms: u64) {
        self.t += ms;
        self.send(&[]);
    }

    fn event(
        &mut self,
        kind: EventKind,
        payload: Option<InputPayload>,
        at: (f32, f32),
    ) -> InputEvent {
        self.t += 10;
        InputEvent {
            kind,
            pos: at,
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods: KeyModifiers::default(),
        }
    }

    fn key_down(&mut self, name: &str) {
        let e = self.event(
            EventKind::KeyDown,
            Some(InputPayload::Key(name.into())),
            (0.0, 0.0),
        );
        self.send(&[e]);
    }

    fn key_up(&mut self, name: &str) {
        let e = self.event(
            EventKind::KeyUp,
            Some(InputPayload::Key(name.into())),
            (0.0, 0.0),
        );
        self.send(&[e]);
    }

    fn press(&mut self, at: (f32, f32)) {
        let e = self.event(EventKind::PointerDown, None, at);
        self.send(&[e]);
    }

    fn release(&mut self, at: (f32, f32)) {
        let e = self.event(EventKind::PointerUp, None, at);
        self.send(&[e]);
    }

    fn set_bool(&mut self, name: &str, on: bool) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, Value::Bool(on));
        self.interp.tick();
        self.t += 1;
        self.send(&[]);
    }

    /// The solid box painted with `rgb` (its fill), whatever its size.
    fn by_colour(&self, rgb: u8) -> BoxInstance {
        let want = srgb(rgb);
        *self
            .frame
            .instances()
            .iter()
            .find(|b| (b.color[0] - want).abs() < 0.01)
            .unwrap_or_else(|| panic!("no box with red {rgb:#x} in {:?}", self.frame.instances()))
    }

    fn animating(&self) -> bool {
        self.interp.has_active_animations()
    }
}

/// The linear value of a red channel written as a byte.
fn srgb(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.6
}

/// A box of a colour whose red byte is `0x11`, the one under test, and a
/// marker of red `0x22` after it.
fn row(attrs: &str) -> String {
    format!(
        "View Main() {{ var on = false \
         Row #[width: 400] {{ \
           Box #[bg: 0x110000, {attrs}] {{}} \
           Box #[bg: 0x220000, width: 30, height: 30] {{}} }} }}"
    )
}

// ── width, height ────────────────────────────────────────────────────────

#[test]
fn a_width_moves_on_its_curve_and_the_neighbour_moves_with_it() {
    let mut app = App::new(&row(
        "height: 40, width: on ? 200 : 100 with anim.linear(1000)",
    ));
    assert!(near(app.by_colour(0x11).rect[2], 100.0));
    assert!(near(app.by_colour(0x22).rect[0], 100.0));
    assert!(!app.animating(), "nothing moves at rest");

    app.set_bool("on", true);
    assert!(app.animating());
    app.wait(500);
    let mid = app.by_colour(0x11).rect[2];
    assert!((140.0..160.0).contains(&mid), "half way, got {mid}");
    assert!(
        (app.by_colour(0x22).rect[0] - mid).abs() < 0.6,
        "the next box sits right after it"
    );
    app.wait(600);
    assert!(near(app.by_colour(0x11).rect[2], 200.0));
    assert!(near(app.by_colour(0x22).rect[0], 200.0));
    assert!(!app.animating(), "settled");
}

#[test]
fn a_height_moves_on_its_curve() {
    let mut app = App::new(&row(
        "width: 50, height: on ? 100 : 40 with anim.linear(1000)",
    ));
    app.set_bool("on", true);
    app.wait(500);
    let h = app.by_colour(0x11).rect[3];
    assert!((65.0..75.0).contains(&h), "half way, got {h}");
    app.wait(600);
    assert!(near(app.by_colour(0x11).rect[3], 100.0));
}

#[test]
fn a_motion_that_does_not_move_marks_no_frame_active() {
    let mut app = App::new(&row(
        "height: 40, width: on ? 200 : 100 with anim.linear(1000)",
    ));
    for _ in 0..5 {
        app.wait(100);
        assert!(!app.animating());
    }
}

// ── padding, margin, gap ─────────────────────────────────────────────────

fn column_child_x(attrs: &str) -> String {
    format!(
        "View Main() {{ var on = false \
         Row #[width: 400, {attrs}] {{ \
           Box #[bg: 0x110000, width: 20, height: 20] {{}} }} }}"
    )
}

#[test]
fn a_padding_side_moves() {
    let mut app = App::new(&column_child_x("pl: on ? 40 : 0 with anim.linear(1000)"));
    assert!(near(app.by_colour(0x11).rect[0], 0.0));
    app.set_bool("on", true);
    app.wait(500);
    let x = app.by_colour(0x11).rect[0];
    assert!((15.0..25.0).contains(&x), "half way, got {x}");
    app.wait(600);
    assert!(near(app.by_colour(0x11).rect[0], 40.0));
}

#[test]
fn a_padding_pair_moves_slot_by_slot() {
    let mut app = App::new(&column_child_x(
        "p: (on ? 20 : 0, on ? 40 : 0) with anim.linear(1000)",
    ));
    app.set_bool("on", true);
    app.wait(500);
    let b = app.by_colour(0x11).rect;
    assert!((8.0..12.0).contains(&b[1]), "top, got {b:?}");
    assert!((15.0..25.0).contains(&b[0]), "left, got {b:?}");
    app.wait(600);
    let b = app.by_colour(0x11).rect;
    assert!(near(b[1], 20.0) && near(b[0], 40.0), "{b:?}");
}

#[test]
fn a_margin_moves() {
    let mut app = App::new(
        "View Main() { var on = false \
         Row #[width: 400] { \
           Box #[bg: 0x110000, width: 20, height: 20, ml: on ? 40 : 0 with anim.linear(1000)] {} } }",
    );
    app.set_bool("on", true);
    app.wait(500);
    let x = app.by_colour(0x11).rect[0];
    assert!((15.0..25.0).contains(&x), "half way, got {x}");
    app.wait(600);
    assert!(near(app.by_colour(0x11).rect[0], 40.0));
}

#[test]
fn a_gap_moves() {
    let mut app = App::new(
        "View Main() { var on = false \
         Row #[width: 400, gap: on ? 40 : 0 with anim.linear(1000)] { \
           Box #[bg: 0x110000, width: 20, height: 20] {} \
           Box #[bg: 0x220000, width: 20, height: 20] {} } }",
    );
    assert!(near(app.by_colour(0x22).rect[0], 20.0));
    app.set_bool("on", true);
    app.wait(500);
    let x = app.by_colour(0x22).rect[0];
    assert!((35.0..45.0).contains(&x), "half way, got {x}");
    app.wait(600);
    assert!(near(app.by_colour(0x22).rect[0], 60.0));
}

#[test]
fn a_text_width_and_a_button_padding_move() {
    // The leaf readers go through the same chokepoint as a container's.
    let mut app = App::new(
        "View Main() { var on = false \
         Column #[width: 400] { \
           Text(\"a\") #[wrap: false, width: on ? 100 : 20 with anim.linear(1000)] \
           Box #[bg: 0x110000, width: 20, height: 20] {} } }",
    );
    app.set_bool("on", true);
    app.wait(1100);
    assert!(!app.animating());
}

// ── a state block reaches layout ─────────────────────────────────────────

const PILL: &str = "View Main() { var n = 0 \
    Row #[width: 400] { \
      Box #[bg: 0x110000, width: 80, height: 40, a11y_role: button, tap => n += 1] { \
        on pressed { width: 120 with anim.linear(1000) } } \
      Box #[bg: 0x220000, width: 30, height: 30] {} } }";

#[test]
fn a_pointer_press_widens_the_box_and_letting_go_brings_it_back() {
    let mut app = App::new(PILL);
    assert!(near(app.by_colour(0x11).rect[2], 80.0));
    app.press((40.0, 20.0));
    app.wait(500);
    let w = app.by_colour(0x11).rect[2];
    assert!((95.0..105.0).contains(&w), "half way out, got {w}");
    assert!(
        (app.by_colour(0x22).rect[0] - w).abs() < 0.6,
        "the neighbour moves"
    );
    app.wait(600);
    assert!(near(app.by_colour(0x11).rect[2], 120.0));
    assert!(!app.animating());

    app.release((40.0, 20.0));
    app.wait(500);
    let w = app.by_colour(0x11).rect[2];
    assert!((95.0..105.0).contains(&w), "half way back, got {w}");
    app.wait(600);
    assert!(near(app.by_colour(0x11).rect[2], 80.0));
}

#[test]
fn a_held_space_widens_it_too() {
    let mut app = App::new(PILL);
    app.key_down("Tab");
    app.key_down("Space");
    app.wait(1100);
    assert!(near(app.by_colour(0x11).rect[2], 120.0));
    app.key_up("Space");
    app.wait(1100);
    assert!(near(app.by_colour(0x11).rect[2], 80.0));
}

#[test]
fn the_base_curve_is_the_curve_of_a_bare_state_value() {
    let mut app = App::new(
        "View Main() { var n = 0 \
         Row #[width: 400] { \
           Box #[bg: 0x110000, width: 80 with anim.linear(1000), height: 40, \
                 a11y_role: button, tap => n += 1] { \
             on pressed { width: 120 } } } }",
    );
    app.press((40.0, 20.0));
    app.wait(500);
    let w = app.by_colour(0x11).rect[2];
    assert!((95.0..105.0).contains(&w), "half way out, got {w}");
}

#[test]
fn a_state_padding_moves_the_content() {
    let mut app = App::new(
        "View Main() { var n = 0 \
         Row #[width: 400] { \
           Box #[width: 100, height: 40, a11y_role: button, tap => n += 1] { \
             on pressed { pl: 40 with anim.linear(1000) } \
             Box #[bg: 0x110000, width: 20, height: 20] {} } } }",
    );
    assert!(near(app.by_colour(0x11).rect[0], 0.0));
    app.press((50.0, 20.0));
    app.wait(1100);
    assert!(near(app.by_colour(0x11).rect[0], 40.0));
}

// ── radius ───────────────────────────────────────────────────────────────

#[test]
fn a_radius_tuple_moves_corner_by_corner() {
    let mut app = App::new(
        "View Main() { var on = false \
         Column #[width: 400] { \
           Box #[bg: 0x110000, width: 50, height: 50, \
                 radius: (on ? 20 : 4, 4, 4, on ? 12 : 4) with anim.linear(1000)] {} } }",
    );
    assert_eq!(app.by_colour(0x11).radii, [4.0; 4]);
    app.set_bool("on", true);
    app.wait(500);
    let r = app.by_colour(0x11).radii;
    assert!((11.0..13.0).contains(&r[0]), "{r:?}");
    assert!(near(r[1], 4.0) && near(r[2], 4.0), "{r:?}");
    assert!((7.0..9.0).contains(&r[3]), "{r:?}");
    app.wait(600);
    assert_eq!(app.by_colour(0x11).radii, [20.0, 4.0, 4.0, 12.0]);
    assert!(!app.animating());
}

#[test]
fn a_radius_may_choose_between_a_tuple_and_a_number() {
    let mut app = App::new(
        "View Main() { var on = false \
         Column #[width: 400] { \
           Box #[bg: 0x110000, width: 50, height: 50, \
                 radius: on ? (4, 4, 0, 0) : 8] {} } }",
    );
    assert_eq!(app.by_colour(0x11).radii, [8.0; 4]);
    app.set_bool("on", true);
    assert_eq!(app.by_colour(0x11).radii, [4.0, 4.0, 0.0, 0.0]);
}

#[test]
fn a_radius_may_choose_between_two_tuples() {
    let mut app = App::new(
        "View Main() { var on = false \
         Column #[width: 400] { \
           Box #[bg: 0x110000, width: 50, height: 50, \
                 radius: on ? (4, 4, 0, 0) : (0, 0, 8, 8)] {} } }",
    );
    assert_eq!(app.by_colour(0x11).radii, [0.0, 0.0, 8.0, 8.0]);
    app.set_bool("on", true);
    assert_eq!(app.by_colour(0x11).radii, [4.0, 4.0, 0.0, 0.0]);
}

#[test]
fn a_chosen_radius_moves_from_one_shape_to_the_other() {
    let mut app = App::new(
        "View Main() { var on = false \
         Column #[width: 400] { \
           Box #[bg: 0x110000, width: 50, height: 50, \
                 radius: on ? (4, 4, 0, 0) : 8 with anim.linear(1000)] {} } }",
    );
    app.set_bool("on", true);
    app.wait(500);
    let r = app.by_colour(0x11).radii;
    assert!((5.5..6.5).contains(&r[0]), "{r:?}");
    assert!((3.5..4.5).contains(&r[2]), "{r:?}");
    app.wait(600);
    assert_eq!(app.by_colour(0x11).radii, [4.0, 4.0, 0.0, 0.0]);
}

#[test]
fn a_state_radius_tuple_moves_on_press() {
    let mut app = App::new(
        "View Main() { var n = 0 \
         Column #[width: 400] { \
           Box #[bg: 0x110000, width: 50, height: 50, radius: (8, 8, 8, 8), \
                 a11y_role: button, tap => n += 1] { \
             on pressed { radius: (20, 8, 8, 20) with anim.linear(1000) } } } }",
    );
    app.press((25.0, 25.0));
    app.wait(1100);
    assert_eq!(app.by_colour(0x11).radii, [20.0, 8.0, 8.0, 20.0]);
}

// ── spacing tuples take a choice ─────────────────────────────────────────

#[test]
fn a_padding_may_choose_between_a_pair_and_a_number() {
    let mut app = App::new(&column_child_x("p: on ? (4, 30) : 10"));
    assert!(near(app.by_colour(0x11).rect[0], 10.0));
    app.set_bool("on", true);
    assert!(near(app.by_colour(0x11).rect[0], 30.0));
    assert!(near(app.by_colour(0x11).rect[1], 4.0));
}

#[test]
fn a_margin_pair_moves() {
    let mut app = App::new(
        "View Main() { var on = false \
         Row #[width: 400] { \
           Box #[bg: 0x110000, width: 20, height: 20, \
                 m: (on ? 20 : 0, on ? 40 : 0) with anim.linear(1000)] {} } }",
    );
    app.set_bool("on", true);
    app.wait(1100);
    let b = app.by_colour(0x11).rect;
    assert!(near(b[1], 20.0) && near(b[0], 40.0), "{b:?}");
}

// ── a `let` in a `for` body ──────────────────────────────────────────────

#[test]
fn a_let_in_a_for_body_feeds_a_length() {
    let app = App::new(
        "View Main() { var xs = [1, 2] \
         Column #[width: 400] { \
           for i, x in xs { \
             let q = 5 \
             Box #[bg: 0x110000, width: 40, height: 40, radius: q] {} } } }",
    );
    let boxes = app.frame.instances();
    assert_eq!(boxes.len(), 2);
    assert!(boxes.iter().all(|b| b.radii == [5.0; 4]), "{boxes:?}");
}

#[test]
fn a_let_in_a_for_body_may_use_the_row() {
    let app = App::new(
        "View Main() { var xs = [10, 20] \
         Column #[width: 400] { \
           for i, x in xs { \
             let w = x * 2 \
             let q = i + 3 \
             Box #[bg: 0x110000, width: w, height: 40, radius: q, p: q, gap: q] {} } } }",
    );
    let boxes = app.frame.instances();
    assert_eq!(boxes.len(), 2);
    assert!(near(boxes[0].rect[2], 20.0) && near(boxes[1].rect[2], 40.0));
    assert_eq!(boxes[0].radii, [3.0; 4]);
    assert_eq!(boxes[1].radii, [4.0; 4]);
}

#[test]
fn a_let_in_a_when_body_feeds_a_length() {
    let app = App::new(
        "View Main() { var on = true \
         Column #[width: 400] { \
           when on { let q = 7  Box #[bg: 0x110000, width: 40, height: 40, radius: q] {} } } }",
    );
    assert_eq!(app.by_colour(0x11).radii, [7.0; 4]);
}

// ── `on <state>` on a Text, a Grid and a ZStack ──────────────────────────

#[test]
fn a_text_state_padding_moves_its_box() {
    let mut app = App::new(
        "View Main() { var n = 0 \
         Row #[width: 400] { \
           Text(\"hello\") #[wrap: false, width: 60, a11y_role: button, tap => n += 1] { \
             on pressed { width: 120 with anim.linear(1000) } } \
           Box #[bg: 0x220000, width: 30, height: 30] {} } }",
    );
    assert!(near(app.by_colour(0x22).rect[0], 60.0));
    app.press((10.0, 8.0));
    app.wait(1100);
    assert!(near(app.by_colour(0x22).rect[0], 120.0));
    app.release((10.0, 8.0));
    app.wait(1100);
    assert!(near(app.by_colour(0x22).rect[0], 60.0));
}

#[test]
fn a_grid_state_width_moves_its_box() {
    let mut app = App::new(
        "View Main() { var n = 0 \
         Row #[width: 400] { \
           Grid #[columns: \"1fr\", rows: \"40\", width: 60, a11y_role: button, tap => n += 1] { \
             on pressed { width: 120 with anim.linear(1000) } \
             Box #[bg: 0x110000, height: 40] {} } \
           Box #[bg: 0x220000, width: 30, height: 30] {} } }",
    );
    assert!(near(app.by_colour(0x22).rect[0], 60.0));
    app.press((10.0, 10.0));
    app.wait(1100);
    assert!(near(app.by_colour(0x22).rect[0], 120.0));
}

#[test]
fn a_zstack_state_width_moves_its_box() {
    let mut app = App::new(
        "View Main() { var n = 0 \
         Row #[width: 400] { \
           ZStack #[width: 60, height: 40, a11y_role: button, tap => n += 1] { \
             on pressed { width: 120 with anim.linear(1000) } \
             Box #[bg: 0x110000, width: 20, height: 20] {} } \
           Box #[bg: 0x220000, width: 30, height: 30] {} } }",
    );
    assert!(near(app.by_colour(0x22).rect[0], 60.0));
    app.press((10.0, 10.0));
    app.wait(1100);
    assert!(near(app.by_colour(0x22).rect[0], 120.0));
}
