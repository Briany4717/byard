//! Input for composed controls (RFC-0063): keys on any element the keyboard
//! can reach, where the pointer is and dragging, Tab entering a group at its
//! selected member, and `a11y_description`.

use byard_compiler::diagnostics::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::access::{AccessNode, AccessRole};
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
        self.frame.set_access_wanted(true);
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

    fn tabs(&mut self, n: usize) {
        for _ in 0..n {
            self.key("Tab");
        }
    }

    fn pointer(&mut self, kind: EventKind, at: (f32, f32)) {
        let mut e = self.event(kind, None, false);
        e.pos = at;
        self.send(&[e]);
    }

    /// A point `dx`, `dy` from the top-left corner of the node named `name`.
    fn at(&self, name: &str, dx: f32, dy: f32) -> (f32, f32) {
        let b = self.named(name).bounds;
        (b.x + dx, b.y + dy)
    }

    fn named(&self, name: &str) -> &AccessNode {
        let tree = self.frame.access().expect("a tree");
        tree.nodes
            .iter()
            .find(|n| n.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("nothing named {name:?} in\n{}", tree.to_text()))
    }

    fn focused(&self) -> Option<String> {
        let tree = self.frame.access()?;
        let id = tree.focus?;
        tree.node(id)?.name.clone()
    }

    fn value(&self, name: &str) -> Value {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.peek(sig)
    }

    fn int(&self, name: &str) -> i64 {
        match self.value(name) {
            Value::Int(n) => n,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn float(&self, name: &str) -> f64 {
        match self.value(name) {
            Value::Float(f) => f,
            #[allow(clippy::cast_precision_loss)]
            Value::Int(n) => n as f64,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn text(&self, name: &str) -> String {
        match self.value(name) {
            Value::Str(s) => s,
            other => panic!("{name} is {other:?}"),
        }
    }
}

fn lower_errors(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let _ = interp.lower_view(&parsed.views[0], &known);
    interp.errors().to_vec()
}

fn check_errors(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    byard_compiler::infer::check_views(&parsed.views).errors
}

fn kinds(errors: &[CompileError]) -> Vec<&'static str> {
    errors.iter().map(CompileError::kind).collect()
}

// ── 1. Keys on any element the keyboard can reach ───────────────────────

const CHIP: &str = r#"
View Main() {
    var removed = 0
    var last = ""
    var released = ""
    var pressed = 0
    Column #[p: 10] {
        Row #[a11y_role: button, a11y_label: "ada", width: 80, height: 28,
              tap => pressed++,
              key_down(k) => { removed++  last = k },
              key_up(k) => released = k] {
            Text("ada")
        }
    }
}
"#;

#[test]
fn a_composed_control_takes_key_down_and_key_up() {
    let mut app = App::new(CHIP);
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("ada"));
    app.key("Backspace");
    assert_eq!(app.int("removed"), 1);
    assert_eq!(app.text("last"), "Backspace");
    assert_eq!(app.text("released"), "Backspace");
    app.key("Delete");
    assert_eq!(app.text("last"), "Delete");
}

#[test]
fn space_and_enter_press_the_role_and_do_not_reach_key_down() {
    let mut app = App::new(CHIP);
    app.tabs(1);
    app.key("Space");
    app.key("Enter");
    assert_eq!(app.int("pressed"), 2);
    assert_eq!(app.int("removed"), 0, "a key the role takes is not offered");
}

#[test]
fn a_focus_handler_or_a_bound_focus_makes_key_down_reachable() {
    let mut app = App::new(
        r#"
View Main() {
    var keys = 0
    var fa = 0
    var f = false
    Column #[p: 10, gap: 8] {
        Box #[a11y_role: group, a11y_label: "A", width: 40, height: 24, focus => fa++,
              key_down(k) => keys++]
        Box #[a11y_role: group, a11y_label: "B", width: 40, height: 24, focused: f,
              key_down(k) => keys++]
    }
}
"#,
    );
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("A"));
    app.key("x");
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("B"));
    app.key("x");
    assert_eq!(app.int("keys"), 2);
}

