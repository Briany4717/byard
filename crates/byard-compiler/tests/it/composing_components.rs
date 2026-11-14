//! RFC-0062: composing components. A slot anywhere in a body, several slots,
//! optional callbacks, layout attributes on a view call, and states that reach
//! other elements (`as field` ... `on field.focused`, `focus_within`).

#![allow(clippy::unreadable_literal, clippy::many_single_char_names)]

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::resolve::{PackageProvider, SourceFile, resolve_program};
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
}

fn texts(app: &App) -> Vec<String> {
    app.frame.texts().iter().map(|t| t.text.clone()).collect()
}

/// Whether a solid box of `w` by `h` is drawn in (nearly) this sRGB colour.
fn has_box(app: &App, w: f32, h: f32, rgb: u32) -> bool {
    let want = |byte: u32| -> f32 {
        let c = f32::from(u8::try_from(byte).unwrap()) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (want(rgb >> 16 & 255), want(rgb >> 8 & 255), want(rgb & 255));
    app.frame.instances().iter().any(|i| {
        (i.rect[2] - w).abs() < 0.5
            && (i.rect[3] - h).abs() < 0.5
            && (i.color[0] - r).abs() < 0.02
            && (i.color[1] - g).abs() < 0.02
            && (i.color[2] - b).abs() < 0.02
    })
}

/// The tap centre of the first box of `w` by `h`.
fn centre(app: &App, w: f32, h: f32) -> (f32, f32) {
    let b = app.boxed(w, h);
    (b.rect[0] + b.rect[2] / 2.0, b.rect[1] + b.rect[3] / 2.0)
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

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

// ── 1. a slot anywhere in the body ──────────────────────────────────────

const SLOT_IN_WHEN: &str = "View Main() { \
    var on = true \
    Column { Item(on: on) { Text(\"inside\") } } } \
    View Item(on: Bool, content) { \
    when on { Row #[tap => {}, bg: 0xFF0000, width: 50, height: 20] { content } } \
    else { Row #[bg: 0x0000FF, width: 60, height: 20] { content } } }";

#[test]
fn a_slot_is_placed_in_both_branches_of_a_when() {
    let mut app = App::new(SLOT_IN_WHEN);
    assert_eq!(texts(&app), vec!["inside"]);
    assert!(has_box(&app, 50.0, 20.0, 0xFF0000));
    app.set_bool("on", false);
    assert_eq!(texts(&app), vec!["inside"], "the else branch has it too");
    assert!(has_box(&app, 60.0, 20.0, 0x0000FF));
    app.set_bool("on", true);
    assert_eq!(texts(&app), vec!["inside"]);
}

#[test]
fn a_slot_is_placed_inside_a_for_and_a_nested_when() {
    let app = App::new(
        "View Main() { \
         var rows = [1, 2, 3] \
         Column { Rows(rows: rows) { Text(\"x\") } } } \
         View Rows(rows: List<Int>, content) { \
         Column { for r in rows { Row { when r > 1 { content } else { Text(\"one\") } } } } }",
    );
    assert_eq!(texts(&app), vec!["one", "x", "x"]);
}

#[test]
fn a_slot_reads_the_callers_names_wherever_it_is_placed() {
    let mut app = App::new(
        "View Main() { var n = 1 Column { Item(on: true) { Text(\"n={n}\") } } } \
         View Item(on: Bool, content) { when on { Row { content } } else { content } }",
    );
    assert_eq!(texts(&app), vec!["n=1"]);
    let sig = app.interp.var_signal(&Symbol::intern("n")).unwrap();
    app.interp.write_var(sig, Value::Int(2));
    app.interp.tick();
    app.send(&[]);
    assert_eq!(texts(&app), vec!["n=2"]);
}

// ── 2. named slots ──────────────────────────────────────────────────────

const LIST_ITEM: &str = "View Main() { Column { \
    ListItem(\"Wi-Fi\", leading: { Text(\"L\") }, trailing: { Text(\"T1\") Text(\"T2\") }) \
    ListItem(\"About\") \
    ListItem(\"Only trailing\", trailing: { Text(\"T3\") }) } } \
    View ListItem(headline: Str, leading: Slot = none, trailing: Slot = none) { \
    Row { when leading != none { leading } Text(headline) \
    when trailing != none { Row { trailing } } } }";

#[test]
fn a_view_takes_several_slots_and_places_them_by_name() {
    let app = App::new(LIST_ITEM);
    assert_eq!(
        texts(&app),
        vec!["L", "Wi-Fi", "T1", "T2", "About", "Only trailing", "T3"]
    );
}

#[test]
fn a_slot_that_was_not_given_is_none_and_places_nothing() {
    let app = App::new(
        "View Main() { Column { Box { Text(\"a\") } Pair(first: { Text(\"1\") }) } } \
         View Pair(first: Slot = none, second: Slot = none) { Row { first second \
         when second == none { Text(\"no second\") } } }",
    );
    assert_eq!(texts(&app), vec!["a", "1", "no second"]);
}

#[test]
fn the_trailing_block_and_named_slots_live_together() {
    let app = App::new(
        "View Main() { Panel(head: { Text(\"H\") }) { Text(\"body\") } } \
         View Panel(head: Slot, content) { Column { head content } }",
    );
    assert_eq!(texts(&app), vec!["H", "body"]);
}

#[test]
fn content_can_be_tested_for_too() {
    let app = App::new(
        "View Main() { Column { Bar() Bar() { Text(\"c\") } } } \
         View Bar(content) { when content != none { Row { content } } else { Text(\"empty\") } }",
    );
    assert_eq!(texts(&app), vec!["empty", "c"]);
}

#[test]
fn a_required_slot_that_is_missing_is_a_missing_param() {
    let errors = checked("View Main() { Panel() } View Panel(head: Slot) { Column { head } }");
    assert!(
        errors.iter().any(|e| e.contains("MissingParam")),
        "{errors:?}"
    );
}

#[test]
fn a_value_for_a_slot_and_a_block_for_a_value_are_told() {
    let errors =
        checked("View Main() { Panel(head: 3) } View Panel(head: Slot = none) { Column { head } }");
    assert!(
        errors.iter().any(|e| e.contains("SlotArgument")),
        "{errors:?}"
    );
    let errors = checked(
        "View Main() { Panel(title: { Text(\"x\") }) } View Panel(title: Str) { Text(title) }",
    );
    assert!(
        errors.iter().any(|e| e.contains("SlotArgument")),
        "{errors:?}"
    );
}

#[test]
fn a_slot_is_forwarded_to_another_view() {
    let app = App::new(
        "View Main() { Outer(icon: { Text(\"i\") }) } \
         View Outer(icon: Slot = none) { Inner(icon: icon) } \
         View Inner(icon: Slot = none) { Row { icon Text(\"t\") } }",
    );
    assert_eq!(texts(&app), vec!["i", "t"]);
}

// ── 3. optional callbacks ───────────────────────────────────────────────

const CARD: &str = "View Main() { var n = 0 Column { \
    Card(on_tap: { n++ }) { Text(\"clickable\") } \
    Card() { Text(\"plain\") } } } \
    View Card(on_tap: Fn() = none, content) { \
    when on_tap != none { Box #[width: 80, height: 30, bg: 0xFF0000, tap => on_tap()] { content } } \
    else { Box #[width: 90, height: 30, bg: 0x00FF00] { content } } }";

#[test]
fn a_card_is_clickable_exactly_when_it_was_given_on_tap() {
    let mut app = App::new(CARD);
    assert!(has_box(&app, 80.0, 30.0, 0xFF0000));
    assert!(has_box(&app, 90.0, 30.0, 0x00FF00));
    let at = centre(&app, 80.0, 30.0);
    app.press(at);
    app.release(at);
    assert_eq!(app.int("n"), 1);
    let at = centre(&app, 90.0, 30.0);
    app.press(at);
    app.release(at);
    assert_eq!(app.int("n"), 1, "the plain card does nothing");
}

#[test]
fn calling_a_callback_that_was_not_given_is_a_no_op() {
    let mut app = App::new(
        "View Main() { Box #[width: 30, height: 30] { Btn() } } \
         View Btn(on_tap: Fn() = none) { Box #[width: 20, height: 20, bg: 0xFF0000, tap => on_tap()] {} }",
    );
    let at = centre(&app, 20.0, 20.0);
    app.press(at);
    app.release(at);
    assert!(app.interp.errors().is_empty(), "{:?}", app.interp.errors());
}

#[test]
fn a_callback_is_forwarded_given_or_not() {
    let mut app = App::new(
        "View Main() { var n = 0 Column { Outer(on_tap: { n++ }) Outer() } } \
         View Outer(on_tap: Fn() = none) { Inner(on_tap: on_tap) } \
         View Inner(on_tap: Fn() = none) { \
         when on_tap != none { Box #[width: 20, height: 20, bg: 0xFF0000, tap => on_tap()] {} } \
         else { Box #[width: 30, height: 30, bg: 0x00FF00] {} } }",
    );
    assert!(has_box(&app, 20.0, 20.0, 0xFF0000));
    assert!(has_box(&app, 30.0, 30.0, 0x00FF00));
    let at = centre(&app, 20.0, 20.0);
    app.press(at);
    app.release(at);
    assert_eq!(app.int("n"), 1);
}

#[test]
fn none_is_compared_only_with_a_parameter_that_declared_it() {
    // A required callback.
    let errors = checked(
        "View Main() { Card(on_tap: {}) } \
         View Card(on_tap: Fn()) { when on_tap != none { Text(\"a\") } }",
    );
    assert!(
        errors.iter().any(|e| e.contains("NoneNeedsOptional")),
        "{errors:?}"
    );
    // A var, and a callback with a default of its own.
    let errors = checked("View Main() { var n = 0 when n == none { Text(\"a\") } }");
    assert!(
        errors.iter().any(|e| e.contains("NoneNeedsOptional")),
        "{errors:?}"
    );
    let errors = checked(
        "View Main() { Card() } \
         View Card(on_tap: Fn() = {}) { when on_tap != none { Text(\"a\") } }",
    );
    assert!(
        errors.iter().any(|e| e.contains("NoneNeedsOptional")),
        "{errors:?}"
    );
    // The declared ones are fine.
    let errors = checked(
        "View Main() { Card() } \
         View Card(on_tap: Fn() = none, head: Slot = none, content) { \
         when on_tap != none { Text(\"a\") } \
         when head != none { head } \
         when content == none { Text(\"b\") } }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

// ── 4. layout attributes on a view call ─────────────────────────────────

const ICONS: &str = "View Main() { Row #[width: 400, height: 100, align: start] { \
    Dot() Dot() #[ml: 30, grow: 1, align_self: end] Dot() #[width: 50] } } \
    View Dot() { Box #[width: 10, height: 10, bg: 0xFF0000] {} }";

#[test]
fn layout_attributes_on_a_view_call_apply_to_its_root() {
    let app = App::new(ICONS);
    let dots: Vec<[f32; 4]> = app.frame.instances().iter().map(|b| b.rect).collect();
    assert_eq!(dots.len(), 3, "{dots:?}");
    // The first is untouched: at the left edge, 10 wide.
    assert!(
        approx(dots[0][0], 0.0) && approx(dots[0][2], 10.0),
        "{dots:?}"
    );
    // The second has a 30 margin, grows, and sits at the cross-axis end.
    assert!(
        approx(dots[1][0], 40.0),
        "margin-left 30 after the first: {dots:?}"
    );
    assert!(dots[1][2] > 100.0, "it grew: {dots:?}");
    assert!(
        approx(dots[1][1] + dots[1][3], 100.0),
        "aligned to the end: {dots:?}"
    );
    // The third is given a width the view did not set.
    assert!(approx(dots[2][2], 50.0), "{dots:?}");
}

#[test]
fn a_layout_attribute_may_be_a_variable_and_a_parameter_of_that_name_wins() {
    let mut app = App::new(
        "View Main() { var gap = 10 Row #[width: 400, height: 100] { \
         Dot() Dot() #[ml: gap] Sized() #[width: 70] } } \
         View Dot() { Box #[width: 10, height: 10, bg: 0xFF0000] {} } \
         View Sized(width: Int = 5) { Box #[width: width, height: 10, bg: 0x00FF00] {} }",
    );
    let xs = |app: &App| -> Vec<f32> { app.frame.instances().iter().map(|b| b.rect[0]).collect() };
    assert!(approx(xs(&app)[1], 20.0), "{:?}", xs(&app));
    let sig = app.interp.var_signal(&Symbol::intern("gap")).unwrap();
    app.interp.write_var(sig, Value::Int(40));
    app.interp.tick();
    app.send(&[]);
    assert!(approx(xs(&app)[1], 50.0), "{:?}", xs(&app));
    assert!(
        app.boxed(70.0, 10.0).rect[2] > 0.0,
        "width went to the parameter"
    );
}

#[test]
fn size_bounds_on_a_view_call_apply_to_its_root() {
    let app = App::new(
        "View Main() { Row #[width: 400, height: 100, align: start] { \
         Wide() #[max_width: 60] Narrow() #[min_width: 40, min_height: 30] } } \
         View Wide() { Box #[width: 200, height: 10, bg: 0xFF0000] {} } \
         View Narrow() { Box #[width: 10, height: 10, bg: 0x00FF00] {} }",
    );
    let boxes: Vec<[f32; 4]> = app.frame.instances().iter().map(|b| b.rect).collect();
    assert_eq!(boxes.len(), 2, "{boxes:?}");
    assert!(approx(boxes[0][2], 60.0), "max_width caps 200: {boxes:?}");
    assert!(
        approx(boxes[1][2], 40.0) && approx(boxes[1][3], 30.0),
        "min_width and min_height raise 10: {boxes:?}"
    );
}

#[test]
fn layout_attributes_reach_every_root_of_a_when() {
    let mut app = App::new(
        "View Main() { var on = true Row #[width: 400, height: 100] { Pick(on: on) #[ml: 20] } } \
         View Pick(on: Bool) { when on { Box #[width: 10, height: 10, bg: 0xFF0000] {} } \
         else { Box #[width: 12, height: 12, bg: 0x00FF00] {} } }",
    );
    assert!(approx(app.boxed(10.0, 10.0).rect[0], 20.0));
    app.set_bool("on", false);
    assert!(approx(app.boxed(12.0, 12.0).rect[0], 20.0));
}

#[test]
fn any_other_attribute_on_a_view_call_names_the_allowed_ones() {
    let errors = checked(
        "View Main() { Dot() #[bg: 0xFF0000] } View Dot() { Box #[width: 10, height: 10] {} }",
    );
    let e = errors
        .iter()
        .find(|e| e.contains("UnknownParam"))
        .unwrap_or_else(|| panic!("{errors:?}"));
    let parsed = parse("View Main() { Dot() #[bg: 0xFF0000] } View Dot() { Box {} }");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known = ["Main", "Dot"];
    let _ = interp.lower_view(&parsed.views[0], &known);
    let text = interp
        .errors()
        .iter()
        .map(byard_compiler::CompileError::headline)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("ml") && text.contains("align_self") && text.contains("grow"),
        "{text}"
    );
    let _ = e;
}

// ── 5. states that reach other elements ─────────────────────────────────

const FIELD: &str = "View Main() { Column #[width: 400, p: 20, gap: 10] { \
    Box #[width: 100, height: 40, bg: 0x888888, a11y_role: button, tap => {}] as field {} \
    Box #[width: 60, height: 20, bg: 0x00FF00] { on field.focused { bg: 0xFF0000 } } } }";

#[test]
fn a_later_sibling_follows_the_focus_of_a_named_element() {
    let mut app = App::new(FIELD);
    assert!(has_box(&app, 60.0, 20.0, 0x00FF00));
    app.key("Tab");
    assert!(has_box(&app, 60.0, 20.0, 0xFF0000), "focused");
    assert!(!has_box(&app, 60.0, 20.0, 0x00FF00));
}

#[test]
fn a_named_elements_hover_and_press_reach_its_siblings_and_descendants() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20, gap: 10] { \
         Box #[width: 100, height: 40, bg: 0x888888, tap => {}] as track { \
         Box #[width: 8, height: 8, bg: 0x00FF00] { on track.pressed { bg: 0x0000FF } } } \
         Box #[width: 60, height: 20, bg: 0x00FF00] { on track.hover { bg: 0xFF0000 } } } }",
    );
    let at = centre(&app, 100.0, 40.0);
    assert!(has_box(&app, 8.0, 8.0, 0x00FF00));
    app.hover(at);
    assert!(
        has_box(&app, 60.0, 20.0, 0xFF0000),
        "hover reaches the sibling"
    );
    assert!(has_box(&app, 8.0, 8.0, 0x00FF00), "not pressed yet");
    app.press(at);
    assert!(has_box(&app, 8.0, 8.0, 0x0000FF), "press reaches the child");
    app.release(at);
    app.hover((390.0, 390.0));
    assert!(has_box(&app, 60.0, 20.0, 0x00FF00), "the pointer left");
}

#[test]
fn remote_states_combine_with_each_other_and_with_the_elements_own() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20, gap: 10] { \
         Box #[width: 100, height: 40, bg: 0x888888, a11y_role: button, tap => {}] as field {} \
         Box #[width: 60, height: 20, bg: 0x00FF00] { \
         on field.focused + field.hover { bg: 0x0000FF } \
         on field.focused { bg: 0xFF0000 } } \
         Box #[width: 70, height: 20, bg: 0x00FF00] { on field.focused + hover { bg: 0xFF0000 } } } }",
    );
    app.key("Tab");
    assert!(has_box(&app, 60.0, 20.0, 0xFF0000), "focused only");
    assert!(
        has_box(&app, 70.0, 20.0, 0x00FF00),
        "not hovering the 70 box"
    );
    app.hover(centre(&app, 100.0, 40.0));
    assert!(has_box(&app, 60.0, 20.0, 0x0000FF), "two states beat one");
    app.hover(centre(&app, 70.0, 20.0));
    assert!(
        has_box(&app, 70.0, 20.0, 0xFF0000),
        "its own hover, the field focused"
    );
}

