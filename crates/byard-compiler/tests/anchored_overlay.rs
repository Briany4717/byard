//! Element-relative overlays (RFC-0036).
//!
//! The claim is geometric: an overlay naming an anchor lands against that
//! anchor's laid-out rect, not against the viewport. So the assertions are
//! about where boxes end up, read out of the frame the interpreter produced.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

const W: f32 = 400.0;
const H: f32 = 300.0;

/// Renders `src` and returns every solid box's **painted** `[x, y, w, h]`.
///
/// The painted rect, not the laid-out one: an anchored overlay is moved by a
/// paint transform rather than by relaying it out, so reading `rect` alone
/// would report where layout left it and miss the entire feature.
fn boxes(src: &str) -> Vec<[f32; 4]> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, W, H);
    frame
        .instances()
        .iter()
        .map(|b| {
            [
                b.rect[0] + b.transform.translate[0],
                b.rect[1] + b.transform.translate[1],
                b.rect[2],
                b.rect[3],
            ]
        })
        .collect()
}

/// The anchor is 120×30 at a known place; the overlay panel is 80×40.
fn source(edge: &str, gap: i32, extra: &str) -> String {
    format!(
        "View Main() {{
    Column #[bg: 0x101010, p: 20, gap: 10, width: 400, height: 300] {{
        Box #[bg: 0x223344, width: 120, height: 30] as field {{}}
    }}
    Overlay #[modal: false] {{
        Box #[bg: 0xAA3344, width: 80, height: 40, anchor_to: \"field\",
              anchor_edge: {edge}, anchor_gap: {gap}{extra}] {{}}
    }}
}}"
    )
}

/// The panel is the 80×40 one; the anchor is the 120×30 one.
fn find(rects: &[[f32; 4]], w: f32, h: f32) -> [f32; 4] {
    *rects
        .iter()
        .find(|r| (r[2] - w).abs() < 0.5 && (r[3] - h).abs() < 0.5)
        .unwrap_or_else(|| panic!("no {w}x{h} box in {rects:?}"))
}

#[test]
fn an_overlay_lands_below_its_anchor_with_the_gap_between_them() {
    let rects = boxes(&source("below", 6, ""));
    let anchor = find(&rects, 120.0, 30.0);
    let panel = find(&rects, 80.0, 40.0);

    assert!(
        (panel[1] - (anchor[1] + anchor[3] + 6.0)).abs() < 0.5,
        "the panel's top must sit one gap under the anchor's bottom: anchor {anchor:?} panel {panel:?}"
    );
    assert!(
        (panel[0] - anchor[0]).abs() < 0.5,
        "`start` alignment lines their left edges up: anchor {anchor:?} panel {panel:?}"
    );
}

#[test]
fn align_center_and_end_line_the_panel_up_along_the_anchor() {
    let centred = find(
        &boxes(&source("below", 0, ", anchor_align: center")),
        80.0,
        40.0,
    );
    let ended = find(
        &boxes(&source("below", 0, ", anchor_align: end")),
        80.0,
        40.0,
    );
    let anchor = find(&boxes(&source("below", 0, "")), 120.0, 30.0);

    assert!(
        (centred[0] - (anchor[0] + (120.0 - 80.0) / 2.0)).abs() < 0.5,
        "centre aligns the midpoints: anchor {anchor:?} panel {centred:?}"
    );
    assert!(
        (ended[0] - (anchor[0] + 120.0 - 80.0)).abs() < 0.5,
        "end aligns the far edges: anchor {anchor:?} panel {ended:?}"
    );
}

/// The reason flipping is on by default: an autocomplete that renders off the
/// bottom of the window is a bug in almost every case.
#[test]
fn an_overlay_that_would_leave_the_viewport_flips_to_the_other_side() {
    // The anchor sits near the bottom, so `below` does not fit and `above`
    // does. Same source, only the anchor's offset differs.
    let src = "View Main() {
    Column #[bg: 0x101010, p: 20, width: 400, height: 300] {
        Spacer #[grow: 1]
        Box #[bg: 0x223344, width: 120, height: 30] as field {}
    }
    Overlay #[modal: false] {
        Box #[bg: 0xAA3344, width: 80, height: 40, anchor_to: \"field\",
              anchor_edge: below, anchor_gap: 6] {}
    }
}";
    let rects = boxes(src);
    let anchor = find(&rects, 120.0, 30.0);
    let panel = find(&rects, 80.0, 40.0);

    // Asserted as the exact flipped position, not merely "somewhere above".
    // An overlay left unplaced at the origin also sits above an anchor near
    // the bottom, so the loose form passed with the feature switched off.
    let expected_y = anchor[1] - 6.0 - panel[3];
    assert!(
        (panel[1] - expected_y).abs() < 0.5,
        "the panel must flip to exactly one gap above the anchor \
         (expected y {expected_y}): anchor {anchor:?} panel {panel:?}"
    );
    assert!(
        (panel[0] - anchor[0]).abs() < 0.5,
        "and keep its `start` alignment through the flip: \
         anchor {anchor:?} panel {panel:?}"
    );
}