#[test]
fn key_down_on_an_element_nothing_can_focus_says_how_to() {
    for body in [
        "Box #[width: 10, height: 10, key_down(k) => n++]",
        "Box #[width: 10, height: 10, key_up => n++]",
        // A tap without a role is still not in the Tab order.
        "Box #[width: 10, height: 10, tap => n++, key_down(k) => n++]",
        // A role that does not take focus, even with a tap.
        "Box #[a11y_role: list_item, width: 10, height: 10, tap => n++, key_down(k) => n++]",
        // A role that does, but nothing to press.
        "Box #[a11y_role: button, width: 10, height: 10, key_down(k) => n++]",
    ] {
        let errors = lower_errors(&format!("View Main() {{ var n = 0\n {body} }}"));
        assert_eq!(kinds(&errors), ["KeyNeedsFocus"], "{body}: {errors:?}");
        let said = errors[0].headline();
        assert!(
            said.contains("focus =>") && said.contains("a11y_role"),
            "{said}"
        );
    }
}

#[test]
fn key_down_is_still_fine_where_it_always_was() {
    for body in [
        "Button(\"x\") #[key_down(k) => n++]",
        "Box #[a11y_role: switch, width: 10, height: 10, tap => n++, key_down(k) => n++]",
        "Box #[a11y_role: on ? link : button, width: 10, height: 10, tap => n++, key_down(k) => n++]",
        "Box #[width: 10, height: 10, blur => n++, key_down(k) => n++]",
    ] {
        let errors = lower_errors(&format!(
            "View Main() {{ var n = 0\n var on = true\n {body} }}"
        ));
        assert!(errors.is_empty(), "{body}: {errors:?}");
    }
}

// ── 2. Where the pointer is ──────────────────────────────────────────────

const POINTER: &str = r#"
View Main() {
    var px = 0.0
    var py = 0.0
    var pw = 0.0
    var ph = 0.0
    var downs = 0
    var moved = 0.0
    Column #[p: 20] {
        Box #[a11y_role: slider, a11y_label: "S", width: 100, height: 40,
              pointer_down(e) => { px = e.x  py = e.y  pw = e.width  ph = e.height  downs++ },
              pointer_move(e) => moved = e.x]
    }
}
"#;

#[test]
fn pointer_down_sees_where_in_the_element_it_landed() {
    let mut app = App::new(POINTER);
    let at = app.at("S", 30.0, 12.0);
    app.pointer(EventKind::PointerDown, at);
    assert_eq!(app.int("downs"), 1);
    assert!((app.float("px") - 30.0).abs() < 0.01, "{}", app.float("px"));
    assert!((app.float("py") - 12.0).abs() < 0.01, "{}", app.float("py"));
    assert!((app.float("pw") - 100.0).abs() < 0.01);
    assert!((app.float("ph") - 40.0).abs() < 0.01);
}

#[test]
fn pointer_move_sees_it_too() {
    let mut app = App::new(POINTER);
    let at = app.at("S", 77.0, 5.0);
    app.pointer(EventKind::PointerMove, at);
    assert!(
        (app.float("moved") - 77.0).abs() < 0.01,
        "{}",
        app.float("moved")
    );
}

#[test]
fn the_position_is_the_elements_not_the_page() {
    // The same press in a second box further right reads the same x.
    let mut app = App::new(
        r#"
View Main() {
    var px = 0.0
    Row #[p: 20, gap: 50] {
        Box #[width: 100, height: 40]
        Box #[a11y_role: slider, a11y_label: "S", width: 100, height: 40,
              pointer_down(e) => px = e.x]
    }
}
"#,
    );
    let at = app.at("S", 30.0, 12.0);
    assert!(at.0 > 150.0);
    app.pointer(EventKind::PointerDown, at);
    assert!((app.float("px") - 30.0).abs() < 0.01, "{}", app.float("px"));
}

