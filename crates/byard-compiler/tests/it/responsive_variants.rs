//! Responsive style variants (RFC-0016): `on width >= md { … }` in a `style`.
//!
//! Read off the frame. The claims are about *which* variant applies at *which*
//! size, which a frame answers exactly and a readback would only approximate.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

fn theme() -> Theme {
    let mut t = Theme::byard_base();
    t.set_breakpoint("md", 600.0);
    t.set_breakpoint("lg", 1000.0);
    t
}

fn lowered(src: &str) -> (Interpreter, Vec<byard_compiler::interp::eval::RenderNode>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_theme(theme());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    (interp, tree)
}

fn frame_at(src: &str, width: f32) -> RenderFrame {
    let (mut interp, tree) = lowered(src);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(&tree, &mut f, width, 400.0);
    f
}

/// The rect of the only 50×50 box in the frame.
fn tile(f: &RenderFrame) -> [f32; 4] {
    *f.instances()
        .iter()
        .map(|b| &b.rect)
        .find(|r| (r[2] - 50.0).abs() < 0.5 && (r[3] - 50.0).abs() < 0.5)
        .expect("the 50×50 tile is emitted")
}

/// A card whose padding is 8 below `md` and 40 from it up.
const PADDED: &str = "View Main() {
    let card = style {
        p: 8
        on width >= md { p: 40 }
    }
    Column #[..card, bg: 0x202020] { Box #[width: 50, height: 50, bg: 0xFF0000] {} }
}";

/// A layout property switches exactly at the breakpoint and not one pixel
/// before it. Layout-class, so this is the half that needed the viewport to
/// reach the layout pass rather than only the paint one.
#[test]
fn a_responsive_padding_switches_exactly_at_the_breakpoint() {
    let below = tile(&frame_at(PADDED, 599.0));
    let at = tile(&frame_at(PADDED, 600.0));
    assert!(
        (below[0] - 8.0).abs() < 0.5,
        "at 599px the narrow padding applies, got {below:?}"
    );
    assert!(
        (at[0] - 40.0).abs() < 0.5,
        "at 600px the `md` padding applies, got {at:?}"
    );
}

/// A direction is a layout property too: two tiles stack below `md` and sit
/// side by side from it up.
#[test]
fn a_responsive_direction_rearranges_the_children() {
    let src = "View Main() {
        let row = style {
            direction: column
            on width >= md { direction: row }
        }
        Box #[..row] {
            Box #[width: 50, height: 50, bg: 0xFF0000] {}
            Box #[width: 50, height: 50, bg: 0x00FF00] {}
        }
    }";
    let tiles = |f: &RenderFrame| -> Vec<[f32; 4]> {
        f.instances()
            .iter()
            .map(|b| b.rect)
            .filter(|r| (r[2] - 50.0).abs() < 0.5)
            .collect()
    };
    let narrow = tiles(&frame_at(src, 400.0));
    let wide = tiles(&frame_at(src, 800.0));
    assert!(
        (narrow[0][0] - narrow[1][0]).abs() < 0.5 && narrow[1][1] > narrow[0][1],
        "narrow: stacked, got {narrow:?}"
    );
    assert!(
        (wide[0][1] - wide[1][1]).abs() < 0.5 && wide[1][0] > wide[0][0],
        "wide: side by side, got {wide:?}"
    );
}

/// `>=` and `<` partition the axis: at every width, including exactly on the
/// breakpoint, one of the two blocks applies and not both.
#[test]
fn at_least_and_below_partition_the_axis_with_no_gap_or_overlap() {
    let src = "View Main() {
        let s = style {
            on width < md { bg: 0xFF0000 }
            on width >= md { bg: 0x00FF00 }
        }
        Column { Box #[..s, width: 50, height: 50] {} }
    }";
    for w in [100.0, 599.0, 599.9, 600.0, 600.1, 2000.0] {
        let f = frame_at(src, w);
        let c = f
            .instances()
            .iter()
            .chain(f.decorated().iter().map(|d| &d.base))
            .find(|b| (b.rect[2] - 50.0).abs() < 0.5)
            .map(|b| b.color)
            .expect("the box is emitted");
        let red = c[0] > 0.9 && c[1] < 0.1;
        let green = c[1] > 0.9 && c[0] < 0.1;
        assert!(
            red != green,
            "at {w}px exactly one block applies, got {c:?}"
        );
        assert_eq!(green, w >= 600.0, "at {w}px, green iff at least `md`");
    }
}

/// A literal width works where a named breakpoint would.
#[test]
fn a_literal_width_is_a_breakpoint() {
    let src = PADDED.replace("on width >= md", "on width >= 700");
    assert!((tile(&frame_at(&src, 699.0))[0] - 8.0).abs() < 0.5);
    assert!((tile(&frame_at(&src, 700.0))[0] - 40.0).abs() < 0.5);
}

/// A breakpoint nobody declared is a compile error naming the nearest one,
/// reported once however many times the element is lowered.
#[test]
fn an_undeclared_breakpoint_is_reported_once_with_a_hint() {
    let src = "View Main() {
        var items = [1, 2, 3]
        let s = style { on width >= mdd { p: 40 } }
        Column { for i in items { Box #[..s, width: 50, height: 50] {} } }
    }";
    let (mut interp, tree) = lowered(src);
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(&tree, &mut f, 800.0, 400.0);
    let errs: Vec<String> = interp.errors().iter().map(|e| format!("{e:?}")).collect();
    let bp: Vec<&String> = errs
        .iter()
        .filter(|e| e.contains("UnknownBreakpoint"))
        .collect();
    assert_eq!(bp.len(), 1, "one written block, one report: {errs:?}");
    assert!(
        bp[0].contains("mdd") && bp[0].contains("\"md\""),
        "{errs:?}"
    );
}
