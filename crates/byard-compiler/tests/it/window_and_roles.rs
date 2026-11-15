//! The window as a value, spin buttons and grids, text styles on a canvas and
//! aligned fields (RFC-0067).

use byard_compiler::diagnostics::CompileError;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::theme::{DeclaredFont, FontWeight, Theme, TypoToken};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::access::{AccessAction, AccessActions, AccessNode, AccessRequest, AccessRole};
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};
use std::sync::Arc;

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    size: (f32, f32),
    t: u64,
}

fn theme() -> Theme {
    let mut theme = Theme::byard_base();
    theme.set_breakpoint("md", 600.0);
    theme.set_breakpoint("lg", 1000.0);
    theme
}

impl App {
    fn new(src: &str) -> Self {
        Self::with(src, theme(), (400.0, 400.0))
    }

    fn with(src: &str, theme: Theme, size: (f32, f32)) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.set_theme(theme);
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            size,
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
            .render(&self.tree, &mut self.frame, self.size.0, self.size.1);
    }

    fn resize(&mut self, width: f32, height: f32) {
        self.size = (width, height);
        self.send(&[]);
    }

    fn event(&mut self, kind: EventKind, payload: Option<InputPayload>) -> InputEvent {
        self.t += 10;
        InputEvent {
            surface: byard_core::SurfaceId::MAIN,
            kind,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods: KeyModifiers::default(),
        }
    }

    fn key(&mut self, name: &str) {
        let down = self.event(EventKind::KeyDown, Some(InputPayload::Key(name.into())));
        let up = self.event(EventKind::KeyUp, Some(InputPayload::Key(name.into())));
        self.send(&[down, up]);
    }

    fn named(&self, name: &str) -> &AccessNode {
        self.try_named(name).unwrap_or_else(|| {
            panic!(
                "nothing named {name:?} in\n{}",
                self.frame.access().unwrap().to_text()
            )
        })
    }

    fn try_named(&self, name: &str) -> Option<&AccessNode> {
        let tree = self.frame.access().expect("a tree");
        tree.nodes.iter().find(|n| n.name.as_deref() == Some(name))
    }

    fn focused(&self) -> Option<String> {
        let tree = self.frame.access()?;
        let id = tree.focus?;
        tree.node(id)?.name.clone()
    }

    fn dump(&self) -> String {
        self.frame.access().expect("a tree").to_text()
    }

    fn text(&self, name: &str) -> String {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        match self.interp.peek(sig) {
            Value::Str(s) => s,
            other => panic!("{name} is {other:?}"),
        }
    }
}

fn lower_errors(src: &str) -> Vec<CompileError> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme());
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

// ── 1. The window ───────────────────────────────────────────────────────

const SCAFFOLD: &str = r#"
View Main() {
    Column {
        when window.width >= md {
            Row { Text("rail") }
        } else {
            Column { Text("bar") }
        }
    }
}
"#;

#[test]
fn a_when_reads_the_window_and_follows_it() {
    let mut app = App::with(SCAFFOLD, theme(), (500.0, 400.0));
    assert!(app.try_named("bar").is_some(), "{}", app.dump());
    assert!(app.try_named("rail").is_none());
    app.resize(700.0, 400.0);
    assert!(app.try_named("rail").is_some(), "{}", app.dump());
    assert!(app.try_named("bar").is_none());
    app.resize(599.0, 400.0);
    assert!(app.try_named("bar").is_some());
}

#[test]
fn the_breakpoint_holds_from_exactly_its_width() {
    let mut app = App::with(SCAFFOLD, theme(), (600.0, 400.0));
    assert!(app.try_named("rail").is_some(), "{}", app.dump());
    app.resize(599.5, 400.0);
    assert!(app.try_named("bar").is_some());
}

#[test]
fn the_first_frame_already_has_the_window_in_it() {
    let app = App::with(SCAFFOLD, theme(), (900.0, 400.0));
    assert!(app.try_named("rail").is_some(), "{}", app.dump());
}