#[test]
fn a_field_the_event_does_not_have_is_an_error_with_the_closest() {
    let errors = check_errors(
        "View Main() { var d = 0.0\n Box #[width: 10, height: 10, pointer_down(e) => d = e.z] }",
    );
    let found: Vec<_> = errors
        .iter()
        .filter_map(|e| match e {
            CompileError::UnknownField { name, hint, .. } => Some((name.as_str(), hint.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(found, [("z", Some("x".to_string()))], "{errors:?}");

    // `dx` is the drag's, not the press's.
    let errors = check_errors(
        "View Main() { var d = 0.0\n Box #[width: 10, height: 10, pointer_move(e) => d = e.dx] }",
    );
    assert_eq!(kinds(&errors), ["UnknownField"], "{errors:?}");
    let said = errors[0].headline();
    assert!(
        said.contains("PointerEvent") && said.contains("dx"),
        "{said}"
    );

    // And the ones it has are fine, on each of the pointer events.
    for event in [
        "pointer_down",
        "pointer_up",
        "pointer_move",
        "pointer_enter",
        "pointer_exit",
        "hover",
    ] {
        let errors = check_errors(&format!(
            "View Main() {{ var d = 0.0\n Box #[width: 10, height: 10, {event}(e) => d = e.x + e.y + e.width + e.height] }}"
        ));
        assert!(errors.is_empty(), "{event}: {errors:?}");
    }
}

#[test]
fn a_drag_event_has_dx_and_dy_and_nothing_else_new() {
    for event in ["drag_start", "drag", "drag_end"] {
        let ok = check_errors(&format!(
            "View Main() {{ var d = 0.0\n Box #[width: 10, height: 10, {event}(e) => d = e.dx + e.dy + e.x + e.y + e.width + e.height] }}"
        ));
        assert!(ok.is_empty(), "{event}: {ok:?}");
        let bad = check_errors(&format!(
            "View Main() {{ var d = 0.0\n Box #[width: 10, height: 10, {event}(e) => d = e.distance] }}"
        ));
        assert_eq!(kinds(&bad), ["UnknownField"], "{event}: {bad:?}");
    }
}

#[test]
fn an_event_that_carries_no_value_takes_no_name() {
    for event in [
        "tap",
        "click",
        "double_tap",
        "long_press",
        "secondary",
        "focus",
        "blur",
    ] {
        let errors = check_errors(&format!(
            "View Main() {{ var d = 0\n Box #[width: 10, height: 10, {event}(e) => d = 1] }}"
        ));
        assert_eq!(kinds(&errors), ["EventHasNoValue"], "{event}: {errors:?}");
    }
    let said = check_errors(
        "View Main() { var d = 0.0\n Box #[width: 10, height: 10, tap(e) => d = e.x] }",
    )[0]
    .headline();
    assert!(said.contains("pointer_down"), "{said}");
    // The ones that do carry one are untouched.
    let fine = check_errors("View Main() { var k = \"\"\n Button(\"b\") #[key_down(c) => k = c] }");
    assert!(fine.is_empty(), "{fine:?}");
}

// ── 2b. Dragging ─────────────────────────────────────────────────────────

const DRAG: &str = r#"
View Main() {
    var starts = 0
    var drags = 0
    var ends = 0
    var taps = 0
    var downs = 0
    var dx = 0.0
    var dy = 0.0
    var lastx = 0.0
    var endx = 0.0
    var under = 0
    Column #[p: 20] {
        Box #[a11y_role: slider, a11y_label: "S", width: 100, height: 40,
              tap => taps++,
              pointer_down => downs++,
              drag_start(e) => starts++,
              drag(e) => { drags++  dx = e.dx  dy = e.dy  lastx = e.x },
              drag_end(e) => { ends++  endx = e.x }] {
            on dragging { scale: 1.5 }
        }
    }
}
"#;

#[test]
fn a_held_press_that_moves_is_a_drag_with_its_travel() {
    let mut app = App::new(DRAG);
    let from = app.at("S", 10.0, 20.0);
    app.pointer(EventKind::PointerDown, from);
    assert_eq!((app.int("starts"), app.int("drags")), (0, 0));
    assert_eq!(app.int("downs"), 1);
    // Within the slop: not yet a drag.
    app.pointer(EventKind::PointerMove, (from.0 + 3.0, from.1));
    assert_eq!((app.int("starts"), app.int("drags")), (0, 0));
    // Past it: it starts, and the same move is the first drag.
    app.pointer(EventKind::PointerMove, (from.0 + 30.0, from.1 + 4.0));
    assert_eq!(app.int("starts"), 1);
    assert_eq!(app.int("drags"), 1);
    assert!((app.float("dx") - 30.0).abs() < 0.01);
    assert!((app.float("dy") - 4.0).abs() < 0.01);
    assert!((app.float("lastx") - 40.0).abs() < 0.01);
    app.pointer(EventKind::PointerMove, (from.0 + 60.0, from.1));
    assert_eq!(app.int("starts"), 1, "starts once");
    assert_eq!(app.int("drags"), 2);
    assert!((app.float("dx") - 60.0).abs() < 0.01);
    assert_eq!(app.int("ends"), 0);
    app.pointer(EventKind::PointerUp, (from.0 + 60.0, from.1));
    assert_eq!(app.int("ends"), 1);
    assert!((app.float("endx") - 70.0).abs() < 0.01);
    assert_eq!(app.int("taps"), 0, "a drag is not a tap");
}

#[test]
fn a_drag_keeps_following_the_pointer_outside_the_element() {
    let mut app = App::new(DRAG);
    let from = app.at("S", 90.0, 20.0);
    app.pointer(EventKind::PointerDown, from);
    app.pointer(EventKind::PointerMove, (from.0 + 40.0, from.1));
    app.pointer(EventKind::PointerMove, (from.0 + 200.0, from.1 + 150.0));
    assert_eq!(app.int("drags"), 2);
    assert!(app.float("lastx") > 100.0, "x runs past the width");
    app.pointer(EventKind::PointerUp, (from.0 + 200.0, from.1 + 150.0));
    assert_eq!(app.int("ends"), 1);
}

#[test]
fn a_tap_fires_no_drag_event() {
    let mut app = App::new(DRAG);
    let at = app.at("S", 10.0, 20.0);
    app.pointer(EventKind::PointerDown, at);
    app.pointer(EventKind::PointerUp, at);
    assert_eq!(app.int("taps"), 1);
    assert_eq!(
        (app.int("starts"), app.int("drags"), app.int("ends")),
        (0, 0, 0)
    );
}

#[test]
fn a_drag_sets_the_dragging_state_until_release() {
    let mut app = App::new(DRAG);
    let from = app.at("S", 10.0, 20.0);
    let w = |app: &App| app.named("S").bounds.width;
    assert!((w(&app) - 100.0).abs() < 0.5);
    app.pointer(EventKind::PointerDown, from);
    app.pointer(EventKind::PointerMove, (from.0 + 30.0, from.1));
    assert!(
        (w(&app) - 150.0).abs() < 0.5,
        "dragging scales it: {}",
        w(&app)
    );
    app.pointer(EventKind::PointerUp, (from.0 + 30.0, from.1));
    assert!((w(&app) - 100.0).abs() < 0.5, "and it ends");
}

#[test]
fn an_element_that_listens_to_a_drag_owns_the_press() {
    let mut app = App::new(
        r#"
View Main() {
    var panel = 0
    var drags = 0
    Column #[p: 20] {
        Box #[a11y_role: group, a11y_label: "Panel", width: 200, height: 100,
              pointer_down => panel++] {
            Box #[a11y_role: slider, a11y_label: "S", width: 100, height: 40,
                  drag => drags++]
        }
    }
}
"#,
    );
    let from = app.at("S", 10.0, 10.0);
    app.pointer(EventKind::PointerDown, from);
    assert_eq!(
        app.int("panel"),
        0,
        "the panel is not told of a press that is S's"
    );
    app.pointer(EventKind::PointerMove, (from.0 + 40.0, from.1));
    assert_eq!(app.int("drags"), 1);
}

