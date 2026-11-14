//! RFC-0059: interaction styles. `on hover` and its kin in an element's body,
//! `outline` and `overlay`, a role from a value, a held key that shows
//! `on pressed`, and the paint bugs the Material package ran into.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::theme::{DeclaredFont, FontWeight, Theme, TypoToken};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::access::{AccessNode, AccessRole};
use byard_core::frame::{BoxInstance, DecoratedBox, RenderFrame};
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};
use std::sync::Arc;

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
        Self::with_theme(src, None)
    }

    fn with_theme(src: &str, theme: Option<Theme>) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        if let Some(theme) = theme {
            interp.set_theme(theme);
        }
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
        self.frame.set_access_wanted(true);
        self.interp.render(&self.tree, &mut self.frame, W, H);
    }

    /// Renders again `ms` later, with no input.
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

    fn key(&mut self, name: &str) {
        self.key_down(name);
        self.key_up(name);
    }

    fn hover(&mut self, at: (f32, f32)) {
        let e = self.event(EventKind::PointerMove, None, at);
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

    fn int(&self, name: &str) -> i64 {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        match self.interp.peek(sig) {
            Value::Int(n) => n,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn set_bool(&mut self, name: &str, on: bool) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, Value::Bool(on));
        self.interp.tick();
        self.t += 1;
        self.send(&[]);
    }

    /// The first solid box whose size is `w` by `h`.
    fn boxed(&self, w: f32, h: f32) -> BoxInstance {
        *self
            .frame
            .instances()
            .iter()
            .find(|b| (b.rect[2] - w).abs() < 0.5 && (b.rect[3] - h).abs() < 0.5)
            .unwrap_or_else(|| panic!("no {w}x{h} solid box in {:?}", self.frame.instances()))
    }

    /// The decorated boxes, with their draw-order depth.
    fn decorated(&self) -> Vec<(DecoratedBox, f32)> {
        self.frame
            .decorated()
            .iter()
            .copied()
            .zip(self.frame.decorated_depths().iter().copied())
            .collect()
    }

    fn access(&self) -> Vec<&AccessNode> {
        self.frame.access().expect("a tree").nodes.iter().collect()
    }
}

/// The tap centre of the first box of `w` by `h`.
fn centre(app: &App, w: f32, h: f32) -> (f32, f32) {
    let b = app.boxed(w, h);
    (b.rect[0] + b.rect[2] / 2.0, b.rect[1] + b.rect[3] / 2.0)
}

fn errors_of(src: &str) -> Vec<String> {
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, W, H);
    interp.errors().iter().map(|e| format!("{e:?}")).collect()
}

fn messages(errors: &[byard_compiler::CompileError]) -> String {
    errors
        .iter()
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

// ── outline ─────────────────────────────────────────────────────────────

const OUTLINED: &str = "View Main() { Column #[width: 400, p: 20] { \
    Box #[bg: 0x6750A4, radius: 10, width: 100, height: 40, \
          outline: 0xFF0000, outline_width: 3, outline_offset: 2] {} } }";

#[test]
fn an_outline_is_a_ring_outside_the_shape_that_follows_the_radius() {
    let app = App::new(OUTLINED);
    let body = app.boxed(100.0, 40.0);
    let ring = app
        .decorated()
        .into_iter()
        .map(|(d, _)| d)
        .find(|d| d.border_width > 0.0)
        .expect("a ring is drawn");
    // Offset 2 and width 3 put the ring's outer edge 5 outside the box.
    assert!(
        approx(ring.base.rect[0], body.rect[0] - 5.0),
        "{:?}",
        ring.base.rect
    );
    assert!(
        approx(ring.base.rect[1], body.rect[1] - 5.0),
        "{:?}",
        ring.base.rect
    );
    assert!(approx(ring.base.rect[2], 110.0), "{:?}", ring.base.rect);
    assert!(approx(ring.base.rect[3], 50.0), "{:?}", ring.base.rect);
    assert!(approx(ring.border_width, 3.0));
    // The radius 10 grows with the offset and the width.
    assert!(
        ring.base.radii.iter().all(|r| approx(*r, 15.0)),
        "{:?}",
        ring.base.radii
    );
    // Red, and nothing inside it: the interior is transparent.
    assert!(ring.border_color[0] > 0.9 && ring.border_color[3] > 0.99);
    assert_eq!(ring.base.color, [0.0; 4]);
}

#[test]
fn an_outline_takes_no_room_in_layout() {
    let plain = "View Main() { Column #[width: 400, p: 20, gap: 8] { \
        Box #[bg: 0x6750A4, radius: 10, width: 100, height: 40] {} \
        Box #[bg: 0x445566, width: 100, height: 40] {} } }";
    let ringed = "View Main() { Column #[width: 400, p: 20, gap: 8] { \
        Box #[bg: 0x6750A4, radius: 10, width: 100, height: 40, \
              outline: 0xFF0000, outline_width: 6, outline_offset: 6] {} \
        Box #[bg: 0x445566, width: 100, height: 40] {} } }";
    let a: Vec<[f32; 4]> = App::new(plain)
        .frame
        .instances()
        .iter()
        .map(|b| b.rect)
        .collect();
    let b: Vec<[f32; 4]> = App::new(ringed)
        .frame
        .instances()
        .iter()
        .map(|b| b.rect)
        .collect();
    assert_eq!(a, b);
}

#[test]
fn a_square_element_has_a_square_ring_and_the_width_defaults_to_two() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, width: 100, height: 40, outline: 0xFF0000] {} } }",
    );
    let ring = app
        .decorated()
        .into_iter()
        .map(|(d, _)| d)
        .find(|d| d.border_width > 0.0)
        .expect("a ring");
    assert!(approx(ring.border_width, 2.0));
    assert!(
        ring.base.radii.iter().all(|r| *r == 0.0),
        "{:?}",
        ring.base.radii
    );
    assert!(approx(ring.base.rect[2], 104.0), "{:?}", ring.base.rect);
}