#[test]
fn two_instances_of_a_view_keep_their_own_named_states() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20, gap: 10] { F(w: 61) F(w: 62) } } \
         View F(w: Int) { Row { \
         Box #[width: 100, height: 40, bg: 0x888888, a11y_role: button, tap => {}] as field {} \
         Box #[width: w, height: 20, bg: 0x00FF00] { on field.focused { bg: 0xFF0000 } } } }",
    );
    app.key("Tab");
    assert!(has_box(&app, 61.0, 20.0, 0xFF0000));
    assert!(
        has_box(&app, 62.0, 20.0, 0x00FF00),
        "the other instance is not focused"
    );
    app.key("Tab");
    assert!(has_box(&app, 61.0, 20.0, 0x00FF00));
    assert!(has_box(&app, 62.0, 20.0, 0xFF0000));
}

#[test]
fn a_name_declared_outside_a_when_or_for_reaches_into_it() {
    let mut app = App::new(
        "View Main() { var show = true Column #[width: 400, p: 20, gap: 10] { \
         Box #[width: 100, height: 40, bg: 0x888888, a11y_role: button, tap => {}] as field {} \
         when show { Box #[width: 60, height: 20, bg: 0x00FF00] { on field.focused { bg: 0xFF0000 } } } \
         for i in [1] { Box #[width: 61, height: 20, bg: 0x00FF00] { on field.focused { bg: 0xFF0000 } } } } }",
    );
    app.key("Tab");
    assert!(has_box(&app, 60.0, 20.0, 0xFF0000));
    assert!(has_box(&app, 61.0, 20.0, 0xFF0000));
}

