//! Controls built from a `Box` are as good as built-in ones (RFC-0052): they
//! can say what they are to a screen reader, the keyboard presses them as their
//! role says, the arrow keys walk a tab list or a radio group, and `focus_visible`
//! is set by the keyboard and not by a pointer press.

use byard_compiler::diagnostics::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::access::{AccessFlags, AccessNode, AccessRole, AccessValue};
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

    /// A key pressed and let go, as `byard shot --key` sends it.
    fn key(&mut self, name: &str) {
        let down = self.event(
            EventKind::KeyDown,
            Some(InputPayload::Key(name.into())),
            false,
        );
        let up = self.event(
            EventKind::KeyUp,
            Some(InputPayload::Key(name.into())),
            false,
        );
        self.send(&[down, up]);
    }

    fn tabs(&mut self, n: usize) {
        for _ in 0..n {
            self.key("Tab");
        }
    }

    /// A pointer press and release at the middle of the node named `name`.
    fn tap(&mut self, name: &str) {
        let b = self.named(name).bounds;
        let at = (b.x + b.width / 2.0, b.y + b.height / 2.0);
        let mut down = self.event(EventKind::PointerDown, None, false);
        down.pos = at;
        let mut up = self.event(EventKind::PointerUp, None, false);
        up.pos = at;
        self.send(&[down, up]);
    }

    fn named(&self, name: &str) -> &AccessNode {
        let tree = self.frame.access().expect("a tree");
        tree.nodes
            .iter()
            .find(|n| n.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("nothing named {name:?} in\n{}", tree.to_text()))
    }

    /// The name of the node with focus.
    fn focused(&self) -> Option<String> {
        let tree = self.frame.access()?;
        let id = tree.focus?;
        tree.node(id)?.name.clone()
    }

    fn int(&self, name: &str) -> i64 {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        match self.interp.peek(sig) {
            Value::Int(n) => n,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn text(&self, name: &str) -> String {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        match self.interp.peek(sig) {
            Value::Str(s) => s,
            other => panic!("{name} is {other:?}"),
        }
    }

    fn flag(&self, name: &str) -> bool {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        matches!(self.interp.peek(sig), Value::Bool(true))
    }
}

const CONTROLS: &str = r#"
View Main() {
    var agreed = false
    var wifi = false
    var visited = 0
    var pressed = 0
    var tab = "a"
    Column #[p: 10, gap: 8] {
        Box #[a11y_role: checkbox, a11y_checked: agreed, a11y_label: "Agree",
              width: 24, height: 24, tap => agreed = !agreed]
        Box #[a11y_role: link, a11y_label: "Docs", width: 60, height: 24, tap => visited++]
        Box #[a11y_role: switch, a11y_checked: wifi, a11y_label: "Wi-Fi",
              width: 52, height: 30, tap => wifi = !wifi]
        Box #[a11y_role: button, a11y_label: "Go", width: 40, height: 24, tap => pressed++]
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
"#;

// ── The attributes reach the tree ───────────────────────────────────────

#[test]
fn a_role_names_the_node_and_its_state_follows_the_var() {
    let mut app = App::new(CONTROLS);
    let agree = app.named("Agree");
    assert_eq!(agree.role, AccessRole::CheckBox);
    assert!(!agree.flags.contains(AccessFlags::CHECKED));
    assert_eq!(app.named("Docs").role, AccessRole::Link);
    let wifi = app.named("Wi-Fi");
    assert_eq!(wifi.role, AccessRole::Switch);
    assert_eq!(wifi.value, Some(AccessValue::Toggled(false)));
    assert_eq!(app.named("Pages").role, AccessRole::TabList);
    let a = app.named("A");
    assert_eq!(a.role, AccessRole::Tab);
    assert!(a.flags.contains(AccessFlags::SELECTED));
    assert!(!app.named("B").flags.contains(AccessFlags::SELECTED));

    app.tap("Agree");
    app.tap("Wi-Fi");
    assert!(
        app.named("Agree").flags.contains(AccessFlags::CHECKED),
        "{}",
        app.frame.access().unwrap().to_text()
    );
    assert_eq!(app.named("Wi-Fi").value, Some(AccessValue::Toggled(true)));
}

