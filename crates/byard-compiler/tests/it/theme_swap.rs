//! Replacing the theme of a running interpreter (a dev reload of `[theme]`,
//! RFC-0022) repaints with the new tokens.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

const SRC: &str = "View Main() {
    inject Theme as t
    Column #[bg: t.surface, width: 200, height: 100] {
        Box #[bg: t.primary, width: 50, height: 50] {}
    }
}";

fn colours(
    interp: &mut Interpreter,
    tree: &[byard_compiler::interp::eval::RenderNode],
    ms: u32,
) -> Vec<[f32; 4]> {
    interp.set_now_ms(ms);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 400.0, 300.0);
    frame
        .instances()
        .iter()
        .map(|b| b.color)
        .chain(frame.decorated().iter().map(|d| d.base.color))
        .collect()
}

#[test]
fn a_new_theme_on_a_running_app_repaints_with_its_tokens() {
    let parsed = parse(SRC);
    let mut interp = Interpreter::new();
    let mut red = Theme::byard_base();
    red.transition_ms = 300;
    red.apply_seed(0xE8_543F);
    interp.set_theme(red);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    let before = colours(&mut interp, &tree, 0);

    let mut blue = Theme::byard_base();
    blue.transition_ms = 300;
    blue.apply_seed(0x28_6EDC);
    interp.set_theme(blue);
    let after = colours(&mut interp, &tree, 5_000);
    assert_eq!(before.len(), after.len());
    assert!(
        after.iter().all(|c| c[3] > 0.99),
        "every fill still opaque: {after:?}"
    );
    assert_ne!(before, after, "the palette moved");
}

/// The dev loop's shape: one frame recycled across ticks, cleared between
/// them, the swap landing between two ticks.
#[test]
fn a_new_theme_repaints_a_recycled_frame() {
    let parsed = parse(SRC);
    let mut interp = Interpreter::new();
    let mut red = Theme::byard_base();
    red.transition_ms = 300;
    red.apply_seed(0xE8_543F);
    interp.set_theme(red);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    let mut frame = RenderFrame::new();
    let paint = |interp: &mut Interpreter, frame: &mut RenderFrame, ms: u32| {
        interp.set_now_ms(ms);
        interp.tick();
        frame.clear();
        interp.render(&tree, frame, 400.0, 300.0);
        frame
            .instances()
            .iter()
            .map(|b| b.color)
            .chain(frame.decorated().iter().map(|d| d.base.color))
            .collect::<Vec<_>>()
    };
    for ms in (0..200).step_by(16) {
        paint(&mut interp, &mut frame, ms);
    }
    let before = paint(&mut interp, &mut frame, 200);
    let mut blue = Theme::byard_base();
    blue.transition_ms = 300;
    blue.apply_seed(0x28_6EDC);
    interp.set_theme(blue);
    let first = paint(&mut interp, &mut frame, 216);
    assert_eq!(
        first.len(),
        before.len(),
        "the frame after the swap is not empty"
    );
    let mut after = Vec::new();
    for ms in (216..1200).step_by(16) {
        after = paint(&mut interp, &mut frame, ms);
    }
    assert_eq!(before.len(), after.len(), "{after:?}");
    assert!(after.iter().all(|c| c[3] > 0.99), "{after:?}");
    assert_ne!(before, after);
}

/// A hot reload of a view that injects the theme still finds it: `reload`
/// starts the environment afresh, and the theme is one of the ambient values
/// it has to put back, like the controllers.
#[test]
fn a_hot_reload_keeps_the_theme_provided() {
    let parsed = parse(SRC);
    let mut interp = Interpreter::new();
    interp.set_theme(Theme::byard_base());
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    let before = colours(&mut interp, &tree, 0);

    let edited = parse(&SRC.replace("width: 50", "width: 60"));
    let kind = byard_compiler::interp::reload::diff_view(&parsed.views[0], &edited.views[0]);
    interp.reload(&edited.views[0], kind);
    interp.load_views(&edited.views);
    let tree = interp.lower_view(&edited.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let after = colours(&mut interp, &tree, 16);
    assert_eq!(after, before, "same colours, one box wider");
}
