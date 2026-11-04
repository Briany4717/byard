# RFC-0044: The byld formatter, `byard fmt`

- **Status:** Active, implemented 2026-10-25 (#342). Breaking long lines other
  than attribute lists, `--stdin` and range formatting are open (#356).
- **Author(s):** Briany4717
- **Created:** 2026-10-24
- **Last updated:** 2026-10-26
- **Depends on:** RFC-0002 (the lexer and parser), RFC-0006 (the CLI).
- **Enables:** one layout for every `.byd` file; format on save in every
  editor through the LSP; a CI check like `cargo fmt --check`.

> **Placement:** `docs/rfcs/0044-byld-formatter.md`

---

## Summary

`byard fmt [path] [--check]` formats `.byd` files in place, or with `--check`
lists the ones that are not formatted and exits 1. The formatter lives in
`byard-compiler` (`byard_compiler::fmt`), works on the token stream with
comments kept, never changes what a file means, and is the one the LSP's
`textDocument/formatting` uses.

## Motivation

The LSP has a formatter (`byld-lsp/src/capabilities/formatting.rs`, 81 lines)
that re-indents by counting `{` and `}` at line starts and ends. It does not
look at tokens, so a line whose string holds an interpolation
(`Text("{count} taps")`) or a brace in text can shift the indentation of
everything after it, and it leaves spacing inside lines as typed. There is no
command line for it, so a project cannot check formatting in CI, and two
contributors' editors disagree.

Rust developers expect `fmt` from their toolchain. Byard's DX promise means
`byard fmt` should feel like `cargo fmt`: zero options to learn, fast enough
to run on every save, and never wrong.

## Guide-level explanation

```bash
byard fmt                 # format every .byd under the project (src/, and packages it owns)
byard fmt src/main.byd    # one file
byard fmt --check         # CI: list unformatted files, exit 1 if any
```

The style, which has no options:

- 4-space indentation, one level per open `{`, `(` that spans lines, or `[`.
- One space around binary operators, `=>`, `=` and after `,` and `:`; none
  inside `(` `)` `[` `]` or before `,`.
- `#[ … ]` attribute lists stay on one line when they fit in 100 columns,
  else one attribute per line with a trailing comma.
- At most one blank line in a row; none after `{` or before `}`.
- Comments stay where they were, attached to the token after them (a line
  comment at the end of a line stays at the end of that line).
- A final newline; no trailing whitespace.

## Reference-level explanation

### 1. A token stream with trivia

The lexer skips whitespace and comments (`#[logos(skip ...)]`). The formatter
needs them, so `byard_compiler::lexer` gains `lex_with_trivia(src) ->
Vec<(Token | Trivia, Span)>`, where `Trivia` is `LineComment(&str)` or
`Newlines(n)`. The existing `lex` is unchanged and still skips them, so
parsing pays nothing.

### 2. The printer

A single pass over the trivia stream with a small state (indent depth, a stack
of open brackets and whether each was opened at a line end, the column, and
the previous token's class). It decides per token whether to emit a space,
nothing, or a newline plus indentation. Attribute lists that do not fit are
re-emitted vertically by a look-ahead to the matching `]`. String literals,
including their interpolations, are copied byte for byte.

It is linear in the file's size and allocates one output `String`.

### 3. The safety check

`fmt::format(src)` returns `Result<String, FmtError>`. Before returning the
output it parses both input and output and compares the two ASTs with spans
ignored (a `PartialEq` that skips `Span`, added for this). If they differ, or
the input does not parse, it returns an error and the file is left untouched;
`byard fmt` reports the file and moves on. A formatter that changes meaning is
worse than none, so this check is never skipped; parsing costs well under a
millisecond for the largest file in the repository.

### 4. Where it is used

- `byard fmt` walks the project's `.byd` files (the manifest's entry directory
  and its path dependencies inside the project, never `.byard/` or packages
  fetched into the cache).
- The LSP's `handle_formatting` calls `fmt::format` and returns one full-file
  edit, as today; its own `format_byd_source` is deleted. Tab size from the
  editor's options is ignored: the style has no options.

## Cost

Nothing at run time: the formatter is a library function the CLI and the LSP
call. The parser is untouched. The new lexer entry point is not used by
`lex`.

## Drawbacks

A formatter with no options will disagree with somebody. `cargo fmt`'s and
`gofmt`'s experience is that the disagreement is shorter-lived than the one
about style.

## Rationale and alternatives

- **A full pretty-printer from the AST** (like `prettier`) gives the best
  line-breaking, but the AST drops comments and would need trivia attached to
  every node first, a parser change for a formatter.
- **Keep the LSP's line re-indenter**: it is the defect.

## Prior art

`gofmt` (no options, token-based with comment trivia), `rustfmt --check`,
`zig fmt` (AST compare as a safety net).

## Resolved questions

### Q1, whether `byard fmt` formats fetched packages

**Resolution: no.** A package fetched into the cache belongs to its author;
formatting it would make the lockfile's hash lie. Path dependencies inside the
project are the project's own and are formatted.

### Q2, line width

**Resolution: 100 columns,** the same as the workspace's Rust (`rustfmt`'s
default `max_width`), so a developer moving between the two halves of an app
sees the same width.

## Acceptance

- Idempotence: for every `.byd` file in the repository (68 today), formatting
  twice gives the same result as once (a test walks the tree).
- Meaning: for every one of those files, the AST of the output equals the
  input's (the same test).
- Targeted tests: interpolated strings with braces, end-of-line comments,
  a long attribute list breaking, blank-line collapsing, CRLF input.
- `byard fmt --check` exits 1 on an unformatted fixture and 0 after
  `byard fmt`; the LSP formatting test runs through `fmt::format`.
- **For a person to check:** `byard fmt --check crates/byard-cli/examples`
  in the PR's notes, and the diff of running `byard fmt` once over the
  repository's examples, committed separately so the reviewer can read it.
