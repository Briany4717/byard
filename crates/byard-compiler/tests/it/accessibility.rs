//! The accessibility tree (RFC-0042), built headlessly from `byld`.
//!
//! No window, no assistive technology and no D-Bus: a frame is asked for its
//! tree the way the platform would ask for one, and the tree is read and acted
//! on as an assistive technology would. The platform half, mapping the tree
//! onto AccessKit, is tested in `byard-platform`; the headless dump of a whole
//! project is tested in `byard-cli`.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::access::{
    AccessAction, AccessActions, AccessFlags, AccessId, AccessNode, AccessRequest, AccessRole,
    AccessTree, AccessValue,
};
use byard_core::frame::RenderFrame;

const W: f32 = 400.0;
const H: f32 = 600.0;

/// A running view, rendered with or without a tree.
struct Run {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
}

impl Run {
    /// Lowers `src`'s first view and renders it once with a tree.
    fn start(src: &str) -> Self {
        let mut run = Self::lowered(src);
        run.render(true);
        run
    }

    /// Lowers `src`'s first view and renders nothing yet.
    fn lowered(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        interp.tick();
        Self {
            interp,
            tree,
            frame: RenderFrame::new(),
        }
    }

    /// One frame, asking for a tree or not.
    fn render(&mut self, wanted: bool) {
        self.interp.tick();
        self.frame.clear();
        self.frame.set_access_wanted(wanted);
        self.interp.render(&self.tree, &mut self.frame, W, H);
    }

    fn access(&self) -> &AccessTree {
        self.frame
            .access()
            .expect("the frame asked for a tree and got one")
    }

    /// The first node with this role.
    fn role(&self, role: AccessRole) -> &AccessNode {
        self.access()
            .nodes
            .iter()
            .find(|n| n.role == role)
            .unwrap_or_else(|| panic!("no {role:?} in\n{}", self.access().to_text()))
    }

    /// The node named `name`.
    fn named(&self, name: &str) -> &AccessNode {
        self.access()
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("nothing named {name:?} in\n{}", self.access().to_text()))
    }

    /// Does what an assistive technology asked, then draws the next frame.
    fn ask(&mut self, target: AccessId, action: AccessAction) {
        let events = AccessRequest { target, action }.to_events(self.access(), 100);
        assert!(!events.is_empty(), "the request produced nothing to do");
        self.interp.dispatch_events(&events);
        self.render(true);
    }

    fn var(&self, name: &str) -> Value {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .unwrap_or_else(|| panic!("no var {name}"));
        self.interp.peek(sig)
    }
}

// ── The role table (§2) ─────────────────────────────────────────────────

const CONTROLS: &str = r#"
View Main() {
    var wifi = true
    var agree = false
    var plan = "pro"
    var volume = 0.5
    var q = "hello"
    Column #[p: 10, gap: 8] {
        Button("Save") #[width: 80, height: 32] => wifi = !wifi
        TextField #[bind: q, placeholder: "Search", width: 200, height: 32]
        Toggle #[bind: wifi, width: 52, height: 32]
        Checkbox #[bind: agree, width: 24, height: 24]
        RadioButton #[bind: plan, value: "pro", width: 24, height: 24]
        Slider #[bind: volume, min: 0, max: 1, step: 0.25, width: 200, height: 24]
        Text("Some text")
        Image("none.png") #[width: 20, height: 20]
        VectorIcon("none.svg") #[size: 20]
    }
}
"#;

#[test]
fn every_control_has_the_role_the_table_gives_it() {
    let run = Run::start(CONTROLS);
    let roles: Vec<AccessRole> = run.access().nodes.iter().map(|n| n.role).collect();
    assert_eq!(
        roles,
        [
            AccessRole::Button,
            AccessRole::TextInput,
            AccessRole::Switch,
            AccessRole::CheckBox,
            AccessRole::RadioButton,
            AccessRole::Slider,
            AccessRole::Label,
            AccessRole::Image,
            AccessRole::Image,
        ],
        "{}",
        run.access().to_text()
    );
}

