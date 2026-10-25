//! Tests for the formatter (RFC-0044 §"Acceptance").

use std::path::{Path, PathBuf};

use super::{FmtError, format, format_using, keeps_meaning};
use crate::lexer::{Piece, Trivia, lex_with_trivia};
use crate::parser::parse;
use crate::parser::span_eq::parsed_files_span_eq;

fn fmt(source: &str) -> String {
    match format(source) {
        Ok(out) => out,
        Err(e) => panic!("{e}\n--- source ---\n{source}"),
    }
}

/// Formats `source`, and checks the result is a fixed point.
fn fmt_stable(source: &str) -> String {
    let once = fmt(source);
    let twice = fmt(&once);
    assert_eq!(once, twice, "formatting twice must change nothing");
    once
}

// ---- spacing ------------------------------------------------------------

#[test]
fn spacing_follows_one_rule_per_token() {
    let out = fmt_stable(
        "View Main( ){\nvar n:Int=0\nlet m=n*2+1-n/3\nText( \"x\" )#[a:1,b:(1,2)]=>n++\n}",
    );
    assert_eq!(
        out,
        "View Main() {\n    var n: Int = 0\n    let m = n * 2 + 1 - n / 3\n    Text(\"x\") #[a: 1, b: (1, 2)] => n++\n}\n"
    );
}

#[test]
fn unary_operators_hug_their_operand_and_binary_ones_do_not() {
    let out = fmt_stable(
        "View M() {\nlet a = -x\nlet b = a-b\nlet c = a - -b\nlet d = !x && !(y)\nlet e = - -x\n}\n",
    );
    assert!(out.contains("let a = -x\n"), "{out}");
    assert!(out.contains("let b = a - b\n"), "{out}");
    assert!(out.contains("let c = a - -b\n"), "{out}");
    assert!(out.contains("let d = !x && !(y)\n"), "{out}");
    assert!(out.contains("let e = - -x\n"), "{out}");
}

#[test]
fn types_ternaries_lambdas_and_selectors() {
    let out = fmt_stable(
        "type P = {a:List<Str>?,b:Str?}\nView M(f:Fn(Int)->Str){\nlet t = ok?1:2\nlet g = |x,y|x+y\nlet h = items.map(|i| i)\nstyle{.a #[x:1]}\nlet s = style{\non hover+focused{bg:1}\non width>=md{bg:2}\n}\n}\n",
    );
    assert!(out.contains("type P = { a: List<Str>?, b: Str? }"), "{out}");
    assert!(out.contains("f: Fn(Int) -> Str"), "{out}");
    assert!(out.contains("let t = ok ? 1 : 2"), "{out}");
    assert!(out.contains("let g = |x, y| x + y"), "{out}");
    assert!(out.contains("items.map(|i| i)"), "{out}");
    assert!(out.contains("on hover+focused { bg: 1 }"), "{out}");
    assert!(out.contains("on width >= md { bg: 2 }"), "{out}");
}

#[test]
fn member_access_calls_and_indexing_stay_tight() {
    let out = fmt_stable("View M() {\nlet a = x.y.z(1)[0].w\nText(a . b)\nif (a) { b }\n}\n");
    assert!(out.contains("let a = x.y.z(1)[0].w"), "{out}");
    assert!(out.contains("Text(a.b)"), "{out}");
    assert!(out.contains("if (a) { b }"), "{out}");
}

// ---- strings ------------------------------------------------------------

/// A string, interpolations and braces included, is copied byte for byte,
/// and what is after it is indented as if the braces were not there (the old
/// formatter counted them).
#[test]
fn an_interpolated_string_is_copied_and_its_braces_do_not_count() {
    let src = "View Main() {\nvar count = 0\nText(\"{count} taps\")\nText(\"{ count  +  1 }:  {{count}}\")  #[a:1]\nText(\"after\")\n}\n";
    let out = fmt_stable(src);
    assert_eq!(
        out,
        "View Main() {\n    var count = 0\n    Text(\"{count} taps\")\n    Text(\"{ count  +  1 }:  {{count}}\") #[a: 1]\n    Text(\"after\")\n}\n"
    );
}

#[test]
fn strings_with_slashes_and_escapes_are_untouched() {
    let src = "View M() {\nText(\"http://a//b \\\" // not a comment\")\n}\n";
    let out = fmt_stable(src);
    assert!(
        out.contains("Text(\"http://a//b \\\" // not a comment\")"),
        "{out}"
    );
}

