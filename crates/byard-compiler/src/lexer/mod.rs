//! Lexer: source text → `Token` stream (RFC-0002 §"Lexer" + full `Token` set;
//! RFC-0003 §"Attribute syntax"; D12 nesting cap).
//!
//! A single `#[derive(Logos)]` enum compiles to one DFA. Whitespace and line
//! comments are skipped at the enum level. String interpolation is handled by a
//! manual callback (`lex_string`) that emits one raw `StrLit` token whose end
//! is located with a fixed-depth two-state stack (`InString` / `InBrace`); the
//! *parser* later re-invokes the pipeline on each `{...}` span (the PEP 701
//! model). Nesting interpolated strings deeper than 3 levels is rejected as
//! [`CompileError::StringNestingTooDeep`] (D12).
//!
//! The driver ([`lex`]) wraps every `logos` lex failure as a [`CompileError`]
//! with a [`Span`], there are no silent failures.

use logos::{Lexer, Logos};

mod trivia;
pub use trivia::{Piece, Trivia, TriviaFile, lex_with_trivia};

use crate::diagnostics::{CompileError, Span};
use crate::symbol::Symbol;

/// Lexer-internal error, carried by `logos` and mapped to a [`CompileError`]
/// (with a span) by the [`lex`] driver. It carries no span itself; the driver
/// attaches `lexer.span()` at the point of failure.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum LexError {
    /// A byte that begins no valid token (the `logos` default error).
    #[default]
    Unexpected,
    /// More than 3 levels of interpolated-string nesting (D12).
    StringNestingTooDeep,
    /// A `"` opened a string that never closed before end of input.
    UnterminatedString,
    /// A `/*` opened a comment that never closed before end of input.
    UnterminatedComment,
}

/// Bit 32 of a [`Token::IntLit`], set at lex time on a hex literal written
/// with **more than six digits**, the RFC-0005 §1 alpha-first `0xAARRGGBB`
/// colour form. The tag is what lets a colour consumer distinguish
/// `0x00FFFFFF` (transparent white, alpha byte explicitly written as zero)
/// from `0xFFFFFF` (opaque white): the two are the same `i64` value.
/// Channel extraction always truncates to `u32`, so the tag never bleeds
/// into a colour; computed (untagged) values keep the magnitude heuristic
/// (`> 0xFFFFFF` ⇒ alpha present).
pub const COLOR_HAS_ALPHA_TAG: i64 = 1 << 32;

/// The value of a colour written `#RGB`, `#RRGGBB` or `#RRGGBBAA` (the
/// digits after the `#`): what `0xRRGGBB` is, or for eight digits the
/// alpha-first `0xAARRGGBB` with [`COLOR_HAS_ALPHA_TAG`], as an eight-digit
/// `0x…` is.
fn hex_color(digits: &str) -> Option<i64> {
    let digits: String = if digits.len() == 3 {
        digits.chars().flat_map(|c| [c, c]).collect()
    } else {
        digits.to_string()
    };
    let v = i64::from_str_radix(&digits, 16).ok()?;
    Some(if digits.len() == 8 {
        ((v & 0xFF) << 24) | (v >> 8) | COLOR_HAS_ALPHA_TAG
    } else {
        v
    })
}

/// The terminal set of the `byld` Lume surface (RFC-0002 §"Data structures",
/// RFC-0003 §"Attribute syntax").
///
/// `#[` is a single token ([`Token::HashBracket`]), never `#` followed by `[`,
/// so it can never lex ambiguously against a future standalone `#`.
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(error = LexError)]
#[logos(skip r"[ \t\r\n]+")]
#[logos(skip r"//[^\n]*")]
// A byte order mark, which some editors put at the start of a file.
#[logos(skip "\u{feff}")]
pub enum Token {
    // ---- keywords (reserved) ----
    /// `View`
    #[token("View")]
    View,
    /// `var`
    #[token("var")]
    Var,
    /// `let`
    #[token("let")]
    Let,
    /// `fn`
    #[token("fn")]
    Fn,
    /// `inject`
    #[token("inject")]
    Inject,
    /// `as`
    #[token("as")]
    As,
    /// `for`
    #[token("for")]
    For,
    /// `in`
    #[token("in")]
    In,
    /// `when`
    #[token("when")]
    When,
    /// `else`
    #[token("else")]
    Else,
    /// `style`
    #[token("style")]
    Style,
    /// `untrack` (reserved intrinsic; D2, parsed as a call, dispatched in the
    /// interpreter).
    #[token("untrack")]
    Untrack,
    /// `with` (RFC-0010 A1): the infix animation operator inside `#[...]`
    /// values (`radius: pressed ? 3 : 10 with anim.spring()`). Reserved as its
    /// own token so it parses cleanly and can never be read as an identifier in
    /// value position.
    #[token("with")]
    With,
    /// `merge` (RFC-0016 M3): the infix style-composition operator,
    /// `base merge overrides` produces a new `Style` whose right operand wins.
    #[token("merge")]
    Merge,
    /// `use` (RFC-0008 D-F): a package-qualified symbol import at file top,
    /// `use material`, `use material as m`, `use material.{Card}`. The name
    /// is resolved by the module resolver against the *manifest-declared*
    /// dependency set; no path string ever appears in `byld` source (the
    /// two-layer rule, RFC-0001 §1).
    #[token("use")]
    Use,