#[test]
fn width_and_height_are_floats_a_text_can_show() {
    let src = r#"
View Main() {
    let size = "{window.width}x{window.height}"
    Text(size)
}
"#;
    let mut app = App::with(src, theme(), (320.0, 480.0));
    assert!(app.try_named("320x480").is_some(), "{}", app.dump());
    app.resize(321.0, 200.0);
    assert!(app.try_named("321x200").is_some(), "{}", app.dump());
}

#[test]
fn a_number_a_height_and_either_side_work() {
    let src = r#"
View Main() {
    Column {
        when 700 <= window.width { Text("wide") }
        when window.height < lg { Text("short") }
        when window.width < 300 { Text("narrow") }
    }
}
"#;
    let mut app = App::with(src, theme(), (800.0, 800.0));
    assert!(app.try_named("wide").is_some(), "{}", app.dump());
    assert!(app.try_named("short").is_some());
    assert!(app.try_named("narrow").is_none());
    app.resize(250.0, 1200.0);
    assert!(app.try_named("wide").is_none());
    assert!(app.try_named("short").is_none());
    assert!(app.try_named("narrow").is_some());
}

#[test]
fn the_window_is_a_record_as_a_whole() {
    let src = r#"
View Main() {
    let w = window
    Text("{w.width}")
}
"#;
    let mut app = App::with(src, theme(), (410.0, 400.0));
    assert!(app.try_named("410").is_some(), "{}", app.dump());
    app.resize(420.0, 400.0);
    assert!(app.try_named("420").is_some());
}

