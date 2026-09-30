//! Which boxes are faded as one picture (RFC-0011 T4), read off the frame.
//!
//! The pixels are in `byard-platform`; this is the decision. A translucent
//! container with children opens a group and draws its contents opaque inside
//! it; a leaf, an opaque box, and a box inside an open group keep the
//! per-instance path they always had.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

fn frame_of(src: &str) -> RenderFrame {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    frame
}

/// Every alpha the frame's solid and decorated boxes carry.
fn alphas(frame: &RenderFrame) -> Vec<f32> {
    frame
        .instances()
        .iter()
        .map(|b| b.color[3] * b.transform.opacity)
        .chain(
            frame
                .decorated()
                .iter()
                .map(|d| d.base.color[3] * d.opacity),
        )
        .collect()
}

#[test]
fn a_translucent_card_with_children_is_one_group_drawn_opaque_inside() {
    let frame = frame_of(
        "View Main() { Column { Box #[opacity: 0.5, width: 100, height: 100, bg: 0xFF0000] {
            Box #[width: 40, height: 40, bg: 0x0000FF] {}
        } } }",
    );
    assert_eq!(
        frame.groups().len(),
        1,
        "one translucent container, one group"
    );
    assert!((frame.groups()[0].opacity - 0.5).abs() < 1e-6);
    // Inside the group everything is drawn opaque; the alpha is applied once,
    // by the composite. A 0.5 left on any primitive would be applied twice.
    for a in alphas(&frame) {
        assert!(
            (a - 1.0).abs() < 1e-6,
            "inside a group every primitive draws opaque, got alpha {a}"
        );
    }
}

/// The group's opacity is the *effective* one: a translucent ancestor that is
/// not itself a group still counts.
#[test]
fn the_group_carries_the_effective_opacity() {
    let frame = frame_of(
        "View Main() { Column { Box #[opacity: 0.5, width: 100, height: 100] {
            Box #[opacity: 0.5, width: 80, height: 80, bg: 0xFF0000] {
                Box #[width: 40, height: 40, bg: 0x0000FF] {}
            }
        } } }",
    );
    // The outer box has a child, so it opens the group at 0.5; the inner one
    // is inside it and falls back, drawing at its own relative 0.5.
    assert_eq!(frame.groups().len(), 1);
    assert!((frame.groups()[0].opacity - 0.5).abs() < 1e-6);
}

#[test]
fn a_translucent_leaf_keeps_the_per_instance_path() {
    let frame = frame_of(
        "View Main() { Column { Box #[opacity: 0.5, width: 100, height: 100, bg: 0xFF0000] {} } }",
    );
    assert!(frame.groups().is_empty(), "a leaf has nothing to overlap");
    assert!(alphas(&frame).iter().any(|a| (a - 0.5).abs() < 1e-6));
}

/// The fast path, asserted: an opaque container opens nothing. Without this, a
/// regression that grouped every container would render identically and cost
/// an offscreen pass per box.
#[test]
fn an_opaque_container_opens_no_group() {
    let frame = frame_of(
        "View Main() { Column { Box #[width: 100, height: 100, bg: 0xFF0000] {
            Box #[width: 40, height: 40, bg: 0x0000FF] {}
        } } }",
    );
    assert!(frame.groups().is_empty());
}
