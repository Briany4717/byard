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

As written, the suite has 822 cases: 818 hold and 4 are gaps.

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

*Done.* `edition = "…"` is read from `[project]` or `[package]`, a manifest without it is
`2026`, and `byard fix` moves a project to `2027`, rewriting its sources through the formatter
and naming the edition in the manifest. The first rewrite is step 12's ternary: it writes the
parentheses that keep `a || b ? x : y` meaning what it meant, and a unit test parses the old
file in 2026 and the new one in 2027 and finds the same program. What `2027` changes that no
rewrite can keep (step 10's saturation, step 11's graphemes and what a list shows in a text)
changes no source, as step 6's `equality-ignores-field-order` shipped in every edition for the
same reason. Left: a package is parsed in `2026` whatever the app that uses it says, since a
provider does not tell its edition yet, and `byard new` writes no edition, so a project
without one stays on `2026`.

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

*Left:* one bullet, with a case, `index-assignment`. `field-assignment` and `nested-field-assignment` hold,
and so do the eight cases added with them.

- `xs[i] = v` is still accepted and changes nothing. It wants a list update to rebuild the
  list with (`updateAt`, step 17).

### 10. Faults a developer can see

*Left:* the half that runs. `division-by-zero-int`, `int-overflow-add` and
`index-out-of-range-is-reported` hold, and so do the eleven cases added with them: `byard
check` warns of a written zero divisor (`DivisionByZero`) and of a number written as an index
past a list written beside it (`IndexOutOfRange`, not where a `&&`, `??` or `? :` guards
it), and edition `2027` saturates an `Int` (`+ - * /`, `-x`, `abs`, `+=`, `-=`, `++`, `--`)
where `2026` wraps. The harness has an `//# edition:` header for it. `byard fix` rewrites
nothing for the change, since no rewrite can tell which programs relied on wrapping.

A *fault* is a division by zero, an overflow, an index past the end, or a field read from
nothing. A fault never stops a shipped app, and that stays. What is not done:

- Under `byard dev`, `byard shot` and `byard test`, a fault is reported once per place,
  with its line: in the status line, in the HUD, and as a failed step of a test. Nothing
  reports a fault that only happens while the program runs, and a field read from nothing is
  not seen at all. The suite cannot hold a case for it, since it runs `byard check`; it
  wants a harness that can run a frame and read what was reported.
- In a shipped app a fault goes to the app's log, and to a hook `App::on_fault` gives a
  Rust half.
- `byard check` sees a literal zero divisor and a literal index; a `let` that is zero, or
  an index in a `var`, it does not.

Do not make these errors that stop the program. An interface that goes blank because a
server sent an empty list is worse than one that shows a gap.

### 11. Text

*Left:* nothing, with a note. `string-interpolation-trailing-junk`, `format-too-many-decimals`,
`list-in-text-shows-its-items`, `len-of-grapheme-cluster` and `index-into-text` hold, and so do
the twelve cases added with them. Trailing junk in a `{ }` and a tenth decimal are
`UnexpectedToken`; `s[i]` is the character at `i`, or nothing, in every edition. Counting by
grapheme (`len`, `slice`, `indexOf`, `split("")`, `s[i]`) and showing a list or a record in a
text are edition `2027`, because `len-counts-code-points-not-graphemes` and
`list-in-text-is-nothing-today` pin the old behaviour in `2026`. `byard fix` rewrites nothing
for either. A `Map` has no text form yet, since there is no `Map` (step 17).

### 12. Small things that parse wrongly

*Left:* nothing, with a note. All eight cases hold (`int-literal-with-separators`,
`float-literal-exponent`, `int-literal-binary`, `block-comment`, `byte-order-mark-at-the-start`,
`untrack`, `slot-passed-on`, `ternary-after-or-means-or-of-a-ternary`), and so do the fifteen
added with them. `? :` is looser than `||` from edition `2027` and tighter in `2026`, so
`MixedTernaryPrecedence` is still told in `2026` and never in `2027`; it goes when `2026` does.
`///` is still an ordinary comment.

## Part 3: what a 1.0 language has

Each of these has a case in `15_wanted_for_1_0.byd` written the way
[the proposals](proposals.md) spell it. The proposals give the syntax and the reasons; this
is only the order.

