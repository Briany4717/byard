//! Layout and text for components (RFC-0064): a row that wraps, minimum and
//! maximum sizes, `max_lines` on a `Text`, the multiline `TextField`, a gap in
//! a border, and the elevation presets.
//!
//! The layout cases read the rectangle a box was given, found by its colour.
//! The editing cases drive a focused field with synthetic events, as
//! `text_editing.rs` does.

// Colours are written as the hex a `.byd` file writes them in.
#![allow(clippy::unreadable_literal, clippy::cast_precision_loss)]

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{Clipboard, EventKind, InputEvent, InputPayload, KeyModifiers, MemoryClipboard};

fn frame_of(src: &str, w: f32, h: f32) -> RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(Theme::default());
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, w, h);
    frame
}

fn frame_in(src: &str) -> RenderFrame {
    frame_of(src, 400.0, 300.0)
}

/// What the checker says about `src`, one debug line per error.
fn errors_of(src: &str) -> Vec<String> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(Theme::default());
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let _ = interp.lower_view(&parsed.views[0], &known);
    interp.errors().iter().map(|e| format!("{e:?}")).collect()
}

fn has_error(src: &str, kind: &str) -> bool {
    let errs = errors_of(src);
    errs.iter().any(|e| e.starts_with(kind))
}

/// The rect of the solid box painted `rgb` (0xRRGGBB, opaque).
fn rect_of(frame: &RenderFrame, rgb: u32) -> [f32; 4] {
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
    frame
        .instances()
        .iter()
        .find(|b| (0..3).all(|i| (b.color[i] - want[i]).abs() < 0.01))
        .unwrap_or_else(|| {
            panic!(
                "no box {rgb:06X} in {:?}",
                frame
                    .instances()
                    .iter()
                    .map(|b| (b.color, b.rect))
                    .collect::<Vec<_>>()
            )
        })
        .rect
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.0
}

/// The height of a line of text at the default size: 1.2 times 16.
const LINE: f32 = 19.2;

// ── a row that wraps ─────────────────────────────────────────────────────

const CHIPS: &str = "View Main() {
    Row #[width: 200, gap: 8, ATTRS] {
        Box #[bg: 0xFF0000, width: 80, height: 30] {}
        Box #[bg: 0x00FF00, width: 80, height: 30] {}
        Box #[bg: 0x0000FF, width: 80, height: 30] {}
    }
}";

#[test]
fn a_wrapping_row_moves_what_does_not_fit_to_the_next_line() {
    let f = frame_in(&CHIPS.replace("ATTRS", "wrap: true, row_gap: 20"));
    let (r, g, b) = (
        rect_of(&f, 0xFF0000),
        rect_of(&f, 0x00FF00),
        rect_of(&f, 0x0000FF),
    );
    assert!(near(r[0], 0.0) && near(r[1], 0.0), "{r:?}");
    assert!(near(g[0], 88.0) && near(g[1], 0.0), "{g:?}");
    assert!(near(b[0], 0.0) && near(b[1], 50.0), "{b:?}");
    // Nothing was squeezed to make room.
    assert!(near(r[2], 80.0) && near(g[2], 80.0) && near(b[2], 80.0));
}

#[test]
fn a_row_that_does_not_wrap_keeps_everything_on_one_line() {
    let f = frame_in(&CHIPS.replace("ATTRS", "p: 0"));
    assert!(near(rect_of(&f, 0x0000FF)[1], 0.0));
}

#[test]
fn the_gap_between_lines_is_the_gap_when_row_gap_is_not_written() {
    let f = frame_in(&CHIPS.replace("ATTRS", "wrap: true"));
    assert!(near(rect_of(&f, 0x0000FF)[1], 38.0), "30 high and 8 apart");
}

#[test]
fn the_wrapping_row_is_as_tall_as_its_lines() {
    let src = "View Main() {
    Column #[align: start] {
        Row #[width: 200, gap: 8, row_gap: 20, wrap: true, bg: 0xABCDEF] {
            Box #[width: 80, height: 30] {}
            Box #[width: 80, height: 30] {}
            Box #[width: 80, height: 30] {}
        }
    }
}";
    let f = frame_in(src);
    assert!(near(rect_of(&f, 0xABCDEF)[3], 80.0), "two lines and a gap");
}

#[test]
fn row_gap_means_nothing_without_wrap() {
    let src = CHIPS.replace("ATTRS", "row_gap: 4");
    assert!(
        has_error(&src, "InvalidAttributeCombination"),
        "{:?}",
        errors_of(&src)
    );
    let src = CHIPS.replace("ATTRS", "wrap: false, row_gap: 4");
    assert!(
        has_error(&src, "InvalidAttributeCombination"),
        "{:?}",
        errors_of(&src)
    );
}

