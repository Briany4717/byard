//! A token stream that keeps what [`lex`](super::lex) throws away: the line
//! comments and the line breaks between tokens (RFC-0044 §1).
//!
//! The lexer skips whitespace and `//` comments at the enum level, so a
//! parser never sees them and pays nothing for them. A formatter needs both,
//! so [`lex_with_trivia`] runs the very same lexer and then reads the text
//! *between* two tokens' spans, which can hold nothing but whitespace and
//! comments (anything else is a token or a lex error, and both have spans).
//! One definition of what a token is, and no second copy of the grammar's
//! terminals to drift from the first.

use logos::Logos;

use super::{LexError, Token};
use crate::diagnostics::{CompileError, Span};

/// What sits between two tokens and is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trivia<'a> {
    /// A `// …` comment, from the slashes to the end of the line, without the
    /// line break and without trailing whitespace (nor the `\r` of a CRLF).
    LineComment(&'a str),
    /// A run of whitespace holding this many line breaks (at least one). A
    /// blank line shows as two or more; spaces and tabs are not recorded, a
    /// formatter chooses its own.
    Newlines(u32),
}

/// One element of the stream [`lex_with_trivia`] returns.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece<'a> {
    /// A token, exactly as [`lex`](super::lex) produces it.
    Token(Token),
    /// Whitespace or a comment between tokens.
    Trivia(Trivia<'a>),
}

/// The result of [`lex_with_trivia`]: tokens and trivia in source order, plus
/// the diagnostics [`lex`](super::lex) would have produced for the same text.
#[derive(Debug, Default)]
pub struct TriviaFile<'a> {
    /// Tokens and trivia, in source order, each with its byte span. A
    /// `Newlines` span covers the whole whitespace run; a `LineComment` span
    /// covers the comment as written (including trailing whitespace the
    /// `&str` omits).
    pub pieces: Vec<(Piece<'a>, Span)>,
    /// Lex diagnostics. A formatter refuses a file that has any.
    pub errors: Vec<CompileError>,
}

/// Lexes `source` like [`lex`](super::lex), keeping line comments and line
/// breaks as [`Trivia`] between the tokens. Spaces and tabs are dropped.
///
/// The token pieces are the tokens `lex` returns, in the same order, and the
/// diagnostics are the same; only the trivia is added. [`lex`](super::lex) is
/// unchanged and still skips it, so parsing does not pay for any of this.
#[must_use]
pub fn lex_with_trivia(source: &str) -> TriviaFile<'_> {
    let mut lexer = Token::lexer(source);
    // About one trivia piece for every two tokens in real files; reserving
    // for the tokens avoids most regrowth without a counting pass.
    let mut out = TriviaFile {
        pieces: Vec::with_capacity(source.len() / 4),
        errors: Vec::new(),
    };
    let mut cursor = 0usize;
    while let Some(result) = lexer.next() {
        let span = lexer.span();
        push_gap(source, cursor, span.start, &mut out.pieces);
        cursor = span.end;
        let span = Span::from(span);
        match result {
            Ok(token) => out.pieces.push((Piece::Token(token), span)),
            Err(err) => out.errors.push(match err {
                LexError::StringNestingTooDeep => CompileError::StringNestingTooDeep { span },
                LexError::UnterminatedString => CompileError::UnterminatedString { span },
                LexError::Unexpected => CompileError::UnexpectedChar { span },
            }),
        }
    }
    push_gap(source, cursor, source.len(), &mut out.pieces);
    out
}