#[test]
fn a_negative_offset_draws_the_ring_inside() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, radius: 8, width: 100, height: 40, \
               outline: 0xFF0000, outline_width: 2, outline_offset: -4] {} } }",
    );
    let ring = app
        .decorated()
        .into_iter()
        .map(|(d, _)| d)
        .find(|d| d.border_width > 0.0)
        .expect("a ring");
    assert!(approx(ring.base.rect[2], 96.0), "{:?}", ring.base.rect);
    assert!(approx(ring.base.rect[3], 36.0), "{:?}", ring.base.rect);
    assert!(
        ring.base.radii.iter().all(|r| approx(*r, 6.0)),
        "{:?}",
        ring.base.radii
    );
}

#[test]
fn a_transparent_outline_draws_nothing() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, width: 100, height: 40, outline: 0x00FF0000, outline_width: 3] {} } }",
    );
    assert!(app.decorated().is_empty(), "{:?}", app.decorated());
}

#[test]
fn an_element_with_no_outline_or_overlay_draws_as_it_did() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, radius: 10, width: 100, height: 40] {} } }",
    );
    assert!(app.decorated().is_empty());
    assert_eq!(app.frame.instances().len(), 1, "just the box");
}

#[test]
fn outline_width_and_colour_animate() {
    let mut app = App::new(
        "View Main() { var on: Bool = false \
         Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, radius: 8, width: 100, height: 40, \
               outline: on ? 0xFF0000 : 0x00FF0000 with anim.linear(1000), \
               outline_width: on ? 6 : 2 with anim.linear(1000)] {} } }",
    );
    assert!(app.decorated().is_empty(), "invisible at rest");
    app.set_bool("on", true);
    app.wait(500);
    let ring = app
        .decorated()
        .into_iter()
        .map(|(d, _)| d)
        .find(|d| d.border_width > 0.0)
        .expect("fading in");
    assert!(
        (3.0..5.0).contains(&ring.border_width),
        "{}",
        ring.border_width
    );
    assert!(
        (0.2..0.8).contains(&ring.border_color[3]),
        "{:?}",
        ring.border_color
    );
    app.wait(1500);
    let ring = app
        .decorated()
        .into_iter()
        .map(|(d, _)| d)
        .find(|d| d.border_width > 0.0)
        .unwrap();
    assert!(approx(ring.border_width, 6.0));
    assert!(ring.border_color[3] > 0.99);
}

// ── overlay ─────────────────────────────────────────────────────────────

const TINTED: &str = "View Main() { Column #[width: 400, p: 20] { \
    Box #[bg: 0x6750A4, radius: 12, width: 100, height: 40, overlay: 0x40FFFFFF] { \
        Box #[bg: 0x112233, width: 10, height: 10] {} } } }";

