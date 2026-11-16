//! RFC-0068 §3, §4, §6: `Panel`, `screens`, and the input region, headless. A
//! small compositor stands in for the host: it places the panels the program
//! creates with the same solver `byard shot` uses and tells the program what size
//! each was given.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use byard_compiler::CompileError;
use byard_compiler::infer::check_views;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::surface::Size;
use byard_core::surface::placement::arrange;
use byard_core::surface::{Exclusive, InputRegion, KeyboardMode, Rect};
use byard_core::{
    EventKind, InputEvent, KeyModifiers, ScreenInfo, SurfaceChange, SurfaceId, SurfaceSpec,
};

struct Shell {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frames: Vec<(SurfaceId, RenderFrame)>,
    changes: Vec<SurfaceChange>,
    /// The panels the compositor knows, in creation order.
    known: Vec<(SurfaceId, SurfaceSpec)>,
    regions: Vec<(SurfaceId, InputRegion)>,
    screen: (i32, i32),
    t: u64,
    views: Vec<byard_compiler::parser::ast::ViewDecl>,
}

impl Shell {
    fn new(src: &str) -> Self {
        Self::with_screens(src, vec![ScreenInfo::new("screen-1", 1280.0, 800.0)])
    }

    fn with_screens(src: &str, screens: Vec<ScreenInfo>) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        interp.set_default_namespace("test-app");
        interp.set_screens(screens);
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
            regions: Vec::new(),
            screen: (1280, 800),
            t: 0,
            views: parsed.views,
        };
        s.settle();
        s
    }

    /// Hot-reloads the program to `src`, as `byard dev` does.
    fn reload(&mut self, src: &str) {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        if let Some(tree) = byard_compiler::interp::reload::apply_program(
            &mut self.interp,
            &self.views,
            &parsed.views,
        ) {
            self.tree = tree;
        }
        self.views = parsed.views;
        self.changes.clear();
        self.settle();
    }

    fn step(&mut self, events: &[InputEvent]) {
        self.t += 16;
        self.interp.set_now_ms(u32::try_from(self.t).unwrap());
        self.interp.dispatch_events(events);
        self.interp.tick();
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
                    self.regions.retain(|(i, _)| i != id);
                }
                SurfaceChange::InputRegion { id, region } => {
                    self.regions.retain(|(i, _)| i != id);
                    self.regions.push((*id, region.clone()));
                }
            }
            self.changes.push(change);
        }
        self.compose();
    }

    /// What the compositor does: place every panel and say what size it got.
    fn compose(&mut self) {
        let layers: Vec<_> = self
            .known
            .iter()
            .map(|(_, spec)| {
                let size = Size::new(spec.size.0 as i32, spec.size.1 as i32);
                spec.layer.as_ref().unwrap().layer_surface(size)
            })
            .collect();
        let placed = arrange(Size::new(self.screen.0, self.screen.1), &layers).unwrap();
        for ((id, _), p) in self.known.iter().zip(&placed.surfaces) {
            self.interp
                .set_surface_size(*id, (p.configure.width as f32, p.configure.height as f32));
        }
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.step(&[]);
        }
    }

    fn panel(&self, name: &str) -> SurfaceId {
        self.interp
            .surface_ids()
            .into_iter()
            .find(|id| self.interp.surface_spec(*id).unwrap().name == name)
            .unwrap_or_else(|| panic!("no panel {name:?}"))
    }

    fn spec(&self, name: &str) -> SurfaceSpec {
        self.interp.surface_spec(self.panel(name)).unwrap().clone()
    }

    fn size(&self, name: &str) -> (f32, f32) {
        self.interp.surface_size(self.panel(name)).unwrap()
    }

    fn paints(&self, name: &str) -> u64 {
        self.interp.surface_paints(self.panel(name))
    }

    fn texts(&self, name: &str) -> Vec<String> {
        let id = self.panel(name);
        let (_, f) = self.frames.iter().find(|(i, _)| *i == id).unwrap();
        f.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn region(&self, name: &str) -> Option<InputRegion> {
        let id = self.panel(name);
        self.regions
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, r)| r.clone())
    }

    fn region_changes(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| matches!(c, SurfaceChange::InputRegion { .. }))
            .count()
    }

    fn configures(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| matches!(c, SurfaceChange::Configure { .. }))
            .count()
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, value);
        self.settle();
    }

    fn set_screens(&mut self, screens: Vec<ScreenInfo>) {
        self.interp.set_screens(screens);
        self.settle();
    }

    fn tap(&mut self, name: &str, x: f32, y: f32) {
        let surface = self.panel(name);
        let ev = |kind, t| InputEvent {
            surface,
            kind,
            pos: (x, y),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
            mods: KeyModifiers::default(),
        };
        let (down, up) = (
            ev(EventKind::PointerDown, self.t + 5),
            ev(EventKind::PointerUp, self.t + 10),
        );
        self.step(&[down, up]);
        self.settle();
    }
}

