//! The printer: one pass over the trivia stream (RFC-0044 §2).
//!
//! State is a stack of open brackets (each remembering the indentation of the
//! line it was opened on and whether it was opened at a line end), the
//! current column, and the class of the previous token. Every token's text is
//! copied from the source; only the whitespace around it is chosen here.
//!
//! The one place the pass looks ahead is an `#[ … ]` list: it is printed on
//! one line first, and if that overruns [`MAX_WIDTH`] the output is rolled
//! back to the `#[` and printed again, one attribute per line.

use super::MAX_WIDTH;
use crate::diagnostics::Span;
use crate::lexer::{Piece, Token, Trivia, lex_with_trivia};
use crate::parser::ast::StyleStateKind;

/// Spaces per indentation level.
const INDENT: usize = 4;

/// Formats `source`, which must lex without errors (the caller has parsed it).
pub(super) fn print(source: &str) -> String {
    let file = lex_with_trivia(source);
    let crlf = source
        .find('\n')
        .is_some_and(|i| i > 0 && source.as_bytes()[i - 1] == b'\r');
    let mut printer = Printer {
        src: source,
        pieces: &file.pieces,
        out: String::with_capacity(source.len() + source.len() / 8),
        eol: if crlf { "\r\n" } else { "\n" },
        frames: vec![Frame::ROOT],
        s: State::START,
    };
    let mut i = 0;
    while i < printer.pieces.len() {
        i = printer.step(i);
    }
    if !printer.out.is_empty() {
        printer.out.push_str(printer.eol);
    }
    printer.out
}

/// The syntactic class of a token, as far as spacing cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum K {
    /// An identifier (or `untrack`, which is called like one).
    Word,
    /// A keyword, which a space always follows.
    Keyword,
    /// A number or string literal.
    Lit,
    LParen,
    RParen,
    LBrack,
    RBrack,
    HashBrack,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    DotDot,
    /// `++` or `--`, written after its operand.
    Incr,
    Minus,
    Bang,
    Lt,
    Gt,
    Pipe,
    Question,
    /// Anything else: an operator with a space on both sides.
    Op,
}

/// What a token turned out to be in its context.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Plain,
    /// A prefix `-` or `!`: no space after.
    Unary,
    /// The `|` that opens a lambda's parameters.
    PipeOpen,
    /// The `|` that closes them.
    PipeClose,
    /// The `<` of `List<Str>`.
    GenOpen,
    /// The `>` of `List<Str>`.
    GenClose,
    /// The `?` of `Str?`, an optional record field.
    OptionalMark,
    /// The `:` of `cond ? a : b`.
    TernaryColon,
    /// A `.` of member access, written against its operand.
    Access,
    /// The `+` that joins interaction states: `on focused+hover { … }`.
    Joiner,
}

fn kind_of(token: &Token) -> K {
    match token {
        Token::Ident(_) | Token::Untrack => K::Word,
        Token::View
        | Token::Var
        | Token::Let
        | Token::Fn
        | Token::Inject
        | Token::As
        | Token::For
        | Token::In
        | Token::When
        | Token::Else
        | Token::Style
        | Token::With
        | Token::Merge
        | Token::Use => K::Keyword,
        Token::AngleLit(_)
        | Token::DurationLit(_)
        | Token::PercentLit(_)
        | Token::FloatLit(_)
        | Token::IntLit(_)
        | Token::StrLit => K::Lit,
        Token::LParen => K::LParen,
        Token::RParen => K::RParen,
        Token::LBrack => K::LBrack,
        Token::RBracket => K::RBrack,
        Token::HashBracket => K::HashBrack,
        Token::LBrace => K::LBrace,
        Token::RBrace => K::RBrace,
        Token::Comma => K::Comma,
        Token::Colon => K::Colon,
        Token::Dot => K::Dot,
        Token::DotDot => K::DotDot,
        Token::PlusPlus | Token::MinusMinus => K::Incr,
        Token::Minus => K::Minus,
        Token::Bang => K::Bang,
        Token::Lt => K::Lt,
        Token::Gt => K::Gt,
        Token::Pipe => K::Pipe,
        Token::Question => K::Question,
        Token::Arrow
        | Token::ThinArrow
        | Token::Eq
        | Token::PlusEq
        | Token::MinusEq
        | Token::QuestionQuestion
        | Token::Plus
        | Token::Star
        | Token::Slash
        | Token::EqEq
        | Token::BangEq
        | Token::LtEq
        | Token::GtEq
        | Token::AmpAmp
        | Token::PipePipe => K::Op,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Brace,
    Paren,
    Brack,
    Attr,
}

/// How an `#[ … ]` list is being printed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrMode {
    /// Not an attribute list.
    None,
    /// On one line: the line breaks the author wrote are dropped.
    Flat,
    /// One attribute per line with a trailing comma.
    Vertical,
}