#[test]
fn a_disabled_element_gets_no_drag() {
    let mut app = App::new(
        r#"
View Main() {
    var drags = 0
    Column #[p: 20] {
        Box #[a11y_role: slider, a11y_label: "S", width: 100, height: 40,
              disabled: true, drag => drags++]
    }
}
"#,
    );
    let from = app.at("S", 10.0, 10.0);
    app.pointer(EventKind::PointerDown, from);
    app.pointer(EventKind::PointerMove, (from.0 + 40.0, from.1));
    assert_eq!(app.int("drags"), 0);
}

// ── 3. The built-in Slider runs the element's handlers ───────────────────

const SLIDER: &str = r#"
View Main() {
    var v = 0.0
    var before = -1.0
    var seen = 0.0
    var keys = ""
    var key_before = -1.0
    var entered = 0
    Column #[p: 20] {
        Slider #[bind: v, min: 0.0, max: 1.0, width: 200, height: 24, step: 0.1,
                 pointer_down(e) => { before = v  seen = e.x },
                 key_down(k) => { keys = k  key_before = v },
                 pointer_enter => entered++]
    }
}
"#;

#[test]
fn a_pointer_down_on_a_built_in_slider_runs_before_the_slider_moves() {
    let mut app = App::new(SLIDER);
    let b = app.interp_slider_bounds();
    app.pointer(EventKind::PointerDown, (b.0 + 100.0, b.1 + 12.0));
    assert!(
        (app.float("before") - 0.0).abs() < 1e-9,
        "saw the old value"
    );
    assert!((app.float("seen") - 100.0).abs() < 0.01);
    assert!(
        (app.float("v") - 0.5).abs() < 1e-6,
        "and the slider still moved"
    );
}

