//! `scheme = "system"`: the theme follows the system's appearance.
//!
//! The host reports it as an event when the window opens and whenever the
//! user changes it; a theme that follows the system takes it, and one with a
//! fixed scheme does not.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::interp::theme::Theme;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent, InputPayload};

fn running(follows_system: bool) -> (Interpreter, Vec<byard_compiler::interp::eval::RenderNode>) {
    let parsed =
        parse("View Main() {\n    inject Theme as t\n    Text(t.dark ? \"dark\" : \"light\")\n}");
    let mut interp = Interpreter::new();
    let mut theme = Theme::default();
    theme.follows_system = follows_system;
    interp.set_theme(theme);
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    (interp, tree)
}

fn shown(interp: &mut Interpreter, tree: &[byard_compiler::interp::eval::RenderNode]) -> String {
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 200.0, 100.0);
    frame.texts()[0].text.clone()
}

fn system(dark: bool) -> InputEvent {
    InputEvent {
        kind: EventKind::SystemScheme,
        pos: (0.0, 0.0),
        delta: (0.0, 0.0),
        payload: Some(InputPayload::Bool(dark)),
        time_ms: 0,
        mods: byard_core::KeyModifiers::default(),
    }
}

#[test]
fn a_theme_that_follows_the_system_takes_its_appearance() {
    let (mut interp, tree) = running(true);
    assert_eq!(shown(&mut interp, &tree), "light");
    interp.dispatch_events(&[system(true)]);
    assert_eq!(shown(&mut interp, &tree), "dark");
    interp.dispatch_events(&[system(false)]);
    assert_eq!(shown(&mut interp, &tree), "light");
}

#[test]
fn a_fixed_scheme_ignores_the_system() {
    let (mut interp, tree) = running(false);
    interp.dispatch_events(&[system(true)]);
    assert_eq!(shown(&mut interp, &tree), "light");
}
