# byld to 1.0: what is missing, in order

What the language has to gain, fix and stop doing before it can be called 1.0, as steps
that can each be done and merged on their own.

It goes with two other things:

- [the language reference](../guide/src/language-reference.md), which says what byld is
  today, with a **Not yet** note at every place it falls short;
- the conformance suite, `crates/byard-cli/tests/conformance/`, where each of those notes
  is a case marked `//# gap:`. A gap case says what should happen and is expected to fail.
  The day it passes, the suite fails and tells you to delete the marker.

So every step below ends the same way: **its cases lose their `gap:` line, the reference
loses its note, and this file loses the step.** When this file is empty and the suite has no
gap, the language is where this document wanted it.

As written, the suite has 613 cases: 570 hold and 43 are gaps.

```sh
cargo test -p byard-cli --test it conformance -- --nocapture
```

prints every open gap with its reason.

## The rules for all of it

**Do**

- Take one step per pull request. Each is small enough to review and has its own cases.
- Turn the cases first. Delete the `gap:` lines of the step, watch them fail, then make
  them pass. A step that needs a case the suite lacks adds it in the same pull request.
- Make every new rejection a diagnostic with a name of its own, a message that says what to
  write instead, and the nearest name when there is one, as the existing ones do.
- Run the whole guide and every example through `byard check` after each step
  (`cargo test -p byard-cli`). A step that makes the checker stricter will find real
  mistakes in them; fix those in the same pull request.
- Ship anything that rejects programs that used to pass as a **warning for one release**,
  then as an error. From step 3 on, behind an edition (step 2), except where a step says otherwise: step 3 is an error in every edition, because what it refuses never meant anything.

**Avoid**

- Fixing a silent failure by making it a different silent failure. `1 / 0` giving `0`
  quietly is not improved by giving `-1` quietly.
- Adding a second way to write something that already has one. The language has two
  lambda syntaxes and two ways to attach a handler; it does not need a third of anything.
- Features that need a new runtime concept when a checked form of an existing one will do.
  `match` is `when` chains the checker can count; it is not a new evaluator.
- Traits, inheritance, macros, user-defined operators, exceptions, `null`, `while`, and
  mutation in place. Each has been asked for in every language and each would cost byld
  the thing that makes it checkable: a program always ends, and a value never changes
  under a reader.
- Widening `Unknown`. The checker's escape hatch for "I cannot tell" is why steps 3 to 7
  exist. New code paths return a real type or an error.
- Changing what a passing case draws without an edition. A program that checked and drew
  `3` yesterday draws `3` tomorrow.

## Part 1: stop accepting wrong programs

The language is typed in its views and untyped in its functions, variables and records.
This part closes that. Nothing here adds syntax.

### 1. One mistake, one error

