//! A text field or a slider written with `grow: 1` takes the free space of
//! its row. Both used to be a fixed 200 wide whatever `grow` said, with no
//! diagnostic, so a search field could not span its window.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// Renders `row`'s children in a 400-wide row with 20 of padding and returns
/// every solid box's `[x, y, w, h]`.
fn boxes(children: &str) -> Vec<[f32; 4]> {
    let src = format!(
        "View Main() {{
    var text: Str = \"\"
    var level: Float = 0.5
    Row #[p: 20, gap: 10, width: 400] {{
        {children}
    }}
}}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame.instances().iter().map(|b| b.rect).collect()
}

fn has(rects: &[[f32; 4]], w: f32, h: f32) -> bool {
    rects
        .iter()
        .any(|r| (r[2] - w).abs() < 0.5 && (r[3] - h).abs() < 0.5)
}

#[test]
fn a_text_field_that_grows_fills_its_row() {
    let rects = boxes("TextField #[bind: text, bg: 0x223344, height: 40, grow: 1]");
    assert!(has(&rects, 360.0, 40.0), "{rects:?}");
}

#[test]
fn a_text_field_shares_the_row_with_what_is_beside_it() {
    let rects = boxes(
        "TextField #[bind: text, bg: 0x223344, height: 40, grow: 1]
        Box #[bg: 0x445566, width: 80, height: 40] {}",
    );
    // 360 of room, less the 80 box and the 10 gap.
    assert!(has(&rects, 270.0, 40.0), "{rects:?}");
}

#[test]
fn without_grow_a_text_field_keeps_its_width() {
    let rects = boxes("TextField #[bind: text, bg: 0x223344, height: 40]");
    assert!(has(&rects, 200.0, 40.0), "{rects:?}");
    let rects = boxes("TextField #[bind: text, bg: 0x223344, height: 40, width: 120]");
    assert!(has(&rects, 120.0, 40.0), "{rects:?}");
}

/// The slider's track is as wide as the slider.
#[test]
fn a_slider_that_grows_fills_its_row() {
    let fixed = boxes("Slider #[bind: level, min: 0.0, max: 1.0]");
    let grown = boxes("Slider #[bind: level, min: 0.0, max: 1.0, grow: 1]");
    let widest = |rects: &[[f32; 4]]| {
        rects
            .iter()
            .filter(|r| r[3] < 30.0)
            .map(|r| r[2])
            .fold(0.0_f32, f32::max)
    };
    assert!((widest(&fixed) - 200.0).abs() < 0.5, "{fixed:?}");
    assert!((widest(&grown) - 360.0).abs() < 0.5, "{grown:?}");
}
