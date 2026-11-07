//! The layout a component needs: `shrink`, `align_self`, a bare `TextField`
//! that fills the way `grow` and `width` say, `bg` and `radius` on a `Text`,
//! and the width an `on measure` hands a child.

// Colours are written as the hex a `.byd` file writes them in.
#![allow(clippy::unreadable_literal, clippy::cast_precision_loss)]

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload};

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
    (a - b).abs() < 0.6
}

fn frame_in(src: &str) -> RenderFrame {
    frame_of(src, 400.0, 300.0)
}

// ── shrink ──────────────────────────────────────────────────────────────

/// A 24 wide icon beside a sibling that wants more room than the row has.
const ICON_ROW: &str = "View Main() {
    Row #[width: 200, gap: 16] {
        Box #[bg: 0xFF0000, width: 24, height: 24, ICON] {}
        ZStack #[grow: 1, height: 30] { Box #[bg: 0x00FF00, width: 400, height: 10] {} }
    }
}";

#[test]
fn a_fixed_size_child_is_squeezed_by_default() {
    // The choice, written down: a `width` is a size to start from, not a
    // floor, as in CSS. Nothing about existing layouts changes.
    let f = frame_in(&ICON_ROW.replace(", ICON", ""));
    assert!(
        rect_of(&f, 0xFF0000)[2] < 24.0,
        "{:?}",
        rect_of(&f, 0xFF0000)
    );
}

#[test]
fn shrink_zero_keeps_a_fixed_size_child_its_size() {
    let f = frame_in(&ICON_ROW.replace("ICON", "shrink: 0"));
    assert!(
        near(rect_of(&f, 0xFF0000)[2], 24.0),
        "{:?}",
        rect_of(&f, 0xFF0000)
    );
}

#[test]
fn shrink_one_is_the_default() {
    let f = frame_in(&ICON_ROW.replace("ICON", "shrink: 1"));
    let g = frame_in(&ICON_ROW.replace(", ICON", ""));
    assert!(near(rect_of(&f, 0xFF0000)[2], rect_of(&g, 0xFF0000)[2]));
}

#[test]
fn shrink_zero_keeps_a_text_icon_from_collapsing() {
    // The repro from the gap log: the glyph's cell ended up 0 wide.
    let src = |attrs: &str| {
        format!(
            "View Main() {{
    Row #[width: 200, gap: 16] {{
        Text(\"mail\") #[size: 24, wrap: false, width: 24{attrs}]
        ZStack #[grow: 1, height: 30] {{ Box #[bg: 0x00FF00, width: 400, height: 10] {{}} }}
    }}
}}"
        )
    };
    let width_of_green_x = |f: &RenderFrame| rect_of(f, 0x00FF00)[0];
    let squeezed = frame_in(&src(""));
    let kept = frame_in(&src(", shrink: 0"));
    // The green box starts after the text's cell and the 16 gap.
    assert!(
        width_of_green_x(&squeezed) < 40.0 - 0.5,
        "{}",
        width_of_green_x(&squeezed)
    );
    assert!(
        near(width_of_green_x(&kept), 40.0),
        "{}",
        width_of_green_x(&kept)
    );
}

#[test]
fn shrink_works_on_a_container_a_text_and_a_column_axis() {
    let f = frame_in(
        "View Main() {
    Column #[height: 100, gap: 0] {
        Column #[bg: 0xFF0000, height: 80, shrink: 0] {}
        Column #[bg: 0x00FF00, height: 80] {}
    }
}",
    );
    assert!(
        near(rect_of(&f, 0xFF0000)[3], 80.0),
        "{:?}",
        rect_of(&f, 0xFF0000)
    );
    assert!(
        near(rect_of(&f, 0x00FF00)[3], 20.0),
        "{:?}",
        rect_of(&f, 0x00FF00)
    );
}

// ── align_self ──────────────────────────────────────────────────────────

fn row_with(align_self: &str) -> RenderFrame {
    frame_in(&format!(
        "View Main() {{
    Row #[height: 100, align: start] {{
        Box #[bg: 0xFF0000, width: 20, height: 20, align_self: {align_self}] {{}}
    }}
}}"
    ))
}

#[test]
fn align_self_places_a_child_on_the_cross_axis() {
    assert!(near(rect_of(&row_with("start"), 0xFF0000)[1], 0.0));
    assert!(near(rect_of(&row_with("center"), 0xFF0000)[1], 40.0));
    assert!(near(rect_of(&row_with("end"), 0xFF0000)[1], 80.0));
}

#[test]
fn align_self_stretch_fills_the_cross_axis_of_a_parent_that_does_not() {
    let f = frame_in(
        "View Main() {
    Row #[height: 100, align: start] {
        Box #[bg: 0xFF0000, width: 20, align_self: stretch] {}
        Box #[bg: 0x00FF00, width: 20] {}
    }
}",
    );
    assert!(
        near(rect_of(&f, 0xFF0000)[3], 100.0),
        "{:?}",
        rect_of(&f, 0xFF0000)
    );
    assert!(
        near(rect_of(&f, 0x00FF00)[3], 0.0),
        "{:?}",
        rect_of(&f, 0x00FF00)
    );
}