#[test]
fn an_overlay_is_drawn_over_the_fill_and_under_the_children() {
    let app = App::new(TINTED);
    let body = app.boxed(100.0, 40.0);
    let child = app.boxed(10.0, 10.0);
    let tint = app
        .decorated()
        .into_iter()
        .find(|(d, _)| d.base.color[3] > 0.0)
        .expect("an overlay is drawn");
    let (tint, depth) = tint;
    assert_eq!(
        tint.base.rect, body.rect,
        "clipped to the element's own shape"
    );
    assert!(
        tint.base.radii.iter().all(|r| approx(*r, 12.0)),
        "{:?}",
        tint.base.radii
    );
    assert!(
        (tint.base.color[3] - 64.0 / 255.0).abs() < 0.01,
        "{:?}",
        tint.base.color
    );
    // Later is nearer (a smaller depth): the fill, the tint, the child.
    let depths = app.frame.solid_depths();
    let at = |rect: [f32; 4]| {
        let i = app
            .frame
            .instances()
            .iter()
            .position(|b| b.rect.iter().zip(rect).all(|(a, b)| approx(*a, b)))
            .unwrap();
        depths[i]
    };
    assert!(at(body.rect) > depth, "the tint is nearer than the fill");
    assert!(depth > at(child.rect), "the child is nearer than the tint");
}

#[test]
fn a_transparent_overlay_draws_nothing() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, width: 100, height: 40, overlay: 0x00FFFFFF] {} } }",
    );
    assert!(app.decorated().is_empty());
}

#[test]
fn an_overlay_can_be_a_gradient() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, radius: 8, width: 100, height: 40, \
               overlay: (angle: 90deg, from: 0x00FFFFFF, to: 0x80FFFFFF)] {} } }",
    );
    let (tint, _) = app
        .decorated()
        .into_iter()
        .find(|(d, _)| d.gradient.is_some())
        .expect("a gradient overlay");
    assert!(tint.gradient.unwrap().to[3] > 0.4);
}

#[test]
fn an_overlay_fades_with_with() {
    let mut app = App::new(
        "View Main() { var on: Bool = false \
         Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, width: 100, height: 40, \
               overlay: on ? 0xFFFFFFFF : 0x00FFFFFF with anim.linear(1000)] {} } }",
    );
    app.set_bool("on", true);
    app.wait(500);
    let alpha = |app: &App| {
        app.decorated()
            .into_iter()
            .map(|(d, _)| d.base.color[3])
            .fold(0.0_f32, f32::max)
    };
    let mid = alpha(&app);
    assert!((0.2..0.8).contains(&mid), "mid-way, got {mid}");
    app.wait(1500);
    assert!(alpha(&app) > 0.99);
}

// ── state blocks in the body ────────────────────────────────────────────

const PILL: &str = "View Main() { var n = 0 \
    Column #[width: 400, p: 20] { \
    Box #[bg: 0x6750A4, radius: 20, width: 120, height: 40, a11y_role: button, tap => n += 1] { \
        on hover { overlay: 0x14FFFFFF } \
        on pressed { overlay: 0x1AFFFFFF, radius: 8 } \
        on focus_visible { outline: 0xD0BCFF, outline_width: 3, outline_offset: 2 } \
    } } }";

fn overlay_alpha(app: &App) -> f32 {
    app.decorated()
        .into_iter()
        .filter(|(d, _)| d.border_width == 0.0)
        .map(|(d, _)| d.base.color[3])
        .fold(0.0_f32, f32::max)
}

fn has_ring(app: &App) -> bool {
    app.decorated().iter().any(|(d, _)| d.border_width > 0.0)
}

#[test]
fn hover_press_and_focus_blocks_in_the_body_style_the_element() {
    let mut app = App::new(PILL);
    assert_eq!(overlay_alpha(&app), 0.0);
    let at = centre(&app, 120.0, 40.0);

    app.hover(at);
    assert!(
        (overlay_alpha(&app) - 20.0 / 255.0).abs() < 0.01,
        "hover tint"
    );

    app.press(at);
    assert!(
        (overlay_alpha(&app) - 26.0 / 255.0).abs() < 0.01,
        "pressed tint wins"
    );
    assert!(
        approx(app.boxed(120.0, 40.0).radii[0], 8.0),
        "pressed radius"
    );

    app.release(at);
    assert!(
        approx(app.boxed(120.0, 40.0).radii[0], 20.0),
        "back to rest"
    );
    assert!(!has_ring(&app), "a pointer press is not focus_visible");
    assert_eq!(app.int("n"), 1);

    app.hover((0.0, 0.0));
    assert_eq!(overlay_alpha(&app), 0.0, "the pointer left");
}

#[test]
fn tab_focus_draws_the_ring_and_a_tap_does_not() {
    let mut app = App::new(PILL);
    assert!(!has_ring(&app));
    app.key("Tab");
    assert!(has_ring(&app), "Tab shows the ring");
    let ring = app
        .decorated()
        .into_iter()
        .map(|(d, _)| d)
        .find(|d| d.border_width > 0.0)
        .unwrap();
    assert!(
        approx(ring.base.rect[2], 120.0 + 10.0),
        "{:?}",
        ring.base.rect
    );
}