#[test]
fn value_mixed_expanded_and_disabled_reach_the_tree() {
    let app = App::new(
        r#"
View Main() {
    var open = true
    Column #[p: 10, gap: 8] {
        Box #[a11y_role: progress_bar, a11y_label: "Upload", a11y_value: "40 percent",
              width: 100, height: 8]
        Box #[a11y_role: checkbox, a11y_label: "All", a11y_checked: true, a11y_mixed: true,
              width: 24, height: 24]
        Box #[a11y_role: button, a11y_label: "More", a11y_expanded: open,
              width: 40, height: 24, tap => open = !open]
        Box #[a11y_role: button, a11y_label: "Less", a11y_expanded: !open,
              width: 40, height: 24, tap => open = !open]
        Box #[a11y_role: menu_item, a11y_label: "Cut", a11y_disabled: true,
              width: 40, height: 24]
        Text("Settings") #[a11y_role: heading]
    }
}
"#,
    );
    assert_eq!(app.named("Upload").role, AccessRole::ProgressBar);
    assert_eq!(
        app.named("Upload").value,
        Some(AccessValue::Text("40 percent".into()))
    );
    let all = app.named("All");
    assert!(all.flags.contains(AccessFlags::MIXED));
    assert!(app.named("More").flags.contains(AccessFlags::EXPANDED));
    assert!(app.named("Less").flags.contains(AccessFlags::COLLAPSED));
    assert!(app.named("Cut").flags.contains(AccessFlags::DISABLED));
    assert_eq!(app.named("Settings").role, AccessRole::Heading);
}

#[test]
fn the_text_inside_a_composed_control_names_it() {
    let app = App::new(
        r#"
View Main() {
    var on = false
    Box #[a11y_role: checkbox, width: 120, height: 24, tap => on = !on] {
        Text("Remember me") #[size: 14]
    }
}
"#,
    );
    let tree = app.frame.access().unwrap();
    assert_eq!(tree.nodes.len(), 1, "{}", tree.to_text());
    assert_eq!(tree.nodes[0].name.as_deref(), Some("Remember me"));
    assert_eq!(tree.nodes[0].role, AccessRole::CheckBox);
}

