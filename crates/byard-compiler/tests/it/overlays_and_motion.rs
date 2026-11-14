//! RFC-0066: focus scopes, menus, `on enter` and `on leave`, announcements and
//! ranges, anchors by name, crossings and `interactive: false`.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
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
        self.interp.set_now_ms(u32::try_from(self.t).unwrap());
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.frame.set_access_wanted(true);
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 400.0);
    }

    /// Frames, `ms` apart, until `total` has passed.
    fn wait(&mut self, total: u64) {
        let end = self.t + total;
        while self.t < end {
            self.t += 16;
            self.send(&[]);
        }
    }

    fn ev(
        &mut self,
        kind: EventKind,
        payload: Option<InputPayload>,
        pos: (f32, f32),
    ) -> InputEvent {
        self.t += 10;
        InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods: KeyModifiers::default(),
        }
    }

    fn key(&mut self, name: &str) {
        let d = self.ev(
            EventKind::KeyDown,
            Some(InputPayload::Key(name.into())),
            (0.0, 0.0),
        );
        let u = self.ev(
            EventKind::KeyUp,
            Some(InputPayload::Key(name.into())),
            (0.0, 0.0),
        );
        self.send(&[d, u]);
        self.send(&[]);
    }

    fn tap(&mut self, x: f32, y: f32) {
        let d = self.ev(EventKind::PointerDown, None, (x, y));
        let u = self.ev(EventKind::PointerUp, None, (x, y));
        self.send(&[d, u]);
        self.send(&[]);
    }

    fn moved(&mut self, x: f32, y: f32) {
        let m = self.ev(EventKind::PointerMove, None, (x, y));
        self.send(&[m]);
    }

    fn dump(&self) -> String {
        self.frame.access().expect("tree").to_text()
    }

    /// The line of the dump that is focused.
    fn focused(&self) -> String {
        self.dump()
            .lines()
            .find(|l| l.contains("focused"))
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

const DIALOG: &str = r#"
View Main() {
    var open = false
    var hits = 0
    Column {
        Button("Open") #[width: 80, height: 40] => { open = true }
        Button("Other") #[width: 80, height: 40] => { hits += 1 }
        when open {
            Overlay #[dismiss => { open = false }] {
                Column #[a11y_role: dialog, a11y_label: "Discard", anchor: center, bg: 0x333344, p: 20, gap: 10, width: 300] {
                    Button("Cancel") #[width: 80, height: 40] => { open = false }
                    Button("Discard") #[width: 80, height: 40, autofocus: true] => { open = false }
                }
            }
        }
    }
}
"#;

#[test]
fn a_dialog_takes_focus_traps_tab_and_gives_it_back() {
    let mut app = App::new(DIALOG);
    app.key("Tab");
    assert!(app.focused().contains("\"Open\""), "{}", app.dump());
    app.key("Enter");
    app.send(&[]);
    // Focus is in the dialog, on the `autofocus` button, not on the opener.
    assert!(app.focused().contains("\"Discard\""), "{}", app.dump());
    // Tab cycles inside it.
    app.key("Tab");
    assert!(app.focused().contains("\"Cancel\""), "{}", app.dump());
    app.key("Tab");
    assert!(app.focused().contains("\"Discard\""), "{}", app.dump());
    // Escape closes it, and focus is the opener's again.
    app.key("Escape");
    app.send(&[]);
    assert!(app.focused().contains("\"Open\""), "{}", app.dump());
}

#[test]
fn a_dialog_without_autofocus_goes_to_the_first_focusable() {
    let src = DIALOG.replace(", autofocus: true", "");
    let mut app = App::new(&src);
    app.key("Tab");
    app.key("Enter");
    app.send(&[]);
    assert!(app.focused().contains("\"Cancel\""), "{}", app.dump());
}

#[test]
fn focus_goes_to_a_neighbour_when_the_opener_is_gone() {
    let src = r#"
View Main() {
    var open = false
    var shown = true
    Column {
        Button("Keep") #[width: 80, height: 40] => { }
        Column {
            when shown {
                Button("Open") #[width: 80, height: 40] => { open = true shown = false }
            }
        }
        when open {
            Overlay #[dismiss => { open = false }] {
                Column #[a11y_role: dialog, anchor: center, width: 200] {
                    Button("Close") #[width: 80, height: 40] => { open = false }
                }
            }
        }
    }
}
"#;
    let mut app = App::new(src);
    app.key("Tab");
    app.key("Tab");
    assert!(app.focused().contains("\"Open\""), "{}", app.dump());
    app.key("Enter");
    app.send(&[]);
    assert!(app.focused().contains("\"Close\""), "{}", app.dump());
    app.key("Escape");
    app.send(&[]);
    // The opener is gone; focus is not left on nothing.
    let f = app.focused();
    assert!(f.is_empty() || f.contains("\"Keep\""), "{}", app.dump());
}