fn errors_of(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    check_views(&parsed.views).errors
}

/// The errors lowering the program finds in its attributes.
fn lowered_kinds(src: &str) -> Vec<&'static str> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.enable_surfaces();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let main = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == "Main")
        .unwrap();
    let _ = interp.lower_view(main, &known);
    interp.errors().iter().map(CompileError::kind).collect()
}

fn kinds(errors: &[CompileError]) -> Vec<&'static str> {
    errors.iter().map(CompileError::kind).collect()
}

fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x, y, w, h)
}

const BAR: &str = r#"
View Main() {
    Panel #[edge: top, namespace: "bar"] {
        Text("Hello from the bar") #[p: 8]
    }
}
"#;

// ── the checker ─────────────────────────────────────────────────────────

#[test]
fn the_smallest_bar_checks_clean_and_is_a_panel_program() {
    let parsed = parse(BAR);
    let inf = check_views(&parsed.views);
    assert!(inf.errors.is_empty(), "{:?}", inf.errors);
    assert!(inf.surfaces.has_panel);
    assert!(!inf.surfaces.has_window);
    assert!(inf.surfaces.is_multi());
}

#[test]
fn edge_with_anchor_is_an_invalid_combination() {
    let errors =
        lowered_kinds(r#"View Main() { Panel #[edge: top, anchor: (top, left)] { Text("x") } }"#);
    assert_eq!(errors, ["InvalidAttributeCombination"]);
    let fine = lowered_kinds(r#"View Main() { Panel #[anchor: (top, left)] { Text("x") } }"#);
    assert!(fine.is_empty(), "{fine:?}");
}

#[test]
fn exclusive_true_without_an_edge_needs_one() {
    let errors =
        errors_of(r#"View Main() { Panel #[anchor: (top, left), exclusive: true] { Text("x") } }"#);
    assert_eq!(kinds(&errors), ["ExclusiveNeedsEdge"], "{errors:?}");
    // With an edge, or three anchors, or a number, it means something.
    for ok in [
        r#"Panel #[edge: top, exclusive: true] { Text("x") }"#,
        r#"Panel #[anchor: (top, left, right), exclusive: true] { Text("x") }"#,
        r#"Panel #[anchor: top, exclusive: 30] { Text("x") }"#,
        r#"Panel #[anchor: top, exclusive: ignore] { Text("x") }"#,
    ] {
        let errors = errors_of(&format!("View Main() {{ {ok} }}"));
        assert!(errors.is_empty(), "{ok}: {errors:?}");
    }
}

#[test]
fn a_text_field_or_key_handler_in_a_panel_without_the_keyboard_is_a_warning() {
    let field = errors_of(
        r#"
View Main() {
    var q = ""
    Panel #[edge: top] { TextField #[bind: q] }
}
"#,
    );
    assert_eq!(kinds(&field), ["PanelTakesNoKeys"], "{field:?}");
    assert!(field[0].is_warning());
    assert!(field[0].headline().contains("on_demand"));
    let key = errors_of(
        r#"
View Main() {
    Panel #[edge: top] { Box #[key_down => ()] { Text("x") } }
}
"#,
    );
    assert_eq!(kinds(&key), ["PanelTakesNoKeys"], "{key:?}");
    // Through a view the panel calls, too.
    let through = errors_of(
        r#"
View Main() { Panel #[edge: top] { Search() } }
View Search() {
    var q = ""
    TextField #[bind: q]
}
"#,
    );
    assert_eq!(kinds(&through), ["PanelTakesNoKeys"], "{through:?}");
    for kb in ["on_demand", "exclusive"] {
        let ok = errors_of(&format!(
            r#"
View Main() {{
    var q = ""
    Panel #[edge: top, keyboard: {kb}] {{ TextField #[bind: q] }}
}}
"#
        ));
        assert!(ok.is_empty(), "{kb}: {ok:?}");
    }
}

#[test]
fn a_namespace_or_screen_that_reads_a_var_is_a_warning() {
    let errors = errors_of(
        r#"
View Main() {
    var name = "bar"
    Panel #[edge: top, namespace: name] { Text("x") }
}
"#,
    );
    assert_eq!(kinds(&errors), ["RecreatesSurface"], "{errors:?}");
    assert!(errors[0].is_warning());
    // The screen of a `for` is how a panel gets its screen: no warning.
    let per_screen = errors_of(
        r#"
View Main() {
    for s in screens key s.name {
        Panel #[screen: s, edge: top, namespace: "bar"] { Text(s.name) }
    }
}
"#,
    );
    assert!(per_screen.is_empty(), "{per_screen:?}");
}

#[test]
fn a_panel_inside_an_element_is_misplaced_and_loose_content_is_outside() {
    let errors = errors_of(
        r#"
View Main() {
    Panel #[edge: top] { Column { Panel #[edge: left] { Text("x") } } }
}
"#,
    );
    assert_eq!(kinds(&errors), ["MisplacedSurface"], "{errors:?}");
    let loose = errors_of(r#"View Main() { Text("x") Panel #[edge: top] { Text("y") } }"#);
    assert_eq!(kinds(&loose), ["ContentOutsideSurface"], "{loose:?}");
}

#[test]
fn a_panel_for_without_a_key_is_a_warning_and_screens_are_typed() {
    let errors = errors_of(
        r#"
View Main() {
    for s in screens { Panel #[screen: s, edge: top, namespace: "bar"] { Text(s.name) } }
}
"#,
    );
    assert_eq!(kinds(&errors), ["SurfaceForNeedsKey"], "{errors:?}");
    let bad = errors_of(
        r#"
View Main() {
    for s in screens key s.name {
        Panel #[screen: s, edge: top, namespace: "bar"] { Text(s.nme) }
    }
}
"#,
    );
    assert_eq!(kinds(&bad), ["UnknownField"], "{bad:?}");
    let wrong = lowered_kinds(r#"View Main() { Panel #[edge: middle] { Text("x") } }"#);
    assert_eq!(wrong, ["AttributeTypeMismatch"]);
    let wrong = lowered_kinds(r#"View Main() { Panel #[screen: 3] { Text("x") } }"#);
    assert_eq!(wrong, ["AttributeTypeMismatch"]);
}

// ── what a panel is ─────────────────────────────────────────────────────

#[test]
fn an_edge_reserves_by_default_and_an_anchor_does_not() {
    let s = Shell::new(
        r#"
View Main() {
    Panel #[edge: top, namespace: "bar"] { Text("bar") }
    Panel #[anchor: (bottom, left), namespace: "corner"] { Text("corner") }
    Panel #[edge: left, exclusive: false, namespace: "dock"] { Text("dock") }
    Panel #[anchor: top, exclusive: 30, namespace: "pad"] { Text("pad") }
    Panel #[edge: bottom, exclusive: ignore, namespace: "wall", layer: background] { Text("wall") }
}
"#,
    );
    let layer = |n: &str| s.spec(n).layer.unwrap();
    assert_eq!(layer("bar").exclusive, Exclusive::Thickness);
    assert_eq!(layer("corner").exclusive, Exclusive::Off);
    assert_eq!(layer("dock").exclusive, Exclusive::Off);
    assert_eq!(layer("pad").exclusive, Exclusive::Pixels(30));
    assert_eq!(layer("wall").exclusive, Exclusive::Ignore);
    assert_eq!(
        layer("wall").layer,
        byard_core::surface::placement::Layer::Background
    );
    assert_eq!(layer("bar").keyboard, KeyboardMode::None);
}

#[test]
fn a_panel_is_a_layer_surface_with_the_protocol_values() {
    let s = Shell::new(
        r#"
View Main() {
    Panel #[edge: top, height: 40, mt: 6, mx: 8, namespace: "bar", keyboard: on_demand] as top_bar {
        Text("x")
    }
}
"#,
    );
    let spec = s.spec("top_bar");
    assert_eq!(spec.kind, byard_core::SurfaceKind::Panel);
    assert!(spec.transparent && !spec.decorations);
    let layer = spec.layer.unwrap();
    assert_eq!(layer.namespace, "bar");
    assert_eq!(layer.keyboard, KeyboardMode::OnDemand);
    assert_eq!(
        (layer.margin.top, layer.margin.left, layer.margin.right),
        (6, 8, 8)
    );
    // Stretched across the width (0 on the wire), 40 thick.
    assert_eq!(spec.size, (0.0, 40.0));
    // The compositor gave it the screen's width less the margins.
    assert_eq!(s.size("top_bar"), (1264.0, 40.0));
    assert_eq!(s.texts("top_bar"), ["x"]);
}

#[test]
fn the_namespace_defaults_to_the_app_id() {
    let s = Shell::new(r#"View Main() { Panel #[edge: top] { Text("x") } }"#);
    assert_eq!(s.spec("test-app").layer.unwrap().namespace, "test-app");
}

#[test]
fn a_panel_has_no_background_of_its_own() {
    let s = Shell::new(BAR);
    let id = s.panel("bar");
    let (_, frame) = s.frames.iter().find(|(i, _)| *i == id).unwrap();
    assert_eq!(frame.texts().len(), 1);
    assert!(
        frame.instances().is_empty() && frame.decorated().is_empty(),
        "nothing is drawn but the text"
    );
}

#[test]
fn reactive_attributes_configure_and_namespace_recreates() {
    let mut s = Shell::new(
        r#"
View Main() {
    var wide = false
    var ns = "one"
    Panel #[anchor: (top, left), width: 100, height: 30, mt: wide ? 20 : 0, namespace: ns] as p {
        Text("x")
    }
}
"#,
    );
    let first = s.panel("p");
    assert_eq!(s.configures(), 0);
    s.set("wide", Value::Bool(true));
    assert_eq!(s.configures(), 1);
    assert_eq!(s.spec("p").layer.unwrap().margin.top, 20);
    assert_eq!(s.panel("p"), first, "a margin is a configure");
    s.set("ns", Value::Str("two".into()));
    assert_ne!(s.panel("p"), first, "a namespace makes the surface again");
    assert!(
        s.changes
            .iter()
            .any(|c| matches!(c, SurfaceChange::Destroy { id } if *id == first))
    );
    assert_eq!(s.spec("p").layer.unwrap().namespace, "two");
}

// ── size from content ───────────────────────────────────────────────────

#[test]
fn a_panel_is_as_large_as_its_content_and_follows_it() {
    let mut s = Shell::new(
        r#"
View Main() {
    var label = "Hi"
    Panel #[anchor: (top, right), namespace: "island"] {
        Text(label) #[p: 8]
    }
}
"#,
    );
    let (w0, h0) = s.size("island");
    assert!(w0 > 20.0 && h0 > 12.0 && w0 < 100.0, "{w0} x {h0}");
    assert_eq!(s.spec("island").size, (w0, h0));
    let before = s.configures();
    s.set(
        "label",
        Value::Str("A much longer label than before".into()),
    );
    let (w1, h1) = s.size("island");
    assert!(w1 > w0 + 60.0, "grew from {w0} to {w1}");
    assert_eq!(h1, h0, "one line either way");
    assert_eq!(
        s.configures(),
        before + 1,
        "one Configure, with the new size"
    );
    assert_eq!(s.spec("island").size, (w1, h1));
    assert_eq!(s.texts("island"), ["A much longer label than before"]);
}

#[test]
fn a_bar_takes_its_height_from_its_content_and_its_width_from_the_screen() {
    let s = Shell::new(BAR);
    let (w, h) = s.size("bar");
    assert_eq!(w, 1280.0);
    assert!(h > 16.0 && h < 60.0, "{h}");
    assert_eq!(s.spec("bar").size.0, 0.0, "stretched: 0 on the wire");
    assert_eq!(s.spec("bar").size.1, h);
}

#[test]
fn a_written_size_is_not_measured() {
    let s = Shell::new(
        r#"View Main() { Panel #[anchor: (top, left), width: 300, height: 50, namespace: "p"] { Text("x") } }"#,
    );
    assert_eq!(s.size("p"), (300.0, 50.0));
}

#[test]
fn a_still_panel_paints_nothing_and_sends_nothing() {
    let mut s = Shell::new(BAR);
    let (paints, changes) = (s.paints("bar"), s.changes.len());
    assert_eq!(paints, 1);
    for _ in 0..300 {
        s.step(&[]);
    }
    assert_eq!(s.paints("bar"), 1);
    assert_eq!(s.changes.len(), changes);
}

#[test]
fn a_change_one_panel_reads_draws_only_that_panel() {
    let mut s = Shell::new(
        r#"
View Main() {
    var a = 0
    var b = 0
    Panel #[edge: top, namespace: "A"] { Text("a:{a}") }
    Panel #[edge: bottom, namespace: "B"] { Text("b:{b}") }
}
"#,
    );
    assert_eq!((s.paints("A"), s.paints("B")), (1, 1));
    s.set("a", Value::Int(1));
    assert_eq!((s.paints("A"), s.paints("B")), (2, 1));
}

// ── the input region ────────────────────────────────────────────────────

const ISLANDS: &str = r#"
View Main() {
    var n = 0
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Row #[p: 8, gap: 8, align: center] {
            Button("one") #[width: 80, height: 30] => n += 1
            Spacer
            Text("clock {n}")
            Box #[width: 50, height: 30, interactive: false] {
                Button("ghost") #[width: 50, height: 30] => n += 100
            }
        }
    }
}
"#;

#[test]
fn only_what_takes_the_pointer_is_in_the_input_region() {
    let s = Shell::new(ISLANDS);
    let Some(InputRegion::Rects(rects)) = s.region("bar") else {
        panic!("a region was expected, got {:?}", s.region("bar"));
    };
    // The button, and nothing else: not the text, not the spacer, and not the
    // button under `interactive: false`.
    assert_eq!(rects.len(), 1, "{rects:?}");
    let b = rects[0];
    assert_eq!((b.x, b.width), (8, 80));
    assert!(b.y >= 0 && b.height > 0);
}

#[test]
fn the_region_is_sent_when_it_changes_and_not_otherwise() {
    let mut s = Shell::new(
        r#"
View Main() {
    var show = false
    var n = 0
    Panel #[anchor: (top, left), width: 200, height: 60, namespace: "p"] {
        Column {
            Text("n:{n}")
            when show { Box #[width: 60, height: 20, tap => n += 1] { Text("go") } }
        }
    }
}
"#,
    );
    assert_eq!(s.region("p"), Some(InputRegion::Rects(vec![])));
    assert_eq!(s.region_changes(), 1);
    // A repaint with the same interactive bounds says nothing.
    s.set("n", Value::Int(3));
    assert_eq!(s.region_changes(), 1);
    s.set("show", Value::Bool(true));
    assert_eq!(s.region_changes(), 2);
    let Some(InputRegion::Rects(rects)) = s.region("p") else {
        panic!()
    };
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].width, 60);
    assert!(rects[0].height >= 20, "the hit area, at least the box");
    s.set("n", Value::Int(4));
    assert_eq!(s.region_changes(), 2);
}

#[test]
fn input_all_and_none_are_the_whole_panel_and_nowhere() {
    let s = Shell::new(
        r#"
View Main() {
    Panel #[edge: top, input: all, namespace: "a"] { Button("x") => () }
    Panel #[edge: bottom, input: none, namespace: "b"] { Button("x") => () }
}
"#,
    );
    // `all` is the protocol's default: nothing to send.
    assert_eq!(s.region("a"), None);
    assert_eq!(s.region("b"), Some(InputRegion::None));
    assert_eq!(s.region_changes(), 1);
}

#[test]
fn a_modal_scrim_takes_the_pointer_over_the_whole_panel() {
    let s = Shell::new(
        r#"
View Main() {
    var open = true
    Panel #[anchor: (top, left), width: 200, height: 100, namespace: "p"] {
        Text("x")
        when open {
            Overlay #[modal: true, dismiss => open = false] {
                Box #[width: 50, height: 20] { Text("dialog") }
            }
        }
    }
}
"#,
    );
    let Some(InputRegion::Rects(rects)) = s.region("p") else {
        panic!("{:?}", s.region("p"))
    };
    assert!(rects.contains(&r(0, 0, 200, 100)), "{rects:?}");
}