    // ---- identifiers & literals ----
    /// An identifier (interned).
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*", |lex| Symbol::intern(lex.slice()))]
    Ident(Symbol),
    /// An angle literal with a unit suffix (RFC-0011 T1: `360deg`, `1.5rad`).
    /// Listed before [`Token::FloatLit`]/[`Token::IntLit`] so `logos`'s
    /// longest-match prefers the suffixed form over a bare number followed by
    /// an `Ident`, same principle as hex-before-decimal below. The value is
    /// canonicalized to **radians** right here at lex time: the deg→rad
    /// conversion is a pure, infallible numeric transform (nothing here can
    /// fail the way string/number parsing can), so there is no benefit to
    /// deferring it to a later compiler pass the way D9 defers *type*
    /// inference, this is unit normalization, not semantic analysis.
    #[regex(r"[0-9]+(\.[0-9]+)?deg", |lex| {
        let s = lex.slice();
        s[..s.len() - 3].parse::<f64>().ok().map(f64::to_radians)
    })]
    #[regex(r"[0-9]+(\.[0-9]+)?rad", |lex| {
        let s = lex.slice();
        s[..s.len() - 3].parse::<f64>().ok()
    })]
    AngleLit(f64),
    /// A duration literal, `200ms` (RFC-0010), the seconds form `1.5s` / `2s`
    /// (RFC-0025 §4), or the minutes form `5min` (RFC-0029 §5), value always in
    /// **milliseconds**.
    ///
    /// Listed before the plain number rules so `logos`'s longest-match prefers
    /// `200ms` over `200` + `ms`. The seconds form is canonicalized to ms right
    /// here, the same unit normalization [`Token::AngleLit`] does for `deg`. The
    /// parser lowers it to an `Expr::IntLit` node (see `parser/expr.rs`): a
    /// duration is only meaningful inside an `anim.*` curve call, read there as
    /// milliseconds.
    #[regex(r"[0-9]+ms", |lex| lex.slice()[..lex.slice().len() - 2].parse::<u32>().ok())]
    // Minutes, listed before the `s` rule so longest-match takes `5min` whole
    // rather than reading `5mi` + `n`. A refresh interval is written in the
    // unit a person would say it in (RFC-0029 §5): `every 5min`, not
    // `every 300000ms`, where a mistyped zero is invisible.
    #[regex(r"[0-9]+(\.[0-9]+)?min", |lex| {
        let s = lex.slice();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        s[..s.len() - 3]
            .parse::<f64>()
            .ok()
            .map(|mins| (mins * 60_000.0).round())
            .filter(|ms| *ms >= 0.0 && *ms <= f64::from(u32::MAX))
            .map(|ms| ms as u32)
    })]
    #[regex(r"[0-9]+(\.[0-9]+)?s", |lex| {
        let s = lex.slice();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        s[..s.len() - 1]
            .parse::<f64>()
            .ok()
            .map(|secs| (secs * 1000.0).round())
            // The regex admits no sign, so the range check is purely the upper
            // bound, but it is what makes the cast below lossless.
            .filter(|ms| *ms >= 0.0 && *ms <= f64::from(u32::MAX))
            .map(|ms| ms as u32)
    })]
    DurationLit(u32),
    /// A percentage literal (RFC-0025 §4: `anim.keyframes(0%: …, 50%: …)`),
    /// canonicalized at lex time to the **fraction** `0.5`, again the
    /// [`Token::AngleLit`] precedent (a pure, infallible unit transform). Listed
    /// before the plain number rules for longest-match.
    ///
    /// `%` is also the remainder operator (RFC-0047), so longest-match alone
    /// would read `10%3` as this literal and a stray `3`. [`lex`] settles it: a
    /// percent literal only stays one when a `:` follows, the only place the
    /// grammar takes one; anywhere else it is split back into the number and a
    /// [`Token::Percent`].
    #[regex(r"[0-9]+(\.[0-9]+)?%", |lex| {
        let s = lex.slice();
        s[..s.len() - 1].parse::<f64>().ok().map(|p| p / 100.0)
    })]
    PercentLit(f64),
    /// A floating-point literal. Listed before [`Token::IntLit`] because
    /// `logos` longest-match makes `3.14` a float, `3` an int.
    #[regex(r"[0-9]+(_[0-9]+)*\.[0-9]+(_[0-9]+)*", |lex| lex.slice().replace('_', "").parse::<f64>().ok())]
    // With an exponent: `1e3`, `1.5e-3`, `2E+4`. Always a `Float`.
    #[regex(r"[0-9]+(_[0-9]+)*(\.[0-9]+(_[0-9]+)*)?[eE][+-]?[0-9]+", |lex| lex.slice().replace('_', "").parse::<f64>().ok())]
    FloatLit(f64),
    /// An integer literal (`i64`; D9), decimal or hex (`0xRRGGBB` colors,
    /// RFC-0005 §1). Hex is listed first so `0x1E` is one hex int, not `0`
    /// followed by an identifier.
    ///
    /// A hex literal with **more than six digits** is, per the RFC-0005 §1
    /// colour contract, alpha-first `0xAARRGGBB`, and the written width is
    /// semantic: `0x00FFFFFF` (transparent white) and `0xFFFFFF` (opaque
    /// white) are the same `i64`, so the value alone cannot carry "the alpha
    /// byte was written". Such literals are tagged with
    /// [`COLOR_HAS_ALPHA_TAG`] (bit 32) here at lex time. Colour consumers
    /// read the tag (or fall back to the magnitude heuristic for computed
    /// values) and truncate to `u32` for the channels, so the tag never
    /// reaches a channel; a >6-digit hex literal used as a plain *number* is
    /// the one degenerate case this trades away (RFC-0005 reserves that
    /// width for colours).
    #[regex(r"0x[0-9a-fA-F]+", |lex| {
        let digits = &lex.slice()[2..];
        i64::from_str_radix(digits, 16)
            .ok()
            .map(|v| if digits.len() > 6 { v | COLOR_HAS_ALPHA_TAG } else { v })
    })]
    // `#RRGGBB`, `#RRGGBBAA` and `#RGB`: a colour as CSS writes it, the same
    // value as `0xRRGGBB`, and for eight digits the alpha written last, as
    // CSS has it, put first as the colour contract does (`0xAARRGGBB`).
    #[regex(r"#[0-9a-fA-F]{3}", |lex| hex_color(&lex.slice()[1..]), priority = 3)]
    #[regex(r"#[0-9a-fA-F]{6}", |lex| hex_color(&lex.slice()[1..]), priority = 3)]
    #[regex(r"#[0-9a-fA-F]{8}", |lex| hex_color(&lex.slice()[1..]), priority = 3)]
    // A `#` run of any other length or with a letter past `F` (`#12345`,
    // `#3B82F6A`) is one lexing error, and not a colour and something else.
    #[regex(r"#[0-9a-zA-Z_]+", |_| None::<i64>, priority = 1)]
    #[regex(r"0b[01]+", |lex| i64::from_str_radix(&lex.slice()[2..], 2).ok())]
    // Digits may be grouped with `_` between them: `1_000`.
    #[regex(r"[0-9]+(_[0-9]+)*", |lex| lex.slice().replace('_', "").parse::<i64>().ok())]
    // A digit run that runs into letters and makes no literal (`12abc`,
    // `1_000x`) is one lexing error, and not a number and a name.
    #[regex(r"[0-9][0-9a-zA-Z_]*", |_| None::<i64>, priority = 1)]
    IntLit(i64),
    /// A raw string literal (possibly interpolated). The slice (via its span)
    /// holds the source including quotes; interpolations are re-lexed by the
    /// parser.
    #[token("\"", lex_string)]
    StrLit,

    // ---- brackets & grouping ----
    /// `(`
    #[token("(")]
    LParen,
    /// `)`
    #[token(")")]
    RParen,
    /// `{`
    #[token("{")]
    LBrace,
    /// `}`
    #[token("}")]
    RBrace,
    /// `[` (array literal)
    #[token("[")]
    LBrack,
    /// `]` (closes an attribute block or array)
    #[token("]")]
    RBracket,
    /// `#[` (opens an attribute block), one token.
    #[token("#[")]
    HashBracket,

    // ---- punctuation & operators ----
    /// `,`
    #[token(",")]
    Comma,
    /// `:`
    #[token(":")]
    Colon,
    /// `.`
    #[token(".")]
    Dot,
    /// `..` (RFC-0016 spread: `#[..style]`). Longest-match keeps this distinct
    /// from a single `.` (member access / sub-property axis).
    #[token("..")]
    DotDot,
    /// `=>` (event / lambda arrow)
    #[token("=>")]
    Arrow,
    /// `->` (function return type)
    #[token("->")]
    ThinArrow,
    /// `=` (assignment)
    #[token("=")]
    Eq,
    /// `+=`
    #[token("+=")]
    PlusEq,
    /// `-=`
    #[token("-=")]
    MinusEq,
    /// `++`
    #[token("++")]
    PlusPlus,
    /// `--`
    #[token("--")]
    MinusMinus,
    /// `<` (opens generic type arguments)
    #[token("<")]
    Lt,
    /// `>` (closes generic type arguments)
    #[token(">")]
    Gt,
    /// `|` (lambda parameter delimiter)
    #[token("|")]
    Pipe,
    /// `?` (ternary)
    #[token("?")]
    Question,
    /// `??`, "or else": the left value, or the right one when the left has
    /// none. Longest-match keeps it distinct from the ternary's `?`.
    #[token("??")]
    QuestionQuestion,
    /// `-`, the sign of a negative numeric literal (`translate: (-8, 0)`)
    /// *or* binary subtraction (part of the minimal arithmetic surface, see
    /// [`Token::Star`]). Longest-match keeps `->`, `-=` and `--` as their own
    /// tokens.
    #[token("-")]
    Minus,
    /// `+`, binary addition. Part of the minimal arithmetic surface
    /// (`+ - * /`) required by RFC-0020's reactive shape parameters
    /// (`sweep: percent * 3.6`). Longest-match keeps `+=` and `++` as their
    /// own tokens.
    #[token("+")]
    Plus,
    /// `?.`, a field read that is nothing when what it reads from is.
    #[token("?.")]
    QuestionDot,
    /// `*`, binary multiplication (see [`Token::Plus`]).
    #[token("*")]
    Star,
    /// `/`, binary division (see [`Token::Plus`]). A `//` comment still wins
    /// over two `/` tokens through the skip rule's longest match.
    #[token("/")]
    // `/* … */`, which nests: skipped like a `//` comment.
    #[token("/*", block_comment)]
    Slash,
    /// `%`, binary remainder (RFC-0047), binding like [`Token::Star`] and
    /// [`Token::Slash`].
    #[token("%")]
    Percent,
    /// `==`, equality comparison (RFC-0027 §1). Longest-match keeps this
    /// distinct from a single `=` (assignment).
    #[token("==")]
    EqEq,
    /// `!=`, inequality comparison (RFC-0027 §1).
    #[token("!=")]
    BangEq,
    /// `!`, boolean negation (RFC-0027 §2). Longest-match keeps `!=` its own
    /// token.
    #[token("!")]
    Bang,
    /// `<=`, less-than-or-equal (RFC-0027 §1). Longest-match prefers this over
    /// `<` (generic open / less-than).
    #[token("<=")]
    LtEq,
    /// `>=`, greater-than-or-equal (RFC-0027 §1).
    #[token(">=")]
    GtEq,
    /// `&&`, short-circuiting logical AND (RFC-0027 §2).
    #[token("&&")]
    AmpAmp,
    /// `||`, short-circuiting logical OR (RFC-0027 §2). Longest-match keeps a
    /// single `|` (lambda delimiter) distinct.
    #[token("||")]
    PipePipe,
}

