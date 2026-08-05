//! RFC-0037 Tier-2: filled paths, their gradients, and the cache that makes a
//! live chart affordable.
//!
//! The interesting claims here are not "a triangle appeared". They are that an
//! unchanged path does not tessellate twice, that the flattening tolerance
//! follows the shape's on-screen size, and that a path gradient is the box
//! gradient, not a second one that agrees today.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// Lowers and renders one view, returning the interpreter and its frame.
fn run(source: &str) -> (Interpreter, Vec<RenderNode>, RenderFrame) {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 600.0);
    (interp, tree, frame)
}

/// A canvas holding one filled path, with whatever paint arguments are given.
fn area(paint: &str) -> String {
    format!(
        "View Main() {{ Canvas #[width: 200, height: 100] {{ \
            path({paint}) {{ \
                move(0, 100) \
                line(0, 40) \
                cubic(60, 0, 140, 80, 200, 20) \
                line(200, 100) \
                close() \
            }} \
        }} }}"
    )
}

#[test]
fn a_closed_path_fills_its_interior() {
    let (interp, _, frame) = run(&area("fill: 0xFF5B8DEF"));
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let fills = frame.fills();
    assert_eq!(fills.len(), 1, "one path, one fill");
    let mesh = &fills[0].mesh;
    assert!(
        mesh.indices.len() >= 3 && mesh.indices.len() % 3 == 0,
        "a filled area is whole triangles: {} indices",
        mesh.indices.len()
    );
    assert!(
        mesh.vertices.len() >= 4,
        "a curve flattens to more than its control points"
    );
    // The mesh lives in the canvas' own space, and its bounds are what a
    // gradient is measured against.
    assert!(
        mesh.bounds[2] > 100.0 && mesh.bounds[3] > 50.0,
        "{:?}",
        mesh.bounds
    );
    assert_eq!(interp.tessellations(), 1);
}

#[test]
fn an_unchanged_path_reuses_its_mesh_instead_of_tessellating_again() {
    // The claim the whole cache exists for, and the one that keeps a chart in
    // budget (INV-18: an incremental path needs an assertion that fails when
    // production stops taking it).
    let (mut interp, tree, _) = run(&area("fill: 0xFF5B8DEF"));
    assert_eq!(interp.tessellations(), 1);

    for _ in 0..10 {
        let mut frame = RenderFrame::new();
        interp.tick();
        interp.render(&tree, &mut frame, 800.0, 600.0);
        assert_eq!(frame.fills().len(), 1, "it still draws every frame");
    }
    assert_eq!(
        interp.tessellations(),
        1,
        "ten frames of an unchanged chart must tessellate nothing"
    );
}

#[test]
fn a_path_whose_numbers_changed_is_tessellated_again() {
    // The other half of the same claim: a cache that never misses is a cache
    // that is showing stale geometry.
    let source = "View Main() { var top = 40.0 \
        Canvas #[width: 200, height: 100] { \
            path(fill: 0xFF5B8DEF) { move(0, 100) line(0, top) line(200, 100) close() } \
        } \
        Button(\"raise\") => top = 10.0 }";
    let (mut interp, tree, _) = run(source);
    assert_eq!(interp.tessellations(), 1);

    // Re-render unchanged: still one.
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 800.0, 600.0);
    assert_eq!(interp.tessellations(), 1);

    // Now move a coordinate, and the mesh is rebuilt.
    interp.dispatch_events(&[byard_core::InputEvent {
        kind: byard_core::platform::EventKind::Tap,
        pos: (30.0, 130.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 1,
    }]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 600.0);
    assert_eq!(
        interp.tessellations(),
        2,
        "a path whose data moved has to be re-tessellated"
    );
}

#[test]
fn flattening_follows_the_shape_on_screen_size() {
    // A sparkline in a small card and the same curve across a large one are
    // the same commands and want very different triangle counts (RFC-0037).
    let curve = |w: u32, h: u32| {
        format!(
            "View Main() {{ Canvas #[width: {w}, height: {h}] {{ \
                path(fill: 0xFF5B8DEF) {{ \
                    move(0, {h}) \
                    cubic({}, 0, {}, {h}, {w}, 0) \
                    close() \
                }} \
            }} }}",
            w / 3,
            2 * w / 3
        )
    };
    let (_, _, small) = run(&curve(40, 24));
    let (_, _, large) = run(&curve(1200, 700));
    let small_tris = small.fills()[0].mesh.indices.len();
    let large_tris = large.fills()[0].mesh.indices.len();
    assert!(
        large_tris > small_tris,
        "a big curve flattens finer than a small one: {small_tris} vs {large_tris}"
    );
}

