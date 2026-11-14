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
    assert!(
        (app.float("x") - 17.0).abs() < 0.5,
        "x = {}",
        app.float("x")
    );
    assert!(
        (app.float("y") - 30.0).abs() < 0.5,
        "y = {}",
        app.float("y")
    );
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
    assert!(
        (app.float("ix") - 100.0).abs() < 0.5,
        "ix = {}",
        app.float("ix")
    );
}

// ── 1. context ──────────────────────────────────────────────────────────

const TABS: &str = r#"
context TabsStyle {
    variant: Str = "primary"
    current: Int = 0
}

View Main() {
    var variant = "primary"
    var current = 1
    Tabs(variant: variant, bind: current) {
        Tab("a", 0)
        Tab("b", 1)
    }
}

View Tabs(bind current: Int, variant: Str, content) {
    provide TabsStyle(variant: variant, current: current)
    Row { content }
}

View Tab(label: Str, index: Int) {
    inject TabsStyle as s
    Text("{label}:{s.variant}:{index == s.current}")
}
"#;

#[test]
fn a_tab_drawn_through_a_slot_reads_the_containers_context_and_follows_it() {
    let mut app = App::new(TABS);
    assert_eq!(texts(&app), ["a:primary:false", "b:primary:true"]);
    app.set("variant", Value::Str("secondary".into()));
    app.set("current", Value::Int(0));
    assert_eq!(texts(&app), ["a:secondary:true", "b:secondary:false"]);
}

#[test]
fn the_nearest_provider_wins() {
    let app = App::new(
        r#"
context Tone { name: Str = "none" }
View Main() { Outer() }
View Outer() {
    provide Tone(name: "outer")
    Column { Probe() Inner() }
}
View Inner() {
    provide Tone(name: "inner")
    Column { Probe() }
}
View Probe() { inject Tone as t
    Text(t.name) }
"#,
    );
    assert_eq!(texts(&app), ["outer", "inner"]);
}

#[test]
fn with_no_provider_a_view_reads_the_defaults() {
    let app = App::new(
        r#"
context TabsStyle { variant: Str = "primary"
    current: Int = 3
    place: Fn(Int) = none }
View Main() { Tab() }
View Tab() { inject TabsStyle as s
    on measure => s.place(1)
    Text("{s.variant}{s.current}") }
"#,
    );
    assert_eq!(texts(&app), ["primary3"]);
}

