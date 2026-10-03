//! A path drawn from data (RFC-0037): `for` and `when` inside a path body
//! write commands, so a chart's curve is a `for` over its samples.
//!
//! The body used to skip anything that was not a written command, in silence:
//! a chart over a list drew nothing and reported nothing. The claims here are
//! that the commands are written, in order, that the type checker and the
//! validator see inside the loop, and that the mesh cache still answers when
//! the data has not moved.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;

fn run(source: &str) -> (Interpreter, Vec<RenderNode>, RenderFrame) {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let checked = byard_compiler::infer::check_views(&parsed.views).errors;
    assert!(checked.is_empty(), "the type checker: {checked:?}");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 600.0);
    (interp, tree, frame)
}

/// The fill's bounds `[x, y, w, h]`, relative to its canvas.
fn bounds(frame: &RenderFrame) -> [f32; 4] {
    let fills = frame.fills();
    assert_eq!(fills.len(), 1, "one path, one fill");
    fills[0].mesh.bounds
}

/// An area under five samples, 50 apart, drawn down from a 100-high canvas.
const CHART: &str = "View Main() {
    var temps: List<Float> = [20.0, 60.0, 35.0, 80.0, 10.0]
    Canvas #[width: 200, height: 100] {
        path(fill: 0xFF5B8DEF) {
            move(0, 100)
            for i, t in temps { line(i * 50, 100 - t) }
            line(200, 100)
            close()
        }
    }
}";

#[test]
fn a_for_writes_one_command_per_item() {
    let (interp, _, frame) = run(CHART);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let b = bounds(&frame);
    // From the first sample to the last across, and from the tallest sample
    // (80, drawn at y = 20) to the baseline.
    assert!(
        (b[2] - 200.0).abs() < 0.5,
        "as wide as its samples span: {b:?}"
    );
    assert!(
        (b[3] - 80.0).abs() < 0.5,
        "as tall as its tallest sample: {b:?}"
    );
}

#[test]
fn a_when_writes_the_branch_it_takes() {
    let chart = |flat: bool| {
        format!(
            "View Main() {{
                var flat = {flat}
                Canvas #[width: 200, height: 100] {{
                    path(fill: 0xFF5B8DEF) {{
                        move(0, 100)
                        when flat {{ line(0, 90) line(200, 90) }} else {{ line(0, 20) line(200, 20) }}
                        line(200, 100)
                        close()
                    }}
                }}
            }}"
        )
    };
    let (_, _, flat) = run(&chart(true));
    let (_, _, tall) = run(&chart(false));
    assert!((bounds(&flat)[3] - 10.0).abs() < 0.5, "{:?}", bounds(&flat));
    assert!((bounds(&tall)[3] - 80.0).abs() < 0.5, "{:?}", bounds(&tall));
}

/// An empty list writes nothing, and what is left is still a path.
#[test]
fn an_empty_list_leaves_the_written_commands() {
    let (interp, _, frame) = run(&CHART.replace("[20.0, 60.0, 35.0, 80.0, 10.0]", "[]"));
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    // move, line, close: a degenerate sliver along the baseline, not a crash.
    assert!(frame.fills().len() <= 1);
}

#[test]
fn commands_inside_a_for_are_validated_like_written_ones() {
    let broken = |inside: &str| {
        let parsed = parse(&CHART.replace("line(i * 50, 100 - t)", inside));
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let _ = interp.lower_view(&parsed.views[0], &known);
        format!("{:?}", interp.errors())
    };
    assert!(broken("lime(i * 50, 100 - t)").contains("UnknownShapeCommand"));
    assert!(broken("line(i * 50)").contains("ArityMismatch"));
}

/// The start rule looks into a `for` that leads the body.
#[test]
fn a_path_that_starts_inside_a_for_still_starts_with_a_move() {
    let errors = |first: &str| {
        let parsed = parse(&format!(
            "View Main() {{
                var xs: List<Float> = [1.0, 2.0]
                Canvas #[width: 100, height: 100] {{
                    path(fill: 0xFF5B8DEF) {{ for x in xs {{ {first}(x, x) }} close() }}
                }}
            }}"
        ));
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let _ = interp.lower_view(&parsed.views[0], &known);
        format!("{:?}", interp.errors())
    };
    assert!(
        errors("line").contains("PathMustStartWithMove"),
        "{}",
        errors("line")
    );
    assert!(
        !errors("move").contains("PathMustStartWithMove"),
        "{}",
        errors("move")
    );
}

/// INV-23 still holds: a chart whose data did not move is not tessellated
/// again, and one whose data did is, once.
#[test]
fn a_chart_is_tessellated_again_only_when_its_data_changes() {
    let (mut interp, tree, _) = run(CHART);
    assert_eq!(interp.tessellations(), 1);
    for _ in 0..5 {
        interp.tick();
        let mut frame = RenderFrame::new();
        interp.render(&tree, &mut frame, 800.0, 600.0);
    }
    assert_eq!(
        interp.tessellations(),
        1,
        "five still frames tessellate nothing"
    );

    let temps = interp.var_signal(&Symbol::intern("temps")).unwrap();
    interp.write_var(
        temps,
        Value::List([20.0, 60.0, 35.0, 95.0, 10.0].map(Value::Float).to_vec()),
    );
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 600.0);
    assert_eq!(interp.tessellations(), 2, "new data, one new mesh");
    assert!(
        (bounds(&frame)[3] - 95.0).abs() < 0.5,
        "{:?}",
        bounds(&frame)
    );
}