#[test]
fn a_wrap_that_depends_on_the_window_is_not_rejected() {
    let src = "View Main() {
        var narrow: Bool = true
        Row #[wrap: narrow, row_gap: 4] { Box #[width: 10, height: 10] {} }
    }";
    assert!(errors_of(src).is_empty(), "{:?}", errors_of(src));
}

#[test]
fn only_a_row_wraps() {
    for el in ["Column", "Box"] {
        let src =
            format!("View Main() {{ {el} #[wrap: true] {{ Box #[width: 1, height: 1] {{}} }} }}");
        assert!(
            has_error(&src, "UnknownAttribute"),
            "{el}: {:?}",
            errors_of(&src)
        );
    }
}

// ── minimum and maximum sizes ────────────────────────────────────────────

fn column_with(child: &str) -> String {
    format!("View Main() {{ Column #[align: start] {{ {child} }} }}")
}

#[test]
fn a_minimum_height_raises_an_element_that_is_shorter() {
    let f = frame_in(&column_with(
        "Box #[bg: 0xFF0000, width: 100, min_height: 50] {}",
    ));
    assert!(near(rect_of(&f, 0xFF0000)[3], 50.0));
}

#[test]
fn a_minimum_width_raises_an_element_that_is_narrower() {
    let f = frame_in(&column_with(
        "Box #[bg: 0xFF0000, width: 20, height: 10, min_width: 90] {}",
    ));
    assert!(near(rect_of(&f, 0xFF0000)[2], 90.0));
}

#[test]
fn a_maximum_width_lowers_an_element_that_is_wider() {
    let f = frame_in(&column_with(
        "Box #[bg: 0xFF0000, width: 300, height: 10, max_width: 120] {}",
    ));
    assert!(near(rect_of(&f, 0xFF0000)[2], 120.0));
}

#[test]
fn a_maximum_height_lowers_an_element_that_is_taller() {
    let f = frame_in(&column_with(
        "Box #[bg: 0xFF0000, width: 10, height: 200, max_height: 60] {}",
    ));
    assert!(near(rect_of(&f, 0xFF0000)[3], 60.0));
}

#[test]
fn the_minimum_wins_when_the_two_disagree() {
    let f = frame_in(&column_with(
        "Box #[bg: 0xFF0000, width: 10, height: 10, min_width: 200, max_width: 150] {}",
    ));
    assert!(near(rect_of(&f, 0xFF0000)[2], 200.0), "as in CSS");
}

#[test]
fn a_container_that_fits_its_children_is_bounded_too() {
    let f = frame_in(&column_with(
        "Row #[bg: 0xFF0000, max_width: 100] {
            Box #[width: 80, height: 10, shrink: 0] {}
            Box #[width: 80, height: 10, shrink: 0] {}
        }",
    ));
    assert!(near(rect_of(&f, 0xFF0000)[2], 100.0));
}

#[test]
fn a_text_has_limits_too() {
    let f = frame_in(&column_with(
        "Text(\"hi\") #[bg: 0xFF0000, min_width: 120, min_height: 40]",
    ));
    let r = rect_of(&f, 0xFF0000);
    assert!(near(r[2], 120.0) && near(r[3], 40.0), "{r:?}");
}

#[test]
fn a_field_has_limits_too() {
    let f = frame_in(
        &column_with("TextField #[bind: q, bg: 0xFF0000, width: 300, height: 36, max_width: 100]")
            .replace("Column", "var q: Str = \"\"\n Column"),
    );
    assert!(near(rect_of(&f, 0xFF0000)[2], 100.0));
}

#[test]
fn a_button_writing_its_own_minimum_replaces_the_touch_floor() {
    let default = frame_in(&column_with("Button(\"ok\") #[bg: 0xFF0000]"));
    let own = frame_in(&column_with(
        "Button(\"ok\") #[bg: 0xFF0000, min_height: 10]",
    ));
    let (a, b) = (rect_of(&default, 0xFF0000), rect_of(&own, 0xFF0000));
    assert!(near(a[3], 40.0), "the theme's floor: {a:?}");
    assert!(b[3] < a[3], "its own: {b:?}");
}

#[test]
fn limits_are_static() {
    let src = "View Main() {
        var x: Int = 10
        Box #[min_width: x with anim.linear(100ms)] {}
    }";
    assert!(
        has_error(src, "LayoutPropNotAnimatable"),
        "{:?}",
        errors_of(src)
    );
}