#[test]
fn a_column_child_that_would_stretch_can_be_centred_instead() {
    // The composed-control case from the gap log: a bare `Column` child
    // stretches to the parent's width.
    let f = frame_in(
        "View Main() {
    Column #[width: 300] {
        Box #[bg: 0xFF0000, height: 20, width: 100, align_self: center] {}
        Box #[bg: 0x00FF00, height: 20] {}
    }
}",
    );
    assert!(
        near(rect_of(&f, 0xFF0000)[0], 100.0),
        "{:?}",
        rect_of(&f, 0xFF0000)
    );
    assert!(
        near(rect_of(&f, 0x00FF00)[2], 300.0),
        "{:?}",
        rect_of(&f, 0x00FF00)
    );
}

#[test]
fn align_self_works_on_a_text_and_a_control() {
    let f = frame_in(
        "View Main() {
    var t = \"\"
    Row #[height: 100, align: start] {
        Text(\"hi\") #[align_self: end]
        TextField #[bind: t, bg: 0x335577, height: 20, width: 50, align_self: center]
    }
}",
    );
    let hi = f.texts().iter().find(|t| t.text == "hi").unwrap();
    assert!(hi.y > 70.0, "the text sits at the bottom: {}", hi.y);
    assert!(
        near(rect_of(&f, 0x335577)[1], 40.0),
        "{:?}",
        rect_of(&f, 0x335577)
    );
}

#[test]
fn a_bad_align_self_is_an_error_not_a_default() {
    let parsed =
        parse("View Main() { Row { Box #[align_self: sideways, width: 10, height: 10] {} } }");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let _ = interp.lower_view(&parsed.views[0], &known);
    let errs = format!("{:?}", interp.errors());
    assert!(
        errs.contains("AttributeTypeMismatch") && errs.contains("stretch"),
        "{errs}"
    );
}

// ── a bare TextField ────────────────────────────────────────────────────

const FIELD_ROW: &str = "View Main() {
    var t = \"\"
    Row #[width: 200, gap: 16] {
        Box #[bg: 0xFF0000, width: 24, height: 24] {}
        TextField #[bind: t, bg: 0x335577, FIELD]
        Box #[bg: 0x00FF00, width: 24, height: 24] {}
    }
}";

#[test]
fn a_growing_field_between_two_icons_squeezes_neither() {
    let f = frame_in(&FIELD_ROW.replace("FIELD", "grow: 1"));
    assert!(
        near(rect_of(&f, 0xFF0000)[2], 24.0),
        "{:?}",
        rect_of(&f, 0xFF0000)
    );
    assert!(
        near(rect_of(&f, 0x00FF00)[2], 24.0),
        "{:?}",
        rect_of(&f, 0x00FF00)
    );
    // 200 less two icons and two gaps.
    assert!(
        near(rect_of(&f, 0x335577)[2], 120.0),
        "{:?}",
        rect_of(&f, 0x335577)
    );
    assert!(
        near(rect_of(&f, 0x335577)[3], 36.0),
        "the height is still 36"
    );
}

#[test]
fn a_field_with_a_width_is_that_wide_and_a_bare_one_falls_back_to_200() {
    let f = frame_in(&FIELD_ROW.replace("FIELD", "width: 60"));
    assert!(near(rect_of(&f, 0x335577)[2], 60.0));
    let f = frame_in(
        &FIELD_ROW
            .replace("FIELD", "height: 30")
            .replace("width: 200", "width: 400"),
    );
    assert!(near(rect_of(&f, 0x335577)[2], 200.0));
}

#[test]
fn a_growing_field_in_a_column_spans_the_column() {
    let f = frame_in(
        "View Main() {
    var t = \"\"
    Column #[width: 300] { TextField #[bind: t, bg: 0x335577, grow: 1] }
}",
    );
    assert!(
        near(rect_of(&f, 0x335577)[2], 300.0),
        "{:?}",
        rect_of(&f, 0x335577)
    );
}

/// A field in a 400x300 window with a second one to take focus from it.
struct Fields {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    frame: RenderFrame,
    t: u64,
}

impl Fields {
    fn start(attrs: &str) -> Self {
        let src = format!(
            "View Main() {{
    var q: Str = \"\"
    Column #[p: 20] {{
        TextField #[bind: q, placeholder: \"first\", width: 200, height: 40{attrs}]
    }}
}}"
        );
        let parsed = parse(&src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.set_theme(Theme::default());
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &["Main"]);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut me = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            t: 0,
        };
        me.send(&[]);
        me
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 400.0, 300.0);
    }

    fn tap(&mut self, pos: (f32, f32)) {
        for kind in [EventKind::PointerDown, EventKind::PointerUp] {
            self.t += 50;
            let e = InputEvent {
                kind,
                pos,
                delta: (0.0, 0.0),
                payload: None::<InputPayload>,
                time_ms: self.t,
                mods: byard_core::KeyModifiers::default(),
            };
            self.send(&[e]);
        }
    }

    /// The 2px bars along the field's bottom edge.
    fn bars(&self) -> usize {
        self.frame
            .instances()
            .iter()
            .filter(|b| (b.rect[3] - 2.0).abs() < 0.01 && b.rect[2] > 150.0)
            .count()
    }
}

