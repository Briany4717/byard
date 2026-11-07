//! RFC-0048: text read as a number (`toInt`, `toFloat`), the methods numbers
//! have (`round`, `floor`, `ceil`, `abs`, `min`, `max`, `toFloat`), and the
//! `{x:,.2}` grouping spec of an interpolation.

use byard_compiler::CompileError;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// What `Text("{expr}")` draws, with the checker's verdict ignored.
fn draws(expr: &str) -> String {
    let src = format!("View Main() {{\n    Text(\"{{{expr}}}\")\n}}");
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 600.0, 100.0);
    frame.texts()[0].text.clone()
}

/// What `Text("{expr}")` draws, after the checker has found nothing wrong.
fn shows(expr: &str) -> String {
    let src = format!("View Main() {{\n    Text(\"{{{expr}}}\")\n}}");
    let parsed = parse(&src);
    let checked = byard_compiler::infer::check_views(&parsed.views).errors;
    assert!(checked.is_empty(), "{expr}: {checked:?}");
    draws(expr)
}

/// The checker's errors for `let v = expr`.
fn errors(expr: &str) -> Vec<CompileError> {
    let src = format!("View Main() {{\n    let v = {expr}\n    Text(\"{{v}}\")\n}}");
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    byard_compiler::infer::check_views(&parsed.views).errors
}

/// `let v = expr` is accepted by the checker.
fn clean(expr: &str) {
    let found = format!("{:?}", errors(expr));
    assert_eq!(found, "[]", "{expr}");
}

fn int(text: &str) -> String {
    shows(&format!("\"{text}\".toInt()"))
}

fn float(text: &str) -> String {
    // Three decimals, so a `Float` and an `Int` cannot look alike by chance.
    shows(&format!("\"{text}\".toFloat():.3"))
}

#[test]
fn toint_reads_whole_numbers() {
    assert_eq!(int("12"), "12");
    assert_eq!(int(" -3 "), "-3");
    assert_eq!(int("+4"), "4");
    assert_eq!(int("007"), "7");
    assert_eq!(int("-0"), "0");
    assert_eq!(int("\\t 42\\n"), "42");
    assert_eq!(int("9223372036854775807"), "9223372036854775807");
    assert_eq!(int("-9223372036854775808"), "-9223372036854775808");
}

#[test]
fn toint_is_nothing_for_anything_else() {
    for bad in [
        "1_000", "0x10", "1.5", "1e3", "1,000", "", " ", "abc", "12a", "a12", "+", "-", "- 3",
        "--3", "+-3", "\u{663}",
    ] {
        assert_eq!(int(bad), "", "{bad:?} is not a whole number");
    }
}

#[test]
fn toint_out_of_range_is_nothing_and_never_wraps() {
    assert_eq!(int("9223372036854775808"), "");
    assert_eq!(int("-9223372036854775809"), "");
    assert_eq!(int("23456789012345678901234"), "");
}

#[test]
fn tofloat_reads_decimals() {
    assert_eq!(float("3"), "3.000");
    assert_eq!(float("1e3"), "1000.000");
    assert_eq!(float("1E3"), "1000.000");
    assert_eq!(float(".5"), "0.500");
    assert_eq!(float("5."), "5.000");
    assert_eq!(float(" -0.25 "), "-0.250");
    assert_eq!(float("+1.5"), "1.500");
    assert_eq!(float("2.5e-1"), "0.250");
    assert_eq!(float("1e+2"), "100.000");
    assert_eq!(float("-.5"), "-0.500");
}

#[test]
fn tofloat_is_nothing_for_anything_else() {
    for bad in [
        "inf", "-inf", "infinity", "nan", "NaN", "1,5", "1_0", "0x10", "", ".", "-", "e3", "1e",
        "1e+", "1.2.3", "1 2", "abc", "1e999",
    ] {
        assert_eq!(float(bad), "", "{bad:?} is not a decimal number");
    }
}

#[test]
fn a_whole_number_text_is_a_float_with_tofloat_and_an_int_with_toint() {
    // Int division truncates, Float division does not.
    assert_eq!(shows("\"3\".toFloat() / 2:.1"), "1.5");
    assert_eq!(shows("\"3\".toInt() / 2"), "1");
}