#[test]
fn layout_containers_are_flattened_into_the_nearest_ancestor_with_a_role() {
    let run = Run::start(
        r#"View Main() {
            var n = 0
            Column {
                Row {
                    Box { Column { Button("Deep") #[width: 60, height: 30] => n = n + 1 } }
                }
                ScrollView #[width: 200, height: 100] {
                    Row { Column { Text("Inside") } }
                }
            }
        }"#,
    );
    let tree = run.access();
    // Seven containers deep and still a button with no parent; the label
    // inside the scroll view hangs off the scroll view, not off the rows.
    let button = run.named("Deep");
    assert_eq!(button.parent, None);
    let scroll = run.role(AccessRole::ScrollView);
    let label = run.named("Inside");
    assert_eq!(label.parent, Some(scroll.id));
    assert_eq!(tree.nodes.len(), 3, "{}", tree.to_text());
    // Parents come before their children.
    let at = |id| tree.nodes.iter().position(|n| n.id == id).unwrap();
    assert!(at(scroll.id) < at(label.id));
}

#[test]
fn an_element_with_a_tap_handler_is_a_button_whatever_it_is_called() {
    let run = Run::start(
        r#"View Main() {
            var n = 0
            Box #[a11y_label: "Close", width: 32, height: 32, tap => n = n + 1] {
                VectorIcon("x.svg") #[size: 16, a11y_hidden: true]
            }
        }"#,
    );
    let close = run.named("Close");
    assert_eq!(close.role, AccessRole::Button);
    assert!(close.actions.contains(AccessActions::PRESS));
    assert_eq!(run.access().nodes.len(), 1, "the icon is decoration");
}

#[test]
fn a_native_view_is_a_group_unless_it_declares_otherwise() {
    use byard_core::render::registry::{DeclaredProp, declare};
    assert!(declare("A11yProbeView", &[] as &[DeclaredProp]));
    let run = Run::start(
        r#"View Main() { Column { A11yProbeView #[a11y_label: "Chart", width: 50, height: 40] } }"#,
    );
    let group = run.named("Chart");
    assert_eq!(group.role, AccessRole::Group);
    assert_eq!(
        byard_core::render::registry::access_role("A11yProbeView"),
        Some(AccessRole::Group)
    );
}

#[test]
fn a_nav_host_is_a_tab_list_of_its_live_screens() {
    let run = Run::start(
        r#"View Main() {
            var tab = "home"
            NavHost(active: tab) #[width: 300, height: 200] {
                tab "home" { Text("Welcome") }
                tab "inbox" { Text("Mail") }
            }
        }"#,
    );
    let list = run.role(AccessRole::TabList);
    let tab = run.role(AccessRole::Tab);
    assert_eq!(tab.name.as_deref(), Some("home"));
    assert_eq!(tab.parent, Some(list.id));
    assert!(tab.flags.contains(AccessFlags::SELECTED));
    assert_eq!(run.named("Welcome").parent, Some(tab.id));
}

#[test]
fn a_modal_overlay_is_a_dialog_and_a_plain_one_is_not() {
    let run = Run::start(
        r#"View Main() {
            Column {
                Button("Behind") #[width: 80, height: 30] => 0
                Overlay #[modal: true, a11y_label: "Confirm"] {
                    Button("OK") #[width: 80, height: 30] => 0
                }
            }
        }"#,
    );
    let dialog = run.role(AccessRole::Dialog);
    assert_eq!(dialog.name.as_deref(), Some("Confirm"));
    assert!(dialog.flags.contains(AccessFlags::MODAL));
    assert_eq!(run.named("OK").parent, Some(dialog.id));
    assert_eq!(run.named("Behind").parent, None);

    let run = Run::start(
        r#"View Main() {
            Column {
                Overlay #[modal: false] { Button("Menu item") #[width: 80, height: 30] => 0 }
            }
        }"#,
    );
    assert!(
        run.access()
            .nodes
            .iter()
            .all(|n| n.role != AccessRole::Dialog),
        "{}",
        run.access().to_text()
    );
    assert_eq!(run.named("Menu item").parent, None);
}

