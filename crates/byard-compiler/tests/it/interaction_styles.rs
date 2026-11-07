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
fn a_zstack_with_no_filling_child_lays_out_as_it_always_did() {
    // The stack's column is as wide as its widest child, packed at the start:
    // a centred child is centred in that column, not in the whole stack.
    let app = App::new(
        "View Main() { Column #[width: 400, p: 20] { \
         ZStack #[width: 300, height: 60, bg: 0xCCCCCC] { \
             Box #[bg: 0x3B82F6, width: 100, height: 20] {} } } }",
    );
    let stack = app.boxed(300.0, 60.0);
    let child = app.boxed(100.0, 20.0);
    assert!(
        approx(child.rect[0], stack.rect[0]),
        "{:?} in {:?}",
        child.rect,
        stack.rect
    );
}
