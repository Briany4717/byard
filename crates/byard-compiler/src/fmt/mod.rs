//! The `byld` formatter (RFC-0044): one layout for every `.byd` file, no
//! options.
//!
//! [`format`] works on the token stream with comments and line breaks kept
//! ([`lex_with_trivia`](crate::lexer::lex_with_trivia)), decides per token
//! whether a space, nothing or a line break comes before it, and copies every
//! token's text from the source byte for byte, so a string (and the
//! interpolations in it) is never touched. It is one pass over the pieces with
//! a small state, and it allocates one output `String`.
//!
//! It never changes what a file means. Before returning, [`format`] parses
//! both the input and the output and compares the two trees with positions
//! ignored ([`SpanEq`](crate::parser::span_eq::SpanEq)); if they differ, or
//! the input does not parse, the caller gets a [`FmtError`] and keeps its file
//! as it was. That check is never skipped: a formatter that changes meaning is
//! worse than none.
//!
//! # The style
//!
//! - Four-space indentation, one level per open `{`, `(` or `[` that spans
//!   lines. A line that carries on a statement (`=> action`, `#[…]`, a result
//!   arm, a trailing operator) is indented one further level.
//! - One space around binary operators, `=>`, `=` and after `,` and `:`;
//!   none inside brackets or before `,`.
//! - An `#[ … ]` attribute list stays on one line when it fits in
//!   [`MAX_WIDTH`] columns, else it takes one attribute per line with a
//!   trailing comma.
//! - Line breaks the author wrote stay (the language ignores them, but they
//!   are how a person groups a statement); runs of blank lines become one,
//!   and there is none after a `{` or before a `}`.
//! - Comments stay where they were: a comment at the end of a line stays
//!   there, one on its own line stays above the token after it.
//! - A final newline, no trailing whitespace. Line endings follow the file:
//!   CRLF in, CRLF out.

mod printer;

#[cfg(test)]
mod tests;

use std::fmt;

use crate::diagnostics::CompileError;
use crate::parser::span_eq::parsed_files_span_eq;
use crate::parser::{ParsedFile, parse};

/// The width, in columns, an attribute list is kept within on one line. The
/// same as the workspace's Rust (`rustfmt`'s default `max_width`).
pub const MAX_WIDTH: usize = 100;

/// Why [`format`] left a file alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmtError {
    /// The source does not parse, so there is no meaning to preserve. The
    /// first diagnostic, located; fix it (`byard check` lists the rest).
    Syntax {
        /// 1-based line of the first problem.
        line: usize,
        /// 1-based column (in characters) of the first problem.
        column: usize,
        /// What was wrong, as `byard check` words it.
        message: String,
    },
    /// The formatted text does not parse to the same program as the input.
    /// That is a bug in the formatter, never in the file; the file is left
    /// untouched.
    ChangedMeaning,
}

impl fmt::Display for FmtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax {
                line,
                column,
                message,
            } => write!(f, "{line}:{column}: {message}"),
            Self::ChangedMeaning => f.write_str(
                "formatting would change what the file means, so it was left alone \
                 (this is a bug in `byard fmt`, please report it with the file)",
            ),
        }
    }
}

impl std::error::Error for FmtError {}

/// Formats `.byd` source.
///
/// Returns the formatted text, which equals `source` when it was already
/// formatted. Formatting is idempotent: `format(&format(s)?)` is
/// `format(s)`.
///
/// # Errors
///
/// [`FmtError::Syntax`] when `source` does not parse, [`FmtError::ChangedMeaning`]
/// when the safety check finds the output is a different program.
pub fn format(source: &str) -> Result<String, FmtError> {
    format_using(source, printer::print)
}

/// [`format`] with the layout step supplied, so a test can hand it a printer
/// that gets it wrong and see the safety check refuse the result.
fn format_using(source: &str, print: impl FnOnce(&str) -> String) -> Result<String, FmtError> {
    let before = parse(source);
    if let Some(first) = before.errors.first() {
        return Err(syntax_error(source, first));
    }

    let formatted = print(source);

    if !keeps_meaning(&before, &formatted) {
        return Err(FmtError::ChangedMeaning);
    }
    Ok(formatted)
}

/// The safety check: `formatted` parses without errors to the same program as
/// `before`, positions aside.
fn keeps_meaning(before: &ParsedFile, formatted: &str) -> bool {
    let after = parse(formatted);
    after.errors.is_empty() && parsed_files_span_eq(before, &after)
}

/// Locates a diagnostic for [`FmtError::Syntax`].
fn syntax_error(source: &str, error: &CompileError) -> FmtError {
    let start = (error.span().start as usize).min(source.len());
    // Snap to a character boundary so a span in the middle of one cannot panic.
    let start = (0..=start)
        .rev()
        .find(|&i| source.is_char_boundary(i))
        .unwrap_or(0);
    let before = &source[..start];
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    FmtError::Syntax {
        line,
        column: source[line_start..start].chars().count() + 1,
        message: error.headline(),
    }
}