#[test]
fn the_focus_line_is_drawn_by_default_and_indicator_none_removes_it() {
    let mut plain = Fields::start("");
    assert_eq!(plain.bars(), 0);
    plain.tap((100.0, 40.0));
    assert_eq!(plain.bars(), 1, "the focus line");

    let mut none = Fields::start(", indicator: none");
    none.tap((100.0, 40.0));
    assert_eq!(none.bars(), 0, "indicator: none draws no line");

    let mut line = Fields::start(", indicator: line");
    line.tap((100.0, 40.0));
    assert_eq!(line.bars(), 1);
}

#[test]
fn indicator_can_be_set_by_a_state_block_and_its_padding_is_unchanged() {
    // The intrinsic `p` is a 10 inset; a design system that wants none says so.
    let mut f = Fields::start("");
    f.tap((100.0, 40.0));
    let hint = f
        .frame
        .texts()
        .iter()
        .find(|t| t.text == "first")
        .unwrap()
        .x;
    assert!(
        near(hint, 30.0),
        "20 of column padding and the field's own 10: {hint}"
    );
    let f = Fields::start(", p: (horizontal: 0)");
    let hint = f
        .frame
        .texts()
        .iter()
        .find(|t| t.text == "first")
        .unwrap()
        .x;
    assert!(near(hint, 20.0), "{hint}");
}

#[test]
fn indicator_rejects_other_words() {
    let parsed = parse("View Main() { var q = \"\" TextField #[bind: q, indicator: wavy] }");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &["Main"]);
    let errs = format!("{:?}", interp.errors());
    assert!(
        errs.contains("AttributeTypeMismatch") && errs.contains("\\\"none\\\""),
        "{errs}"
    );
}

// ── bg and radius on Text ───────────────────────────────────────────────

#[test]
fn a_text_with_bg_fills_its_box_including_its_padding() {
    let f = frame_in(
        "View Main() {
    Column { Text(\"hi\") #[bg: 0x445566, p: (4, 10)] }
}",
    );
    let r = rect_of(&f, 0x445566);
    let t = f.texts().iter().find(|t| t.text == "hi").unwrap();
    assert!(near(r[0], 0.0) && near(r[1], 0.0), "{r:?}");
    assert!(near(t.x, 10.0), "the words sit inside the padding: {}", t.x);
    assert!(
        r[2] > 20.0 && r[3] > 8.0,
        "the fill covers the padding too: {r:?}"
    );
}

#[test]
fn a_text_with_radius_rounds_its_fill() {
    let f = frame_in("View Main() { Column { Text(\"hi\") #[bg: 0x445566, radius: 6, p: 4] } }");
    let b = f
        .instances()
        .iter()
        .find(|b| (b.rect[2] - rect_of(&f, 0x445566)[2]).abs() < 0.01)
        .unwrap();
    assert_eq!(b.radii, [6.0; 4]);
}

#[test]
fn a_text_without_bg_draws_no_fill() {
    let f = frame_in("View Main() { Column { Text(\"hi\") #[p: 4, radius: 6] Text(\"yo\") } }");
    assert!(f.instances().is_empty(), "{:?}", f.instances().len());
    assert!(f.decorated().is_empty());
}

#[test]
fn a_translucent_text_fill_blends() {
    let f = frame_in("View Main() { Column { Text(\"hi\") #[bg: 0x80445566, p: 4] } }");
    assert_eq!(f.decorated().len(), 1);
    assert!(f.decorated()[0].base.color[3] < 0.6);
}

// ── on measure feeding a width back ─────────────────────────────────────

/// A card whose field is as wide as the card's measured width less its
/// padding, with the card height read frame by frame.
fn card_heights(sub: &str) -> Vec<f32> {
    let src = format!(
        "View Main() {{
    var t = \"\"
    var w: Float = 0.0
    Column {{
        Column #[bg: 0x112233, p: 16, gap: 8] {{
            on measure => {{ w = it.w }}
            Row {{ TextField #[bind: t, width: w - {sub}, bg: 0x334455] }}
        }}
    }}
}}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(Theme::default());
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let mut frame = RenderFrame::new();
    let mut out = vec![];
    for vw in [360.0, 360.0, 360.0, 300.0, 300.0, 300.0, 420.0, 420.0] {
        interp.tick();
        frame.clear();
        interp.render(&tree, &mut frame, vw, 600.0);
        out.push(rect_of(&frame, 0x112233)[3]);
    }
    out
}

#[test]
fn a_field_sized_from_its_measured_parent_leaves_the_parent_its_height() {
    // Not a failing-first test: the report could not be reproduced (see the
    // notes). This pins the shape that was reported, with exactly the row's
    // inner width and one pixel less, so a regression shows here.
    for sub in ["32.0", "33.0"] {
        for h in card_heights(sub) {
            assert!(near(h, 68.0), "sub {sub}: {h}");
        }
    }
}