// ── max_lines on a Text ──────────────────────────────────────────────────

const LONG: &str = "one two three four five six seven eight nine ten eleven twelve thirteen";

fn text_in_100(attrs: &str) -> String {
    format!(
        "View Main() {{
    Column #[width: 100, align: start] {{
        Text(\"{LONG}\") #[bg: 0xFF0000, size: 16{attrs}]
    }}
}}"
    )
}

#[test]
fn a_text_without_max_lines_takes_every_line() {
    let r = rect_of(&frame_in(&text_in_100("")), 0xFF0000);
    assert!(r[3] > 4.0 * LINE, "{r:?}");
}

#[test]
fn max_lines_caps_the_height_at_that_many_lines() {
    let r = rect_of(&frame_in(&text_in_100(", max_lines: 2")), 0xFF0000);
    assert!((r[3] - 2.0 * LINE).abs() < 1.0, "{r:?}");
    assert!(r[2] <= 100.0, "{r:?}");
}

#[test]
fn a_cap_above_the_line_count_changes_nothing() {
    let all = rect_of(&frame_in(&text_in_100("")), 0xFF0000);
    let capped = rect_of(&frame_in(&text_in_100(", max_lines: 50")), 0xFF0000);
    assert!(near(all[3], capped[3]), "{all:?} {capped:?}");
}

#[test]
fn max_lines_can_be_an_expression() {
    let src = "View Main() {
        var n: Int = 1
        Column #[width: 100, align: start] {
            Text(\"one two three four five six seven eight nine ten\") #[bg: 0xFF0000, size: 16, max_lines: n + 1]
        }
    }";
    let r = rect_of(&frame_in(src), 0xFF0000);
    assert!((r[3] - 2.0 * LINE).abs() < 1.0, "{r:?}");
}

#[test]
fn overflow_without_max_lines_is_an_error() {
    let src = text_in_100(", overflow: clip");
    assert!(
        has_error(&src, "InvalidAttributeCombination"),
        "{:?}",
        errors_of(&src)
    );
}

#[test]
fn overflow_is_ellipsis_or_clip() {
    let src = text_in_100(", max_lines: 2, overflow: fade");
    assert!(
        has_error(&src, "AttributeTypeMismatch"),
        "{:?}",
        errors_of(&src)
    );
    for ok in ["ellipsis", "clip"] {
        let src = text_in_100(&format!(", max_lines: 2, overflow: {ok}"));
        assert!(errors_of(&src).is_empty(), "{ok}: {:?}", errors_of(&src));
    }
}

#[test]
fn max_lines_on_a_text_that_does_not_wrap_is_an_error() {
    let src = text_in_100(", max_lines: 2, wrap: false");
    assert!(
        has_error(&src, "InvalidAttributeCombination"),
        "{:?}",
        errors_of(&src)
    );
}

#[test]
fn max_lines_below_one_is_an_error() {
    let src = text_in_100(", max_lines: 0");
    assert!(
        has_error(&src, "InvalidAttributeCombination"),
        "{:?}",
        errors_of(&src)
    );
}

#[test]
fn the_run_is_drawn_with_the_cap_and_the_mark() {
    let line = |attrs: &str| {
        let f = frame_in(&text_in_100(attrs));
        let t = f.texts().first().expect("the text").clone();
        (t.max_lines, t.ellipsis)
    };
    assert_eq!(line(""), (0, false), "no cap, as before");
    assert_eq!(line(", max_lines: 2"), (2, true), "ellipsis is the default");
    assert_eq!(line(", max_lines: 3, overflow: ellipsis"), (3, true));
    assert_eq!(line(", max_lines: 2, overflow: clip"), (2, false));
}

// ── elevation ────────────────────────────────────────────────────────────

/// `(dy, blur)` of the shadow a box with `shadow: preset` casts.
fn cast(preset: &str) -> (f32, f32) {
    let f = frame_in(&format!(
        "View Main() {{ Box #[bg: 0xFFFFFF, width: 50, height: 50, shadow: \"{preset}\"] {{}} }}"
    ));
    let s = f
        .decorated()
        .iter()
        .find(|d| d.shadow_blur > 0.0)
        .unwrap_or_else(|| panic!("no shadow for {preset}"));
    (s.shadow_dy, s.shadow_blur)
}

#[test]
fn the_presets_cover_the_five_material_elevations() {
    assert_eq!(cast("sm"), (1.0, 3.0), "level 1");
    assert_eq!(cast("md"), (3.0, 8.0), "level 2");
    assert_eq!(cast("lg"), (6.0, 16.0), "level 3");
    assert_eq!(cast("xl"), (8.0, 20.0), "level 4");
    assert_eq!(cast("2xl"), (12.0, 28.0), "level 5");
}

