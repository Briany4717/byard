//! Bugs and small gaps Byard Material's second wave found: each test is the
//! smallest Byld program that showed the bug.

#![allow(
    clippy::unreadable_literal,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::float_cmp
)]

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, KeyModifiers};

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
}

impl App {
    fn new(src: &str) -> Self {
        Self::new_with(src, |_| {})
    }

    fn new_with(src: &str, setup: impl FnOnce(&mut Interpreter)) -> Self {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.set_theme(Theme::default());
        setup(&mut interp);
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&parsed.views[0], &known);
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
        };
        app.render();
        app
    }

    /// Every error so far, from lowering and from rendering.
    fn errors(&self) -> Vec<String> {
        self.interp
            .errors()
            .iter()
            .map(|e| format!("{e:?}"))
            .collect()
    }

    fn render(&mut self) {
        self.interp.tick();
        self.frame.clear();
        self.frame.set_access_wanted(true);
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 400.0);
    }

    fn tap(&mut self, pos: (f32, f32)) {
        let ev = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
            mods: KeyModifiers::default(),
        };
        self.interp
            .dispatch_events(&[ev(EventKind::PointerDown, 0), ev(EventKind::PointerUp, 8)]);
        self.render();
    }

    fn count(&self, name: &str) -> i64 {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .unwrap_or_else(|| panic!("`{name}` is declared"));
        self.interp.peek(sig).as_int().unwrap_or(-1)
    }

    /// Where the solid box painted `rgb` is drawn: its layout rect moved by its translate.
    fn drawn(&self, rgb: u32) -> [f32; 4] {
        let r = self.rect(rgb);
        let t = self
            .frame
            .instances()
            .iter()
            .find(|b| b.rect == r)
            .map_or([0.0; 2], |b| b.transform.translate);
        [r[0] + t[0], r[1] + t[1], r[2], r[3]]
    }

    /// The rect of the solid box painted `rgb`.
    fn rect(&self, rgb: u32) -> [f32; 4] {
        let lin = |c: u32| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let want = [
            lin((rgb >> 16) & 0xFF),
            lin((rgb >> 8) & 0xFF),
            lin(rgb & 0xFF),
        ];
        self.frame
            .instances()
            .iter()
            .find(|b| (0..3).all(|i| (b.color[i] - want[i]).abs() < 0.01))
            .unwrap_or_else(|| panic!("no box {rgb:06X}"))
            .rect
    }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.0
}

// ── #14 a parameter named `selected` ─────────────────────────────────────