#[test]
fn a_tap_reaches_the_button_of_a_panel() {
    let mut s = Shell::new(ISLANDS);
    s.tap("bar", 20.0, 15.0);
    assert_eq!(s.texts("bar"), ["one", "clock 1", "ghost"]);
}

// ── screens ─────────────────────────────────────────────────────────────

#[test]
fn screens_exist_only_when_a_program_reads_them() {
    let s = Shell::new(BAR);
    assert!(!s.interp.has_screens_signal());
    let s = Shell::new(
        r#"
View Main() {
    for s in screens key s.name { Panel #[screen: s, edge: top, namespace: "bar"] { Text(s.name) } }
}
"#,
    );
    assert!(s.interp.has_screens_signal());
    assert_eq!(s.texts("bar"), ["screen-1"]);
    assert_eq!(
        s.spec("bar").layer.unwrap().screen.as_deref(),
        Some("screen-1")
    );
}

const PER_SCREEN: &str = r#"
View Main() {
    for s in screens key s.name {
        Panel #[screen: s, edge: top, height: 30, namespace: "bar-{s.name}"] {
            Text("{s.name} {s.width}x{s.height} @{s.scale}")
        }
    }
}
"#;

/// The list the Wayland host tells the program arrives as an event, since the
/// interpreter lives on the logic thread and the host does not.
#[test]
fn the_hosts_list_of_screens_arrives_as_an_event_and_makes_the_panels() {
    let mut s = Shell::with_screens(PER_SCREEN, Vec::new());
    assert!(s.interp.surface_ids().is_empty(), "no screens, no panels");
    let list = vec![
        ScreenInfo::new("DP-1", 2560.0, 1440.0),
        ScreenInfo {
            x: 2560.0,
            ..ScreenInfo::new("HDMI-1", 1280.0, 800.0)
        },
    ];
    let event = |screens| InputEvent {
        surface: SurfaceId::MAIN,
        kind: EventKind::Screens,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(byard_core::InputPayload::Screens(screens)),
        time_ms: 0,
        mods: KeyModifiers::default(),
    };
    s.step(&[event(list.clone())]);
    s.settle();
    assert_eq!(s.interp.screens(), list.as_slice());
    assert_eq!(s.interp.surface_ids().len(), 2);
    // One unplugged: the event with the shorter list takes its panel away.
    s.step(&[event(list[..1].to_vec())]);
    s.settle();
    assert_eq!(s.interp.surface_ids().len(), 1);
}

