# Writing the guide

The guide is an [mdBook](https://rust-lang.github.io/mdBook/). Its pages are in
`src/`, its table of contents is `src/SUMMARY.md`, and `book.toml` configures it.

To read it as a book:

```sh
cargo install mdbook
mdbook serve docs/guide
```

Nothing needs mdBook to be checked. `cargo test` reads the Markdown itself.

## Every sample is a test

`crates/byard-cli/tests/it/guide.rs` walks `docs/guide/src/**/*.md` and checks each
fenced block that has a tag it knows. It runs a `byld` block through the same steps `byard check`
runs. When one fails, the message names the file, the line of the block and the
diagnostics:

```text
docs/guide/src/state.md:35: this byld sample does not check (1 diagnostic):
    docs/guide/src/state.md:37:24: error[UnknownName]: nothing here is named `count`
```

Run just the guide's tests with:

```sh
cargo test -p byard-cli --test it guide
```

### The tags

| Fence | The test requires |
|-------|-------------------|
| ` ```byld ` | A complete file. Zero diagnostics, warnings included. |
| ` ```byld,fragment ` | Statements or elements. The test wraps them in `View Main() { … }`, then checks as above. |
| ` ```byld,fails ` | At least one diagnostic. For showing what an error looks like. |
| ` ```byld,ignore ` | Nothing. Use it rarely, and put a comment in the block saying why. |
| ` ```diagnostic ` | Straight after a `byld,fails` block: every line must appear in a diagnostic that block produces, written `error[Kind]: message` (or `warning[Kind]: …`). |
| ` ```toml ` | It parses as TOML. `toml,ignore` is skipped. |
| ` ```rust,ignore,excerpt ` | Every non-blank line appears, as written, in a `.rs` file under `crates/` or `examples/` (doc comments count, without their `///`), or in the README. A line that is only `// …` marks something left out. |

An unknown tag on a `byld` block is an error, so a typo such as `byld,fial` cannot turn a
check off by accident.

Rust is not compiled by the guide. It is *quoted* from code that CI builds. Copy the lines
from the example, and the test keeps them true. Use plain `rust,ignore` only for a sketch
that is not a real file, and say so.

### When a sample cannot be checked alone

A sample is checked as a bare file with the built-in theme, no `byard.toml` and no
dependencies. Three things are out of its reach, and `byld,ignore` with a comment is the way
to show them:

- an app's own controllers. `inject Greeter as greeter` checks with a *warning*, which the
  guide shows with a `fails` block and a `diagnostic` quote;
- a package, since `use` needs a `[dependencies]` entry;
- a theme token, font family or breakpoint that `byard.toml` declares, and a native view.

For these, show the `byard.toml` in a `toml` block, and keep the byld block beside it.

## The book

`book.toml` and `SUMMARY.md` must exist, every `.md` file under `src/` must be listed in
`SUMMARY.md`, and every link must lead somewhere:

- a relative link points at a file in the book, and, when it has a `#fragment`, at a heading
  that exists. The anchor is the heading in lower case, with spaces as `-` and punctuation
  dropped, as mdBook makes it;
- a link to something outside the book, a source file or an example, is written as a GitHub URL under
  `https://github.com/Briany4717/byard/blob/main/` (or `tree/main` for a folder), because the
  rendered book cannot reach the repository's files. The test checks the path is there.

## Writing

- Each chapter starts with a small, complete example, then explains it.
- Short, plain sentences. British spelling: colour, not color. No em dashes.
- Document what the code does and an example shows. If an RFC describes something the code does not do, leave it out. The RFCs are design documents and some of what they describe
  was never built, or built differently.
- Prefer a sample that is a whole file to a fragment, and a real error message to a paraphrase.
- A claim about how the engine behaves, such as what is kept on a reload or what an event
  does, should be one you have seen in the code or in a running example.
