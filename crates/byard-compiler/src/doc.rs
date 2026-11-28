//! `///` doc comments: what a declaration says about itself, for the editor's
//! hover and for `byard doc`.
//!
//! A doc comment is the run of `///` lines right above a declaration, with
//! nothing but blank space before each on its line, and no blank line
//! between it and the declaration:
//!
//! ```text
//! /// A card with a title.
//! ///
//! /// `label` is drawn in the card's own colour.
//! View Card(label: Str) { … }
//! ```
//!
//! The comment is read from the text, above the line the declaration starts
//! on, so the parser and the tree know nothing of it: a `///` anywhere else is
//! the comment it always was. Each line loses its `///` and one space after
//! it, and the lines are Markdown.

use std::fmt::Write as _;

use crate::parser::ParsedFile;
use crate::parser::ast::{ConstDecl, EnumDecl, FnDecl, RecordDecl, ViewDecl};

/// The doc comment of the declaration that starts at byte `at` of `source`.
#[must_use]
pub fn comment_above(source: &str, at: usize) -> Option<String> {
    let at = at.min(source.len());
    let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut lines: Vec<&str> = Vec::new();
    for line in source[..line_start].lines().rev() {
        let Some(text) = line.trim_start().strip_prefix("///") else {
            break;
        };
        // `////…` is a rule, not a doc comment.
        if text.starts_with('/') {
            break;
        }
        lines.push(text.strip_prefix(' ').unwrap_or(text).trim_end());
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    let text = lines.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The declaration's first line as written, up to where its body begins: a
/// view's `View Card(label: Str)`, a function's `fn total(n: Int) -> Int`.
fn header(source: &str, start: usize, end: usize, stop: &[&str]) -> String {
    let text = source.get(start..end.min(source.len())).unwrap_or_default();
    let cut = stop
        .iter()
        .filter_map(|s| depth_zero_find(text, s))
        .min()
        .unwrap_or(text.len());
    text[..cut].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where `needle` first occurs in `text` outside parentheses, brackets and
/// strings.
fn depth_zero_find(text: &str, needle: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'(' | b'[' | b'<' => depth += 1,
                b')' | b']' | b'>' if depth > 0 => depth -= 1,
                _ if depth == 0 && text[i..].starts_with(needle) => return Some(i),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// The whole declaration as written, on one line per line it took.
fn whole(source: &str, start: usize, end: usize) -> String {
    source
        .get(start..end.min(source.len()))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// What is said of one declaration: its signature in a `byld` block, and its
/// doc comment under it.
fn described(signature: &str, doc: Option<String>) -> String {
    let mut out = format!("```byld\n{signature}\n```");
    if let Some(doc) = doc {
        out.push_str("\n\n");
        out.push_str(&doc);
    }
    out
}

fn span_of(span: crate::diagnostics::Span) -> (usize, usize) {
    (span.start as usize, span.end as usize)
}

/// A view: `View Card(label: Str)`, and its doc comment.
#[must_use]
pub fn view(source: &str, decl: &ViewDecl) -> String {
    let (start, end) = span_of(decl.span);
    described(
        &header(source, start, end, &["{"]),
        comment_above(source, start),
    )
}

/// A function: `fn total(n: Int) -> Int`, and its doc comment.
#[must_use]
pub fn function(source: &str, decl: &FnDecl) -> String {
    let (start, end) = span_of(decl.span);
    described(
        &header(source, start, end, &["=>"]),
        comment_above(source, start),
    )
}

/// A `type`, a `context` or a `store`, whole, and its doc comment.
#[must_use]
pub fn record(source: &str, decl: &RecordDecl) -> String {
    let (start, end) = span_of(decl.span);
    described(&whole(source, start, end), comment_above(source, start))
}

/// An enum, whole, and its doc comment.
#[must_use]
pub fn enumeration(source: &str, decl: &EnumDecl) -> String {
    let (start, end) = span_of(decl.span);
    described(&whole(source, start, end), comment_above(source, start))
}

/// A constant, whole, and its doc comment.
#[must_use]
pub fn constant(source: &str, decl: &ConstDecl) -> String {
    let (start, end) = span_of(decl.span);
    described(&whole(source, start, end), comment_above(source, start))
}

/// The Markdown page of a package or a project: every declaration its files
/// make public, by kind, each with its signature and its doc comment. A
/// `private` one is the package's own, and is left out.
#[must_use]
pub fn page(title: &str, files: &[(String, String)]) -> String {
    let parsed: Vec<(&str, ParsedFile)> = files
        .iter()
        .map(|(_, source)| (source.as_str(), crate::parser::parse(source)))
        .collect();
    let mut out = format!("# {title}\n");
    let mut section = |heading: &str, entries: Vec<(String, String)>| {
        if entries.is_empty() {
            return;
        }
        let mut entries = entries;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let _ = write!(out, "\n## {heading}\n");
        for (name, text) in entries {
            let _ = write!(out, "\n### {name}\n\n{text}\n");
        }
    };
    section(
        "Views",
        parsed
            .iter()
            .flat_map(|(src, p)| {
                p.views
                    .iter()
                    .filter(|v| !v.private)
                    .map(|v| (v.name.as_str().to_string(), view(src, v)))
            })
            .collect(),
    );
    section(
        "Functions",
        parsed
            .iter()
            .flat_map(|(src, p)| {
                p.fns
                    .iter()
                    .filter(|f| !f.private)
                    .map(|f| (f.name.as_str().to_string(), function(src, f)))
            })
            .collect(),
    );
    section(
        "Types",
        parsed
            .iter()
            .flat_map(|(src, p)| {
                p.records
                    .iter()
                    .filter(|r| !r.private && !r.context)
                    .map(|r| (r.name.as_str().to_string(), record(src, r)))
            })
            .collect(),
    );
    section(
        "Enums",
        parsed
            .iter()
            .flat_map(|(src, p)| {
                p.enums
                    .iter()
                    .filter(|e| !e.private)
                    .map(|e| (e.name.as_str().to_string(), enumeration(src, e)))
            })
            .collect(),
    );
    section(
        "Contexts and stores",
        parsed
            .iter()
            .flat_map(|(src, p)| {
                p.records
                    .iter()
                    .filter(|r| !r.private && r.context)
                    .map(|r| (r.name.as_str().to_string(), record(src, r)))
            })
            .collect(),
    );
    section(
        "Constants",
        parsed
            .iter()
            .flat_map(|(src, p)| {
                p.consts
                    .iter()
                    .filter(|c| !c.private)
                    .map(|c| (c.name.as_str().to_string(), constant(src, c)))
            })
            .collect(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"/// A card.
///
/// It draws `label`.
View Card(label: Str, n: Int = 1) {
    Text(label)
}

// An ordinary comment.
/// The total, with tax.
fn total(price: Int, tax: Int = price / 10) -> Int => price + tax

/// Not this one: a blank line follows.

type Place = { name: Str }

//// A rule, not a comment.
enum Size { small, large }

private View Inner() { Text("in") }
"#;

    #[test]
    fn the_comment_is_the_run_of_doc_lines_right_above() {
        let parsed = crate::parser::parse(SOURCE);
        let card = &parsed.views[0];
        assert_eq!(
            comment_above(SOURCE, card.span.start as usize).as_deref(),
            Some("A card.\n\nIt draws `label`.")
        );
        let place = &parsed.records[0];
        assert_eq!(comment_above(SOURCE, place.span.start as usize), None);
        let size = &parsed.enums[0];
        assert_eq!(comment_above(SOURCE, size.span.start as usize), None);
    }

    #[test]
    fn a_signature_is_the_declaration_up_to_its_body() {
        let parsed = crate::parser::parse(SOURCE);
        assert!(
            view(SOURCE, &parsed.views[0])
                .starts_with("```byld\nView Card(label: Str, n: Int = 1)\n```")
        );
        let f = function(SOURCE, &parsed.fns[0]);
        assert!(
            f.starts_with("```byld\nfn total(price: Int, tax: Int = price / 10) -> Int\n```"),
            "{f}"
        );
        assert!(f.ends_with("The total, with tax."), "{f}");
    }

    #[test]
    fn a_page_lists_what_is_public_by_kind() {
        let page = page("kit", &[("main.byd".to_string(), SOURCE.to_string())]);
        assert!(page.starts_with("# kit\n"));
        for heading in [
            "## Views",
            "### Card",
            "## Functions",
            "### total",
            "## Types",
            "## Enums",
        ] {
            assert!(page.contains(heading), "{heading} in\n{page}");
        }
        assert!(
            !page.contains("Inner"),
            "a private view is the package's own:\n{page}"
        );
    }
}