### 13. `none` for any value

*Left:* a few forms, with no case yet. `optional-var`, `if-let-on-an-optional`,
`optional-chaining` and `list-get-is-optional` hold, and so do the fourteen cases added with
them. `T?` is written on a `var`, a `let` and in a record; `a?.b` is a field read that is
nothing when `a` is; `if let v = a { … } else { … }` in an action binds `v` to the value, in
that block only; `xs.get(i)` is a `T?`. All of it is in every edition: none of it was written
before. Passing an optional on to a `let` (`let v = n`) is not a read, and writing one is not
either.

- `when let v = a { … }` in a view. `when a != none { … }` does it for a name today.
- `T?` as the result of a `fn` or the type of a lambda's parameter.
- `optional-var` was written with `picked ?? "none"` on an `Int?`, which the neighbouring
  `otherwise-default-of-the-wrong-type` refuses for the same shape; its default is `0` now,
  and `??` stays strict.

### 14. `match`, and enums that carry values

*Done.* `enum-with-data` holds, and so do the twenty-one cases added with it.
What stays open:

- A name a pattern binds, `.failed(message)`, is not told when it is never read, as an unused
  `let` is (`UnusedVariable`).
- A case with values is a record at run time, `{ case: "failed", message: … }`, and a reply from
  a controller or JSON with that shape is taken as that case without being checked against the
  fields it declares. Step 19, with the other data from outside, is where it is.

### 16. Functions that scale

*Done.* `function-with-a-block-body`, `function-named-arguments` and `generic-function` hold, and
so do the twenty-two cases added with them. What stays open:

- Type parameters on `type`, `type Box<T> = { value: T }`. A type the checker knows is a
  name with no arguments, so a generic one needs the checker to carry them.
- Default values on a `fn`, `fn total(items: List<Item>, tax: Float = 0.0)`. The parameter
  is parsed and not used.
- A function that reads a name it cannot reach is told twice, `UnknownName` and
  `ImpureFunction`, for one mistake. It was before this step.

### 17. Collections

*Done.* `map-type`, `list-helpers` and `list-insert-and-update` hold, and so does
`index-assignment`, left from step 9, with the forty-one cases added with them. What stays open:

- The other list methods the proposal names: `take`, `skip`, `flatMap`, `groupBy`, `unique`,
  `zip`, `chunk` and `enumerate`.
- A lookup in a `Map` or a `Set` searches what was added, as `find` does. The order they keep
  is what the reference promises; a hash for the keys that can be hashed would make the lookup
  fast without changing it.
- `m.keys()`, `m.values()` and a `Set` operation such as `union` do not exist. A `for` goes over
  a map for its keys.
- `var-map-type-does-not-exist` said `Map` was not a type; it is one now, so it asks the same of
  `Dict`.

### 18. Types for what is an `Int` today

*Done, in edition 2027.* `colour-type-shows-as-a-colour` and `duration-type-keeps-its-unit` hold
with `//# edition: 2027`, and so do the twenty-one cases added with them; the baseline edition
is as it was (`colour-is-still-a-number-before-2027`). What stays open:

- `Color.mix`, `Color.lighten` and `Color.rgb(r, g, b)` from the proposal, and a colour
  written as `#3B82F6`. `alpha` is the one method a `Color` has.
- A colour is still told apart from an `Int` only by the type written for it: `0xFF0000` is
  a number until it is given to a `Color`. A text shows it as a colour only where the checker
  knows the type, so a colour that went through a function or a list of `Unknown` shows as a
  number.
- `byard fix` writes nothing for this change: no rewrite can tell which `Int` was a colour.

### 19. Data from outside, with a type

*Done.* `typed-json` holds, with the eight cases added with it and a Rust test file for the half the
suite cannot run (`typed_reply.rs`: a call that waits for its reply). What stays open:

- `as` checks a record, a list or a scalar, and a record's optional fields; it does not check an
  enum case or a `Map` against what arrives, and a number written as text is not told apart
  from a number.
- `untyped-reply` is a lint of the field read: `res.json` and `res.status` are both reads of the
  reply and both told. A reply passed whole to a function is not a read.
- The three examples that read replies, `aura_weather`, `weather` and `inventory`, now give
  theirs a type.

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