#[test]
fn a_screen_added_before_keeps_the_panel_of_the_one_after() {
    let mut s = Shell::with_screens(
        PER_SCREEN,
        vec![ScreenInfo {
            x: 1920.0,
            ..ScreenInfo::new("HDMI-1", 1280.0, 800.0)
        }],
    );
    let hdmi = s.panel("bar-HDMI-1");
    let paints = s.paints("bar-HDMI-1");
    // A screen plugged in to the left of it: first in the list.
    let mut both = vec![
        ScreenInfo::new("DP-1", 2560.0, 1440.0),
        ScreenInfo {
            x: 1920.0,
            ..ScreenInfo::new("HDMI-1", 1280.0, 800.0)
        },
    ];
    ScreenInfo::sort(&mut both);
    s.set_screens(both);
    assert_eq!(
        s.panel("bar-HDMI-1"),
        hdmi,
        "keyed by name, not by position"
    );
    assert_eq!(
        s.paints("bar-HDMI-1"),
        paints,
        "and nothing about it changed"
    );
    assert_eq!(s.interp.surface_ids().len(), 2);
    assert_eq!(s.texts("bar-DP-1"), ["DP-1 2560x1440 @1"]);
    // Unplugged: its panel goes, the other stays.
    s.set_screens(vec![ScreenInfo {
        x: 1920.0,
        ..ScreenInfo::new("HDMI-1", 1280.0, 800.0)
    }]);
    assert_eq!(s.interp.surface_ids(), vec![hdmi]);
    assert!(
        s.changes
            .iter()
            .any(|c| matches!(c, SurfaceChange::Destroy { .. }))
    );
}