#[test]
fn a_fab_can_lift_from_level_three_to_four() {
    let src = "View Main() {
        var hovered: Bool = false
        Box #[bg: 0xFFFFFF, width: 56, height: 56, shadow: hovered ? \"xl\" : \"lg\"] {}
    }";
    assert!(errors_of(src).is_empty(), "{:?}", errors_of(src));
}

#[test]
fn the_error_for_a_word_that_is_not_a_preset_lists_the_five() {
    let src = "View Main() { Box #[bg: 0xFFFFFF, width: 5, height: 5, shadow: \"huge\"] {} }";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 100.0, 100.0);
    let all = interp
        .errors()
        .iter()
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>()
        .join("\n");
    for word in ["sm", "md", "lg", "xl", "2xl"] {
        assert!(
            all.contains(&format!("\\\"{word}\\\"")),
            "{word} missing from {all}"
        );
    }
}

// ── a border with a gap ──────────────────────────────────────────────────

#[test]
fn a_border_gap_reaches_the_decorated_box() {
    let f = frame_in(
        "View Main() {
            Box #[border: 0x79747E, border_width: 1, radius: 4, width: 100, height: 50,
                  border_gap: (12, 40)] {}
        }",
    );
    let d = f
        .decorated()
        .iter()
        .find(|d| d.border_width > 0.0)
        .expect("the border");
    assert_eq!(d.border_gap, [12.0, 40.0]);
}

#[test]
fn an_element_without_a_gap_has_none() {
    let f = frame_in(
        "View Main() {
            Box #[border: 0x79747E, border_width: 1, width: 100, height: 50] {}
        }",
    );
    let d = f.decorated().iter().find(|d| d.border_width > 0.0).unwrap();
    assert_eq!(d.border_gap, [0.0, 0.0]);
}

#[test]
fn the_gap_cuts_a_border_that_sits_over_a_fill_too() {
    // An opaque fill takes the solid path and the border is a decorated
    // overlay; the gap must still be on the overlay.
    let f = frame_in(
        "View Main() {
            Box #[bg: 0x222222, border: 0x79747E, border_width: 1, width: 100, height: 50,
                  border_gap: (12, 40)] {}
        }",
    );
    let d = f.decorated().iter().find(|d| d.border_width > 0.0).unwrap();
    assert_eq!(d.border_gap, [12.0, 40.0]);
}

#[test]
fn a_gap_can_depend_on_state() {
    let f = frame_in(
        "View Main() {
            var w: Int = 30
            Box #[border: 0x79747E, border_width: 1, width: 100, height: 50,
                  border_gap: (8, w + 4)] {}
        }",
    );
    let d = f.decorated().iter().find(|d| d.border_width > 0.0).unwrap();
    assert_eq!(d.border_gap, [8.0, 34.0]);
}

#[test]
fn a_gap_needs_a_border() {
    let src =
        "View Main() { Box #[bg: 0x222222, width: 100, height: 50, border_gap: (12, 40)] {} }";
    assert!(
        has_error(src, "InvalidAttributeCombination"),
        "{:?}",
        errors_of(src)
    );
}

#[test]
fn a_border_set_by_a_state_block_is_a_border() {
    let src = "View Main() {
        Box #[width: 100, height: 50, border_gap: (12, 40)] {
            on hover { border: 0x79747E, border_width: 1 }
        }
    }";
    assert!(errors_of(src).is_empty(), "{:?}", errors_of(src));
}

#[test]
fn a_gap_is_a_pair() {
    for bad in ["12", "(1, 2, 3)", "\"x\""] {
        let src = format!(
            "View Main() {{ Box #[border: 0x79747E, width: 100, height: 50, border_gap: {bad}] {{}} }}"
        );
        assert!(
            has_error(&src, "AttributeTypeMismatch"),
            "{bad}: {:?}",
            errors_of(&src)
        );
    }
}

// ── the multiline field ──────────────────────────────────────────────────

const FIELD_TOP: f32 = 20.0;

fn field_src(attrs: &str, value: &str) -> String {
    format!(
        "View Main() {{
    var q: Str = \"{value}\"
    var submitted: Int = 0
    Column #[p: 20, gap: 10, width: 400, height: 300, align: start] {{
        TextField #[bind: q, width: 200, bg: 0xFF0000, submit => submitted++{attrs}]
        Text(\"below\")
    }}
}}"
    )
}