#[test]
fn an_attribute_can_read_the_window() {
    let src = r#"
View Main() {
    Column { Box #[width: window.width / 2, height: 10, a11y_role: group, a11y_label: "half"] {} }
}
"#;
    let mut app = App::with(src, theme(), (400.0, 400.0));
    assert!((app.named("half").bounds.width - 200.0).abs() < 0.5);
    app.resize(600.0, 400.0);
    assert!((app.named("half").bounds.width - 300.0).abs() < 0.5);
}

#[test]
fn an_undeclared_breakpoint_is_an_error_with_a_hint() {
    let errors = lower_errors("View Main() { when window.width >= mdd { Text(\"x\") } }");
    assert_eq!(kinds(&errors), ["UnknownBreakpoint"], "{errors:?}");
    match &errors[0] {
        CompileError::UnknownBreakpoint { name, hint, .. } => {
            assert_eq!(name, "mdd");
            assert_eq!(hint.as_deref(), Some("md"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_undeclared_breakpoint_never_holds() {
    let src = "View Main() { when window.width >= huge { Text(\"x\") } else { Text(\"y\") } }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    frame.set_access_wanted(true);
    interp.render(&tree, &mut frame, 5000.0, 400.0);
    let text = frame.access().unwrap().to_text();
    assert!(text.contains("\"y\""), "{text}");
    assert!(!text.contains("\"x\""), "{text}");
}

#[test]
fn the_checker_knows_window_and_leaves_the_breakpoint_to_the_project() {
    let src = r#"
View Main() {
    Column {
        when window.width >= md { Text("a") }
        when lg < window.height { Text("b") }
    }
}
"#;
    assert!(check_errors(src).is_empty(), "{:?}", check_errors(src));
}

#[test]
fn a_name_that_is_not_beside_the_window_is_still_unknown() {
    let src = "View Main() { when 3 >= md { Text(\"a\") } }";
    assert_eq!(kinds(&check_errors(src)), ["UnknownName"]);
}

#[test]
fn window_has_a_width_and_a_height_and_nothing_else() {
    let errors = check_errors("View Main() { when window.widht > 3 { Text(\"a\") } }");
    assert_eq!(kinds(&errors), ["UnknownField"], "{errors:?}");
    match &errors[0] {
        CompileError::UnknownField { hint, .. } => assert_eq!(hint.as_deref(), Some("width")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_view_that_binds_window_reads_its_own() {
    let src = r#"
View Main() {
    var window = 7
    Text("{window}")
}
"#;
    let app = App::new(src);
    assert!(app.try_named("7").is_some(), "{}", app.dump());
    assert_eq!(kinds(&check_errors(src)), Vec::<&str>::new());
}

// ── 2. Spin buttons ─────────────────────────────────────────────────────

const SPIN: &str = r#"
View Main() {
    var hour = 4
    var focus = false
    Column #[p: 10] {
        Box #[a11y_role: spin_button, a11y_label: "Hours", a11y_value: "{hour}",
              a11y_value_min: 1, a11y_value_max: 12, a11y_value_step: 1,
              width: 80, height: 40, focused: focus,
              key_down(k) => { if k == "ArrowUp" { hour = hour % 12 + 1 } }] {
            Text("{hour}")
        }
    }
}
"#;

#[test]
fn a_spin_button_has_a_role_a_value_and_a_range() {
    let app = App::new(SPIN);
    let node = app.named("Hours");
    assert_eq!(node.role, AccessRole::SpinButton);
    assert!(
        app.dump()
            .contains("SpinButton \"Hours\" = 4 in 1..12 step 1"),
        "{}",
        app.dump()
    );
}

#[test]
fn a_reader_steps_a_spin_button_with_its_own_keys() {
    let mut app = App::new(SPIN);
    let id = app.named("Hours").id;
    assert!(
        app.named("Hours")
            .actions
            .contains(AccessActions::INCREMENT)
    );
    assert!(
        app.named("Hours")
            .actions
            .contains(AccessActions::DECREMENT)
    );
    let tree = app.frame.access().unwrap().clone();
    let events = |action| AccessRequest { target: id, action }.to_events(&tree, 100);
    let up = events(AccessAction::Increment);
    app.send(&up);
    assert!(app.dump().contains("= 5 in 1..12"), "{}", app.dump());
    assert_eq!(app.focused().as_deref(), Some("Hours"));
    // The control owns the value, so a number set from outside has nowhere to
    // go.
    assert_eq!(events(AccessAction::SetNumber(9.0)).len(), 0);
}

#[test]
fn up_is_the_controls_own() {
    let mut app = App::new(SPIN);
    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("Hours"));
    app.key("ArrowUp");
    assert!(app.try_named("Hours").is_some());
    assert!(app.dump().contains("= 5 in 1..12"), "{}", app.dump());
}

#[test]
fn a_range_needs_a_role_that_has_one() {
    let on_button = "View Main() { Box #[a11y_role: button, a11y_value_min: 1, tap => {}] {} }";
    assert_eq!(kinds(&lower_errors(on_button)), ["AttributeNeedsRole"]);
    let on_spin =
        "View Main() { Box #[a11y_role: spin_button, a11y_label: \"n\", a11y_value_min: 1] {} }";
    assert!(
        lower_errors(on_spin).is_empty(),
        "{:?}",
        lower_errors(on_spin)
    );
}

#[test]
fn a_misspelt_role_offers_the_new_words() {
    let errors = lower_errors("View Main() { Box #[a11y_role: spin_buton, a11y_label: \"n\"] {} }");
    assert_ne!(errors.len(), 0);
    let text = format!("{errors:?}");
    assert!(text.contains("spin_button"), "{text}");
}

// ── 3. Grids ────────────────────────────────────────────────────────────

/// A month of four days a row, three rows, the day 6 selected, and a button
/// after it. Every cell logs the keys the grid did not use.
const GRID: &str = r#"
View Main() {
    var last = ""
    var picked = 0
    Column #[p: 10, gap: 4] {
        Button("before") #[width: 60, height: 30] => {}
        Column #[a11y_role: grid, a11y_label: "November"] {
            for r in [0, 1, 2] {
                Row #[a11y_role: row, gap: 4] {
                    for c in [1, 2, 3, 4] {
                        Box #[a11y_role: grid_cell, a11y_label: "d{r * 4 + c}",
                              a11y_selected: r * 4 + c == 6,
                              disabled: r * 4 + c == 7,
                              width: 40, height: 30,
                              tap => picked = r * 4 + c,
                              key_down(k) => last = k] {
                            Text("{r * 4 + c}")
                        }
                    }
                }
            }
        }
        Button("after") #[width: 60, height: 30] => {}
    }
}
"#;

#[test]
fn a_grid_its_rows_and_its_cells_are_in_the_tree() {
    let app = App::new(GRID);
    let dump = app.dump();
    assert!(dump.contains("Grid \"November\""), "{dump}");
    assert!(dump.contains("Row"), "{dump}");
    assert!(dump.contains("GridCell \"d6\""), "{dump}");
    assert_eq!(app.named("d6").role, AccessRole::GridCell);
    assert!(
        dump.contains("selected"),
        "the selected cell says so\n{dump}"
    );
}

#[test]
fn a_grid_is_one_tab_stop_entered_at_the_selected_cell() {
    let mut app = App::new(GRID);
    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("before"));
    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("d6"));
    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("after"));
    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("before"));
}

