# RFC-0047: Text methods and the remainder operator

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-10-26
- **Last updated:** 2026-10-26
- **Depends on:** RFC-0027 (data and collection operations; §4 defines the
  closed method set this extends), RFC-0025 (looping animations, which owns the
  `50%:` keyframe step this has to leave alone), RFC-0044 (the formatter).

> **Placement:** `docs/rfcs/0047-text-and-remainder.md`

---

## Summary

`byld` gets the `%` operator, nine methods on `Str` (`trim`, `toLower`,
`toUpper`, `contains`, `startsWith`, `endsWith`, `replace`, `split`,
`indexOf`) and two on `List` (`reverse`, `join`). They are what an app needs to
do what every app does with a string: clean what the user typed, test it, take
it apart and put it back together. The set stays closed (RFC-0027 §4), every
method is pure and total, and the checker types all of them, so a typo in one
is `UnknownMethod`, not a quiet nothing. `%` binds like `*` and `/`, and like
`/` it answers `0` for a zero divisor. This RFC also decides what `10%3` means
now that `%` is an operator and `50%` is a literal.

## Motivation

Today a `Str` has `slice`, `urlEncoded` and `formatDate`, and `.len`. Nothing
else. A search box that ignores case has to be written in Rust, in a
controller, to compare two strings. `examples/aura_weather` keeps five
parallel lists because there is no way to split `"Mon,Tue,Wed"`. A clock face
cannot wrap hours without a remainder, and a striped list cannot alternate its
rows:

```byld
for row, i in rows {
    Row #[bg: (i % 2 == 0 ? 0x1E1E28 : 0x242430)] { Text(row.name) }
}
```

The interpreter's answer to a method it does not know is to do nothing, which
is why RFC-0027 closed the set and `UnknownMethod` exists. That same rule means
the missing methods are not merely absent: the one an author reaches for first,
`"hello".contains("h")`, is reported as a mistake. The gap is a sentence the
language cannot say, not a feature nobody asked for.

## Guide-level explanation

```byld
View Search() {
    var query = ""
    var names = ["Ada Lovelace", "Alan Turing", "Grace Hopper"]
    let wanted = query.trim().toLower()
    let shown = names.filter(|n| n.toLower().contains(wanted))

    Column {
        TextField #[bind: query, placeholder: "Search"]
        Text(shown.join(", "))
    }
}
```

| Form | Result | Notes |
|---|---|---|
| `s.trim()` | `Str` | Drops white space at both ends. |
| `s.toLower()` / `s.toUpper()` | `Str` | Full Unicode case mapping, in no locale. |
| `s.contains(t)` | `Bool` | `true` when `t` is empty. |
| `s.startsWith(t)` / `s.endsWith(t)` | `Bool` | `true` when `t` is empty. |
| `s.replace(a, b)` | `Str` | Every occurrence of `a`, left to right. An empty `a` replaces nothing. |
| `s.split(sep)` | `List<Str>` | `"a,,b".split(",")` is `["a", "", "b"]`; `split("")` is one entry per character. |
| `s.indexOf(t)` | `Int` | In characters, as `len` and `slice` count; `-1` when absent. |
| `xs.reverse()` | `List` | A new list. |
| `xs.join(sep)` | `Str` | Each element as `"{x}"` writes it. |
| `a % b` | `Int` or `Float` | Remainder; `a % 0` is `0`. |

A few results to have in front of you:

```byld
"  hi  ".trim()              // "hi"
"Straße".toUpper()           // "STRASSE"  (the length changes)
"a-b-c".replace("-", "")     // "abc"
"héllo".indexOf("l")         // 2          (characters, not bytes)
"".split(",")                // [""]
"abc".split("")              // ["a", "b", "c"]
[1, 2, 3].reverse().join("") // "321"
-7 % 3                       // -1
7 % -3                       // 1
7.5 % 2                      // 1.5
10%3                         // 1          (see §4)
```

## Reference-level explanation

### 1. The remainder operator