/// `anchor_flip: false` is the opt-out, and it has to actually opt out.
#[test]
fn flipping_can_be_turned_off() {
    let src = "View Main() {
    Column #[bg: 0x101010, p: 20, width: 400, height: 300] {
        Spacer #[grow: 1]
        Box #[bg: 0x223344, width: 120, height: 30] as field {}
    }
    Overlay #[modal: false] {
        Box #[bg: 0xAA3344, width: 80, height: 40, anchor_to: \"field\",
              anchor_edge: below, anchor_gap: 6, anchor_flip: false] {}
    }
}";
    let rects = boxes(src);
    let anchor = find(&rects, 120.0, 30.0);
    let panel = find(&rects, 80.0, 40.0);

    assert!(
        panel[1] > anchor[1],
        "with flipping off the panel stays below, clamped into view: \
         anchor {anchor:?} panel {panel:?}"
    );
}

/// An overlay naming nothing keeps RFC-0017's viewport behaviour untouched.
#[test]
fn an_unanchored_overlay_is_unaffected() {
    let src = "View Main() {
    Column #[bg: 0x101010, p: 20, width: 400, height: 300] {
        Box #[bg: 0x223344, width: 120, height: 30] as field {}
    }
    Overlay #[modal: false] {
        Box #[bg: 0xAA3344, width: 80, height: 40, anchor: center] {}
    }
}";
    let panel = find(&boxes(src), 80.0, 40.0);
    assert!(
        (panel[0] - (W - 80.0) / 2.0).abs() < 1.0 && (panel[1] - (H - 40.0) / 2.0).abs() < 1.0,
        "a viewport-anchored overlay still centres: panel {panel:?}"
    );
}

/// A misspelt anchor is a compile error, not a silently mis-placed overlay.
///
/// This is the whole reason the RFC chose a declared name over a free-form
/// offset: without the check, a typo produces an overlay that sits wherever
/// layout happened to leave it, which reads as a placement bug and sends the
/// reader looking in the wrong place entirely.
#[test]
fn a_misspelt_anchor_is_reported_with_a_hint() {
    let src = "View Main() {
    Column #[width: 300, height: 200] {
        Box #[width: 100, height: 30] as searchField {}
    }
    Overlay #[modal: false] {
        Box #[anchor_to: \"serchField\"] { Text(\"typo\") }
    }
}";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &known);

    let rendered = format!("{:?}", interp.errors());
    assert!(
        rendered.contains("UnknownAnchor") && rendered.contains("serchField"),
        "expected an UnknownAnchor for the typo, got {rendered}"
    );
    assert!(
        rendered.contains("searchField"),
        "and a hint pointing at the real name, got {rendered}"
    );
}

/// Anchoring forward is refused, which is what makes a cycle unwritable.
#[test]
fn anchoring_to_a_later_element_is_refused() {
    let src = "View Main() {
    Overlay #[modal: false] {
        Box #[anchor_to: \"later\"] { Text(\"forward\") }
    }
    Column #[width: 300, height: 200] {
        Box #[width: 100, height: 30] as later {}
    }
}";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &known);

    assert!(
        format!("{:?}", interp.errors()).contains("UnknownAnchor"),
        "a forward reference must be refused: {:?}",
        interp.errors()
    );
}

/// An element cannot anchor to itself, which is the smallest cycle.
#[test]
fn an_element_cannot_anchor_to_itself() {
    let src = "View Main() {
    Overlay #[modal: false] {
        Box #[anchor_to: \"me\"] as me { Text(\"self\") }
    }
}";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &known);

    assert!(
        format!("{:?}", interp.errors()).contains("UnknownAnchor"),
        "self-anchoring must be refused: {:?}",
        interp.errors()
    );
}
