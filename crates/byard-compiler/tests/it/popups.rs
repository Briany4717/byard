//! RFC-0068 §7: popups. In a `Panel`, an `Overlay` whose first child has
//! `anchor_to` is a surface of its own, headless. A small compositor stands in
//! for the host: it places the panels, and says what the program does with the
//! popups it is told about.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::surface::Size;
use byard_core::surface::placement::arrange;
use byard_core::surface::popup::{Align, Side};
use byard_core::surface::positioner::{Anchor, ConstraintAdjustment};
use byard_core::{
    EventKind, InputEvent, InputPayload, KeyModifiers, ScreenInfo, SurfaceChange, SurfaceId,
    SurfaceKind, SurfaceSpec,
};

struct Shell {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frames: Vec<(SurfaceId, RenderFrame)>,
    changes: Vec<SurfaceChange>,
    /// The surfaces the compositor knows, in creation order.
    known: Vec<(SurfaceId, SurfaceSpec)>,
    t: u64,
}

impl Shell {
    fn new(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        interp.set_default_namespace("test-app");
        interp.set_default_window_title("test-app");
        interp.set_screens(vec![ScreenInfo::new("screen-1", 1280.0, 800.0)]);
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let main = parsed.views.iter().find(|v| v.name.as_str() == "Main");
        let tree = interp.lower_view(main.unwrap(), &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut s = Self {
            interp,
            tree,
            frames: Vec::new(),
            changes: Vec::new(),
            known: Vec::new(),
            t: 0,
        };
        s.settle();
        s
    }

    fn step(&mut self, events: &[InputEvent]) {
        self.t += 16;
        self.interp.set_now_ms(u32::try_from(self.t).unwrap());
        self.interp.dispatch_events(events);
        self.interp.tick();
        for _ in 0..4 {
            let mut out = Vec::new();
            self.interp.render_surfaces(
                &self.tree,
                &mut || {
                    let mut frame = RenderFrame::new();
                    frame.request_full_redraw();
                    frame
                },
                &mut out,
            );
            for (id, frame) in out {
                match self.frames.iter_mut().find(|(i, _)| *i == id) {
                    Some(slot) => slot.1 = frame,
                    None => self.frames.push((id, frame)),
                }
            }
            for change in self.interp.take_surface_changes() {
                match &change {
                    SurfaceChange::Create { id, spec } => self.known.push((*id, spec.clone())),
                    SurfaceChange::Configure { id, spec } => {
                        if let Some(k) = self.known.iter_mut().find(|(i, _)| i == id) {
                            k.1 = spec.clone();
                        }
                    }
                    SurfaceChange::Destroy { id } => {
                        self.known.retain(|(i, _)| i != id);
                        self.frames.retain(|(i, _)| i != id);
                    }
                    SurfaceChange::InputRegion { .. } => {}
                }
                self.changes.push(change);
            }
            if !self.compose() {
                break;
            }
        }
    }

    /// What the compositor does with the panels: places them and says what size
    /// each got. Popups are the size they asked for. Returns whether a size
    /// moved.
    fn compose(&mut self) -> bool {
        let panels: Vec<(SurfaceId, SurfaceSpec)> = self
            .known
            .iter()
            .filter(|(_, s)| s.layer.is_some())
            .cloned()
            .collect();
        let layers: Vec<_> = panels
            .iter()
            .map(|(_, spec)| {
                let size = Size::new(spec.size.0 as i32, spec.size.1 as i32);
                spec.layer.as_ref().unwrap().layer_surface(size)
            })
            .collect();
        let placed = arrange(Size::new(1280, 800), &layers).unwrap();
        let mut moved = false;
        for ((id, _), p) in panels.iter().zip(&placed.surfaces) {
            let size = (p.configure.width as f32, p.configure.height as f32);
            if self.interp.surface_size(*id) != Some(size) {
                self.interp.set_surface_size(*id, size);
                moved = true;
            }
        }
        moved
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.step(&[]);
        }
    }

    /// Runs `ms` of app time with nothing happening.
    fn wait(&mut self, ms: u64) {
        for _ in 0..ms.div_ceil(16) {
            self.step(&[]);
        }
    }