#[test]
fn a_context_callback_reports_to_the_provider() {
    let app = App::new(
        r"
context Report { hit: Fn(Int) = none }
View Main() { var seen = 0
    Host(seen: seen) }
View Host(bind seen: Int) {
    provide Report(hit: {|n| seen = n })
    Item()
}
View Item() { inject Report as r
    Box #[width: 10, height: 10] { on measure => r.hit(7) } }
",
    );
    assert_eq!(app.int("seen"), 7);
}

#[test]
fn a_view_without_context_is_untouched_by_a_provider_elsewhere() {
    let app = App::new(
        "context C { n: Int = 1 }\n\
         View Main() { Column { Plain() } }\n\
         View Plain() { Text(\"x\") }",
    );
    assert_eq!(texts(&app), ["x"]);
}

#[test]
fn context_diagnostics() {
    let base = "context C { n: Int = 1\n s: Str = \"a\" }\n";
    let e = checked(&format!(
        "{base}View Main() {{ provide C(n: 1)\n provide C(n: 2)\n Text(\"x\") }}"
    ));
    assert!(has(&e, "DuplicateProvide"), "{e:?}");
    let e = checked(&format!(
        "{base}View Main() {{ provide D(n: 1)\n Text(\"x\") }}"
    ));
    assert!(has(&e, "UnknownContext"), "{e:?}");
    let e = checked(&format!(
        "{base}View Main() {{ inject Cc as c\n Text(\"x\") }}"
    ));
    assert!(has(&e, "UnknownContext"), "{e:?}");
    let e = checked(&format!(
        "{base}View Main() {{ provide C(nn: 1)\n Text(\"x\") }}"
    ));
    assert!(has(&e, "UnknownField"), "{e:?}");
    let e = checked("context C { n: Int }\nView Main() { Text(\"x\") }");
    assert!(has(&e, "ContextFieldNeedsDefault"), "{e:?}");
    let e = checked(&format!(
        "{base}View Main() {{ when true {{ provide C(n: 1) }}\n Text(\"x\") }}"
    ));
    assert!(has(&e, "MisplacedProvide"), "{e:?}");
    let e = checked(&format!(
        "{base}View Main() {{ provide C(n: \"x\")\n Text(\"x\") }}"
    ));
    assert!(has(&e, "ArgumentTypeMismatch"), "{e:?}");
    let ok = checked(&format!(
        "{base}View Main() {{ provide C(n: 2)\n Text(\"x\") }}"
    ));
    assert!(ok.is_empty(), "{ok:?}");
}

// ── 2. optional values and bind defaults ────────────────────────────────

const ICON: &str = r"
View Main() { Column { Chip(1) Chip(2, color: 0x00FF00) Chip(3, color: none) } }
View Chip(n: Int, color: Color? = none) {
    Box #[width: 10 + n, height: 10, bg: color ?? 0xFF0000]
}
";

#[test]
fn an_optional_colour_is_the_argument_or_the_fallback() {
    let app = App::new(ICON);
    let r = app.boxed(11.0, 10.0).color;
    let g = app.boxed(12.0, 10.0).color;
    let n = app.boxed(13.0, 10.0).color;
    assert!(r[0] > 0.9 && r[1] < 0.1, "{r:?}");
    assert!(g[1] > 0.9 && g[0] < 0.1, "{g:?}");
    assert!(n[0] > 0.9 && n[1] < 0.1, "{n:?}");
}

#[test]
fn an_optional_is_known_inside_a_not_none_branch() {
    let app = App::new(
        r"
View Main() { Column { Chip(1) Chip(2, color: 0x00FF00) } }
View Chip(n: Int, color: Color? = none) {
    when color != none { Box #[width: 10 + n, height: 10, bg: color] }
    else { Box #[width: 10 + n, height: 20, bg: 0x333333] }
}
",
    );
    assert!(app.boxed(11.0, 20.0).color[3] > 0.0);
    assert!(app.boxed(12.0, 10.0).color[1] > 0.9);
}

#[test]
fn reading_an_optional_bare_is_none_not_handled() {
    let e = checked(
        "View Main() { Chip(1) }\n\
         View Chip(n: Int, color: Color? = none) { Box #[width: 10, height: 10, bg: color] }",
    );
    assert!(has(&e, "NoneNotHandled"), "{e:?}");
    let ok = checked(
        "View Main() { Chip(1) }\n\
         View Chip(n: Int, color: Color? = none) { Box #[width: 10, height: 10, bg: color != none ? color : 0x112233] }",
    );
    assert!(ok.is_empty(), "{ok:?}");
    let e = checked(
        "View Main() { Chip(1) }\n\
         View Chip(n: Int, color: Color? = 5) { Text(\"x\") }",
    );
    assert!(has(&e, "BadOptionalDefault"), "{e:?}");
}

const GROUP: &str = r#"
View Main() { var page = 5
    Column { Group() Group(bind: page) Text("{page}") } }
View Group(bind sel: Int = 0) {
    Box #[width: 10, height: 10, bg: 0x333333, tap => sel = sel + 1]
    Text("s{sel}")
}
"#;

#[test]
fn an_unbound_bind_with_a_default_keeps_its_own_var() {
    let mut app = App::new(GROUP);
    let t = texts(&app);
    assert_eq!(t, ["s0", "s5", "5"], "{t:?}");
    let at = {
        let b = app.boxed(10.0, 10.0);
        (b.rect[0] + 5.0, b.rect[1] + 5.0)
    };
    app.tap(at);
    assert_eq!(texts(&app), ["s1", "s5", "5"]);
}

#[test]
fn a_bind_with_a_default_is_no_longer_a_bad_binding_and_without_one_is_required() {
    let ok = checked("View Main() { Group() }\nView Group(bind sel: Int = 0) { Text(\"x\") }");
    assert!(ok.is_empty(), "{ok:?}");
    let e = checked("View Main() { Group() }\nView Group(bind sel: Int) { Text(\"x\") }");
    assert!(has(&e, "MissingParam"), "{e:?}");
}