#[test]
fn a_string_spanning_lines_keeps_its_own_line_breaks() {
    let src = "View M() {\nText(\"one\n   two\") #[a: 1]\nText(\"x\")\n}\n";
    let out = fmt_stable(src);
    assert!(
        out.contains("Text(\"one\n   two\") #[a: 1]\n    Text(\"x\")"),
        "{out}"
    );
}

// ---- comments -----------------------------------------------------------

#[test]
fn a_comment_at_the_end_of_a_line_stays_there() {
    let out = fmt_stable("View M() { // the view\nvar a = 1   //   one\nText(\"x\") // text\n}\n");
    assert_eq!(
        out,
        "View M() { // the view\n    var a = 1 //   one\n    Text(\"x\") // text\n}\n"
    );
}

#[test]
fn a_comment_on_its_own_line_takes_the_indent_of_the_token_after_it() {
    let out = fmt_stable(
        "// top\nView M() {\n// inside\nvar a = 1\n        // before text\nText(\"x\")\n}\n",
    );
    assert_eq!(
        out,
        "// top\nView M() {\n    // inside\n    var a = 1\n    // before text\n    Text(\"x\")\n}\n"
    );
}

#[test]
fn a_comment_before_a_closer_belongs_to_the_lines_inside() {
    let out = fmt_stable("View M() {\nText(\"x\")\n// last word\n}\n// after\n");
    assert_eq!(
        out,
        "View M() {\n    Text(\"x\")\n    // last word\n}\n// after\n"
    );
}

#[test]
fn trailing_whitespace_in_a_comment_goes() {
    let out = fmt_stable("// one   \t\nView M() {}\n");
    assert_eq!(out, "// one\nView M() {}\n");
}

#[test]
fn a_comment_inside_a_call_pushes_what_follows_to_the_next_line() {
    let out = fmt_stable("View M() {\nText(a, // why\nb)\n}\n");
    assert_eq!(out, "View M() {\n    Text(a, // why\n        b)\n}\n");
}

/// Every line comment of the input is in the output, in order.
#[test]
fn no_comment_is_lost_or_reordered() {
    let src = "// a\nView M() { // b\n// c\nvar x = 1 // d\nText(\"x\") #[a: 1, // e\nb: 2]\n// f\n}\n// g";
    let out = fmt_stable(src);
    assert_eq!(comments(src), comments(&out));
    assert_eq!(comments(&out).len(), 7);
}

fn comments(source: &str) -> Vec<String> {
    lex_with_trivia(source)
        .pieces
        .into_iter()
        .filter_map(|(p, _)| match p {
            Piece::Trivia(Trivia::LineComment(c)) => Some(c.to_string()),
            _ => None,
        })
        .collect()
}

// ---- attribute lists ----------------------------------------------------

#[test]
fn a_short_attribute_list_is_joined_onto_one_line() {
    let out = fmt_stable("View M() {\nBox #[\n  bg: 1,\n  radius: 2\n,\n]\n}\n");
    assert_eq!(out, "View M() {\n    Box #[bg: 1, radius: 2]\n}\n");
}

#[test]
fn a_long_attribute_list_takes_a_line_per_attribute_with_a_trailing_comma() {
    let src = "View M() {\nColumn #[bg: 0x14141C, radius: 12, width: 560, height: 400, gap: 16, align: center, justify: center] {\nText(\"x\")\n}\n}\n";
    let out = fmt_stable(src);
    assert_eq!(
        out,
        "View M() {\n    Column #[\n        bg: 0x14141C,\n        radius: 12,\n        width: 560,\n        height: 400,\n        gap: 16,\n        align: center,\n        justify: center,\n    ] {\n        Text(\"x\")\n    }\n}\n"
    );
}

#[test]
fn a_list_exactly_at_the_width_stays_and_one_column_more_breaks() {
    // `        Box #[` is 14 columns with the indent; pad the value so the
    // closing `]` lands on column 100, then on 101.
    let make = |pad: usize| {
        format!(
            "View M() {{\n    Box #[a: \"{}\", b: 2]\n}}\n",
            "x".repeat(pad)
        )
    };
    let prefix = "    Box #[a: \"".len();
    let suffix = "\", b: 2]".len();
    let fits = 100 - prefix - suffix;
    let out = fmt_stable(&make(fits));
    assert!(out.contains("Box #[a: \""), "100 columns fit: {out}");
    let out = fmt_stable(&make(fits + 1));
    assert!(out.contains("Box #[\n        a: "), "101 break: {out}");
}