#[test]
fn a_body_block_wins_a_tie_with_a_style_and_the_two_mix() {
    let mut app = App::new(
        "View Main() { \
         let base = style { bg: 0x6750A4, on hover { overlay: 0x40FF0000 } } \
         Column #[width: 400, p: 20] { \
         Box #[..base, width: 100, height: 40] { on hover { overlay: 0x80FFFFFF } } } }",
    );
    let at = centre(&app, 100.0, 40.0);
    app.hover(at);
    let (d, _) = app.decorated().into_iter().next().expect("an overlay");
    assert!(
        (d.base.color[3] - 128.0 / 255.0).abs() < 0.01,
        "{:?}",
        d.base.color
    );
    assert!(
        d.base.color[1] > 0.9,
        "white, not the style's red: {:?}",
        d.base.color
    );
}

#[test]
fn a_state_block_outside_an_element_says_where_it_goes() {
    let parsed = parse("View Main() { on hover { bg: 0xFF0000 } Box #[width: 10, height: 10] {} }");
    let msg = messages(&parsed.errors);
    assert!(!parsed.errors.is_empty(), "silently accepted");
    assert!(msg.contains("inside the element"), "{msg}");
}

#[test]
fn an_unknown_state_in_a_body_is_the_usual_error() {
    let parsed = parse("View Main() { Box #[width: 10, height: 10] { on hovr { bg: 0xFF0000 } } }");
    let msg = messages(&parsed.errors);
    assert!(msg.contains("UnknownStyleState"), "{msg}");
    assert!(msg.contains("hover"), "{msg}");
}

#[test]
fn the_other_on_forms_still_parse_in_a_body() {
    let parsed = parse(
        "View Main() { var a = 0 Box #[width: 10, height: 10] { \
         on mount => a = 1 on change(a) => a = 2 on hover { bg: 0xFF0000 } } }",
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
}

#[test]
fn the_formatter_keeps_a_body_block() {
    let src = "View Main() {\n    Box #[width: 10, height: 10] {\n        on hover { overlay: 0x14FFFFFF }\n        on focus_visible { outline: 0xD0BCFF, outline_width: 3 }\n    }\n}\n";
    let out = byard_compiler::fmt::format(src).expect("it formats");
    assert_eq!(out, src);
}

// ── a state that returns by the curve it left on ────────────────────────

#[test]
fn a_state_returns_to_rest_on_the_curve_it_left_on() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, radius: 20, width: 120, height: 40, tap => {}] { \
             on pressed { radius: 4 with anim.linear(1000) } } } }",
    );
    let at = centre(&app, 120.0, 40.0);
    app.press(at);
    app.wait(1100);
    assert!(
        approx(app.boxed(120.0, 40.0).radii[0], 4.0),
        "pressed radius"
    );
    app.release(at);
    app.wait(500);
    let r = app.boxed(120.0, 40.0).radii[0];
    assert!((8.0..16.0).contains(&r), "half way back, not snapped: {r}");
    app.wait(1200);
    assert!(approx(app.boxed(120.0, 40.0).radii[0], 20.0));
}

#[test]
fn an_overlay_set_only_in_a_state_fades_out_with_its_curve() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[bg: 0x6750A4, width: 100, height: 40] { \
             on hover { overlay: 0xFFFFFFFF with anim.linear(1000) } } } }",
    );
    let at = centre(&app, 100.0, 40.0);
    app.hover(at);
    app.wait(1200);
    assert!(overlay_alpha(&app) > 0.99);
    app.hover((0.0, 0.0));
    app.wait(500);
    let mid = overlay_alpha(&app);
    assert!((0.2..0.8).contains(&mid), "fading out, not gone: {mid}");
    app.wait(1200);
    assert_eq!(overlay_alpha(&app), 0.0);
}

// ── a held key shows `on pressed` ───────────────────────────────────────

#[test]
fn a_held_space_shows_on_pressed_and_letting_go_clears_it() {
    let mut app = App::new(PILL);
    app.key("Tab");
    assert_eq!(overlay_alpha(&app), 0.0);
    app.key_down("Space");
    assert_eq!(app.int("n"), 1, "the press ran the tap");
    assert!(
        (overlay_alpha(&app) - 26.0 / 255.0).abs() < 0.01,
        "pressed shows for a key"
    );
    assert!(approx(app.boxed(120.0, 40.0).radii[0], 8.0));
    app.key_up("Space");
    assert_eq!(overlay_alpha(&app), 0.0);
    assert!(approx(app.boxed(120.0, 40.0).radii[0], 20.0));
}