    fn id_of(&self, name: &str) -> SurfaceId {
        self.interp
            .surface_ids()
            .into_iter()
            .find(|id| self.interp.surface_spec(*id).unwrap().name == name)
            .unwrap_or_else(|| panic!("no surface {name:?}"))
    }

    fn spec_of(&self, id: SurfaceId) -> SurfaceSpec {
        self.interp.surface_spec(id).unwrap().clone()
    }

    fn popups(&self) -> Vec<(SurfaceId, SurfaceSpec)> {
        self.interp
            .surface_ids()
            .into_iter()
            .map(|id| (id, self.spec_of(id)))
            .filter(|(_, s)| s.kind == SurfaceKind::Popup)
            .collect()
    }

    fn assert_no_popups(&self) {
        assert_eq!(self.popups().len(), 0, "{:?}", self.popups());
    }

    fn only_popup(&self) -> (SurfaceId, SurfaceSpec) {
        let mut all = self.popups();
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }

    fn texts(&self, id: SurfaceId) -> Vec<String> {
        let (_, f) = self.frames.iter().find(|(i, _)| *i == id).unwrap();
        f.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn var(&self, name: &str) -> Value {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.peek(sig)
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, value);
        self.settle();
    }

    fn count(&self, f: impl Fn(&SurfaceChange) -> bool) -> usize {
        self.changes.iter().filter(|c| f(c)).count()
    }

    fn event(surface: SurfaceId, kind: EventKind, x: f32, y: f32, t: u64) -> InputEvent {
        InputEvent {
            surface,
            kind,
            pos: (x, y),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
            mods: KeyModifiers::default(),
        }
    }

    fn tap(&mut self, surface: SurfaceId, x: f32, y: f32) {
        let down = Self::event(surface, EventKind::PointerDown, x, y, self.t + 5);
        let up = Self::event(surface, EventKind::PointerUp, x, y, self.t + 10);
        self.step(&[down, up]);
        self.settle();
    }

    fn key(&mut self, surface: SurfaceId, name: &str) {
        let mut ev = Self::event(surface, EventKind::KeyDown, 0.0, 0.0, self.t + 5);
        ev.payload = Some(InputPayload::Key(name.to_string()));
        let mut up = ev.clone();
        up.kind = EventKind::KeyUp;
        self.step(&[ev, up]);
        self.settle();
    }

    fn done(&mut self, surface: SurfaceId) {
        let ev = Self::event(surface, EventKind::PopupDone, 0.0, 0.0, self.t + 5);
        self.step(&[ev]);
        self.settle();
    }
}

/// A bar with a button that opens a menu below it. The menu is a `menu`, so it
/// is a focus scope.
const MENU: &str = r#"
View Main() {
    var open = false
    var picked = ""
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Row #[pl: 12, align: center] {
            Button("Menu") #[width: 80, height: 30] as menu => open = !open
        }
        when open {
            Overlay #[modal: false] {
                Column #[anchor_to: menu, anchor_edge: below, anchor_gap: 4, width: 160,
                         bg: 0x202830, a11y_role: menu, dismiss => open = false] as pop {
                    Button("Open") #[width: 160, height: 32] => picked = "open"
                    Button("Quit") #[width: 160, height: 32] => picked = "quit"
                }
            }
        }
    }
}
"#;

#[test]
fn an_anchored_overlay_in_a_panel_is_a_popup_made_when_it_mounts_and_destroyed_with_its_when() {
    let mut s = Shell::new(MENU);
    s.assert_no_popups();
    assert_eq!(s.count(|c| matches!(c, SurfaceChange::Create { .. })), 1);

    let bar = s.id_of("bar");
    s.tap(bar, 30.0, 20.0);
    assert_eq!(s.var("open"), Value::Bool(true));
    let (id, spec) = s.only_popup();
    assert_eq!(spec.name, "pop");
    assert_eq!(spec.kind, SurfaceKind::Popup);
    assert!(spec.transparent && !spec.decorations && !spec.resizable);
    assert_eq!(spec.popup.unwrap().parent, bar);
    // One Create for the bar, one for the popup, in that order.
    let created: Vec<_> = s
        .changes
        .iter()
        .filter_map(|c| match c {
            SurfaceChange::Create { id, spec } => Some((*id, spec.kind)),
            _ => None,
        })
        .collect();
    assert_eq!(
        created,
        [(bar, SurfaceKind::Panel), (id, SurfaceKind::Popup)]
    );
    assert_eq!(s.texts(id), ["Open", "Quit"]);

    s.set("open", Value::Bool(false));
    s.assert_no_popups();
    assert_eq!(
        s.count(|c| matches!(c, SurfaceChange::Destroy { id: gone } if *gone == id)),
        1
    );
    // And it opens again, as a new surface.
    s.set("open", Value::Bool(true));
    let (again, _) = s.only_popup();
    assert_ne!(again, id, "an id is not used twice");
}