fn field_height(attrs: &str, value: &str) -> f32 {
    rect_of(&frame_in(&field_src(attrs, value)), 0xFF0000)[3]
}

#[test]
fn a_field_with_no_lines_is_the_single_line_field() {
    assert!(near(field_height("", ""), 36.0));
    assert!(near(field_height(", lines: 1", ""), 36.0));
    assert!(near(field_height(", max_lines: 1", "a\\nb"), 36.0));
}

#[test]
fn lines_fixes_the_height_at_that_many_lines_and_the_padding() {
    assert!(near(field_height(", lines: 3", ""), 3.0 * LINE + 16.0));
    assert!(near(field_height(", lines: 5", "x"), 5.0 * LINE + 16.0));
    // And it stays that high whatever it holds.
    assert!(near(
        field_height(", lines: 3", "a\\nb\\nc\\nd\\ne\\nf"),
        3.0 * LINE + 16.0
    ));
}

#[test]
fn an_explicit_height_beats_lines() {
    assert!(near(field_height(", lines: 3, height: 100", ""), 100.0));
}

#[test]
fn a_field_with_a_range_of_lines_grows_with_its_content_and_stops() {
    let attrs = ", min_lines: 2, max_lines: 4";
    assert!(
        near(field_height(attrs, ""), 2.0 * LINE + 16.0),
        "empty: the minimum"
    );
    assert!(
        near(field_height(attrs, "a\\nb\\nc"), 3.0 * LINE + 16.0),
        "three lines"
    );
    assert!(
        near(
            field_height(attrs, "a\\nb\\nc\\nd\\ne\\nf\\ng"),
            4.0 * LINE + 16.0
        ),
        "capped"
    );
}

#[test]
fn a_long_line_wraps_at_the_width_of_the_field_and_grows_it() {
    let long = "word ".repeat(30);
    let h = field_height(", min_lines: 1, max_lines: 20", &long);
    assert!(h > 3.0 * LINE + 16.0, "{h}");
}

#[test]
fn a_range_with_only_a_minimum_has_no_ceiling() {
    let many = "a\\n".repeat(8);
    let h = field_height(", min_lines: 2", &many);
    assert!(h > 7.0 * LINE, "{h}");
}

#[test]
fn lines_with_a_range_is_an_error() {
    for attrs in [", lines: 2, min_lines: 1", ", lines: 2, max_lines: 3"] {
        let src = field_src(attrs, "");
        assert!(
            has_error(&src, "InvalidAttributeCombination"),
            "{attrs}: {:?}",
            errors_of(&src)
        );
    }
}

#[test]
fn a_minimum_above_the_maximum_is_an_error() {
    let src = field_src(", min_lines: 4, max_lines: 2", "");
    assert!(
        has_error(&src, "InvalidAttributeCombination"),
        "{:?}",
        errors_of(&src)
    );
}

#[test]
fn a_line_count_below_one_is_an_error() {
    for attrs in [", lines: 0", ", min_lines: 0", ", max_lines: 0"] {
        let src = field_src(attrs, "");
        assert!(
            has_error(&src, "InvalidAttributeCombination"),
            "{attrs}: {:?}",
            errors_of(&src)
        );
    }
}

// ── editing in it ────────────────────────────────────────────────────────

struct App {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    t: u64,
}

fn none() -> KeyModifiers {
    KeyModifiers::default()
}

fn shift() -> KeyModifiers {
    KeyModifiers {
        shift: true,
        ..none()
    }
}

fn modk() -> KeyModifiers {
    KeyModifiers::parse_chord("mod+a").unwrap().0
}

impl App {
    fn new(attrs: &str, value: &str) -> Self {
        let parsed = parse(&field_src(attrs, value));
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &["Main"]);
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

