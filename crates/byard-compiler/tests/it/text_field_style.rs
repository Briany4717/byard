//! A `TextField` reads the `color:`, `p:` and `typo:` written on it.
//!
//! Its text was always white, its placeholder always the same grey, and both
//! always 10 from the left edge, whatever the field said: a field on a light
//! theme drew white on white.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::TextLine;
use byard_core::frame::RenderFrame;

/// Renders one field at (20, 20) holding `value` and returns its text line.
fn field(value: &str, attrs: &str) -> TextLine {
    let src = format!(
        "View Main() {{
    var q: Str = \"{value}\"
    Column #[p: 20] {{
        TextField #[bind: q, placeholder: \"hint\", width: 200, height: 40{attrs}]
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
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let shown = if value.is_empty() { "hint" } else { value };
    frame
        .texts()
        .iter()
        .find(|t| t.text == shown)
        .unwrap_or_else(|| panic!("{shown} in the frame"))
        .clone()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn the_text_starts_at_the_left_padding() {
    assert!(close(field("abc", "").x, 30.0), "10 by default");
    assert!(close(field("abc", ", p: (horizontal: 18)").x, 38.0));
    assert!(close(field("abc", ", p: 24").x, 44.0));
    assert!(close(field("abc", ", pl: 6").x, 26.0));
    // The placeholder sits where the text will.
    assert!(close(field("", ", p: (horizontal: 18)").x, 38.0));
}

#[test]
fn the_text_is_drawn_in_the_fields_color() {
    let white = field("abc", "").color;
    assert!(close(white[0], 1.0) && close(white[1], 1.0) && close(white[2], 1.0));
    let red = field("abc", ", color: 0xFF0000").color;
    assert!(
        close(red[0], 1.0) && close(red[1], 0.0) && close(red[2], 0.0) && close(red[3], 1.0),
        "{red:?}"
    );
}

#[test]
fn the_placeholder_is_the_same_color_fainter() {
    let value = field("abc", ", color: 0xFF0000").color;
    let hint = field("", ", color: 0xFF0000").color;
    assert!(close(hint[0], value[0]) && close(hint[1], value[1]) && close(hint[2], value[2]));
    assert!(hint[3] < value[3] * 0.7 && hint[3] > 0.3, "{hint:?}");
    // With no colour written it is the grey it always was.
    let grey = field("", "").color;
    assert!(
        close(grey[0], grey[1]) && grey[0] < 0.5 && close(grey[3], 1.0),
        "{grey:?}"
    );
}

#[test]
fn size_is_read_and_the_default_stays() {
    assert!(close(field("abc", "").font_size, 16.0));
    assert!(close(field("abc", ", size: 22").font_size, 22.0));
}
