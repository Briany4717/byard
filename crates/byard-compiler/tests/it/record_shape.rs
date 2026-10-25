//! A value written into a `var` of a record type is compared with the type.
//!
//! A record type checks the code that reads a value; a JSON reply is only a
//! record of whatever the server sent. The interpreter compares what arrives
//! with the declared type when it is written, and reports the first
//! difference once, where the assignment is written.

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::{EventKind, InputEvent};

/// Runs a view whose button writes `value` into `found: List<Place>`, taps
/// it `taps` times, and returns the shape mismatches reported.
fn written(place: &str, value: &str, taps: usize) -> Vec<String> {
    let src = format!(
        "{place}\nView Main() {{
    var found: List<Place> = []
    Button(\"go\") #[width: 100, height: 40] => found = {value}
    Text(\"{{found.len}}\")
}}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 200.0);
    for t in 0..taps {
        let at = |kind, time| InputEvent {
            kind,
            pos: (20.0, 20.0),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: time,
            mods: byard_core::KeyModifiers::default(),
        };
        let time = 100 + t as u64 * 100;
        interp.dispatch_events(&[
            at(EventKind::PointerDown, time),
            at(EventKind::PointerUp, time + 8),
        ]);
        interp.tick();
        frame.clear();
        interp.render(&tree, &mut frame, 400.0, 200.0);
    }
    interp
        .errors()
        .iter()
        .filter_map(|e| match e {
            CompileError::ShapeMismatch { .. } => Some(e.headline()),
            _ => None,
        })
        .collect()
}

const PLACE: &str = "type Place = { name: Str, country: Str, latitude: Float }";

#[test]
fn a_value_with_the_shape_is_fine() {
    let told = written(
        PLACE,
        "[{ name: \"Lisbon\", country: \"Portugal\", latitude: 38.7 }]",
        1,
    );
    assert!(told.is_empty(), "{told:?}");
}

#[test]
fn a_missing_field_is_told_with_where_it_is() {
    let told = written(
        PLACE,
        "[{ name: \"Lisbon\", country: \"Portugal\", latitude: 38.7 }, { name: \"Lis\", latitude: 41.6 }]",
        1,
    );
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("found") && told[0].contains("List<Place>"),
        "{}",
        told[0]
    );
    assert!(told[0].contains("item 1 has no `country`"), "{}", told[0]);
}

#[test]
fn a_field_of_the_wrong_kind_is_told() {
    let told = written(
        PLACE,
        "[{ name: \"Lisbon\", country: \"Portugal\", latitude: \"north\" }]",
        1,
    );
    assert!(
        told[0].contains("has a `latitude` that is text, not a number"),
        "{told:?}"
    );
}

/// A field the type marks optional (`?`) may be left out.
#[test]
fn an_optional_field_may_be_left_out() {
    let told = written(
        "type Place = { name: Str, country: Str?, latitude: Float }",
        "[{ name: \"Lis\", latitude: 41.6 }]",
        1,
    );
    assert!(told.is_empty(), "{told:?}");
}

/// Told once for an assignment, however often it runs.
#[test]
fn it_is_told_once() {
    let told = written(PLACE, "[{ name: \"Lis\" }]", 3);
    assert_eq!(told.len(), 1, "{told:?}");
}

/// A `var` whose type names no record is the checker's business only.
#[test]
fn a_var_of_no_record_type_is_not_compared() {
    let src = "View Main() {
    var xs: List<Int> = []
    Button(\"go\") #[width: 100, height: 40] => xs = [\"not\", \"numbers\"]
    Text(\"{xs.len}\")
}";
    let parsed = parse(src);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    let mut frame = RenderFrame::new();
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 200.0);
    let at = |kind, time| InputEvent {
        kind,
        pos: (20.0, 20.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: time,
        mods: byard_core::KeyModifiers::default(),
    };
    interp.dispatch_events(&[
        at(EventKind::PointerDown, 100),
        at(EventKind::PointerUp, 108),
    ]);
    interp.tick();
    assert!(
        !interp
            .errors()
            .iter()
            .any(|e| matches!(e, CompileError::ShapeMismatch { .. })),
        "{:?}",
        interp.errors()
    );
}