#[test]
fn a_bound_focused_var_moves_focus_when_written() {
    let src = r#"
View Main() {
    var want = false
    var other = false
    Column {
        Button("A") #[width: 80, height: 40, focused: other] => { }
        Button("B") #[width: 80, height: 40, focused: want] => { }
        Button("Go") #[width: 80, height: 40] => { want = true }
    }
}
"#;
    let mut app = App::new(src);
    app.tap(40.0, 100.0);
    app.send(&[]);
    assert!(app.focused().contains("\"B\""), "{}", app.dump());
}

const MENU: &str = r#"
View Main() {
    var sub = false
    var picked = 0
    Column {
        Overlay #[modal: false, dismiss_on_outside: false] {
            Column #[a11y_role: menu, anchor: top] {
                Button("Copy") #[a11y_role: menu_item, width: 80, height: 30] => { picked = 1 }
                Button("Share") #[a11y_role: menu_item, a11y_expanded: sub, width: 80, height: 30] as share => { sub = true }
                Button("Paste") #[a11y_role: menu_item, width: 80, height: 30] => { picked = 3 }
            }
        }
        when sub {
            Overlay #[modal: false] {
                Column #[a11y_role: menu, anchor_to: share, anchor_edge: after, dismiss => { sub = false }] {
                    Button("Mail") #[a11y_role: menu_item, width: 80, height: 30] => { picked = 4 }
                    Button("Chat") #[a11y_role: menu_item, width: 80, height: 30] => { picked = 5 }
                }
            }
        }
        Text("picked={picked}")
    }
}
"#;

#[test]
fn a_menu_is_an_arrow_group_with_submenus() {
    let mut app = App::new(MENU);
    app.send(&[]);
    assert!(app.dump().contains("MenuItem \"Copy\""), "{}", app.dump());
    assert!(app.focused().contains("\"Copy\""), "{}", app.dump());
    app.key("ArrowDown");
    assert!(app.focused().contains("\"Share\""), "{}", app.dump());
    app.key("ArrowDown");
    app.key("ArrowDown");
    assert!(app.focused().contains("\"Copy\""), "wraps: {}", app.dump());
    app.key("ArrowUp");
    assert!(app.focused().contains("\"Paste\""), "{}", app.dump());
    app.key("Home");
    assert!(app.focused().contains("\"Copy\""), "{}", app.dump());
    app.key("End");
    assert!(app.focused().contains("\"Paste\""), "{}", app.dump());
    // Right opens the submenu, which takes focus.
    app.key("Home");
    app.key("ArrowDown");
    app.key("ArrowRight");
    app.send(&[]);
    assert!(app.focused().contains("\"Mail\""), "{}", app.dump());
    // Left closes it and focus is back on the item.
    app.key("ArrowLeft");
    app.send(&[]);
    assert!(app.focused().contains("\"Share\""), "{}", app.dump());
    // Escape closes the submenu only; here the root has no dismiss.
    app.key("ArrowRight");
    app.send(&[]);
    assert!(app.focused().contains("\"Mail\""), "{}", app.dump());
    app.key("Escape");
    app.send(&[]);
    assert!(app.focused().contains("\"Share\""), "{}", app.dump());
    assert!(app.dump().contains("MenuItem \"Copy\""), "the menu stays");
}

const LEAVE: &str = r#"
View Main() {
    var open = true
    var taps = 0
    Column {
        Button("Toggle") #[width: 80, height: 30] => { open = !open }
        when open {
            Column #[width: 100, height: 100, bg: 0xFF0000, opacity: 1.0 with anim.linear(200ms),
                     a11y_role: group, a11y_label: "Card"] {
                on enter { opacity: 0.0 }
                on leave { opacity: 0.0 }
                Button("Inside") #[width: 80, height: 30] => { taps += 1 }
            }
        }
        Text("taps={taps}")
    }
}
"#;