#[test]
fn a_rescaled_screen_keeps_its_panel_and_the_panel_reads_the_new_scale() {
    let mut s = Shell::with_screens(PER_SCREEN, vec![ScreenInfo::new("DP-1", 1280.0, 800.0)]);
    let id = s.panel("bar-DP-1");
    s.set_screens(vec![ScreenInfo {
        scale: 1.5,
        ..ScreenInfo::new("DP-1", 1280.0, 800.0)
    }]);
    assert_eq!(s.panel("bar-DP-1"), id);
    assert_eq!(s.texts("bar-DP-1"), ["DP-1 1280x800 @1.5"]);
}

#[test]
fn a_program_with_no_screens_set_sees_an_empty_list() {
    let s = Shell::with_screens(
        r"View Main() { for s in screens key s.name { Panel #[screen: s, edge: top] { Text(s.name) } } }",
        Vec::new(),
    );
    assert_eq!(s.interp.surface_ids().len(), 0);
}

#[test]
fn a_size_on_a_stretched_axis_is_a_warning() {
    let errors =
        errors_of(r#"View Main() { Panel #[edge: top, width: 300, height: 40] { Text("x") } }"#);
    assert_eq!(kinds(&errors), ["SizeOnStretchedAxis"], "{errors:?}");
    assert!(errors[0].is_warning());
    assert!(errors[0].headline().contains("anchored to left and right"));
    let both = errors_of(
        r#"View Main() { Panel #[anchor: (top, bottom, left), height: 5, width: 5] { Text("x") } }"#,
    );
    assert_eq!(kinds(&both), ["SizeOnStretchedAxis"], "{both:?}");
    let side =
        errors_of(r#"View Main() { Panel #[edge: left, width: 64, height: 5] { Text("x") } }"#);
    assert_eq!(kinds(&side), ["SizeOnStretchedAxis"], "{side:?}");
    for ok in [
        r#"Panel #[edge: top, height: 40] { Text("x") }"#,
        r#"Panel #[anchor: (top, right), width: 300, height: 40] { Text("x") }"#,
        r#"Panel #[width: 300, height: 40] { Text("x") }"#,
    ] {
        let errors = errors_of(&format!("View Main() {{ {ok} }}"));
        assert!(errors.is_empty(), "{ok}: {errors:?}");
    }
}

// ── hot reload ──────────────────────────────────────────────────────────

fn reload_src(height: u32, namespace: &str, label: &str) -> String {
    format!(
        r#"
View Main() {{
    Panel #[anchor: (top, left), width: 200, height: {height}, namespace: "{namespace}"] {{
        Text("{label}")
    }}
}}
"#
    )
}