#[test]
fn a_key_down_on_a_built_in_slider_runs_before_it_steps() {
    let mut app = App::new(SLIDER);
    app.tabs(1);
    app.key("ArrowRight");
    assert_eq!(app.text("keys"), "ArrowRight");
    assert!((app.float("key_before") - 0.0).abs() < 1e-9);
    assert!((app.float("v") - 0.1).abs() < 1e-6, "{}", app.float("v"));
}

#[test]
fn the_other_events_written_on_a_built_in_slider_run_too() {
    let mut app = App::new(SLIDER);
    let b = app.interp_slider_bounds();
    app.pointer(EventKind::PointerMove, (b.0 + 10.0, b.1 + 12.0));
    assert!(app.int("entered") >= 1);
}

impl App {
    /// The top-left of the slider, found from the tree.
    fn interp_slider_bounds(&self) -> (f32, f32) {
        let tree = self.frame.access().expect("a tree");
        let n = tree
            .nodes
            .iter()
            .find(|n| n.role == AccessRole::Slider)
            .expect("a slider");
        (n.bounds.x, n.bounds.y)
    }
}

// ── 4. Tab enters a group at its selected member ─────────────────────────

const GROUP: &str = r#"
View Main() {
    var size = "m"
    var clicked = 0
    Column #[p: 10, gap: 8] {
        Box #[a11y_role: button, a11y_label: "Before", width: 40, height: 24, tap => clicked++]
        Row #[a11y_role: radio_group, a11y_label: "Size", gap: 4] {
            Box #[a11y_role: radio, a11y_label: "S", a11y_checked: size == "s",
                  width: 30, height: 24, tap => size = "s"]
            Box #[a11y_role: radio, a11y_label: "M", a11y_checked: size == "m",
                  width: 30, height: 24, tap => size = "m"]
            Box #[a11y_role: radio, a11y_label: "L", a11y_checked: size == "l",
                  width: 30, height: 24, tap => size = "l"]
        }
        Box #[a11y_role: button, a11y_label: "After", width: 40, height: 24, tap => clicked++]
    }
}
"#;

#[test]
fn tab_lands_on_the_checked_member_and_leaves_the_group_at_the_next() {
    let mut app = App::new(GROUP);
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("Before"));
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("M"), "the checked one");
    app.tabs(1);
    assert_eq!(
        app.focused().as_deref(),
        Some("After"),
        "the group is one stop"
    );
    app.key_with("Tab", true);
    assert_eq!(
        app.focused().as_deref(),
        Some("M"),
        "and back at the checked one"
    );
    app.key_with("Tab", true);
    assert_eq!(app.focused().as_deref(), Some("Before"));
}