// ── a11y_role from a value ──────────────────────────────────────────────

fn role_of(app: &App, name: &str) -> AccessRole {
    app.access()
        .into_iter()
        .find(|n| n.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no node {name:?}"))
        .role
}

#[test]
fn one_root_is_a_toggle_or_a_button_by_a_ternary() {
    let mut app = App::new(
        "View Main() { var toggle = false \
         Column #[width: 400, p: 20] { \
         Box #[a11y_role: toggle ? toggle_button : button, a11y_label: \"x\", \
               width: 40, height: 40, tap => {}] {} } }",
    );
    assert_eq!(role_of(&app, "x"), AccessRole::Button);
    app.set_bool("toggle", true);
    assert_eq!(role_of(&app, "x"), AccessRole::ToggleButton);
}

#[test]
fn a_role_can_come_from_a_let_a_parameter_or_a_string() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Pick(kind: \"checkbox\") } } \
         View Pick(kind: Str) { \
             let role = kind == \"checkbox\" ? \"checkbox\" : \"switch\" \
             Box #[a11y_role: role, a11y_label: \"a\", width: 20, height: 20, tap => {}] {} \
             Box #[a11y_role: kind, a11y_label: \"b\", width: 20, height: 20, tap => {}] {} \
             Box #[a11y_role: \"radio\", a11y_label: \"c\", width: 20, height: 20, tap => {}] {} }",
    );
    assert_eq!(role_of(&app, "a"), AccessRole::CheckBox);
    assert_eq!(role_of(&app, "b"), AccessRole::CheckBox);
    assert_eq!(role_of(&app, "c"), AccessRole::RadioButton);
}

#[test]
fn a_role_that_comes_to_text_that_is_no_role_is_an_error() {
    let errs = errors_of(
        "View Main() { var kind = \"chekbox\" Box #[a11y_role: kind, width: 20, height: 20] {} }",
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("checkbox") && e.contains("`chekbox`")),
        "the message names the text it got and the closest role: {errs:?}"
    );
}

#[test]
fn a_misspelt_role_word_is_still_reported_with_the_closest() {
    let errs = errors_of("View Main() { Box #[a11y_role: chekbox, width: 20, height: 20] {} }");
    assert!(errs.iter().any(|e| e.contains("checkbox")), "{errs:?}");
    let errs = errors_of(
        "View Main() { var t = true Box #[a11y_role: t ? chekbox : button, width: 20, height: 20] {} }",
    );
    assert!(errs.iter().any(|e| e.contains("checkbox")), "{errs:?}");
}

// ── bugs ────────────────────────────────────────────────────────────────

fn face(file: &str) -> DeclaredFont {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../byard-cli/examples/assets/fonts")
        .join(file);
    let bytes: Arc<[u8]> =
        Arc::from(std::fs::read(&path).expect("the shipped font is in the tree"));
    let resolved = byard_core::text::family_name(&bytes).expect("it parses");
    DeclaredFont {
        path: file.to_string(),
        resolved: Arc::from(resolved),
        bytes,
        icons: false,
        icons_include: Vec::new(),
    }
}

#[test]
fn typo_chosen_by_a_ternary_keeps_family_and_weight() {
    let mut theme = Theme::byard_base();
    theme.add_font("display", face("SpaceGrotesk-Variable.ttf"));
    theme.set_typo(
        "hero",
        TypoToken {
            family: Some("display".to_string()),
            weight: FontWeight::Bold,
            ..TypoToken::plain(40.0)
        },
    );
    theme.set_typo("small", TypoToken::plain(12.0));
    let app = App::with_theme(
        "View Main() { var big = true inject Theme as t \
         Column #[width: 400] { Text(\"a\") #[typo: big ? t.hero : t.small] } }",
        Some(theme),
    );
    let line = &app.frame.texts()[0];
    assert!((line.font_size - 40.0).abs() < 1e-3);
    assert_eq!(line.family.as_deref(), Some("Space Grotesk"), "the family");
    assert_eq!(line.weight, 700, "the weight");
}

const SHADOW_LIST: &str = "(y: 4, blur: 8, color: 0xFF000000)";

fn shadow_blur(app: &App) -> Vec<f32> {
    app.decorated()
        .into_iter()
        .filter(|(d, _)| d.shadow_blur > 0.0)
        .map(|(d, _)| d.shadow_blur)
        .collect()
}

#[test]
fn a_shadow_list_in_a_let_is_drawn() {
    let app = App::new(&format!(
        "View Main() {{ let l = [{SHADOW_LIST}] Column #[width: 400, p: 20] {{ \
         Box #[bg: 0xFFFFFF, width: 100, height: 40, shadow: l] {{}} }} }}"
    ));
    assert_eq!(shadow_blur(&app), vec![8.0]);
}

