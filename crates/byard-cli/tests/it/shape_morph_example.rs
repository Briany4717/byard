//! Guards the committed RFC-0031 §S9–§S10 example through the real `byard`
//! binary: the `shape_morph` project, the `ngon` shape kind across its whole
//! parameter set, the Material 3 Expressive loading indicator as seven members
//! and one `anim.linear(…, repeat: infinite)` scalar, a morph between two
//! *different* shape kinds, and an interaction-state spring, must check clean
//! (parse, type-check, lower, validate with no diagnostics).

use std::path::PathBuf;
use std::process::Command;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/shape_morph")
}

/// `byard check <project-dir>` on the shape-morph example reports no errors.
#[test]
fn shape_morph_example_checks_clean() {
    let out = Command::new(env!("CARGO_BIN_EXE_byard"))
        .arg("check")
        .arg(example_dir())
        .output()
        .expect("run `byard check`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "check failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("0 errors"),
        "expected a clean check, got:\n{stdout}"
    );
}

/// The play/pause canvas (RFC-0031 §S11) draws one blended path, and tapping
/// it carries the outline from the triangle to the two bars.
///
/// Read off the frame's fills: the example's only body paths are these two,
/// so the one fill on the frame is the morph. The triangle's tip reaches
/// x = 92 on the canvas and the bars stop at 90, which is how the two ends
/// are told apart.
#[test]
fn the_play_pause_paths_morph_when_tapped() {
    use byard_compiler::interp::env::Value;
    use byard_compiler::interp::eval::Interpreter;
    use byard_compiler::parser::parse;
    use byard_compiler::symbol::Symbol;
    use byard_core::frame::RenderFrame;

    let src = std::fs::read_to_string(example_dir().join("src/main.byd")).unwrap();
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let right_edge = |interp: &mut Interpreter, ms: u32| {
        interp.set_now_ms(ms);
        interp.tick();
        let mut frame = RenderFrame::new();
        interp.render(&tree, &mut frame, 720.0, 900.0);
        let fills = frame.fills();
        assert_eq!(fills.len(), 1, "the morph draws one path, not both");
        let b = fills[0].mesh.bounds;
        (b[0], b[0] + b[2])
    };
    let (left, play) = right_edge(&mut interp, 1000);
    let sig = interp
        .var_signal(&Symbol::intern("playing"))
        .expect("`playing` is declared");
    interp.write_var(sig, Value::Bool(true));
    let _ = right_edge(&mut interp, 1016);
    let (_, pause) = right_edge(&mut interp, 5000);
    assert!(
        ((play - left) - 62.0).abs() < 0.5 && ((pause - left) - 60.0).abs() < 0.5,
        "the outline goes from the triangle (62 wide) to the bars (60 wide): {play} -> {pause}"
    );
}
