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

/// A computed anchor name resolves, and does so without interning it.
///
/// The interner is process-global and append-only for the life of the process,
/// so a name built per frame — `"row-{i}"` in a list, say — would grow memory
/// without bound and take the interner's write lock on the logic thread every
/// time a new one appeared. The lookup compares text instead.
///
/// Asserted through behaviour rather than by inspecting the interner: the
/// overlay lands on its anchor, which it can only do if a string that was
/// never interned still found the entry.
#[test]
fn a_computed_anchor_name_resolves() {
    let src = "View Main() {
    var which: Str = \"field\"
    Column #[bg: 0x101010, p: 20, width: 400, height: 300] {
        Box #[bg: 0x223344, width: 120, height: 30] as field {}
    }
    Overlay #[modal: false] {
        Box #[bg: 0xAA3344, width: 80, height: 40, anchor_to: \"{which}\",
              anchor_edge: below, anchor_gap: 6] {}
    }
}";
    let rects = boxes(src);
    let anchor = find(&rects, 120.0, 30.0);
    let panel = find(&rects, 80.0, 40.0);

    assert!(
        (panel[1] - (anchor[1] + anchor[3] + 6.0)).abs() < 0.5,
        "an interpolated name must place the same as a literal one: \
         anchor {anchor:?} panel {panel:?}"
    );
}

// ── `width: match(ref)`: a panel as wide as the element it hangs from ──────

/// Errors reported for `src`, without asserting the frame.
fn errors_of(src: &str) -> Vec<String> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let _ = interp.lower_view(&parsed.views[0], &known);
    interp.errors().iter().map(|e| format!("{e:?}")).collect()
}

/// A view whose field is `field_w` wide and whose panel matches it.
fn matched(field_w: i32) -> String {
    format!(
        "View Main() {{
    Column #[bg: 0x101010, p: 20, gap: 10, width: 400, height: 300] {{
        Box #[bg: 0x223344, width: {field_w}, height: 30] as field {{}}
    }}
    Overlay #[modal: false] {{
        Box #[bg: 0xAA3344, height: 40, anchor_to: \"field\", width: match(field)] {{
            Box #[bg: 0x115522, height: 12] {{}}
        }}
    }}
}}"
    )
}

/// The panel ends up exactly as wide as its anchor, whatever that is.
///
/// Two widths rather than one, because a single case passes for a panel that
/// happened to be that wide already, which is the shape of test this project
/// keeps having to rewrite.
#[test]
fn a_matched_panel_is_exactly_as_wide_as_its_anchor() {
    for field_w in [120.0_f32, 260.0] {
        #[allow(clippy::cast_possible_truncation)]
        let rects = boxes(&matched(field_w as i32));
        let panel = find(&rects, field_w, 40.0);
        assert!(
            (panel[2] - field_w).abs() < 0.5,
            "the panel must be {field_w} wide, got {panel:?}"
        );
        // And it is still placed against the anchor, so matching the width did
        // not cost the placement.
        let anchor = find(&rects, field_w, 30.0);
        assert!(
            (panel[0] - anchor[0]).abs() < 0.5,
            "anchor {anchor:?} panel {panel:?}"
        );
    }
}

/// The panel's *children* are laid out at the matched width too.
///
/// This is the assertion that rules out the cheap version of this feature.
/// Widening the finished rect and calling it done leaves every row inside the
/// panel at the width the panel used to have, so the panel is the right size
/// and its contents rattle around inside it.
#[test]
fn the_matched_width_reaches_the_panels_children() {
    let rects = boxes(&matched(260));
    let row = rects
        .iter()
        .find(|r| (r[3] - 12.0).abs() < 0.5)
        .unwrap_or_else(|| panic!("no row in {rects:?}"));
    assert!(
        (row[2] - 260.0).abs() < 0.5,
        "the row inside the panel must fill the matched width, got {row:?}: \
         a rect widened after layout leaves its children where they were"
    );
}

/// A width matched against a name nobody tagged is a diagnostic naming the
/// nearest one, not a panel that silently keeps its own width.
#[test]
fn a_matched_width_naming_no_anchor_is_a_diagnostic() {
    let src = matched(120).replace("match(field)", "match(feild)");
    let errs = errors_of(&src);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("MisplacedAnchorTail"), "{errs:?}");
    assert!(errs[0].contains("feild"), "{errs:?}");
    assert!(
        errs[0].contains("field"),
        "the nearest tag must be offered: {errs:?}"
    );
}

/// And on an element that anchors to nothing it is a diagnostic too: the width
/// comes from the anchor's rect, and there is no anchor to read one from.
#[test]
fn a_matched_width_without_an_anchor_is_a_diagnostic() {
    let src = "View Main() {
    Column #[width: 400, height: 300] {
        Box #[bg: 0x223344, width: 120, height: 30] as field {}
        Box #[bg: 0xAA3344, height: 40, width: match(field)] {}
    }
}";
    let errs = errors_of(src);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("MisplacedAnchorTail"), "{errs:?}");
    assert!(errs[0].contains("anchor_to"), "{errs:?}");
}