#[test]
fn a_default_replaces_nothing() {
    assert_eq!(shows("\"12\".toInt() ?? 0"), "12");
    assert_eq!(shows("\"abc\".toInt() ?? 0"), "0");
    assert_eq!(shows("\"\".toInt() ?? -1"), "-1");
    assert_eq!(shows("\"x\".toFloat() ?? 1.5:.1"), "1.5");
    assert_eq!(shows("\"2\".toFloat() ?? 1.5:.1"), "2.0");
    assert_eq!(shows("\" 3 \".trim().toInt() ?? 0"), "3");
}

#[test]
fn nothing_carries_through_arithmetic() {
    assert_eq!(shows("\"3\".toInt() + 1"), "4");
    assert_eq!(shows("\"x\".toInt() + 1"), "");
    assert_eq!(shows("(\"x\".toInt() ?? 0) + 1"), "1");
}

#[test]
fn round_goes_away_from_zero_on_a_half() {
    assert_eq!(shows("(2.5).round()"), "3");
    assert_eq!(shows("(-2.5).round()"), "-3");
    assert_eq!(shows("(2.4).round()"), "2");
    assert_eq!(shows("(2.6).round()"), "3");
    assert_eq!(shows("(-2.4).round()"), "-2");
    assert_eq!(shows("(-2.6).round()"), "-3");
    assert_eq!(shows("(0.5).round()"), "1");
    assert_eq!(shows("(-0.5).round()"), "-1");
    assert_eq!(shows("(-0.4).round()"), "0");
    assert_eq!(shows("(3.0).round()"), "3");
}

#[test]
fn floor_and_ceil_go_toward_the_infinities() {
    assert_eq!(shows("(2.5).floor()"), "2");
    assert_eq!(shows("(-2.5).floor()"), "-3");
    assert_eq!(shows("(2.5).ceil()"), "3");
    assert_eq!(shows("(-2.5).ceil()"), "-2");
    assert_eq!(shows("(4.0).floor()"), "4");
    assert_eq!(shows("(4.0).ceil()"), "4");
}

#[test]
fn rounding_what_does_not_fit_an_int_is_nothing() {
    assert_eq!(shows("(1000000000000000000000000000000.0).round()"), "");
    assert_eq!(shows("(-1000000000000000000000000000000.0).floor()"), "");
    assert_eq!(shows("(10000000000000000000.0).ceil()"), "");
    assert_eq!(shows("(9007199254740992.0).round()"), "9007199254740992");
    // 2^63 is the first value past the largest Int; -2^63 is the smallest Int.
    assert_eq!(shows("(9223372036854775807.0).round()"), "");
    assert_eq!(
        shows("(-9223372036854775808.0).round()"),
        "-9223372036854775808"
    );
    // `0.0 / 0.0` answers 0 in byld, so a NaN has to come from outside; the
    // out-of-range cases above are the same code path.
}

#[test]
fn int_tofloat_abs_min_and_max() {
    assert_eq!(shows("(3).toFloat() / 2:.1"), "1.5");
    assert_eq!(shows("(-5).abs()"), "5");
    assert_eq!(shows("(5).abs()"), "5");
    assert_eq!(shows("(-2.5).abs():.1"), "2.5");
    assert_eq!(shows("(7).max(10)"), "10");
    assert_eq!(shows("(7).min(10)"), "7");
    assert_eq!(shows("(-3).max(0)"), "0");
    assert_eq!(shows("(2.5).max(1):.1"), "2.5");
    assert_eq!(shows("(2).max(2.5):.1"), "2.5");
    assert_eq!(shows("(2).min(2.5):.1"), "2.0");
    assert_eq!(shows("(2.5).min(1.5):.1"), "1.5");
}

#[test]
fn clamping_a_typed_quantity_needs_no_fn() {
    assert_eq!(shows("\"-4\".toInt().max(0)"), "0");
    assert_eq!(shows("(\"x\".toInt() ?? 0).max(0)"), "0");
    assert_eq!(shows("\"12\".toInt().max(0).min(10)"), "10");
}

