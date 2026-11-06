# RFC-0048: Numbers from text

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-05
- **Last updated:** 2026-11-05
- **Depends on:** RFC-0027 (data and collection operations; §4 defines the
  closed method set this extends), RFC-0047 (text methods and `%`, which left
  `parseInt`/`parseFloat` as a future possibility), RFC-0044 (the formatter).

> **Placement:** `docs/rfcs/0048-numbers-from-text.md`

---

## Summary

`byld` gets two methods that turn text into a number, `Str.toInt()` and
`Str.toFloat()`, each answering the number or *nothing* when the text is not
one, so `qty.toInt() ?? 0` is the whole of reading a form field. Numbers get
the handful of methods that go with them: `round()`, `floor()` and `ceil()` on
`Float` (each gives an `Int`), `toFloat()` on `Int`, and `abs()`, `min(other)`
and `max(other)` on both. An interpolation gets a grouping flag, `{x:,}` and
`{x:,.2}`, that writes a comma between every three digits of the integer part.
The set stays closed (RFC-0027 §4), every method is pure and total, and the
checker types all of them, so a typo is `UnknownMethod`, with its suggestion.
Arithmetic is not touched; what it does on overflow is written down as an open
question.

## Motivation

A `TextField` binds a `Str`. Every form that takes a quantity, a price or an
age therefore has to get from `"12"` to `12`, and today it cannot. The
inventory app (`examples/inventory`) needs it on every screen, and writes it
out by hand in `numbers.byd`:

```byld
fn isWhole(s: Str) -> Bool => s.len > 0 && s.split("").filter(c => "0123456789".indexOf(c) < 0).len == 0
fn toInt(s: Str) -> Int => isWhole(s) ? s.split("").reduce(0, (acc, c) => acc * 10 + "0123456789".indexOf(c)) : 0
```

That is slow to write and slower to read, and it is wrong in three ways the
author had to accept. It cannot read a negative number. It reads `"1_000"` as
nothing only because `_` is not a digit. And it stops at nine digits, because
`Int` arithmetic wraps silently on overflow: a 23-digit field read back as
`200376420520689663`, a plausible-looking number that is simply wrong. A parser
in the engine has none of these problems, since it can refuse a value that does
not fit instead of wrapping it.

The other half is what to do with the number. A price typed as `4.567` is shown
with `{price:.2}`, but a quantity cannot be clamped to zero without a `fn`
(`n < 0 ? 0 : n`), a `Float` cannot be turned into the `Int` a list index or a
`.slice` wants, and a total of `193440.7` is shown as `193440.70` where every
reader expects `193,440.70`. None of these is a feature nobody asked for: each
is a sentence an app of any size has to say and the language cannot.

## Guide-level explanation

```byld
View Order() {
    var qty = ""
    var price = ""
    let n = qty.toInt() ?? 0
    let unit = price.toFloat() ?? 0.0
    let total = n.max(0) * unit

    Column {
        TextField #[bind: qty, placeholder: "Quantity"]
        TextField #[bind: price, placeholder: "Unit price"]
        Text("{n} x {unit:,.2} = {total:,.2}")
    }
}
```

| Form | Result | Notes |
|---|---|---|
| `s.toInt()` | `Int` or nothing | A whole number: optional sign, ASCII digits. |
| `s.toFloat()` | `Float` or nothing | Also a fraction and an exponent. |
| `x.round()`, `x.floor()`, `x.ceil()` | `Int` | On a `Float`. Nothing for `NaN` or out of range. |
| `n.toFloat()` | `Float` | On an `Int`. |
| `x.abs()` | same type | On both. |
| `x.min(y)`, `x.max(y)` | `Int`, or `Float` if either is | On both. |
| `{x:,}`, `{x:,.2}` | text | Commas between thousands; see §4. |

What `toInt` and `toFloat` accept, and what they answer nothing to:

```byld
"12".toInt()          // 12
" -3 ".toInt()        // -3         (white space around it is ignored)
"+4".toInt()          // 4
"007".toInt()         // 7
"1_000".toInt()       // nothing
"0x10".toInt()        // nothing
"1.5".toInt()         // nothing    (it is not a whole number)
"9223372036854775808".toInt()  // nothing  (one past the largest Int)
"".toInt()            // nothing
"abc".toInt()         // nothing

"3".toFloat()         // 3.0
"1e3".toFloat()       // 1000.0
".5".toFloat()        // 0.5
"5.".toFloat()        // 5.0
"-0.25".toFloat()     // -0.25
"inf".toFloat()       // nothing
"1,5".toFloat()       // nothing    (the decimal mark is a point)
```

