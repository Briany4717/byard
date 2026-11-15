//! RFC-0068 §1, §2, §5, §6: `Window`, the surface level and the checker rules
//! around it, and a program that draws several surfaces from one interpreter.

use byard_compiler::CompileError;
use byard_compiler::infer::check_views;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, KeyModifiers, SurfaceChange, SurfaceId};

/// A program of windows, run headlessly one tick at a time.
struct Windows {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    /// The last frame each surface was drawn into.
    frames: Vec<(SurfaceId, RenderFrame)>,
    /// Every change the program asked of the platform, in order.
    changes: Vec<SurfaceChange>,
    t: u64,
}

impl Windows {
    fn new(src: &str) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let main = parsed.views.iter().find(|v| v.name.as_str() == "Main");
        let tree = interp.lower_view(main.unwrap(), &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut w = Self {
            interp,
            tree,
            frames: Vec::new(),
            changes: Vec::new(),
            t: 0,
        };
        w.step(&[]);
        w
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
        self.frames
            .retain(|(id, _)| self.interp.surface_ids().contains(id));
        self.changes.extend(self.interp.take_surface_changes());
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.step(&[]);
        }
    }

    /// The surface whose title is `title`.
    fn surface(&self, title: &str) -> SurfaceId {
        self.interp
            .surface_ids()
            .into_iter()
            .find(|id| self.interp.surface_spec(*id).unwrap().title == title)
            .unwrap_or_else(|| panic!("no window titled {title:?}"))
    }

    fn texts(&self, title: &str) -> Vec<String> {
        let id = self.surface(title);
        let (_, frame) = self.frames.iter().find(|(i, _)| *i == id).unwrap();
        frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn event(&mut self, surface: SurfaceId, kind: EventKind, pos: (f32, f32)) -> InputEvent {
        self.t += 10;
        InputEvent {
            surface,
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: self.t,
            mods: KeyModifiers::default(),
        }
    }

    fn tap(&mut self, title: &str, x: f32, y: f32) {
        let id = self.surface(title);
        let down = self.event(id, EventKind::PointerDown, (x, y));
        let up = self.event(id, EventKind::PointerUp, (x, y));
        self.step(&[down, up]);
        self.settle();
    }

    fn key(&mut self, title: &str, name: &str) {
        let id = self.surface(title);
        let mut down = self.event(id, EventKind::KeyDown, (0.0, 0.0));
        down.payload = Some(byard_core::InputPayload::Key(name.to_string()));
        let mut up = down.clone();
        up.kind = EventKind::KeyUp;
        self.step(&[down, up]);
        self.settle();
    }

    fn close(&mut self, title: &str) {
        let id = self.surface(title);
        let ev = self.event(id, EventKind::CloseRequest, (0.0, 0.0));
        self.step(&[ev]);
        self.settle();
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.write_var(sig, value);
        self.step(&[]);
    }

    fn paints(&self, title: &str) -> u64 {
        self.interp.surface_paints(self.surface(title))
    }

    fn created(&self) -> Vec<(SurfaceId, String)> {
        self.changes
            .iter()
            .filter_map(|c| match c {
                SurfaceChange::Create { id, spec } => Some((*id, spec.title.clone())),
                _ => None,
            })
            .collect()
    }

    fn destroyed(&self) -> Vec<SurfaceId> {
        self.changes
            .iter()
            .filter_map(|c| match c {
                SurfaceChange::Destroy { id } => Some(*id),
                _ => None,
            })
            .collect()
    }
}

fn errors_of(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    check_views(&parsed.views).errors
}

fn kinds(errors: &[CompileError]) -> Vec<&'static str> {
    errors.iter().map(CompileError::kind).collect()
}

const TWO: &str = r#"
View Main() {
    var count = 0
    Window #[title: "A", width: 300, height: 200] {
        Column {
            Text("a:{count}") #[height: 30]
            Button("inc") #[width: 100, height: 40] => count += 1
        }
    }
    Window #[title: "B", width: 320, height: 240] {
        Text("b:{count}")
    }
}
"#;