#[test]
fn a_trailing_comma_is_added_when_vertical_and_dropped_when_not() {
    let out = fmt_stable("View M() {\nBox #[a: 1, b: 2,]\nBox #[a: 1, // x\nb: 2]\n}\n");
    assert!(out.contains("Box #[a: 1, b: 2]\n"), "{out}");
    assert!(
        out.contains("Box #[\n        a: 1, // x\n        b: 2,\n    ]"),
        "{out}"
    );
}

#[test]
fn a_trailing_comment_on_the_last_attribute_keeps_the_comma_before_it() {
    let out = fmt_stable("View M() {\nBox #[a: 1,\nb: 2 // last\n]\n}\n");
    assert_eq!(
        out,
        "View M() {\n    Box #[\n        a: 1,\n        b: 2, // last\n    ]\n}\n"
    );
}

#[test]
fn an_attribute_list_with_a_block_stays_vertical() {
    let out = fmt_stable("View M() {\nBox #[tap => {\nn++\nm++\n}, bg: 1]\n}\n");
    assert_eq!(
        out,
        "View M() {\n    Box #[\n        tap => {\n            n++\n            m++\n        },\n        bg: 1,\n    ]\n}\n"
    );
}

#[test]
fn an_empty_list_and_a_lone_attribute_are_left_alone() {
    let out = fmt_stable("View M() {\nBox #[ ]\nBox #[a:1]\n}\n");
    assert_eq!(out, "View M() {\n    Box #[]\n    Box #[a: 1]\n}\n");
}

/// A lone attribute breaks onto a line of its own only when it is the list
/// that overruns; the column the `]` lands on decides, exactly.
#[test]
fn a_single_attribute_breaks_only_when_its_own_list_overruns() {
    let make = |pad: usize| format!("View M() {{\n    Box #[a: \"{}\"]\n}}\n", "x".repeat(pad));
    let fits = 100 - "    Box #[a: \"".len() - "\"]".len();
    assert!(fmt_stable(&make(fits)).contains("Box #[a: \""));
    assert!(fmt_stable(&make(fits + 1)).contains("Box #[\n        a: "));
}

/// The same boundary when only what follows overruns: a lone attribute whose
/// `]` is on column 100 stays, on column 101 it breaks.
#[test]
fn a_lone_attribute_with_an_overlong_tail_stays_unless_its_own_list_overruns() {
    let make = |pad: usize| {
        format!(
            "View M() {{\n    Button(\"{}\") #[bg: 1] => n = 1\n}}\n",
            "x".repeat(pad)
        )
    };
    let fits = 100 - "    Button(\"".len() - "\") #[bg: 1]".len();
    assert!(fmt_stable(&make(fits)).contains("#[bg: 1] => n = 1"));
    assert!(fmt_stable(&make(fits + 1)).contains("#[\n"));
}

/// A list that overruns because of what follows it (`=> action`) breaks; a
/// lone attribute does not, as a line to itself gains nothing.
#[test]
fn an_overlong_action_after_the_list_breaks_a_list_of_several() {
    let long = "x".repeat(70);
    let src = format!(
        "View M() {{\nButton(\"b\") #[bg: 1, radius: 2] => n = \"{long}\"\nButton(\"b\") #[bg: 1] => n = \"{long}\"\n}}\n"
    );
    let out = fmt_stable(&src);
    assert!(
        out.contains("Button(\"b\") #[\n        bg: 1,\n        radius: 2,\n    ] => n ="),
        "{out}"
    );
    assert!(out.contains("Button(\"b\") #[bg: 1] => n ="), "{out}");
}

/// When what is before the `#[` already overruns the width, the list goes to
/// the next line instead of breaking at an opener that is already too far.
#[test]
fn a_list_after_an_overlong_prefix_moves_to_its_own_line() {
    let long = "x".repeat(110);
    let src = format!("View M() {{\nText(\"{long}\") #[typo: body, color: 1]\n}}\n");
    let out = fmt_stable(&src);
    assert!(
        out.contains(&format!(
            "Text(\"{long}\")\n        #[typo: body, color: 1]\n"
        )),
        "{out}"
    );
}

// ---- lines --------------------------------------------------------------

