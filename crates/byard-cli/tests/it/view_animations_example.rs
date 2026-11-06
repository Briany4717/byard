//! Guards the committed view-animations example: one view with motion, called
//! six times on one screen, each call springing to its own radius. It must
//! `byard check` clean, and every call must be drawn with the radius it asked
//! for, before and after the target moves, driven headlessly through the real
//! interpreter.
//!
//! Every call of a view shares the source position of the `with` inside it, so
//! this was a picture that rendered, laid out and checked exactly as written
//! while all six boxes came out with the last call's radius.

use std::path::PathBuf;
use std::process::Command;

use byard_compiler::Symbol;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

const SRC: &str = include_str!("../../examples/view_animations/src/main.byd");

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/view_animations")
}

#[test]
fn view_animations_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard-cli"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "check failed:\n{stdout}");
    assert!(stdout.contains("0 errors"), "not clean:\n{stdout}");
}

fn load() -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let main = parsed.views.iter().find(|v| v.name.as_str() == "Main");
    let tree = interp.lower_view(main.expect("Main"), &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    (interp, tree)
}

/// The corner radius of every chip, top to bottom and left to right.
fn radii(interp: &mut Interpreter, tree: &[RenderNode], ms: u32) -> Vec<f32> {
    interp.tick();
    interp.set_now_ms(ms);
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 360.0, 220.0);
    let mut chips: Vec<_> = frame
        .instances()
        .iter()
        .filter(|b| (b.rect[2] - 80.0).abs() < 0.5 && (b.rect[3] - 50.0).abs() < 0.5)
        .map(|b| (b.rect[1], b.rect[0], b.radii[0]))
        .collect();
    chips.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    chips.into_iter().map(|c| c.2).collect()
}

fn close(got: &[f32], want: &[f32]) {
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 0.6, "wanted {want:?}, got {got:?}");
    }
}

#[test]
fn every_call_is_drawn_with_its_own_radius_and_moves_to_its_own() {
    let (mut interp, tree) = load();
    radii(&mut interp, &tree, 0);
    close(
        &radii(&mut interp, &tree, 1_500),
        &[28.0, 2.0, 28.0, 14.0, 28.0, 2.0],
    );

    let flipped = interp.var_signal(&Symbol::intern("flipped")).unwrap();
    interp.write_var(flipped, Value::Bool(true));
    radii(&mut interp, &tree, 1_516);
    close(
        &radii(&mut interp, &tree, 4_000),
        &[2.0, 28.0, 14.0, 28.0, 14.0, 28.0],
    );
}