#[test]
fn the_positioner_data_is_what_the_attributes_say() {
    let s = {
        let mut s = Shell::new(MENU);
        let bar = s.id_of("bar");
        s.tap(bar, 30.0, 20.0);
        s
    };
    let (_, spec) = s.only_popup();
    let popup = spec.popup.unwrap();
    // The anchor rectangle is the button's bounds in the bar: it starts at the
    // row's padding, and a button is at least 44 high where it takes a press.
    let a = popup.anchor_rect;
    assert_eq!((a.x, a.width), (12, 80), "{a:?}");
    assert!(a.y >= 0 && a.height >= 30 && a.y + a.height <= 40, "{a:?}");
    // `below`, `start`: anchor bottom-left, gravity bottom-right; the gap is the
    // offset; flip on the vertical axis and slide on both.
    assert_eq!(popup.anchor, Anchor::BottomLeft);
    assert_eq!(popup.gravity, Anchor::BottomRight);
    assert_eq!(popup.offset, (0, 4));
    assert!(popup.adjustment.contains(ConstraintAdjustment::FLIP_Y));
    assert!(popup.adjustment.contains(ConstraintAdjustment::SLIDE_X));
    assert!(popup.adjustment.contains(ConstraintAdjustment::SLIDE_Y));
    assert!(!popup.adjustment.contains(ConstraintAdjustment::FLIP_X));
    // Its size is what its content measured: two 32 high buttons, 160 wide.
    assert_eq!(spec.size, (160.0, 64.0));
    // The popup spec says the same as the helper does for these attributes.
    assert_eq!(
        popup,
        byard_core::surface::PopupSpec::anchored(
            popup.parent,
            a,
            Side::Below,
            Align::Start,
            4,
            true,
            popup.grab
        )
    );
}

#[test]
fn the_other_edges_and_alignments_map_to_the_positioner() {
    let src = MENU.replace(
        "anchor_edge: below",
        "anchor_edge: above, anchor_align: end, anchor_flip: false",
    );
    let mut s = Shell::new(&src);
    let bar = s.id_of("bar");
    s.tap(bar, 30.0, 20.0);
    let popup = s.only_popup().1.popup.unwrap();
    assert_eq!(popup.anchor, Anchor::TopRight);
    assert_eq!(popup.gravity, Anchor::TopLeft);
    assert_eq!(popup.offset, (0, -4));
    assert!(!popup.adjustment.contains(ConstraintAdjustment::FLIP_Y));
    assert!(popup.adjustment.contains(ConstraintAdjustment::SLIDE_Y));
}

#[test]
fn a_menu_has_the_grab_and_a_tooltip_does_not() {
    let mut s = Shell::new(MENU);
    let bar = s.id_of("bar");
    s.tap(bar, 30.0, 20.0);
    let (id, spec) = s.only_popup();
    assert!(spec.popup.unwrap().grab, "a menu is a focus scope");
    assert_eq!(s.interp.grab_surface(), Some(id));

    let tip = r#"
View Main() {
    var open = true
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Box #[width: 60, height: 20] as label {}
        when open {
            Overlay #[modal: false] {
                Box #[anchor_to: label, anchor_edge: below, bg: 0x222222, p: 6] { Text("a tip") }
            }
        }
    }
}
"#;
    let s = Shell::new(tip);
    let (_, spec) = s.only_popup();
    assert!(!spec.popup.unwrap().grab, "a tooltip is not a focus scope");
    assert_eq!(s.interp.grab_surface(), None);
    assert_eq!(
        spec.name, "popup",
        "a popup with no `as` name is called popup"
    );

    // A modal overlay is a scope too, and so is one holding a dialog.
    let modal = tip.replace("modal: false", "modal: true");
    assert!(Shell::new(&modal).only_popup().1.popup.unwrap().grab);
    let dialog = tip.replace("Box #[anchor_to", "Box #[a11y_role: dialog, anchor_to");
    assert!(Shell::new(&dialog).only_popup().1.popup.unwrap().grab);
}