#[test]
fn a_shadow_chosen_by_a_ternary_is_drawn_or_not() {
    let mut app = App::new(&format!(
        "View Main() {{ var lift = true let l = [{SHADOW_LIST}] Column #[width: 400, p: 20] {{ \
         Box #[bg: 0xFFFFFF, width: 100, height: 40, shadow: lift ? l : ()] {{}} }} }}"
    ));
    assert_eq!(shadow_blur(&app), vec![8.0]);
    app.set_bool("lift", false);
    assert_eq!(shadow_blur(&app), Vec::<f32>::new());
}

#[test]
fn a_shadow_chosen_between_presets_and_tuples_is_drawn() {
    let app = App::new(&format!(
        "View Main() {{ var lift = true Column #[width: 400, p: 20] {{ \
         Box #[bg: 0xFFFFFF, width: 100, height: 40, \
               shadow: lift ? [{SHADOW_LIST}, (y: 1, blur: 2)] : \"sm\"] {{}} }} }}"
    ));
    assert_eq!(shadow_blur(&app).len(), 2);
}

#[test]
fn a_shadow_followed_by_with_is_drawn_and_moves() {
    let mut app = App::new(
        "View Main() { var on: Bool = false Column #[width: 400, p: 20] { \
         Box #[bg: 0xFFFFFF, width: 100, height: 40, \
               shadow: on ? (y: 8, blur: 16, color: 0xFF000000) \
                          : (y: 0, blur: 2, color: 0xFF000000) with anim.linear(1000)] {} } }",
    );
    assert_eq!(shadow_blur(&app), vec![2.0]);
    app.set_bool("on", true);
    app.wait(500);
    let mid = shadow_blur(&app)[0];
    assert!((6.0..12.0).contains(&mid), "mid-way, got {mid}");
    app.wait(1500);
    assert_eq!(shadow_blur(&app), vec![16.0]);
}

#[test]
fn a_shadow_that_is_not_a_shadow_is_an_error() {
    let errs = errors_of(
        "View Main() { var s = 3 Box #[bg: 0xFFFFFF, width: 10, height: 10, shadow: s] {} }",
    );
    assert!(
        errs.iter().any(|e| e.contains("AttributeTypeMismatch")),
        "{errs:?}"
    );
}

#[test]
fn a_border_without_a_background_is_drawn() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         Box #[border: 0xFF0000, border_width: 3, radius: 10, width: 100, height: 40] {} } }",
    );
    let (ring, _) = app
        .decorated()
        .into_iter()
        .find(|(d, _)| d.border_width > 0.0)
        .expect("the border is drawn");
    assert!(approx(ring.border_width, 3.0));
    assert!(ring.border_color[0] > 0.9);
    assert_eq!(ring.base.color[3], 0.0, "and nothing fills it");
    assert!(
        app.frame
            .instances()
            .iter()
            .all(|b| !(approx(b.rect[2], 100.0) && b.color[3] > 0.0)),
        "no solid fill is pushed for a box with no bg"
    );
}

#[test]
fn a_zstack_child_with_grow_or_stretch_fills_the_stack() {
    for attr in ["grow: 1", "align_self: stretch"] {
        let app = App::new(&format!(
            "View Main() {{ Column #[width: 400, p: 20] {{ \
             ZStack #[width: 150, height: 60, bg: 0xCCCCCC] {{ \
                 Box #[bg: 0x3B82F6, {attr}] {{}} }} }} }}"
        ));
        let stack = app.boxed(150.0, 60.0);
        let child = app
            .frame
            .instances()
            .iter()
            .find(|b| b.color[2] > 0.8 && b.color[0] < 0.2)
            .copied();
        let child = child.unwrap_or_else(|| panic!("{attr}: no child box"));
        assert!(
            approx(child.rect[2], 150.0) && approx(child.rect[3], 60.0),
            "{attr}: the child is {:?}, the stack {:?}",
            child.rect,
            stack.rect
        );
    }
}

#[test]
fn a_zstack_child_with_a_size_of_its_own_keeps_it() {
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         ZStack #[width: 150, height: 60, bg: 0xCCCCCC] { \
             Box #[bg: 0x3B82F6, width: 40, grow: 1] {} } } }",
    );
    let child = app.boxed(40.0, 60.0);
    assert!(approx(child.rect[2], 40.0));
}