fn at_d6() -> App {
    let mut app = App::new(GRID);
    app.key("Tab");
    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("d6"));
    app
}

#[test]
fn left_and_right_move_by_cell_and_on_to_the_next_row() {
    let mut app = at_d6();
    app.key("ArrowRight");
    // d7 is disabled: the next enabled cell is d8.
    assert_eq!(app.focused().as_deref(), Some("d8"));
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("d9"));
    app.key("ArrowLeft");
    app.key("ArrowLeft");
    assert_eq!(app.focused().as_deref(), Some("d6"));
    app.key("ArrowLeft");
    assert_eq!(app.focused().as_deref(), Some("d5"));
}

#[test]
fn up_and_down_move_by_row_in_the_same_column() {
    let mut app = at_d6();
    app.key("ArrowDown");
    assert_eq!(app.focused().as_deref(), Some("d10"));
    app.key("ArrowUp");
    app.key("ArrowUp");
    assert_eq!(app.focused().as_deref(), Some("d2"));
}

#[test]
fn a_disabled_cell_does_not_shift_the_columns() {
    let mut app = at_d6();
    // d6 is the second column; Up then Down lands in the second column.
    app.key("ArrowUp");
    app.key("ArrowRight");
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("d4"));
    app.key("ArrowDown");
    // Column four of the second row is d8.
    assert_eq!(app.focused().as_deref(), Some("d8"));
}

#[test]
fn a_cell_below_a_disabled_one_is_skipped_down_the_column() {
    let mut app = App::new(GRID);
    app.key("Tab");
    app.key("Tab");
    app.key("ArrowUp");
    // d2 -> Down is d6; go to the third column through the first row.
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("d3"));
    app.key("ArrowDown");
    // d7 is disabled, so the column goes on to d11.
    assert_eq!(app.focused().as_deref(), Some("d11"));
}

#[test]
fn home_and_end_go_to_the_ends_of_the_row() {
    let mut app = at_d6();
    app.key("End");
    assert_eq!(app.focused().as_deref(), Some("d8"));
    app.key("Home");
    assert_eq!(app.focused().as_deref(), Some("d5"));
}

#[test]
fn an_arrow_with_nowhere_to_go_is_the_cells_own() {
    let mut app = at_d6();
    app.key("ArrowUp");
    assert_eq!(app.focused().as_deref(), Some("d2"));
    app.key("ArrowUp");
    assert_eq!(app.focused().as_deref(), Some("d2"), "the first row stays");
    assert_eq!(app.text("last"), "ArrowUp", "and the key reaches key_down");
    app.key("ArrowLeft");
    app.key("ArrowLeft");
    assert_eq!(app.focused().as_deref(), Some("d1"));
    app.key("ArrowLeft");
    assert_eq!(app.text("last"), "ArrowLeft");
}

#[test]
fn page_keys_and_the_rest_are_left_to_the_cell() {
    let mut app = at_d6();
    app.key("PageDown");
    assert_eq!(app.text("last"), "PageDown");
    assert_eq!(app.focused().as_deref(), Some("d6"));
    app.key("PageUp");
    assert_eq!(app.text("last"), "PageUp");
    app.key("x");
    assert_eq!(app.text("last"), "x");
}

#[test]
fn space_and_enter_press_the_cell() {
    let mut app = at_d6();
    app.key("Space");
    let sig = app.interp.var_signal(&Symbol::intern("picked")).unwrap();
    assert_eq!(app.interp.peek(sig), Value::Int(6));
    app.key("ArrowRight");
    app.key("Enter");
    assert_eq!(app.interp.peek(sig), Value::Int(8));
}

