//! `color.alpha(0.6)`: a colour at an opacity, theme tokens included.
//!
//! A translucent tint of a theme colour had to be written out as its own hex
//! (`0xCCFFC46B` beside `t.secondary`), so it stayed the same when the theme
//! changed. `t.secondary.alpha(0.8)` follows the token.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// The fill colour of the one box `body` draws, in linear RGBA.
fn fill(body: &str) -> [f32; 4] {
    let parsed = parse(&format!(
        "View Main() {{\n    inject Theme as t\n    Box #[width: 10, height: 10, bg: {body}] {{}}\n}}"
    ));
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let checked = byard_compiler::infer::check_views(&parsed.views).errors;
    assert!(checked.is_empty(), "{checked:?}");
    let mut interp = Interpreter::new();
    interp.set_theme(Theme::default());
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 100.0, 100.0);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    // An opaque fill is a plain instance; a translucent one is drawn as a
    // decorated box.
    frame
        .instances()
        .first()
        .map(|b| b.color)
        .or_else(|| frame.decorated().first().map(|d| d.base.color))
        .unwrap_or_else(|| panic!("a fill for {body}"))
}

#[test]
fn a_literal_takes_the_opacity_and_keeps_its_colour() {
    let solid = fill("0xFF0000");
    let half = fill("0xFF0000.alpha(0.5)");
    assert!((solid[3] - 1.0).abs() < 1e-3, "{solid:?}");
    assert!((half[3] - 0.5).abs() < 0.01, "{half:?}");
    assert!(
        (half[0] - solid[0]).abs() < 1e-3,
        "same red: {half:?} {solid:?}"
    );
}

/// Fully transparent stays transparent: `0x00RRGGBB` is not read back as
/// an opaque `0xRRGGBB`.
#[test]
fn zero_is_transparent_not_opaque() {
    let none = fill("0x3B82F6.alpha(0.0)");
    assert!(none[3].abs() < 1e-3, "{none:?}");
}

/// A theme token takes it too, and an alpha already there is replaced.
#[test]
fn a_theme_token_and_an_alpha_colour_take_it() {
    let token = fill("t.primary.alpha(0.25)");
    assert!((token[3] - 0.25).abs() < 0.01, "{token:?}");
    let solid = fill("t.primary");
    assert!((token[0] - solid[0]).abs() < 1e-3, "{token:?} {solid:?}");
    let replaced = fill("0x80FF0000.alpha(1.0)");
    assert!((replaced[3] - 1.0).abs() < 1e-3, "{replaced:?}");
}

/// Out of range is clamped rather than wrapped into another colour.
#[test]
fn an_opacity_out_of_range_is_clamped() {
    assert!((fill("0xFF0000.alpha(2.0)")[3] - 1.0).abs() < 1e-3);
    assert!(fill("0xFF0000.alpha(-1.0)")[3].abs() < 1e-3);
}