#[test]
fn a_zstack_with_no_filling_child_centres_in_the_whole_stack() {
    // `alignment` defaults to `center`, and a stack wider than its widest child
    // centres the child in the stack, not in a column as wide as the child.
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         ZStack #[width: 300, height: 60, bg: 0xCCCCCC] { \
             Box #[bg: 0x3B82F6, width: 100, height: 20] {} } } }",
    );
    let stack = app.boxed(300.0, 60.0);
    let child = app.boxed(100.0, 20.0);
    assert!(
        approx(child.rect[0], stack.rect[0] + 100.0),
        "{:?} in {:?}",
        child.rect,
        stack.rect
    );
}

// ── a keyword chosen with `?:` ──────────────────────────────────────────

fn child_y(app: &App) -> f32 {
    app.boxed(40.0, 20.0).rect[1] - app.boxed(300.0, 60.0).rect[1]
}

#[test]
fn align_chosen_by_a_ternary_places_the_child_and_follows_the_condition() {
    let mut app = App::new(
        "View Main() { var on = true Column #[width: 400] { \
         Row #[width: 300, height: 60, align: on ? center : end, bg: 0xCCCCCC] { \
             Box #[bg: 0x3B82F6, width: 40, height: 20] {} } } }",
    );
    assert!(
        approx(child_y(&app), 20.0),
        "centred, got {}",
        child_y(&app)
    );
    app.set_bool("on", false);
    assert!(
        approx(child_y(&app), 40.0),
        "at the end, got {}",
        child_y(&app)
    );
}

#[test]
fn a_nested_ternary_and_justify_and_direction_read_the_leaf() {
    let mut app = App::new(
        "View Main() { var a = false var b = true Column #[width: 400] { \
         Row #[width: 300, height: 60, justify: a ? center : b ? end : start, bg: 0xCCCCCC] { \
             Box #[bg: 0x3B82F6, width: 40, height: 20] {} } \
         Box #[direction: a ? column : row, width: 100, height: 50, bg: 0xEEEEEE] { \
             Box #[bg: 0xFF0000, width: 10, height: 10] {} Box #[bg: 0x00FF00, width: 10, height: 10] {} } } }",
    );
    let x = |app: &App| app.boxed(40.0, 20.0).rect[0] - app.boxed(300.0, 60.0).rect[0];
    assert!(
        approx(x(&app), 260.0),
        "the nested leaf `end`, got {}",
        x(&app)
    );
    let red_green_same_row = |app: &App| {
        let find = |g: bool| {
            app.frame
                .instances()
                .iter()
                .find(|i| approx(i.rect[2], 10.0) && (i.color[1] > 0.5) == g)
                .map(|i| i.rect)
                .unwrap()
        };
        approx(find(true)[1], find(false)[1])
    };
    assert!(red_green_same_row(&app), "direction: row");
    app.set_bool("a", true);
    assert!(
        approx(x(&app), 130.0),
        "centred once `a` holds, got {}",
        x(&app)
    );
    assert!(!red_green_same_row(&app), "direction: column");
}

#[test]
fn a_paint_keyword_chosen_by_a_ternary_is_read_too() {
    // `blur_quality` is a paint attribute: the tier the backdrop is drawn at.
    let mut app = App::new(
        "View Main() { var on = true Column #[width: 400] { \
         Box #[blur: 10, blur_quality: on ? low : high, width: 100, height: 40] {} } }",
    );
    let low = byard_core::frame::BLUR_QUALITY_LOW;
    assert_eq!(app.frame.backdrops()[0].quality, low);
    app.set_bool("on", false);
    assert_ne!(app.frame.backdrops()[0].quality, low);
}

#[test]
fn a_misspelt_leaf_of_a_keyword_ternary_is_the_usual_error() {
    let errs = errors_of(
        "View Main() { var on = true Row #[align: on ? centre : end, width: 10, height: 10] {} }",
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("center") && e.contains("AttributeTypeMismatch")),
        "{errs:?}"
    );
    let errs = errors_of(
        "View Main() { var a = true var b = true \
         Row #[justify: a ? start : b ? midle : end, width: 10, height: 10] {} }",
    );
    assert!(
        errs.iter().any(|e| e.contains("AttributeTypeMismatch")),
        "{errs:?}"
    );
}

// ── `align` on Text ─────────────────────────────────────────────────────

fn text_align(src: &str) -> (byard_core::frame::TextAlign, Option<f32>) {
    let app = App::new(src);
    (app.frame.texts()[0].align, app.frame.text_wraps()[0])
}