Rounding goes away from zero on a half, and a grouped number reads the way a
person writes it:

```byld
(2.5).round()     // 3
(-2.5).round()    // -3
(-2.5).floor()    // -3
(-2.5).ceil()     // -2
(-5).abs()        // 5
(7).max(10)       // 10
193440.7          // "{x:,.2}" is 193,440.70
-1234             // "{x:,}" is -1,234
```

Nothing is not an error. It prints as an empty text, `??` gives it a default,
and arithmetic with it is nothing again (`"x".toInt() + 1` is nothing), which
is the rule `find` and `min` on an empty list already follow.

## Reference-level explanation

### 1. `Str.toInt()`

The receiver is first trimmed of Unicode `White_Space` at both ends (the
`trim` of RFC-0047). What is left must be: an optional single `+` or `-`, then
one or more ASCII digits `0` to `9`, and nothing else. The value is the base-10
number those digits spell, negated for `-`. It is `Int` (`i64`) when it fits
and **nothing** when it does not: a value outside `-9223372036854775808` to
`9223372036854775807` is nothing, never a wrapped number. Leading zeros are
allowed (`"007"` is `7`), `"-0"` is `0`.

Everything else is nothing: the empty text, a sign alone, `"1_000"`, `"0x10"`,
`"1.5"`, `"1e3"`, `"1,000"`, `"٣"` (a digit of another script), and any text
with something between the sign and the digits (`"- 3"`). It is Rust's
`str::parse::<i64>` applied to the trimmed text, and that is its specification.

A receiver that is not text (only possible where the checker could not see the
type) gives nothing, the rule the other text methods follow.

### 2. `Str.toFloat()`

The receiver is trimmed as above. What is left must be a decimal floating-point
number in this form: an optional sign, then digits with an optional `.` and
digits after it, with at least one digit on one side of the point (`".5"` and
`"5."` are accepted, `"."` is not), then an optional exponent, `e` or `E`, an
optional sign and one or more digits. A text that spells a whole number is
accepted and is the `Float` of it (`"3"` is `3.0`).

Not accepted, so nothing: `inf`, `infinity`, `nan` in any case; `,` as the
decimal mark (`"1,5"`); `_`; hex; a trailing or a doubled `e`; and a number
whose magnitude overflows to infinity (`"1e999"`). The result of `toFloat` is
therefore always finite. To keep Rust's parser from accepting the words, the
text is first checked to hold only `0-9 . e E + -`, and then handed to
`str::parse::<f64>`, which supplies the rounding to the nearest `f64`.

`toFloat` on `"3"` and `toInt` on `"3"` are both accepted and differ only in
type. `"1.5".toInt()` is nothing and `"1.5".toFloat()` is `1.5`. An app that
wants a whole number from decimal text writes `(s.toFloat() ?? 0.0).round()`.

### 3. Methods on numbers

All are pure and total. The receiver's type picks the meaning, as for `slice`
and `contains`: the checker dispatches on the type it has, and the interpreter
on the value.

- **`round()`, `floor()`, `ceil()`** on `Float` give an `Int`. `round` rounds a
  half **away from zero**: `2.5` is `3` and `-2.5` is `-3` (Rust's
  `f64::round`). It is the rule most people expect, and the one that makes
  `x.round()` and `(-x).round()` mirror each other. It is not the "banker's"
  rounding of Python's `round` or `Math.rint`. `floor` is toward negative
  infinity (`-2.5` is `-3`) and `ceil` toward positive (`-2.5` is `-2`). The
  result is **nothing** for `NaN` and for a value that does not fit an `Int`
  (magnitude of `2^63` or more), never a saturated number. On an `Int` these
  three are not defined: the checker says `UnknownMethod`, since an `Int` is
  already whole.
- **`toFloat()`** on `Int` is the `Float` of it (`i64 as f64`; above `2^53` it
  rounds to the nearest representable number).
- **`abs()`** keeps the type. `Int` uses `wrapping_abs`, so `abs` of the
  smallest `Int` is itself, consistent with arithmetic wrapping today (see
  Unresolved).
- **`min(other)`, `max(other)`** are the smaller and larger of the receiver and
  the argument. `Int` with `Int` is `Int`; if either is a `Float` the other is
  widened and the result is `Float`, as in arithmetic. A `NaN` argument gives
  the other value. An argument that is not a number gives the receiver.
  `List.min()`/`List.max()` keep their meaning: the same names, told apart by
  the receiver and by the argument.

