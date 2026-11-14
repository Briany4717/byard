//! RFC-0065: context and optional values. `context` / `provide` / `inject`
//! (slots included), `T?` parameters with `= none` and `??`, `bind` with a
//! default, and `it.x` / `it.y` in `on measure`.

#![allow(clippy::unreadable_literal, clippy::many_single_char_names)]

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
        self.frame.set_access_wanted(true);
        self.interp.render(&self.tree, &mut self.frame, W, H);
    }

    fn event(&mut self, kind: EventKind, at: (f32, f32)) -> InputEvent {
        self.t += 10;
        InputEvent {
            kind,
            pos: at,
            delta: (0.0, 0.0),
            payload: None::<InputPayload>,
            time_ms: self.t,
            mods: KeyModifiers::default(),
        }
    }

    fn tap(&mut self, at: (f32, f32)) {
        let down = self.event(EventKind::PointerDown, at);
        self.send(&[down]);
        let up = self.event(EventKind::PointerUp, at);
        self.send(&[up]);
    }

    fn var(&self, name: &str) -> Value {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .unwrap_or_else(|| panic!("no var {name}"));
        self.interp.peek(sig)
    }

    fn int(&self, name: &str) -> i64 {
        match self.var(name) {
            Value::Int(n) => n,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn float(&self, name: &str) -> f64 {
        match self.var(name) {
            Value::Float(f) => f,
            Value::Int(n) => f64::from(i32::try_from(n).unwrap()),
            other => panic!("{name} is {other:?}"),
        }
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, value);
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
}

fn texts(app: &App) -> Vec<String> {
    app.frame.texts().iter().map(|t| t.text.clone()).collect()
}

/// Every diagnostic `byard check` would give: the checker's, then the
/// lowering of every branch of the first view.
fn checked(src: &str) -> Vec<String> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut out: Vec<String> = byard_compiler::infer::check_views(&parsed.views)
        .errors
        .iter()
        .map(|e| format!("{e:?}"))
        .collect();
    let mut interp = Interpreter::new();
    interp.set_checking(true);
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, W, H);
    interp.lower_every_branch();
    out.extend(interp.errors().iter().map(|e| format!("{e:?}")));
    out
}

/// Whether some diagnostic in `errors` is of kind `kind`.
fn has(errors: &[String], kind: &str) -> bool {
    errors.iter().any(|e| e.starts_with(kind))
}

// ── 3. position in `on measure` ─────────────────────────────────────────

#[test]
fn on_measure_gives_the_position_in_the_parent() {
    let app = App::new(
        "View Main() { \
         var x: Float = -1.0 \
         var y: Float = -1.0 \
         Column #[p: 10] { \
           Box #[width: 30, height: 20] \
           Box #[width: 50, height: 20, ml: 7] { on measure => { x = it.x \n y = it.y } } } }",
    );
    // 10 of padding in the parent, then 7 of margin; below the first box and
    // the padding.
    assert!((app.float("x") - 17.0).abs() < 0.5, "x = {}", app.float("x"));
    assert!((app.float("y") - 30.0).abs() < 0.5, "y = {}", app.float("y"));
}

#[test]
fn a_sliding_indicator_follows_an_item_that_only_moves() {
    // The first item grows; the second keeps its size and changes position
    // alone, which must fire its `on measure`.
    let mut app = App::new(
        "View Main() { \
         var pad: Int = 40 \
         var ix: Float = 0.0 \
         var iw: Float = 0.0 \
         Column { \
           Row { \
             Box #[width: pad, height: 20] \
             Box #[width: 60, height: 20] { on measure => { ix = it.x \n iw = it.w } } } \
           Box #[width: iw, height: 3, translate.x: ix, bg: 0xFF0000] } }",
    );
    assert!((app.float("ix") - 40.0).abs() < 0.5);
    assert!((app.float("iw") - 60.0).abs() < 0.5);
    app.set("pad", Value::Int(100));
    assert!((app.float("ix") - 100.0).abs() < 0.5, "ix = {}", app.float("ix"));
}