#[test]
fn runs_of_blank_lines_become_one() {
    let out = fmt_stable("View A() {}\n\n\n\n\nView B() {\nvar a = 1\n\n\n\nvar b = 2\n}\n");
    assert_eq!(
        out,
        "View A() {}\n\nView B() {\n    var a = 1\n\n    var b = 2\n}\n"
    );
}

#[test]
fn no_blank_line_after_an_opener_or_before_a_closer() {
    let out = fmt_stable("View M() {\n\n\nvar a = 1\n\n}\n");
    assert_eq!(out, "View M() {\n    var a = 1\n}\n");
}

#[test]
fn a_closer_goes_on_its_own_line_when_its_opener_ended_a_line() {
    let out =
        fmt_stable("View M() {\nColumn {\nText(\"a\") }\nRow { Text(\"b\") }\nf(\na, b)\n}\n");
    assert_eq!(
        out,
        "View M() {\n    Column {\n        Text(\"a\")\n    }\n    Row { Text(\"b\") }\n    f(\n        a, b\n    )\n}\n"
    );
}

#[test]
fn the_file_ends_with_exactly_one_newline() {
    assert_eq!(fmt("View M() {}"), "View M() {}\n");
    assert_eq!(fmt("View M() {}\n\n\n"), "View M() {}\n");
    assert_eq!(fmt(""), "");
    assert_eq!(fmt("\n\n  \n"), "");
    assert_eq!(fmt("// only a comment"), "// only a comment\n");
}

#[test]
fn indentation_is_four_spaces_whatever_it_was() {
    let out = fmt_stable("View M() {\n\t\tColumn {\n  Text(\"a\")\n\t}\n}\n");
    assert_eq!(
        out,
        "View M() {\n    Column {\n        Text(\"a\")\n    }\n}\n"
    );
}

#[test]
fn a_line_that_carries_on_a_statement_is_indented_one_level_more() {
    let out = fmt_stable(
        "View M() {\nButton(\"a\") #[x: 1]\n=> n = 1\nlet v = a &&\nb\nlet w = a\n? b\n: c\nrun(1)\nok x => y\nerr e => {}\n}\n",
    );
    assert_eq!(
        out,
        "View M() {\n    Button(\"a\") #[x: 1]\n        => n = 1\n    let v = a &&\n        b\n    let w = a\n        ? b\n        : c\n    run(1)\n        ok x => y\n        err e => {}\n}\n"
    );
}

#[test]
fn what_follows_a_closer_belongs_to_the_line_its_bracket_opened_on() {
    let out = fmt_stable(
        "View M() {\nfor x in [1,\n2] {\nText(\"a\")\n}\nroute \"/a\" {|p|\nText(\"b\")\n}\n}\n",
    );
    assert_eq!(
        out,
        "View M() {\n    for x in [1,\n        2] {\n        Text(\"a\")\n    }\n    route \"/a\" {|p|\n        Text(\"b\")\n    }\n}\n"
    );
}

#[test]
fn else_chains_and_result_arms() {
    let src = "View M() {\non tap=>{\nif a {\nb\n}else if c{\nd\n}else{\ne\n}\napi.go(1)\nok r=>{ n=r }\nerr e=>{ n=0 }\n}\n}\n";
    let out = fmt_stable(src);
    assert!(out.contains("} else if c {"), "{out}");
    assert!(
        out.contains(
            "        api.go(1)\n            ok r => { n = r }\n            err e => { n = 0 }\n"
        ),
        "{out}"
    );
}

// ---- line endings ---------------------------------------------------------

#[test]
fn crlf_in_crlf_out() {
    let src = "// c\r\nView M() {\r\nvar a=1 // x\r\n\r\n\r\nText(\"a\")\r\n}\r\n";
    let out = fmt_stable(src);
    assert_eq!(
        out,
        "// c\r\nView M() {\r\n    var a = 1 // x\r\n\r\n    Text(\"a\")\r\n}\r\n"
    );
    assert!(!out.replace("\r\n", "").contains('\r'));
    assert!(!out.replace("\r\n", "").contains('\n'));
}

#[test]
fn lf_stays_lf() {
    assert!(!fmt("View M() {\nText(\"a\")\n}\n").contains('\r'));
}

// ---- the safety check ---------------------------------------------------------