/// The opacity every 100 wide box is drawn at, wherever the frame keeps it:
/// a group, the blended pass, or the solid one. Empty when none is drawn.
fn opacities(app: &App) -> Vec<f32> {
    let mut out: Vec<f32> = app.frame.groups().iter().map(|g| g.opacity).collect();
    out.extend(
        app.frame
            .decorated()
            .iter()
            .filter(|d| (d.base.rect[2] - 100.0).abs() < 0.5)
            .map(|d| d.base.color[3] * d.opacity),
    );
    out.extend(
        app.frame
            .instances()
            .iter()
            .filter(|b| (b.rect[2] - 100.0).abs() < 0.5)
            .map(|b| b.color[3] * b.transform.opacity),
    );
    out
}

fn card_opacity(app: &App) -> f32 {
    opacities(app).first().copied().unwrap_or(-1.0)
}

#[test]
fn a_leaving_element_draws_takes_nothing_and_is_then_gone() {
    let mut app = App::new(LEAVE);
    app.wait(400);
    assert!(card_opacity(&app) > 0.99, "arrived: {}", card_opacity(&app));
    assert!(app.dump().contains("Card"));
    app.tap(40.0, 15.0);
    // Mid-leave: still drawn, partly faded, not in the tree, takes no tap.
    app.wait(64);
    let mid = card_opacity(&app);
    assert!(mid > 0.0 && mid < 0.99, "still drawn, fading: {mid}");
    assert!(!app.dump().contains("Card"), "{}", app.dump());
    assert!(!app.dump().contains("Inside"), "{}", app.dump());
    app.tap(40.0, 60.0);
    assert!(app.frame.texts().iter().any(|t| t.text == "taps=0"));
    app.wait(500);
    assert_eq!(card_opacity(&app), -1.0, "gone");
}

#[test]
fn a_leaving_element_comes_back_when_shown_again() {
    let mut app = App::new(LEAVE);
    app.wait(400);
    app.tap(40.0, 15.0);
    app.wait(64);
    let mid = card_opacity(&app);
    app.tap(40.0, 15.0);
    let at_tap = card_opacity(&app);
    assert!(
        at_tap <= mid && at_tap > 0.3,
        "from where it was: {mid} then {at_tap}"
    );
    app.wait(32);
    let back = card_opacity(&app);
    assert!(back > at_tap, "turns round: {at_tap} then {back}");
    app.wait(500);
    assert!(card_opacity(&app) > 0.99);
    assert!(app.dump().contains("Card"));
}

#[test]
fn on_enter_starts_from_its_values_and_a_token_stagger_needs_no_from() {
    let src = r#"
View Main() {
    var open = false
    Column {
        Button("Go") #[width: 80, height: 30] => { open = true }
        when open {
            for i in [0, 1] {
                Box #[width: 100, height: 20, bg: 0x00FF00, opacity: 1.0 with anim.stagger(anim.linear(200ms), 100ms, i)] {
                    on enter { opacity: 0.0 }
                }
            }
        }
    }
}
"#;
    let mut app = App::new(src);
    app.tap(40.0, 15.0);
    app.wait(32);
    let op = opacities;
    let early = op(&app);
    assert_eq!(early.len(), 2);
    assert!(early.iter().all(|a| *a < 0.5), "{early:?}");
    app.wait(120);
    let mid = op(&app);
    assert!(mid[0] > mid[1], "the first row is ahead: {mid:?}");
    app.wait(600);
    assert!(op(&app).iter().all(|a| *a > 0.99));
}

#[test]
fn live_described_by_and_ranges_are_in_the_dump() {
    let src = r#"
View Main() {
    Column {
        Box #[a11y_role: slider, a11y_label: "Volume", a11y_value: "40", a11y_value_min: 0, a11y_value_max: 100, a11y_value_step: 5, width: 100, height: 20, ]
        Text("Saved") #[a11y_live: polite]
        Box #[a11y_role: group, a11y_label: "Field", a11y_described_by: hint, width: 50, height: 20]
        Box #[a11y_role: group, a11y_label: "Pick a name", width: 50, height: 20] as hint {}
    }
}
"#;
    let app = App::new(src);
    let d = app.dump();
    assert!(d.contains("Slider \"Volume\" = 40 in 0..100 step 5"), "{d}");
    assert!(d.contains("live polite"), "{d}");
    assert!(d.contains("described_by \"Pick a name\""), "{d}");
}

