//! Which texts turn through a rotated group (RFC-0011), read off the frame.
//!
//! Rotation is the one part of a transform a text run cannot bake into its
//! anchor and size, so a rotated run, or a rotated box holding one, is drawn
//! into a group the composite turns. Everything else keeps the direct path,
//! and must not pay the full redraw a rotated group costs.

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

#[test]
fn a_rotated_text_is_one_rotated_group_stored_upright() {
    let frame = frame_of("View Main() { Column { Text(\"turned\") #[rotate: 90deg, size: 20] } }");
    assert_eq!(frame.groups().len(), 1);
    let group = &frame.groups()[0];
    assert!((group.rotate - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    assert!((group.opacity - 1.0).abs() < 1e-6);
    assert!(frame.has_rotated_group());
    // Stored upright: the run sits where layout put it, the composite turns it.
    let upright = frame_of("View Main() { Column { Text(\"turned\") #[size: 20] } }");
    let (a, b) = (&frame.texts()[0], &upright.texts()[0]);
    assert!(
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3,
        "{a:?} vs {b:?}"
    );
}

#[test]
fn a_rotated_box_turns_its_text_through_one_group() {
    let frame = frame_of(
        "View Main() { Column { Box #[rotate: 45deg, origin: center, width: 200, height: 60] {
            Text(\"inside\") #[size: 18]
        } } }",
    );
    assert_eq!(
        frame.groups().len(),
        1,
        "the box's group, and no second one"
    );
    assert!((frame.groups()[0].rotate - std::f32::consts::FRAC_PI_4).abs() < 1e-4);
}

#[test]
fn scale_and_translate_stay_on_the_direct_path() {
    let frame = frame_of(
        "View Main() { Column { Text(\"big\") #[scale: 2, translate: (x: 10, y: 5), size: 10] } }",
    );
    assert!(frame.groups().is_empty(), "no rotation, no group");
    assert!(!frame.has_rotated_group());
    // Baked in: shaped at the size it is drawn, so it stays sharp.
    assert!((frame.texts()[0].font_size - 20.0).abs() < 1e-4);
}

#[test]
fn a_still_upright_scene_is_not_a_full_redraw() {
    let frame = frame_of(
        "View Main() { Column { Text(\"a\") Box #[width: 10, height: 10, bg: 0xFF0000] {} } }",
    );
    assert!(frame.groups().is_empty());
    assert!(!frame.has_rotated_group());
}

/// A clip inside a rotated card is cut where its content is stored, upright,
/// and turned with it by the composite. Cut on screen instead, the outline is
/// rotated twice and keeps a sheared sliver of the content.
#[test]
fn a_clip_inside_a_rotated_card_is_cut_upright() {
    let src = |rotate: &str| {
        format!(
            "View Main() {{ Column {{ Box #[width: 300, height: 200, p: 20, {rotate} origin: center] {{
                Clip #[rrect: 12, width: 200, height: 100] {{ Box #[width: 400, height: 400, bg: 0xFF0000] {{}} }}
            }} }} }}"
        )
    };
    let turned = frame_of(&src("rotate: 30deg,"));
    let upright = frame_of(&src(""));
    assert_eq!(turned.groups().len(), 1);
    assert!(
        turned.clip_masks().is_empty(),
        "an upright clip is a rounded rect, not a path mask"
    );
    assert_eq!(turned.clips().len(), upright.clips().len());
    for (a, b) in turned.clips().iter().zip(upright.clips()) {
        assert!(
            (a.rect.x - b.rect.x).abs() < 1e-3
                && (a.rect.y - b.rect.y).abs() < 1e-3
                && (a.rect.width - b.rect.width).abs() < 1e-3
                && (a.rect.height - b.rect.height).abs() < 1e-3,
            "{:?} vs {:?}",
            a.rect,
            b.rect
        );
    }
}
