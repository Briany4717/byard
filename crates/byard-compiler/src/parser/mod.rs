//! Parser: `Token` stream → `Vec<ViewDecl>` (RFC-0002 §"Parser" + §"Grammar";
//! RFC-0003 §"Attribute syntax").
//!
//! Declarations, elements, and control flow are ordinary recursive descent,
//! one function per production, following the EBNF. Expressions use the Pratt
//! parser in `expr`. Error recovery records a [`CompileError`], substitutes an
//! [`Expr::Error`] (or skips one token at member level), and continues, so a
//! single pass collects multiple diagnostics.

pub mod ast;
mod expr;
pub mod span_eq;

use ast::{
    Attr, AttrKind, ConstDecl, ElementNode, EnumDecl, Expr, FnDecl, Member, Param, RecordDecl,
    RecordField, RouteKind, StateBlock, StyleRule, Type, UseDecl, ViewDecl,
};

use crate::diagnostics::{CompileError, Span};
use crate::lexer::{SpannedToken, Token, lex, lex_remapped};
use crate::symbol::Symbol;

/// The result of parsing a whole `.byd` file.
#[derive(Debug, Default)]
pub struct ParsedFile {
    /// The file's `use` imports (RFC-0008 Pillar A), consumed by the module
    /// resolver. Empty in an already-resolved program.
    pub imports: Vec<UseDecl>,
    /// The parsed views (D11: a file may declare several).
    pub views: Vec<ViewDecl>,
    /// The file's top-level functions, also shared into every view's
    /// `helpers`.
    pub fns: Vec<FnDecl>,
    /// The file's record types, also shared into every view's `records`.
    pub records: Vec<RecordDecl>,
    /// The file's top-level constants, also shared into every view's
    /// `consts`.
    pub consts: Vec<ConstDecl>,
    /// The file's enums, also shared into every view's `enums`.
    pub enums: Vec<EnumDecl>,
    /// All diagnostics from lexing and parsing, in source order.
    pub errors: Vec<CompileError>,
}

/// Parses `source` into a [`ParsedFile`].
#[must_use]
pub fn parse(source: &str) -> ParsedFile {
    let mut parser = Parser::new(source);
    let Items {
        imports,
        mut views,
        fns,
        records,
        consts,
        enums,
    } = parser.parse_file();
    // A lone file's views see its own functions, types, constants and enums;
    // a project's are replaced by the program-wide tables when it is resolved.
    let helpers = std::sync::Arc::new(fns.clone());
    let shared_records = std::sync::Arc::new(records.clone());
    let shared_consts = std::sync::Arc::new(consts.clone());
    let shared_enums = std::sync::Arc::new(enums.clone());
    for view in &mut views {
        view.helpers = std::sync::Arc::clone(&helpers);
        view.records = std::sync::Arc::clone(&shared_records);
        view.consts = std::sync::Arc::clone(&shared_consts);
        view.enums = std::sync::Arc::clone(&shared_enums);
    }
    ParsedFile {
        imports,
        views,
        fns,
        records,
        consts,
        enums,
        errors: parser.errors,
    }
}

/// What a file declares at its top level.
struct Items {
    imports: Vec<UseDecl>,
    views: Vec<ViewDecl>,
    fns: Vec<FnDecl>,
    records: Vec<RecordDecl>,
    consts: Vec<ConstDecl>,
    enums: Vec<EnumDecl>,
}

/// A recursive-descent + Pratt parser over a lexed token stream.
pub(crate) struct Parser<'a> {
    source: &'a str,
    tokens: Vec<SpannedToken>,
    pos: usize,
    errors: Vec<CompileError>,
    /// For a sub-parser over a string interpolation's fragment: the file byte
    /// offset of each fragment byte, plus one past the end. The fragment is
    /// not a verbatim slice of the file (nested quotes arrive escaped as `\"`),
    /// so a single base offset is not enough. `None` for a whole file, whose
    /// offsets are already the file's.
    offsets: Option<Vec<u32>>,
}

impl<'a> Parser<'a> {
    /// Lexes `source` and prepares a parser positioned at the first token. Any
    /// lex diagnostics are carried forward into [`Parser::errors`].
    fn new(source: &'a str) -> Self {
        let lexed = lex(source);
        Self {
            source,
            tokens: lexed.tokens,
            pos: 0,
            errors: lexed.errors,
            offsets: None,
        }
    }

    /// A parser over an interpolation fragment whose byte `i` sits at file
    /// offset `offsets[i]` (`offsets` has one extra entry, for the end). Every
    /// span it produces, token, node and diagnostic alike, is a file span.
    fn new_fragment(source: &'a str, offsets: Vec<u32>) -> Self {
        debug_assert_eq!(offsets.len(), source.len() + 1);
        let lexed = lex_remapped(source, |span| {
            Span::new(offsets[span.start as usize], offsets[span.end as usize])
        });
        Self {
            source,
            tokens: lexed.tokens,
            pos: 0,
            errors: lexed.errors,
            offsets: Some(offsets),
        }
    }

    /// The file offset of byte `local` of this parser's source.
    fn file_offset(&self, local: usize) -> u32 {
        self.offsets
            .as_ref()
            .map_or(local as u32, |offsets| offsets[local])
    }