/// State of the two-state string-scanning stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StrState {
    /// Scanning the characters of a string literal.
    InString,
    /// Scanning an interpolation expression inside `{ ... }`.
    InBrace,
}

/// Hard cap on interpolated-string nesting (D12): the outer string is level 1,
/// so at most 3 `InString` frames may exist at once.
const MAX_STRING_DEPTH: usize = 3;

/// Locates the end of a string literal opened by `"`, accounting for escapes
/// and interpolation (`{ expr }`, where `expr` may itself contain strings).
///
/// On success bumps the lexer past the closing quote and returns `Ok(())`.
/// A literal nested deeper than [`MAX_STRING_DEPTH`] (D12) is still scanned to
/// its closing quote, and the whole of it is one
/// [`LexError::StringNestingTooDeep`]: stopping at the level that is too deep
/// would leave the levels inside it to be lexed as strings of their own, one
/// error each. [`LexError::UnterminatedString`] if input ends first.
/// Skips a block comment after its `/*`, to the `*/` that closes it. They nest,
/// so a block of code can be commented out whatever comments it holds.
/// [`LexError::UnterminatedComment`] if input ends first.
fn block_comment(lex: &mut Lexer<Token>) -> logos::FilterResult<(), LexError> {
    let rest = lex.remainder().as_bytes();
    let mut depth = 1usize;
    let mut i = 0usize;
    while i < rest.len() {
        if rest[i..].starts_with(b"*/") {
            depth -= 1;
            i += 2;
            if depth == 0 {
                lex.bump(i);
                return logos::FilterResult::Skip;
            }
        } else if rest[i..].starts_with(b"/*") {
            depth += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    lex.bump(rest.len());
    logos::FilterResult::Error(LexError::UnterminatedComment)
}

/// A string written over lines, `"""` to `"""`, after its first quote: it runs to
/// the next three quotes that are not escaped.
fn lex_triple_string(lex: &mut Lexer<Token>) -> Result<(), LexError> {
    let rest = lex.remainder().as_bytes();
    // The first quote is the token's own; `""` follows it.
    let mut i = 2;
    while i < rest.len() {
        if rest[i] == b'\\' {
            i += 2;
        } else if rest[i..].starts_with(b"\"\"\"") {
            lex.bump(i + 3);
            return Ok(());
        } else {
            i += 1;
        }
    }
    lex.bump(rest.len());
    Err(LexError::UnterminatedString)
}

fn lex_string(lex: &mut Lexer<Token>) -> Result<(), LexError> {
    if lex.remainder().starts_with("\"\"") {
        return lex_triple_string(lex);
    }
    // stack[0] is the opening quote's `InString` frame.
    let mut stack = vec![StrState::InString];
    let mut string_depth = 1usize; // number of `InString` frames currently open
    let mut too_deep = false;
    let mut consumed = 0usize;
    let mut escaped = false;

    for c in lex.remainder().chars() {
        consumed += c.len_utf8();
        // Escapes are honored in *both* states: a `\"` inside an interpolation
        // (which is itself inside this string) is an escaped quote, a literal,
        // it must not toggle string/brace state. D12's nesting cap still counts
        // genuinely nested *un-escaped* strings via the two-state stack below.
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        let Some(&state) = stack.last() else {
            break;
        };
        match state {
            StrState::InString => {
                if c == '"' {
                    stack.pop();
                    string_depth -= 1;
                    if stack.is_empty() {
                        lex.bump(consumed);
                        return if too_deep {
                            Err(LexError::StringNestingTooDeep)
                        } else {
                            Ok(())
                        };
                    }
                } else if c == '{' {
                    stack.push(StrState::InBrace);
                }
            }
            StrState::InBrace => match c {
                '{' => stack.push(StrState::InBrace),
                '}' => {
                    stack.pop();
                }
                '"' => {
                    string_depth += 1;
                    too_deep |= string_depth > MAX_STRING_DEPTH;
                    stack.push(StrState::InString);
                }
                _ => {}
            },
        }
    }
    Err(LexError::UnterminatedString)
}

/// A token paired with its source span.
pub type SpannedToken = (Token, Span);

/// The result of lexing a source file: the token stream plus any diagnostics.
///
/// Lexing never aborts on the first error, it records a [`CompileError`] and
/// continues, so one pass surfaces multiple problems.
#[derive(Debug, Default)]
pub struct LexedFile {
    /// Successfully lexed tokens, in source order.
    pub tokens: Vec<SpannedToken>,
    /// Diagnostics collected during lexing.
    pub errors: Vec<CompileError>,
    /// Where each lex error that left no token behind ended, as an offset in
    /// the file. The token after one is the parser's first surprise at what
    /// is left of a mistake it has already been told about, and the parser
    /// does not report that one again.
    pub gaps: Vec<u32>,
}

/// Lexes `source` into a [`LexedFile`]. Every `logos` failure becomes a
/// span-carrying [`CompileError`]; the driver never panics on malformed input.
#[must_use]
pub fn lex(source: &str) -> LexedFile {
    lex_remapped(source, |span| span)
}

/// Whether `text` is made of what a name is made of, in any alphabet.
fn is_word(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// [`lex`], with every token and diagnostic span passed through `remap`. The
/// parser uses it to re-lex a string interpolation's fragment so its spans
/// land on the fragment's position in the file, not at the fragment's start.
///
/// One mistake is one error. A lex error does not leave a hole for the parser
/// to trip over: a number too large to read is replaced by a `0` and a word
/// with a letter outside the English alphabet by one name, both with the error
/// on the whole of them; an unclosed string swallows the rest of the file, as
/// the language says it does; and any other bad token is skipped and its place
/// recorded in [`LexedFile::gaps`].
pub(crate) fn lex_remapped(source: &str, remap: impl Fn(Span) -> Span) -> LexedFile {
    let mut lexer = Token::lexer(source);
    let mut out = LexedFile::default();
    // The source range of each token, for splitting a percent literal below.
    let mut raw: Vec<std::ops::Range<usize>> = Vec::new();
    while let Some(result) = lexer.next() {
        let range = lexer.span();
        let span = remap(range.clone().into());
        match result {
            Ok(token) => {
                out.tokens.push((token, span));
                raw.push(range);
            }
            Err(LexError::StringNestingTooDeep) => {
                out.errors.push(CompileError::StringNestingTooDeep { span });
                out.gaps.push(span.end);
            }
            Err(LexError::UnterminatedString) => {
                out.errors.push(CompileError::UnterminatedString { span });
                out.gaps.push(span.end);
                // Everything after the quote is the string, so there is
                // nothing left to read.
                lexer.bump(lexer.remainder().len());
            }
            Err(LexError::UnterminatedComment) => {
                out.errors.push(CompileError::UnterminatedComment { span });
                out.gaps.push(span.end);
            }
            Err(LexError::Unexpected) => {
                let text = &source[range.clone()];
                if text.starts_with(|c: char| c.is_ascii_digit()) {
                    // A literal that does not fit what it is: a number too
                    // large to hold, a duration past its limit.
                    out.errors.push(CompileError::UnexpectedChar { span });
                    out.tokens.push((Token::IntLit(0), span));
                    raw.push(range);
                } else if text.starts_with(|c: char| c.is_alphanumeric()) {
                    // A letter outside the English alphabet: the error is the
                    // whole word it is in, which then stands for one name.
                    let mut start = range.start;
                    if let Some(before) = raw.last()
                        && before.end == start
                        && is_word(&source[before.clone()])
                    {
                        start = before.start;
                        out.tokens.pop();
                        raw.pop();
                    }
                    let end = range.end
                        + source[range.end..]
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .map(char::len_utf8)
                            .sum::<usize>();
                    lexer.bump(end - range.end);
                    let word = remap((start..end).into());
                    out.errors.push(CompileError::UnexpectedChar { span: word });
                    out.tokens
                        .push((Token::Ident(Symbol::intern(&source[start..end])), word));
                    raw.push(start..end);
                } else {
                    out.errors.push(CompileError::UnexpectedChar { span });
                    out.gaps.push(span.end);
                }
            }
        }
    }
    split_unkeyed_percents(&mut out.tokens, &raw, source, &remap);
    out
}

/// The number a percent literal was written from, and where its `%` is, when
/// the literal is to be read as a remainder instead (RFC-0047 §4). `None` for a
/// number too large to read back, which is then left as the literal it lexed as.
pub(crate) fn percent_halves(
    source: &str,
    range: &std::ops::Range<usize>,
) -> Option<(Token, std::ops::Range<usize>, std::ops::Range<usize>)> {
    let digits = &source[range.start..range.end - 1];
    let number = if digits.contains('.') {
        Token::FloatLit(digits.parse::<f64>().ok()?)
    } else {
        Token::IntLit(digits.parse::<i64>().ok()?)
    };
    Some((number, range.start..range.end - 1, range.end - 1..range.end))
}

/// Turns every percent literal not followed by `:` back into the number it was
/// written from and a [`Token::Percent`] (RFC-0047 §4), so `10%3` is a
/// remainder and `0%: 0, 50%: 1` stays a pair of keyframe steps.
fn split_unkeyed_percents(
    tokens: &mut Vec<SpannedToken>,
    raw: &[std::ops::Range<usize>],
    source: &str,
    remap: &impl Fn(Span) -> Span,
) {
    if !tokens
        .iter()
        .any(|(t, _)| matches!(t, Token::PercentLit(_)))
    {
        return;
    }
    let old = std::mem::take(tokens);
    tokens.reserve(old.len() + 2);
    for (i, (token, span)) in old.iter().enumerate() {
        let keyed = matches!(old.get(i + 1), Some((Token::Colon, _)));
        let halves = if matches!(token, Token::PercentLit(_)) && !keyed {
            percent_halves(source, &raw[i])
        } else {
            None
        };
        let Some((number, digits, percent)) = halves else {
            tokens.push((token.clone(), *span));
            continue;
        };
        let cut = |r: std::ops::Range<usize>| {
            remap(Span::new(
                u32::try_from(r.start).unwrap_or(u32::MAX),
                u32::try_from(r.end).unwrap_or(u32::MAX),
            ))
        };
        tokens.push((number, cut(digits)));
        tokens.push((Token::Percent, cut(percent)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects just the token kinds (dropping spans) for terse assertions.
    fn kinds(source: &str) -> Vec<Token> {
        let lexed = lex(source);
        assert!(
            lexed.errors.is_empty(),
            "unexpected lex errors: {:?}",
            lexed.errors
        );
        lexed.tokens.into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn keywords_lex_to_their_tokens() {
        assert_eq!(
            kinds("View var let fn inject as for in when else style untrack with merge use"),
            vec![
                Token::View,
                Token::Var,
                Token::Let,
                Token::Fn,
                Token::Inject,
                Token::As,
                Token::For,
                Token::In,
                Token::When,
                Token::Else,
                Token::Style,
                Token::Untrack,
                Token::With,
                Token::Merge,
                Token::Use,
            ]
        );
    }

    #[test]
    fn operators_and_brackets_lex() {
        assert_eq!(
            kinds("( ) { } [ ] #[ , : . => -> = += -= ++ -- < > | ?"),
            vec![
                Token::LParen,
                Token::RParen,
                Token::LBrace,
                Token::RBrace,
                Token::LBrack,
                Token::RBracket,
                Token::HashBracket,
                Token::Comma,
                Token::Colon,
                Token::Dot,
                Token::Arrow,
                Token::ThinArrow,
                Token::Eq,
                Token::PlusEq,
                Token::MinusEq,
                Token::PlusPlus,
                Token::MinusMinus,
                Token::Lt,
                Token::Gt,
                Token::Pipe,
                Token::Question,
            ]
        );
    }

    #[test]
    fn comparison_and_logic_operators_lex() {
        // Longest-match must keep `==`/`!=`/`<=`/`>=`/`&&`/`||` distinct from
        // their single-char prefixes (`=`, `!`, `<`, `>`, `|`).
        assert_eq!(
            kinds("== != < <= > >= && || ! |"),
            vec![
                Token::EqEq,
                Token::BangEq,
                Token::Lt,
                Token::LtEq,
                Token::Gt,
                Token::GtEq,
                Token::AmpAmp,
                Token::PipePipe,
                Token::Bang,
                Token::Pipe,
            ]
        );
    }

    #[test]
    fn hash_bracket_is_one_token() {
        // `#[gap: 12]`, `#[` must be a single HashBracket, not `#` + `[`.
        assert_eq!(
            kinds("#[gap: 12]"),
            vec![
                Token::HashBracket,
                Token::Ident(Symbol::intern("gap")),
                Token::Colon,
                Token::IntLit(12),
                Token::RBracket,
            ]
        );
    }

    #[test]
    fn int_and_float_literals() {
        assert_eq!(
            kinds("42 1.5"),
            vec![Token::IntLit(42), Token::FloatLit(1.5)]
        );
    }

    #[test]
    fn angle_literals_lex_as_one_token_and_canonicalize_to_radians() {
        let toks = kinds("360deg 180deg 1.5rad");
        let Token::AngleLit(full_turn) = &toks[0] else {
            panic!("expected AngleLit, got {:?}", toks[0]);
        };
        assert!((full_turn - std::f64::consts::TAU).abs() < 1e-9);

        let Token::AngleLit(half_turn) = &toks[1] else {
            panic!("expected AngleLit, got {:?}", toks[1]);
        };
        assert!((half_turn - std::f64::consts::PI).abs() < 1e-9);

        // `rad` is already radians, passed through unchanged.
        assert_eq!(toks[2], Token::AngleLit(1.5));
    }

    #[test]
    fn angle_suffix_wins_over_a_bare_number_then_identifier() {
        // Longest-match must prefer `360deg` as one AngleLit, never
        // `IntLit(360)` followed by `Ident("deg")`.
        assert_eq!(
            kinds("360deg"),
            vec![Token::AngleLit(std::f64::consts::TAU)]
        );
    }

    #[test]
    fn arrow_wins_over_eq() {
        // longest-match: `=>` is Arrow, a following `=` is Eq.
        assert_eq!(kinds("=> ="), vec![Token::Arrow, Token::Eq]);
    }

    #[test]
    fn interpolated_string_is_a_single_strlit_with_correct_span() {
        let lexed = lex("\"Clicks: {clicks}\"");
        assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
        assert_eq!(lexed.tokens.len(), 1);
        let (tok, span) = &lexed.tokens[0];
        assert_eq!(*tok, Token::StrLit);
        assert_eq!(*span, Span::new(0, 18));
    }

    #[test]
    fn nested_literal_finds_the_correct_end() {
        // The inner `"b"` must not terminate the outer string early.
        let src = "\"a {f(\"b\")}\" x";
        let lexed = lex(src);
        assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
        assert_eq!(lexed.tokens[0].0, Token::StrLit);
        let end = lexed.tokens[0].1.end as usize;
        assert_eq!(&src[..end], "\"a {f(\"b\")}\"");
        // The trailing `x` lexes as a separate identifier.
        assert_eq!(lexed.tokens[1].0, Token::Ident(Symbol::intern("x")));
    }

    #[test]
    fn three_levels_of_interpolation_lex_ok() {
        // "L1 { "L2 { "L3" }" }", exactly the D12 boundary, must pass.
        let src = "\"L1 { \"L2 { \"L3\" }\" }\"";
        let lexed = lex(src);
        assert!(
            lexed.errors.is_empty(),
            "3 levels must lex: {:?}",
            lexed.errors
        );
        assert_eq!(lexed.tokens.len(), 1);
        assert_eq!(lexed.tokens[0].0, Token::StrLit);
    }

    #[test]
    fn four_levels_of_interpolation_is_too_deep() {
        let src = "\"L1 { \"L2 { \"L3 { \"L4\" }\" }\" }\"";
        let lexed = lex(src);
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::StringNestingTooDeep { .. })),
            "4 levels must be rejected: {:?}",
            lexed.errors
        );
    }

    #[test]
    fn unterminated_string_is_reported_not_panicked() {
        let lexed = lex("\"open");
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::UnterminatedString { .. }))
        );
    }

    #[test]
    fn an_unclosed_string_swallows_the_rest_and_is_one_error() {
        let lexed = lex("a \"open { 1 + \"x\" b c d");
        assert_eq!(lexed.errors.len(), 1, "{:?}", lexed.errors);
        assert_eq!(
            lexed.tokens.len(),
            1,
            "only `a` is left: {:?}",
            lexed.tokens
        );
        assert_eq!(lexed.gaps.len(), 1);
    }

    #[test]
    fn a_string_nested_too_deep_is_one_error_however_deep() {
        let lexed = lex(r#"a "{"{"{"{"{"{"x"}"}"}"}"}"}" b"#);
        assert_eq!(lexed.errors.len(), 1, "{:?}", lexed.errors);
        assert!(matches!(
            lexed.errors[0],
            CompileError::StringNestingTooDeep { .. }
        ));
        // Lexing goes on after the whole string, at `b`.
        assert_eq!(lexed.tokens.len(), 2, "{:?}", lexed.tokens);
    }

    #[test]
    fn a_word_with_a_foreign_letter_is_one_error_and_one_name() {
        let lexed = lex("let año = 3");
        assert_eq!(lexed.errors.len(), 1, "{:?}", lexed.errors);
        let names: Vec<_> = lexed
            .tokens
            .iter()
            .filter_map(|(t, _)| match t {
                Token::Ident(name) => Some(name.as_str().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["año"]);
        assert!(lexed.gaps.is_empty(), "the name stands in for the word");
    }

    #[test]
    fn a_number_too_large_is_one_error_and_stands_as_a_zero() {
        let lexed = lex("1 + 9223372036854775808");
        assert_eq!(lexed.errors.len(), 1, "{:?}", lexed.errors);
        assert_eq!(lexed.tokens.len(), 3);
        assert_eq!(lexed.tokens[2].0, Token::IntLit(0));
    }

    #[test]
    fn unexpected_char_becomes_a_diagnostic_never_a_panic() {
        // `@`, `$`, `~`, `` ` `` begin no token.
        let lexed = lex("View @ A");
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::UnexpectedChar { .. })),
            "got {:?}",
            lexed.errors
        );
        // Lexing still recovers and yields the surrounding tokens.
        assert_eq!(lexed.tokens[0].0, Token::View);
    }

    #[test]
    fn fuzz_junk_never_panics() {
        for junk in [
            "@#$%", "\"\"\"\"", "{{{{", "}}}}", "....", ">>>><<<<", "\\\\", "#",
        ] {
            let _ = lex(junk); // must not panic; errors are fine
        }
    }

    #[test]
    fn a_percent_literal_is_only_one_before_a_colon() {
        // Keyframe steps keep their literal, however they are spaced.
        assert_eq!(
            kinds("0%: 1, 50% : 2"),
            vec![
                Token::PercentLit(0.0),
                Token::Colon,
                Token::IntLit(1),
                Token::Comma,
                Token::PercentLit(0.5),
                Token::Colon,
                Token::IntLit(2),
            ]
        );
        // Anywhere else `N%` is the number and the remainder operator.
        assert_eq!(
            kinds("10%3"),
            vec![Token::IntLit(10), Token::Percent, Token::IntLit(3)]
        );
        assert_eq!(
            kinds("1.5%2 x%3 (a)%b"),
            vec![
                Token::FloatLit(1.5),
                Token::Percent,
                Token::IntLit(2),
                Token::Ident(Symbol::intern("x")),
                Token::Percent,
                Token::IntLit(3),
                Token::LParen,
                Token::Ident(Symbol::intern("a")),
                Token::RParen,
                Token::Percent,
                Token::Ident(Symbol::intern("b")),
            ]
        );
    }

    #[test]
    fn the_halves_of_a_split_percent_keep_their_own_spans() {
        let lexed = lex("a = 10%3");
        let spans: Vec<(u32, u32)> = lexed.tokens.iter().map(|(_, s)| (s.start, s.end)).collect();
        assert_eq!(spans, [(0, 1), (2, 3), (4, 6), (6, 7), (7, 8)]);
    }

    #[test]
    fn a_colour_is_written_as_css_writes_it() {
        let ints = |src: &str| -> Vec<i64> {
            lex(src)
                .tokens
                .into_iter()
                .filter_map(|(t, _)| match t {
                    Token::IntLit(n) => Some(n),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(ints("#3B82F6"), [0x3B_82F6]);
        assert_eq!(ints("#F0a"), [0xFF_00AA]);
        assert_eq!(ints("#3B82F680"), [0x803B_82F6 | COLOR_HAS_ALPHA_TAG]);
        assert_eq!(ints("#3B82F680"), ints("0x803B82F6"));
        // `#[` is still the attribute block.
        assert!(
            lex("#[gap: 4]")
                .tokens
                .first()
                .is_some_and(|(t, _)| *t == Token::HashBracket)
        );
        assert_ne!(lex("#12345").errors.len(), 0);
    }
}