#[test]
fn a_parameter_named_selected_works_in_arithmetic() {
    let app = App::new(
        "View Main() {
            var s = 5
            T(bind: s)
        }
        View T(bind selected: Int) {
            let x = selected * 2.0
            Box #[translate.x: x, width: 10, height: 10, bg: 0xFF0000] {}
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    assert!(
        near(app.drawn(0xFF0000)[0], 10.0),
        "{:?}",
        app.drawn(0xFF0000)
    );
}

#[test]
fn a_plain_parameter_named_selected_works_in_arithmetic() {
    let app = App::new(
        "View Main() { T(selected: 5) }
        View T(selected: Int) {
            let x = selected * 2.0
            Box #[translate.x: x, width: 10, height: 10, bg: 0xFF0000] {}
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    assert!(
        near(app.drawn(0xFF0000)[0], 10.0),
        "{:?}",
        app.drawn(0xFF0000)
    );
}

// ── #15 ZStack alignment ─────────────────────────────────────────────────

#[test]
fn a_zstack_aligns_a_child_to_the_bottom() {
    let app = App::new(
        "View Main() {
            ZStack #[alignment: bottom_start, width: 100, height: 100] {
                Box #[width: 20, height: 10, bg: 0xFF0000] {}
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let r = app.rect(0xFF0000);
    assert!(near(r[0], 0.0) && near(r[1], 90.0), "{r:?}");
}

#[test]
fn a_stretching_child_with_a_height_of_its_own_sits_where_alignment_puts_it() {
    // A 1 high rule across the bottom: it stretches across, keeps its height.
    let app = App::new(
        "View Main() {
            ZStack #[alignment: bottom_start, width: 100, height: 100] {
                Box #[height: 1, align_self: stretch, bg: 0xFF0000] {}
                Row { Box #[width: 50, height: 30, bg: 0x0000FF] {} }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let r = app.rect(0xFF0000);
    assert!(
        near(r[0], 0.0) && near(r[1], 99.0) && near(r[2], 100.0) && near(r[3], 1.0),
        "{r:?}"
    );
    let b = app.rect(0x0000FF);
    assert!(near(b[1], 70.0), "{b:?}");
}

// ── #18 translate and the hit area ───────────────────────────────────────

#[test]
fn a_translated_box_is_pressable_where_it_is_drawn() {
    let src = "View Main() {
        var n = 0
        ZStack #[width: 200, height: 44, alignment: top_start] {
            Box #[width: 4, height: 44, translate: (100, 0), bg: 0xFF0000, tap => n++] {}
        }
    }";
    let mut app = App::new(src);
    app.tap((2.0, 20.0));
    assert_eq!(app.count("n"), 0, "where it was laid out is empty now");
    app.tap((102.0, 20.0));
    assert_eq!(app.count("n"), 1, "where it is drawn");
}

#[test]
fn a_translated_box_in_a_scroll_view_is_pressable_where_it_is_drawn() {
    let src = "View Main() {
        var n = 0
        var y = 30.0
        ScrollView #[width: 300, height: 200, offset: (0.0, y)] {
            Column {
                Box #[width: 300, height: 100] {}
                Box #[width: 100, height: 60, translate: (150, 0), bg: 0xFF0000, tap => n++] {}
                Box #[width: 300, height: 400] {}
            }
        }
    }";
    let mut app = App::new(src);
    // Laid out at (0, 100), scrolled 30 up, moved 150 right.
    app.tap((160.0, 85.0));
    assert_eq!(app.count("n"), 1, "{:?}", app.rect(0xFF0000));
    app.tap((10.0, 85.0));
    assert_eq!(app.count("n"), 1);
}

// ── #19 `as name` inside a `when` branch ─────────────────────────────────

#[test]
fn a_state_name_tagged_inside_a_when_branch_is_found_in_that_branch() {
    let app = App::new(
        "View Main() {
            var a = true
            Column {
                when a {
                    Box #[width: 40, height: 40, bg: 0x0000FF, focus => {}] as field {}
                    Box #[width: 10, height: 10, bg: 0xFF0000] {
                        on field.hover { bg: 0x00FF00 }
                    }
                } else {
                    Box #[width: 40, height: 40, bg: 0x0000FF] as field {}
                    Box #[width: 10, height: 10, bg: 0xFF0000] {
                        on field.hover { bg: 0x00FF00 }
                    }
                }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
}

// ── #12 a11y_label on an Overlay ─────────────────────────────────────────

#[test]
fn an_overlays_a11y_label_is_an_expression() {
    let app = App::new(
        "View Main() { Column { Dialog(headline: \"Delete it?\") } }
        View Dialog(headline: Str) {
            Overlay #[modal: true, a11y_label: headline] {
                Button(\"OK\") #[width: 80, height: 30] => 0
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let text = app.frame.access().unwrap().to_text();
    assert!(text.contains("Dialog \"Delete it?\""), "{text}");
}

#[test]
fn a_full_screen_dialog_keeps_its_name() {
    let app = App::new(
        "View Main() {
            Column {
                Overlay #[modal: true, a11y_label: \"Settings\"] {
                    Box #[grow: 1, a11y_role: dialog, a11y_label: \"Inner\"] {
                        Button(\"OK\") #[width: 80, height: 30] => 0
                    }
                }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let text = app.frame.access().unwrap().to_text();
    assert!(!text.contains("unnamed"), "{text}");
}

// ── #3 an explicit height on a collapse header ───────────────────────────

#[test]
fn a_collapse_header_child_keeps_its_explicit_height() {
    let app = App::new(
        "View Main() {
            var y = 0.0
            ScrollView #[collapse_header: true, offset: (0.0, y), width: 300, height: 200] {
                Box #[height: 152, bg: 0xFF0000] {}
                Column {
                    Box #[width: 300, height: 300, bg: 0x00FF00] {}
                    Box #[width: 300, height: 300, bg: 0x0000FF] {}
                }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let r = app.rect(0xFF0000);
    assert!(near(r[3], 152.0), "{r:?}");
}

// ── #8 a Clip of width 0 ─────────────────────────────────────────────────

#[test]
fn a_zero_width_clip_measures_its_child_at_its_own_size() {
    let app = App::new(
        "View Main() {
            var w = -1.0
            Clip #[width: 0, height: 30] {
                Row {
                    on measure => { w = it.w }
                    Box #[width: 50, height: 20, bg: 0xFF0000] {}
                }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let sig = app.interp.var_signal(&Symbol::intern("w")).unwrap();
    let w = app.interp.peek(sig);
    assert!(
        matches!(w, byard_compiler::interp::env::Value::Float(f) if (f - 50.0).abs() < 1.0),
        "{w:?}"
    );
}

// ── #7 Text takes opacity ────────────────────────────────────────────────

#[test]
fn a_text_takes_opacity() {
    let app = App::new(
        "View Main() {
            Column {
                Text(\"half\") #[opacity: 0.5, color: 0xFFFFFF]
                Text(\"full\") #[color: 0xFFFFFF]
                Row #[opacity: 0.5] { Text(\"nested\") #[opacity: 0.5, color: 0xFFFFFF] }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let alpha = |text: &str| {
        app.frame
            .texts()
            .iter()
            .find(|t| t.text == text)
            .unwrap_or_else(|| panic!("no {text}"))
            .color[3]
    };
    assert!((alpha("half") - 0.5).abs() < 0.01, "{}", alpha("half"));
    assert!((alpha("full") - 1.0).abs() < 0.01);
    // The row fades as a group, so the run keeps only its own half.
    assert!((alpha("nested") - 0.5).abs() < 0.01, "{}", alpha("nested"));
}

// ── a `let` inside an element ────────────────────────────────────────────

#[test]
fn a_let_inside_an_element_is_a_clear_error() {
    let app = App::new(
        "View Main() {
            Row { let x = 4.0
                Box #[translate.x: x, width: 10, height: 10, bg: 0xFF0000] {} }
        }",
    );
    let errs = app.errors();
    assert!(
        errs.iter().any(|e| e.starts_with("LetInElementBody")),
        "{errs:?}"
    );
    // And it is the first thing said.
    assert!(errs[0].starts_with("LetInElementBody"), "{errs:?}");
}

#[test]
fn a_zstack_aligns_a_narrow_child_on_both_axes() {
    for (alignment, x, y) in [
        ("top_end", 80.0, 0.0),
        ("bottom_start", 0.0, 90.0),
        ("bottom_end", 80.0, 90.0),
        ("center", 40.0, 45.0),
    ] {
        let app = App::new(&format!(
            "View Main() {{ ZStack #[alignment: {alignment}, width: 100, height: 100] {{
                Box #[width: 20, height: 10, bg: 0xFF0000] {{}} }} }}"
        ));
        let r = app.rect(0xFF0000);
        assert!(near(r[0], x) && near(r[1], y), "{alignment}: {r:?}");
    }
}

#[test]
fn a_clip_measures_its_child_at_its_own_size_at_a_middle_width() {
    let app = App::new(
        "View Main() {
            var w = -1.0
            var f = -1.0
            Column {
                Clip #[width: 20, height: 30] {
                    Row {
                        on measure => { w = it.w }
                        Box #[width: 50, height: 20, bg: 0xFF0000] {}
                    }
                }
                Clip #[width: 20, height: 30] {
                    Row #[align_self: stretch] {
                        on measure => { f = it.w }
                        Box #[width: 5, height: 20] {}
                    }
                }
            }
        }",
    );
    let get = |n: &str| {
        let sig = app.interp.var_signal(&Symbol::intern(n)).unwrap();
        match app.interp.peek(sig) {
            byard_compiler::interp::env::Value::Float(f) => f,
            other => panic!("{other:?}"),
        }
    };
    assert!((get("w") - 50.0).abs() < 1.0, "{}", get("w"));
    assert!(
        (get("f") - 20.0).abs() < 1.0,
        "a child that asks fills: {}",
        get("f")
    );
}

// ── timers on the virtual clock, and a duration that is an expression ────

fn timed(src: &str) -> App {
    let mut app = App::new_with(src, Interpreter::use_virtual_timers);
    app.at(0);
    app
}

impl App {
    fn at(&mut self, ms: u32) {
        self.interp.set_now_ms(ms);
        self.render();
    }
}

#[test]
fn an_after_timer_fires_when_the_app_clock_passes_it() {
    let mut app = timed("View Main() { var a = 0\n after 300ms => a = 1\n Text(\"{a}\") }");
    app.at(100);
    assert_eq!(app.count("a"), 0);
    app.at(400);
    assert_eq!(app.count("a"), 1);
}

#[test]
fn an_every_timer_repeats_on_the_app_clock_and_after_fires_once() {
    let mut app = timed(
        "View Main() { var n = 0\n var once = 0\n every 100ms => n++\n after 100ms => once++\n Text(\"{n}\") }",
    );
    for ms in (16..=420).step_by(16) {
        app.at(ms);
    }
    assert!(app.count("n") >= 3, "{}", app.count("n"));
    assert_eq!(app.count("once"), 1);
}

#[test]
fn a_timer_takes_a_duration_that_is_an_expression() {
    let mut app = timed(
        "View Main() { var wait = 250\n var a = 0\n after wait * 1ms => a = 1\n Text(\"{a}\") }",
    );
    app.at(200);
    assert_eq!(app.count("a"), 0);
    app.at(300);
    assert_eq!(app.count("a"), 1);
}

// ── long_press ───────────────────────────────────────────────────────────

impl App {
    fn pointer(&mut self, kind: EventKind, pos: (f32, f32), t: u64) {
        let ev = InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
            mods: KeyModifiers::default(),
        };
        self.interp.dispatch_events(&[ev]);
        self.render();
    }
}

#[test]
fn a_long_press_fires_while_the_press_is_held_and_once() {
    let mut app = App::new(
        "View Main() { var n = 0
            Box #[width: 100, height: 100, bg: 0xFF0000, long_press => n++] {} }",
    );
    app.at(0);
    app.pointer(EventKind::PointerDown, (20.0, 20.0), 0);
    app.at(300);
    assert_eq!(app.count("n"), 0, "not yet");
    app.at(600);
    assert_eq!(app.count("n"), 1, "held past the threshold");
    app.at(900);
    app.pointer(EventKind::PointerUp, (20.0, 20.0), 900);
    assert_eq!(app.count("n"), 1, "once, and not again on release");
}

#[test]
fn a_wrappers_long_press_fires_over_a_child_that_takes_the_tap() {
    let mut app = App::new(
        "View Main() { var n = 0\n var taps = 0
            Box #[width: 100, height: 100, long_press => n++] {
                Box #[width: 100, height: 100, bg: 0xFF0000, tap => taps++] {} } }",
    );
    app.at(0);
    app.pointer(EventKind::PointerDown, (20.0, 20.0), 0);
    app.at(600);
    app.pointer(EventKind::PointerUp, (20.0, 20.0), 600);
    assert_eq!(app.count("n"), 1);
    assert_eq!(app.count("taps"), 0, "a long press is not a tap");
}

// ── a drag over a child that taps scrolls, and the tap is cancelled ──────

const CAROUSEL: &str = "View Main() {
    var x = 0.0
    var taps = 0
    ScrollView #[axis: horizontal, offset: (x, 0.0), width: 300, height: 100] {
        Row {
            Box #[width: 200, height: 100, bg: 0xFF0000, tap => taps++] {}
            Box #[width: 400, height: 100, bg: 0x0000FF] {}
        }
    }
}";

#[test]
fn a_drag_over_a_child_with_a_tap_scrolls_and_does_not_tap() {
    let mut app = App::new(CAROUSEL);
    app.pointer(EventKind::PointerDown, (150.0, 50.0), 0);
    app.pointer(EventKind::PointerMove, (100.0, 50.0), 16);
    app.pointer(EventKind::PointerMove, (50.0, 50.0), 32);
    app.pointer(EventKind::PointerUp, (50.0, 50.0), 48);
    let sig = app.interp.var_signal(&Symbol::intern("x")).unwrap();
    let x = match app.interp.peek(sig) {
        byard_compiler::interp::env::Value::Float(f) => f,
        other => panic!("{other:?}"),
    };
    assert!((x - 100.0).abs() < 1.0, "scrolled by the travel: {x}");
    assert_eq!(app.count("taps"), 0);
}

#[test]
fn a_tap_that_stays_put_still_taps_in_a_scroll_view() {
    let mut app = App::new(CAROUSEL);
    app.tap((100.0, 50.0));
    assert_eq!(app.count("taps"), 1);
}

// ── culling by where a child is drawn ────────────────────────────────────

#[test]
fn a_scroll_view_does_not_cull_a_child_its_translate_brought_into_view() {
    // The card's slot is far below the 100 high view; it is drawn 300 up.
    let app = App::new(
        "View Main() {
            ScrollView #[width: 300, height: 100] {
                Column {
                    Box #[width: 300, height: 400] {}
                    Box #[width: 100, height: 50, translate: (0, -380), bg: 0xFF0000] {}
                }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let r = app.rect(0xFF0000);
    assert!(near(r[1], 400.0), "laid out in its slot: {r:?}");
}

// ── dismiss on an anchored panel, in a view with its own state ───────────

#[test]
fn an_anchored_panels_dismiss_reaches_the_state_of_its_view() {
    let mut app = App::new(
        "View Main() { Panel() }
        View Panel() {
            var open = true
            Column {
                Box #[width: 100, height: 30] as bar {}
                Text(\"open: {open}\")
                Overlay #[modal: false] {
                    Column #[anchor_to: \"bar\", anchor_edge: below, width: 100, height: 80,
                             bg: 0x333333, dismiss => open = false] {
                        Box #[height: 10] {}
                    }
                }
            }
        }",
    );
    let shows = |app: &App| app.frame.texts().iter().any(|t| t.text == "open: true");
    assert!(shows(&app));
    app.tap((300.0, 300.0));
    assert!(!shows(&app), "a press outside closed it");
}

// ── math ─────────────────────────────────────────────────────────────────

#[test]
fn sin_cos_sqrt_and_atan2_are_number_methods() {
    let app = App::new(
        "View Main() {
            let a = (90deg).sin()
            let b = (0.0).cos()
            let c = 16.sqrt()
            let d = (1.0).atan2(1.0)
            Text(\"{a} {b} {c} {d}\")
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let t = app
        .frame
        .texts()
        .iter()
        .map(|t| t.text.clone())
        .collect::<Vec<_>>();
    assert_eq!(t, ["1 1 4 0.7853981633974483"], "{t:?}");
}

// ── a stroke on a Canvas path ────────────────────────────────────────────

#[test]
fn a_canvas_path_takes_a_stroke() {
    let app = App::new(
        "View Main() { Canvas #[width: 100, height: 100] {
            path(stroke: 0xFF0000, stroke_width: 6, cap: round, join: round) {
                move(10, 10)
                cubic(40, 0, 60, 100, 90, 90)
            }
            path(fill: 0x00FF00, stroke: 0x0000FF, stroke_width: 2) {
                move(0, 0) line(20, 0) line(20, 20) close()
            }
        } }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    // The first is a stroke alone, the second a fill and its outline.
    assert_eq!(app.frame.fills().len(), 3);
    let thick = &app.frame.fills()[0].mesh;
    // The line is 6 wide: the mesh reaches 3 either side of the points.
    let min_x = thick
        .vertices
        .iter()
        .map(|v| v.pos[0])
        .fold(f32::MAX, f32::min);
    assert!(min_x < 9.0, "{min_x}");
}

#[test]
fn a_dash_on_a_canvas_path_is_still_refused() {
    let app = App::new(
        "View Main() { Canvas #[width: 100, height: 100] {
            path(stroke: 0xFF0000, dash: (4, 2)) { move(0, 0) line(50, 50) }
        } }",
    );
    assert!(
        app.errors()
            .iter()
            .any(|e| e.starts_with("PathStrokeUnsupported")),
        "{:?}",
        app.errors()
    );
}