    /// A focused field holding `text`.
    fn with(attrs: &str, text: &str) -> Self {
        let mut app = Self::new(attrs, "");
        app.tap((100.0, FIELD_TOP + 10.0));
        app.insert(text);
        app
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 300.0);
    }

    fn event(
        &mut self,
        kind: EventKind,
        pos: (f32, f32),
        p: Option<InputPayload>,
        mods: KeyModifiers,
    ) -> InputEvent {
        self.t += 50;
        InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: p,
            time_ms: self.t,
            mods,
        }
    }

    fn tap(&mut self, pos: (f32, f32)) {
        self.t += 1000;
        let down = self.event(EventKind::PointerDown, pos, None, none());
        let up = self.event(EventKind::PointerUp, pos, None, none());
        self.send(&[down, up]);
    }

    fn insert(&mut self, text: &str) {
        let e = self.event(
            EventKind::TextInput,
            (0.0, 0.0),
            Some(InputPayload::Key(text.into())),
            none(),
        );
        self.send(&[e]);
    }

    fn key(&mut self, key: &str, mods: KeyModifiers) {
        let e = self.event(
            EventKind::KeyDown,
            (0.0, 0.0),
            Some(InputPayload::Key(key.into())),
            mods,
        );
        self.send(&[e]);
    }

    fn keys(&mut self, key: &str, mods: KeyModifiers, times: usize) {
        for _ in 0..times {
            self.key(key, mods);
        }
    }

    fn var(&self, name: &str) -> Value {
        let sig = self.interp.var_signal(&Symbol::intern(name)).unwrap();
        self.interp.peek(sig)
    }

    fn q(&self) -> String {
        match self.var("q") {
            Value::Str(s) => s,
            other => panic!("q is {other:?}"),
        }
    }

    fn submitted(&self) -> i64 {
        self.var("submitted").as_int().unwrap()
    }

    fn sel(&self) -> (usize, usize) {
        self.interp.focused_selection().expect("a focused field")
    }

    /// The field as read: `|` at the caret, `[` `]` around a selection.
    fn shown(&self) -> String {
        let (anchor, caret) = self.sel();
        let (lo, hi) = (anchor.min(caret), anchor.max(caret));
        let mut s = self.q();
        if lo == hi {
            s.insert(lo, '|');
        } else {
            s.insert(hi, ']');
            s.insert(lo, '[');
        }
        s
    }

    fn caret(&self) -> byard_core::frame::Rect {
        self.frame.text_input().expect("a focused field").caret
    }
}

const THREE: &str = ", lines: 3";

#[test]
fn enter_inserts_a_newline_and_does_not_submit() {
    let mut app = App::with(THREE, "ab");
    app.key("Enter", none());
    assert_eq!(app.q(), "ab\n");
    app.insert("c");
    assert_eq!(app.q(), "ab\nc");
    assert_eq!(app.submitted(), 0);
}

#[test]
fn enter_still_submits_in_a_single_line_field() {
    let mut app = App::with("", "ab");
    app.key("Enter", none());
    assert_eq!(app.q(), "ab");
    assert_eq!(app.submitted(), 1);
    let mut app = App::with(", lines: 1", "ab");
    app.key("Enter", none());
    assert_eq!(app.submitted(), 1);
}

#[test]
fn a_range_of_lines_is_multiline_too() {
    let mut app = App::with(", min_lines: 2, max_lines: 5", "ab");
    app.key("Enter", none());
    assert_eq!(app.q(), "ab\n");
    assert_eq!(app.submitted(), 0);
}

#[test]
fn backspace_removes_a_newline_like_any_character() {
    let mut app = App::with(THREE, "a\nb");
    app.keys("ArrowLeft", none(), 1);
    app.key("Backspace", none());
    assert_eq!(app.q(), "ab");
}

#[test]
fn a_paste_keeps_its_line_breaks_in_a_multiline_field() {
    let mut app = App::with(THREE, "");
    let mut c = MemoryClipboard::new();
    c.write_text("one\r\ntwo\rthree\n\u{7}four");
    app.interp.set_clipboard(Box::new(c));
    app.key("v", modk());
    assert_eq!(app.q(), "one\ntwo\nthree\nfour");
}

#[test]
fn a_paste_stays_one_line_in_a_single_line_field() {
    let mut app = App::with("", "");
    let mut c = MemoryClipboard::new();
    c.write_text("one\ntwo");
    app.interp.set_clipboard(Box::new(c));
    app.key("v", modk());
    assert_eq!(app.q(), "one two");
}

#[test]
fn up_and_down_move_between_lines_keeping_the_column() {
    let mut app = App::with(THREE, "xxxx\nx");
    assert_eq!(app.shown(), "xxxx\nx|");
    app.key("ArrowUp", none());
    assert_eq!(app.shown(), "x|xxx\nx", "one x in, as far as the caret was");
    app.key("ArrowDown", none());
    assert_eq!(app.shown(), "xxxx\nx|");
}

#[test]
fn the_column_survives_a_shorter_line_in_between() {
    let mut app = App::with(THREE, "xxxx\n\nxxxx");
    app.key("ArrowUp", none());
    assert_eq!(app.shown(), "xxxx\n|\nxxxx", "the empty line");
    app.key("ArrowUp", none());
    assert_eq!(
        app.shown(),
        "xxxx|\n\nxxxx",
        "and back to the column it had"
    );
}