The argument list of `min`/`max` on a list is empty and on a number has one
entry; the interpreter looks at the value, and the checker at the receiver's
type.

### 4. Grouping in an interpolation

An interpolation `{expr}` may end in a spec, today `:.N` (RFC-0027). The spec
becomes: an optional `,`, then an optional `.N`, and at least one of the two:
`{x:.2}`, `{x:,}`, `{x:,.2}`. `,.2` is the order; `.2,` is not a spec, so the
colon stays part of the expression and the usual error follows.

`,` inserts a comma between every three digits of the **integer part**, counted
from the decimal point. Nothing else changes: the sign stays first, the
fraction and its digits are as `.N` (or the plain `{x}`) write them, and the
comma is always `,` with the point always `.`. There is **no locale**, for the
reason `formatDate` has none: the value of an expression must not depend on the
machine it runs on, and a Spanish reader's `193.440,70` is a presentation an
app can ask for with `replace` until a locale RFC exists.

| Value | Spec | Shows |
|---|---|---|
| `193440.7` | `,.2` | `193,440.70` |
| `-1234` | `,` | `-1,234` |
| `0` | `,` | `0` |
| `999` | `,` | `999` |
| `1000` | `,` | `1,000` |
| `1234567` | `,` | `1,234,567` |
| `1234567.891` | `,` | `1,234,567.891` |
| `-0.2` | `,.0` | `0` (no sign on a zero, as `.N` already does) |
| `"abc"` | `,` | `abc` (not a number, as with `.N`) |
| nothing | `,` | empty |

It works for `Int` and `Float`. A `NaN` or infinite `Float` is written as it
is, ungrouped. A value that is not a number is written as `{x}` would write it,
exactly as `.N` does.

**Where it is parsed.** The splitter of interpolations (`split_decimals` in the
parser) finds the last `:` outside brackets and strings, and reads what follows
as a spec. It now reads `,` and `.N` and carries them in the AST as a small
`NumFormat { group, decimals }` in place of the bare `Option<u8>`. The expression
still ends at the colon. A `:` followed by anything else is not a spec, so
`a ? b : c` is never split, as before. The interpreter applies it where it
already applies the decimals: grouping runs over the text `{x:.N}` produces, so
the two cannot disagree about rounding.

**The formatter** (RFC-0044) keeps a string literal verbatim; it never looks
inside `{ }`. `{x:,.2}` is therefore preserved byte for byte, and a test holds
it.

### 5. The checker

`STR_METHODS` grows `toInt` and `toFloat`, and two new lists are added,
`INT_METHODS` (`toFloat`, `abs`, `min`, `max`, and `alpha`, which colours
already use) and `FLOAT_METHODS` (`round`, `floor`, `ceil`, `abs`, `min`,
`max`). A method not in the receiver's list is `UnknownMethod` with the closest
name as the suggestion, where before a method on a number was not checked at
all. Result types:

| Receiver | Method | Type |
|---|---|---|
| `Str` | `toInt` | `Int` |
| `Str` | `toFloat` | `Float` |
| `Float` | `round`, `floor`, `ceil` | `Int` |
| `Int` | `toFloat` | `Float` |
| `Int`, `Float` | `abs` | the receiver |
| `Int`, `Float` | `min`, `max` | `Int` if both are, else `Float` |

`toInt` is typed `Int`, not an optional: the language has no optional type, and
the value of `find` and `min()` that may also be nothing is likewise just the
element's type, or unknown. So `"3".toInt() + 1` is Int arithmetic and
type-checks; `"3".toInt() ?? 0` is `Int` with an `Int` default; and
`"3".toInt() ?? "none"` is a `TypeMismatch`, "a default of the wrong kind", as
the guide already says for `??`. The cost is that the checker cannot tell
`toInt() + 1` from a definite `Int`; the run-time result is nothing, and the
reader sees an empty text where the number should be.

## Drawbacks

- **Nine names and a spec flag** in a language whose method set was closed to
  stay small. Each is pure, adds no syntax beyond the `,` in a spec, and costs
  nothing in an app that does not call it: an arm in a `match` on the method
  name, chosen when the expression is lowered.