#[test]
fn the_dump_prints_the_new_roles_and_states() {
    let app = App::new(CONTROLS);
    let dump = app.frame.access().unwrap().to_text();
    assert!(
        dump.contains(r#"Switch "Wi-Fi" = off [focusable]"#),
        "{dump}"
    );
    assert!(dump.contains(r#"Link "Docs" [focusable]"#), "{dump}");
    assert!(dump.contains(r#"Tab "A" [focusable selected]"#), "{dump}");
}

// ── A misspelt role is an error ─────────────────────────────────────────

fn lower_errors(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let _ = interp.lower_view(&parsed.views[0], &known);
    interp.errors().to_vec()
}

#[test]
fn a_misspelt_role_is_an_error_naming_the_closest() {
    for (written, wanted) in [
        ("chekbox", "checkbox"),
        ("swich", "switch"),
        ("\"tab_lst\"", "tab_list"),
    ] {
        let errors = lower_errors(&format!(
            "View Main() {{ Box #[a11y_role: {written}, width: 10, height: 10] }}"
        ));
        let mismatch = errors
            .iter()
            .find(|e| e.kind() == "AttributeTypeMismatch")
            .unwrap_or_else(|| panic!("no error for {written}: {errors:?}"));
        let text = format!("{mismatch:?}");
        assert!(text.contains(&format!("did you mean `{wanted}`")), "{text}");
    }
}

#[test]
fn every_keyword_is_a_role_and_nothing_else_is() {
    for word in AccessRole::KEYWORDS {
        assert!(AccessRole::from_keyword(word).is_some(), "{word}");
        let errors = lower_errors(&format!(
            "View Main() {{ Box #[a11y_role: {word}, width: 10, height: 10] }}"
        ));
        assert!(errors.is_empty(), "{word}: {errors:?}");
    }
    assert_eq!(AccessRole::from_keyword("Button"), None);
}

// ── The keyboard ────────────────────────────────────────────────────────

#[test]
fn a_composed_checkbox_takes_space_and_not_enter() {
    let mut app = App::new(CONTROLS);
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("Agree"));
    app.key("Enter");
    assert!(!app.flag("agreed"), "Enter must not press a checkbox");
    app.key("Space");
    assert!(app.flag("agreed"));
    app.key(" ");
    assert!(!app.flag("agreed"));
}

#[test]
fn a_composed_link_takes_enter_and_not_space() {
    let mut app = App::new(CONTROLS);
    app.tabs(2);
    assert_eq!(app.focused().as_deref(), Some("Docs"));
    app.key("Space");
    assert_eq!(app.int("visited"), 0, "Space must not follow a link");
    app.key("Enter");
    assert_eq!(app.int("visited"), 1);
}

#[test]
fn a_composed_switch_and_button_take_both() {
    let mut app = App::new(CONTROLS);
    app.tabs(3);
    assert_eq!(app.focused().as_deref(), Some("Wi-Fi"));
    app.key("Space");
    assert!(app.flag("wifi"));
    app.key("Enter");
    assert!(!app.flag("wifi"));
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("Go"));
    app.key("Space");
    app.key("Enter");
    assert_eq!(app.int("pressed"), 2);
}

#[test]
fn a_held_key_presses_a_composed_control_once() {
    let mut app = App::new(CONTROLS);
    app.tabs(4);
    let down = |app: &mut App| {
        let e = app.event(
            EventKind::KeyDown,
            Some(InputPayload::Key("Enter".into())),
            false,
        );
        app.send(&[e]);
    };
    down(&mut app);
    down(&mut app);
    down(&mut app);
    assert_eq!(app.int("pressed"), 1);
}

#[test]
fn arrows_walk_a_tab_list_and_enter_selects() {
    let mut app = App::new(CONTROLS);
    app.tabs(5);
    assert_eq!(app.focused().as_deref(), Some("A"));
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("B"));
    assert_eq!(app.text("tab"), "a", "moving focus does not select a tab");
    app.key("Enter");
    assert_eq!(app.text("tab"), "b");
    app.key("ArrowRight");
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("A"), "wraps");
    app.key("ArrowLeft");
    assert_eq!(app.focused().as_deref(), Some("C"), "wraps back");
    app.key("Home");
    assert_eq!(app.focused().as_deref(), Some("A"));
    app.key("End");
    assert_eq!(app.focused().as_deref(), Some("C"));
    // An arrow outside a group is left to whoever wanted it.
    app.tabs(1);
    app.key("ArrowRight");
}

#[test]
fn arrows_in_a_radio_group_move_and_select() {
    let mut app = App::new(
        r#"
View Main() {
    var size = "s"
    Row #[a11y_role: radio_group, a11y_label: "Size", gap: 4, p: 10] {
        Box #[a11y_role: radio, a11y_label: "S", a11y_checked: size == "s",
              width: 30, height: 24, tap => size = "s"]
        Box #[a11y_role: radio, a11y_label: "M", a11y_checked: size == "m",
              width: 30, height: 24, tap => size = "m"]
        Box #[a11y_role: radio, a11y_label: "L", a11y_checked: size == "l",
              width: 30, height: 24, tap => size = "l"]
    }
}
"#,
    );
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("S"));
    app.key("ArrowDown");
    assert_eq!(app.focused().as_deref(), Some("M"));
    assert_eq!(app.text("size"), "m");
    assert!(app.named("M").flags.contains(AccessFlags::CHECKED));
    assert!(!app.named("S").flags.contains(AccessFlags::CHECKED));
    app.key("ArrowLeft");
    app.key("ArrowLeft");
    assert_eq!(app.focused().as_deref(), Some("L"), "wraps");
    assert_eq!(app.text("size"), "l");
    // Space on a radio, and not Enter.
    app.key("ArrowUp");
    assert_eq!(app.text("size"), "m");
}

#[test]
fn a_disabled_member_is_skipped_by_the_arrows() {
    let mut app = App::new(
        r#"
View Main() {
    var tab = "a"
    Row #[a11y_role: tab_list, gap: 4, p: 10] {
        Box #[a11y_role: tab, a11y_label: "A", width: 30, height: 24, tap => tab = "a"]
        Box #[a11y_role: tab, a11y_label: "B", width: 30, height: 24, disabled: true,
              tap => tab = "b"]
        Box #[a11y_role: tab, a11y_label: "C", width: 30, height: 24, tap => tab = "c"]
    }
}
"#,
    );
    app.tabs(1);
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("C"));
}

