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

### Names

A view, a function, a type, a parameter, a `var`, a `let` and a record's field are
all named with a word of your own. The words the language uses itself cannot be
names: `View`, `var`, `let`, `fn`, `inject`, `as`, `for`, `in`, `when`, `else`,
`style`, `untrack`, `with`, `merge` and `use`. One of them in a name's place is one
error, at the name:

```byld,fails
fn when(at: Str) -> Str => at

View Main() {
    Text("x")
}
```

```diagnostic
error[KeywordAsName]: `when` is a keyword and cannot name a function
```

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

An argument has to be the type the parameter declares. A `Bool` given to a `Str`
parameter is an error that names both, instead of text nobody meant. An `Int` may go
where a `Float` is wanted, and nothing else converts:

```byld,fails
View Main() {
    var failed = false
    Badge(failed)
}

View Badge(label: Str) {
    Text(label)
}
```

```diagnostic
error[ArgumentTypeMismatch]: parameter `label` of view `Badge` is a `Str`, but this is a `Bool`
```

A parameter is a copy of the caller's value. A control cannot write to one, so
`TextField #[bind: label]` on a parameter is an error, unless the parameter is a
[binding](#binding-a-var).

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

The names in the braces mean what they mean where you wrote them, always. If
`ActionButton` has a parameter, a `let` or a `var` called `count` of its own, the
braces above still change the caller's `count`, and a callback that passes
through several views reaches the same place as one that goes straight up.

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

A callback goes in by name. A view does not take a trailing `=> action` the way
`Button` does, because `Button` is built in and knows what a tap on it is; your view
only knows the parameters you gave it. Write the action after a view call, or an
event handler in its `#[...]`, and the checker stops you instead of dropping it:

```byld,fails
View Card(label: Str, on_tap: Fn() = {}) {
    Box #[bg: 0x3B82F6, p: 12, tap => on_tap()] { Text(label) }
}

View Main() {
    var count = 0
    Column { Card("Add") => count++ }
}
```

```diagnostic
error[ViewCallAction]: `Card` takes no trailing action; pass it as `on_tap: { … }`
```

If the view has one `Fn()` parameter the message names it. If it has none or
several, it says to declare one.

Callbacks are how views talk to the views that contain them. Data flows down as
parameters and actions flow back up as callbacks. [Events and
actions](events-and-actions.md) covers what an action may contain.

## Binding a `var`

A callback hands one change up. When a view only shows and edits one of the caller's
`var`s, as a switch or a text field does, declare the parameter a *binding* with `bind`
before its name. The caller passes the `var` itself, and the view uses it as if it were
its own:

```byld
View Switch(bind on: Bool, label: Str = "") {
    Row #[gap: 12, align: center] {
        Toggle #[bind: on, bg: 0x3B82F6, width: 52, height: 30]
        Button(label) #[bg: 0x00000000, color: 0xFFFFFF] => on = !on
    }
}

View Main() {
    var wifi = true

    Column #[bg: 0x14141C, p: 24, gap: 12] {
        Switch(bind: wifi, label: "Wi-Fi")
        Text(wifi ? "on" : "off") #[color: 0xFFFFFF]
    }
}
```

Inside `Switch`, `on` is used as a `var` is: read it in a text or an attribute and
the place follows it, assign it in an action, give it to a control's `bind:`, pass it
on to another view's binding. Every write goes to `wifi` in `Main`. There is no copy
in the view and no callback between the two, so tapping the button changes the
caller's text in the same frame, and `Main` changing `wifi` moves the switch.

A binding is declared with its type and has no default value: it is the caller's
`var`, so there is nothing a default could stand in for. A view that wants to work
without a caller's `var` keeps one of its own, and takes a plain parameter for its
starting value.

### At the call

With exactly one binding, write `bind:` where you would write a name, and give it a
`var`. With several, name each one after `bind`, as it was declared:

```byld
View FormField(label: Str, bind text: Str, bind valid: Bool) {
    Column #[gap: 4] {
        Text(label) #[color: 0xA0A8B8]
        TextField #[bind: text, input(t) => valid = t.len > 2, width: 240, height: 36]
        Text(valid ? "" : "Too short") #[color: 0xFF6B6B, size: 12]
    }
}

View Main() {
    var name = ""
    var ok = false

    Column #[bg: 0x14141C, p: 24] {
        FormField(label: "Name", bind text: name, bind valid: ok)
    }
}
```

`bind text: name` is the long form and is always allowed, also for a view with one
binding; `bind: name` is its short form. In `#[...]` only the short form is accepted, so
a view with several bindings takes them in the parentheses. A binding is never
positional: `FormField("Name", name, ok)` fills only the value parameters, in order.

The argument has to be a `var` of the calling view, a binding of the calling view, or
`theme.dark`. It must have the type the parameter declares exactly: an `Int` var is not
a `Float` binding, since the view writes to it. Anything else cannot be written and is an
error:

```byld,fails
View Switch(bind on: Bool) {
    Toggle #[bind: on]
}

View Main() {
    var wifi = true
    let off = !wifi
    Switch(bind: off)
}
```

```diagnostic
error[BindNotAVar]: `Switch(bind: off)`: `off` is a `let`, which is computed and cannot be written
```

A `for` row's variable, and a field of it, are copies of the list's item, so they are
the same error. Bind a `var` of the row's own view instead, or hand the change up with a
callback.

A value where the view declares a binding, and `bind` where it declares a value, are
errors that say which each parameter is:

```byld,fails
View Switch(bind on: Bool) {
    Toggle #[bind: on]
}

View Main() {
    var wifi = true
    Switch(on: wifi)
}
```

```diagnostic
error[BindArgMismatch]: parameter `on` of view `Switch` is a binding, so the caller passes a `var` of its own: write `bind: wifi`, not `on: wifi`
```

### Passing it on

A binding can be handed to another view's binding, and it is still the caller's `var`,
however many views down:

```byld
View Settings() {
    var dark = false
    Column #[bg: 0x14141C, p: 24, gap: 12] {
        Panel(bind: dark)
        Text("dark is {dark}") #[color: 0xFFFFFF]
    }
}

View Panel(bind enabled: Bool) {
    Row { Row2(bind: enabled) }
}

View Row2(bind enabled: Bool) {
    Button("flip") => enabled = !enabled
}

View Main() {
    Settings()
}
```

If the view that owns the `var` goes away (a `when` turned false) while a view holding
its binding is still being used, nothing breaks: the binding keeps reading and writing
the cell it was given, and the view that owned it starts over with a new one when it is
shown again.

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