/// A matched width costs a second layout pass on the frame it changes, and on
/// no other.
///
/// Stated as a *difference* against the same screen with a fixed width rather
/// than as an absolute count, because a frame already runs one retained pass
/// of its own and that number is nobody's invariant. What is an invariant is
/// that matching a width adds nothing to a steady frame.
///
/// This is the assertion that keeps the feature affordable, and the one that
/// would fail if the width were re-applied unconditionally: `set_style` marks
/// a node dirty in Taffy whether or not the value moved, so writing the same
/// width every frame would recompute the panel every frame and turn the
/// retained path back into a full one for any screen with a dropdown open.
#[test]
fn a_matched_width_adds_no_layout_work_to_a_steady_frame() {
    use byard_core::atlas::layout::path_counters;

    fn steady_recomputes(src: &str) -> u64 {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut render_once = || {
            path_counters::reset();
            interp.tick();
            let mut f = RenderFrame::new();
            interp.render(&tree, &mut f, W, H);
            path_counters::snapshot()
        };
        // First frame builds, second settles; the third is the steady one.
        let _ = render_once();
        let _ = render_once();
        let steady = render_once();
        assert_eq!(steady.full_computes, 0, "steady frame rebuilt: {steady:?}");
        steady.retained_recomputes
    }

    let fixed = matched(120).replace("width: match(field)", "width: 120");
    assert_eq!(
        steady_recomputes(&matched(120)),
        steady_recomputes(&fixed),
        "a matched width must cost a steady frame nothing over a fixed one"
    );
}

// ── RFC-0017 §Positioning: an absolute `(x, y)` in the viewport ────────────

/// A view placing an 80×40 panel at an absolute offset.
fn placed_at(x: i32, y: i32) -> String {
    format!(
        "View Main() {{
    Column #[bg: 0x101010, p: 20, width: 400, height: 300] {{
        Box #[bg: 0x223344, width: 120, height: 30] as field {{}}
    }}
    Overlay #[modal: false] {{
        Box #[bg: 0xAA3344, width: 80, height: 40, at: ({x}, {y})] {{}}
    }}
}}"
    )
}

/// The panel's top-left is exactly the offset written, measured from the
/// viewport's own top-left.
#[test]
fn an_absolute_overlay_lands_at_the_offset_it_was_given() {
    let panel = find(&boxes(&placed_at(120, 48)), 80.0, 40.0);
    assert!(
        (panel[0] - 120.0).abs() < 0.5 && (panel[1] - 48.0).abs() < 0.5,
        "the panel must sit at (120, 48), got {panel:?}"
    );
}

/// The offset is viewport-space, not centre-relative and not
/// content-relative.
///
/// The two window sizes are the whole test: an offset measured from anything
/// that moves with the window would give two different answers here, and one
/// of them would look right.
#[test]
fn an_absolute_offset_is_measured_from_the_viewport_origin() {
    fn panel_at(src: &str, w: f32, h: f32) -> [f32; 4] {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        interp.tick();
        let mut frame = RenderFrame::new();
        interp.render(&tree, &mut frame, w, h);
        let rects: Vec<[f32; 4]> = frame
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
            .collect();
        find(&rects, 80.0, 40.0)
    }
    let src = placed_at(120, 48);
    let small = panel_at(&src, 400.0, 300.0);
    let large = panel_at(&src, 900.0, 700.0);
    assert_eq!(
        (small[0], small[1]),
        (large[0], large[1]),
        "the offset moved with the window, so it is not viewport-space"
    );
    // And it is *the* offset, not merely a stable one: without this the test
    // passes with the feature switched off, because a panel left at the
    // wrapper's origin is viewport-invariant too.
    assert!(
        (small[0] - 120.0).abs() < 0.5 && (small[1] - 48.0).abs() < 0.5,
        "got {small:?}"
    );
}

/// An off-screen offset stays off-screen. Quietly pulling it back would make a
/// wrong coordinate look like a layout decision, which is the harder bug.
#[test]
fn an_absolute_overlay_off_the_viewport_is_not_clamped_back() {
    let panel = find(&boxes(&placed_at(600, 500)), 80.0, 40.0);
    assert!(
        panel[0] > 500.0 && panel[1] > 400.0,
        "the panel must stay where it was put, got {panel:?}"
    );
}

/// `at:` and `anchor_to:` are two answers to one question, so writing both is
/// refused rather than resolved by a precedence rule nobody would remember.
#[test]
fn an_absolute_offset_together_with_an_anchor_is_a_diagnostic() {
    let src = "View Main() {
    Column #[width: 400, height: 300] {
        Box #[bg: 0x223344, width: 120, height: 30] as field {}
    }
    Overlay #[modal: false] {
        Box #[width: 80, height: 40, at: (10, 10), anchor_to: \"field\"] {}
    }
}";
    let errs = errors_of(src);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("MisplacedAnchorTail"), "{errs:?}");
    assert!(errs[0].contains("anchor_to"), "{errs:?}");
}
