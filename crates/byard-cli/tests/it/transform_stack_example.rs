//! Guards the committed RFC-0011 example: it checks clean, and at a tilt its
//! button is pressed where it is drawn.
//!
//! The press is aimed from the frame rather than from a hand-computed
//! coordinate: the button's drawn centre is its laid-out centre mapped through
//! the transform the frame carries for it, so the test cannot pass by aiming
//! at a point that is correct for some other angle.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};
use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/transform_stack")
}

#[test]
fn transform_stack_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "{stdout}");
}

#[test]
fn at_a_tilt_the_button_is_pressed_where_it_is_drawn() {
    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let angle = interp.var_signal(&Symbol::intern("angle")).unwrap();
    interp.write_var(angle, Value::Int(60));
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 520.0, 560.0);

    // The button is the 90×44 box; aim at its *drawn* centre. A plain rounded
    // fill lands in the solid pool and a decorated one in the other, so both
    // are searched rather than betting on which the engine chose.
    let is_button = |r: [f32; 4]| (r[2] - 90.0).abs() < 0.5 && (r[3] - 44.0).abs() < 0.5;
    let (rect, transform) = frame
        .instances()
        .iter()
        .map(|b| (b.rect, b.transform))
        .chain(
            frame
                .decorated()
                .iter()
                .map(|d| (d.base.rect, d.base.transform)),
        )
        .find(|(r, _)| is_button(*r))
        .expect("the +1 button is emitted");
    let [x, y, w, h] = rect;
    let laid_out = (x + w / 2.0, y + h / 2.0);
    let drawn = transform.apply_point([laid_out.0, laid_out.1]);
    assert!(
        (drawn[0] - laid_out.0).abs() + (drawn[1] - laid_out.1).abs() > 40.0,
        "at 60° the drawn and laid-out centres must be far apart, or this test \
         cannot tell the two behaviours apart: {laid_out:?} vs {drawn:?}"
    );

    let press = |interp: &mut Interpreter, pos: (f32, f32)| {
        let ev = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
        };
        interp.dispatch_events(&[ev(EventKind::PointerDown, 0), ev(EventKind::PointerUp, 8)]);
        interp.tick();
        let mut next = RenderFrame::new();
        interp.render(&tree, &mut next, 520.0, 560.0);
    };
    let presses = interp.var_signal(&Symbol::intern("presses")).unwrap();

    press(&mut interp, laid_out);
    assert_eq!(
        interp.peek(presses).as_int(),
        Some(0),
        "where the button would be unrotated is not where it is"
    );
    press(&mut interp, (drawn[0], drawn[1]));
    assert_eq!(
        interp.peek(presses).as_int(),
        Some(1),
        "a press on the button where it is drawn must reach it"
    );
}