#[test]
fn an_unknown_or_later_state_target_is_told() {
    let errors = checked(
        "View Main() { Column { Box #[width: 10, height: 10] as field {} \
         Box { on feild.hover { bg: 0xFF0000 } } } }",
    );
    let e = errors
        .iter()
        .find(|e| e.contains("UnknownStateTarget"))
        .unwrap_or_else(|| panic!("{errors:?}"));
    assert!(e.contains("field"), "the closest name is offered: {e}");
    let parsed = parse(
        "View Main() { Column { Box #[width: 10, height: 10] as field {} \
         Box { on field.hovr { bg: 0xFF0000 } } } }",
    );
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| format!("{e:?}").contains("UnknownStyleState")),
        "{:?}",
        parsed.errors
    );
}

#[test]
fn focus_within_holds_for_an_element_with_focus_inside_it() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20, gap: 10] { \
         Row #[width: 200, height: 50, bg: 0x00FF00] { \
         on focus_within { bg: 0xFF0000 } \
         Box #[width: 40, height: 40, bg: 0x888888, a11y_role: button, tap => {}] {} } \
         Box #[width: 40, height: 40, bg: 0x888888, a11y_role: button, tap => {}] {} } }",
    );
    assert!(has_box(&app, 200.0, 50.0, 0x00FF00));
    app.key("Tab");
    assert!(has_box(&app, 200.0, 50.0, 0xFF0000), "the child has focus");
    app.key("Tab");
    assert!(has_box(&app, 200.0, 50.0, 0x00FF00), "focus moved out");
}