// ── Names (§3) ──────────────────────────────────────────────────────────

#[test]
fn a_label_you_wrote_beats_every_other_source_of_a_name() {
    let run = Run::start(
        r#"View Main() {
            var q = ""
            Column {
                Button("Visible text") #[a11y_label: "Spoken text", width: 90, height: 30] => 0
                TextField #[bind: q, placeholder: "Placeholder", a11y_label: "Search the shop", width: 200, height: 30]
            }
        }"#,
    );
    assert_eq!(
        run.role(AccessRole::Button).name.as_deref(),
        Some("Spoken text")
    );
    let field = run.role(AccessRole::TextInput);
    assert_eq!(field.name.as_deref(), Some("Search the shop"));
    // The placeholder is not lost; it describes the field instead.
    assert_eq!(field.description.as_deref(), Some("Placeholder"));
}

#[test]
fn a_text_field_without_a_label_is_named_by_its_placeholder() {
    let run = Run::start(
        r#"View Main() {
            var q = "typed"
            TextField #[bind: q, placeholder: "Search", width: 200, height: 30]
        }"#,
    );
    let field = run.role(AccessRole::TextInput);
    assert_eq!(field.name.as_deref(), Some("Search"));
    assert_eq!(field.description, None, "it would only say the name twice");
    assert_eq!(field.value, Some(AccessValue::Text("typed".into())));
}

#[test]
fn a_control_is_named_by_the_text_inside_it_and_that_text_is_not_repeated() {
    let run = Run::start(
        r#"View Main() {
            Column {
                Button("Save") #[width: 80, height: 30] => 0
                Box #[width: 120, height: 40, tap => 0] {
                    Row { Text("Mark") Text("as read") }
                }
            }
        }"#,
    );
    assert_eq!(run.access().nodes.len(), 2, "{}", run.access().to_text());
    assert_eq!(run.access().nodes[0].name.as_deref(), Some("Save"));
    assert_eq!(
        run.access().nodes[1].name.as_deref(),
        Some("Mark as read"),
        "the Text children, joined with a space"
    );
}

#[test]
fn a_switch_checkbox_or_radio_is_named_by_the_label_beside_it_in_its_row() {
    let run = Run::start(
        r#"View Main() {
            var a = true
            var b = false
            var c = "x"
            Column {
                Row { Toggle #[bind: a, width: 52, height: 30] Text("Wi-Fi") }
                Row { Checkbox #[bind: b, width: 24, height: 24] Text("Remember me") }
                Row { RadioButton #[bind: c, value: "x", width: 24, height: 24] Text("Express") }
                Row { Toggle #[bind: a, width: 52, height: 30] }
                Text("Stray")
            }
        }"#,
    );
    let names: Vec<(AccessRole, Option<&str>)> = run
        .access()
        .nodes
        .iter()
        .filter(|n| n.role != AccessRole::Label)
        .map(|n| (n.role, n.name.as_deref()))
        .collect();
    assert_eq!(
        names,
        [
            (AccessRole::Switch, Some("Wi-Fi")),
            (AccessRole::CheckBox, Some("Remember me")),
            (AccessRole::RadioButton, Some("Express")),
            // The row has no text of its own, and a label in another row is
            // not its label.
            (AccessRole::Switch, None),
        ]
    );
    assert!(
        run.access().to_text().contains("Switch (unnamed)"),
        "{}",
        run.access().to_text()
    );
}

#[test]
fn a_label_is_reactive() {
    let mut run = Run::start(
        r#"View Main() {
            var count = 3
            Column {
                Button("Inbox") #[a11y_label: "{count} unread", width: 80, height: 30] => count = count + 1
            }
        }"#,
    );
    assert_eq!(
        run.role(AccessRole::Button).name.as_deref(),
        Some("3 unread")
    );
    let id = run.role(AccessRole::Button).id;
    run.ask(id, AccessAction::Press);
    assert_eq!(
        run.role(AccessRole::Button).name.as_deref(),
        Some("4 unread")
    );
}

#[test]
fn a_hidden_element_takes_its_subtree_out_of_the_tree() {
    let run = Run::start(
        r#"View Main() {
            Column {
                Image("hero.png") #[width: 100, height: 50, a11y_hidden: true]
                Row #[a11y_hidden: true] {
                    Button("Gone") #[width: 80, height: 30] => 0
                    Text("Also gone")
                }
                Text("Kept")
            }
        }"#,
    );
    let names: Vec<_> = run
        .access()
        .nodes
        .iter()
        .map(|n| n.name.as_deref().unwrap_or("?"))
        .collect();
    assert_eq!(names, ["Kept"], "{}", run.access().to_text());
}