#[test]
fn popup_done_runs_the_overlays_dismiss() {
    let mut s = Shell::new(MENU);
    let bar = s.id_of("bar");
    s.tap(bar, 30.0, 20.0);
    let (id, _) = s.only_popup();
    assert_eq!(s.var("open"), Value::Bool(true));
    s.done(id);
    assert_eq!(
        s.var("open"),
        Value::Bool(false),
        "dismiss => open = false ran"
    );
    s.assert_no_popups();
}

#[test]
fn escape_in_a_popup_with_the_grab_dismisses_it() {
    let mut s = Shell::new(MENU);
    let bar = s.id_of("bar");
    s.tap(bar, 30.0, 20.0);
    let (id, _) = s.only_popup();
    s.key(id, "Escape");
    assert_eq!(s.var("open"), Value::Bool(false));
    s.assert_no_popups();
}

#[test]
fn a_tap_in_a_popup_reaches_its_own_button() {
    let mut s = Shell::new(MENU);
    let bar = s.id_of("bar");
    s.tap(bar, 30.0, 20.0);
    let (id, _) = s.only_popup();
    s.tap(id, 20.0, 48.0);
    assert_eq!(s.var("picked"), Value::Str("quit".into()));
    s.tap(id, 20.0, 10.0);
    assert_eq!(s.var("picked"), Value::Str("open".into()));
}

#[test]
fn a_popup_with_the_grab_takes_focus_and_the_panel_keeps_its_own() {
    let mut s = Shell::new(MENU);
    let bar = s.id_of("bar");
    assert_eq!(s.interp.focused_element_of(bar), None);
    s.tap(bar, 30.0, 20.0);
    let (id, _) = s.only_popup();
    assert!(
        s.interp.focused_element_of(id).is_some(),
        "focus goes into the menu"
    );
    s.done(id);
    s.assert_no_popups();
    assert_eq!(s.interp.grab_surface(), None, "the grab is given back");
}

const SUBMENU: &str = r#"
View Main() {
    var open = true
    var more = true
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Row #[pl: 12, align: center] {
            Button("Menu") #[width: 80, height: 30] as menu => open = !open
        }
        when open {
            Overlay #[modal: false] {
                Column #[anchor_to: menu, anchor_edge: below, width: 160, a11y_role: menu,
                         dismiss => open = false] as first {
                    Button("Share") #[width: 160, height: 32] as share => more = !more
                    Button("Quit") #[width: 160, height: 32]
                    when more {
                        Overlay #[modal: false] {
                            Column #[anchor_to: share, anchor_edge: after, width: 120,
                                     a11y_role: menu, dismiss => more = false] as second {
                                Button("Mail") #[width: 120, height: 32]
                            }
                        }
                    }
                }
            }
        }
    }
}
"#;