/// An open bracket.
#[derive(Clone, Copy)]
struct Frame {
    kind: FrameKind,
    mode: AttrMode,
    /// Indentation level of the line the bracket was opened on: where its
    /// closer goes.
    base: u16,
    /// Level of the lines inside.
    inner: u16,
    /// Whether a line break follows the opener, so the closer gets its own
    /// line.
    eol_open: bool,
    /// `?` ternaries opened in this bracket whose `:` has not come yet.
    ternary: u8,
    /// `<` generic argument lists opened in this bracket and not yet closed.
    generic: u8,
}

impl Frame {
    /// The file itself: never popped, so there is always a top frame.
    const ROOT: Self = Self {
        kind: FrameKind::Brace,
        mode: AttrMode::None,
        base: 0,
        inner: 0,
        eol_open: false,
        ternary: 0,
        generic: 0,
    };
}

/// The previous token.
// Independent yes/no facts about one token, read together by the spacing rules;
// an enum per flag would be the same bits with more ceremony.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
struct Prev {
    k: K,
    role: Role,
    /// Source offset one past its end, to tell whether the author had a space
    /// after it.
    end: u32,
    /// The identifier `if`, which is followed by a space before a `(`.
    is_if: bool,
    /// An identifier starting with a capital: a type name.
    upper: bool,
    /// Whether the token ends an operand, so a following `-` subtracts.
    operand_end: bool,
    /// Whether it leaves its statement unfinished, so a line break after it
    /// continues the statement.
    open_ended: bool,
}

/// Everything about the pass that is a plain value, so a rollback is a copy.
// The flags are independent facts about the position in the file, not the
// states of one machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
struct State {
    col: usize,
    line_level: u16,
    prev: Option<Prev>,
    /// Line breaks seen (or decided) since the last thing printed, at most 2.
    pending_nl: u32,
    /// Whether the source had a line break since the last thing printed.
    nl_seen: bool,
    /// A comment ended the current line, so the next thing must start a new
    /// one.
    after_comment: bool,
    /// The last token was an opening bracket.
    after_open: bool,
    /// Nothing but indentation is on the current line yet.
    at_line_start: bool,
    /// Inside the `|…|` of a lambda's parameters.
    pipe_open: bool,
    /// The `|…|` being read is a route's `{|params|`, after which the body
    /// starts rather than an expression carrying on.
    route_pipe: bool,
    /// Between `on` and the `{` of an interaction-state block.
    on_selector: bool,
    /// Output length just after the last token.
    last_tok_end: usize,
}

impl State {
    const START: Self = Self {
        col: 0,
        line_level: 0,
        prev: None,
        pending_nl: 0,
        nl_seen: false,
        after_comment: false,
        after_open: false,
        at_line_start: true,
        pipe_open: false,
        route_pipe: false,
        on_selector: false,
        last_tok_end: 0,
    };
}

struct Printer<'a, 's> {
    src: &'s str,
    pieces: &'a [(Piece<'s>, Span)],
    out: String,
    eol: &'static str,
    frames: Vec<Frame>,
    s: State,
}

