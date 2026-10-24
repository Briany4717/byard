# Expressions and data

```byld
View Main() {
    var scores = [
        { name: "Ada", points: 31 },
        { name: "Grace", points: 44 },
        { name: "Linus", points: 12 }
    ]

    let ranked = scores.sortBy(s => 0 - s.points)
    let total = scores.reduce(0, (sum, s) => sum + s.points)
    let leader = ranked[0]

    Column #[p: 24, gap: 8] {
        Text("{leader.name} leads with {leader.points} of {total}") #[size: 18]
        for i, s in ranked {
            Text("{i + 1}. {s.name}  {s.points}")
        }
        Button("Give Linus 10") => {
            scores = scores.map(s => s.name == "Linus" ? { ..s, points: s.points + 10 } : s)
        }
    }
}
```

This chapter is about the values a view works with, and how to compute with
them. Everything here is an *expression*: it produces a value, and it can sit
anywhere a value is wanted, in a text, an attribute or a `let`.

## Literals

| Kind | Examples |
|------|----------|
| `Int` | `42`, `-3`, `0x3B82F6` |
| `Float` | `0.5`, `-12.0` |
| `Str` | `"hello"`, `"{count} taps"` |
| `Bool` | `true`, `false` |
| Angle | `90deg`, `-0.5deg`, `1.5rad` |
| Duration | `200ms`, `1.4s`, `5min` |
| List | `[1, 2, 3]`, `[]` |
| Record | `{ name: "Ada", score: 3 }` |
| Pair, quad | `(8, 16)`, `(top: 4, horizontal: 8)` |

A colour is an `Int`. `0xRRGGBB` is opaque and `0xAARRGGBB` carries an alpha, so
`0x80FF0000` is half-transparent red. Angles and durations are written with their
unit and mean something only where an angle or a duration is wanted, as in
`rotate: 90deg` and `anim.linear(200ms)`.

## Operators

From the loosest to the tightest:

| Operators | Meaning |
|-----------|---------|
| `=` `+=` `-=` | Assignment, in actions only |
| `with` | Animation, see [motion](motion.md) |
| `\|\|` | Or |
| `? :` | Choose one of two values |
| `&&` | And |
| `==` `!=` `<` `<=` `>` `>=` | Comparison |
| `??` | The left value, or the right one if the left has none |
| `+` `-` | Add, subtract |
| `*` `/` | Multiply, divide |
| `!` `-` | Not, negate |
| `++` `--` | Add or subtract one, in actions only |
| `.` `[ ]` `( )` | Field, index, call |

`&&` and `||` stop as soon as the answer is known, so the right side may safely
depend on the left: `xs.len > 0 && xs[0] > 5`.

Arithmetic on two `Int`s gives an `Int`, so `7 / 2` is `3`. Make one side a
`Float` for a fraction: `7 / 2.0` is `3.5`. Dividing by zero gives `0` and does
not stop the app.

`+` also joins text and appends lists. A number next to text becomes its digits:

```byld,fragment
Text("n = " + 5)
Text("{[1, 2] + [3]}")
```

## Text and interpolation

Anything between braces in a string is an expression, and the text follows it:

```byld,fragment
var count = 3
var price = 4.5
Text("{count} items at {price:.2} each, {count * price:.1} in all")
Text("{count == 1 ? "item" : "items"}")
Text("a literal \{brace\} and a \"quote\"")
```

- `{x:.2}` shows a number with exactly two decimals. Up to nine are allowed.
- A string inside the braces needs no escaping: `{n == 1 ? "item" : "items"}`.
- `\{` and `\}` are literal braces, `\"` a quote, `\n` a new line, `\t` a tab.

## Lists

```byld
View Main() {
    var xs = [3, 1, 2]

    Column #[p: 24, gap: 6] {
        Text("{xs.len} items, first is {xs[0]}")
        Text("sorted {xs.sort()[0]}, largest {xs.max()}, smallest {xs.min()}")
        Text("total {xs.reduce(0, (acc, x) => acc + x)}")
        Text("position of 2: {xs.indexOf(2)}, has 9: {xs.contains(9)}")
        Button("add a 9") => xs = xs.push(9)
        Button("drop the first") => xs = xs.removeAt(0)
    }
}
```

`xs.len` is how many there are, and `xs[i]` the one at position `i`, from zero.

Every list method returns a *new* value and changes nothing in place. To change
a list you write the result back: `xs = xs.push(9)`. This is what lets the screen
follow, because the write is what the engine sees.

| Method | Result |
|--------|--------|
| `push(x)` | The list with `x` added at the end |
| `removeAt(i)` | The list without the item at `i` |
| `contains(x)` | `true` if `x` is in it |
| `indexOf(x)` | Its position, or `-1` |
| `slice(from, to)` | The items from `from` up to, not including, `to` |
| `map(f)` | The list of `f(item)` |
| `filter(f)` | The items for which `f(item)` is `true` |
| `find(f)` | The first item for which `f` is `true` |
| `sort()` | Smallest first |
| `sortBy(f)` | Ordered by `f(item)`, smallest first, ties kept in place |
| `reduce(start, f)` | Folds the list: `f(acc, item)` over each, from `start` |
| `min()`, `max()` | The smallest or largest |