#[test]
fn the_attributes_are_checked_like_any_other() {
    let parsed = parse(
        r#"View Main() { Column { Button("x") #[a11y_hidden: "yes", a11y_label: 4] => 0 } }"#,
    );
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &["Main"]);
    let errors: Vec<String> = interp
        .errors()
        .iter()
        .map(byard_compiler::CompileError::headline)
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    // And every element takes them: the ones that build their own prop table
    // by hand included.
    let ok = parse(
        r#"View Main() { Column {
            Text("t") #[a11y_label: "x", a11y_hidden: true]
            VectorIcon("i.svg") #[a11y_label: "x"]
            Canvas #[width: 10, height: 10, a11y_label: "chart"] { circle(cx: 5, cy: 5, r: 2) }
            Overlay #[a11y_label: "dialog"] { Text("t") }
            Spacer
        } }"#,
    );
    let mut interp = Interpreter::new();
    interp.load_views(&ok.views);
    let _ = interp.lower_view(&ok.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
}

// ── Value and state (§1, §2) ────────────────────────────────────────────

#[test]
fn every_value_control_reports_its_value_and_state() {
    let run = Run::start(CONTROLS);
    assert_eq!(
        run.role(AccessRole::Switch).value,
        Some(AccessValue::Toggled(true))
    );
    assert!(
        !run.role(AccessRole::CheckBox)
            .flags
            .contains(AccessFlags::CHECKED)
    );
    assert!(
        run.role(AccessRole::RadioButton)
            .flags
            .contains(AccessFlags::CHECKED)
    );
    assert_eq!(
        run.role(AccessRole::Slider).value,
        Some(AccessValue::Number {
            value: 0.5,
            min: 0.0,
            max: 1.0,
            step: Some(0.25),
        })
    );
    assert_eq!(
        run.role(AccessRole::TextInput).value,
        Some(AccessValue::Text("hello".into()))
    );
}

#[test]
fn a_checkbox_can_be_mixed_and_a_disabled_control_says_so_and_cannot_be_pressed() {
    let run = Run::start(
        r#"View Main() {
            var on = true
            Column {
                Checkbox #[indeterminate: true, width: 24, height: 24]
                Button("Nope") #[disabled: true, width: 80, height: 30] => on = false
            }
        }"#,
    );
    let mixed = run.role(AccessRole::CheckBox);
    assert!(mixed.flags.contains(AccessFlags::MIXED));
    assert!(
        mixed.actions.is_empty(),
        "a sample with no `bind:` does nothing"
    );
    let button = run.named("Nope");
    assert!(button.flags.contains(AccessFlags::DISABLED));
    assert!(!button.actions.contains(AccessActions::PRESS));
}

#[test]
fn a_scroll_view_reports_its_position_and_how_far_it_can_go() {
    let run = Run::start(
        r"View Main() {
            var y = 30
            ScrollView #[width: 200, height: 100, offset: (0, y)] {
                Column {
                    Box #[width: 100, height: 400] {}
                }
            }
        }",
    );
    let scroll = run.role(AccessRole::ScrollView);
    assert_eq!(
        scroll.value,
        Some(AccessValue::Scroll {
            x: 0.0,
            y: 30.0,
            max_x: 0.0,
            max_y: 300.0
        })
    );
}