- **Nothing rather than a failure.** `"abc".toInt()` is quietly nothing, and
  `nothing + 1` is nothing. That matches the rest of the language (RFC-0027:
  user data never panics) and `??` is one token, but a forgotten `??` shows an
  empty text. The alternative, a typed optional, is a language change of its own
  (Alternatives).
- **`min`/`max` mean two things**, the extremum of a list and of two numbers,
  told apart by their receiver. It is how `contains` and `indexOf` already work.
- **No locale.** `{x:,}` is wrong for a reader who expects `.` between
  thousands. It is wrong in the same way, and for the same reason, every other
  formatted value in the language is: there is no machine-dependent output.
- **A newly checked receiver.** A method on an `Int` or `Float` that no list
  names is now `UnknownMethod`. It had no meaning before (the interpreter
  answered nothing), so no working program is affected.

## Rationale and alternatives

- **Do nothing and keep `numbers.byd`.** It is nine digits long, reads no sign,
  and every app copies it. The engine can refuse an out-of-range value; a user
  `fn` cannot, since the arithmetic it is built from wraps.
- **`parseInt(s)` / `Int.parse(s)`, free functions.** Methods are how `slice`
  and `formatDate` are written, how a pipeline reads left to right
  (`field.trim().toInt() ?? 0`), and what `UnknownMethod` suggests from. The
  names follow `toLower`/`toUpper`.
- **A default argument, `s.toInt(0)`.** Shorter, but it gives a second way to
  spell "or" beside `??`, and it cannot tell "was zero" from "was not a number"
  when the caller wants to show the invalid style. `formatDate` already
  answers nothing, so the same form applies here.
- **`toInt` as `0` for text that is not a number.** What `numbers.byd` does.
  It makes `"abc"` and `"0"` the same, so a form cannot say "not a number".
- **A typed optional (`Int?`).** The right long-term answer, and the reason
  nothing here is called `Option`. It needs syntax, narrowing and a decision
  about every method, and `??` plus the inferred type give most of it for now.
- **Leniency in `toInt`.** Accepting `"1_000"`, `"1,000"`, `"0x10"` or `"1.9"`
  (truncated) would make one reader's field act differently from the next's;
  `"1.9".toInt()` quietly being `1` is the bug a price field would ship with.
  Strict is easy to loosen and impossible to tighten.
- **`toFloat` accepting `,` as a decimal mark.** Locale again, and `"1,5"` could
  as easily be a list item or a thousands group. The decimal mark is a point.
- **`Float.round(digits)` returning a `Float`: not included.** It reads well,
  `price.round(2)`, but it does not do what it says. `2.675.round(2)` is
  `2.67` or `2.68` depending on the binary neighbour of `2.675`, and its
  result, `2.68`, is still a binary number that prints as `2.68` only because
  the printer is kind: `0.1 + 0.2` after rounding is not `0.3`. Two methods
  with one name (`round()` gives an `Int`, `round(2)` a `Float`) also make the
  result type depend on the argument count. What a person wants from
  `round(2)` is either to *show* two decimals, which is `{x:.2}`, or to keep
  money exactly, which is an `Int` of cents and `round()`. Both are available.
  It can be added later without breaking anything; taking it out could not.
- **`round()` to the nearest even.** Statistically neutral and what IEEE says,
  but `2.5` becoming `2` surprises everyone who is not a numerical analyst.
- **`floor` and `ceil` returning `Float`**, as in JavaScript. Then `xs[n.floor()]`
  and `s.slice(n.ceil())` need a second conversion, and the point of the method
  is to leave floating point.
- **A saturating `round()`** (a huge `Float` to the largest `Int`). It would
  turn a bad input into a believable number, the same failure as wrapping.

## Prior art

- **Rust**: `str::parse::<i64>` is the definition of `toInt`; `f64::round`
  (half away from zero), `floor`, `ceil`, `abs`, `min`, `max` are the
  definitions of the rest, so the interpreter's implementation is the
  specification. Rust's `as i64` on a float saturates and maps `NaN` to `0`;
  this RFC deliberately does not.
- **Kotlin** has `toIntOrNull()` and `toDoubleOrNull()`: the same
  "number or nothing" shape, which `?:` then defaults.
- **Swift** has `Int("12")`, failable and strict (no sign-space, no separators),
  and `Double("1e3")`; `Double("nan")` and `Double("inf")` are accepted there,
  which this RFC does not copy.
- **JavaScript** has `parseInt("12px")` (12) and `Number("")` (0): lenient
  readers whose results are the source of a well-known class of bugs.
  `toFixed`, `toLocaleString` is the grouping story there, and it is
  locale-dependent.
