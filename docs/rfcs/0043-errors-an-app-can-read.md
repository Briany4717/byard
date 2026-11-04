# RFC-0043: Errors an app can read

- **Status:** Active, implemented 2026-10-25 (#339). Three errors are still
  reported as platform errors (#355).
- **Author(s):** Briany4717
- **Created:** 2026-10-24
- **Last updated:** 2026-10-26
- **Depends on:** RFC-0006 (the CLI and its diagnostics), RFC-0030 (the caret
  block diagnostics are printed in), RFC-0008 (packages, the resolver).
- **Enables:** an app's `main` that can tell a broken project from a broken
  GPU; the same diagnostics from `cargo run` as from `byard check`.

> **Placement:** `docs/rfcs/0043-errors-an-app-can-read.md`

---

## Summary

`ByardError` gets the two variants it is missing, `Project` and `Compile`, and
`byard-project` returns a typed `ProjectError` instead of `String`. When an
app's views do not compile, `App::run` and `Shot` print the same caret-block
diagnostics `byard check` prints, with the source line and the suggestion, and
return `ByardError::Compile` holding them. `Platform` goes back to meaning the
window, surface or event loop failed.

## Motivation

Run an app whose view has a misspelt name and `cargo run` prints

```
Error: platform host error: `examples/aura_weather/src/main.byd` did not compile:
  unknown name `cuont`
```

Three things are wrong with that, and all three are developer experience:

1. It says *platform*. The developer goes looking at their GPU driver.
2. It shows the headline only. `byard check` shows the file, line, column, the
   source line with a caret under the name and `did you mean count?`. The
   developer has to run a second tool to learn what the first already knew.
3. Code cannot tell the cases apart. A launcher that wants to show "the
   project is broken" and "your GPU is not supported" differently has to
   match on the text of a message.

The cause is that every project and compile failure is squeezed into
`ByardError::Platform(String)` (17 call sites in `byard` and `byard-platform`),
and that `byard-project` returns `Result<_, String>` from 38 functions
(manifest, resolver, solver, archive), so the structure is gone before
`byard` sees it.

## Guide-level explanation

```rust
match byard::App::new(env!("CARGO_MANIFEST_DIR")).run() {
    Ok(()) => {}
    Err(byard::ByardError::Compile(report)) => {
        // already printed; `report` has each diagnostic with file and span
        std::process::exit(2);
    }
    Err(e) => { eprintln!("{e}"); std::process::exit(1) }
}
```

and `fn main() -> Result<(), ByardError>` keeps working: a compile error now
prints the full diagnostics once (from `run`) and then one line,
`Error: 1 error in the project's views (shown above)`, instead of repeating
them as a flattened list.

## Reference-level explanation

### 1. `ProjectError` in `byard-project`

```rust
#[non_exhaustive]
pub enum ProjectError {
    NoManifest { searched_from: PathBuf },
    Manifest { path: PathBuf, line: Option<usize>, message: String },
    Io { path: PathBuf, source: std::io::Error },
    Resolve(ResolveError),       // unsatisfiable requirement, with pubgrub's explanation
    Fetch { package: String, source_desc: String, message: String },
    Lock { path: PathBuf, message: String },
    Archive { path: PathBuf, message: String },
}
```

with `Display` written to read as a sentence (the messages today are good;
they move into `Display` unchanged where they already exist) and `source()`
for `Io`. No `thiserror`: the workspace writes its `Display`s by hand
(`ByardError`, `AtlasError`), and one more proc-macro on the build path costs
every build for no runtime gain. The CLI keeps `String` at its own top level
(`commands::*::run -> Result<(), String>` prints and exits), converting with
`to_string()`; it is the end of the line, nothing downstream matches on it.

### 2. Rendering diagnostics is a compiler function

The caret-block renderer lives in `byard-cli` today
(`commands::check::print_diagnostics`). It moves to
`byard_compiler::diagnostics::render(errors, source_map, style) -> String`,
with `style` choosing colour (when stderr is a terminal and `NO_COLOR` is
unset) or plain, and `short` for the one-line form `--short` prints. The CLI
calls it; `byard` calls it. One renderer, so the two can never disagree.

### 3. `ByardError`

```rust
Project(byard_project::ProjectError),   // via a boxed or re-exported type, see Q1
Compile(CompileReport),
```

`CompileReport` holds the diagnostics and the source map, `Display` prints the
one-line summary, and `CompileReport::render()` the full block. `App::run`,
`App::hot_reload`'s initial load and `Shot::shot` print `render()` to stderr
before returning `Compile`. A reload that fails later keeps printing as it does
today and keeps the app running.

`Platform(String)` keeps its meaning: window, surface, event loop. The call
sites in `byard/src/app.rs` and `shot.rs` that report a project or a view
problem move to `Project` or `Compile`; "the program declares no `View`"
becomes a `Compile` diagnostic, so it gets a span (the entry file) like any
other.

## Cost

None at run time: errors are cold paths. One function moves between crates.
Binary size is unchanged within noise.

## Drawbacks

A breaking change to `ByardError` (two variants) and to every public
`byard-project` signature. Pre-alpha, nothing is published, and the enum is
already `#[non_exhaustive]`, so downstream `match`es already have a `_` arm.

## Rationale and alternatives

- **`anyhow` in `byard`**: loses the ability to match, which is the third
  problem above.
- **Keep `String` and fix the message**: fixes the first problem only.

## Resolved questions

### Q1, `byard-core` cannot name `ProjectError`

`ByardError` lives in `byard-core`, which `byard-project` depends on, so core
cannot depend back. **Options.** (a) move `ByardError` to `byard`; (b) a
`Project(Box<dyn Error + Send + Sync>)` variant with a downcast helper; (c)
define `ProjectError` in `byard-core`. **Resolution: (b), with
`ByardError::as_project() -> Option<&ProjectError>` provided by an extension
trait in `byard`.** (a) moves a type every crate uses and breaks the engine's
own signatures; (c) puts manifest knowledge in the engine. (b) costs one box
on a cold path. `CompileReport` has the same problem with
`byard-compiler`'s types and gets the same answer.

### Q2, whether the report is printed by `run` or left to `main`

**Resolution: printed by `run`.** `fn main() -> Result<(), E>` prints
`Debug`, which is unreadable for a diagnostic, and that is the `main` every
example and the README use. Printing once in `run` and returning a short
`Display` gives the good output to the code everyone writes.

## Acceptance

- `byard-project` has no `Result<_, String>` left in its public API (a test
  module asserts the signatures compile against `ProjectError`).
- An app test: a project with a misspelt name makes `load_program` return
  `Compile`, and the rendered text contains the file, the line, the caret and
  `did you mean`.
- A missing manifest returns `Project` and its `Display` names the directory
  searched.
- `byard check` output is byte-for-byte unchanged on the CLI's existing
  diagnostic tests (it now goes through the moved renderer).
- **For a person to check:** a broken copy of an example
  (`crates/byard-cli/tests/fixtures/broken_app`) run through `Shot` from an
  integration test whose stderr is captured to a snapshot file that the PR
  shows; and by hand, misspell a name in `examples/aura_weather/src/main.byd`
  and run `cargo run -p aura-weather`.
