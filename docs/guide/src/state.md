# State and reactivity

```byld
View Main() {
    Column #[p: 24, gap: 12] {
        Counter(label: "Apples")
        Counter(label: "Pears", step: 5)
    }
}

View Counter(label: Str, step: Int = 1) {
    var count = 0
    let total = count * 10
    let add = () => { count += step }

    Row #[gap: 12, align: center] {
        Text("{label}: {count} ({total})")
        Button("add") => add()
        Button("reset") => count = 0
    }
}
```

Two counters, and each has its own count. Tapping one does not touch the other.
That is the whole model: a view owns state, and what it shows is a function of
that state.

## `var`: state that changes

```byld,fragment
var count = 0
var price: Float = 4.5
var names: List<Str> = []
```

A `var` is a value that can change. Write to it with `=`, `+=`, `-=`, `++` or
`--`, in an [action](events-and-actions.md). Reading it anywhere on screen, in a
text, an attribute, a `when` or a `for`, makes that place follow it.

Types are inferred from the first value: `0` is an `Int`, `4.5` a `Float`, `"hi"`
a `Str`, `true` a `Bool`, and a list of numbers a `List<Int>`. Write the type
when there is nothing to infer from. An empty list has no element type, so it
needs one:

```byld,fails
View Main() {
    var names = []
    Text("x")
}
```

```diagnostic
error[CannotInfer]: cannot infer a type; add an explicit annotation
```

`var names: List<Str> = []` is the fix. A bare `List` says only that it is a list
of something, and it is what the todo example uses for a list of records.

## How the screen follows

Byard does not redraw everything when a `var` changes. Writing a `var` marks
every place that read it. On the next tick, only those places are evaluated
again, and only the parts of the screen they change are laid out and drawn. A
button that reads nothing is not touched when a count goes up.

This is why there is no `setState`, no observer to register and no list of
dependencies to keep. You write what the screen should show, and the engine
works out what to update. It also means a `var` is cheap to read in many places.

## `let`: a value worked out from other values

```byld,fragment
var todos: List = []
let remaining = todos.filter(t => !t.done).len
let summary = remaining == 0 ? "all done" : "{remaining} to go"
```

A `let` is *derived*. Its value is computed from other state, kept, and computed
again only when something it read changes. Use one for anything you would
otherwise calculate in two places, and for anything expensive. A `let` is read
like a `var` and is never written to.

`let` can read other `let`s, and so on, in any depth.

## `fn`: a function inside a view

```byld
View Main() {
    var score = 72

    fn grade(points: Int) -> Str => points >= 90 ? "A" : points >= 70 ? "B" : "C"

    Column #[p: 24, gap: 8] {
        Text("{score} is a {grade(score)}")
        Button("+10") => score += 10
    }
}
```

`fn name(parameters) -> Type => expression` is a function whose body is one
expression. Its parameters and its return type must both be written, because
nothing else would tell the checker what they are. The body may read the view's
own state.

## Named actions

Something you do more than once can be named. A lambda whose body is a block is
an action:

```byld
View Main() {
    var count = 0
    var steps = 0

    let add = (by: Int) => {
        count += by
        steps++
    }

    Row #[p: 24, gap: 12] {
        Button("+1") => add(1)
        Button("+10") => add(10)
        Text("{count} after {steps} steps")
    }
}
```

An action may write any `var` in the view, and run other actions. A lambda's
parameters may state their types, as `by: Int` does here. Without them the type is
worked out from how the lambda is used.

## Parameters are state too

A view's parameters change when its parent's values change, and the view follows.
The child below receives `place` as a parameter and shows it. When the parent
changes `city`, `Card` shows the new name with no further code.

```byld
View Main() {
    var city = "Tokyo"

    Column #[p: 24, gap: 12] {
        Row #[gap: 8] {
            Button("Tokyo") => city = "Tokyo"
            Button("Lima") => city = "Lima"
        }
        Card(place: city)
    }
}

View Card(place: Str) {
    Box #[bg: 0x1E1E2A, radius: 12, p: 16] {
        Text(place) #[color: 0xFFFFFF, size: 20]
    }
}
```

To *do* something when a parameter changes, such as fetch data for the new place,
use `on change`. [Data and I/O](data-and-io.md) shows it.

## State belongs to an instance

Every use of a view makes a new instance with its own `var`s, as `Counter` showed.
The state lives as long as the instance is on screen. A view inside a `when` that
turns false is gone, and its state with it. Showing it again starts fresh.

Hot reload keeps an instance's state through every edit that does not change the
view's parameters, `var`s, `let`s or `inject`s, which is why `byard dev` keeps
you on your screen. A [navigation](navigation.md) stack keeps the
screens it covers alive underneath, so their state is still there when you go
back.

## Functions shared by the project

A `fn` written at the top of a file, outside any view, is shared by every view in
the project:

```byld
fn clamp(n: Int, lo: Int, hi: Int) -> Int => n < lo ? lo : n > hi ? hi : n

View Main() {
    var volume = 7

    Row #[p: 24, gap: 12, align: center] {
        Button("-") => volume = clamp(volume - 1, 0, 10)
        Text("{volume}")
        Button("+") => volume = clamp(volume + 1, 0, 10)
    }
}
```

A shared function is *pure*. It may read only its parameters and call other
shared functions. It cannot read a `var` or write one, because it does not belong
to any view. Try to, and the checker says so:

```byld,fails
fn double() -> Int => count * 2

View Main() {
    var count = 5
    Text("{double()}")
}
```

```diagnostic
error[ImpureFunction]: function `double` must be pure
```

A package's shared functions are used through the package's name, like its views.
A view's own name wins over a function with the same name, and two functions with
one name are an error.

## Sharing state between views

There is no global state, and no top-level `var`. When two views need the same
value, hold it in a view above both, and pass it down as a parameter. Pass an
action down, as a `Fn()` parameter, for the child to change it. This is the same
move a callback makes in [views and parameters](views-and-parameters.md).

Anything that must outlive the app, such as a saved list, goes in a *store*, which
[Data and I/O](data-and-io.md) covers.