#[test]
fn up_on_the_first_line_goes_to_the_start_and_down_on_the_last_to_the_end() {
    let mut app = App::with(THREE, "ab\ncd");
    app.keys("ArrowUp", none(), 2);
    assert_eq!(app.shown(), "|ab\ncd");
    app.keys("ArrowDown", none(), 3);
    assert_eq!(app.shown(), "ab\ncd|");
}

#[test]
fn shift_with_up_selects_to_the_line_above() {
    let mut app = App::with(THREE, "xxxx\nxx");
    app.key("ArrowUp", shift());
    assert_eq!(app.shown(), "xx[xx\nxx]");
}

#[test]
fn home_and_end_go_to_the_ends_of_the_line() {
    let mut app = App::with(THREE, "ab\ncd");
    app.key("Home", none());
    assert_eq!(app.shown(), "ab\n|cd");
    app.key("End", none());
    assert_eq!(app.shown(), "ab\ncd|");
    app.keys("ArrowLeft", none(), 3);
    assert_eq!(app.shown(), "ab|\ncd", "the end of the first line");
    app.key("Home", none());
    assert_eq!(app.shown(), "|ab\ncd");
}

#[test]
fn the_shortcut_with_home_and_end_goes_to_the_ends_of_the_text() {
    let mut app = App::with(THREE, "ab\ncd\nef");
    app.keys("ArrowUp", none(), 1);
    app.key("Home", modk());
    assert_eq!(app.shown(), "|ab\ncd\nef");
    app.key("End", modk());
    assert_eq!(app.shown(), "ab\ncd\nef|");
}

#[test]
fn a_press_puts_the_caret_on_the_line_it_landed_on() {
    let mut app = App::with(THREE, "xxxx\nyyyy\nzzzz");
    // Inside the field, 8 down to the first line, then a line each.
    let y = |line: f32| FIELD_TOP + 8.0 + LINE * line + LINE / 2.0;
    app.tap((22.0, y(1.0)));
    let (_, caret) = app.sel();
    assert_eq!(caret, 5, "the start of the second line: {}", app.shown());
    app.tap((22.0, y(2.0)));
    assert_eq!(app.sel().1, 10);
    app.tap((22.0, y(0.0)));
    assert_eq!(app.sel().1, 0);
}

#[test]
fn a_press_below_the_last_line_lands_on_it() {
    let mut app = App::with(", lines: 5", "xxxx\nyyyy");
    app.tap((22.0, FIELD_TOP + 8.0 + LINE * 4.0));
    assert_eq!(app.sel().1, 5, "{}", app.shown());
}

#[test]
fn the_caret_is_drawn_on_the_line_it_is_in() {
    let mut app = App::with(", lines: 4", "a");
    let top = app.caret().y;
    app.key("Enter", none());
    assert!((app.caret().y - top - LINE).abs() < 1.0);
    app.key("Enter", none());
    assert!((app.caret().y - top - 2.0 * LINE).abs() < 1.0);
    // A new line starts at the left edge again.
    assert!((app.caret().x - app.caret().x.floor()).abs() < 1.0);
}

#[test]
fn a_long_line_wraps_and_the_caret_follows_it_down() {
    let mut app = App::with(", lines: 4", "");
    let top = app.caret().y;
    app.insert(&"word ".repeat(12));
    let after = app.caret();
    assert!(
        after.y > top + LINE - 1.0,
        "it wrapped: {after:?} from {top}"
    );
    // Up moves to the line above on the same text, not to the start of it.
    let (_, at) = app.sel();
    app.key("ArrowUp", none());
    let (_, up) = app.sel();
    assert!(up > 0 && up < at, "{up} of {at}");
}

#[test]
fn a_field_scrolls_to_keep_the_caret_in_view() {
    let mut app = App::with(", lines: 2", "");
    let field = rect_of(&app.frame, 0xFF0000);
    for _ in 0..6 {
        app.key("Enter", none());
    }
    let c = app.caret();
    assert!(
        c.y >= field[1] && c.y + c.height <= field[1] + field[3] + 0.5,
        "caret {c:?} inside {field:?}"
    );
    // And back up again.
    app.key("Home", modk());
    let c = app.caret();
    assert!(
        c.y >= field[1] && c.y + c.height <= field[1] + field[3] + 0.5,
        "{c:?}"
    );
}

#[test]
fn the_selection_is_one_rectangle_per_line() {
    let mut app = App::with(", lines: 4", "xxxx\nxxxx\nxxxx");
    let count = |app: &App| app.frame.instances().len() + app.frame.decorated().len();
    let before = count(&app);
    app.key("a", modk());
    let after = count(&app);
    assert!(after - before >= 3, "{before} -> {after}");
}

