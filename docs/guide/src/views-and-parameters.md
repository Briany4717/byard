# Views and parameters

```byld
View Main() {
    Column #[p: 24, gap: 12] {
        Badge("new")
        Badge(label: "sale", tint: 0xEF4444)
        Panel(title: "Profile") {
            Text("Ada Lovelace")
            Text("Analyst")
        }
    }
}

View Badge(label: Str, tint: Int = 0x3B82F6) {
    Box #[bg: tint, radius: 12, p: (4, 10)] {
        Text(label) #[color: 0xFFFFFF, size: 12]
    }
}

View Panel(title: Str, content) {
    Column #[bg: 0x1E1E2A, radius: 12, p: 16, gap: 8] {
        Text(title) #[color: 0xFFFFFF, size: 18, weight: bold]
        content
    }
}
```

Everything on screen is a *view*. A view is a named piece of interface with
parameters, like a function that returns a picture. `Main` uses `Badge` and
`Panel`, and those use `Box`, `Column` and `Text`, which come with the engine.
You will not see a difference at the call site between a view you wrote and one
the engine provides.

## Declaring a view

```text
View Name(parameters) {
    …members…
}
```

A view's name starts with a capital letter. Its body is a list of *members*: state
(`var`, `let`, `fn`), elements, and a few control forms (`when`, `for`) that this
guide introduces one by one. Most of a body is elements.

A file holds as many views as you like. Every view in a project can use every
other by name, whichever file it is in.

An element is a name, then optionally content in parentheses, then attributes
between `#[` and `]`, then children in braces:

```byld,fragment
Text("Hello") #[size: 20, color: 0xFFFFFF]
Column #[gap: 8] {
    Text("one")
    Text("two")
}
```

`Text("Hello")` has *content*, the one value it shows. `#[size: 20]` has
*attributes*, which style and configure it. Children go inside braces. A view you
write is used the same way: its parameters take the place of content.

## Parameters

Parameters go in the parentheses of the declaration. Each one needs a type.

```byld,fails
View Card(label) {
    Text(label)
}
```

```diagnostic
error[MissingAnnotation]: missing type annotation on view parameter
```

The types you will use:

| Type | What it holds | Example |
|------|---------------|---------|
| `Int` | A whole number. Colours are `Int`s too. | `42`, `0x3B82F6` |
| `Float` | A number with a fraction | `0.5`, `-12.0` |
| `Str` | Text | `"Ada"` |
| `Bool` | `true` or `false` | `true` |
| `List<T>` | A list of `T` | `["a", "b"]` |
| `Fn(…)` | A callback | `{ count++ }` |

A [record type](expressions.md#record-types) you declare yourself works as a
type too.

### Defaults

Put `= value` after a parameter's type and callers may leave it out. `Badge`
above has one: `Badge("new")` gets the default blue, and `Badge("sale",
0xEF4444)` overrides it. Parameters that have no default are required.

### Calling a view

Pass arguments by position, by name, or a mix, as long as the positional ones
come first. Names read better when a view has more than two parameters or when
the values are numbers.

A call that does not fit is an error, and the message names the problem:

```byld,fails
View Main() {
    Badge(lable: "new")
}

View Badge(label: Str) {
    Text(label)
}
```

```diagnostic
error[UnknownParam]: `Badge` has no parameter `lable` (did you mean `label`?)
error[MissingParam]: missing required argument `label` for view `Badge`
```

## Passing children

A view takes children when it declares a parameter named `content`, written with
no type. Place `content` where the children should go:

```byld
View Main() {
    Callout(kind: "tip") {
        Text("Save often.")
        Text("Hot reload keeps your state.")
    }
}

View Callout(kind: Str, content) {
    Column #[bg: 0x1E2A1E, radius: 10, p: 14, gap: 4] {
        Text(kind) #[color: 0x8FD19E, size: 12, weight: bold]
        content
    }
}
```

`content` is a *slot*, not a value. You cannot store it, pass it on or inspect it.
There is one per view, and that covers wrappers such as cards, dialogs and
screens that share a frame. Aura Weather's `Screen` view puts a title above
whatever each screen passes in.

## Passing behaviour

A parameter of type `Fn()` takes an action. The caller writes the action in
braces and the view runs it when it likes:

```byld
View Main() {
    var count = 0

    Column #[p: 24, gap: 12] {
        Text("count = {count}")
        ActionButton(label: "Add one", on_tap: { count++ })
        ActionButton(label: "Reset", on_tap: { count = 0 })
        ActionButton(label: "Does nothing")
    }
}

View ActionButton(label: Str, on_tap: Fn() = {}) {
    Box #[bg: 0x3B82F6, radius: 8, p: (10, 18), tap => on_tap()] {
        Text(label) #[color: 0xFFFFFF, size: 15]
    }
}
```

The braces run in the *caller's* scope. `ActionButton` never sees `count`, yet a
tap inside it changes it. That is the whole trick, and it is how a reusable
component stays ignorant of what it is used for. A default of `{}` is an action
that does nothing, so the callback is optional.

A callback can pass values back. Declare them in the type, `Fn(Str)`, and name
them between bars in the caller's braces:

```byld
View Main() {
    var picked = "none"

    Column #[p: 24, gap: 12] {
        Text("picked: {picked}")
        Choice(options: ["red", "green", "blue"], on_pick: {|choice| picked = choice })
    }
}

View Choice(options: List<Str>, on_pick: Fn(Str) = {|_| }) {
    Row #[gap: 8] {
        for option in options {
            Button(option) => on_pick(option)
        }
    }
}
```

Callbacks are how views talk to the views that contain them. Data flows down as
parameters and actions flow back up as callbacks. [Events and
actions](events-and-actions.md) covers what an action may contain.

## Views from other places

A view in a package is used through the package's name:

```byld,ignore
// Needs a `[dependencies]` entry in byard.toml, so it cannot be checked alone.
use brand as b

View Main() {
    b.Badge(label: "from a package")
}
```

[Packages](packages.md) explains `use`. Views and functions from your own
project need none of this.

## Where views come from

Three sources, and a call site cannot tell them apart:

1. The **engine's elements**: `Box`, `Column`, `Row`, `Spacer`, `Text`, `Button`,
   `TextField`, `Toggle`, `Slider`, `Checkbox`, `RadioButton`, `Image`,
   `VectorIcon`, `ScrollView`, `Overlay`, `Canvas`, `Grid`, `Clip`, `ZStack`,
   `NavStack` and `NavHost`. The chapters ahead cover them.
2. **Views you write**, as in this chapter.
3. **Native views**, written in Rust and declared in `byard.toml`. See
   [native views](rust-native-views.md).

An element the compiler does not know is an error with a suggestion:

```byld,fails
View Main() {
    Colum { Text("hi") }
}
```

```diagnostic
error[UnknownView]: unknown view `Colum` (did you mean `Column`?)
```