// ── the checker ─────────────────────────────────────────────────────────

#[test]
fn a_window_inside_an_element_is_misplaced() {
    let errors = errors_of(
        r#"
View Main() {
    Window #[title: "A"] {
        Column {
            Window #[title: "B"] { Text("x") }
        }
    }
}
"#,
    );
    assert_eq!(kinds(&errors), ["MisplacedSurface"], "{errors:?}");
    let hint = errors[0].headline();
    assert!(hint.contains("Overlay"), "{hint}");
}

#[test]
fn a_window_in_a_program_that_is_a_single_window_is_misplaced_too() {
    // No window at the surface level, so the program is the implicit window,
    // and a `Window` inside its content has nowhere to be.
    let errors = errors_of(
        r#"
View Main() {
    Column {
        Window #[title: "B"] { Text("x") }
    }
}
"#,
    );
    assert_eq!(kinds(&errors), ["MisplacedSurface"], "{errors:?}");
}

#[test]
fn content_beside_the_windows_is_outside_every_window() {
    let errors = errors_of(
        r#"
View Main() {
    Text("loose")
    Window #[title: "A"] { Text("x") }
}
"#,
    );
    assert_eq!(kinds(&errors), ["ContentOutsideSurface"], "{errors:?}");
    assert!(errors[0].headline().contains("`Text`"));
}