#[test]
fn bounds_are_where_the_control_is_drawn_after_scroll_and_transform() {
    let run = Run::start(
        r#"View Main() {
            var y = 20
            Column {
                ScrollView #[width: 200, height: 100, offset: (0, y)] {
                    Column {
                        Box #[width: 100, height: 40, tap => 0] { Text("one") }
                        Box #[width: 100, height: 40, tap => 0] { Text("two") }
                    }
                }
                Box #[width: 60, height: 60, translate: (10, 5), scale: 2, origin: (0, 0), tap => 0] {
                    Text("zoomed")
                }
            }
        }"#,
    );
    let one = run.named("one").bounds;
    let two = run.named("two").bounds;
    assert_eq!(
        (one.x, one.y, one.width, one.height),
        (0.0, -20.0, 100.0, 40.0)
    );
    assert_eq!((two.x, two.y), (0.0, 20.0), "scrolled by 20");
    let zoomed = run.named("zoomed").bounds;
    // 100 below the top (the scroll view), moved by the translate and doubled
    // by the scale.
    assert_eq!((zoomed.x, zoomed.y), (10.0, 105.0));
    assert_eq!((zoomed.width, zoomed.height), (120.0, 120.0));
}

// ── The cost (§Cost) ────────────────────────────────────────────────────

#[test]
fn a_frame_that_did_not_ask_builds_no_tree_and_allocates_none() {
    let mut run = Run::lowered(CONTROLS);
    let before = byard_core::access::builds();
    for _ in 0..3 {
        run.render(false);
        assert!(run.frame.access().is_none());
    }
    assert_eq!(
        byard_core::access::builds(),
        before,
        "a frame that never asked for a tree must not start one"
    );
    // Nothing was allocated for one, on the frame or in the interpreter.
    assert_eq!(run.frame.access_capacity(), 0);
    assert_eq!(run.interp.access_scratch_capacity(), 0);

    // The same assertions, to show they are able to fail: one frame that does
    // ask builds a tree and leaves storage behind.
    run.render(true);
    assert!(byard_core::access::builds() > before);
    assert!(run.frame.access_capacity() > 0);
    assert!(run.interp.access_scratch_capacity() > 0);

    // And a frame that stops asking stops building, whatever came before.
    let after = byard_core::access::builds();
    run.render(false);
    run.render(false);
    assert_eq!(byard_core::access::builds(), after);
    assert!(run.frame.access().is_none());
}

#[test]
fn an_idle_app_says_the_same_thing_every_frame() {
    let mut run = Run::start(CONTROLS);
    let first = run.access().content_hash();
    for _ in 0..3 {
        run.render(true);
        assert_eq!(run.access().content_hash(), first);
    }
}

// ── Actions (§5) ────────────────────────────────────────────────────────

#[test]
fn pressing_a_button_node_runs_its_handler() {
    let mut run = Run::start(CONTROLS);
    assert_eq!(run.var("wifi"), Value::Bool(true));
    let id = run.named("Save").id;
    run.ask(id, AccessAction::Press);
    assert_eq!(run.var("wifi"), Value::Bool(false), "the handler ran");
    // The same press works on a switch, a checkbox and a radio button: every
    // control already handles a tap.
    let id = run.role(AccessRole::CheckBox).id;
    run.ask(id, AccessAction::Press);
    assert_eq!(run.var("agree"), Value::Bool(true));
}