#[test]
fn list_min_and_max_are_unchanged() {
    assert_eq!(shows("[3, 1, 2].min()"), "1");
    assert_eq!(shows("[3, 1, 2].max()"), "3");
}

#[test]
fn grouping_inserts_a_comma_every_three_digits() {
    for (n, shown) in [
        (0, "0"),
        (5, "5"),
        (12, "12"),
        (123, "123"),
        (1234, "1,234"),
        (12345, "12,345"),
        (123_456, "123,456"),
        (1_234_567, "1,234,567"),
    ] {
        assert_eq!(shows(&format!("{n}:,")), shown);
        let negative = if n == 0 {
            "0".to_string()
        } else {
            format!("-{shown}")
        };
        assert_eq!(shows(&format!("-{n}:,")), negative);
    }
}

#[test]
fn grouping_combines_with_decimals() {
    assert_eq!(shows("193440.7:,.2"), "193,440.70");
    assert_eq!(shows("-1234:,"), "-1,234");
    assert_eq!(shows("1234:,.2"), "1,234.00");
    assert_eq!(shows("999.999:,.2"), "1,000.00");
    assert_eq!(shows("-1234567.891:,.1"), "-1,234,567.9");
    assert_eq!(shows("0.5:,.2"), "0.50");
    assert_eq!(shows("-0.2:,.0"), "0");
    assert_eq!(shows("1234567.891:,"), "1,234,567.891");
    assert_eq!(shows("1234.5:,"), "1,234.5");
    assert_eq!(shows("12.5:,"), "12.5");
    assert_eq!(shows("(1.5).round() * 1000:,"), "2,000");
}

#[test]
fn grouping_leaves_what_is_not_a_number_alone() {
    assert_eq!(shows("\"abc\":,"), "abc");
    assert_eq!(shows("true:,"), "true");
    assert_eq!(shows("\"x\".toInt():,"), "");
}

#[test]
fn the_older_specs_and_the_ternary_still_parse() {
    assert_eq!(shows("21.46:.1"), "21.5");
    assert_eq!(shows("3 > 2 ? 1 : 0"), "1");
    assert_eq!(shows("true ? \"a\" : \"b\""), "a");
}

#[test]
fn numbers_have_a_closed_method_set_too() {
    let named = |expr: &str| -> Vec<(String, Option<String>)> {
        errors(expr)
            .iter()
            .filter_map(|e| match e {
                CompileError::UnknownMethod { name, hint, .. } => {
                    Some((name.clone(), hint.clone()))
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        named("(2.5).roud()"),
        [("roud".to_string(), Some("round".to_string()))]
    );
    assert_eq!(
        named("(2).tofloat()"),
        [("tofloat".to_string(), Some("toFloat".to_string()))]
    );
    // An `Int` is whole already.
    assert_eq!(named("(2).round()").len(), 1);
    assert_eq!(
        named("\"3\".toIn()"),
        [("toIn".to_string(), Some("toInt".to_string()))]
    );
    assert_eq!(named("\"3\".toInt()"), []);
    assert_eq!(named("(2.5).round()"), []);
    // A colour is an `Int`, and keeps its method.
    assert_eq!(named("0xFF0000.alpha(0.5)"), []);
}

#[test]
fn the_checker_types_the_results() {
    // Int arithmetic with a maybe-nothing.
    clean("\"3\".toInt() + 1");
    clean("\"3\".toInt() * 2.5");
    clean("\"3\".toFloat() + 1");
    clean("(2.5).round() + 1");
    clean("(2).toFloat() + 0.5");
    clean("(2).max(3) + 1");
    clean("\"3\".toInt() ?? 0");
    clean("\"3\".toFloat() ?? 0.0");
    // A number is not text.
    let mismatch = |expr: &str| {
        errors(expr)
            .iter()
            .any(|e| matches!(e, CompileError::TypeMismatch { .. }))
    };
    assert!(mismatch("\"3\".toInt() - \"a\""));
    assert!(mismatch("\"3\".toInt() ?? \"none\""));
    assert!(mismatch("(2.5).round() - \"a\""));
    assert!(mismatch("(2.5).abs() - true"));
}