`%` is a binary operator with the precedence and associativity of `*` and `/`
(left, one level above `+` and `-`). It matches `/` in everything but the
arithmetic:

- `Int % Int` is `Int`, computed as Rust's `i64::wrapping_rem`: the result
  takes the sign of the dividend (`-7 % 3 == -1`, `7 % -3 == 1`), and
  `i64::MIN % -1` is `0` rather than a panic.
- With a `Float` on either side the other is widened and the result is Rust's
  `f64 % f64` (`fmod`, the same sign rule).
- A zero divisor answers `0` (`0.0` for floats), never a trap and never `NaN`,
  exactly as `/` does (`1 / 0` is `0`). One rule for both is easier to hold in
  the head than two, and a divisor that comes from a `var` the user can set to
  zero is the normal case: an app must not crash because a field was cleared.
- The checker types it as `-`, `*` and `/` are typed: both operands numeric,
  `Int % Int` is `Int`, anything with a `Float` is `Float`, and a concrete
  non-number is `TypeMismatch { op: "%" }`.

Remainder, not modulo: the sign follows the dividend, as in Rust, C, Java and
JavaScript, and unlike Python's `%`. A calendar that wants `(day - 1) % 7` to
be non-negative for `day == 0` writes `((day - 1) % 7 + 7) % 7`, and the guide
says so. There is no `%=`; `+=` and `-=` are the only compound assignments
today and this RFC does not widen them.

### 2. Text methods

All are pure, take their arguments by value, and are total: a receiver or
argument of the wrong type at run time (only possible where the checker could
not see the type) gives a neutral answer (`false`, `[]`, `-1`, or the receiver
unchanged) rather than a panic, the rule `slice` already follows.

