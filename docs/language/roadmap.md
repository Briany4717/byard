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

As written, the suite has 534 cases: 431 hold and 103 are gaps.

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
  then as an error. From step 3 on, behind an edition (step 2).

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
`equality-ignores-field-order` or step 12's ternary, whichever comes first, adds a migration to
`Edition::migrations` in `byard-compiler/src/edition.rs`, and the case that proves `byard fix`
keeps a program's meaning. Until then `byard new` writes no edition, and a project without one
stays on `2026`.

### 3. Assignments and first values

*Cases:* `var-annotation-of-the-wrong-type`, `var-list-annotation-of-the-wrong-type`,
`let-annotation-of-the-wrong-type`, `var-assigned-another-type`,
`var-plus-equals-another-type`, `var-increment-on-text`,
`var-int-literal-annotated-float-divides-as-a-float`.

- The first value of a `var`, `let` or constant is checked against its written type. This
  already exists for constants (`ConstantTypeMismatch`); use the one path for all three.
- `x = e`, `x += e`, `x -= e`, `x++` and `x--` are checked against the type of `x`.
- An `Int` where a `Float` is wanted is *converted*, not merely allowed: `var w: Float = 3`
  holds `3.0`, so `w / 2` is `1.5`. The same conversion is step 5's for arguments.

### 4. Type names

*Cases:* `var-annotation-that-is-no-type`, `var-map-type-does-not-exist`,
`record-type-with-unknown-field-type`.

A type name that is not built in, not a declared record, enum or context, and not
`package.Name`, is `UnknownType`, with the nearest name. Today `Map<Str, Int>` and `Qux`
are accepted anywhere a type is written.

### 5. Calls to functions and actions

*Cases:* `function-too-few-arguments`, `function-too-many-arguments`,
`function-argument-of-the-wrong-type`, `function-body-of-the-wrong-type`,
`function-int-where-float-is-wanted`, `view-int-argument-divides-as-a-float`,
`named-action-wrong-arity`, `named-action-wrong-type`, `calling-a-number`,
`callback-argument-of-the-wrong-type`, `map-wrong-arity-lambda`, `push-wrong-type`,
`filter-predicate-must-be-bool`, `index-with-float-is-an-error`.

- A call to a `fn` or a named action is checked as a call to a view already is: number,
  then types. Reuse `ArgumentTypeMismatch` and `ArityMismatch`.
- A function's body is checked against its result type.
- A parameter that was not given must never again read as its own name. That it does
  (`add(1)` draws `1b`) is the interpreter resolving an unbound name to text; after this
  step the checker refuses the call, and the interpreter's fallback becomes an internal
  error, not a value.
- The built-in methods get signatures the checker reads: `push(T)` on a `List<T>`, a
  one-parameter function for `map`, a `Bool` result for `filter` wherever it is written.

### 6. Records

*Cases:* `declared-type-literal-with-a-wrong-field-type`,
`declared-type-literal-with-an-extra-field`,
`declared-type-literal-missing-a-required-field`, `missing-field-on-a-literal-is-an-error`,
`duplicate-field-in-a-literal`, `lambda-field-typo-on-a-typed-list`,
`lambda-field-typo-on-an-inferred-list`, `equality-ignores-field-order` **(edition)**.

- A literal given to something with a record type is checked field by field.
- A literal with no declared type has the type its fields give it, so `u.nmae` on
  `let u = { name: "Ada" }` is `UnknownField`. Only a value from JSON or from a controller
  with no types is still "anything".
- A lambda's parameter takes the item type of the list its method is called on.
- Equality compares fields by name.

### 7. Conditions, and what can be written to

*Cases:* `when-condition-must-be-bool`, `action-if-condition-must-be-bool`,
`ternary-condition-must-be-bool`, `ternary-arms-of-different-types`,
`logic-on-a-number-is-an-error`, `assign-to-a-let`, `assign-to-a-parameter`,
`assign-to-a-literal`, `assign-to-a-loop-variable`,
`assignment-is-not-an-expression-in-a-text`, `increment-in-a-let-is-an-error`,
`for-over-a-non-list`.

- The condition of `when`, `if` and `? :` is a `Bool`. The two arms of `? :` are of one
  type (an `Int` and a `Float` are a `Float`).
- An assignment's target is a `var`, a binding, or a field of one (step 9). Anything else
  is `NotAssignable`, which exists and is not raised for these.
- An assignment outside an action is `EffectInPureContext`, which exists too.
- `for` takes a list.

### 8. Names

*Cases:* `var-declared-twice`, `var-and-let-of-one-name`, `var-named-like-a-parameter`,
`view-duplicate-parameter`, `var-inside-an-element-body`, `let-cycle`,
`attribute-written-twice`, `state-block-twice`, `intrinsic-unknown-named-argument`,
`trailing-action-on-an-element-that-takes-none`, `with-outside-an-attribute`,
`view-named-before-positional`, `view-lowercase-name`, `enum-empty`, `timer-of-zero`,
`var-unused`.

- Two declarations of one name in a view, or a declaration over a parameter, is
  `NameCollision`.
- A `var` in an element's braces is refused (`LetInElementBody` exists for `let`) until
  block-scoped state is designed; today it is accepted and then reads as the text `n`.
- `let`s in a view that read each other in a circle are the error constants already get.
- An attribute or a state block written twice, a named argument an element lacks, and an
  action after an element that has no press are errors, not silence.
- Lints, as warnings with names, each switchable in `byard.toml` under `[lints]`: a name
  never read, a view never used, a parameter never read.

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
