//! The weight a `Text` resolves to (RFC-0034), without a GPU or a font.
//!
//! The readback suite proves the weight reaches the glyphs; what it cannot
//! prove portably is *which* weight, because that depends on which faces the
//! machine's fonts ship. This one reads the number off the frame, so it says
//! `weight: 750` means 750 on every platform, and it is the test that should
//! have existed before any pixel was compared.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// The weight of the first text line the source produces.
fn weight_of(src: &str) -> u16 {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    frame.texts().first().expect("a text line").weight
}

fn text_with(attrs: &str) -> String {
    format!("View Main() {{ Column #[width: 400] {{ Text(\"t\") #[{attrs}] }} }}")
}

/// Every keyword lands on its documented axis value.
#[test]
fn the_keywords_are_aliases_for_axis_values() {
    for (kw, axis) in [
        ("thin", 100),
        ("regular", 400),
        ("medium", 500),
        ("bold", 700),
    ] {
        assert_eq!(
            weight_of(&text_with(&format!("size: 20, weight: {kw}"))),
            axis,
            "`{kw}` must resolve to {axis}"
        );
    }
}

/// A number passes through as itself, including values no keyword names.
#[test]
fn a_number_is_the_axis_value() {
    for axis in [100, 250, 600, 750, 900] {
        assert_eq!(
            weight_of(&text_with(&format!("size: 20, weight: {axis}"))),
            axis,
            "`weight: {axis}` must resolve to {axis} and not snap to a keyword"
        );
    }
}

/// Saying nothing is regular.
#[test]
fn the_default_is_regular() {
    assert_eq!(weight_of(&text_with("size: 20")), 400);
}