#[test]
fn a_layout_grid_takes_its_rows_from_its_columns() {
    let src = r#"
View Main() {
    Column #[p: 10] {
        Grid #[a11y_role: grid, a11y_label: "Days", columns: "repeat(3, 40)", gap: 4] {
            for i in [1, 2, 3, 4, 5, 6, 7] {
                Box #[a11y_role: grid_cell, a11y_label: "c{i}", width: 40, height: 30,
                      tap => {}] { Text("{i}") }
            }
        }
    }
}
"#;
    let mut app = App::new(src);
    app.key("Tab");
    assert_eq!(app.focused().as_deref(), Some("c1"));
    app.key("ArrowDown");
    assert_eq!(app.focused().as_deref(), Some("c4"));
    app.key("ArrowDown");
    assert_eq!(app.focused().as_deref(), Some("c7"));
    app.key("ArrowUp");
    app.key("ArrowRight");
    assert_eq!(app.focused().as_deref(), Some("c5"));
    app.key("ArrowDown");
    // The last row has one cell, so Down from the second column goes to it.
    assert_eq!(app.focused().as_deref(), Some("c7"));
    app.key("Home");
    assert_eq!(app.focused().as_deref(), Some("c7"));
}

#[test]
fn a_grid_with_no_rows_is_one_row() {
    let src = r#"
View Main() {
    Column #[a11y_role: grid, a11y_label: "g"] {
        Box #[a11y_role: grid_cell, a11y_label: "a", width: 40, height: 30, tap => {}] {}
        Box #[a11y_role: grid_cell, a11y_label: "b", width: 40, height: 30, tap => {}] {}
    }
}
"#;
    let mut app = App::new(src);
    app.key("Tab");
    app.key("ArrowDown");
    assert_eq!(app.focused().as_deref(), Some("a"));
    app.key("End");
    assert_eq!(app.focused().as_deref(), Some("b"));
}

// ── 4. Text styles on a canvas ──────────────────────────────────────────

fn face(file: &str) -> DeclaredFont {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../byard-cli/examples/assets/fonts")
        .join(file);
    let bytes: Arc<[u8]> = Arc::from(std::fs::read(&path).expect("the shipped font"));
    let resolved = byard_core::text::family_name(&bytes).expect("it parses");
    DeclaredFont {
        path: file.to_string(),
        resolved: Arc::from(resolved),
        bytes,
        icons: false,
        icons_include: Vec::new(),
    }
}

fn typo_theme() -> Theme {
    let mut theme = Theme::byard_base();
    theme.add_font("display", face("SpaceGrotesk-Variable.ttf"));
    theme.set_typo(
        "hero",
        TypoToken {
            family: Some("display".to_string()),
            weight: FontWeight::Bold,
            ..TypoToken::plain(30.0)
        },
    );
    theme
}