#[test]
fn a_reload_that_edits_a_panels_content_keeps_its_surface() {
    let mut s = Shell::new(&reload_src(30, "bar", "before"));
    let id = s.panel("bar");
    s.reload(&reload_src(30, "bar", "after"));
    assert_eq!(s.panel("bar"), id);
    assert_eq!(s.texts("bar"), ["after"]);
    assert!(
        !s.changes.iter().any(|c| matches!(
            c,
            SurfaceChange::Create { .. } | SurfaceChange::Destroy { .. }
        )),
        "{:?}",
        s.changes
    );
}

#[test]
fn a_reload_that_changes_a_panels_height_is_a_configure() {
    let mut s = Shell::new(&reload_src(30, "bar", "x"));
    let id = s.panel("bar");
    s.reload(&reload_src(50, "bar", "x"));
    assert_eq!(s.panel("bar"), id);
    assert_eq!(s.size("bar"), (200.0, 50.0));
    let configures: Vec<_> = s
        .changes
        .iter()
        .filter_map(|c| match c {
            SurfaceChange::Configure { id: i, spec } if *i == id => Some(spec.size),
            _ => None,
        })
        .collect();
    assert_eq!(configures, [(200.0, 50.0)], "{:?}", s.changes);
}

#[test]
fn a_reload_that_changes_a_panels_namespace_makes_it_again() {
    let mut s = Shell::new(&reload_src(30, "bar", "x"));
    let id = s.panel("bar");
    s.reload(&reload_src(30, "dock", "x"));
    let new = s.panel("dock");
    assert_ne!(new, id);
    assert!(
        s.changes
            .iter()
            .any(|c| matches!(c, SurfaceChange::Destroy { id: i } if *i == id))
    );
    assert!(
        s.changes
            .iter()
            .any(|c| matches!(c, SurfaceChange::Create { id: i, .. } if *i == new))
    );
    assert_eq!(s.interp.surface_ids(), vec![new]);
}