#[test]
fn a_tap_with_no_role_or_focus_stays_out_of_the_tab_order() {
    let mut app = App::new(
        r#"
View Main() {
    var n = 0
    Box #[a11y_label: "Plain", width: 40, height: 24, tap => n++]
}
"#,
    );
    app.tabs(1);
    assert_eq!(app.focused(), None);
}

#[test]
fn a_focus_handler_makes_a_tapped_box_pressable_by_space_and_enter() {
    let mut app = App::new(
        r#"
View Main() {
    var n = 0
    var f = 0
    Box #[a11y_label: "Plain", width: 40, height: 24, tap => n++, focus => f++]
}
"#,
    );
    app.tabs(1);
    assert_eq!(app.focused().as_deref(), Some("Plain"));
    app.key("Space");
    app.key("Enter");
    assert_eq!(app.int("n"), 2);
    assert_eq!(app.int("f"), 1);
}

// ── focus_visible ───────────────────────────────────────────────────────

/// Boxes whose size tells which state they are in: 50 wide plain, 60 when
/// focused, 70 when focus-visible (a `scale` is what a state block can change
/// and a node's bounds report).
const RINGS: &str = r#"
View Main() {
    var typed = ""
    let ring = style {
        on focused { scale: 1.2 }
        on focus_visible { scale: 1.4 }
    }
    Column #[p: 10, gap: 8] {
        Box #[..ring, a11y_role: button, a11y_label: "One", width: 50, height: 24, tap => typed = "1"]
        Box #[..ring, a11y_role: button, a11y_label: "Two", width: 50, height: 24, tap => typed = "2"]
        TextField #[..ring, bind: typed, placeholder: "Field", width: 50, height: 24]
    }
}
"#;

fn width(app: &App, name: &str) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        app.named(name).bounds.width.round() as i32
    }
}

#[test]
fn focus_visible_is_set_by_tab_and_not_by_a_tap() {
    let mut app = App::new(RINGS);
    assert_eq!(width(&app, "One"), 50);

    app.tap("One");
    assert_eq!(app.focused().as_deref(), Some("One"));
    assert_eq!(width(&app, "One"), 60, "focused, and not focus-visible");

    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("Two"));
    assert_eq!(width(&app, "One"), 50);
    assert_eq!(width(&app, "Two"), 70, "Tab shows the ring");

    app.tap("One");
    assert_eq!(width(&app, "One"), 60);
    assert_eq!(width(&app, "Two"), 50, "a tap elsewhere takes the ring off");
}

#[test]
fn tabbing_round_to_a_clicked_control_shows_the_ring() {
    let mut app = App::new(RINGS);
    app.tap("Two");
    assert_eq!(width(&app, "Two"), 60);
    app.tabs(3);
    assert_eq!(width(&app, "Two"), 70);
}

#[test]
fn a_text_field_shows_focus_however_it_was_reached() {
    let mut app = App::new(RINGS);
    app.tap("Field");
    assert_eq!(app.focused().as_deref(), Some("Field"));
    assert_eq!(width(&app, "Field"), 70);
}

#[test]
fn arrows_in_a_group_show_the_ring() {
    let mut app = App::new(
        r#"
View Main() {
    let ring = style { on focus_visible { scale: 1.4 } }
    Row #[a11y_role: tab_list, gap: 4, p: 10] {
        Box #[..ring, a11y_role: tab, a11y_label: "A", width: 30, height: 24, tap => {}]
        Box #[..ring, a11y_role: tab, a11y_label: "B", width: 30, height: 24, tap => {}]
    }
}
"#,
    );
    app.tap("A");
    assert_eq!(
        width(&app, "A"),
        30,
        "focused by the pointer: not focus-visible"
    );
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("B"));
    assert_eq!(width(&app, "B"), 42);
}

#[test]
fn an_unknown_state_suggests_focus_visible() {
    let parsed = parse(
        "View Main() { let s = style { on focus_visble { width: 3 } } Box #[..s, width: 1, height: 1] }",
    );
    let e = parsed
        .errors
        .iter()
        .find(|e| matches!(e, CompileError::UnknownStyleState { .. }))
        .unwrap_or_else(|| panic!("{:?}", parsed.errors));
    assert!(
        format!("{e:?}").contains("hint: Some(\"focus_visible\")"),
        "{e:?}"
    );
}