#[test]
fn tab_leaves_from_where_the_arrows_left_it() {
    let mut app = App::new(GROUP);
    app.tabs(2);
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("L"));
    assert_eq!(app.text("size"), "l");
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("After"));
    app.key_with("Tab", true);
    assert_eq!(app.focused().as_deref(), Some("L"), "now the checked one");
}

#[test]
fn with_nothing_selected_tab_enters_at_the_first() {
    let mut app = App::new(&GROUP.replace(r#"var size = "m""#, r#"var size = "none""#));
    app.tabs(2);
    assert_eq!(app.focused().as_deref(), Some("S"));
}

#[test]
fn a_selected_tab_is_where_tab_enters_a_tab_list() {
    let mut app = App::new(
        r#"
View Main() {
    var tab = "b"
    Column #[p: 10, gap: 8] {
        Row #[a11y_role: tab_list, a11y_label: "Pages", gap: 4] {
            Box #[a11y_role: tab, a11y_label: "A", a11y_selected: tab == "a",
                  width: 40, height: 24, tap => tab = "a"]
            Box #[a11y_role: tab, a11y_label: "B", a11y_selected: tab == "b",
                  width: 40, height: 24, tap => tab = "b"]
            Box #[a11y_role: tab, a11y_label: "C", a11y_selected: tab == "c",
                  width: 40, height: 24, tap => tab = "c"]
        }
    }
}
"#,
    );
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("B"));
    app.tabs(1);
    assert_eq!(
        app.focused().as_deref(),
        Some("B"),
        "the only stop; wraps to itself"
    );
}

#[test]
fn a_disabled_selected_member_is_not_where_tab_enters() {
    let mut app = App::new(
        r#"
View Main() {
    var size = "m"
    Row #[a11y_role: radio_group, a11y_label: "Size", gap: 4, p: 10] {
        Box #[a11y_role: radio, a11y_label: "S", width: 30, height: 24, tap => size = "s"]
        Box #[a11y_role: radio, a11y_label: "M", a11y_checked: size == "m", disabled: true,
              width: 30, height: 24, tap => size = "m"]
    }
}
"#,
    );
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("S"));
}

// ── 5. a11y_description ──────────────────────────────────────────────────

#[test]
fn a_description_reaches_the_node_and_the_dump() {
    let app = App::new(
        r#"
View Main() {
    var email = ""
    var bad = true
    Column #[p: 10, gap: 8] {
        TextField #[bind: email, a11y_label: "Email", placeholder: "you@example.com",
                    width: 200, height: 30,
                    a11y_description: bad ? "Enter a valid address" : "We never share it"]
        Box #[a11y_role: switch, a11y_label: "Wi-Fi", a11y_description: "Turns the radio on",
              width: 52, height: 30]
        Text("Notes") #[a11y_description: ""]
    }
}
"#,
    );
    let email = app.named("Email");
    assert_eq!(email.description.as_deref(), Some("Enter a valid address"));
    assert_eq!(
        app.named("Wi-Fi").description.as_deref(),
        Some("Turns the radio on")
    );
    assert_eq!(
        app.named("Notes").description,
        None,
        "an empty one says nothing"
    );
    let dump = app.frame.access().unwrap().to_text();
    assert!(
        dump.contains(r#"TextInput "Email" description "Enter a valid address""#),
        "{dump}"
    );
    assert!(
        dump.contains(r#"Switch "Wi-Fi" description "Turns the radio on""#),
        "{dump}"
    );
}

#[test]
fn a_description_follows_its_var() {
    let mut app = App::new(
        r#"
View Main() {
    var bad = false
    Box #[a11y_role: button, a11y_label: "Go", width: 40, height: 24,
          a11y_description: bad ? "Fix the form first" : "",
          tap => bad = !bad]
}
"#,
    );
    assert_eq!(app.named("Go").description, None);
    app.tabs(1);
    app.key("Space");
    assert_eq!(
        app.named("Go").description.as_deref(),
        Some("Fix the form first")
    );
}