#[test]
fn an_unclosed_fill_closes_itself_rather_than_drawing_nothing() {
    // The RFC's resolved question: an open filled path is almost always an
    // oversight, and the obviously-intended shape is the one that closes.
    let (interp, _, frame) = run("View Main() { Canvas #[width: 100, height: 100] { \
            path(fill: 0xFF5B8DEF) { move(10, 90) line(50, 10) line(90, 90) } \
        } }");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!(
        !frame.fills()[0].mesh.indices.is_empty(),
        "the triangle closed itself and filled"
    );
}

#[test]
fn winding_decides_what_a_self_intersecting_path_encloses() {
    // A square with a square hole traced the same direction: `nonzero` fills
    // the lot, `even_odd` leaves the hole.
    let shape = |winding: &str| {
        format!(
            "View Main() {{ Canvas #[width: 100, height: 100] {{ \
                path(fill: 0xFF5B8DEF, winding: {winding}) {{ \
                    move(0, 0) line(100, 0) line(100, 100) line(0, 100) close() \
                    move(25, 25) line(75, 25) line(75, 75) line(25, 75) close() \
                }} \
            }} }}"
        )
    };
    let (_, _, nonzero) = run(&shape("nonzero"));
    let (_, _, even_odd) = run(&shape("even_odd"));
    let area_of = |frame: &RenderFrame| {
        let mesh = &frame.fills()[0].mesh;
        mesh.indices
            .chunks_exact(3)
            .map(|t| {
                let p = |i: u32| mesh.vertices[i as usize].pos;
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() * 0.5
            })
            .sum::<f32>()
    };
    let solid = area_of(&nonzero);
    let holed = area_of(&even_odd);
    assert!(
        solid > holed + 1000.0,
        "even_odd must leave the inner square out: {solid} vs {holed}"
    );
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "the point is that the two are bit-identical: an approximate match would pass while they drifted"
)]
fn a_path_gradient_is_the_box_gradient() {
    // RFC-0037 shares RFC-0035's descriptor rather than forking it, so the
    // *same* words produce the same value on a path as on a box. Read both
    // out of one frame and compare them field by field.
    let (interp, _, frame) = run("View Main() { \
            Box #[width: 200, height: 100, bg: 0xFF101010, \
                  gradient: (kind: linear, angle: 90deg, from: 0xFFF9A8A8, to: 0x00000000)] {} \
            Canvas #[width: 200, height: 100] { \
                path(fill: 0xFF000000, \
                     gradient: (kind: linear, angle: 90deg, from: 0xFFF9A8A8, to: 0x00000000)) { \
                    move(0, 100) line(0, 0) line(200, 0) line(200, 100) close() \
                } \
            } \
        }");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let box_gradient = frame.decorated()[0].gradient.expect("the box has one");
    let path_gradient = frame.fills()[0].gradient.expect("the path has one");
    assert_eq!(
        box_gradient, path_gradient,
        "one descriptor, one parser: a path gradient cannot mean something else"
    );
    assert_eq!(box_gradient.axis(), path_gradient.axis());
}

#[test]
fn a_path_body_starts_with_a_move() {
    let (interp, _, _) = run("View Main() { Canvas #[width: 100, height: 100] { \
            path(fill: 0xFF5B8DEF) { line(50, 50) close() } \
        } }");
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| e.kind() == "PathMustStartWithMove"),
        "a path with no start point must be refused: {:?}",
        interp.errors()
    );
}

#[test]
fn an_unknown_path_command_is_named_with_a_hint() {
    let (interp, _, _) = run("View Main() { Canvas #[width: 100, height: 100] { \
            path(fill: 0xFF5B8DEF) { move(0, 0) lien(50, 50) close() } \
        } }");
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| e.kind() == "UnknownShapeCommand"),
        "{:?}",
        interp.errors()
    );
}

#[test]
fn a_command_missing_a_coordinate_is_a_compile_error() {
    let (interp, _, _) = run("View Main() { Canvas #[width: 100, height: 100] { \
            path(fill: 0xFF5B8DEF) { move(0) line(50, 50) close() } \
        } }");
    assert!(
        !interp.errors().is_empty(),
        "a `move` with one number is not a point"
    );
}

#[test]
fn a_static_path_still_takes_the_atlas_path() {
    // The dividing line the RFC documents: `d:` art is baked and reused,
    // a command body is tessellated. Both remain `path`, and the one with no
    // body must not have started producing meshes.
    let (interp, _, frame) = run("View Main() { Canvas #[width: 100, height: 100] { \
            path(d: \"M10 10 L90 10 L50 90 Z\", fill: 0xFF5B8DEF) \
        } }");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert!(
        frame.fills().is_empty(),
        "a `d:` path is Tier-1 and does not tessellate"
    );
}