#[test]
fn range_attributes_need_a_role_that_has_a_value() {
    let parsed = parse(
        "
View Main() {
    Box #[a11y_role: button, a11y_value_min: 0, width: 10, height: 10]
}
",
    );
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &known);
    assert!(
        format!("{:?}", interp.errors()).contains("AttributeNeedsRole"),
        "{:?}",
        interp.errors()
    );
}

#[test]
fn the_diagnostics_are_raised() {
    let check = |src: &str| {
        let parsed = parse(src);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let _ = interp.lower_view(&parsed.views[0], &known);
        format!("{:?}", interp.errors())
    };
    assert!(
        check("View Main() { Box #[autofocus: true, width: 10, height: 10] }")
            .contains("AutofocusNeedsFocus")
    );
    assert!(
        check("View Main() { Box #[width: 10, height: 10] { on enter { width: 5 } } }")
            .contains("LayoutInStateBlock")
    );
    assert!(check("View Main() { Box #[width: 10, height: 10] { on enter { opacity: 0.0 } on enter { opacity: 0.5 } } }").contains("DuplicateStateBlock"));
    assert!(
        check("View Main() { Box #[width: 10, height: 10, a11y_described_by: nothing] }")
            .contains("UnknownStateTarget")
    );
}

const TIPS: &str = r#"
View Main() {
    Row #[gap: 20] {
        Tip("one", "tip-one")
        Tip("two", "tip-two")
    }
}
View Tip(caption: Str, words: Str) {
    var shown = false
    Box #[width: 60, height: 30, bg: 0x2222AA, pointer_enter => shown = true, pointer_leave => shown = false] as target {
        Text(caption)
    }
    when shown {
        Overlay #[modal: false] {
            Box #[anchor_to: target, anchor_edge: below, bg: 0xAA2222, width: 50, height: 20, interactive: false] {
                Text(words)
            }
        }
    }
}
"#;

#[test]
fn two_tooltips_anchor_to_their_own_targets_and_pointer_events_cross_a_wrapper() {
    let mut app = App::new(TIPS);
    app.moved(20.0, 10.0);
    app.send(&[]);
    let t1 = app
        .frame
        .texts()
        .iter()
        .find(|t| t.text == "tip-one")
        .map(|t| t.x);
    assert!(t1.is_some(), "the first tip shows");
    app.moved(300.0, 300.0);
    app.send(&[]);
    assert!(!app.frame.texts().iter().any(|t| t.text == "tip-one"));
    app.moved(100.0, 10.0);
    app.send(&[]);
    let t2 = app
        .frame
        .texts()
        .iter()
        .find(|t| t.text == "tip-two")
        .map(|t| t.x);
    assert!(t2.is_some(), "the second shows");
    assert!(t2.unwrap() > t1.unwrap() + 40.0, "{t1:?} {t2:?}");
}

#[test]
fn interactive_false_lets_a_tap_through() {
    let src = r#"
View Main() {
    var under = 0
    var over = 0
    ZStack {
        Box #[width: 100, height: 100, bg: 0x00FF00, tap => under += 1]
        Box #[width: 100, height: 100, interactive: false, tap => over += 1]
        Text("{under}:{over}")
    }
}
"#;
    let mut app = App::new(src);
    app.tap(50.0, 50.0);
    assert!(
        app.frame.texts().iter().any(|t| t.text == "1:0"),
        "{:?}",
        app.frame
            .texts()
            .iter()
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_wrapper_over_a_slot_hears_the_crossings_once() {
    let src = r#"
View Main() {
    var ins = 0
    var outs = 0
    Column {
        Wrap(ins: ins, outs: outs) {
            Button("Inside") #[width: 60, height: 30] => { }
        }
        Text("{ins}/{outs}")
    }
}
View Wrap(bind ins: Int, bind outs: Int, content: Slot) {
    Box #[p: 20, bg: 0x333333, pointer_enter => ins += 1, pointer_leave => outs += 1] {
        content
    }
}
"#;
    let mut app = App::new(src);
    let shown = |app: &App| app.frame.texts().iter().any(|t| t.text == "2/1");
    app.moved(5.0, 5.0); // the wrapper's padding
    app.moved(40.0, 30.0); // the button inside it
    app.moved(5.0, 5.0); // back on the padding: no crossing
    app.moved(300.0, 300.0); // out
    app.moved(5.0, 5.0); // in again
    assert!(
        shown(&app),
        "{:?}",
        app.frame
            .texts()
            .iter()
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
    );
}