- **Python** raises `ValueError` on bad input; a language where user data never
  panics (RFC-0027) cannot. Its format language has `{:,.2f}`, the origin of the
  spelling here: `,` for the grouping, then the precision.
- **C#/.NET** `int.TryParse`: a boolean and an out parameter. The optional
  value is the same idea without the out parameter.

## Resolved questions

### Q1, what does `toInt` do with text that is not a whole number?

**Question.** A default number, a diagnostic, or nothing.

**Options.** (a) `0`; (b) nothing; (c) a typed optional or an error value.

**Resolution: (b).** `formatDate` already answers nothing for text that is not a
date, and `??` is how a default is written. `0` hides the difference between an
empty field and a zero; a typed optional is a larger decision than this RFC.

### Q2, how strict is the text?

**Question.** Surrounding white space, a `+`, `_`, hex, a fraction.

**Options.** (a) Rust's `parse` on the trimmed text; (b) something more
lenient, as JavaScript; (c) no white space either.

**Resolution: (a).** White space is ignored because a pasted number often has
some and no one can see it. A single sign is allowed because `"+4"` and `"-3"`
are numbers. Anything the keyboard layout or a locale could mean differently
(`_`, `,`, `0x`) is rejected, so the same text is the same number on every
machine.

### Q3, what is out of range?

**Question.** `"9223372036854775808".toInt()`, `1e999`, `1e30.round()`.

**Options.** (a) wrap; (b) saturate; (c) nothing.

**Resolution: (c).** Both (a) and (b) answer with a believable number that is
not what was typed. The inventory app's 23-digit field becoming
`200376420520689663` is the case this RFC exists for.

### Q4, which way does `round` break a tie?

**Question.** `2.5` and `-2.5`.

**Options.** (a) half away from zero (`3`, `-3`); (b) half to even (`2`, `-2`);
(c) half up (`3`, `-2`).

**Resolution: (a).** It is the rule most people expect, it is symmetrical, and
it is Rust's `f64::round`, so the interpreter and a compiled form agree. (c)
makes `x.round()` and `(-x).round()` disagree by more than the sign.

### Q5, `Float.round(digits)`?

**Resolution: not now.** See Alternatives. `{x:.2}` shows two decimals and
cents in an `Int` keep money exact.

### Q6, a locale for grouping?

**Resolution: no.** The comma is always `,` and the point always `.`. See §4.

## Unresolved questions

- **`Int` arithmetic overflows silently.** `+`, `-` and `*` use Rust's
  `wrapping_add`, `wrapping_sub` and `wrapping_mul`, so
  `9223372036854775807 + 1` is `-9223372036854775808`, and the 23-digit field
  of the inventory app became `200376420520689663` through repeated
  `acc * 10 + d`. This RFC does not change arithmetic: doing so changes the
  value of expressions that run today, and it belongs to a decision of its own.
  What other languages do, for that decision:
  - **Rust** panics in a debug build and wraps in a release build; the
    `checked_`, `saturating_` and `wrapping_` families make the choice
    explicit.
  - **Swift** traps on overflow, with `&+` for the wrapping operator.
  - **Python** and **Ruby** have integers of unbounded size, so nothing
    overflows.
  - **JavaScript** has no `Int` and loses precision above `2^53`, with
    `BigInt` for the rest.
  - **Java**, **C#** (unchecked by default), **Go** and **C** wrap silently;
    C's signed overflow is undefined behaviour.
  - **Kotlin** wraps; Zig and Ada trap.

  The options for `byld`, in the order of least surprise: a result of nothing
  (consistent with this RFC's `toInt`, and `??` can default it), a saturating
  result, or keeping the wrap and documenting it. `Int.abs()` of the smallest
  `Int`, and `Int.toFloat()` above `2^53`, belong to the same discussion.
- **`NaN` and infinity as values.** `toFloat` never produces them; arithmetic
  can (`0.0 / 0.0` is `0` today, but `f64::MAX * 2.0` is infinity).

## Future possibilities

- A typed optional, `Int?`, with `??` narrowing it, and an optional chain
  (`s.toFloat()?.round()`).
- `Float.round(digits)`, if `{x:.2}` and cents prove not to be enough.
- A locale for `formatDate` and for grouping, together, as one RFC.
- `clamp(lo, hi)`, if `n.max(lo).min(hi)` is common enough to name.
- Checked arithmetic, from the first Unresolved question.
