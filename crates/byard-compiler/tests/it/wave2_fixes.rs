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
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.set_theme(Theme::default());
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
    // `grow` is the same, and a width of its own works the same way across.
    let app = App::new(
        "View Main() {
            ZStack #[alignment: top_end, width: 100, height: 100] {
                Box #[width: 1, grow: 1, bg: 0xFF0000] {}
            }
        }",
    );
    let r = app.rect(0xFF0000);
    assert!(
        near(r[0], 99.0) && near(r[1], 0.0) && near(r[2], 1.0) && near(r[3], 100.0),
        "{r:?}"
    );
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
fn a_scaled_box_is_pressable_where_it_is_drawn() {
    // 100 wide scaled to half about its top-left: drawn over 0..50.
    let src = "View Main() {
        var n = 0
        ZStack #[width: 300, height: 300, alignment: top_start] {
            Box #[width: 100, height: 100, scale: 0.5, origin: (0.0, 0.0), bg: 0xFF0000, tap => n++] {}
        }
    }";
    let mut app = App::new(src);
    app.tap((80.0, 80.0));
    assert_eq!(app.count("n"), 0, "past where it is drawn");
    app.tap((40.0, 40.0));
    assert_eq!(app.count("n"), 1);
}

#[test]
fn a_translated_box_in_a_scroll_view_is_pressable_where_it_is_drawn() {
    let src = "View Main() {
        var n = 0
        var y = 30.0
        ScrollView #[width: 300, height: 200, offset: (0.0, y)] {
            Column {
                Box #[width: 300, height: 100] {}
                Box #[width: 100, height: 60, translate: (150, 0), scale: 0.5, origin: (0.0, 0.0), bg: 0xFF0000, tap => n++] {}
                Box #[width: 300, height: 400] {}
            }
        }
    }";
    let mut app = App::new(src);
    // Laid out at (0, 100), scrolled 30 up, scaled to 50x30, moved 150 right.
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