*Left:* one bullet, with no case yet. Its nine cases (`int-literal-too-large-is-an-error`,
`identifier-unicode-is-rejected`, `string-interpolation-unclosed`, `attribute-with-equals`,
`type-without-braces`, `lambda-without-a-body`, `one-mistake-is-one-error-in-a-call`,
`deeply-nested-interpolation-is-refused`, `constant-self-cycle-is-one-error`; issues #358 and
#359) hold, and so do the cases added with them.

- Report attribute errors in the same pass as the others, not after they are all fixed.
  They come from lowering the program to elements, which `byard check` skips while the type
  checks have found anything. Lowering anyway was tried, and it puts follow-on errors beside
  the real ones (`ClockOutsideAction` beside `ImpureFunction`, `MissingParam` beside
  `SlotArgument`). It wants either the attribute checks run without lowering, or a list of
  the kinds lowering is allowed to add. Add a case, `attribute-error-beside-a-type-error`,
  with it.

### 2. Editions, and `byard fix`

*Done, except for what only the first **(edition)** change can show.* `edition = "…"` is read
from `[project]` or `[package]`, a manifest without it is `2026`, and `byard fix` moves a
project to `2027`, rewriting its sources through the formatter and naming the edition in the
manifest. The two editions differ by nothing yet. What is left is the first rewrite: step 6's
`equality-ignores-field-order` shipped in every edition, since no rewrite can tell which programs
relied on reordered records being unequal, so step 12's ternary is the one that adds a migration to
`Edition::migrations` in `byard-compiler/src/edition.rs`, and the case that proves `byard fix`
keeps a program's meaning. Until then `byard new` writes no edition, and a project without one
stays on `2026`.

### 8. Names

*Left:* three bullets, with no case yet. The sixteen cases (`var-declared-twice`,
`var-and-let-of-one-name`, `var-named-like-a-parameter`, `view-duplicate-parameter`,
`var-inside-an-element-body`, `let-cycle`, `attribute-written-twice`, `state-block-twice`,
`intrinsic-unknown-named-argument`, `trailing-action-on-an-element-that-takes-none`,
`with-outside-an-attribute`, `view-named-before-positional`, `view-lowercase-name`,
`enum-empty`, `timer-of-zero`, `var-unused`) hold, and so do the twenty-nine added with them.
Eleven older cases that declare a name and never use it (the `…-does-not-panic` ones, and
two that test scope) now name the warning they get, `//# warning: UnusedVariable`, and
nothing else of them changed.

- A name that is only written, `n++` and nothing else, counts as used. "Never read" is the
  stricter test the lint is called for, and the suite's `timer-of-zero` and `timers-are-accepted`
  both keep such a `var` and expect no warning, so it wants those cases to say what they mean
  first.
- A circle of `let`s is an error only when something reads it (`let-cycle-does-not-hang`
  pins that a circle nobody reads draws), and only in the view's own body: in a `when` or a
  `for` a name below is `UnknownName`, once, which is the right error for it.
- `UnusedView` is for a `private View` nothing draws. A view of an app that is not `Main`
  and that nothing draws is not reported, because nothing here says which view an app runs
  but the name.

## Part 2: stop failing quietly

### 9. Writing a field

*Cases:* `field-assignment`, `nested-field-assignment`.

`u.score = 10` means `u = { ..u, score: 10 }`, and `u.a.b = 2` the same, one level down;
`xs[i] = v` likewise for a list. The root has to be a `var` or a binding. This is sugar
over what the language already does, and it is the one place where today's behaviour is
worst: it checks clean and does nothing.

### 10. Faults a developer can see

*Cases:* `division-by-zero-int`, `int-overflow-add`, `index-out-of-range-is-reported`.

A *fault* is a division by zero, an overflow, an index past the end, or a field read from
nothing. A fault never stops a shipped app, and that stays. What changes:

- Under `byard dev`, `byard shot` and `byard test`, a fault is reported once per place,
  with its line: in the status line, in the HUD, and as a failed step of a test.
- `byard check` reports the ones it can see without running: a literal zero divisor, a
  literal index past a literal list.
- In a shipped app a fault goes to the app's log, and to a hook `App::on_fault` gives a
  Rust half.
- Overflow saturates instead of wrapping **(edition)**. A total that wraps to a negative
  number is worse than one that stops at the largest.

Do not make these errors that stop the program. An interface that goes blank because a
server sent an empty list is worse than one that shows a gap.

### 11. Text

*Cases:* `string-interpolation-trailing-junk`, `format-too-many-decimals`,
`list-in-text-shows-its-items`, `len-of-grapheme-cluster` **(edition)**, `index-into-text`.

- What follows the expression in `{ }` is an error, not dropped.
- A list in a text shows as `[1, 2, 3]`, a record as `{ name: Ada }`. Never the debug form
  of the Rust value, and never nothing.
- `len`, `slice` and `indexOf` count what a reader sees as characters. `s[i]` is the
  character at `i`, or nothing.

### 12. Small things that parse wrongly

*Cases:* `int-literal-with-separators`, `float-literal-exponent`, `int-literal-binary`,
`block-comment`, `byte-order-mark-at-the-start`, `untrack`, `slot-passed-on`,
`ternary-after-or-means-or-of-a-ternary` **(edition)**.

- `1_000`, `1e3`, `1.5e-3` and `0b101` are numbers. A digit followed by a letter that
  makes no literal is one lexing error, not a number and a name.
- `/* … */`, nesting, and a byte order mark skipped.
- `untrack(expr)` parses, as RFC-0004 has said since the start (#358).
- A required `Slot` can be handed to another view, as the guide says.
- `? :` becomes looser than `||`, as in every language people come from. `byard fix`
  adds the parentheses that keep an old program meaning what it meant, and
  `MixedTernaryPrecedence` goes.

## Part 3: what a 1.0 language has

Each of these has a case in `15_wanted_for_1_0.byd` written the way
[the proposals](proposals.md) spell it. The proposals give the syntax and the reasons; this
is only the order.

### 13. `none` for any value

*Cases:* `optional-var`, `if-let-on-an-optional`, `optional-chaining`,
`list-get-is-optional`.

`T?` on a `var`, a field and a result, not only a parameter. Then the three things that
make it pleasant: `a?.b`, `if let v = a { … }` in an action, and `xs.get(i)`, which is
`T?` where `xs[i]` is `T`. Do this before `match`, which needs it.

### 14. `match`, and enums that carry values

*Cases:* `match-on-an-enum`, `match-as-a-view-member`, `match-must-cover-every-case`,
`enum-with-data`, `when-chain-over-an-enum-is-not-checked-for-missing-cases`.

One feature in two pull requests: `match` over today's enums with the missing-case check,
then cases with fields. Together they replace the three parallel `var`s every loading
screen carries.

### 15. Loops in actions, and ranges

*Cases:* `action-for-loop`, `for-with-range`, `action-early-return`.

`for x in xs { … }` as a statement, `a..b` as a list of `Int`s, and `return` to leave an
action. No `while`: every action still ends.

### 16. Functions that scale

*Cases:* `function-with-a-block-body`, `function-named-arguments`, `generic-function`.

A block body with `let`s and a last expression; arguments by name, as views take them;
type parameters on `fn` and `type`.

### 17. Collections

*Cases:* `map-type`, `list-helpers`, `list-insert-and-update`.

`Map<K, V>` and `Set<T>`, and the list methods people write by hand every week.

### 18. Types for what is an `Int` today

*Cases:* `colour-type-shows-as-a-colour`, `duration-type-keeps-its-unit` **(edition)**.

`Color`, `Duration` and `Angle` as types of their own. `0xFF0000 + 200ms` stops meaning
something.

### 19. Data from outside, with a type

*Case:* `typed-json`.

`value as Type` checks a reply's shape where it arrives and fails into the `err` arm with
the path of the first field that does not fit. Reading a field of an untyped reply becomes
a warning. With step 6, this ends the misspelt-JSON-field problem both reference apps
recorded.

### 20. State above the views

*Case:* `global-store`.

`store Name { field: Type = default }`, read and written through `inject`. One instance
for the program, optionally kept between runs.

### 21. The rest of text

*Cases:* `multi-line-string`, `string-format-width`.

Multi-line strings that drop their indentation, padding in a format, and doc comments that
the language server and `byard doc` read.

## Part 4: freeze

22. **The specification.** The reference becomes the normative text. Every rule in it has
    a case, and a case with no rule is deleted or given one.
23. **Fuzzing past the parser.** Today's fuzzing is random text into the lexer and parser.
    Add: random *well-formed* programs into the checker and the interpreter, which must
    never panic; and `fmt(fmt(x)) == fmt(x)` with the same tree, for every case in the
    suite.
24. **Three applications nobody on the project wrote**, each run through the edition
    migration, before the edition is named 1.0.
25. **The promise.** A program that checks under an edition keeps its meaning for as long
    as that edition exists. The suite, frozen per edition, is the test of that promise.

## How to tell it is done

- `cargo test -p byard-cli --test it conformance` prints `0 gaps open`.
- No `Not yet` is left in the language reference.
- Every example in the repository and every sample in the guide checks under the newest
  edition with no warning.
- Aura Weather and Stockroom have deleted the workarounds their READMEs list under "still
  could not be built with".