- **`trim()`** removes the Unicode `White_Space` characters from both ends
  (Rust's `str::trim`). The inside of the string is untouched.
- **`toLower()`, `toUpper()`** use Unicode's full case mapping, not the simple
  one: `"ß".toUpper()` is `"SS"`, and a final sigma lowers to `ς` (Rust's
  `str::to_lowercase`/`to_uppercase`). The mapping is locale-independent:
  there is no Turkish dotted-`i` mode, because the value of an expression must
  not depend on the machine it runs on. The consequence authors must know is
  that the output can be longer than the input, so `.len` may change.
- **`contains(t)`, `startsWith(t)`, `endsWith(t)`** compare scalar values
  exactly, with no normalisation and no case folding: `"é"` as one character
  and as `e` plus a combining accent are different strings, and a
  case-insensitive search is `a.toLower().contains(b.toLower())`. An empty `t`
  is contained in, a prefix of and a suffix of every string.
- **`replace(a, b)`** replaces every non-overlapping occurrence of `a`,
  scanning left to right (`"aaa".replace("aa", "b")` is `"ba"`). An empty `a`
  replaces nothing: Rust inserts `b` between every character instead, which
  nobody writing `replace("", x)` asked for. All occurrences, not the first,
  because the first-only form is the one callers have to loop to extend.
- **`split(sep)`** splits at every occurrence of `sep` and keeps empty pieces:
  `"a,,b".split(",")` is `["a", "", "b"]`, `"".split(",")` is `[""]`,
  `",".split(",")` is `["", ""]`. The exception is the empty separator, which
  would otherwise yield an empty string at each end: `split("")` returns one
  entry per Unicode scalar value (`"héllo".split("")` has five) and
  `"".split("")` is `[]`. Scalar values, not grapheme clusters: a flag emoji or
  a letter with a combining accent is more than one entry. That is the unit
  `len` and `slice` count in, and cutting by grapheme would put Unicode's
  segmentation tables in every app's binary for a case the other methods do not
  share.
- **`indexOf(t)`** is the index, in characters, of the first occurrence of `t`,
  or `-1`; `indexOf("")` is `0`. It is the index `slice` takes, so
  `s.slice(s.indexOf(t))` begins at `t`.

`contains` and `indexOf` already exist on `List`. The receiver's type picks the
meaning: the checker dispatches on the type it has, and the interpreter on the
value, as `slice` already does for both.

### 3. `List.reverse` and `List.join`

`xs.reverse()` returns a new list with the elements in the opposite order, like
every list method (RFC-0027 §4: values, never mutation). `xs.join(sep)` is a
`Str`: each element is written the way `"{x}"` writes it (the shared scalar
formatter), separated by `sep`. A record, a list or a lambda has no scalar form
and contributes the empty string, as it does in an interpolation; `join` on an
empty list is `""`.

### 4. The lexer, and `10%3`

`50%` is already a token, the percent literal of RFC-0025 §4, canonicalised to
the fraction `0.5`, and the lexer takes the longest match, so `10%3` lexes as
the literal `10%` followed by `3`. That expression has not been written yet and
will be the first one anybody writes: ten mod three. Left alone it would be a
parse error pointing at a `3`, for a program that says exactly what it means.

The rule: **a percent literal is a percent literal only where a `:` follows
it**, the only place the grammar accepts one (`keyframe_step := PERCENT ":"
expr`). Anywhere else `N%` is the number `N` and the operator `%`. It is applied
once, to the token stream, after lexing, by both lexers (the parser's `lex` and
the formatter's `lex_with_trivia`): each percent literal not followed by a `:`
is split back into the number it was written from (an `Int`, or a `Float` when
it has a fraction) and a `Percent`, with the span divided between them.

| Source | Meaning |
|---|---|
| `10 % 3`, `10%3`, `x%3`, `(a + 1)%n` | remainder |
| `1.5%2` | `1.5 % 2` |
| `anim.keyframes(0%: 0, 50%: 1, 100%: 0)` | percent literals, as before |
| `50% :` | a percent literal (white space is not a token) |

Nothing that parses today changes meaning: a percent literal outside a keyframe
step is not accepted now, so no program has one. The formatter prints every
binary operator with a space either side, so `10%3` is saved as `10 % 3`, and
`50%:` is saved as it was.

### 5. The checker

`STR_METHODS` grows the nine names and `LIST_METHODS` the two, so the
`UnknownMethod` suggestion draws on them. Result types: `trim`, `toLower`,
`toUpper`, `replace` are `Str`; `contains`, `startsWith`, `endsWith` are `Bool`;
`split` is `List<Str>`; `indexOf` is `Int` for both receivers; `join` is `Str`;
`reverse` is the receiver's own list type. Arguments are not type-checked, as
none of the existing methods' are; a wrong one gives the neutral answer above.

## Drawbacks

- **Eleven names and an operator** in a language whose method set was closed to
  stay small. Each is pure, adds no syntax, and costs nothing in an app that
  does not call it: an arm in a `match` on the method name, chosen when the
  expression is lowered.
- **Full Unicode case mapping** needs no new dependency (it is in `std`) but
  makes `toUpper` change lengths. ASCII-only would be wrong for the names and
  words an app shows.
- **`split("")` and `replace("", x)` are chosen, not derived.** Rust's
  behaviour is the other option for both; the rule used is the one a reader
  would predict from the table.
- **The token-stream pass** is one more place that knows what a percent literal
  is, in two lexers. It is small and sits beside them; the alternative is two
  grammars that disagree about `%`.

## Rationale and alternatives

- **Do nothing and keep these in controllers.** A controller is an async hop to
  Rust for `a.toLower()`. RFC-0028 exists for effects, not for string functions
  a search box would wait on every keystroke.
- **A `String` module of free functions** (`lower(s)`). Methods are how `slice`
  and `urlEncoded` are written, how a pipeline reads left to right, and what the
  `UnknownMethod` hint can suggest from. A second spelling would be the first
  thing to disagree.
- **Python's `%`** (sign of the divisor) is the better modulo for calendars and
  the worse remainder for everything that matches Rust and C. The sign is
  documented and the workaround is one line, but it is the one choice here that
  is hard to change later without breaking programs.
- **`x % 0` as a diagnostic or a trap.** A constant zero divisor could be a
  compile error, but a zero from a `var` cannot, and two behaviours are worse
  than one.
- **A distinct token for the percent suffix**, reachable only inside
  `anim.keyframes(`. The lexer has no such context; this pass is the smaller
  seam.

## Prior art

- **Rust's `str`** supplies every definition except where this RFC says
  otherwise (`replace("", …)` and `split("")`), so the interpreter's
  implementation is the specification.
- **JavaScript** has `"abc".split("")` as characters; its `indexOf` counts UTF-16
  units, the mistake `len` and `slice` already avoided here.
- **Python** raises on `split("")`. A language where user data never panics
  (RFC-0027) cannot.
- **Dart, Kotlin and Swift** document `%` as a remainder with the dividend's
  sign, with a separate `floorMod`/`mod` for the other.

## Resolved questions

### Q1, does `%` take the sign of the dividend or of the divisor?

**Question.** `-7 % 3` is `-1` (dividend) or `2` (divisor).

**Options.** (a) the dividend's, as Rust; (b) the divisor's, as Python.

**Resolution: (a).** It is the operation the hardware, Rust and the languages
this evaluator may be ported to compute, so the interpreter and a compiled form
cannot disagree. The cost is a sentence in the guide and one line of arithmetic
for the cases that want a non-negative result.

### Q2, what is `x % 0`?

**Question.** A trap, `NaN`, or a number.

**Options.** (a) an error value; (b) `0`; (c) `NaN` for floats and `0` for ints.

**Resolution: (b).** `/` already answers `0`, and a user-controlled divisor
makes the zero case a normal input. Two rules for two operators of the same
family would be remembered as none.

### Q3, what do `split("")` and `replace("", x)` do?

**Question.** Rust gives results with empty pieces at both ends for both.

**Options.** (a) Rust's; (b) `split("")` is the characters and `replace("", x)`
is the identity; (c) both are diagnostics.

**Resolution: (b).** The empty-separator split is the idiom for "each
character" in the language most authors know best, and the Rust result has
empty pieces nobody wants. An empty pattern matching between every character is
an artefact of how a matcher is written; "replace nothing" is what the words
say. Neither is worth a diagnostic, since the argument may come from a `var`.

### Q4, in what unit does `indexOf` count?

**Question.** Bytes, UTF-16 units, scalar values, or grapheme clusters.

**Options.** (a) bytes; (b) scalar values; (c) grapheme clusters.

**Resolution: (b).** `len`, `slice` and `split("")` already count scalar values,
so `s.slice(s.indexOf(t))` and `s.len` agree with it. Bytes would hand authors an
offset that misplaces `slice` on any non-ASCII text. Graphemes are the right
unit for caret movement and the wrong one to pay for in every binary for a
search function; `TextField` has them where they matter (RFC-0041).

### Q5, what does `10%3` do, now that `50%` is a literal?

**Question.** The lexer's longest match turns `10%3` into a percent literal and
a number.

**Options.** (a) leave it, a parse error; (b) require spaces, and report `10%3`
with a hint; (c) read a percent literal as one only before a `:`, and as a
number and an operator everywhere else.

**Resolution: (c).** The program has one reasonable reading, and (a) and (b)
punish it. No program that parses today has a percent literal outside a
keyframe step, so nothing changes meaning, and the formatter normalises the
spelling. The test that holds it is the keyframe case: it must lex as it did.

## Future possibilities

- A `mod` method for the divisor's-sign case, if the workaround is common.
- `parseInt`/`parseFloat`, which want the optional type the language does not
  have as a return yet.
- `padStart`, `lastIndexOf`, `repeat`: the rest of what a text-heavy app asks
  for, one at a time, each against this RFC's rules.
- Type-checking the arguments of the closed set, which applies to `slice` and
  `push` as much as to these.
- `List.zip`, ranges and `enumerate`: a separate decision about iteration, not
  about text.