#[test]
fn windows_in_when_for_and_views_are_at_the_surface_level() {
    let errors = errors_of(
        r#"
View Main() {
    var open = true
    var names = ["a"]
    when open {
        Settings()
    }
    for n in names key n {
        Window #[title: n] { Text(n) }
    }
}
View Settings() {
    Window #[title: "Settings"] { Text("x") }
}
"#,
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_view_called_at_the_surface_level_with_loose_content_is_reported() {
    let errors = errors_of(
        r#"
View Main() {
    Window #[title: "A"] { Text("x") }
    Footer()
}
View Footer() {
    Text("footer")
}
"#,
    );
    assert_eq!(kinds(&errors), ["ContentOutsideSurface"], "{errors:?}");
}

#[test]
fn an_opening_size_that_reads_a_var_is_a_warning() {
    let errors = errors_of(
        r#"
View Main() {
    var w = 400
    Window #[title: "A", width: w, height: 200] { Text("x") }
}
"#,
    );
    assert_eq!(kinds(&errors), ["OpeningSizeIsStatic"], "{errors:?}");
    assert!(errors[0].is_warning());
    // A literal is the size it opens at, and says nothing.
    let literal = errors_of(r#"View Main() { Window #[width: 400] { Text("x") } }"#);
    assert!(literal.is_empty(), "{literal:?}");
}

#[test]
fn a_for_of_windows_without_a_key_is_a_warning() {
    let errors = errors_of(
        r#"
View Main() {
    var names = ["a", "b"]
    for n in names {
        Window #[title: n] { Text(n) }
    }
}
"#,
    );
    assert_eq!(kinds(&errors), ["SurfaceForNeedsKey"], "{errors:?}");
    assert!(errors[0].is_warning());
    assert!(errors[0].headline().contains("key"));
}

#[test]
fn the_checker_says_whether_a_program_has_windows() {
    let has = |src: &str| {
        let parsed = parse(src);
        check_views(&parsed.views).surfaces
    };
    assert!(!has(r#"View Main() { Text("x") }"#).has_window);
    let two = has(TWO);
    assert!(two.has_window && two.is_multi());
    assert!(!two.has_panel);
}

#[test]
fn window_attributes_are_checked() {
    // Attributes are validated when the element is lowered, as every element's.
    let parsed = parse(
        r#"
View Main() {
    Window #[title: 3, wide: 4] { Text("x") }
}
"#,
    );
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known = ["Main"];
    interp.lower_view(&parsed.views[0], &known);
    let found: Vec<&str> = interp.errors().iter().map(CompileError::kind).collect();
    assert!(
        found.contains(&"AttributeTypeMismatch") && found.contains(&"UnknownAttribute"),
        "{found:?}"
    );
}

// ── the interpreter ─────────────────────────────────────────────────────

#[test]
fn each_window_is_a_surface_with_its_own_spec() {
    let w = Windows::new(TWO);
    assert_eq!(w.interp.surface_ids().len(), 2);
    let a = w.interp.surface_spec(w.surface("A")).unwrap();
    assert_eq!(a.size, (300.0, 200.0));
    let b = w.interp.surface_spec(w.surface("B")).unwrap();
    assert_eq!(b.size, (320.0, 240.0));
    // No window is `MAIN`: the ids start at 1.
    assert!(w.interp.surface_ids().iter().all(|id| !id.is_main()));
    assert_eq!(
        w.created(),
        [
            (SurfaceId::new(1), "A".to_string()),
            (SurfaceId::new(2), "B".to_string())
        ]
    );
}

#[test]
fn two_windows_share_a_var() {
    let mut w = Windows::new(TWO);
    assert_eq!(w.texts("A"), ["a:0", "inc"]);
    assert_eq!(w.texts("B"), ["b:0"]);
    // A tap in A changes the text in B.
    w.tap("A", 50.0, 50.0);
    assert_eq!(w.texts("B"), ["b:1"]);
    assert_eq!(w.texts("A"), ["a:1", "inc"]);
    w.tap("A", 50.0, 50.0);
    assert_eq!(w.texts("B"), ["b:2"]);
}

#[test]
fn a_tap_reaches_the_surface_it_names_and_no_other() {
    let mut w = Windows::new(TWO);
    // B has no button: the same point on B does nothing.
    w.tap("B", 50.0, 50.0);
    assert_eq!(w.texts("A"), ["a:0", "inc"]);
}

const TOGGLE: &str = r#"
View Main() {
    var settings = false
    Window #[title: "Notes", width: 400, height: 300] {
        Column {
            Text("notes") #[height: 30]
            Button("settings") #[width: 100, height: 40] => settings = !settings
        }
    }
    when settings {
        Window #[title: "Settings", width: 200, height: 200, close => settings = false] {
            Text("settings page")
        }
    }
}
"#;

#[test]
fn a_when_opens_and_closes_a_window_and_ids_are_not_reused() {
    let mut w = Windows::new(TOGGLE);
    assert_eq!(w.interp.surface_ids().len(), 1);
    w.tap("Notes", 50.0, 50.0);
    assert_eq!(w.interp.surface_ids().len(), 2);
    let settings = w.surface("Settings");
    assert_eq!(settings, SurfaceId::new(2));
    assert_eq!(w.texts("Settings"), ["settings page"]);
    w.tap("Notes", 50.0, 50.0);
    assert_eq!(w.interp.surface_ids().len(), 1);
    assert_eq!(w.destroyed(), [SurfaceId::new(2)]);
    // Open again: a new surface, a new id.
    w.tap("Notes", 50.0, 50.0);
    assert_eq!(w.surface("Settings"), SurfaceId::new(3));
    assert_eq!(
        w.created()
            .iter()
            .map(|(id, _)| id.index())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(!w.interp.quit_requested());
}

#[test]
fn close_runs_the_windows_close_action() {
    let mut w = Windows::new(TOGGLE);
    w.tap("Notes", 50.0, 50.0);
    assert_eq!(w.interp.surface_ids().len(), 2);
    w.close("Settings");
    // `close => settings = false` shut it: the program goes on.
    assert_eq!(w.interp.surface_ids().len(), 1);
    assert_eq!(w.destroyed(), [SurfaceId::new(2)]);
    assert!(!w.interp.quit_requested());
}

#[test]
fn a_window_without_close_ends_the_program() {
    let mut w = Windows::new(TOGGLE);
    assert!(!w.interp.quit_requested());
    w.close("Notes");
    assert!(w.interp.quit_requested());
    // The window is still there: ending the program is the host's to do.
    assert_eq!(w.interp.surface_ids().len(), 1);
}

#[test]
fn the_last_window_closing_ends_a_program_of_windows() {
    let src = r#"
View Main() {
    var open = true
    when open {
        Window #[title: "Only", close => open = false] { Text("x") }
    }
}
"#;
    let mut w = Windows::new(src);
    assert!(!w.interp.quit_requested());
    w.close("Only");
    assert!(w.interp.surface_ids().is_empty(), "the window is gone");
    assert!(w.interp.quit_requested());
}

#[test]
fn a_still_program_draws_nothing_after_the_first_frame() {
    let mut w = Windows::new(TWO);
    assert_eq!((w.paints("A"), w.paints("B")), (1, 1));
    for _ in 0..300 {
        w.step(&[]);
    }
    assert_eq!((w.paints("A"), w.paints("B")), (1, 1));
}

#[test]
fn a_change_read_by_one_window_draws_only_that_one() {
    let src = r#"
View Main() {
    var a = 0
    var b = 0
    Window #[title: "A"] { Text("a:{a}") }
    Window #[title: "B"] { Text("b:{b}") }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    assert_eq!((w.paints("A"), w.paints("B")), (1, 1));
    w.set("a", Value::Int(1));
    w.settle();
    assert_eq!((w.paints("A"), w.paints("B")), (2, 1));
    assert_eq!(w.texts("A"), ["a:1"]);
    w.set("b", Value::Int(5));
    w.settle();
    assert_eq!((w.paints("A"), w.paints("B")), (2, 2));
    assert_eq!(w.texts("B"), ["b:5"]);
}

#[test]
fn an_attribute_read_by_one_window_draws_only_that_one() {
    // `bg` is read when the window is painted, not by a binding, which is the
    // dependence only a record of what was read can see.
    let src = r#"
View Main() {
    var on = false
    var other = 0
    Window #[title: "A"] { Box #[width: 40, height: 40, bg: on ? 0xFFFF0000 : 0xFF0000FF] }
    Window #[title: "B"] { Box #[width: 40, height: 40, bg: other > 0 ? 0xFF00FF00 : 0xFF0000FF] }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    let before = (w.paints("A"), w.paints("B"));
    w.set("on", Value::Bool(true));
    w.settle();
    assert_eq!((w.paints("A"), w.paints("B")), (before.0 + 1, before.1));
}

#[test]
fn window_width_is_the_width_of_the_enclosing_window() {
    let src = r#"
View Main() {
    Window #[title: "Narrow", width: 300, height: 200] { Gauge() }
    Window #[title: "Wide", width: 900, height: 200] { Gauge() }
}
View Gauge() {
    Text("w={window.width}")
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    assert_eq!(w.texts("Narrow"), ["w=300"]);
    assert_eq!(w.texts("Wide"), ["w=900"]);
    // Resizing one window moves its `window` and no other's.
    let wide = w.surface("Wide");
    w.interp.set_surface_size(wide, (640.0, 200.0));
    w.settle();
    assert_eq!(w.texts("Wide"), ["w=640"]);
    assert_eq!(w.texts("Narrow"), ["w=300"]);
    assert_eq!(w.paints("Narrow"), 1);
}

#[test]
fn a_when_on_window_width_is_per_window() {
    let src = r#"
View Main() {
    Window #[title: "Narrow", width: 300, height: 200] { Page() }
    Window #[title: "Wide", width: 900, height: 200] { Page() }
}
View Page() {
    when window.width >= 600 {
        Text("wide layout")
    } else {
        Text("narrow layout")
    }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    assert_eq!(w.texts("Narrow"), ["narrow layout"]);
    assert_eq!(w.texts("Wide"), ["wide layout"]);
}

const FIELDS: &str = r#"
View Main() {
    var one = ""
    var two = ""
    Window #[title: "A", width: 300, height: 200] {
        Column {
            TextField #[bind: one, width: 200, height: 30]
            TextField #[bind: two, width: 200, height: 30]
        }
    }
    Window #[title: "B", width: 300, height: 200] {
        Column {
            Button("b1") #[width: 100, height: 30, focused: f1] => one = "b1"
            Button("b2") #[width: 100, height: 30] => one = "b2"
        }
    }
}
"#;

#[test]
fn tab_moves_focus_inside_its_window_and_never_to_another() {
    let mut w = Windows::new(FIELDS);
    // Focus the first field of A with a tap, then Tab: the second field of A,
    // and Tab again wraps inside A.
    w.tap("A", 20.0, 15.0);
    let a = w.surface("A");
    let b = w.surface("B");
    assert!(w.interp.focused_element_of(a).is_some());
    let first = w.interp.focused_element_of(a);
    w.key("A", "Tab");
    let second = w.interp.focused_element_of(a);
    assert_ne!(first, second);
    w.key("A", "Tab");
    assert_eq!(w.interp.focused_element_of(a), first);
    // Nothing of B was ever focused by it.
    assert_eq!(w.interp.focused_element_of(b), None);
}

#[test]
fn a_keyed_for_of_windows_reordered_closes_and_opens_nothing() {
    let src = r#"
View Main() {
    var names = ["a", "b", "c"]
    for n in names key n {
        Window #[title: n, width: 200, height: 100] { Text("page {n}") }
    }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    let ids: Vec<(String, SurfaceId)> = ["a", "b", "c"]
        .iter()
        .map(|t| ((*t).to_string(), w.surface(t)))
        .collect();
    let created = w.changes.len();
    // Reordered, and one inserted at the front: the others stay as they are.
    w.set(
        "names",
        Value::List(
            ["z", "c", "a", "b"]
                .iter()
                .map(|s| Value::Str((*s).into()))
                .collect(),
        ),
    );
    w.settle();
    for (title, id) in &ids {
        assert_eq!(w.surface(title), *id, "{title} kept its surface");
    }
    let after: Vec<&SurfaceChange> = w.changes[created..].iter().collect();
    assert_eq!(after.len(), 1, "only z was created: {after:?}");
    assert!(matches!(after[0], SurfaceChange::Create { spec, .. } if spec.title == "z"));
    assert!(w.destroyed().is_empty(), "{:?}", w.destroyed());
    // And one removed closes exactly that one.
    w.set(
        "names",
        Value::List(
            ["z", "a", "b"]
                .iter()
                .map(|s| Value::Str((*s).into()))
                .collect(),
        ),
    );
    w.settle();
    assert_eq!(w.destroyed(), [ids[2].1]);
}

#[test]
fn a_reactive_title_is_a_configure() {
    let src = r#"
View Main() {
    var name = "one"
    Window #[title: "doc {name}", width: 200, height: 100, min_width: 120] { Text("x") }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    assert_eq!(w.created().len(), 1);
    assert_eq!(
        w.interp.surface_spec(SurfaceId::new(1)).unwrap().min_size,
        (Some(120.0), None)
    );
    w.set("name", Value::Str("two".into()));
    w.settle();
    let configs: Vec<_> = w
        .changes
        .iter()
        .filter_map(|c| match c {
            SurfaceChange::Configure { id, spec } => Some((*id, spec.title.clone(), spec.size)),
            _ => None,
        })
        .collect();
    assert_eq!(
        configs,
        [(SurfaceId::new(1), "doc two".to_string(), (200.0, 100.0))]
    );
}

#[test]
fn the_as_name_is_the_name_a_surface_is_found_by() {
    let src = r#"
View Main() {
    Window #[title: "Long title"] as inspector { Text("x") }
}
"#;
    let w = Windows::new(src);
    assert_eq!(
        w.interp.surface_spec(SurfaceId::new(1)).unwrap().name,
        "inspector"
    );
}

#[test]
fn a_held_press_is_a_long_press_in_its_own_window() {
    // The clock reaches every window's router, not only the first's.
    let src = r#"
View Main() {
    var n = 0
    Window #[title: "A", width: 200, height: 100] { Text("a") }
    Window #[title: "B", width: 200, height: 100] {
        Column {
            Box #[width: 100, height: 60, long_press => n += 1] {}
            Text("n={n}") #[height: 20]
        }
    }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    let b = w.surface("B");
    let down = w.event(b, EventKind::PointerDown, (10.0, 10.0));
    w.step(&[down]);
    for _ in 0..50 {
        w.step(&[]);
    }
    assert_eq!(w.texts("B"), ["n=1"]);
}

#[test]
fn a_window_that_opens_again_has_the_state_its_when_kept() {
    let src = r#"
View Main() {
    var open = true
    Window #[title: "Host", width: 200, height: 100] {
        Button("toggle") #[width: 100, height: 40] => open = !open
    }
    when open {
        Window #[title: "Pad", width: 200, height: 100] { Pad() }
    }
}
View Pad() {
    var taps = 0
    Column {
        Button("tap") #[width: 100, height: 40] => taps += 1
        Text("taps={taps}") #[height: 20]
    }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    w.tap("Pad", 20.0, 20.0);
    w.tap("Pad", 20.0, 20.0);
    assert_eq!(w.texts("Pad"), ["tap", "taps=2"]);
    w.tap("Host", 20.0, 20.0);
    assert!(
        !w.interp
            .surface_ids()
            .iter()
            .any(|id| w.interp.surface_spec(*id).unwrap().title == "Pad")
    );
    w.tap("Host", 20.0, 20.0);
    // A new surface (a new id) showing what the `when` kept of its content,
    // as an element under a `when` always does.
    assert_eq!(w.surface("Pad"), SurfaceId::new(3));
    assert_eq!(w.texts("Pad"), ["tap", "taps=2"]);
}

#[test]
fn typing_in_one_windows_field_is_read_by_the_other() {
    let src = r#"
View Main() {
    var name = ""
    Window #[title: "Form", width: 300, height: 100] {
        TextField #[bind: name, width: 200, height: 30]
    }
    Window #[title: "Echo", width: 300, height: 100] {
        Text("hello {name}")
    }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    w.tap("Form", 20.0, 15.0);
    let form = w.surface("Form");
    let typed: Vec<InputEvent> = "ada"
        .chars()
        .map(|c| {
            let mut ev = w.event(form, EventKind::TextInput, (0.0, 0.0));
            ev.payload = Some(byard_core::InputPayload::Key(c.to_string()));
            ev
        })
        .collect();
    w.step(&typed);
    w.settle();
    assert_eq!(w.texts("Echo"), ["hello ada"]);
}

#[test]
fn an_overlay_belongs_to_the_window_that_wrote_it() {
    let src = r#"
View Main() {
    var open = false
    Window #[title: "A", width: 300, height: 200] {
        Column {
            Button("open") #[width: 100, height: 40] => open = true
            when open {
                Overlay #[modal: true, dismiss => open = false] {
                    Box #[width: 120, height: 60, bg: 0xFFFFFF] {
                        Text("dialog")
                    }
                }
            }
        }
    }
    Window #[title: "B", width: 300, height: 200] { Text("plain") }
}
"#;
    let mut w = Windows::new(src);
    w.settle();
    assert_eq!(w.texts("A"), ["open"]);
    w.tap("A", 20.0, 20.0);
    assert!(
        w.texts("A").contains(&"dialog".to_string()),
        "{:?}",
        w.texts("A")
    );
    assert_eq!(w.texts("B"), ["plain"]);
    assert_eq!(w.paints("B"), 1);
}