#[test]
fn a_submenu_is_a_popup_whose_parent_is_the_popup() {
    let mut s = Shell::new(SUBMENU);
    let bar = s.id_of("bar");
    let first = s.id_of("first");
    let second = s.id_of("second");
    assert_eq!(s.spec_of(first).popup.unwrap().parent, bar);
    assert_eq!(s.spec_of(second).popup.unwrap().parent, first);
    // The submenu opens to the right of the item it hangs from, in the parent
    // popup's coordinates.
    let sub = s.spec_of(second).popup.unwrap();
    assert_eq!(
        (sub.anchor, sub.gravity),
        (Anchor::TopRight, Anchor::BottomRight)
    );
    assert_eq!(sub.anchor_rect.x, 0);
    assert_eq!(sub.anchor_rect.width, 160);
    assert_eq!(s.spec_of(second).size, (120.0, 32.0));
    // Created in order: the bar, the menu, then the submenu.
    let order: Vec<SurfaceId> = s
        .changes
        .iter()
        .filter_map(|c| match c {
            SurfaceChange::Create { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(order, [bar, first, second]);
    // The innermost menu has the grab.
    assert_eq!(s.interp.grab_surface(), Some(second));

    // Closing the submenu leaves the menu; closing the menu takes both.
    s.set("more", Value::Bool(false));
    assert_eq!(s.popups().len(), 1);
    s.set("more", Value::Bool(true));
    assert_eq!(s.popups().len(), 2);
    let (first, second) = (s.id_of("first"), s.id_of("second"));
    s.set("open", Value::Bool(false));
    s.assert_no_popups();
    let gone: Vec<SurfaceId> = s
        .changes
        .iter()
        .filter_map(|c| match c {
            SurfaceChange::Destroy { id } => Some(*id),
            _ => None,
        })
        .collect();
    let n = gone.len();
    assert_eq!(
        gone[n - 2..],
        [second, first],
        "the submenu goes before its menu"
    );
}

#[test]
fn popup_done_of_a_submenu_dismisses_the_submenu_only() {
    let mut s = Shell::new(SUBMENU);
    let second = s.id_of("second");
    s.done(second);
    assert_eq!(s.var("more"), Value::Bool(false));
    assert_eq!(s.var("open"), Value::Bool(true));
    assert_eq!(s.popups().len(), 1);
}

#[test]
fn an_overlay_in_a_panel_with_no_anchor_stays_inside_the_panel() {
    let src = r#"
View Main() {
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Text("bar")
        Overlay #[modal: false] {
            Box #[anchor: center, bg: 0x333333, p: 4] { Text("inside") }
        }
    }
}
"#;
    let s = Shell::new(src);
    s.assert_no_popups();
    assert!(s.texts(s.id_of("bar")).contains(&"inside".to_string()));
}

#[test]
fn an_anchored_overlay_in_a_window_is_drawn_inside_it_as_before() {
    let src = r#"
View Main() {
    Window #[title: "app", width: 400, height: 300] {
        Column #[p: 20] {
            Button("Menu") #[width: 80, height: 30] as menu
        }
        Overlay #[modal: false] {
            Box #[anchor_to: menu, anchor_edge: below, bg: 0x202830, p: 6] { Text("in the window") }
        }
    }
}
"#;
    let s = Shell::new(src);
    s.assert_no_popups();
    assert_eq!(s.interp.surface_ids().len(), 1);
    let w = s.id_of("app");
    assert!(s.texts(w).contains(&"in the window".to_string()));
}

#[test]
fn the_popup_is_configured_when_its_content_changes_size() {
    let src = r#"
View Main() {
    var rows = ["a", "b"]
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Box #[width: 60, height: 20] as label {}
        Overlay #[modal: false] {
            Column #[anchor_to: label, width: 100] {
                for row in rows key row { Box #[height: 20] { Text(row) } }
            }
        }
    }
}
"#;
    let mut s = Shell::new(src);
    assert_eq!(s.only_popup().1.size, (100.0, 40.0));
    let before = s.count(|c| matches!(c, SurfaceChange::Configure { .. }));
    s.set(
        "rows",
        Value::List(
            ["a", "b", "c", "d", "e"]
                .map(|t| Value::Str(t.into()))
                .to_vec(),
        ),
    );
    let (_, spec) = s.only_popup();
    assert_eq!(spec.size, (100.0, 100.0));
    assert_eq!(
        s.count(|c| matches!(c, SurfaceChange::Configure { .. })),
        before + 1,
        "one Configure for the new size"
    );
}

#[test]
fn the_popup_is_configured_when_its_anchor_moves() {
    let src = r#"
View Main() {
    var pad = 10
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Row #[align: center] {
            Box #[width: pad, height: 10] {}
            Box #[width: 60, height: 20] as label {}
        }
        Overlay #[modal: false] {
            Box #[anchor_to: label, width: 50, height: 20, bg: 0x333333] {}
        }
    }
}
"#;
    let mut s = Shell::new(src);
    let x0 = s.only_popup().1.popup.unwrap().anchor_rect.x;
    assert_eq!(x0, 10);
    s.set("pad", Value::Int(60));
    let x1 = s.only_popup().1.popup.unwrap().anchor_rect.x;
    assert_eq!(x1, 60);
}

