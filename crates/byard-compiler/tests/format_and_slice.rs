//! RFC-0027's second batch, the two an app showing live data needs first:
//! `{x:.N}`, a number with a fixed count of decimals, and `slice`, a part of
//! a list or of a string.
//!
//! Both are read back from the texts a real render draws, so they cover the
//! parser and the evaluator together.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// Renders `body` inside a `View Main()` and returns every text drawn, in
/// order. Fails on any parse or check error, so a test that expects a value
/// also proves the source is accepted.
fn texts(body: &str) -> Vec<String> {
    let src = format!("View Main() {{\n{body}\n}}");
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 400.0);
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

#[test]
fn decimals_round_a_float_to_what_was_asked() {
    let drawn = texts(
        "let t = 21.46\n\
         Column {\n\
             Text(\"{t:.0}\")\n\
             Text(\"{t:.1}\")\n\
             Text(\"{t:.3}\")\n\
             Text(\"{t}\")\n\
         }",
    );
    assert_eq!(drawn, ["21", "21.5", "21.460", "21.46"]);
}

#[test]
fn decimals_apply_to_an_int_and_to_any_expression() {
    let drawn = texts(
        "let n = 3\n\
         let xs = [1.25, 2.5]\n\
         Column {\n\
             Text(\"{n:.2}\")\n\
             Text(\"{xs[0] * 2:.1} and {(n + 1) / 3.0:.2}\")\n\
         }",
    );
    assert_eq!(drawn, ["3.00", "2.5 and 1.33"]);
}

#[test]
fn a_value_that_rounds_to_zero_has_no_sign() {
    let drawn = texts(
        "let t = -0.2\n\
         let cold = -3.6\n\
         Column { Text(\"{t:.0}°\") Text(\"{cold:.0}°\") }",
    );
    assert_eq!(drawn, ["0°", "-4°"], "a forecast never reads -0°");
}

#[test]
fn a_colon_that_is_not_a_decimals_spec_is_left_alone() {
    // A ternary's `:` and a `:.` inside a string are part of the expression.
    let drawn = texts(
        "let hot = true\n\
         Column {\n\
             Text(\"{hot ? 1 : 2}\")\n\
             Text(\"{\"a:.1\"}\")\n\
         }",
    );
    assert_eq!(drawn, ["1", "a:.1"]);
}

#[test]
fn slice_takes_part_of_a_list() {
    let drawn = texts(
        "let xs = [10, 20, 30, 40, 50]\n\
         Row { for x in xs.slice(1, 3) { Text(\"{x}\") } }",
    );
    assert_eq!(drawn, ["20", "30"]);
}

#[test]
fn slice_takes_part_of_a_string_by_character() {
    let drawn = texts(
        "let time = \"2026-09-26T14:00\"\n\
         let word = \"añejo\"\n\
         Column {\n\
             Text(time.slice(11, 16))\n\
             Text(word.slice(1, 3))\n\
             Text(time.slice(11))\n\
         }",
    );
    assert_eq!(drawn, ["14:00", "ñe", "14:00"]);
}

#[test]
fn slice_bounds_clamp_rather_than_fail() {
    let drawn = texts(
        "let xs = [1, 2, 3]\n\
         Column {\n\
             Text(\"{xs.slice(1, 99).len}\")\n\
             Text(\"{xs.slice(5).len}\")\n\
             Text(\"{xs.slice(2, 1).len}\")\n\
             Text(\"{xs.slice(-4, 2).len}\")\n\
         }",
    );
    assert_eq!(drawn, ["2", "0", "0", "2"]);
}