#[test]
fn focus_within_reaches_other_elements_through_a_name() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20, gap: 10] { \
         Row #[width: 200, height: 50] as group { \
         Box #[width: 40, height: 40, bg: 0x888888, a11y_role: button, tap => {}] {} } \
         Box #[width: 60, height: 20, bg: 0x00FF00] { on group.focus_within { bg: 0xFF0000 } } } }",
    );
    assert!(has_box(&app, 60.0, 20.0, 0x00FF00));
    app.key("Tab");
    assert!(has_box(&app, 60.0, 20.0, 0xFF0000));
}

// ── a slot placed in a `for` is its own element in every row ────────────

const ROWS: &str = "View Main() { Column #[width: 400, p: 20, gap: 10] { \
    Rows(rows: [1, 2, 3]) { \
    Box #[width: 100, height: 40, bg: 0x888888, a11y_role: button, tap => {}] { \
    on focused { bg: 0xFF0000 } \
    on hover { bg: 0x0000FF with anim.linear(50) } } } } } \
    View Rows(rows: List<Int>, content) { Column { for r in rows { Row { content } } } }";

fn grey_boxes(app: &App) -> usize {
    app.frame
        .instances()
        .iter()
        .filter(|b| (b.rect[2] - 100.0).abs() < 0.5 && (b.rect[3] - 40.0).abs() < 0.5)
        .count()
}

