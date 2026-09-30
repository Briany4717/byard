//! A `Button` the author wrote no layout for still looks like a button.
//!
//! Before the theme supplied defaults, `Button("Go") #[bg: …]` hugged its
//! label: a pill exactly one text line tall with the label flush against its
//! left edge, so every example that looked right carried a hand-written `p:`.
//! These read the laid-out box and label off the frame, so they need no GPU.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::{BUTTON_MIN_SIZE, BUTTON_PADDING};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// The button's background rect and its label's left edge, for a Button
/// written with `attrs` (a `bg` is always added so the box is painted).
fn button(attrs: &str) -> ([f32; 4], f32) {
    labelled("Hover target", attrs)
}

/// As [`button`], with the label given.
fn labelled(label: &str, attrs: &str) -> ([f32; 4], f32) {
    let src = format!(
        "View Main() {{ Column #[width: 400, align: start] {{ \
         Button(\"{label}\") #[bg: 0x2A3242, radius: 8{attrs}] }} }}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let rect = frame.instances().first().expect("the button's box").rect;
    let label = frame.texts().first().expect("the button's label");
    (rect, label.x)
}

/// A bare Button is at least the touch height, and its label is inset by the
/// theme's horizontal padding on both sides, so it sits centred.
#[test]
fn a_bare_button_gets_padding_and_a_touch_height() {
    let ([x, _, w, h], label_x) = button("");
    assert!(
        h >= BUTTON_MIN_SIZE,
        "a bare Button must be at least {BUTTON_MIN_SIZE}px tall, got {h}"
    );
    let inset = label_x - x;
    assert!(
        (inset - BUTTON_PADDING.1).abs() < 0.5,
        "the label must be inset {}px from the left edge, got {inset}",
        BUTTON_PADDING.1
    );
    assert!(
        w > 2.0 * BUTTON_PADDING.1,
        "the box must be wider than its padding alone, got {w}"
    );
}

/// A single glyph with tight padding still gets a square touch target, not a
/// tall sliver: the floor applies to both axes.
#[test]
fn a_one_glyph_button_is_a_square_target() {
    let ([_, _, w, h], _) = labelled("x", ", p: 8");
    assert!(
        w >= BUTTON_MIN_SIZE && h >= BUTTON_MIN_SIZE,
        "a one-glyph Button must be at least {BUTTON_MIN_SIZE}px square, got {w}x{h}"
    );
    // An explicit width is still the author's call.
    let ([_, _, w, _], _) = labelled("x", ", p: 8, width: 30");
    assert!(
        (w - 30.0).abs() < 0.5,
        "`width: 30` must stay 30px, got {w}"
    );
}

/// Explicit padding replaces the default rather than adding to it.
#[test]
fn explicit_padding_wins() {
    let ([x, ..], label_x) = button(", p: 3");
    assert!(
        (label_x - x - 3.0).abs() < 0.5,
        "`p: 3` must inset the label 3px, got {}",
        label_x - x
    );
    // One side overrides only that side.
    let ([x, ..], label_x) = button(", pl: 5");
    assert!(
        (label_x - x - 5.0).abs() < 0.5,
        "`pl: 5` must inset the label 5px, got {}",
        label_x - x
    );
}

/// An explicit height is the author's call: the touch floor does not raise it.
#[test]
fn explicit_height_wins_over_the_touch_floor() {
    let ([_, _, _, h], _) = button(", height: 24");
    assert!(
        (h - 24.0).abs() < 0.5,
        "`height: 24` must stay 24px, got {h}"
    );
}

/// A fixed-width Button centres its label, not left-aligns it.
#[test]
fn a_wide_button_centres_its_label() {
    let ([x, _, w, _], label_x) = button(", width: 300");
    let inset = label_x - x;
    assert!(
        inset > BUTTON_PADDING.1 + 20.0 && inset < w / 2.0,
        "the label must sit centred in a 300px button, inset was {inset}"
    );
    // `align: start` puts it back where the author asked.
    let ([x, ..], label_x) = button(", width: 300, align: start");
    assert!(
        (label_x - x - BUTTON_PADDING.1).abs() < 0.5,
        "`align: start` must left-align the label at the padding, got {}",
        label_x - x
    );
}