/// A point to roll back to.
struct Snapshot {
    out_len: usize,
    s: State,
    frames_len: usize,
    top: Frame,
}

impl<'s> Printer<'_, 's> {
    fn top(&self) -> &Frame {
        self.frames.last().expect("the root frame is never popped")
    }

    fn top_mut(&mut self) -> &mut Frame {
        self.frames
            .last_mut()
            .expect("the root frame is never popped")
    }

    fn text(&self, span: Span) -> &'s str {
        &self.src[span.start as usize..span.end as usize]
    }

    /// Prints the piece at `i`, returning the index of the next one to print.
    fn step(&mut self, i: usize) -> usize {
        let (piece, span) = &self.pieces[i];
        let span = *span;
        match piece {
            Piece::Trivia(Trivia::Newlines(n)) => {
                self.s.nl_seen = true;
                // Inside an attribute list the formatter owns the breaks
                // between attributes.
                if self.top().mode == AttrMode::None {
                    self.s.pending_nl = self.s.pending_nl.max((*n).min(2));
                }
                i + 1
            }
            Piece::Trivia(Trivia::LineComment(text)) => {
                self.comment(text, i);
                i + 1
            }
            Piece::Token(Token::HashBracket) => self.attr_list(i),
            Piece::Token(token) => {
                self.token(token, span, i, AttrMode::None);
                i + 1
            }
        }
    }

    // ---- comments -------------------------------------------------------

    fn comment(&mut self, text: &str, i: usize) {
        let own_line = self.s.nl_seen || self.out.is_empty();
        if own_line {
            let mut nl = self.s.pending_nl.clamp(1, 2);
            if self.s.after_open {
                nl = nl.min(1);
            }
            if self.out.is_empty() {
                nl = 0;
            }
            self.push_newlines(nl);
            // A comment sits where the token after it will: attached to it.
            // Before a closer it belongs to the lines inside instead.
            let level = match self.next_token(i) {
                Some((_, t)) if is_closer(kind_of(t)) => self.top().inner,
                Some((j, t)) => self.level_for_line(false, self.continues(t, j)),
                None => self.top().inner,
            };
            self.indent(level);
            self.s.pending_nl = 0;
        } else {
            self.out.push(' ');
            self.s.col += 1;
        }
        self.out.push_str(text);
        self.s.col += text.chars().count();
        self.s.nl_seen = false;
        self.s.after_comment = true;
    }

    // ---- tokens ---------------------------------------------------------

    fn token(&mut self, token: &Token, span: Span, i: usize, attr_mode: AttrMode) {
        let text = self.text(span);
        let k = kind_of(token);
        let had_space = self.s.prev.is_some_and(|p| span.start > p.end);
        let role = self.role(token, k, had_space);
        let closing = is_closer(k);

        if k == K::RBrack && self.top().kind == FrameKind::Attr {
            match self.top().mode {
                AttrMode::Vertical => self.ensure_trailing_comma(),
                AttrMode::Flat => self.drop_trailing_comma(),
                AttrMode::None => {}
            }
        }

        // How many line breaks come before this token.
        let mut nl = self.s.pending_nl.min(2);
        if self.s.after_comment {
            nl = nl.max(1);
        }
        if closing {
            nl = nl.min(1);
            if self.top().eol_open {
                nl = nl.max(1);
            }
        } else if self.s.after_open {
            nl = nl.min(1);
        }
        if self.out.is_empty() {
            nl = 0;
        }

        if nl > 0 {
            self.push_newlines(nl);
        }
        if self.s.at_line_start {
            let cont = self.continues(token, i) || self.s.prev.is_some_and(|p| p.open_ended);
            let level = self.level_for_line(closing, cont);
            self.indent(level);
        } else if self.wants_space(k, role, had_space) {
            self.out.push(' ');
            self.s.col += 1;
        }

        self.out.push_str(text);
        match text.rfind('\n') {
            Some(at) => self.s.col = text[at + 1..].chars().count(),
            None => self.s.col += text.chars().count(),
        }

        // ---- state after the token ----
        let prev = self.s.prev;
        self.s.pending_nl = 0;
        self.s.nl_seen = false;
        self.s.after_comment = false;
        self.s.after_open = matches!(k, K::LParen | K::LBrack | K::LBrace | K::HashBrack);
        self.s.last_tok_end = self.out.len();

        if k == K::Word && text == "on" {
            self.s.on_selector = matches!(
                self.next_token(i),
                Some((_, Token::Ident(next))) if StyleStateKind::from_name(next.as_str()).is_some()
            );
        } else if k == K::LBrace {
            self.s.on_selector = false;
        }

        match k {
            K::LParen => self.open(FrameKind::Paren, self.eol_follows(i)),
            K::LBrack => self.open(FrameKind::Brack, self.eol_follows(i)),
            K::LBrace => self.open(FrameKind::Brace, self.eol_follows(i)),
            K::HashBrack => {
                let vertical = attr_mode == AttrMode::Vertical;
                self.open(FrameKind::Attr, vertical);
                self.top_mut().mode = attr_mode;
            }
            K::RParen | K::RBrack | K::RBrace if self.frames.len() > 1 => {
                // What follows the closer belongs to the line the bracket was
                // opened on (`] {`, `) =>`), not to the one it is closed on.
                if let Some(closed) = self.frames.pop() {
                    self.s.line_level = closed.base;
                }
            }
            K::Comma if self.top().mode == AttrMode::Vertical => self.s.pending_nl = 1,
            _ => {}
        }

        let is_if = k == K::Word && text == "if";
        self.s.prev = Some(Prev {
            k,
            role,
            end: span.end,
            is_if,
            upper: k == K::Word && text.starts_with(|c: char| c.is_ascii_uppercase()),
            operand_end: matches!(k, K::Lit | K::RParen | K::RBrack | K::Incr)
                || (k == K::Word && !is_if),
            open_ended: matches!(
                token,
                Token::Eq
                    | Token::PlusEq
                    | Token::MinusEq
                    | Token::Arrow
                    | Token::ThinArrow
                    | Token::QuestionQuestion
                    | Token::Plus
                    | Token::Star
                    | Token::Slash
                    | Token::EqEq
                    | Token::BangEq
                    | Token::LtEq
                    | Token::GtEq
                    | Token::AmpAmp
                    | Token::PipePipe
                    | Token::With
                    | Token::Merge
            ) || (k == K::Colon)
                || (k == K::Question && role != Role::OptionalMark)
                || (k == K::Minus && role == Role::Plain)
                || (role == Role::PipeClose && !self.s.route_pipe),
        });
        let _ = prev;
    }

    fn open(&mut self, kind: FrameKind, eol_open: bool) {
        let base = self.s.line_level;
        self.frames.push(Frame {
            kind,
            mode: AttrMode::None,
            base,
            inner: base + 1,
            eol_open,
            ternary: 0,
            generic: 0,
        });
    }

    /// The token's role in its context, keeping the per-bracket counters.
    fn role(&mut self, token: &Token, k: K, had_space: bool) -> Role {
        let prev = self.s.prev;
        let operand_end = prev.is_some_and(|p| p.operand_end);
        match k {
            K::Minus => {
                if operand_end {
                    Role::Plain
                } else {
                    Role::Unary
                }
            }
            K::Bang => Role::Unary,
            K::Pipe => {
                if self.s.pipe_open {
                    self.s.pipe_open = false;
                    Role::PipeClose
                } else {
                    self.s.pipe_open = true;
                    self.s.route_pipe = prev.is_some_and(|p| p.k == K::LBrace);
                    Role::PipeOpen
                }
            }
            K::Lt => {
                if !had_space && prev.is_some_and(|p| p.k == K::Word && p.upper) {
                    let top = self.top_mut();
                    top.generic = top.generic.saturating_add(1);
                    Role::GenOpen
                } else {
                    Role::Plain
                }
            }
            K::Gt => {
                let top = self.top_mut();
                if top.generic > 0 {
                    top.generic -= 1;
                    Role::GenClose
                } else {
                    Role::Plain
                }
            }
            K::Question => {
                let optional = !had_space
                    && prev
                        .is_some_and(|p| (p.k == K::Word && p.upper) || p.role == Role::GenClose);
                if optional {
                    Role::OptionalMark
                } else {
                    let top = self.top_mut();
                    top.ternary = top.ternary.saturating_add(1);
                    Role::Plain
                }
            }
            K::Colon => {
                let top = self.top_mut();
                if top.ternary > 0 {
                    top.ternary -= 1;
                    Role::TernaryColon
                } else {
                    Role::Plain
                }
            }
            K::Dot => {
                if operand_end && !(prev.is_some_and(|p| p.k == K::RBrack) && had_space) {
                    Role::Access
                } else {
                    Role::Plain
                }
            }
            K::Op if self.s.on_selector && matches!(token, Token::Plus) => Role::Joiner,
            _ => Role::Plain,
        }
    }

    /// Whether a space goes between the previous token and this one, on a
    /// line they share.
    fn wants_space(&self, k: K, role: Role, had_space: bool) -> bool {
        let _ = had_space;
        let Some(p) = self.s.prev else {
            return false;
        };
        // What the previous token wants after it.
        if matches!(
            p.k,
            K::LParen | K::LBrack | K::HashBrack | K::Dot | K::DotDot | K::Bang
        ) || matches!(
            p.role,
            Role::Unary | Role::PipeOpen | Role::GenOpen | Role::Joiner
        ) {
            // `- -x` must not become `--x`.
            return p.role == Role::Unary && matches!(k, K::Minus | K::Incr);
        }
        // What this token wants before it.
        if role == Role::Joiner {
            return false;
        }
        match k {
            K::RParen | K::RBrack | K::Comma | K::Incr => false,
            K::Colon => role == Role::TernaryColon,
            K::Question => role != Role::OptionalMark,
            K::Dot => role != Role::Access,
            K::Pipe => match role {
                Role::PipeClose => false,
                _ => p.k != K::LBrace,
            },
            K::Lt => role != Role::GenOpen,
            K::Gt => role != Role::GenClose,
            K::LParen | K::LBrack => {
                !(matches!(p.k, K::RParen | K::RBrack) || (p.k == K::Word && !p.is_if))
            }
            K::RBrace => p.k != K::LBrace,
            _ => true,
        }
    }

    // ---- lines ----------------------------------------------------------

    fn push_newlines(&mut self, n: u32) {
        for _ in 0..n {
            self.out.push_str(self.eol);
        }
        self.s.col = 0;
        self.s.at_line_start = true;
    }

    fn indent(&mut self, level: u16) {
        for _ in 0..usize::from(level) * INDENT {
            self.out.push(' ');
        }
        self.s.col = usize::from(level) * INDENT;
        self.s.line_level = level;
        self.s.at_line_start = false;
    }

    /// The level a new line starts at.
    fn level_for_line(&self, closing: bool, continues: bool) -> u16 {
        let top = self.top();
        if closing {
            top.base
        } else {
            top.inner + u16::from(continues)
        }
    }

    /// Whether a line that starts with `token` carries on the statement
    /// before it, so it is indented one level further.
    fn continues(&self, token: &Token, i: usize) -> bool {
        match token {
            Token::HashBracket
            | Token::Arrow
            | Token::ThinArrow
            | Token::Eq
            | Token::PlusEq
            | Token::MinusEq
            | Token::Question
            | Token::Colon
            | Token::AmpAmp
            | Token::PipePipe
            | Token::QuestionQuestion
            | Token::Plus
            | Token::Star
            | Token::Slash
            | Token::EqEq
            | Token::BangEq
            | Token::LtEq
            | Token::GtEq
            | Token::With
            | Token::Merge
            | Token::As => true,
            // A result arm: `ok name => …`, `err name => …`.
            Token::Ident(sym) if matches!(sym.as_str(), "ok" | "err") => {
                let mut rest = self.tokens_after(i);
                matches!(rest.next(), Some(Token::Ident(_)))
                    && matches!(rest.next(), Some(Token::Arrow))
            }
            _ => false,
        }
    }

    /// The tokens after piece `i`, trivia skipped.
    fn tokens_after(&self, i: usize) -> impl Iterator<Item = &Token> {
        self.pieces[i + 1..].iter().filter_map(|(p, _)| match p {
            Piece::Token(t) => Some(t),
            Piece::Trivia(_) => None,
        })
    }

    /// The next token after piece `i`, with its index.
    fn next_token(&self, i: usize) -> Option<(usize, &Token)> {
        self.pieces
            .iter()
            .enumerate()
            .skip(i + 1)
            .find_map(|(j, (p, _))| match p {
                Piece::Token(t) => Some((j, t)),
                Piece::Trivia(_) => None,
            })
    }

    /// Whether a line break (after any comment) follows piece `i`.
    fn eol_follows(&self, i: usize) -> bool {
        for (piece, _) in &self.pieces[i + 1..] {
            match piece {
                Piece::Trivia(Trivia::LineComment(_)) => {}
                Piece::Trivia(Trivia::Newlines(_)) => return true,
                Piece::Token(_) => return false,
            }
        }
        false
    }

    // ---- attribute lists --------------------------------------------------

    /// Prints the `#[ … ]` list starting at piece `i`; returns the index after
    /// its `]`.
    fn attr_list(&mut self, i: usize) -> usize {
        let Some(scan) = self.scan_attr_list(i) else {
            // Unbalanced: cannot happen in a file that parsed.
            let span = self.pieces[i].1;
            self.token(&Token::HashBracket, span, i, AttrMode::Flat);
            return i + 1;
        };

        if !scan.has_comment && !scan.nested_break {
            let snap = self.snapshot();
            let fit = self.try_flat(i, scan.end);
            // A lone attribute gains nothing from a line to itself unless the
            // list is what overruns.
            if fit.line || (scan.attrs <= 1 && fit.list) {
                return scan.end + 1;
            }
            self.rollback(&snap);
            // When what comes before the `#[` already overruns the line, a
            // vertical list would not help: carry the list on the next line.
            if !self.s.at_line_start && self.s.col + 3 > MAX_WIDTH {
                self.s.pending_nl = self.s.pending_nl.max(1);
                let snap = self.snapshot();
                let fit = self.try_flat(i, scan.end);
                if fit.line || (scan.attrs <= 1 && fit.list) {
                    return scan.end + 1;
                }
                self.rollback(&snap);
            }
        }
        self.print_attr_list(i, scan.end, AttrMode::Vertical, scan.non_empty);
        scan.end + 1
    }

    /// Prints the list on one line and says whether the line it ends up on,
    /// up to the first line break the source has after it (`=> action`,
    /// `{`), fits. Leaves the state just after the list either way.
    fn try_flat(&mut self, i: usize, end: usize) -> Fit {
        self.print_attr_list(i, end, AttrMode::Flat, false);
        let list = self.s.col <= MAX_WIDTH;
        let after = self.snapshot();
        let mut j = end + 1;
        while let Some((piece, _)) = self.pieces.get(j) {
            let Piece::Token(t) = piece else { break };
            let k = kind_of(t);
            if is_closer(k) && self.frames.len() <= after.frames_len {
                break;
            }
            j = self.step(j);
            if k == K::LBrace {
                break;
            }
        }
        let line = self.s.col <= MAX_WIDTH;
        self.rollback(&after);
        Fit { list, line }
    }

    fn print_attr_list(&mut self, i: usize, end: usize, mode: AttrMode, break_after_open: bool) {
        let span = self.pieces[i].1;
        self.token(&Token::HashBracket, span, i, mode);
        if mode == AttrMode::Vertical && break_after_open {
            self.s.pending_nl = 1;
        }
        let mut j = i + 1;
        while j <= end {
            j = self.step(j);
        }
    }

    /// Finds the `]` that closes the `#[` at `i`, and whether the list holds
    /// a comment or a line break inside a nested bracket (which a one-line
    /// form could not keep).
    fn scan_attr_list(&self, i: usize) -> Option<AttrScan> {
        let mut depth = 1u32;
        let mut scan = AttrScan {
            end: 0,
            has_comment: false,
            nested_break: false,
            non_empty: false,
            attrs: 0,
        };
        let mut last_was_comma = false;
        for (j, (piece, _)) in self.pieces.iter().enumerate().skip(i + 1) {
            match piece {
                Piece::Trivia(Trivia::LineComment(_)) => scan.has_comment = true,
                Piece::Trivia(Trivia::Newlines(_)) => scan.nested_break |= depth > 1,
                Piece::Token(t) => {
                    let k = kind_of(t);
                    match k {
                        K::LParen | K::LBrack | K::LBrace | K::HashBrack => depth += 1,
                        K::RParen | K::RBrack | K::RBrace => {
                            depth -= 1;
                            if depth == 0 {
                                scan.end = j;
                                scan.attrs += usize::from(scan.non_empty && !last_was_comma);
                                return Some(scan);
                            }
                        }
                        K::Comma if depth == 1 => scan.attrs += 1,
                        _ => {}
                    }
                    last_was_comma = depth == 1 && k == K::Comma;
                }
            }
            if let Piece::Token(_) = piece {
                scan.non_empty = true;
            }
        }
        None
    }

    /// A vertical list ends every attribute with a comma.
    fn ensure_trailing_comma(&mut self) {
        let Some(p) = self.s.prev else {
            return;
        };
        if matches!(p.k, K::Comma | K::HashBrack) {
            return;
        }
        // Before a trailing comment, if the last attribute has one.
        self.out.insert(self.s.last_tok_end, ',');
        self.s.last_tok_end += 1;
        self.s.col += 1;
        self.s.prev = Some(Prev {
            k: K::Comma,
            role: Role::Plain,
            open_ended: false,
            operand_end: false,
            ..p
        });
    }

    /// A one-line list has no trailing comma.
    fn drop_trailing_comma(&mut self) {
        if self.s.prev.is_some_and(|p| p.k == K::Comma) && self.out.ends_with(',') {
            self.out.pop();
            self.s.col -= 1;
        }
    }

    // ---- rollback -----------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            out_len: self.out.len(),
            s: self.s,
            frames_len: self.frames.len(),
            top: *self.top(),
        }
    }

    fn rollback(&mut self, snap: &Snapshot) {
        self.out.truncate(snap.out_len);
        self.s = snap.s;
        self.frames.truncate(snap.frames_len);
        *self.top_mut() = snap.top;
    }
}

/// Whether a one-line list fits.
struct Fit {
    /// The list itself ends within the width.
    list: bool,
    /// So does the rest of the line after it, up to the source's next break.
    line: bool,
}

/// What [`Printer::scan_attr_list`] found.
struct AttrScan {
    /// Index of the closing `]`.
    end: usize,
    has_comment: bool,
    /// A line break inside a bracket nested in the list.
    nested_break: bool,
    /// Whether the list holds any token besides its `]`.
    non_empty: bool,
    /// How many attributes it holds.
    attrs: usize,
}

fn is_closer(k: K) -> bool {
    matches!(k, K::RParen | K::RBrack | K::RBrace)
}

#[allow(dead_code)]
const _: usize = MAX_WIDTH;