    /// The byte of this parser's source at file offset `file` (the inverse of
    /// [`Parser::file_offset`], for offsets that start or end a token).
    fn local_offset(&self, file: u32) -> usize {
        self.offsets.as_ref().map_or(file as usize, |offsets| {
            offsets.binary_search(&file).unwrap_or_else(|i| i)
        })
    }

    /// The source text a (file) `span` covers.
    fn text(&self, span: Span) -> &'a str {
        &self.source[self.local_offset(span.start)..self.local_offset(span.end)]
    }

    // ---- token cursor ----

    fn cur(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }

    /// Whether `every` or `after` under the cursor starts a timer: a duration
    /// literal follows, or an expression that ends at a `=>` on the way (so
    /// `after wait * 1ms => …`, and still not a variable named `after`).
    fn at_timer_duration(&self) -> bool {
        match self.peek2() {
            Some(Token::DurationLit(_)) => true,
            Some(Token::Ident(_) | Token::IntLit(_) | Token::FloatLit(_) | Token::LParen) => {
                let mut depth = 0_usize;
                for (token, _) in self.tokens.iter().skip(self.pos + 2).take(24) {
                    match token {
                        Token::LParen | Token::LBrack => depth += 1,
                        Token::RParen | Token::RBracket => depth = depth.saturating_sub(1),
                        Token::Arrow if depth == 0 => return true,
                        Token::LBrace | Token::RBrace | Token::Comma => return false,
                        _ => {}
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn peek2(&self) -> Option<Token> {
        self.tokens.get(self.pos + 1).map(|(t, _)| t.clone())
    }

    /// Whether the parenthesis just opened (the cursor is on the first token
    /// inside it) holds a lambda's parameters: whether its matching `)` is
    /// followed by `=>`. Looked ahead rather than parsed as a tuple and
    /// reinterpreted, because a typed parameter (`xs: List<Int>`) is not an
    /// expression.
    fn paren_is_lambda(&self) -> bool {
        let mut depth = 1_usize;
        for (at, (token, _)) in self.tokens.iter().enumerate().skip(self.pos) {
            match token {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.tokens.get(at + 1), Some((Token::Arrow, _)));
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn peek3(&self) -> Option<Token> {
        self.tokens.get(self.pos + 2).map(|(t, _)| t.clone())
    }

    fn cur_span(&self) -> Span {
        self.tokens.get(self.pos).map_or_else(
            || {
                let n = self.file_offset(self.source.len());
                Span::new(n, n)
            },
            |(_, s)| *s,
        )
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    /// Consumes the current token iff it equals `want` (for unit tokens).
    fn eat(&mut self, want: &Token) -> bool {
        if self.cur() == Some(want) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consumes `want`, or records an "expected `what`" diagnostic.
    fn expect(&mut self, want: &Token, what: &str) -> bool {
        if self.eat(want) {
            true
        } else {
            self.error(what);
            false
        }
    }

    /// Consumes and returns the current identifier, or records a diagnostic.
    fn expect_ident(&mut self, what: &str) -> Option<Symbol> {
        if let Some(Token::Ident(sym)) = self.cur() {
            let sym = sym.clone();
            self.advance();
            Some(sym)
        } else {
            self.error(what);
            None
        }
    }

    /// Consumes the name a declaration introduces (`what` is how the error
    /// names the thing: "a function"). A keyword there is one error, at the
    /// keyword, and is taken as the name so that the rest of the declaration
    /// parses as it was meant, instead of every token after it going wrong.
    fn expect_decl_name(&mut self, what: &str, expected: &str) -> Option<Symbol> {
        match self.cur() {
            Some(tok) if is_name_keyword(tok) => {
                let span = self.cur_span();
                let keyword = self.text(span).to_string();
                self.errors.push(CompileError::KeywordAsName {
                    span,
                    keyword: keyword.clone(),
                    what: what.to_string(),
                });
                self.advance();
                Some(Symbol::intern(&keyword))
            }
            _ => self.expect_ident(expected),
        }
    }

    /// Consumes a *name*: an identifier or a contextual keyword used as one
    /// (e.g. `style` in `#[style: .title]`). Keywords are recovered from their
    /// source slice and interned.
    fn expect_name(&mut self, what: &str) -> Option<Symbol> {
        match self.cur() {
            Some(Token::Ident(sym)) => {
                let sym = sym.clone();
                self.advance();
                Some(sym)
            }
            Some(tok) if is_keyword(tok) => {
                let span = self.cur_span();
                let sym = Symbol::intern(self.text(span));
                self.advance();
                Some(sym)
            }
            _ => {
                self.error(what);
                None
            }
        }
    }

    /// The span from `start.start` to the end of the most recently consumed
    /// token.
    fn span_from(&self, start: Span) -> Span {
        let end = if self.pos > 0 {
            self.tokens
                .get(self.pos - 1)
                .map_or(start.end, |(_, s)| s.end)
        } else {
            start.end
        };
        Span::new(start.start, end.max(start.start))
    }

    fn error(&mut self, expected: &str) {
        let span = self.cur_span();
        self.error_at(span, expected);
    }

    fn error_at(&mut self, span: Span, expected: &str) {
        self.errors.push(CompileError::UnexpectedToken {
            span,
            expected: expected.to_string(),
        });
    }

    // ---- file / view ----

    fn parse_file(&mut self) -> Items {
        let mut items = Items {
            imports: Vec::new(),
            views: Vec::new(),
            fns: Vec::new(),
            records: Vec::new(),
            consts: Vec::new(),
            enums: Vec::new(),
        };
        while self.cur().is_some() {
            let before = self.pos;
            // `private` before an item (RFC-0055). Contextual: only when what
            // follows starts one, so it is an ordinary name anywhere else.
            let private = self.at_private_item();
            if private {
                self.advance();
            }
            match self.cur() {
                Some(Token::View) => {
                    let mut view = self.parse_view();
                    view.private = private;
                    items.views.push(view);
                }
                // `type Name = { … }`. Contextual: `type` is an ordinary name
                // anywhere else.
                Some(Token::Ident(word))
                    if word.as_str() == "type"
                        && self.peek2().is_some_and(|t| {
                            matches!(t, Token::Ident(_)) || is_name_keyword(&t)
                        })
                        && matches!(self.peek3(), Some(Token::Eq)) =>
                {
                    let mut record = self.parse_record_decl();
                    record.private = private;
                    items.records.push(record);
                }
                // `enum Name { a, b }`, contextual as `type` is.
                Some(Token::Ident(word))
                    if word.as_str() == "enum"
                        && self.peek2().is_some_and(|t| {
                            matches!(t, Token::Ident(_)) || is_name_keyword(&t)
                        })
                        && matches!(self.peek3(), Some(Token::LBrace)) =>
                {
                    let mut decl = self.parse_enum_decl();
                    decl.private = private;
                    items.enums.push(decl);
                }
                // `let name = expr`: a constant. A view's `let` is a member and
                // never reaches here.
                Some(Token::Let) => {
                    if let Member::Let {
                        name,
                        ty,
                        init,
                        span,
                    } = self.parse_binding(false)
                    {
                        items.consts.push(ConstDecl {
                            name,
                            ty,
                            init,
                            private,
                            span,
                        });
                    }
                }
                Some(Token::Fn) => {
                    if let Member::Fn {
                        name,
                        params,
                        ret,
                        body,
                        span,
                    } = self.parse_fn()
                    {
                        items.fns.push(FnDecl {
                            name,
                            params,
                            ret,
                            body,
                            private,
                            span,
                        });
                    }
                }
                Some(Token::Use) => {
                    let import = self.parse_use();
                    // Imports are legal only at file top, before any `View`
                    // (RFC-0008 Pillar A). Record the decl anyway so the
                    // resolver can still do useful work during recovery.
                    if !items.views.is_empty() {
                        self.errors
                            .push(CompileError::ImportAfterView { span: import.span });
                    }
                    items.imports.push(import);
                }
                _ => {
                    self.error("'View', 'use', 'fn', 'type', 'enum' or 'let'");
                    self.skip_to_next_item();
                }
            }
            if self.pos == before {
                self.advance(); // guaranteed progress on unrecognized input
            }
        }
        items
    }

    /// Whether the cursor is on a `private` that modifies the item after it.
    fn at_private_item(&self) -> bool {
        let Some(Token::Ident(word)) = self.cur() else {
            return false;
        };
        word.as_str() == "private"
            && match self.peek2() {
                Some(Token::View | Token::Fn | Token::Let) => true,
                Some(Token::Ident(next)) => matches!(next.as_str(), "type" | "enum"),
                _ => false,
            }
    }

    /// After a stray token between top-level items: skips to the next item
    /// (a `View`, `fn`, `use` or `type` outside any braces), so one mistake is
    /// one error, not one per token that follows it.
    fn skip_to_next_item(&mut self) {
        let mut depth = 0usize;
        // The stray token itself is never the start of an item.
        self.advance();
        while let Some(tok) = self.cur() {
            match tok {
                Token::LBrace => depth += 1,
                Token::RBrace => depth = depth.saturating_sub(1),
                Token::View | Token::Fn | Token::Use | Token::Let if depth == 0 => return,
                Token::Ident(word)
                    if depth == 0
                        && matches!(word.as_str(), "type" | "enum")
                        && matches!(self.peek2(), Some(Token::Ident(_)))
                        && matches!(self.peek3(), Some(Token::Eq | Token::LBrace)) =>
                {
                    return;
                }
                _ => {}
            }
            self.advance();
        }
    }

    /// `record_decl := "type" IDENT "=" "{" (IDENT ":" type "?"? ","?)* "}"`.
    fn parse_record_decl(&mut self) -> RecordDecl {
        let start = self.cur_span();
        self.advance(); // type
        let name = self
            .expect_decl_name("a type", "a type name")
            .unwrap_or_else(|| Symbol::intern(""));
        self.expect(&Token::Eq, "'='");
        self.expect(&Token::LBrace, "'{'");
        let mut fields = Vec::new();
        while !matches!(self.cur(), Some(Token::RBrace) | None) {
            let Some(field) = self.expect_decl_name("a field", "a field name") else {
                break;
            };
            self.expect(&Token::Colon, "':'");
            let ty = self.parse_type();
            let optional = self.eat(&Token::Question);
            fields.push(RecordField {
                name: field,
                ty,
                optional,
            });
            self.eat(&Token::Comma);
        }
        self.expect(&Token::RBrace, "'}'");
        RecordDecl {
            name,
            fields,
            private: false,
            span: self.span_from(start),
        }
    }

    /// `enum_decl := "enum" IDENT "{" (IDENT ","?)* "}"` (RFC-0055).
    fn parse_enum_decl(&mut self) -> EnumDecl {
        let start = self.cur_span();
        self.advance(); // enum
        let name = self
            .expect_decl_name("an enum", "an enum name")
            .unwrap_or_else(|| Symbol::intern(""));
        self.expect(&Token::LBrace, "'{'");
        let mut cases = Vec::new();
        while !matches!(self.cur(), Some(Token::RBrace) | None) {
            let at = self.cur_span();
            let Some(case) = self.expect_decl_name("a case", "a case name") else {
                break;
            };
            cases.push((case, self.span_from(at)));
            self.eat(&Token::Comma);
        }
        self.expect(&Token::RBrace, "'}'");
        EnumDecl {
            name,
            cases,
            private: false,
            span: self.span_from(start),
        }
    }

    /// `use_decl := "use" IDENT ("as" IDENT | "." "{" IDENT ("," IDENT)* "}")?`
    /// (RFC-0008 D-F/D-G). The alias and selective forms are exclusive.
    fn parse_use(&mut self) -> UseDecl {
        let start = self.cur_span();
        self.advance(); // use
        let package = self
            .expect_ident("a package name")
            .unwrap_or_else(|| Symbol::intern(""));

        let mut alias = None;
        let mut symbols = None;
        if self.eat(&Token::As) {
            alias = self.expect_ident("an alias name");
        } else if self.eat(&Token::Dot) {
            self.expect(&Token::LBrace, "'{' to open a symbol list");
            let mut list = Vec::new();
            while !matches!(self.cur(), Some(Token::RBrace) | None) {
                let sym_span = self.cur_span();
                match self.expect_ident("an imported view name") {
                    Some(sym) => list.push((sym, self.span_from(sym_span))),
                    None => break,
                }
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::RBrace, "'}'");
            symbols = Some(list);
        }

        UseDecl {
            package,
            alias,
            symbols,
            span: self.span_from(start),
        }
    }

    fn parse_view(&mut self) -> ViewDecl {
        let start = self.cur_span();
        self.advance(); // View
        let name = self
            .expect_decl_name("a view", "a view name")
            .unwrap_or_else(|| Symbol::intern(""));
        self.expect(&Token::LParen, "'('");
        let params = self.parse_params();
        self.expect(&Token::RParen, "')'");
        self.expect(&Token::LBrace, "'{'");
        let body = self.parse_block_members();
        ViewDecl {
            records: std::sync::Arc::default(),
            consts: std::sync::Arc::default(),
            enums: std::sync::Arc::default(),
            private: false,
            name,
            params,
            body,
            span: self.span_from(start),
            helpers: std::sync::Arc::default(),
        }
    }

    /// Parameters after a consumed `(`, up to (not including) `)`.
    fn parse_params(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        while !matches!(self.cur(), Some(Token::RParen) | None) {
            let start = self.cur_span();
            // `bind name: Type` declares a binding (RFC-0051). `bind` alone, or
            // before `:`, `,`, `=` or `)`, is still an ordinary name.
            let bind = matches!(self.cur(), Some(Token::Ident(w)) if w.as_str() == "bind")
                && !matches!(
                    self.peek2(),
                    Some(Token::Colon | Token::Comma | Token::RParen | Token::Eq) | None
                );
            if bind {
                self.advance();
            }
            let name = self
                .expect_decl_name("a parameter", "a parameter name")
                .unwrap_or_else(|| Symbol::intern(""));
            let ty = if self.eat(&Token::Colon) {
                Some(self.parse_type())
            } else {
                None
            };
            // Optional default value (`= expr`), RFC-0007 D-B.
            let default = if self.eat(&Token::Eq) {
                Some(self.parse_expr(0))
            } else {
                None
            };
            params.push(Param {
                name,
                ty,
                default,
                bind,
                span: self.span_from(start),
            });
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        params
    }

    // ---- members ----

    /// Members up to and including the closing `}` (the opening `{` must have
    /// been consumed by the caller).
    fn parse_block_members(&mut self) -> Vec<Member> {
        let mut members = Vec::new();
        while !matches!(self.cur(), Some(Token::RBrace) | None) {
            let before = self.pos;
            if let Some(m) = self.parse_member() {
                members.push(m);
            }
            if self.pos == before {
                self.advance(); // recovery: skip the offending token
            }
        }
        self.expect(&Token::RBrace, "'}'");
        members
    }

    /// The members of an element's `{ … }`: [`parse_block_members`] plus the
    /// `on <state> { … }` blocks (RFC-0059), which belong to the element and are
    /// collected into `states` rather than left among its children.
    fn parse_element_members(&mut self, states: &mut Vec<StateBlock>) -> Vec<Member> {
        let mut members = Vec::new();
        while !matches!(self.cur(), Some(Token::RBrace) | None) {
            let before = self.pos;
            if self.at_state_block_member() {
                if let Some(block) = self.parse_state_block() {
                    states.push(block);
                }
            } else if let Some(m) = self.parse_member() {
                members.push(m);
            }
            if self.pos == before {
                self.advance(); // recovery: skip the offending token
            }
        }
        self.expect(&Token::RBrace, "'}'");
        members
    }

    /// Whether the cursor is on an `on <state> { … }` block, as opposed to the
    /// other forms that start with `on` (`on mount =>`, `on unmount =>`,
    /// `on measure =>`, `on change(…) =>`, and an element that is called `on`).
    /// It is one when the word after `on` is a state or an axis (`width`,
    /// `height`), or when what follows it is the `{` or `+` of a block, so a
    /// misspelt state is still reported as one.
    fn at_state_block_member(&self) -> bool {
        use ast::StyleStateKind;
        let Some(Token::Ident(word)) = self.peek2() else {
            return false;
        };
        if !self.at_state_block() {
            return false;
        }
        let word = word.as_str();
        if matches!(word, "mount" | "unmount" | "measure" | "change") {
            return false;
        }
        // `on field.focused { … }` (RFC-0062): a name, a dot, a state.
        let remote = matches!(
            self.tokens.get(self.pos + 2).map(|(t, _)| t),
            Some(Token::Dot)
        ) && matches!(
            self.tokens.get(self.pos + 3).map(|(t, _)| t),
            Some(Token::Ident(_))
        ) && matches!(
            self.tokens.get(self.pos + 4).map(|(t, _)| t),
            Some(Token::LBrace | Token::Plus)
        );
        remote
            || StyleStateKind::from_name(word).is_some()
            || matches!(word, "width" | "height")
            || matches!(
                self.tokens.get(self.pos + 2).map(|(t, _)| t),
                Some(Token::LBrace | Token::Plus)
            )
    }

    fn parse_member(&mut self) -> Option<Member> {
        match self.cur() {
            Some(Token::Var) => Some(self.parse_binding(true)),
            Some(Token::Let) => Some(self.parse_binding(false)),
            Some(Token::Fn) => Some(self.parse_fn()),
            Some(Token::Inject) => Some(self.parse_inject()),
            Some(Token::For) => Some(self.parse_for()),
            Some(Token::When) => Some(self.parse_when()),
            Some(Token::Style) => Some(self.parse_style_block()),
            // RFC-0026 `route "/x" { … }` / `tab "home" { … }`. Contextual, not
            // reserved: `route`/`tab` stay ordinary identifiers everywhere else,
            // and the following string literal is what distinguishes a nav case
            // from an element (no element form ever puts a literal after its
            // name). Placement, `route` under a `NavStack`, `tab` under a
            // `NavHost`, is a *checker* rule, so a misplaced case still parses
            // and gets a precise diagnostic instead of a parse cascade.
            // RFC-0028 §4b `on mount => …` / `on unmount => …`. Contextual like
            // the `on <state> { … }` style block it shares a keyword with: that
            // one is followed by `{`, this one by `=>`, and `mount`/`unmount`
            // are not style states, so the two can never be confused.
            Some(Token::Ident(name))
                if name.as_str() == "on"
                    && matches!(self.peek2(), Some(Token::Ident(ref s))
                        if s.as_str() == "mount" || s.as_str() == "unmount") =>
            {
                Some(self.parse_lifecycle())
            }
            // RFC-0038 `on measure => …`. Contextual on the same rule as the
            // two above: `on` is only special when the word after it is one
            // this grammar knows and an `=>` follows, so `measure` stays an
            // ordinary identifier everywhere an app wants it.
            Some(Token::Ident(name))
                if name.as_str() == "on"
                    && matches!(self.peek2(), Some(Token::Ident(ref s))
                        if s.as_str() == "measure") =>
            {
                Some(self.parse_measure())
            }
            // `on change(a, b) => …`. Contextual like the three above, and the
            // parenthesis is part of it: `on change => …` is not this form.
            Some(Token::Ident(name))
                if name.as_str() == "on"
                    && matches!(self.peek2(), Some(Token::Ident(ref s))
                        if s.as_str() == "change") =>
            {
                Some(self.parse_change())
            }
            // RFC-0029 §5 `every <dur> => …` / `after <dur> => …`. Contextual,
            // like `on mount`: the keyword is only special when a duration
            // literal follows it, so `every` and `after` stay ordinary
            // identifiers anywhere an app wants them.
            Some(Token::Ident(name))
                if (name.as_str() == "every" || name.as_str() == "after")
                    && self.at_timer_duration() =>
            {
                Some(self.parse_timer())
            }
            Some(Token::Ident(name)) if matches!(self.peek2(), Some(Token::StrLit)) => {
                match name.as_str() {
                    "route" => Some(self.parse_route(RouteKind::Route)),
                    "tab" => Some(self.parse_route(RouteKind::Tab)),
                    _ => Some(Member::Element(self.parse_element())),
                }
            }
            // RFC-0059: a state block styles the element it is written in. Here
            // there is none, which is said once and the block is skipped whole.
            Some(Token::Ident(_)) if self.at_state_block_member() => {
                let start = self.cur_span();
                let _ = self.parse_state_block();
                self.error_at(
                    self.span_from(start),
                    "a declaration or element (an `on` state block goes inside the element \
                     it styles)",
                );
                None
            }
            Some(Token::Ident(_)) => Some(Member::Element(self.parse_element())),
            _ => {
                self.error("a declaration or element");
                None
            }
        }
    }

    /// `var`/`let` share a shape: `kw IDENT (":" type)? "=" expr`.
    fn parse_binding(&mut self, is_var: bool) -> Member {
        let start = self.cur_span();
        self.advance(); // var | let
        let name = self
            .expect_decl_name(if is_var { "a `var`" } else { "a `let`" }, "a binding name")
            .unwrap_or_else(|| Symbol::intern(""));
        let ty = if self.eat(&Token::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        self.expect(&Token::Eq, "'='");
        let init = self.parse_expr(0);
        let span = self.span_from(start);
        if is_var {
            Member::Var {
                name,
                ty,
                init,
                span,
            }
        } else {
            Member::Let {
                name,
                ty,
                init,
                span,
            }
        }
    }

    /// `fn_decl := "fn" IDENT "(" param_list? ")" ("->" type)? "=>" expr`.
    fn parse_fn(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // fn
        let name = self
            .expect_decl_name("a function", "a function name")
            .unwrap_or_else(|| Symbol::intern(""));
        self.expect(&Token::LParen, "'('");
        let params = self.parse_params();
        self.expect(&Token::RParen, "')'");
        let ret = if self.eat(&Token::ThinArrow) {
            Some(self.parse_type())
        } else {
            None
        };
        self.expect(&Token::Arrow, "'=>'");
        let body = self.parse_expr(0);
        Member::Fn {
            name,
            params,
            ret,
            body,
            span: self.span_from(start),
        }
    }

    /// `lifecycle := "on" ("mount" | "unmount") "=>" action` (RFC-0028 §4b).
    fn parse_lifecycle(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // on
        let on_mount = matches!(self.cur(), Some(Token::Ident(s)) if s.as_str() == "mount");
        self.advance(); // mount | unmount
        self.expect(&Token::Arrow, "'=>'");
        let action = self.parse_expr(0);
        Member::Lifecycle {
            on_mount,
            action,
            span: self.span_from(start),
        }
    }

    /// `measure := "on" "measure" "=>" action` (RFC-0038).
    ///
    /// The payload binds as `it` rather than through a written parameter: the
    /// event has exactly one value and no second one is imaginable, so a
    /// binding position would be ceremony over `on measure => size = it`.
    fn parse_measure(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // on
        self.advance(); // measure
        self.expect(&Token::Arrow, "'=>'");
        let action = self.parse_expr(0);
        Member::Measure {
            action,
            span: self.span_from(start),
        }
    }

    /// `change := "on" "change" "(" expr ("," expr)* ")" "=>" action`.
    ///
    /// The watched values sit in parentheses because `on change lat => load()`
    /// would read `lat => load()` as a lambda.
    fn parse_change(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // on
        self.advance(); // change
        self.expect(&Token::LParen, "'('");
        let mut watched = Vec::new();
        while !matches!(self.cur(), Some(Token::RParen) | None) {
            watched.push(self.parse_expr(0));
            if !matches!(self.cur(), Some(Token::Comma)) {
                break;
            }
            self.advance();
        }
        self.expect(&Token::RParen, "')'");
        self.expect(&Token::Arrow, "'=>'");
        let action = self.parse_expr(0);
        Member::Change {
            watched,
            action,
            span: self.span_from(start),
        }
    }

    /// `timer := ("every" | "after") duration "=>" action` (RFC-0029 §5).
    fn parse_timer(&mut self) -> Member {
        let start = self.cur_span();
        let every = matches!(self.cur(), Some(Token::Ident(s)) if s.as_str() == "every");
        self.advance(); // every | after
        let dur = self.parse_expr(0);
        self.expect(&Token::Arrow, "'=>'");
        let action = self.parse_expr(0);
        Member::Timer {
            every,
            dur,
            action,
            span: self.span_from(start),
        }
    }

    /// `inject_stmt := "inject" type "as" IDENT`.
    fn parse_inject(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // inject
        let ty = self.parse_type();
        self.expect(&Token::As, "'as'");
        let name = self
            .expect_decl_name("an `inject` binding", "an inject binding name")
            .unwrap_or_else(|| Symbol::intern(""));
        Member::Inject {
            ty,
            name,
            span: self.span_from(start),
        }
    }

    /// `for_stmt := "for" (IDENT ",")? IDENT "in" expr "{" member* "}"`.
    ///
    /// The two-name form binds the item's index first (`for i, item in items`),
    /// which is what RFC-0025's stagger reads (`delay: i * 50ms`).
    fn parse_for(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // for
        let first = self
            .expect_ident("a loop variable")
            .unwrap_or_else(|| Symbol::intern(""));
        // `for i, item in …`, the first name was the index.
        let (index, var) = if self.eat(&Token::Comma) {
            let item = self
                .expect_ident("a loop variable")
                .unwrap_or_else(|| Symbol::intern(""));
            (Some(first), item)
        } else {
            (None, first)
        };
        self.expect(&Token::In, "'in'");
        let iter = self.parse_expr(0);
        self.expect(&Token::LBrace, "'{'");
        let body = self.parse_block_members();
        Member::For {
            var,
            index,
            iter,
            body,
            span: self.span_from(start),
        }
    }

    /// `when_stmt := "when" expr "{" member* "}" ("else" ("{" member* "}" | when_stmt))?`.
    fn parse_when(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // when
        let cond = self.parse_expr(0);
        self.expect(&Token::LBrace, "'{'");
        let then = self.parse_block_members();
        let els = if self.eat(&Token::Else) {
            // `else when …` is the nested form written without its braces
            // (RFC-0055): the next `when` is the whole else branch.
            if matches!(self.cur(), Some(Token::When)) {
                Some(vec![self.parse_when()])
            } else {
                self.expect(&Token::LBrace, "'{'");
                Some(self.parse_block_members())
            }
        } else {
            None
        };
        Member::When {
            cond,
            then,
            els,
            span: self.span_from(start),
        }
    }

    /// `nav_case := ("route" | "tab") STRING "{" ("|" IDENT "|")? member* "}"`
    /// (RFC-0026).
    ///
    /// The pattern is a *static* literal, a route table is resolved at mount
    /// time, so an interpolated pattern has no meaning; any interpolation is
    /// dropped with a diagnostic rather than silently half-matched. The optional
    /// `|params|` header binds the extracted `:param` segments as a record.
    fn parse_route(&mut self, kind: RouteKind) -> Member {
        let start = self.cur_span();
        self.advance(); // route | tab
        let pattern_span = self.cur_span();
        self.advance(); // STRING
        let parts = self.parse_string_literal(pattern_span);
        let mut pattern = String::new();
        for part in &parts {
            match part {
                ast::StrPart::Text(t) => pattern.push_str(t.as_str()),
                ast::StrPart::Interp(_, _) => self.error_at(
                    pattern_span,
                    "a literal route pattern (interpolation is not allowed, a route \
                     table is fixed at mount time)",
                ),
            }
        }

        self.expect(&Token::LBrace, "'{'");
        // `{|params| … }`, the params binding, written like a lambda header so
        // it reads the same as every other block parameter in the language.
        let params = if self.eat(&Token::Pipe) {
            let name = self.expect_ident("a route params binding name");
            self.expect(&Token::Pipe, "'|'");
            name
        } else {
            None
        };
        let body = self.parse_block_members();
        Member::Route {
            kind,
            pattern,
            params,
            body,
            pattern_span,
            span: self.span_from(start),
        }
    }

    /// `style_block := "style" "{" style_rule* "}"`,
    /// `style_rule := "." IDENT attr_block`.
    fn parse_style_block(&mut self) -> Member {
        let start = self.cur_span();
        self.advance(); // style
        self.expect(&Token::LBrace, "'{'");
        let mut rules = Vec::new();
        while !matches!(self.cur(), Some(Token::RBrace) | None) {
            let before = self.pos;
            let rstart = self.cur_span();
            self.expect(&Token::Dot, "'.' to start a style rule");
            let class = self
                .expect_ident("a class name")
                .unwrap_or_else(|| Symbol::intern(""));
            let attrs = if matches!(self.cur(), Some(Token::HashBracket)) {
                self.parse_attr_block()
            } else {
                Vec::new()
            };
            rules.push(StyleRule {
                class,
                attrs,
                span: self.span_from(rstart),
            });
            if self.pos == before {
                self.advance();
            }
        }
        self.expect(&Token::RBrace, "'}'");
        Member::Style {
            rules,
            span: self.span_from(start),
        }
    }

    // ---- elements & attributes ----

    /// `element := element_name ("(" arg_list? ")")? attr_block? element_tail?`,
    /// `element_name := IDENT ("." IDENT)?`, the qualified form is a
    /// package-namespaced user-`View` reference (`m.Card`, RFC-0008 Pillar B),
    /// interned as one dotted symbol and resolved by the module resolver.
    fn parse_element(&mut self) -> ElementNode {
        let start = self.cur_span();
        let mut name = self
            .expect_ident("an element name")
            .unwrap_or_else(|| Symbol::intern(""));
        if matches!(self.cur(), Some(Token::Dot)) && matches!(self.peek2(), Some(Token::Ident(_))) {
            self.advance(); // .
            if let Some(member) = self.expect_ident("a view name after '.'") {
                name = Symbol::intern(&format!("{}.{}", name.as_str(), member.as_str()));
            }
        }

        let content = if matches!(self.cur(), Some(Token::LParen)) {
            self.advance();
            let args = self.parse_arg_list(&Token::RParen);
            self.expect(&Token::RParen, "')'");
            args
        } else {
            Vec::new()
        };

        let mut states = Vec::new();
        let mut attrs = if matches!(self.cur(), Some(Token::HashBracket)) {
            self.parse_attr_block()
        } else {
            Vec::new()
        };

        // `as <name>` (RFC-0036), before the tail: it names *this* element, and
        // reads next to the element rather than after its body, which for a
        // container can be many lines further down.
        let anchor_name = if self.eat(&Token::As) {
            if let Some(Token::Ident(n)) = self.cur().cloned() {
                self.advance();
                Some(n)
            } else {
                self.error("a name after `as`");
                None
            }
        } else {
            None
        };

        // element_tail := "{" member* "}" | "=>" expr
        let mut action = None;
        let mut children = Vec::new();
        if self.eat(&Token::Arrow) {
            action = Some(self.parse_expr(0));
        } else if self.eat(&Token::LBrace) {
            children = self.parse_element_members(&mut states);
        }

        // RFC-0059: `on hover { … }` written in the body styles this element, as
        // the same block in a `style` does. They ride as one inline style spread
        // after the written attributes, so a tie is won by the body and nothing
        // downstream needs to know a second spelling exists.
        if let (Some(first), Some(last)) = (states.first(), states.last()) {
            let span = Span::new(first.span.start, last.span.end);
            attrs.push(Attr {
                name: Symbol::intern(""),
                axis: None,
                kind: AttrKind::Spread {
                    value: Expr::StyleValue {
                        attrs: Vec::new(),
                        states,
                        span,
                    },
                },
                span,
            });
        }

        ElementNode {
            name,
            content,
            attrs,
            action,
            children,
            anchor_name,
            span: self.span_from(start),
        }
    }

    /// `attr_block := "#[" attr_list? "]"`.
    fn parse_attr_block(&mut self) -> Vec<Attr> {
        self.advance(); // #[
        let mut attrs = Vec::new();
        while !matches!(self.cur(), Some(Token::RBracket) | None) {
            let before = self.pos;
            if let Some(a) = self.parse_attr() {
                attrs.push(a);
            }
            if self.pos == before {
                self.advance();
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RBracket, "']'");
        attrs
    }

    /// `attr := prop_attr | event_attr`, distinguished by the separator:
    /// `IDENT ("." IDENT)? ":" expr` (property, the optional sub-property
    /// axis is RFC-0011's `translate.y: 2`) vs `IDENT ("(" IDENT ")")? "=>"
    /// expr` (event, no sub-property form).
    fn parse_attr(&mut self) -> Option<Attr> {
        let start = self.cur_span();
        // `..style` spread (RFC-0016): no name, it splices a style value's
        // attributes into this list.
        if self.eat(&Token::DotDot) {
            let value = self.parse_expr(0);
            return Some(Attr {
                name: Symbol::intern(""),
                axis: None,
                kind: AttrKind::Spread { value },
                span: self.span_from(start),
            });
        }
        let name = self.expect_name("an attribute name")?;
        if self.eat(&Token::LParen) {
            let payload = self.expect_ident("an event payload name");
            self.expect(&Token::RParen, "')'");
            self.expect(&Token::Arrow, "'=>'");
            let action = self.parse_expr(0);
            Some(Attr {
                name,
                axis: None,
                kind: AttrKind::Event { payload, action },
                span: self.span_from(start),
            })
        } else if self.eat(&Token::Dot) {
            // Sub-property access (RFC-0011): `translate.y: 2`. Only the
            // property form takes an axis, an event never does.
            let axis = self.expect_name("a sub-property axis (e.g. `x`, `y`)")?;
            self.expect(&Token::Colon, "':'");
            let value = self.parse_expr(0);
            Some(Attr {
                name,
                axis: Some(axis),
                kind: AttrKind::Prop { value },
                span: self.span_from(start),
            })
        } else if self.eat(&Token::Colon) {
            let value = self.parse_arg_value();
            Some(Attr {
                name,
                axis: None,
                kind: AttrKind::Prop { value },
                span: self.span_from(start),
            })
        } else if self.eat(&Token::Arrow) {
            let action = self.parse_expr(0);
            Some(Attr {
                name,
                axis: None,
                kind: AttrKind::Event {
                    payload: None,
                    action,
                },
                span: self.span_from(start),
            })
        } else {
            self.error("':' (property) or '=>' (event)");
            None
        }
    }

    // ---- types ----

    /// `type := IDENT ("<" type ("," type)* ">")?`, plus the function type
    /// `Fn(type,*) ("->" type)?` (RFC-0003 E2). `Fn` lexes as a plain `Ident`,
    /// so it is recognized here by name.
    fn parse_type(&mut self) -> Type {
        let start = self.cur_span();
        let mut name = self
            .expect_ident("a type name")
            .unwrap_or_else(|| Symbol::intern(""));
        // `m.Variant`: a type of another package, qualified by the name or
        // alias it was imported under (RFC-0055).
        if matches!(self.cur(), Some(Token::Dot)) && matches!(self.peek2(), Some(Token::Ident(_))) {
            self.advance(); // .
            if let Some(member) = self.expect_ident("a type name") {
                name = Symbol::intern(&format!("{}.{}", name.as_str(), member.as_str()));
            }
        }

        if name.as_str() == "Fn" && matches!(self.cur(), Some(Token::LParen)) {
            self.advance(); // (
            let mut params = Vec::new();
            while !matches!(self.cur(), Some(Token::RParen) | None) {
                params.push(self.parse_type());
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::RParen, "')'");
            let ret = if self.eat(&Token::ThinArrow) {
                Some(Box::new(self.parse_type()))
            } else {
                None
            };
            return Type::Function {
                params,
                ret,
                span: self.span_from(start),
            };
        }

        let mut args = Vec::new();
        if self.eat(&Token::Lt) {
            while !matches!(self.cur(), Some(Token::Gt) | None) {
                args.push(self.parse_type());
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::Gt, "'>'");
        }
        Type::Named {
            name,
            args,
            span: self.span_from(start),
        }
    }
}

/// Whether `tok` is any reserved word, which no declaration may use as its
/// name.
fn is_name_keyword(tok: &Token) -> bool {
    is_keyword(tok) || matches!(tok, Token::With | Token::Merge)
}

/// Whether `tok` is a reserved keyword that may also be used as a contextual
/// name (an attribute, field, or argument label).
fn is_keyword(tok: &Token) -> bool {
    matches!(
        tok,
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
            | Token::Untrack
            | Token::Use
    )
}

#[cfg(test)]
mod tests;
