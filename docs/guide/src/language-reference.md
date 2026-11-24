# Language reference

The chapters before this one teach byld. This one states it: every form the language has,
what it means, and what happens at its edges. Read it when the guide leaves a question open,
such as what `-7 % 3` is, or which of two operators binds tighter.

Three things keep this page true.

- Every `byld` sample here is checked by `cargo test`, as every sample in the guide is.
- The rules are also written as small programs in the *conformance suite*,
  `crates/byard-cli/tests/conformance/`, each with the text it must draw or the error it must
  report. A rule on this page that the suite has a case for names the file.
- Where the language does not yet do what it should, this page says so in a **Not yet**
  note. Each note is a case in the suite marked `gap`, which fails the day the gap is
  closed, so the notes cannot go stale. The order they will be closed in is in
  [`docs/language/roadmap.md`](https://github.com/Briany4717/byard/blob/main/docs/language/roadmap.md).

## Source text

A byld file is UTF-8 text with the extension `.byd`. Lines end in a line feed, or a carriage
return and a line feed.

White space separates tokens and means nothing else. There is no statement separator: a
declaration, an element or a statement ends where the next one begins, so several may share a
line, and a line break is never required.

```byld,fragment
var n = 1 let m = n + 2 Text("{m}")
```

A comment starts at `//` and runs to the end of the line.

`/* … */` is a comment too, and nests, so a block of code can be commented out whatever
comments it holds. One that is never closed is `UnterminatedComment`. A byte order mark at the
start of a file is skipped.

> **Not yet.** `///` is an ordinary comment that no tool reads.

## Words

A name is a letter of the English alphabet or `_`, followed by any number of those and
digits: `count`, `_a1`, `on_tap`. Names are case sensitive. A letter outside the English
alphabet is an `UnexpectedChar` error.

Fifteen words are *keywords* and cannot be names:

```text
View  var  let  fn  inject  as  for  in  when  else  style  untrack  with  merge  use
```

A keyword where a name is wanted is `KeywordAsName`.

Other words mean something only in one place, and are ordinary names everywhere else:
`type`, `enum`, `context`, `private`, `provide`, `bind`, `key`, `on`, `mount`, `unmount`,
`change`, `measure`, `every`, `after`, `route`, `tab`, `if`, `ok`, `err` and `none`.

```byld,fragment
let on = 1
let key = 2
let type = 3
Text("{on} {key} {type}")
```

## Literals

| Kind | Written | Value |
|------|---------|-------|
| Whole number | `42`, `007`, `1_000`, `0xFF`, `0b101` | An `Int`. Decimal, hexadecimal or binary; `_` may separate digits. |
| Fraction | `0.5`, `12.25`, `1e3`, `1.5e-3` | A `Float`. Digits on both sides of the point: `.5` and `5.` are errors. With an exponent it is a `Float` even when it is whole. |
| Truth | `true`, `false` | A `Bool`. |
| Text | `"hello"` | A `Str`. |
| Angle | `90deg`, `1.5rad` | A `Float`, in radians. |
| Duration | `200ms`, `1.4s`, `5min` | An `Int`, in milliseconds: `1.4s` is `1400`. |
| Percentage | `50%` | Only as a step of `anim.keyframes`. |
| Nothing | `none` | Only for an optional parameter. See [optional values](#optional-values). |

An `Int` is a 64-bit signed whole number, from -9223372036854775808 to
9223372036854775807. A literal larger than that is an error. A `Float` is a 64-bit IEEE 754
number.

A `-` before a number is part of the literal: `-3`, `-0.5`, `-90deg`.

```byld,fragment
Text("{0xFF} {1.4s} {5min} {180deg:.2}")
```

That draws `255 1400 300000 3.14`.

A number that runs into letters and makes no literal, `12abc` or `1_000x`, is one
`UnexpectedChar`, and not a number and a name.

### Text

Text is written between double quotes and may span lines; a line break inside the quotes is
part of the text.

| Escape | Is |
|--------|----|
| `\"` | A double quote |
| `\\` | A backslash |
| `\n` | A line break |
| `\t` | A tab |
| `\{`, `\}` | A brace |

Any other character after a backslash is kept with its backslash: `"a\qb"` is `a\qb`.

An expression between braces is *interpolated*: it is evaluated and its text form takes its
place. A text inside the braces is written with plain quotes, and may interpolate in turn.
Text can nest three deep; a fourth level is `StringNestingTooDeep`.

```byld,fragment
var count = 2
Text("{count} {count == 1 ? "item" : "items"}")
Text("a-{"b-{"c"}"}")
```

Empty braces, `"{}"`, are an error. Write `\{\}` for the two characters.

A *format* after a `:` shapes a number:

| Format | Does | `1234567.891` |
|--------|------|---------------|
| `{x:.2}` | Exactly that many decimals, from 0 to 9 | `1234567.89` |
| `{x:,}` | A comma between every three digits of the whole part | `1,234,567.891` |
| `{x:,.2}` | Both | `1,234,567.89` |

An `Int` takes a format too: `{7:.2}` is `7.00`. A format on something that is not a number
is ignored. Rounding is of the number as it is stored, so `{0.125:.2}` is `0.12` and
`{2.675:.2}` is `2.67`. There is no locale: the separator is always `,` and the point
always `.`.

What follows the expression inside the braces is `UnexpectedToken`: `"{1 2 3}"` is an error, a
`{ }` holds one expression. So is more than nine decimals.

> **Not yet.** There is no padding or width. (`15_wanted_for_1_0.byd`)

## Types

| Type | Holds |
|------|-------|
| `Int` | A whole number. A colour is an `Int`, and `Color` is another name for it. |
| `Float` | A number with a fraction |
| `Str` | Text |
| `Bool` | `true` or `false` |
| `List<T>` | Any number of `T`, in order. `List` alone is a list of anything. |
| A record type | Named fields, declared with [`type`](#record-types) |
| An enum | One of a closed set of cases, declared with [`enum`](#enums) |
| `Fn(A, B)`, `Fn(A) -> R` | An action or a function, as a [parameter](#callbacks) |
| `Slot` | Elements a caller writes, as a [parameter](#slots) |
| `T?` | A `T`, or nothing |

`Text` is an element and not a type; writing it where a type goes is `TextUsedAsType`, and
the type meant is `Str`.

Every value is immutable. A list method returns a new list, a record is changed by building
another, and the only thing that changes is which value a `var` holds.

### What has to be written, and what is inferred

- A **view parameter** needs its type (`MissingAnnotation`). The one exception is the slot
  named `content`.
- A **function** needs the type of every parameter and of its result.
- A **`var`**, a **`let`** and a **constant** take the type of their first value: `0` is an
  `Int`, `4.5` a `Float`, `"hi"` a `Str`, `true` a `Bool`, `[1, 2]` a `List<Int>`. A type
  may be written after the name, and has to be when the value says nothing: an empty list,
  or a list of mixed things, is `CannotInfer`.
- A **lambda's parameters** may be typed, and otherwise take their types from where the
  lambda is used.

### What the checker checks

`byard check` checks that:

- every name, view, method, field of a declared record type, enum case, attribute and event
  exists, and suggests the nearest one that does;
- the arguments of a view call are the right number, have the right names and the right
  types, and that a binding is given a `var`;
- the two sides of an operator can be combined (`"a" - 1`, `true + 1` and `1 < 2 < 3` are
  `TypeMismatch`), and that two values compared with `==` are of one type;
- a constant with a written type has a value of that type (`ConstantTypeMismatch`);
- a call to a `fn`, a named action or a callback gives the number of arguments it declares
  (`ArityMismatch`) and each of the type it declares (`ArgumentTypeMismatch`, and
  `CallbackTypeMismatch` for a callback); an `Int` given for a `Float` is a `Float`. A `fn`'s
  body is of the type it says it returns, and a name that is not a function cannot be called;
- a list's item is found by an `Int` (`xs[0.5]` is `TypeMismatch`), `push` adds an item of the
  type the list holds, a lambda given to `map`, `filter`, `find` or `sortBy` takes one
  parameter and one given to `reduce` two, and what `filter` and `find` give back is a `Bool`
  (`PredicateNotBool`). The parameter of such a lambda is of the item's type when the list is
  of numbers, text or truth;
- a record literal given where a declared record is wanted (a `var`'s first value or a later
  one, an item of a list) writes only fields the record has (`UnknownField`), of the types it
  has (`TypeMismatch`), and all the fields it needs, the ones not marked `?`;
- `for` goes over a list;
- every type written is one: built in (`Int`, `Float`, `Str`, `Bool`, `Color`, `List<T>`,
  `Fn(…)`, `Slot`, and the `Screen` and `Window` the language provides) or declared by the
  program as a `type`, an `enum` or a `context`, or another package's as `package.Name`. Any
  other name, `Qux` or `Map<Str, Int>`, is `UnknownType` and suggests the nearest;
- the first value of a `var` or `let` has the type written for it, each item of a list on its
  own (`var n: Int = "a"` and `var xs: List<Int> = [1, "a"]` are `TypeMismatch`);
- what is written to a `var` is a value it can hold: `n = e` a value of its type, `n += e` and
  `n -= e` a number added to a number that keeps its type (`n += 0.5` on an `Int` is not), and
  `n++` and `n--` a number;
- an optional parameter is read only where nothing has been ruled out (`NoneNotHandled`);
- a default given with `??` is of the type it stands in for.

An `Int` may be given where a `Float` is wanted, and is converted: `var w: Float = 3` holds
`3.0`, so `w / 2` is `1.5`, and so does a `Float` `var` that is later written an `Int`. Nothing
else converts.

> **Not yet.** A number of things a typed language checks are not checked, and each is a
> gap in the suite (`12_state.byd`, `08_functions.byd`, `05_records.byd`,
> `07_operators.byd`). The types judged so far are the scalars and lists of them; a record,
> an enum or an optional is let through until its own step:
>
> - two `var`s of one name in a view.
>
> Until they are, such a program runs, and what it draws is whatever the values happen to
> give.

## Values as text

What a value looks like in a text, `"{x}"`, and joined to one with `+`:

| Value | Text |
|-------|------|
| `Int` | Its digits: `-3`, `16711680` |
| `Float` | The shortest digits that give the same number back: `0.1`, `0.30000000000000004`. A whole one has no point: `3.0` is `3`. |
| `Bool` | `true` or `false` |
| `Str` | Itself |
| An enum case | Its name: `medium` |
| A list | From edition 2027, its items as written, `[1, 2, 3]`, a text in it bare: `[a, b]`. Before, nothing. |
| A record | From edition 2027, its fields, `{ name: Ada, score: 3 }`. Before, nothing. |
| Nothing | Nothing at all: an empty text |

A list is also shown with `join`. See [lists](#lists).

> **Not yet.** A colour is shown as its number. (`15_wanted_for_1_0.byd`)

## Expressions

### Operators

From the loosest to the tightest. Operators on one row bind equally and group from the left,
except where noted.

| Level | Operators | Notes |
|-------|-----------|-------|
| 1 | `=` `+=` `-=` | Assignment. Groups from the right. In an [action](#actions) only. |
| 2 | `with` | Animation of an attribute's value |
| 3 | `\|\|` | Or. (In edition 2027 levels 3 and 4 swap: `? :` is looser than `\|\|`.) |
| 4 | `? :` | Choice. Groups from the right. |
| 5 | `&&` | And |
| 6 | `==` `!=` `<` `<=` `>` `>=` | Comparison. Does not chain: `1 < 2 < 3` is an error. |
| 7 | `??` `merge` | A value or its default; one style over another. Group from the right. |
| 8 | `+` `-` | |
| 9 | `*` `/` `%` | |
| 10 | `!` `-` | Not, negation. Before their operand. |
| 11 | `++` `--` | After their operand. In an action only. |
| 12 | `( )` | A call |
| 13 | `.` `[ ]` | A field or a method; an item |

So `1 + 2 * 3` is `7`, `-a * 3` is `(-a) * 3`, `!a && b` is `(!a) && b`, `a ?? 0 + 1` is
`a ?? (0 + 1)`, and `a ?? 0 == 0` is `(a ?? 0) == 0`. Parentheses group as they do anywhere.

In edition 2026 `? :` binds tighter than `||`: `a || b ? x : y` is `a || (b ? x : y)`, which
is not what C or JavaScript make of it, and the checker warns (`MixedTernaryPrecedence`). In
edition 2027 `? :` is looser than `||`, as it is there: `a || b ? x : y` is
`(a || b) ? x : y`. `byard fix` writes the parentheses that keep an old program meaning what
it meant.

### Numbers

`+`, `-`, `*`, `/` and `%` take two numbers. Two `Int`s give an `Int`; if either side is a
`Float` the result is a `Float`.

- **Division of two `Int`s drops the fraction**, towards zero: `7 / 2` is `3` and `-7 / 2`
  is `-3`.
- **`%` is the remainder**, with the sign of the left side: `7 % 3` is `1`, `-7 % 3` is
  `-1`, `7 % -3` is `1`. On `Float`s too: `7.5 % 2` is `1.5`.
- **Division and remainder by zero are `0`**, of an `Int` and of a `Float`, and the program
  goes on. A zero written as the divisor, `n / 0`, is the warning `DivisionByZero`.
- **An `Int` that overflows wraps around** in edition 2026: one more than the largest `Int`
  is the smallest. In edition 2027 it **stops at the largest or the smallest** instead,
  in `+`, `-`, `*`, `/`, `-x`, `abs()`, `+=`, `-=`, `++` and `--`. Neither stops the program.

```byld,fragment
let none = 0
Text("{7 / 2} {-7 / 2} {7 / 2.0} {-7 % 3} {1 / none}")
```

That draws `3 -3 3.5 -1 0`.

`<`, `<=`, `>` and `>=` compare two numbers, an `Int` with a `Float` included: `1 == 1.0` is
`true`.

A zero divisor that is only known when the program runs, and an overflow, are not reported:
only the written zero is. What the roadmap still has to do about them is under step 10.

| Method | On | Result |
|--------|----|--------|
| `abs()` | both | The number without its sign |
| `min(n)`, `max(n)` | both | The smaller, the larger |
| `toFloat()` | `Int` | The same number as a `Float` |
| `round()`, `floor()`, `ceil()` | `Float` | An `Int`. `round` takes a half away from zero: `2.5` is `3`, `-2.5` is `-3`. |
| `sin()`, `cos()`, `sqrt()` | both | A `Float`. The angle is in radians. |
| `atan2(x)` | both | The angle of the point `(x, this)`, a `Float` |
| `alpha(a)` | `Int` | The colour with its opacity set to `a`, from 0 to 1 |

A method of a literal needs parentheses when the literal is negative: `(-3).abs()`. A method
a number does not have is `UnknownMethod`.

### Text

`+` joins two texts, and a text with a number or a `Bool`: `"n = " + 5` is `n = 5`. No other
operator takes a text and a number. `==` and `!=` compare two texts exactly, and `<` and its
kin order them by character code, so `"B" < "a"` and `"10" < "9"` are `true`.

`len` is a field: the number of characters. In edition 2026 a character is a Unicode code
point; from edition 2027 it is what a reader sees as one (a grapheme cluster), so a family
emoji has a `len` of 1, and `slice`, `indexOf` and `split("")` count and cut the same way.
`s[i]` is the character at `i`, or nothing past the end.

| Method | Result |
|--------|--------|
| `slice(from, to)` | The characters from `from` up to, not including, `to`. `to` may be left out. A range past the end is cut short, and an empty or backward range is empty. |
| `trim()` | Without white space at either end |
| `toLower()`, `toUpper()` | In another case, by Unicode with no locale: `"Straße".toUpper()` is `STRASSE` |
| `contains(t)`, `startsWith(t)`, `endsWith(t)` | A `Bool`. `true` for an empty `t`. |
| `replace(a, b)` | Every `a` replaced by `b`. An empty `a` changes nothing. |
| `split(sep)` | The pieces between each `sep`, empty ones kept: `"a,,b".split(",")` is three. An empty `sep` gives one piece per character. |
| `indexOf(t)` | Where `t` first begins, in characters, or `-1` |
| `toInt()` | The whole number it spells, or nothing. White space around it is ignored and a sign is read; a fraction, or a number too large for an `Int`, is nothing. |
| `toFloat()` | The number it spells, or nothing. `NaN` and `inf` are nothing. |
| `urlEncoded()` | Safe to put in a URL's query |
| `formatDate(pattern)` | A date and time, read from ISO text, written by the pattern. See [the clock](data-and-io.md#the-clock). |

```byld,fragment
Text("{"héllo".len} {"héllo".slice(1, 3)} {"12".toInt() ?? 0} {"x".toInt() ?? 0}")
```

That draws `5 él 12 0`.

### Truth

`&&`, `||` and `!` take `Bool`s. `&&` and `||` stop as soon as the answer is known, so the
right side may depend on the left: `xs.len > 0 && xs[0] > 5`. `a ? b : c` evaluates `a`,
then only the arm it picks.

A number, a text, a list or a record where a `Bool` goes (`1 && true`, `!5`) is `TypeMismatch`.
So is the condition of `when`, `if` and `? :`, and so are the two arms of a `? :` of two types,
though an `Int` beside a `Float` is a `Float`.

### Lists

`[1, 2, 3]` is a list; a comma may follow the last item. `xs.len` is how many there are.
`xs[i]` is the item at `i`, counted from zero. `+` joins two lists. Two lists are equal when
they have equal items in the same order.

**An index past either end is nothing**, and so is `min()` or `max()` of an empty list and a
`find` that matches nothing. Give them a default with `??`.

```byld,fragment
let xs = [3, 1, 2]
let none: List<Int> = []
Text("{xs[0]} {xs[9] ?? 0} {none.max() ?? 0} {(xs + [4]).len}")
```

That draws `3 0 0 4`.

Every method returns a new value and leaves the list as it was.

| Method | Result |
|--------|--------|
| `push(x)` | With `x` at the end |
| `removeAt(i)` | Without the item at `i`. Unchanged when there is none. |
| `reverse()` | Backwards |
| `slice(from, to)` | As for text |
| `contains(x)`, `indexOf(x)` | Whether `x` is in it; where, or `-1` |
| `join(sep)` | One text: each item's text form, with `sep` between. Empty for an empty list. |
| `map(f)` | `f(item)` for each |
| `filter(f)` | The items for which `f(item)` is `true` |
| `find(f)` | The first of those, or nothing |
| `sort()` | Smallest first, numbers or texts |
| `sortBy(f)` | Ordered by `f(item)`, smallest first. Items that tie stay in the order they had. |
| `reduce(start, f)` | `f(acc, item)` over each, from `start`. `start` for an empty list. |
| `min()`, `max()` | The smallest or largest, or nothing |

A function given to one of these must be *pure*: it computes and returns, and assigns
nothing (`EffectInPureLambda`).

An index written as a number that a list written beside it does not have, `xs[5]` where `xs`
is a `let` of one item, reads as nothing and is the warning `IndexOutOfRange`; it is not told
where something checks first (the right of `&&` and `??`, an arm of `? :`).

An index that is only known to be past the end when the program runs is not reported, and
arithmetic on the nothing it gives joins text.

### Records

`{ name: "Ada", score: 3 }` is a record. `u.name` reads a field. `{ ..u, score: 10 }` is a
copy of `u` with `score` set; the fields written after the spread win.

```byld,fragment
var u = { name: "Ada", score: 3 }
let v = { ..u, score: 10 }
Text("{u.score} {v.score} {v.name}")
```

That draws `3 10 Ada`.

A record literal has the type its fields give it, and a field it does not have is
`UnknownField`, with the nearest one: `u.nmae` on `{ name: "Ada" }`. So has a record with a
[declared type](#record-types). A field read to the left of a `??` may be missing, which is
what the default is for: `u.email ?? "none"` is `none`, and so is `u.a.b.c ?? "none"` as soon
as one step is missing. Only a value that came from outside, such as a reply from a server,
has fields the checker cannot know, and reading one is as free as it ever was.

A field written twice in one literal is `DuplicateField`. The type of `{ ..u, k: v }` is that
of `u` with `k` set, and a `var` that began as one record holds records with those fields.

Two records are equal when they have the same fields, with equal values, whatever order the
fields were written in: `{ a: 1, b: 2 } == { b: 2, a: 1 }`.

`{}` alone is an empty action, not an empty record.

`u.score = 10` writes a field. It means `u = { ..u, score: 10 }`, and `u.a.b = 2` the same one
level down, so the other fields are kept and `u` has to be a `var` (`NotAssignable` for a `let`
or a loop variable). `+=`, `-=`, `++` and `--` work on a field too. A field the record does not
have, or a value it cannot hold, is `TypeMismatch`.

> **Not yet.** `xs[i] = v` is accepted and changes nothing. (`05_records.byd`)

### A value or a default: `??`

`a ?? b` is `a`, or `b` when `a` is nothing. `b` is evaluated only then. Zero, an empty text
and `false` are values, not nothing: `0 ?? 9` is `0`.

### Functions as values

A lambda is written three ways, which mean the same:

```byld,fragment
let xs = [1, 2, 3]
let a = xs.map(x => x * 2)
let b = xs.map(|x| x * 2)
let c = xs.reduce(0, (acc, x) => acc + x)
Text("{a.len} {b.len} {c}")
```

`x => e` takes one parameter, `(a, b) => e` any number, each optionally typed, and
`|a, b| e` is the same. A lambda sees the names around it. One whose body is a block,
`() => { … }`, is an [action](#actions).

### Tuples

`(8, 16)` and `(top: 4, horizontal: 8)` are tuples, the values of attributes that take two
or four lengths. They are not a general type: a tuple has no fields to read.

## Declarations outside a view

A file is a list of items, in any order, except that every `use` comes before the first
view (`ImportAfterView`):

```text
file    := item*
item    := "private"? ( view | function | record | enum | context | constant ) | use
```

Every item is visible from every file of the project, with no import. Two of one kind with
one name are an error (`DuplicateViewName`, `DuplicateFunction`, `DuplicateRecordType`,
`DuplicateEnum`, `DuplicateConstant`). `private` keeps an item of a
[package](packages.md) to that package, and changes nothing in an app.

There is no `var` outside a view: a program has no global state.

### Constants

```byld
let gap = 8
let pad = gap * 2
let title: Str = "Settings"

View Main() {
    Text("{title}: {gap} and {pad}")
}
```

A `let` outside a view is evaluated once. It may read other constants and call shared
functions, in any order, and nothing else: no clock (`ImpureConstant`). Constants that read
each other in a circle are `ConstantCycle`. A name declared in a view hides a constant of
the same name there.

### Shared functions

```byld
fn clamp(n: Int, lo: Int, hi: Int) -> Int => n < lo ? lo : n > hi ? hi : n

View Main() {
    Text("{clamp(99, 0, 10)}")
}
```

```text
function := "fn" NAME "(" (NAME ":" type ("," NAME ":" type)*)? ")" "->" type "=>" expr
```

The body is one expression. A shared function is pure: it reads its parameters and
constants and calls other shared functions (`ImpureFunction`). A function is written into
each place that calls it, so it cannot call itself, directly or through another
(`RecursiveFunction`).

> **Not yet.** Arguments cannot be given by name, a function cannot have type parameters or
> default values, and its body cannot be a block. (`08_functions.byd`)

### Record types

```byld
type Place = { name: Str, country: Str?, latitude: Float }

View Main() {
    var places: List<Place> = [{ name: "Oslo", country: "NO", latitude: 59.9 }]
    for place in places {
        Text("{place.name}, {place.country ?? "at sea"}")
    }
}
```

```text
record := "type" NAME "=" "{" (NAME ":" type "?"? ","?)* "}"
```

A `?` after a field's type says it may be left out. A `var`, a parameter, a loop variable or
an item with a declared type has its fields checked.

### Enums

```byld
enum Size { small, medium, large }

View Main() {
    var size = Size.medium
    Text("{size} {size == Size.large}")
}
```

```text
enum := "enum" NAME "{" NAME ","? (NAME ","?)* "}"
```

An enum has at least one case: `enum Size { }` is a syntax error.

A case is written `Size.large`. Cases are compared with `==` and `!=` and are not ordered.
A case is a different type from text and from the cases of another enum. In a text, to a
controller and in JSON it is its name. Where a view's parameter is declared with the enum,
a call may write the case alone, `.large`; anywhere else that is `EnumShorthand`.

> **Not yet.** There is no `match`, a case cannot carry values, and nothing says when a
> chain of `when`s forgets a case. (`11_control_flow.byd`, `15_wanted_for_1_0.byd`)

### Contexts

```text
context := "context" NAME "{" (NAME ":" type "?"? "=" expr ","?)* "}"
```

A context is a set of values with defaults that a view gives to everything it draws. See
[`provide` and `inject`](#provide-and-inject).

### `use`

```text
use := "use" NAME ( "as" NAME | "." "{" NAME ("," NAME)* "}" )?
```

`use kit` makes a package's views, functions, types and constants reachable as `kit.Name`,
`use kit as k` as `k.Name`, and `use kit.{Card, Badge}` by their own names. The package has
to be in `[dependencies]` (`UnknownPackage`). See [packages](packages.md).

## Views

```text
view   := "View" NAME "(" params? ")" "{" member* "}"
params := param ("," param)*
param  := "bind"? NAME (":" type "?"?)? ("=" expr)?
```

The view named `Main` is the one a program runs. A view's name starts with a capital letter,
which is how `Card(…)` is told from a name in an expression; `View card()` is a syntax error.

### Parameters

A parameter has a type, and may have a default, which may read the parameters before it.
One with no default is required (`MissingParam`).

```byld
View Pair(a: Int, b: Int = a * 2) {
    Text("{a} {b}")
}

View Main() {
    Pair(2)
    Pair(2, b: 5)
}
```

A call gives arguments by position, by name, or positions first and then names: once an
argument has a name the rest have to, and a bare value after a named one is a syntax error.
An unknown name is `UnknownParam`, too many arguments `ViewArityMismatch`, an argument given
twice `DuplicateParam`, and one of the wrong type `ArgumentTypeMismatch`. A view that declares
a parameter twice, `View Card(a: Int, a: Int)`, is `DuplicateParam` too. An element of the
engine takes no argument by name, `Text("a", size: 3)` is `UnknownParam` (an attribute goes in
`#[ … ]`), except the one `NavStack(path: …)` and `NavHost(active: …)` name.

A parameter is a copy of what the caller gave, and follows it: when the caller's value
changes, so does everything in the view that read the parameter.

A view may call itself only under a condition that ends, as a tree does. Two views that
always draw each other are `RecursiveView`.

### Bindings

`bind name: Type` is the caller's `var` itself. The view reads and writes it, and every
write is the caller's. The call passes it as `bind: v` when the view has one binding, or
`bind name: v` for each when it has several. What is passed must be a `var` or a binding of
the calling view (`BindNotAVar`), of exactly the declared type. A binding with a default,
`bind n: Int = 0`, may be left out, and the view then keeps a `var` of its own.

### Callbacks

A parameter of type `Fn(…)` takes an action, written in braces at the call and run in the
*caller's* scope: the names in it mean what they mean where it was written.
`{|a, b| … }` names what the view passes back.

```byld
View Go(on_done: Fn(Str) = {|_| }) {
    on mount => on_done("ready")
    Text("go")
}

View Main() {
    var state = "waiting"
    Go(on_done: {|s| state = s })
    Text(state)
}
```

A default of `{}` does nothing. A default of `none` lets the view ask whether one was
given, `when on_done != none`; calling one that was not given does nothing. Calling a
callback with the wrong number of values is `CallbackArityMismatch`. A view takes no
trailing `=> action` (`ViewCallAction`).

### Slots

A parameter named `content`, written with no type, is the block of elements after the call.
A parameter of type `Slot` is a named one, filled with `name: { … }`. The view places a slot
by writing its name, any number of times. A slot's elements see the caller's names.
`= none` makes a slot optional, and `name != none` asks whether it was given.

```byld
View Card(title: Str, footer: Slot = none, content) {
    Column {
        Text(title)
        content
        when footer != none { footer }
    }
}

View Main() {
    Card("Hello", footer: { Text("the end") }) {
        Text("the body")
    }
}
```

A block given to a view with no slot is `UnexpectedChildren`; a value given to a slot is
`SlotArgument`.

A slot can be handed on to another view by name, `Inner(icon: icon)`, and is required there as
it is where it was declared: leaving it out of the first call is the one `MissingParam`.

### Optional values

`name: T? = none` is a `T` or nothing. It is read with `??`, or inside `when name != none`
and the true arm of `name != none ? a : b`. Any other read is `NoneNotHandled`. The default
of a `T?` is `none` and nothing else (`BadOptionalDefault`), and `none` compared with a
value that cannot be nothing is `NoneNeedsOptional`.

> **Not yet.** Only a parameter can be optional. A `var` cannot start as nothing.
> (`10_bind_context_optional.byd`)

### Layout on a call

A call to a view of yours takes, in `#[ ]`, the attributes a parent reads from a child:
`width`, `height`, `min_width`, `max_width`, `min_height`, `max_height`, `m` and its sides,
`grow`, `shrink`, `basis` and `align_self`. They go to the view's root element. Any other
attribute there is `UnknownParam`.

## What a view holds

```text
member := var | let | function | inject | provide | element | for | when
        | lifecycle | timer | style_block | state_block | route
```

Members may come in any order. A name declared anywhere in a view's body is visible in all
of it, before its line included.

### `var`

```text
var := "var" NAME (":" type)? "=" expr
```

State. Its first value is evaluated once, when the instance is made; if that value read
another `var`, it does not follow it afterwards. A `var` is written in an
[action](#actions) and nowhere else. Writing the value it already holds is not a change.

Every use of a view is an *instance* with `var`s of its own, which live while it is on
screen. An instance inside a `when` that turns false is gone, and starts again from its
first values when it comes back.

### `let`

```text
let := "let" NAME (":" type)? "=" expr
```

A value derived from others. It is evaluated when first read, kept, and evaluated again
only when something it read has changed. It is never written.

```byld,fragment
var a = 1
let b = a * 2
let c = b + 2
on mount => { a = 10 }
Text("{c}")
```

That draws `22`.

Two declarations of one name in one body, or one over a parameter, are `NameCollision`: the
second would hide the first. A body inside a `when`, a `for` or a `route` is a scope of its
own, and may reuse a name of the view's. `let`s of a view that read each other in a circle are
`ConstantCycle`, once one of them is read. A `var` or `let` that nothing in its view mentions is
the warning `UnusedVariable`, a parameter its view never mentions `UnusedParam`, and a
`private View` no view draws `UnusedView`. A name that starts with `_` says it is meant to be
unused, and `[lints]` in `byard.toml` switches a lint off, by its name:

```text
[lints]
unused-variable  = "allow"
unused-parameter = "allow"
unused-view      = "allow"
```

> **Not yet.** A `var` that is only ever written, `n++` and nothing else, counts as used, and
> so does a name another view of a package never reads. (`12_state.byd`)

### Functions in a view

A `fn` inside a view is as a shared one, and may also read the view's state. It is
evaluated again when that state changes.

### Elements

```text
element := NAME ("." NAME)? ("(" args? ")")? ("#[" attrs? "]")? ("as" NAME)?
           ( "{" member* "}" | "=>" action )?
attr    := NAME ("." NAME)? ":" expr            a property
         | NAME ("(" NAME ")")? "=>" action     an event
         | ".." expr                            a style spread in
```

An element is one of the engine's, a view of yours, or `package.View`. An unknown one is
`UnknownView`.

- The parentheses hold *content*: for a view its arguments, for an engine element what it
  shows. `Text` takes one `Str`.
- `#[ … ]` holds attributes, separated by commas; a comma may follow the last. An unknown
  one is `UnknownAttribute`, a value of the wrong kind `AttributeTypeMismatch`.
  `translate.y: 2` sets one part of an attribute that has several.
- `as name` names the element, for another element's `anchor_to` or `on name.hover`.
- Braces hold children, and the element's `on state { … }` blocks. An element that takes no
  children is `UnexpectedChildren`.
- `=> action` after an element that has a press of its own, such as `Button`, is its `tap`.
  After one that is only drawn (`Text`, `Image`, `VectorIcon`, `Spacer`, `Overlay`,
  `NavStack`, `NavHost`) it is `ViewCallAction`: the action would never run.

An event names what it is told with a word in parentheses, `input(text) => …`,
`pointer_down(e) => …`. An event that tells nothing takes none (`EventHasNoValue`).

[Layout](layout.md), [styling](styling.md), [controls](controls.md) and
[events](events-and-actions.md) list the elements, their attributes and their events.

An attribute written twice in one `#[ … ]` is `DuplicateAttribute`, and so is `translate.y`
twice; `translate.x` and `translate.y` are two. A state block twice in one element, with the
same states, is `DuplicateStateBlock`. A `var` or a `let` in an element's braces is
`LetInElementBody`: declare state at the top of the view.

### `when`

```text
when := "when" expr "{" member* "}" ("else" ( "{" member* "}" | when ))?
```

Shows its block while the condition holds, and the `else` block otherwise. Only the arm
that holds exists: its instances are made when it starts to hold and gone when it stops.
`byard check` checks every arm, shown or not.

### `for`

```text
for := "for" (NAME ",")? NAME "in" expr ("key" expr)? "{" member* "}"
```

One copy of the block for each item of a list. With two names, the first is the item's
position. The names belong to the block.

Without `key`, the copies stay where they are in the list: a new first item is shown by
what was the first copy, with that copy's state. With `key`, a copy belongs to its key and
follows it when the list is reordered, and only new keys get new copies. A key is a `Str`,
an `Int`, a `Float`, a `Bool`, an enum case or a record of those (`ForKeyNotComparable`),
and no two items may share one (`DuplicateForKey`).

```byld,fragment
for i, name in ["Ada", "Bo"] key name {
    Text("{i} {name}")
}
```

### Lifecycle and timers

```text
lifecycle := "on" ("mount" | "unmount") "=>" action
           | "on" "change" "(" expr ("," expr)* ")" "=>" action
           | "on" "measure" "=>" action
timer     := ("every" | "after") expr "=>" action
```

- `on mount` runs once, when the instance appears. `on unmount` runs when it goes.
- `on change(a, b)` runs when the value of any of them becomes different from what it was.
  It does not run for the values they have when the view appears, nor for what `on mount`
  wrote. An action that writes what it watches runs again until the value settles.
- `on measure` runs when the element it is written in changes size, with `it.width`,
  `it.height`, `it.x` and `it.y`.
- `after d` runs once, `d` milliseconds after the block it is in appears. `every d` runs
  each `d` milliseconds, first after one interval. Both stop when their block goes: a
  timer inside a `when` lives while that `when` holds. A duration written as `0ms` is
  `InvalidAnimation`: it would never fire.

### `provide` and `inject`

```text
provide := "provide" NAME "(" args ")"
inject  := "inject" NAME "as" NAME
```

`provide` gives a context's values to everything the view draws, the caller's slots
included. `inject` reads the nearest provider above, or the context's defaults when there
is none, and follows it when the provided values change. A field may be an `Fn`, for an
item to report to its container.

`inject` also names a *capability*, a service in Rust: `Http`, `Json` and `Store` come with
Byard, and an app adds its own. One the checker has not linked is a warning,
`UncheckableInject`.

### Styles

A *style* is a set of attributes with a name. `style { … }` is a value; `..s` spreads one
into an element; `a merge b` is `a` with `b` over it. Inside a style, and inside an
element's braces, `on hover { … }` gives the attributes a state adds, and
`on width >= 600 { … }` those a window size adds.

```byld,fragment
let card = style {
    bg: 0x222222
    radius: 8
    on hover { bg: 0x333333 }
}
Box #[..card, p: 12] {
    Text("a card")
}
```

After a spread, an attribute written later wins. States are `hover`, `pressed`, `focused`,
`focus_visible`, `focus_within`, `disabled`, `checked`, `selected`, `invalid`,
`indeterminate`, `dragging`, `enter` and `leave`, combined with `+`. An unknown one is
`UnknownStyleState`. `on name.hover { … }` follows the state of the element named `name`.

### Motion

`value with curve` after an attribute's value animates each change of it:
`opacity: on ? 1.0 : 0.4 with anim.spring()`. An attribute that cannot move is
`LayoutPropNotAnimatable` or `NotAnimatable`, and an unknown curve `UnknownAnimation`. A
`with` anywhere but on an attribute's value, in a `var`, a `let` or a condition, is
`NotAnimatable` too. See
[motion](motion.md).

## Actions

An action is what runs when something happens: after `=>` on an element, an event, a
lifecycle line or a timer, in the braces of a callback, and in the body of a named action,
`let save = () => { … }`.

```text
action    := statement | "{" statement* "}"
statement := target ("=" | "+=" | "-=") expr
           | target ("++" | "--")
           | "let" NAME (":" type)? "=" expr
           | "if" expr "{" statement* "}" ("else" ("if" … | "{" statement* "}"))?
           | call (("ok" NAME "=>" action)? ("err" NAME "=>" action)?)
```

Statements run in order, and each sees what the ones before it wrote. A `let` holds for the
statements after it, in its block. `if` needs braces.

A call to a capability or a controller returns at once. Its `ok` arm runs later with the
answer, its `err` arm with the error, a record with a `kind` and a `message`; both arms are
optional.

Only an action may change anything or read the clock. An assignment, `now()` or a call with
arms anywhere else — in a `let`, an attribute, a text, a `fn` — is an error
(`EffectInPureLambda`, `ClockOutsideAction`, `ImpureFunction`).

```byld,fragment
var count = 5
var message = ""
let add = (by: Int) => {
    let next = count + by
    if next > 5 {
        message = "that is the limit"
    } else {
        count = next
    }
}
Button("Add") => add(1)
Text(message)
```

What an assignment writes is a `var`, a `bind` parameter, or a field or an item of one. A
`let`, a parameter, a loop variable, a lambda's parameter or a literal is `NotAssignable`. An
assignment, `++` or `--` anywhere but an action, such as inside a text or a `let`, is
`EffectInPureContext`.

> **Not yet.** An action cannot loop and cannot stop early. (`11_control_flow.byd`)

## How the screen follows

Evaluating a text, an attribute, a `let`, a `when` or a `for` records every `var` and `let`
it read. Writing a `var` marks the places that read it, and on the next tick those places,
and only those, are evaluated again; what they changed is laid out and drawn. Nothing is
evaluated because of a write that changed nothing. A program with nothing in flight
evaluates nothing and draws nothing.

`untrack(expr)` reads without recording: a `let` that reads `untrack(a)` does not change when
`a` does.

## Built-in names

| Name | Is |
|------|----|
| `window.width`, `window.height` | The window's size, in logical pixels |
| `screens` | The connected screens. See [panels](panels.md). |
| `anim` | The curves `with` takes. See [motion](motion.md). |
| `route` | The parameters of the screen a navigation case draws. See [navigation](navigation.md). |
| `scroll_fraction` | How far a collapsing header has collapsed |
| `now()`, `nowUtc()` | The time, as ISO text. In an action only. |
| `navigate`, `back`, `replace` | Moves in a navigation stack |
| `match(name)` | The width of the element an overlay is anchored to |

## Editions

A change that would alter what a program means, or refuse one that is accepted, does not
happen in a release. It happens in a new *edition*, and a project says which one it is
written in:

```toml
[project]
edition = "2026"
```

A manifest with no `edition` is the first edition, `2026`, the language as it was before
editions, and it keeps that behaviour for as long as the edition exists: a program that
checked and drew `3` yesterday draws `3` tomorrow. An edition the compiler does not know is an
error that lists the ones it does. A library package names its edition under `[package]`.

[`byard fix`](cli-reference.md#byard-fix) moves a project to the next edition. It rewrites the
sources so they mean what they meant, through the formatter, and then writes the new edition
into the manifest.

Today `2027` differs from `2026` in four things: `? :` is looser than `||`, an `Int` that
overflows stops at the largest or the smallest instead of wrapping, a character of a text is a
grapheme cluster and not a code point, and a list or a record in a text shows its contents
instead of nothing. `byard fix` writes the parentheses that keep the first one's meaning. For
the other three no rewrite can tell which programs relied on the old behaviour, so it changes
no source. The other changes marked *(edition)* in
[the roadmap](https://github.com/Briany4717/byard/blob/main/docs/language/roadmap.md) will land
in `2027` too.

## One mistake, one error

The checker means to report each mistake once, at the place it was made, and to go on to
find the next. A stray token at the top of a file, a keyword used as a name and an unknown
word in an expression are each one error.

What the parser does after a mistake is chosen so that nothing it reads next is a second one:

- A number too large to hold, and a word with a letter outside the English alphabet, are one
  `UnexpectedChar` over the whole of the number or the word. The number then stands as `0`
  and the word as one name, so the rest of the line reads as it was meant.
- A string that is never closed runs to the end of the file, as the language says a string
  may, and that is its one `UnterminatedString`. Nesting past three levels is one
  `StringNestingTooDeep`, however deep it goes.
- A call that is missing its `)`, or has something before it, skips to the `)` that closes
  it. A `type` with no braces skips to the next declaration. A lambda's `=>` followed, on the
  next line, by an element of the engine is one error, not that element taken for a
  function.
- `size = 12` in an attribute list is `WrongAttributeSeparator`, and `12` is read as the
  value, so what follows is checked as it was written.
- Constants that read each other in a circle are one `ConstantCycle`; the names in it are
  not also unknown.

> **Not yet.** Attribute errors are only reported once every other error is fixed: an
> unknown attribute, a value of the wrong kind, `:` where `=>` is wanted. They are found
> by lowering the program to elements, and a program that already has type errors piles
> follow-on errors onto them when lowered. (`13_elements_styles.byd`)

No text, however malformed, makes the checker or a running program stop with a panic. The
suite's last file is made of such texts.