#[test]
fn text_align_reaches_the_run_and_a_text_without_it_is_start() {
    use byard_core::frame::TextAlign::{Center, End, Justify, Start};
    let run = |attr: &str| {
        text_align(&format!(
            "View Main() {{ Column #[width: 400] {{ Text(\"one\\ntwo words\") #[width: 200{attr}] }} }}"
        ))
    };
    assert_eq!(run("").0, Start);
    assert_eq!(run(", align: start").0, Start);
    assert_eq!(run(", align: center").0, Center);
    assert_eq!(run(", align: end").0, End);
    assert_eq!(run(", align: justify").0, Justify);
    assert_eq!(
        run(", align: center").1,
        Some(200.0),
        "aligned inside the box's width"
    );
}

#[test]
fn text_align_chosen_by_a_ternary_follows_the_condition() {
    use byard_core::frame::TextAlign::{Center, End};
    let mut app = App::new(
        "View Main() { var mid = true Column #[width: 400] { \
         Text(\"one\\ntwo words\") #[width: 200, align: mid ? center : end] } }",
    );
    assert_eq!(app.frame.texts()[0].align, Center);
    app.set_bool("mid", false);
    assert_eq!(app.frame.texts()[0].align, End);
}

#[test]
fn a_wrapped_paragraph_keeps_its_wrap_width_when_aligned() {
    let (align, wrap) = text_align(
        "View Main() { Column #[width: 400] { \
         Text(\"the quick brown fox jumps over the lazy dog and keeps running\") \
             #[width: 150, align: justify] } }",
    );
    assert_eq!(align, byard_core::frame::TextAlign::Justify);
    assert_eq!(wrap, Some(150.0));
}

// ── a disabled element has no interaction states ────────────────────────

const DISABLED_PILL: &str = "View Main() { var n = 0 var off = true \
    Column #[width: 400, p: 20] { \
    Box #[bg: 0x6750A4, radius: 20, width: 120, height: 40, a11y_role: button, \
          disabled: off, tap => n += 1] { \
        on hover { overlay: 0x14FFFFFF } \
        on pressed { overlay: 0x1AFFFFFF, radius: 8 } \
        on focus_visible { outline: 0xD0BCFF, outline_width: 3, outline_offset: 2 } \
        on disabled { overlay: 0x05FFFFFF } \
    } } }";

#[test]
fn a_disabled_element_is_never_hovered_pressed_or_focused() {
    let mut app = App::new(DISABLED_PILL);
    let at = centre(&app, 120.0, 40.0);
    let resting = 5.0 / 255.0;
    assert!(
        (overlay_alpha(&app) - resting).abs() < 0.01,
        "on disabled applies"
    );

    app.hover(at);
    assert!(
        (overlay_alpha(&app) - resting).abs() < 0.01,
        "no hover tint on a disabled element"
    );
    app.press(at);
    assert!(
        (overlay_alpha(&app) - resting).abs() < 0.01,
        "no pressed tint"
    );
    assert!(
        approx(app.boxed(120.0, 40.0).radii[0], 20.0),
        "no pressed radius"
    );
    app.release(at);
    app.key("Tab");
    app.key_down("Space");
    assert!(!has_ring(&app), "Tab skips it");
    assert!(
        (overlay_alpha(&app) - resting).abs() < 0.01,
        "a held key doesn't press it"
    );
    app.key_up("Space");
    assert_eq!(app.int("n"), 0);

    // Enabled again, the same pointer over it hovers it.
    app.set_bool("off", false);
    app.hover(at);
    assert!(
        (overlay_alpha(&app) - 20.0 / 255.0).abs() < 0.01,
        "hover tint once enabled"
    );
}

#[test]
fn a_block_for_disabled_and_hover_is_an_error() {
    for state in ["hover", "pressed", "focused", "focus_visible", "dragging"] {
        let src = format!(
            "View Main() {{ Box #[width: 10, height: 10, disabled: true] {{ \
             on disabled+{state} {{ overlay: 0x14FFFFFF }} }} }}"
        );
        let errors = parse(&src).errors;
        assert_eq!(errors.len(), 1, "{state}: {}", messages(&errors));
        assert_eq!(errors[0].kind(), "DisabledStateNeverMet", "{state}");
        assert!(
            errors[0]
                .headline()
                .contains(&format!("on disabled+{state}")),
            "{}",
            errors[0].headline()
        );
    }
    // In either order.
    let errors = parse(
        "View Main() { Box #[disabled: true] { on hover+disabled { overlay: 0x14FFFFFF } } }",
    )
    .errors;
    assert_eq!(errors.len(), 1, "{}", messages(&errors));
    // `disabled` with a state a disabled element can have is fine.
    let errors = parse("View Main() { Box #[disabled: true, selected: true] { on disabled+selected { overlay: 0x14FFFFFF } } }").errors;
    assert!(errors.is_empty(), "{}", messages(&errors));
}