`min()` and `max()` of an empty list, and `find` with no match, read as nothing.
Give them a default with `??`, below. `slice` never fails: a range past the end is
cut short. It works on text too, and a mistyped method name is an error with the
closest real one suggested:

```byld,fails
View Main() {
    var xs = [1, 2]
    Text("{xs.pusj(3).len}")
}
```

```diagnostic
error[UnknownMethod]: `List<Int>` has no method `pusj` (did you mean `push`?)
```

To show a list on screen, use `for`, as [layout](layout.md) does.

## Records

A record is a set of named values. It is what JSON becomes, and what you use for
anything with more than one field:

```byld
View Main() {
    var user = { name: "Ada", score: 0 }

    Column #[p: 24, gap: 8] {
        Text("{user.name}: {user.score}")
        Button("score +10") => user = { ..user, score: user.score + 10 }
    }
}
```

Read a field with a dot. To change one, build a new record: `{ ..user, score: 10 }`
copies every field of `user`, then sets `score`. As with lists, nothing changes in
place, and you write the new record back.

### `??`: a value or a default

A field the record does not have reads as nothing, and so does a missing item in
a reply from a server. `a ?? b` is `a`, or `b` when `a` is nothing:

```byld,fragment
var user = { name: "Ada" }
Text(user.email ?? "no email yet")
```

The right side is only worked out when it is needed. A default of the wrong kind
is a type error.

### Record types

A record is only as safe as its field names. Declare its shape, and a misspelt
field becomes an error instead of an empty gap on screen:

```byld
type Place = { name: Str, country: Str?, latitude: Float }

View Main() {
    var places: List<Place> = [
        { name: "Oslo", country: "NO", latitude: 59.9 },
        { name: "Mid-ocean", latitude: 0.0 }
    ]

    Column #[p: 24, gap: 6] {
        for place in places {
            Text("{place.name}, {place.country ?? "at sea"}, {place.latitude:.1}°")
        }
    }
}
```

`type Name = { field: Type, … }` goes outside any view. A `?` after a type, as in
`Str?`, says the field may be left out. Once a `var`, a loop variable, a
parameter or an indexed item has a declared type, its fields are checked:

```byld,fails
type Place = { name: Str, country: Str? }

View Main() {
    var places: List<Place> = []
    for hit in places {
        Text(hit.nmae)
    }
}
```

```diagnostic
error[UnknownField]: a `Place` has no field `nmae` (did you mean `name`?)
```

Types belong to the whole program, whichever file declares them, and two with
one name are an error. A reply from a server is compared with the declared type
when it is written into a `var`, and the first difference is reported, such as
"item 3 has no `country`". See [data and I/O](data-and-io.md).

## Lambdas

A lambda is a small function you pass to a list method:

```byld,fragment
var xs = [1, 2, 3]
let doubled = xs.map(x => x * 2)
let sum = xs.reduce(0, (acc, x) => acc + x)
let big = xs.filter(|x| x > 1)
```

`x => …` is one parameter, `(a, b) => …` several, and `|x| …` is another way to
write the same thing. A lambda given to `map`, `filter` and the other list methods
must be *pure*. It can compute and return, and it cannot assign or run an action,
because it may be run again whenever its inputs change:

```byld,fails
View Main() {
    var xs = [1, 2]
    let bad = xs.map(x => { xs = [] })
    Text("x")
}
```

```diagnostic
error[EffectInPureLambda]: a `map`/`filter` lambda must be a pure expression
```

A lambda with a block body is an *action*, and may assign. See [state](state.md)
and [events and actions](events-and-actions.md).

## Text methods

Text has `len`, `slice`, and two helpers for the two jobs that come up most.

```byld,fragment
let moment = "2026-10-16T14:00"
Text("{moment.formatDate("dddd D MMMM YYYY, HH:mm")}")
Text("/search?q={"Tom & Jerry".urlEncoded()}")
```

The first shows `Friday 16 October 2026, 14:00`. `formatDate` takes an ISO date or
date-time and these tokens: `YYYY`, `MMMM`, `MMM`, `MM`, `M`, `DD`, `D`, `dddd`,
`ddd`, `HH`, `H`, `mm`. Anything else is kept as written. Names are in English.
Text that is not a date reads as nothing, so `?? "unknown"` gives it a default.

`urlEncoded()` makes text safe inside a URL. Letters, digits and `- . _ ~` stay,
and everything else, a space included, becomes `%XX`. The second line above is
`/search?q=Tom%20%26%20Jerry`, where a bare `&` would have ended the query early.