#[test]
fn a_file_that_does_not_parse_is_refused_with_its_position() {
    let err = format("View M() {\n    Text(\n}\n").unwrap_err();
    let FmtError::Syntax { line, column, .. } = err else {
        panic!("expected a syntax error, got {err:?}");
    };
    assert!(line >= 2, "line {line}");
    assert!(column >= 1);
    assert!(matches!(format("View M( {"), Err(FmtError::Syntax { .. })));
    // A lexer error is refused too.
    assert!(matches!(
        format("View M() { @ }"),
        Err(FmtError::Syntax { .. })
    ));
}

#[test]
fn the_meaning_check_catches_a_changed_program() {
    let before = parse("View M() { var a = 1\nText(\"x\") #[b: 2] }\n");
    // Same program, different layout: accepted.
    assert!(keeps_meaning(
        &before,
        "View M() {\n    var a = 1\n    Text(\"x\") #[b: 2]\n}\n"
    ));
    // A changed literal, a dropped member, a renamed name, a changed
    // operator, a broken file: all refused.
    for wrong in [
        "View M() { var a = 2\nText(\"x\") #[b: 2] }\n",
        "View M() { var a = 1 }\n",
        "View M() { var c = 1\nText(\"x\") #[b: 2] }\n",
        "View M() { var a = 1\nText(\"y\") #[b: 2] }\n",
        "View M() { var a = 1\nText(\"x\") #[b: 3] }\n",
        "View M() { var a = 1\nText(\"x\") #[b: 2] ",
    ] {
        assert!(!keeps_meaning(&before, wrong), "accepted: {wrong}");
    }
}

/// `format` itself refuses a printer's wrong answer: a changed literal, a
/// lost token, output that no longer parses. Nothing is returned to write.
#[test]
fn format_refuses_a_printer_that_changes_the_program() {
    let src = "View M() {\n    var a = 1\n    Text(\"x\")\n}\n";
    assert_eq!(
        format_using(src, str::to_string).unwrap(),
        src,
        "an honest printer"
    );
    for wrong in [
        |s: &str| s.replace("= 1", "= 2"),
        |s: &str| s.replace("Text(\"x\")", ""),
        |s: &str| s.replace("{\n", "{\n    var b = 1\n"),
        |s: &str| s.trim_end_matches("}\n").to_string(),
        |s: &str| s.replace("var", "let"),
    ] {
        assert_eq!(format_using(src, wrong), Err(FmtError::ChangedMeaning));
    }
}

#[test]
fn span_blind_equality_covers_what_the_formatter_can_reach() {
    let a = parse("View M() { let x = a ? b : c\nlet y = f(1, g: 2)\nText(\"{x}\") }\n");
    let b =
        parse("View M() {\n    let x = a ? b : c\n    let y = f(1, g: 2)\n    Text(\"{x}\")\n}\n");
    assert!(parsed_files_span_eq(&a, &b));
    // Spans differ, so the derived equality does not hold: that is why the
    // check exists.
    assert_ne!(a.views, b.views);
    let c = parse("View M() { let x = a ? c : b\nlet y = f(1, g: 2)\nText(\"{x}\") }\n");
    assert!(!parsed_files_span_eq(&a, &c));
}

// ---- the repository's own files -------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn byd_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                byd_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "byd") {
            out.push(path);
        }
    }
}

fn repo_files() -> Vec<(PathBuf, String)> {
    let mut paths = Vec::new();
    byd_files(&repo_root(), &mut paths);
    paths
        .into_iter()
        .map(|p| {
            let src = std::fs::read_to_string(&p).unwrap();
            (p, src)
        })
        .collect()
}

/// RFC-0044 Acceptance: for every `.byd` file in the repository, formatting
/// twice gives what formatting once did, the output means what the input did,
/// and no comment is lost.
#[test]
fn every_byd_file_in_the_repository_formats_idempotently_and_keeps_its_meaning() {
    let files = repo_files();
    assert!(files.len() >= 60, "found only {} files", files.len());
    for (path, src) in &files {
        let once = format(src).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let twice = format(&once).unwrap_or_else(|e| panic!("{} (2nd): {e}", path.display()));
        assert_eq!(once, twice, "{} is not idempotent", path.display());

        // The same comparison `format` makes, made again here on its own.
        let (before, after) = (parse(src), parse(&once));
        assert!(before.errors.is_empty() && after.errors.is_empty());
        assert!(
            parsed_files_span_eq(&before, &after),
            "{} changed meaning",
            path.display()
        );
        assert_eq!(
            comments(src),
            comments(&once),
            "{} lost a comment",
            path.display()
        );
        assert!(
            !once.contains(" \n"),
            "{} has trailing whitespace",
            path.display()
        );
        assert!(
            once.ends_with('\n') && !once.ends_with("\n\n"),
            "{}",
            path.display()
        );
    }
}

