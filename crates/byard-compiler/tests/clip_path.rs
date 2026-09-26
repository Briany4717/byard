//! `clip(path)` in `byld` (RFC-0037): a `path` child of `Clip` is its mask.
//!
//! The pixels are proven in `byard-platform`; this is the surface. Which
//! element the mask belongs to, that `path` stays a hard error everywhere
//! else, and that the mask follows its data and is not re-tessellated when the
//! data does not move.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;

fn build(src: &str) -> (Interpreter, Vec<byard_compiler::interp::eval::RenderNode>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    (interp, tree)
}

fn render(
    interp: &mut Interpreter,
    tree: &[byard_compiler::interp::eval::RenderNode],
) -> RenderFrame {
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 300.0);
    frame
}

const TRIANGLE: &str = r"
View Main() {
    var tip = 120
    Column #[p: 20] {
        Clip #[width: 160, height: 120] {
            path {
                move(0, 0)
                line(160, 0)
                line(0, tip)
                close()
            }
            Box #[bg: 0x3366CC, width: 160, height: 120] {}
        }
    }
}
";

/// The mask reaches the frame, measured from the `Clip`'s own top-left.
///
/// Deterministic, and first: the mask's bounds are read off the frame, which
/// says *where* the outline landed, a thing a readback can only approximate.
#[test]
fn a_path_child_of_clip_becomes_the_clips_mask() {
    let (mut interp, tree) = build(TRIANGLE);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let frame = render(&mut interp, &tree);
    let masks = frame.clip_masks();
    assert_eq!(masks.len(), 1, "one mask for one Clip");
    let b = masks[0].bounds;
    assert_eq!(
        (b.x, b.y, b.width, b.height),
        (20.0, 20.0, 160.0, 120.0),
        "the mask's commands are measured from the Clip's own origin"
    );
    // The content is still content: the mask did not eat the child list.
    assert!(
        frame
            .instances()
            .iter()
            .any(|i| (i.rect[2] - 160.0).abs() < 0.5),
        "the Clip's box child must still be emitted"
    );
    // And the content is drawn under a clip that names the mask.
    assert!(
        frame.clips().iter().any(|c| c.mask == Some(0)),
        "some clip entry must carry the mask"
    );
}

/// `path` outside a `Canvas` and outside a `Clip` is exactly the error it
/// always was. The slot is claimed by one parent; the rule is not relaxed.
#[test]
fn a_path_anywhere_else_is_still_an_error() {
    let (interp, _) =
        build(r"View Main() { Column { path { move(0, 0) line(10, 0) line(0, 10) close() } } }");
    let errs = format!("{:?}", interp.errors());
    assert!(errs.contains("ShapeOutsideCanvas"), "{errs}");
}

/// A second `path` in the same `Clip` is not a second mask: it is content, and
/// content that is a shape outside a `Canvas` is the existing error.
#[test]
fn a_second_path_in_a_clip_is_an_error() {
    let (interp, _) = build(
        r"View Main() {
            Clip #[width: 50, height: 50] {
                path { move(0, 0) line(50, 0) line(0, 50) close() }
                path { move(0, 0) line(50, 0) line(0, 50) close() }
            }
        }",
    );
    let errs = format!("{:?}", interp.errors());
    assert!(errs.contains("ShapeOutsideCanvas"), "{errs}");
}

/// A misspelt command in a mask reads as it would in a canvas path.
#[test]
fn a_bad_command_in_a_mask_is_reported() {
    let (interp, _) = build(
        r"View Main() {
            Clip #[width: 50, height: 50] {
                path { move(0, 0) lien(50, 0) close() }
            }
        }",
    );
    assert!(
        !interp.errors().is_empty(),
        "a misspelt path command must be reported"
    );
}

/// The mask follows its data, and re-tessellates only when the data moved.
///
/// The second half is the incremental-path assertion: a mask that
/// re-tessellated every frame would render identically and cost a
/// tessellation per frame for ever, so the count is what is checked.
#[test]
fn the_mask_follows_its_var_and_is_cached_when_it_does_not_move() {
    let (mut interp, tree) = build(TRIANGLE);
    let _ = render(&mut interp, &tree);
    let before = interp.tessellations();
    let steady = render(&mut interp, &tree);
    assert_eq!(
        interp.tessellations(),
        before,
        "an unchanged mask must not be re-tessellated"
    );
    let h0 = steady.clip_masks()[0].bounds.height;

    let sig = interp
        .var_signal(&Symbol::intern("tip"))
        .expect("`tip` is declared");
    interp.write_var(sig, Value::Int(60));
    let moved = render(&mut interp, &tree);
    assert_eq!(
        interp.tessellations(),
        before + 1,
        "a mask whose outline moved must be tessellated once"
    );
    let h1 = moved.clip_masks()[0].bounds.height;
    assert!(
        (h0 - 120.0).abs() < 0.5 && (h1 - 60.0).abs() < 0.5,
        "the mask's extent must follow `tip`: {h0} → {h1}"
    );
}
