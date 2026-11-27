# The byld conformance suite

What the language does, one small program at a time. Each `.byd` file here holds many
cases; a case is a `//# case:` line, the `//#` lines that say what it must draw or report,
and a program.

```sh
cargo test -p byard-cli --test it conformance -- --nocapture
```

The headers, and how a case is run, are described at the top of
[`../it/conformance.rs`](../it/conformance.rs). In short:

```text
//# case: int-division-truncates
//# fragment
//# text: 3
Text("{7 / 2}")
```

A case marked `//# gap: why` says what *should* happen where the language does not do it
yet, and is expected to fail. When it starts to pass, the suite fails and says to delete
the marker, so the list of gaps is always the list of what is left. The reasons and the
order are in [`docs/language/roadmap.md`](../../../../docs/language/roadmap.md), and what
the language is today is in the guide's
[language reference](../../../../docs/guide/src/language-reference.md).

A file says which sections of the language reference it is the cases of, under its leading
comment (`//# covers: lists, maps-and-sets`, by the anchor of the heading). A section no file covers
is in `uncovered.txt`, a list that can only get shorter: the test
`every_section_of_the_reference_has_cases` fails when a section has no cases and is not on it,
and when one on it has cases.

Past the parser: the same test file formats every program of the suite to a fixed point
(`fmt(fmt(x)) == fmt(x)`, and `byard fmt` already refuses a result whose tree differs), damages each
a few ways (a line gone, two swapped, a token put where it does not go, the end cut off) and runs the
result through `byard check` and a frame, and writes a few thousand programs from the grammar in both
editions: none may panic. The generator writes an expression for the type that is wanted, so most of
what it makes checks, and now and then one of any shape, so the refusals are reached too. Its seeds are
fixed, so a failure is the same failure next time and prints the program.

These files are not programs to run: several declare `Main`, and many are wrong on
purpose. `byard fmt` and `byard check` are not meant for them.

| File | About |
|------|-------|
| `01_lexical` | Comments, literals, strings, escapes, formats |
| `02_numbers` | Arithmetic, division, overflow, number methods |
| `03_text` | `Str` methods, comparison, Unicode |
| `04_lists` | Indexing, every list method, `for` |
| `05_records` | Fields, spread, record types, equality |
| `06_enums_constants` | Enums, constants, shared functions, `private` |
| `07_operators` | Logic, `? :`, `??`, precedence, assignment targets |
| `08_functions` | `fn`, lambdas, named actions, calls |
| `09_views` | Parameters, defaults, slots, callbacks |
| `10_bind_context_optional` | Bindings, contexts, optional values |
| `11_control_flow` | `when`, `for`, `key`, the statements of an action |
| `12_state` | `var`, `let`, inference, reactivity, lifecycle, timers |
| `13_elements_styles` | Elements, attributes, events, styles, states, motion |
| `14_syntax_and_recovery` | Malformed programs: one mistake, one error, never a panic |
| `15_wanted_for_1_0` | What 1.0 needed and the language lacked, each once a gap and now a case; a new gap goes in the file of its subject |