#[test]
fn setting_a_slider_node_writes_its_var() {
    let mut run = Run::start(CONTROLS);
    let id = run.role(AccessRole::Slider).id;
    run.ask(id, AccessAction::SetNumber(0.3));
    assert_eq!(
        run.var("volume"),
        Value::Float(0.3),
        "exactly, not as an f32"
    );
    run.ask(id, AccessAction::SetNumber(0.5));
    run.ask(id, AccessAction::Increment);
    assert_eq!(run.var("volume"), Value::Float(0.75));
    run.ask(id, AccessAction::Increment);
    assert_eq!(run.var("volume"), Value::Float(1.0), "stops at the end");
    run.ask(id, AccessAction::Decrement);
    assert_eq!(run.var("volume"), Value::Float(0.75));
    assert_eq!(
        run.role(AccessRole::Slider).value,
        Some(AccessValue::Number {
            value: 0.75,
            min: 0.0,
            max: 1.0,
            step: Some(0.25)
        })
    );
}

#[test]
fn setting_a_text_field_node_writes_its_var() {
    let mut run = Run::start(CONTROLS);
    let id = run.role(AccessRole::TextInput).id;
    run.ask(id, AccessAction::SetText("shoes".into()));
    assert_eq!(run.var("q"), Value::Str("shoes".into()));
    assert_eq!(
        run.role(AccessRole::TextInput).value,
        Some(AccessValue::Text("shoes".into()))
    );
}

#[test]
fn focusing_a_node_moves_the_routers_focus_there() {
    let mut run = Run::start(CONTROLS);
    assert_eq!(run.access().focus, None);
    let field = run.role(AccessRole::TextInput).id;
    let checkbox = run.role(AccessRole::CheckBox).id;
    assert!(
        run.access()
            .node(field)
            .unwrap()
            .flags
            .contains(AccessFlags::FOCUSABLE)
    );

    run.ask(checkbox, AccessAction::Focus);
    assert_eq!(run.access().focus, Some(checkbox));
    assert!(
        run.access()
            .node(checkbox)
            .unwrap()
            .flags
            .contains(AccessFlags::FOCUSED)
    );

    run.ask(field, AccessAction::Focus);
    assert_eq!(run.access().focus, Some(field));
    assert!(
        !run.access()
            .node(checkbox)
            .unwrap()
            .flags
            .contains(AccessFlags::FOCUSED)
    );
}

#[test]
fn a_request_for_something_that_cannot_be_focused_changes_nothing() {
    let mut run = Run::start(CONTROLS);
    let field = run.role(AccessRole::TextInput).id;
    run.ask(field, AccessAction::Focus);
    // A label offers no focus action, so there is nothing to send; and an id
    // the router never registered is refused by the router itself.
    let label = run.role(AccessRole::Label).id;
    assert_eq!(
        AccessRequest {
            target: label,
            action: AccessAction::Focus
        }
        .to_events(run.access(), 0)
        .len(),
        0
    );
    let forged = byard_core::InputEvent {
        kind: byard_core::EventKind::AccessFocus,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::InputPayload::Element(label.0)),
        time_ms: 0,
    };
    run.interp.dispatch_events(&[forged]);
    run.render(true);
    assert_eq!(run.access().focus, Some(field));
}

#[test]
fn scrolling_a_node_into_view_scrolls_its_scroll_view() {
    let mut run = Run::start(
        r#"View Main() {
            var y = 0
            ScrollView #[width: 200, height: 100, offset: (0, y)] {
                Column {
                    Box #[width: 100, height: 40, tap => 0] { Text("a") }
                    Box #[width: 100, height: 40, tap => 0] { Text("b") }
                    Box #[width: 100, height: 40, tap => 0] { Text("c") }
                }
            }
        }"#,
    );
    let c = run.named("c");
    assert!(c.actions.contains(AccessActions::SCROLL_INTO_VIEW));
    assert!(
        !run.role(AccessRole::ScrollView)
            .actions
            .contains(AccessActions::SCROLL_INTO_VIEW)
    );
    // Row c runs from 80 to 120 in a view 100 tall.
    let id = c.id;
    run.ask(id, AccessAction::ScrollIntoView);
    assert_eq!(run.var("y"), Value::Int(20));
    assert_eq!(run.named("c").bounds.y, 60.0);
}