fn canvas_line(args: &str, theme: Theme) -> byard_core::TextLine {
    let src = format!(
        "View Main() {{ Canvas #[width: 200, height: 100] {{ text(\"12\", x: 100, y: 50{args}) }} }}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame.texts().first().expect("a text line").clone()
}

#[test]
fn a_canvas_text_takes_the_size_weight_and_family_of_a_typo() {
    let line = canvas_line(", typo: hero", typo_theme());
    assert!((line.font_size - 30.0).abs() < 0.01, "{}", line.font_size);
    assert_eq!(line.weight, 700);
    assert_eq!(line.family.as_deref(), Some("Space Grotesk"));
}

#[test]
fn a_size_beside_a_typo_wins_for_the_size_only() {
    let line = canvas_line(", typo: hero, size: 12", typo_theme());
    assert!((line.font_size - 12.0).abs() < 0.01);
    assert_eq!(line.weight, 700);
    assert_eq!(line.family.as_deref(), Some("Space Grotesk"));
}

#[test]
fn a_canvas_text_without_a_typo_is_drawn_as_before() {
    let line = canvas_line(", size: 20", typo_theme());
    assert_eq!(line.weight, 400);
    assert_eq!(line.family, None);
}

#[test]
fn a_centred_canvas_text_is_centred_in_the_face_it_is_drawn_in() {
    let plain = canvas_line(", size: 30, align: center", typo_theme());
    let styled = canvas_line(", typo: hero, align: center", typo_theme());
    // The styled line is measured in its own face: its left edge is where a
    // run that wide centred on x = 100 begins, not where the default face's
    // would have.
    assert!(
        (plain.x - styled.x).abs() > 0.01,
        "{} vs {}",
        plain.x,
        styled.x
    );
}

// ── 5. Aligned fields ───────────────────────────────────────────────────

fn field(align: &str, value: &str) -> App {
    let src = format!(
        r#"
View Main() {{
    var v = "{value}"
    Column {{ TextField #[bind: v, width: 200, height: 40, placeholder: "ph"{align}] }}
}}
"#
    );
    App::new(&src)
}

fn value_x(app: &App, text: &str) -> f32 {
    app.frame
        .texts()
        .iter()
        .find(|t| t.text == text)
        .unwrap_or_else(|| panic!("no line {text:?}"))
        .x
}

#[test]
fn a_field_starts_its_text_at_the_start_by_default() {
    let start = value_x(&field("", "12"), "12");
    let explicit = value_x(&field(", text_align: start", "12"), "12");
    assert!((start - explicit).abs() < 0.01);
}

#[test]
fn center_and_end_move_the_text_within_the_field() {
    let start = value_x(&field("", "12"), "12");
    let center = value_x(&field(", text_align: center", "12"), "12");
    let end = value_x(&field(", text_align: end", "12"), "12");
    assert!(center > start + 20.0, "{start} {center}");
    assert!(end > center + 20.0, "{center} {end}");
    // The text's right edge meets the content box's: `end` leaves the room the
    // start has on the left on the right.
    let text_w = {
        let probe = field(", text_align: end", "12");
        let measured = probe.frame.texts().iter().find(|t| t.text == "12").unwrap();
        measured.font_size * 1.2
    };
    assert!(end + text_w > 200.0 - 20.0, "{end} {text_w}");
}

#[test]
fn a_placeholder_is_aligned_too() {
    let start = value_x(&field("", ""), "ph");
    let center = value_x(&field(", text_align: center", ""), "ph");
    assert!(center > start + 20.0, "{start} {center}");
}

#[test]
fn text_wider_than_the_box_is_where_it_was() {
    let long = "a very long text that cannot fit in two hundred pixels at all";
    let start = value_x(&field("", long), long);
    let end = value_x(&field(", text_align: end", long), long);
    assert!((start - end).abs() < 0.01);
}

#[test]
fn the_caret_and_a_tap_follow_the_aligned_text() {
    let mut app = field(", text_align: center", "123");
    let x = value_x(&app, "123");
    // Press just inside the start of the text, to the right of the field's
    // own left padding: the caret goes to the start of the value, and typing
    // inserts there.
    let mut down = app.event(EventKind::PointerDown, None);
    down.pos = (x + 1.0, 20.0);
    let mut up = app.event(EventKind::PointerUp, None);
    up.pos = down.pos;
    app.send(&[down, up]);
    let ty = app.event(EventKind::TextInput, Some(InputPayload::Str("x".into())));
    app.send(&[ty]);
    assert_eq!(app.text("v"), "x123");
}

#[test]
fn text_align_has_three_words() {
    let errors = lower_errors(
        "View Main() { var v = \"\" Column { TextField #[bind: v, text_align: middle] } }",
    );
    assert!(!errors.is_empty(), "a misspelt word is an error");
}

#[test]
fn text_align_is_for_one_line() {
    let src = "View Main() { var v = \"\" Column { TextField #[bind: v, lines: 3, text_align: center] } }";
    assert_eq!(kinds(&lower_errors(src)), ["InvalidAttributeCombination"]);
    let ok =
        "View Main() { var v = \"\" Column { TextField #[bind: v, lines: 3, text_align: start] } }";
    assert_eq!(kinds(&lower_errors(ok)), Vec::<&str>::new());
    let one =
        "View Main() { var v = \"\" Column { TextField #[bind: v, lines: 1, text_align: end] } }";
    assert_eq!(kinds(&lower_errors(one)), Vec::<&str>::new());
}
