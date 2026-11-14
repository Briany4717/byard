//! Bugs and small gaps Byard Material found after RFC-0065 and RFC-0066: each test is the
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
    now: u32,
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
            now: 0,
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

    /// Moves the clock on by `ms` and draws one frame.
    fn advance(&mut self, ms: u32) {
        self.now += ms;
        self.render();
    }

    fn render(&mut self) {
        self.interp.set_now_ms(self.now);
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
    #[allow(dead_code)]
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

#[allow(dead_code)]
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.0
}

// ── 30. `none` from a `?:` arm ───────────────────────────────────────────

const OPTIONAL_COLOR: &str = "context C { color: Color? = none }
View Main() { provide C(color: false ? 0xFF0000 : none)
    Inner() }
View Inner() {
    inject C as c
    Column {
        when c.color != none { Box #[width: 10, height: 10, bg: 0xFF0000] {} }
        else { Box #[width: 10, height: 10, bg: 0x00FF00] {} }
    }
}";

#[test]
fn none_from_a_ternary_arm_into_a_context_field_is_none() {
    let app = App::new(OPTIONAL_COLOR);
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    app.rect(0x00FF00);
}

#[test]
fn a_value_from_a_ternary_arm_into_an_optional_context_field_is_present() {
    let app = App::new(&OPTIONAL_COLOR.replace("false ?", "true ?"));
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    app.rect(0xFF0000);
}

#[test]
fn none_from_a_ternary_arm_into_an_optional_parameter_is_none() {
    let app = App::new(
        "View Main() { var on = false
            Inner(color: on ? 0xFF0000 : none) }
        View Inner(color: Color? = none) {
            when color != none { Box #[width: 10, height: 10, bg: 0xFF0000] {} }
            else { Box #[width: 10, height: 10, bg: 0x00FF00] {} }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    app.rect(0x00FF00);
}

#[test]
fn a_ternary_arm_none_is_still_text_where_the_text_is_wanted() {
    let app = App::new("View Main() { var a = false\n Text(a ? \"x\" : \"none\") }");
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    assert!(app.frame.texts().iter().any(|t| t.text == "none"));
}
// ── 32. `a11y_described_by` ──────────────────────────────────────────────

#[test]
fn described_by_can_point_at_a_bare_text() {
    let app = App::new(
        "View Main() {
            Column #[a11y_role: group, a11y_label: \"Box\", a11y_described_by: note] {
                Text(\"Read this first\") as note
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let text = app.frame.access().unwrap().to_text();
    assert!(text.contains("described_by"), "{text}");
}

#[test]
fn a_name_tagged_in_a_when_is_visible_to_attributes_outside_it() {
    let src = "View Main() {
            var has = true
            Column #[a11y_role: group, a11y_label: \"Box\", a11y_described_by: note] {
                when has { Text(\"Read this first\") as note }
            }
        }";
    let app = App::new(src);
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let text = app.frame.access().unwrap().to_text();
    assert!(text.contains("described_by"), "{text}");
    // Not mounted: it describes nothing, and it is not an error.
    let app = App::new(&src.replace("var has = true", "var has = false"));
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    let text = app.frame.access().unwrap().to_text();
    assert!(!text.contains("described_by"), "{text}");
}

// ── 31. the first measured value lands without motion ────────────────────

const MEASURED: &str = "View Main() {
    var w = 0.0
    Column #[width: 120] {
        Row #[width: 120, height: 10] {
            on measure => { w = it.w }
        }
        Box #[height: 6, bg: 0xFF0000, width: w with t.springSpatialFast] {}
    }
}";

#[test]
fn a_value_the_first_measure_writes_lands_without_motion() {
    let mut app = App::new(MEASURED);
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    for _ in 0..4 {
        app.advance(16);
    }
    // The frame after the measure shows the measured width at once.
    let first = app.rect(0xFF0000)[2];
    assert!(near(first, 120.0), "{first}");
}

#[test]
fn a_later_measure_still_moves() {
    let mut app = App::new(
        "View Main() {
            var w = 0.0
            var wide = false
            Column {
                Row #[width: wide ? 200 : 100, height: 10] {
                    on measure => { w = it.w }
                }
                Box #[height: 6, bg: 0xFF0000, width: w with t.springSpatialFast] {}
                Box #[width: 10, height: 10, bg: 0x0000FF, tap => wide = true] {}
            }
        }",
    );
    for _ in 0..40 {
        app.advance(16);
    }
    assert!(near(app.rect(0xFF0000)[2], 100.0));
    app.tap((5.0, 20.0));
    for _ in 0..2 {
        app.advance(16);
    }
    let mid = app.rect(0xFF0000)[2];
    assert!(mid > 100.0 && mid < 200.0, "{mid}");
}

// ── 33 and 38. `on enter` and a delayed `with`; two speeds ───────────────

/// The opacity the 40 wide box is drawn at, wherever the frame keeps it
/// (a group, the blended pass or the solid one); 0 when none is drawn.
fn opacity_of(app: &App, _rgb: u32) -> f32 {
    let mut out: Vec<f32> = app.frame.groups().iter().map(|g| g.opacity).collect();
    out.extend(
        app.frame
            .decorated()
            .iter()
            .filter(|d| (d.base.rect[2] - 40.0).abs() < 0.5)
            .map(|d| d.base.color[3] * d.opacity),
    );
    out.extend(
        app.frame
            .instances()
            .iter()
            .filter(|b| (b.rect[2] - 40.0).abs() < 0.5)
            .map(|b| b.color[3] * b.transform.opacity),
    );
    out.first().copied().unwrap_or(0.0)
}

#[test]
fn on_enter_honours_a_delayed_with() {
    let mut app = App::new(
        "View Main() {
            Box #[width: 40, height: 40, bg: 0xFF0000,
                  opacity: 1.0 with anim.ease(150ms, delay: 2000ms)] {
                on enter { opacity: 0.0 }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    for _ in 0..19 {
        app.advance(16);
    }
    let early = opacity_of(&app, 0xFF0000);
    assert!(early < 0.01, "still waiting at 300 ms, opacity {early}");
    app.advance(2000);
    app.advance(200);
    let late = opacity_of(&app, 0xFF0000);
    assert!(late > 0.99, "{late}");
}

#[test]
fn enter_and_leave_can_have_different_speeds() {
    let src = "View Main() {
        var on = true
        Column {
            Box #[width: 10, height: 10, bg: 0x0000FF, tap => on = !on] {}
            when on {
                Box #[width: 40, height: 40, bg: 0xFF0000,
                      opacity: 1.0 with anim.ease(100ms)] {
                    on enter { opacity: 0.0 }
                    on leave { opacity: 0.0 with anim.ease(1000ms) }
                }
            }
        }
    }";
    let mut app = App::new(src);
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    // In: done in 100 ms.
    for _ in 0..10 {
        app.advance(16);
    }
    assert!(opacity_of(&app, 0xFF0000) > 0.99, "{}", opacity_of(&app, 0xFF0000));
    // Out: still going after 200 ms (a 100 ms motion would be over).
    app.tap((5.0, 5.0));
    for _ in 0..12 {
        app.advance(16);
    }
    let out = opacity_of(&app, 0xFF0000);
    assert!(out > 0.3, "the exit took the speed of the way in: {out}");
}

// ── 34. the modal scrim follows its overlay's enter and leave ────────────

fn scrim_alpha(app: &App) -> f32 {
    app.frame
        .instances()
        .iter()
        .find(|b| b.rect == [0.0, 0.0, 400.0, 400.0])
        .map_or(0.0, |b| b.color[3])
}

#[test]
fn the_modal_scrim_fades_with_its_overlay() {
    let mut app = App::new(
        "View Main() {
            var open = false
            Column {
                Box #[width: 10, height: 10, bg: 0x0000FF, tap => open = !open] {}
                when open {
                    Overlay #[modal: true, dismiss => open = false] {
                        Box #[width: 100, height: 100, bg: 0xFF0000, opacity: 1.0 with anim.linear(200ms)] {
                            on enter { opacity: 0.0 }
                            on leave { opacity: 0.0 }
                        }
                    }
                }
            }
        }",
    );
    assert!(app.errors().is_empty(), "{:?}", app.errors());
    assert!(scrim_alpha(&app) == 0.0);
    app.tap((5.0, 5.0));
    let first = scrim_alpha(&app);
    assert!(first < 0.05, "the scrim starts clear: {first}");
    for _ in 0..4 {
        app.advance(16);
    }
    let mid = scrim_alpha(&app);
    assert!(mid > 0.0 && mid < 0.3, "{mid}");
    for _ in 0..20 {
        app.advance(16);
    }
    assert!((scrim_alpha(&app) - 0.32).abs() < 0.01, "{}", scrim_alpha(&app));
    // And back out: a press on the scrim dismisses.
    app.tap((390.0, 390.0));
    for _ in 0..3 {
        app.advance(16);
    }
    let out = scrim_alpha(&app);
    assert!(out > 0.0 && out < 0.3, "{out}");
}