/// The layout does not depend on the indentation it was given: the
/// repository's files with every line's indentation stripped format to what
/// they format to as written.
#[test]
fn indentation_in_the_input_never_matters() {
    for (path, src) in repo_files() {
        // Joined with the file's own line ending: a Windows checkout gives the
        // files CRLF ones, and the formatter keeps whichever it is given.
        let eol = if src.contains("\r\n") { "\r\n" } else { "\n" };
        let flat: String = src
            .lines()
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join(eol);
        assert_eq!(
            fmt(&flat),
            fmt(&src),
            "{} formats differently without its indentation",
            path.display()
        );
    }
}

/// One change at a time, in every kind of node the formatter's check has to
/// see: the second program is never the same as the first.
#[test]
fn every_kind_of_difference_is_seen_by_the_meaning_check() {
    let pairs = [
        ("View M() { let a = x + y }", "View M() { let a = x - y }"),
        ("View M() { let a = x + y }", "View M() { let a = y + x }"),
        (
            "View M() { let a = x ? 1 : 2 }",
            "View M() { let a = x ? 2 : 1 }",
        ),
        ("View M() { let a = -x }", "View M() { let a = !x }"),
        (
            "View M() { let a = x ? 1 : 2 }",
            "View M() { let a = x ? 1 : 3 }",
        ),
        (
            "View M() { let a = x ? 1 : 2 }",
            "View M() { let a = y ? 1 : 2 }",
        ),
        ("View M() { let a = x + y }", "View M() { let a = x + z }"),
        (
            "View M() { let a = f(x)(y) }",
            "View M() { let a = f(x)(z) }",
        ),
        ("View M() { let a = f(1, 2) }", "View M() { let a = f(1) }"),
        (
            "View M() { let a = f(k: 1) }",
            "View M() { let a = f(j: 1) }",
        ),
        ("View M() { let a = [1, 2] }", "View M() { let a = (1, 2) }"),
        ("View M() { let a = x.y }", "View M() { let a = x.z }"),
        ("View M() { let a = x[0] }", "View M() { let a = x[1] }"),
        ("View M() { let a = 1.5 }", "View M() { let a = 1.25 }"),
        ("View M() { let a = 90deg }", "View M() { let a = 91deg }"),
        (
            "View M() { let a = \"a{x}\" }",
            "View M() { let a = \"a{y}\" }",
        ),
        (
            "View M() { let a = \"{x:.1}\" }",
            "View M() { let a = \"{x:.2}\" }",
        ),
        (
            "View M() { let a = {k: 1} }",
            "View M() { let a = {k: 1, j: 2} }",
        ),
        (
            "View M() { let a = {..r, k: 1} }",
            "View M() { let a = {k: 1} }",
        ),
        ("View M() { let a = |x| x }", "View M() { let a = |y| y }"),
        (
            "View M() { let a = x with anim.linear() }",
            "View M() { let a = x with anim.spring() }",
        ),
        ("View M() { Box #[a: 1] }", "View M() { Box #[a: 1, b: 1] }"),
        ("View M() { Box #[a: 1] }", "View M() { Box #[a => 1] }"),
        ("View M() { Box #[a.x: 1] }", "View M() { Box #[a.y: 1] }"),
        ("View M() { Box #[..s] }", "View M() { Box #[s: 1] }"),
        ("View M() { Box => n++ }", "View M() { Box => n-- }"),
        (
            "View M() { Box { Text(\"a\") } }",
            "View M() { Box { } Text(\"a\") }",
        ),
        ("View M() { Box as a }", "View M() { Box as b }"),
        (
            "View M() { when a { x } else { y } }",
            "View M() { when a { x } }",
        ),
        (
            "View M() { for i in a { x } }",
            "View M() { for i, j in a { x } }",
        ),
        ("View M() { on mount => a }", "View M() { on unmount => a }"),
        ("View M() { every 5s => a }", "View M() { after 5s => a }"),
        ("View M() { every 5s => a }", "View M() { every 6s => a }"),
        (
            "View M() { on change(a) => b }",
            "View M() { on change(a, c) => b }",
        ),
        (
            "View M() { on measure => a }",
            "View M() { on measure => b }",
        ),
        (
            "View M() { route \"/a\" { x } }",
            "View M() { tab \"/a\" { x } }",
        ),
        (
            "View M() { route \"/a\" { x } }",
            "View M() { route \"/b\" { x } }",
        ),
        (
            "View M() { route \"/a\" {|p| x } }",
            "View M() { route \"/a\" { x } }",
        ),
        (
            "View M() { var a: Int = 1 }",
            "View M() { var a: Float = 1 }",
        ),
        (
            "View M() { var a: List<Int> = 1 }",
            "View M() { var a: List<Str> = 1 }",
        ),
        ("View M() { var a = 1 }", "View M() { let a = 1 }"),
        ("View M() { inject T as t }", "View M() { inject T as u }"),
        (
            "View M() { fn f(a: Int) -> Int => a }",
            "View M() { fn f(a: Int) -> Str => a }",
        ),
        (
            "View M() { style { .a #[x: 1] } }",
            "View M() { style { .b #[x: 1] } }",
        ),
        (
            "View M() { let s = style { x: 1 } }",
            "View M() { let s = style { x: 2 } }",
        ),
        (
            "View M() { let s = style { on hover { x: 1 } } }",
            "View M() { let s = style { on pressed { x: 1 } } }",
        ),
        (
            "View M() { let s = style { on width >= md { x: 1 } } }",
            "View M() { let s = style { on width < md { x: 1 } } }",
        ),
        (
            "View M() { let s = style { on width >= 600 { x: 1 } } }",
            "View M() { let s = style { on width >= 601 { x: 1 } } }",
        ),
        (
            "View M() { a.b(1) ok r => x err e => y }",
            "View M() { a.b(1) ok r => x }",
        ),
        (
            "View M() { a.b(1) ok r => x }",
            "View M() { a.b(1) ok s => x }",
        ),
        ("View M(a: Int = 1) { x }", "View M(a: Int = 2) { x }"),
        ("View M(a: Int) { x }", "View M(b: Int) { x }"),
        (
            "View M() { x }\nView N() { x }",
            "View N() { x }\nView M() { x }",
        ),
        ("fn f() => 1\nView M() { x }", "fn f() => 2\nView M() { x }"),
        (
            "type T = { a: Int }\nView M() { x }",
            "type T = { a: Int? }\nView M() { x }",
        ),
        ("use a\nView M() { x }", "use a as b\nView M() { x }"),
        ("use a.{B}\nView M() { x }", "use a.{C}\nView M() { x }"),
        (
            "View M() { on tap => { if a { b } } }",
            "View M() { on tap => { if a { b } else { c } } }",
        ),
        (
            "View M() { on tap => { let a = 1 } }",
            "View M() { on tap => { let b = 1 } }",
        ),
        (
            "View M() { let a = anim.keyframes(0%: 1, 50%: 2 ease_out) }",
            "View M() { let a = anim.keyframes(0%: 1, 60%: 2 ease_out) }",
        ),
        (
            "View M() { let a = anim.keyframes(0%: 1, 50%: 2 ease_out) }",
            "View M() { let a = anim.keyframes(0%: 1, 50%: 2 ease_in) }",
        ),
        ("View M() { let v = a ?? b }", "View M() { let v = b ?? a }"),
    ];
    for (a, b) in pairs {
        let (pa, pb) = (parse(a), parse(b));
        assert!(pa.errors.is_empty(), "{a}: {:?}", pa.errors);
        assert!(pb.errors.is_empty(), "{b}: {:?}", pb.errors);
        assert!(parsed_files_span_eq(&pa, &pa), "{a} differs from itself");
        assert!(!parsed_files_span_eq(&pa, &pb), "not told apart:\n{a}\n{b}");
        assert!(!parsed_files_span_eq(&pb, &pa), "not told apart:\n{b}\n{a}");
    }
}

/// The repository's own `.byd` files are formatted, like its Rust: a file
/// that is not is listed, and `byard fmt` fixes it.
#[test]
fn the_repositorys_byd_files_are_formatted() {
    let unformatted: Vec<String> = repo_files()
        .into_iter()
        .filter(|(_, src)| fmt(src) != *src)
        .map(|(path, _)| path.display().to_string())
        .collect();
    assert!(
        unformatted.is_empty(),
        "run `cargo run -p byard-cli -- fmt .` to format:\n{}",
        unformatted.join("\n")
    );
}