#[test]
fn a_still_popup_paints_nothing_and_a_change_it_reads_draws_only_it() {
    let src = r#"
View Main() {
    var n = 0
    var m = 0
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Box #[width: 60, height: 20] as label { Text("bar {m}") }
        Overlay #[modal: false] {
            Box #[anchor_to: label, bg: 0x333333, p: 4] { Text("n is {n}") }
        }
    }
}
"#;
    let mut s = Shell::new(src);
    let (id, _) = s.only_popup();
    let bar = s.id_of("bar");
    let (pb, pp) = (s.interp.surface_paints(bar), s.interp.surface_paints(id));
    s.wait(1000);
    assert_eq!(s.interp.surface_paints(bar), pb, "a still bar is not drawn");
    assert_eq!(
        s.interp.surface_paints(id),
        pp,
        "a still popup is not drawn"
    );
    s.set("n", Value::Int(1));
    assert_eq!(s.interp.surface_paints(bar), pb, "the bar did not read n");
    assert_eq!(s.interp.surface_paints(id), pp + 1);
    assert_eq!(s.texts(id), ["n is 1"]);
    s.set("m", Value::Int(1));
    assert_eq!(
        s.interp.surface_paints(id),
        pp + 1,
        "the popup did not read m"
    );
}

#[test]
fn a_popup_is_destroyed_after_its_leave_motion_settles() {
    let src = r#"
View Main() {
    var open = true
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Box #[width: 60, height: 20] as label {}
        when open {
            Overlay #[modal: false] {
                Box #[anchor_to: label, width: 80, height: 30, bg: 0x333333,
                      opacity: 1.0 with anim.linear(200ms)] {
                    on leave { opacity: 0.0 }
                    Text("bye")
                }
            }
        }
    }
}
"#;
    let mut s = Shell::new(src);
    let (id, _) = s.only_popup();
    s.set("open", Value::Bool(false));
    // It is still open while its motion plays...
    assert_eq!(s.popups().len(), 1, "the popup outlives `open`");
    assert_eq!(
        s.count(|c| matches!(c, SurfaceChange::Destroy { id: g } if *g == id)),
        0
    );
    // ...inert: it does not hold the grab any more...
    assert_eq!(s.interp.grab_surface(), None);
    // ...and goes when it is done.
    s.wait(600);
    s.assert_no_popups();
    assert_eq!(
        s.count(|c| matches!(c, SurfaceChange::Destroy { id: g } if *g == id)),
        1
    );
}

#[test]
fn a_popup_whose_panel_goes_goes_first() {
    let src = r#"
View Main() {
    var shown = true
    when shown {
        Panel #[edge: top, height: 40, namespace: "bar"] {
            Box #[width: 60, height: 20] as label {}
            Overlay #[modal: false] {
                Box #[anchor_to: label, width: 80, height: 30, bg: 0x333333] {}
            }
        }
    }
}
"#;
    let mut s = Shell::new(src);
    let (id, _) = s.only_popup();
    let bar = s.id_of("bar");
    s.set("shown", Value::Bool(false));
    let gone: Vec<SurfaceId> = s
        .changes
        .iter()
        .filter_map(|c| match c {
            SurfaceChange::Destroy { id } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(gone, [id, bar], "the popup is destroyed before its parent");
    assert_eq!(s.interp.surface_ids().len(), 0);
}

#[test]
fn closing_a_popup_does_not_disturb_the_surfaces_made_after_it() {
    let src = r#"
View Main() {
    var open = true
    var n = 0
    Panel #[edge: top, height: 40, namespace: "first"] {
        Box #[width: 60, height: 20] as label {}
        when open {
            Overlay #[modal: false] {
                Box #[anchor_to: label, width: 80, height: 30, bg: 0x333333] {}
            }
        }
    }
    Panel #[edge: bottom, height: 40, namespace: "second"] { Text("n {n}") }
}
"#;
    let mut s = Shell::new(src);
    s.set("open", Value::Bool(false));
    s.set("n", Value::Int(3));
    assert_eq!(s.texts(s.id_of("second")), ["n 3"]);
    s.assert_no_popups();
}