// ── the second round: submit, paging, the wheel, the preedit ─────────────

impl App {
    fn wheel(&mut self, pos: (f32, f32), lines: f32) {
        let mut e = self.event(EventKind::Wheel, pos, None, none());
        e.delta = (0.0, lines);
        self.send(&[e]);
    }

    fn preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        self.t += 50;
        let inputs: Vec<InputEvent> = [byard_core::ImeEvent::Preedit {
            text: text.into(),
            cursor,
        }]
        .into_iter()
        .filter_map(|e| e.into_input(self.t))
        .collect();
        self.send(&inputs);
    }
}

#[test]
fn the_shortcut_with_enter_submits_a_multiline_field_and_enter_alone_does_not() {
    let mut app = App::with(THREE, "ab");
    app.key("Enter", modk());
    assert_eq!(app.submitted(), 1);
    assert_eq!(app.q(), "ab", "no line break came with it");
    app.key("Enter", none());
    assert_eq!(app.submitted(), 1);
    assert_eq!(app.q(), "ab\n");
}

#[test]
fn page_down_and_up_move_a_page_of_lines_and_keep_the_column() {
    // Three lines are in view, so a page is three lines.
    let text = (0..10)
        .map(|i| format!("xx{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = App::with(THREE, &text);
    app.key("Home", modk());
    app.key("ArrowRight", none());
    assert_eq!(app.sel().1, 1);
    app.key("PageDown", none());
    assert_eq!(app.sel().1, 3 * 4 + 1, "the same column, three lines on");
    app.key("PageUp", none());
    assert_eq!(app.sel().1, 1);
    app.key("PageUp", none());
    assert_eq!(app.sel().1, 0, "the first page goes to the start");
    app.key("PageDown", shift());
    assert_eq!(app.sel(), (0, 12), "Shift selects a page");
}

#[test]
fn the_wheel_scrolls_a_field_that_has_more_lines_than_it_shows() {
    let lines = "x\n".repeat(7);
    let mut app = App::with(", lines: 2", &lines);
    let field = rect_of(&app.frame, 0xFF0000);
    let at = (field[0] + 40.0, field[1] + 20.0);
    let bottom = app.caret().y;
    app.wheel(at, 1.0);
    assert!(
        (app.caret().y - bottom - 40.0).abs() < 1.0,
        "a line of wheel is 40 px: {} from {bottom}",
        app.caret().y
    );
    // The scroll stays where it was put while nothing moves the caret.
    app.send(&[]);
    assert!((app.caret().y - bottom - 40.0).abs() < 1.0);
    // And typing brings the caret back into view.
    app.insert("y");
    assert!((app.caret().y - bottom).abs() < 1.0, "{}", app.caret().y);
    // The wheel never scrolls past the text.
    for _ in 0..10 {
        app.wheel(at, 1.0);
    }
    assert!(app.caret().y < bottom + 120.0);
    for _ in 0..10 {
        app.wheel(at, -1.0);
    }
    assert!((app.caret().y - bottom).abs() < 1.0, "back at the end");
}

#[test]
fn a_field_that_fits_its_text_does_not_scroll() {
    let mut app = App::with(", lines: 4", "a\nb");
    let before = app.caret().y;
    let field = rect_of(&app.frame, 0xFF0000);
    app.wheel((field[0] + 40.0, field[1] + 20.0), -3.0);
    assert!((app.caret().y - before).abs() < 0.5);
}

#[test]
fn a_preedit_is_inside_the_text_and_moves_what_follows_it_aside() {
    let mut app = App::with(THREE, "ab\ncd");
    app.key("Home", modk());
    app.keys("ArrowRight", none(), 2);
    assert_eq!(app.sel().1, 2);
    let before = app.caret().x;
    app.preedit("XY", None);
    let text = app
        .frame
        .texts()
        .iter()
        .map(|t| t.text.clone())
        .collect::<Vec<_>>();
    assert!(
        text.iter().any(|t| t == "abXY\ncd"),
        "the preedit is in the run, before the rest: {text:?}"
    );
    assert!(app.caret().x > before + 5.0, "the caret is after it");
    assert_eq!(app.q(), "ab\ncd", "and the value is not touched");
    // A preedit long enough to wrap takes the caret to the next line.
    let top = app.caret().y;
    app.preedit(&"W".repeat(40), None);
    assert!(app.caret().y > top + LINE - 1.0, "wrapped");
}