fn boxes_of(app: &App, rgb: u32) -> usize {
    let lin = |byte: u32| {
        let c = f32::from(u8::try_from(byte).unwrap()) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let want = [lin(rgb >> 16 & 255), lin(rgb >> 8 & 255), lin(rgb & 255)];
    app.frame
        .instances()
        .iter()
        .filter(|b| {
            (b.rect[2] - 100.0).abs() < 0.5 && (0..3).all(|i| (b.color[i] - want[i]).abs() < 0.02)
        })
        .count()
}

#[test]
fn a_slot_in_a_for_has_its_own_focus_in_every_row() {
    let mut app = App::new(ROWS);
    assert_eq!(grey_boxes(&app), 3);
    app.key("Tab");
    app.t += 200;
    app.send(&[]);
    assert_eq!(boxes_of(&app, 0xFF0000), 1, "only the focused row");
    app.key("Tab");
    app.t += 200;
    app.send(&[]);
    assert_eq!(boxes_of(&app, 0xFF0000), 1, "focus moved to the next row");
}

#[test]
fn a_slot_in_a_for_animates_each_row_alone() {
    let mut app = App::new(ROWS);
    let rows: Vec<(f32, f32)> = app
        .frame
        .instances()
        .iter()
        .filter(|b| (b.rect[2] - 100.0).abs() < 0.5)
        .map(|b| (b.rect[0] + 50.0, b.rect[1] + 20.0))
        .collect();
    app.hover(rows[1]);
    app.t += 200;
    app.send(&[]);
    assert_eq!(boxes_of(&app, 0x0000FF), 1, "only the hovered row");
    assert_eq!(boxes_of(&app, 0x888888), 2);
}

/// The identities of the boxes directly under the first box of the tree.
fn child_uids(app: &App) -> Vec<u32> {
    let RenderNode::Box { children, .. } = &app.tree[0] else {
        panic!("expected a box, got {:?}", app.tree[0]);
    };
    children
        .iter()
        .filter_map(|c| match c {
            RenderNode::Box { uid, .. } => Some(*uid),
            _ => None,
        })
        .collect()
}

#[test]
fn a_slot_placed_twice_is_two_elements_with_identities_of_their_own() {
    let app = App::new(
        "View Main() { Twice() { Box #[width: 10, height: 10, a11y_role: button, tap => {}] {} } } \
         View Twice(content) { Column { content content } }",
    );
    let uids = child_uids(&app);
    assert_eq!(uids.len(), 2, "{uids:?}");
    assert_ne!(uids[0], uids[1], "one identity shared by two elements");
}

// ── a `{ … }` argument is actions or elements by the parameter's type ───

const BLOCKS: &str = "View Main() { var Count = 0 let Bump = () => { Count += 1 } Column { \
    Pad(on_tap: { Bump() }, leading: { Text(\"slot\") }) \
    Pad(on_tap: { Count += 1 }) } } \
    View Pad(on_tap: Fn() = none, leading: Slot = none) { Row { \
    when leading != none { leading } \
    Box #[width: 30, height: 30, bg: 0xFF0000, tap => on_tap()] {} } }";

#[test]
fn a_capitalised_call_in_a_block_is_a_callback_when_the_parameter_is_an_fn() {
    let mut app = App::new(BLOCKS);
    assert_eq!(texts(&app), vec!["slot"], "the same shape given to a Slot");
    let boxes: Vec<(f32, f32)> = app
        .frame
        .instances()
        .iter()
        .filter(|b| (b.rect[2] - 30.0).abs() < 0.5)
        .map(|b| (b.rect[0] + 15.0, b.rect[1] + 15.0))
        .collect();
    assert_eq!(boxes.len(), 2);
    for at in boxes {
        app.press(at);
        app.release(at);
    }
    assert_eq!(
        app.int("Count"),
        2,
        "`{{ Bump() }}` and `{{ Count += 1 }}` both ran"
    );
}

#[test]
fn a_block_that_fits_either_reading_keeps_both_until_the_callee_is_known() {
    let parsed =
        parse("View Main() { Item(a: { Text(\"x\") }, b: { m.Icon(\"x\") }, c: { Api.save() }) }");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let byard_compiler::parser::ast::Member::Element(call) = &parsed.views[0].body[0] else {
        panic!("a call");
    };
    for arg in &call.content {
        assert!(
            matches!(
                &arg.value,
                byard_compiler::parser::ast::Expr::Lambda { slot: Some(_), .. }
            ),
            "{:?}",
            arg.value
        );
    }
    // Settled by the parameters: `a` and `b` are slots, `c` an `Fn`.
    let settled = call
        .with_blocks_settled(|n| matches!(n.as_str(), "a" | "b"), |n| n.as_str() == "c")
        .expect("ambiguous arguments");
    let kinds: Vec<&str> = settled
        .content
        .iter()
        .map(|a| match &a.value {
            byard_compiler::parser::ast::Expr::SlotBlock { .. } => "slot",
            byard_compiler::parser::ast::Expr::Lambda { slot: None, .. } => "callback",
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["slot", "slot", "callback"]);
}

#[test]
fn a_block_of_elements_given_to_an_fn_and_actions_given_to_a_slot_are_told() {
    // Only elements parse (an attribute block), given to a callback.
    let errors = checked(
        "View Main() { Card(on_tap: { Row #[gap: 1] { } }) } View Card(on_tap: Fn() = none) { Box {} }",
    );
    assert!(
        errors.iter().any(|e| e.contains("SlotArgument")),
        "{errors:?}"
    );
    // Only actions parse, given to a Slot.
    let errors = checked(
        "View Main() { var n = 0 Card(head: { n++ }) } View Card(head: Slot = none) { Box { head } }",
    );
    assert!(
        errors.iter().any(|e| e.contains("SlotArgument")),
        "{errors:?}"
    );
}

#[test]
fn a_block_that_is_neither_says_what_each_reading_expected() {
    let parsed = parse("View Main() { var n = 0 Card(x: { Row #[gap: 1] n++ }) }");
    let e = parsed
        .errors
        .iter()
        .find(|e| format!("{e:?}").contains("BlockFitsNeither"))
        .unwrap_or_else(|| panic!("{:?}", parsed.errors));
    let text = e.headline();
    assert!(text.contains("Fn") && text.contains("Slot"), "{text}");
}

// ── names are the whole view's, and a state block only paints ───────────

#[test]
fn a_label_written_before_its_field_follows_the_fields_focus() {
    let mut app = App::new(
        "View Main() { Column #[width: 400, p: 20, gap: 10] { \
         Box #[width: 60, height: 20, bg: 0x00FF00] { on field.focused { bg: 0xFF0000 } } \
         Box #[width: 100, height: 40, bg: 0x888888, a11y_role: button, tap => {}] as field {} } }",
    );
    assert!(has_box(&app, 60.0, 20.0, 0x00FF00));
    app.key("Tab");
    assert!(
        has_box(&app, 60.0, 20.0, 0xFF0000),
        "the field is written after the label, and the frame it gains focus is the one that shows it"
    );
}

#[test]
fn a_name_used_before_its_declaration_is_still_scoped_to_its_own_view() {
    let errors = checked(
        "View Main() { Column { Box { on field.hover { bg: 0xFF0000 } } Other() } } \
         View Other() { Box #[width: 10, height: 10] as field {} }",
    );
    assert!(
        errors.iter().any(|e| e.contains("UnknownStateTarget")),
        "{errors:?}"
    );
}

#[test]
fn a_layout_attribute_in_a_block_on_another_elements_states_is_an_error() {
    let errors = checked(
        "View Main() { Column { Box #[width: 10, height: 10] as field {} \
         Box #[width: 10, height: 10] { on field.focused { height: 40, bg: 0xFF0000 } } } }",
    );
    let e = errors
        .iter()
        .find(|e| e.contains("LayoutInStateBlock"))
        .unwrap_or_else(|| panic!("{errors:?}"));
    assert!(e.contains("height") && e.contains("field.focused"), "{e}");
    assert_eq!(
        errors
            .iter()
            .filter(|e| e.contains("LayoutInStateBlock"))
            .count(),
        1,
        "`bg` is paint: {errors:?}"
    );
    let errors = checked(
        "View Main() { Row #[width: 100, height: 30] { on focus_within { width: 200 } \
         Box #[width: 10, height: 10] {} } }",
    );
    assert!(
        errors.iter().any(|e| e.contains("LayoutInStateBlock")),
        "{errors:?}"
    );
    // Paint attributes are fine, and so is `translate`.
    let errors = checked(
        "View Main() { Column { Box #[width: 10, height: 10] as field {} \
         Box #[width: 10, height: 10] { on field.focused { translate.y: -8, scale: 0.75 } } } }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

// ── only the chosen reading of a block speaks ───────────────────────────

struct Kits(Vec<(&'static str, &'static str, &'static str)>);

impl PackageProvider for Kits {
    fn package_files(
        &mut self,
        _dependent: &str,
        package: &str,
    ) -> Result<Vec<SourceFile>, String> {
        self.0
            .iter()
            .find(|(name, ..)| *name == package)
            .map(|(_, file, source)| {
                vec![SourceFile {
                    name: (*file).to_string(),
                    source: (*source).to_string(),
                }]
            })
            .ok_or_else(|| "not declared".to_string())
    }
}

fn resolved_errors(
    source: &str,
    kits: Vec<(&'static str, &'static str, &'static str)>,
) -> Vec<String> {
    let program = resolve_program(
        vec![SourceFile {
            name: "main.byd".into(),
            source: source.into(),
        }],
        &mut Kits(kits),
    );
    program.errors.iter().map(|e| format!("{e:?}")).collect()
}

const CARD_VIEW: &str =
    "View Card(on_tap: Fn() = none, content) { Box #[tap => on_tap()] { content } }";

#[test]
fn a_capability_call_in_a_callback_block_is_not_read_as_a_package() {
    let errors = resolved_errors(
        &format!(
            "View Main() {{ inject Http as http\n Card(on_tap: {{ http.get(\"https://example.com\") }}) {{ Text(\"x\") }} }}\n{CARD_VIEW}"
        ),
        Vec::new(),
    );
    assert!(
        !errors.iter().any(|e| e.contains("UnknownPackage")),
        "{errors:?}"
    );
    // The same call as an event action checks the same.
    let action = resolved_errors(
        "View Main() { inject Http as http\n Button(\"go\") => http.get(\"https://example.com\") }",
        Vec::new(),
    );
    assert_eq!(errors, action, "a block and an action say the same");
}

#[test]
fn a_capability_call_with_result_arms_is_a_callback() {
    let errors = resolved_errors(
        &format!(
            "View Main() {{ inject Http as http\n var n = 0\n \
             Card(on_tap: {{ http.get(\"https://example.com\") ok res => {{ n = 1 }} err e => {{ n = 2 }} }}) {{ Text(\"x\") }} }}\n{CARD_VIEW}"
        ),
        Vec::new(),
    );
    assert!(
        !errors.iter().any(|e| e.contains("UnknownPackage")),
        "{errors:?}"
    );
}

#[test]
fn a_package_qualified_view_in_a_block_is_a_slot() {
    let kits = vec![(
        "kit",
        "kit/icon.byd",
        "View Glyph(name: Str) { Text(name) }",
    )];
    let source = "use kit as m\nView Main() { Panel(leading: { m.Glyph(\"wifi\") }) }\n\
                  View Panel(leading: Slot = none) { Row { leading } }";
    let program = resolve_program(
        vec![SourceFile {
            name: "main.byd".into(),
            source: source.into(),
        }],
        &mut Kits(kits),
    );
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let root = byard_compiler::parser::ast::root_view(&program.views).unwrap();
    let tree = interp.lower_view(root, &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, W, H);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let texts: Vec<String> = frame.texts().iter().map(|t| t.text.clone()).collect();
    assert_eq!(texts, vec!["wifi"]);
}

#[test]
fn value_states_of_a_named_element_reach_elements_written_before_it() {
    let mut app = App::new(
        "View Main() { var on = false Column #[width: 400, p: 20, gap: 10] { \
         Box #[width: 61, height: 20, bg: 0x00FF00] { on field.invalid { bg: 0xFF0000 } } \
         Box #[width: 62, height: 20, bg: 0x00FF00] { on field.selected { bg: 0xFF0000 } } \
         Box #[width: 63, height: 20, bg: 0x00FF00] { on field.disabled { bg: 0xFF0000 } } \
         Box #[width: 64, height: 20, bg: 0x00FF00] { on sw.checked { bg: 0xFF0000 } } \
         Box #[width: 100, height: 40, invalid: true, selected: true, disabled: true] as field {} \
         Toggle #[bind: on] as sw } }",
    );
    for w in [61.0, 62.0, 63.0] {
        assert!(
            has_box(&app, w, 20.0, 0xFF0000),
            "{w} follows the field written after it"
        );
    }
    assert!(has_box(&app, 64.0, 20.0, 0x00FF00), "the switch is off");
    app.set_bool("on", true);
    assert!(
        has_box(&app, 64.0, 20.0, 0xFF0000),
        "the switch is on, the same frame"
    );
}