/// Reads the whitespace and comments of `source[from..to]` into `pieces`.
///
/// The gap holds only what the skip rules matched: `[ \t\r\n]+` and
/// `//[^\n]*`. A `\r` is whitespace and, before a `\n`, part of the line
/// break; the line break itself is the `\n`.
fn push_gap<'a>(source: &'a str, from: usize, to: usize, pieces: &mut Vec<(Piece<'a>, Span)>) {
    let bytes = source.as_bytes();
    let mut i = from;
    while i < to {
        match bytes[i] {
            b'\n' => {
                let start = i;
                let mut newlines = 0u32;
                while i < to && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                    if bytes[i] == b'\n' {
                        newlines += 1;
                    }
                    i += 1;
                }
                pieces.push((
                    Piece::Trivia(Trivia::Newlines(newlines)),
                    Span::from(start..i),
                ));
            }
            b' ' | b'\t' | b'\r' => {
                // Whitespace on a line. Only worth a piece when it runs into a
                // line break, so look ahead without recording the spaces.
                let start = i;
                let mut newlines = 0u32;
                while i < to && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                    if bytes[i] == b'\n' {
                        newlines += 1;
                    }
                    i += 1;
                }
                if newlines > 0 {
                    pieces.push((
                        Piece::Trivia(Trivia::Newlines(newlines)),
                        Span::from(start..i),
                    ));
                }
            }
            _ => {
                // `//`: to the end of the line, the `\n` excluded.
                let start = i;
                while i < to && bytes[i] != b'\n' {
                    i += 1;
                }
                let text = source[start..i].trim_end();
                pieces.push((
                    Piece::Trivia(Trivia::LineComment(text)),
                    Span::from(start..i),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn pieces(source: &str) -> Vec<Piece<'_>> {
        lex_with_trivia(source)
            .pieces
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    fn comment(text: &str) -> Piece<'_> {
        Piece::Trivia(Trivia::LineComment(text))
    }

    fn newlines(n: u32) -> Piece<'static> {
        Piece::Trivia(Trivia::Newlines(n))
    }

    #[test]
    fn comments_and_line_breaks_sit_between_the_tokens() {
        assert_eq!(
            pieces("a // one\n\n  b\n"),
            vec![
                Piece::Token(Token::Ident(crate::symbol::Symbol::intern("a"))),
                comment("// one"),
                newlines(2),
                Piece::Token(Token::Ident(crate::symbol::Symbol::intern("b"))),
                newlines(1),
            ]
        );
    }

    /// Spaces and tabs on a line are not recorded, and a comment ends where
    /// its text does: no trailing whitespace, no `\r` of a CRLF.
    #[test]
    fn a_comment_drops_trailing_whitespace_and_the_carriage_return() {
        assert_eq!(
            pieces("// x \t \r\n// y"),
            vec![comment("// x"), newlines(1), comment("// y")]
        );
        assert_eq!(pieces("a \t b").len(), 2, "no trivia for same-line spaces");
    }

    /// A blank line is two line breaks, however much whitespace is on it.
    #[test]
    fn blank_lines_count_their_breaks() {
        assert_eq!(
            pieces("a\n  \n\t\n\nb")[1],
            newlines(4),
            "four breaks, three blank lines"
        );
    }

    /// `//` inside a string is the string's, not a comment.
    #[test]
    fn slashes_in_a_string_are_not_a_comment() {
        let got = pieces("\"http://x\" // real");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Piece::Token(Token::StrLit));
        assert_eq!(got[1], comment("// real"));
    }

    /// The token pieces are exactly what `lex` returns, spans included, and
    /// the diagnostics are the same: `lex` is unchanged by all this.
    #[test]
    fn the_tokens_are_the_tokens_lex_returns() {
        let source = "View Main() { // c\n    var n = 1 // d\n\n    Text(\"{n}\") #[a: 2ms]\n} @";
        let plain = lex(source);
        let with = lex_with_trivia(source);
        let kept: Vec<_> = with
            .pieces
            .iter()
            .filter_map(|(p, s)| match p {
                Piece::Token(t) => Some((t.clone(), *s)),
                Piece::Trivia(_) => None,
            })
            .collect();
        assert_eq!(kept, plain.tokens);
        assert_eq!(with.errors, plain.errors);
        assert!(!with.errors.is_empty(), "the stray `@` is reported");
    }

    /// Every byte of the source is in a token's span, a trivia span, or
    /// spaces and tabs; the spans never overlap and stay in order.
    #[test]
    fn spans_are_ordered_and_inside_the_source() {
        let source = "a // x\n\n b";
        let mut end = 0;
        for (_, span) in lex_with_trivia(source).pieces {
            assert!(span.start as usize >= end);
            end = span.end as usize;
        }
        assert!(end <= source.len());
    }
}
