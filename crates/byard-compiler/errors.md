# Error index

Every diagnostic `byard check` prints, and every one the language server shows,
starts with a name in brackets:

```text
main.byd:7:14: error[UnknownAttribute]: unknown attribute `colour` on `Column`
```

The name is the error's identity. It does not change from one release to the
next, so a script, an editor's problem matcher or a search can rely on it, and
it is what this page is indexed by. `byard explain <Name>` prints the section
of this page for that name, in the terminal:

```text
byard explain UnknownAttribute
```

A *warning* is a diagnostic the program still runs with. The ones that are
lints can be turned off, or made errors, in the `[lints]` table of
`byard.toml`.

Each section says what the error means and why it is one, and, where a single
file can show it, gives a program that has it and the line the compiler
prints for it. Those examples are checked on every build, so they cannot drift
from what the compiler says. The errors that need a package, a controller of
the Rust half, an asset file or a reply at runtime to happen have no example
yet.

## A11yNeedsRole

`a11y_label` or `a11y_description` on a layout container that is not in the accessibility tree, so nothing would read it.

```byld,fails
View Main() {
    Column #[a11y_label: "x"] {}
}
```

```diagnostic
error[A11yNeedsRole]: `a11y_label` on a `Column` is read by nothing: a layout container is in the accessibility tree only with a role; give it an `a11y_role` (button, group, list_item, …), or put the label on the control or `Text` inside
```

## AnchorToText

*A warning*: the program still runs.

`anchor_to: "name"`, a text looked up in the whole window, where the name itself would reach this view's own element.

```byld,fails
View Main() {
    Text("a") as label
    Box #[width: 10, height: 10, anchor_to: "label"]
}
```

```diagnostic
warning[AnchorToText]: `anchor_to: "label"` is looked up among every `as` in the window, so two copies of a view share one name; write `anchor_to: label` to point at the element tagged `as label` in this view
```

## ArgumentTypeMismatch

An argument of a user-`View` call does not have the type the callee declares for that parameter.

```byld,fails
View Bar(w: Int) { Text("{w}") }
View Main() { Bar(3.5) }
```

```diagnostic
error[ArgumentTypeMismatch]: parameter `w` of view `Bar` is a `Int`, but this is a `Float`
```

## ArityMismatch

A built-in element received the wrong number of positional `(...)` content arguments.

```byld,fails
View Main() {
    Text()
}
```

```diagnostic
error[ArityMismatch]: `Text` takes 1 argument(s), found 0
```

## AttributeNeedsRole

A range attribute (`a11y_value_min`, `a11y_value_max`, `a11y_value_step`) on an element whose role has no value to give a range to.

```byld,fails
View Main() { Box #[a11y_role: button, a11y_value_min: 1, tap => {}] {} }
```

```diagnostic
error[AttributeNeedsRole]: `a11y_value_min` is the range of a value, and only a `slider`, a `spin_button` or a `progress_bar` has one: add `a11y_role: slider` (or `spin_button`, `progress_bar`) to this element, or drop the attribute
```

## AttributeTypeMismatch

An attribute value (or content value) had the wrong scalar type for the built-in element's contract.

```byld,fails
View Main() {
    Text(5)
}
```

```diagnostic
error[AttributeTypeMismatch]: expected a string
```

## AutofocusNeedsFocus

`autofocus` on an element nothing can focus: focus would never go there, so the attribute would do nothing.

```byld,fails
View Main() { Box #[autofocus: true, width: 10, height: 10] }
```

```diagnostic
error[AutofocusNeedsFocus]: `autofocus` names the element focus goes to, and a `Box` written like this cannot take focus; give it a `focus => …` handler, a `focused:` binding, or an `a11y_role` that takes focus (button, toggle_button, switch, checkbox, radio, tab, menu_item, link) together with a `tap`
```

## BadBindingParam

A binding parameter that cannot be one: it has no type, is a callback, or is called `bind`.

```byld,fails
View V(bind: Int) { Text("x") }
```

```diagnostic
error[BadBindingParam]: `bind` introduces a binding, so it cannot name a parameter; at a call, `bind:` is the short form of `bind name:`. Pick another name
```

## BadContextField

A context field of a type a context cannot hold: a `Slot`.

```byld,fails
context Shell { header: Slot = {} }
View Main() { Text("a") }
```

```diagnostic
error[BadContextField]: field `header` of context `Shell` is a `Slot`: a context holds values and `Fn` callbacks. A block of elements goes to a parameter declared `: Slot`
```

## BadOptionalDefault

An optional parameter (`T?`) with a default that is not `none`, or `= none` on a parameter that is not optional.

```byld,fails
View Pill(n: Int? = 3) { Text("{n ?? 7}") }
View Main() { Pill() }
```

```diagnostic
error[BadOptionalDefault]: the default of `n` can only be `none`: a `T?` parameter may be left out. For a default value, drop the `?` and write `n: T = value`
```

## BindArgMismatch

A view call passes a value where the view declares a binding, or passes `bind name:` for a parameter that is a value.

```byld,fails
View Switch(bind on: Bool) { Toggle #[bind: on] }
View Main() {
    var wifi = true
    Switch(on: wifi)
}
```

```diagnostic
error[BindArgMismatch]: parameter `on` of view `Switch` is a binding, so the caller passes a `var` of its own: write `bind: wifi`, not `on: wifi`
```

## BindNeedsName

`bind: x` for a view with more than one binding: it does not say which.

```byld,fails
View P(bind a: Int, bind b: Int) { Text("{a}{b}") }
         View Main() { var x = 1
             P(bind: x) }
```

```diagnostic
error[BindNeedsName]: view `P` has 2 bindings, so `bind:` does not say which; name the one you pass: `bind a: …`, `bind b: …`
```

## BindNotAVar

A control's `bind:` (or `value:`) names something that cannot be written: a view parameter, a `let`, a loop variable or a literal. The control would draw its default and never write, silently. The same check covers a view's binding: `Switch(bind: x)` with an `x` that is not a `var` or a binding of the calling view.

```byld,fails
View Field(label: Str) { TextField #[bind: label] }
View Main() { Field("a") }
```

```diagnostic
error[BindNotAVar]: `TextField #[bind: label]` cannot change `label`: a parameter is a copy of what the caller passed, so a change would be lost. Declare it a binding, `bind label: Str`, and the caller passes a `var` with `bind: …`; or keep a `var` of your own, bind to it, and hand each change up through a callback parameter
```

## BindOutsideViewCall

`bind name: x` where only a user view can take it: a function call, a tuple, a control.

```byld,fails
View Main() { var a = 1
 let t = (bind x: a)
 Text("x") }
```

```diagnostic
error[BindOutsideViewCall]: `bind x: …` passes a binding to a view of the program; a function, a tuple or a built-in takes a plain `x: …`
```

## BlockFitsNeither

A `{ … }` argument that is neither a callback (a list of actions) nor a block of elements. Which one it is depends on the parameter it is given to, so both readings are tried and this says what each expected.

```byld,fails
View Main() { var n = 0 Card(x: { Row #[gap: 1] n++ }) }
```

```diagnostic
error[BlockFitsNeither]: this block is neither actions nor elements. As the actions of an `Fn` parameter it expected an expression; as the elements of a `Slot` parameter it expected a declaration or element
```

## CallbackArityMismatch

A callback prop (`Fn(...)` parameter) was invoked with the wrong number of arguments, e.g. `on_change()` where the parameter is declared `Fn(Str)`.

```byld,fails
View Choice(on_pick: Fn(Str) = {|_| }) {
    on mount => on_pick("a", "b")
    Text("x")
}
View Main() { Choice() }
```

```diagnostic
error[CallbackArityMismatch]: callback `on_pick` expects 1 argument(s), but was invoked with 2
```

## CallbackNotInvocable

A callback prop (`Fn(...)` parameter) was referenced somewhere other than an invocation, callbacks are fire-and-forget action handlers, not first-class values, so `on_tap` may only appear as `on_tap(...)` in an event handler.

```byld,fails
View Item(on_tap: Fn()) {
    Text("{on_tap}")
}
View Main() { Item(on_tap: {}) }
```

```diagnostic
error[CallbackNotInvocable]: callback prop `on_tap` can only be invoked (`on_tap(...)`) inside an event handler; it is not a first-class value
```

## CallbackTypeMismatch

A non-callback expression was passed to a `Fn(...)` parameter, or a callback literal was passed to a scalar parameter.

```byld,fails
View Choice(on_pick: Fn(Str) = {|_| }) {
    on mount => on_pick(5)
    Text("x")
}
View Main() { Choice() }
```

```diagnostic
error[CallbackTypeMismatch]: parameter `on_pick` of view `Choice` is a callback (`Fn(...)`); pass an action block like `on_pick: { … }`
```

## CannotInfer

A `var`/`let` initializer cannot be inferred and has no annotation (the empty array `[]`, or a heterogeneous array).

```byld,fails
View Main() {
    var xs = []
    Text("x")
}
```

```diagnostic
error[CannotInfer]: cannot infer a type; add an explicit annotation
```

## CanvasMissingSize

A `Canvas` was declared without the required `width`/`height` props (a canvas is a fixed-size drawing surface).

```byld,fails
View V() { Canvas #[width: 48] { circle(cx: 1, cy: 1, r: 1) } }
```

```diagnostic
error[CanvasMissingSize]: `Canvas` requires explicit `width` and `height` props
```

## ClockOutsideAction

`now()` or `nowUtc()` appeared where no action runs: a `let`, an attribute, a text interpolation, a `var`'s initialiser, a `fn`. The clock is read when the call runs, and a projection runs when what it reads changes. The clock is not something it reads, so it would show the time it was first computed and never move.

```byld,fails
View Main() {
    let t = now()
    Text(t)
}
```

```diagnostic
error[ClockOutsideAction]: `now()` reads the clock once, when an action runs; to show the time, write it to a `var` in an action, such as `every 1s => time = now()`
```

## ConflictingGroupMode

Both `fuse:` and `morph:` on one `Canvas`. They cannot be combined: morphing between fused groups would need groups inside groups, and a group is kept flat so the cost of drawing it stays bounded.

```byld,fails
View Main() {
    Canvas #[width: 140, height: 48, fuse: 16, morph: 0.5] {
        circle(cx: 20, cy: 24, r: 10)
        circle(cx: 40, cy: 24, r: 10)
    }
}
```

```diagnostic
error[ConflictingGroupMode]: `fuse` and `morph` are mutually exclusive on one `Canvas`; a morph between fused sub-groups would need nested groups
```

## ConflictingSpacingField

A `p`/`m` spacing tuple named a side more than once, combined an axis shorthand (`horizontal`/`vertical`) with one of its component sides, or mixed named and positional fields (`Len` erratum).

```byld,fails
View Main() {
    Box #[p: (top: 4, vertical: 8)] { Text("a") }
}
```

```diagnostic
error[ConflictingSpacingField]: spacing side `top` was set more than once
```

## ConstantCycle

Top-level constants that read one another in a circle.

```byld,fails
let a = a + 1
View Main() { Text("x") }
```

```diagnostic
error[ConstantCycle]: constants and `let`s cannot read each other in a circle: a → a
```

## ConstantTypeMismatch

A constant's value is not of the type written for it.

```byld,fails
let title: Str = 5
View Main() { Text(title) }
```

```diagnostic
error[ConstantTypeMismatch]: constant `title` is declared `Str` but its value is `Int`
```

## ContentOutsideSurface

Content at the top of a program that has windows, outside any of them: there is no window for it to be drawn in.

```byld,fails
View Main() { Text("x") Panel #[edge: top] { Text("y") } }
```

```diagnostic
error[ContentOutsideSurface]: this `Text` is outside every window: a program that writes `Window` says where each window is, so what is shown has to be inside one; move it into a `Window`
```

## ContextFieldNeedsDefault

A context field with no default: every field has one, so an `inject` with no provider above has something to read.

```byld,fails
context Tabs { current: Int }
View Main() { Text("x") }
```

```diagnostic
error[ContextFieldNeedsDefault]: field `current` of context `Tabs` needs a default: write `current: Type = value`, so a view with no provider above it has something to read
```

## ControllerCallFailed

A controller call failed and its call site wrote no `err` arm. Distinct from [`UnknownControllerMethod`](#unknowncontrollermethod) on purpose: "this controller has no such method" is a wiring mistake the developer fixes in the call, while this is the controller's own runtime failure, reported because a success nobody asked about is fine and an **error** nobody asked about is a silent failure.

## DashOnFusedStroke

A fused stroke is given a dash pattern. The length along the edge of a fused shape has no exact form, and any estimate would make the dashes crawl as the fusion animates, so it is refused.

```byld,fails
View Main() {
    Canvas #[width: 140, height: 48, fuse: 16] {
        circle(cx: 24, cy: 24, r: 18, stroke: 0xFFFFFF, stroke_width: 2, dash: (6, 4), fill: 0x6750A4)
        circle(cx: 60, cy: 24, r: 14, fill: 0x6750A4)
    }
}
```

```diagnostic
error[DashOnFusedStroke]: a fused stroke cannot be dashed: there is no arc length along the union of two shapes, and an approximation would make the dashes crawl as the fusion changes
```

## DisabledStateNeverMet

An `on disabled+hover { … }` block (or `pressed`, `focused`, `focus_visible`, `dragging`): a disabled element never is any of those, so the block could never apply.

```byld,fails
View Main() { Box #[disabled: true] { on hover+disabled { overlay: 0x14FFFFFF } } }
```

```diagnostic
error[DisabledStateNeverMet]: `on disabled+hover` never applies: a disabled element is never hovered; write `on disabled` for how it looks
```

## DiscardedControllerReply

A reply came back for a continuation that is gone: its view unmounted, or a hot reload replaced the program. Reported rather than silently dropped, because the alternative reading, "the controller never answered", sends the developer looking in the wrong half of the app.

## DivisionByZero

*A warning*: the program still runs.

`x / 0` or `x % 0`, with the zero written: the answer is 0, which is almost never what was meant.a divisor that is only zero while the program runs stays 0, as it was.

```byld,fails
View Main() {
    Text("{1 / 0}")
}
```

```diagnostic
warning[DivisionByZero]: `/ 0` answers 0, since a number cannot be divided by nothing: check the divisor before dividing, as in `d != 0 ? n / d : 0`
```

## DuplicateAttribute

An element's `#[ … ]` writes one attribute twice: `#[size: 12, size: 14]`.

```byld,fails
View Main() {
    Text("a") #[size: 12, size: 14]
}
```

```diagnostic
error[DuplicateAttribute]: `size` is written twice in this `#[ … ]`: the last one wins, so remove the one that does not count
```

## DuplicateConstant

The same top-level constant is declared twice within one package.

```byld,fails
let a = 1
let a = 2
View Main() { Text("{a}") }
```

```diagnostic
error[DuplicateConstant]: constant `a` is declared more than once in this project
```

## DuplicateEnum

The same enum is declared twice within one package.

```byld,fails
enum A { x }
enum A { y }
View Main() { Text("a") }
```

```diagnostic
error[DuplicateEnum]: enum `A` is declared more than once in this project
```

## DuplicateEnumCase

An enum lists the same case twice.

```byld,fails
enum A { x, x }
View Main() { Text("a") }
```

```diagnostic
error[DuplicateEnumCase]: enum `A` lists the case `x` more than once
```

## DuplicateField

A record literal writes one field twice: `{ a: 1, a: 2 }`.

```byld,fails
View Main() {
    let u = { a: 1, a: 2 }
    Text("{u.a}")
}
```

```diagnostic
error[DuplicateField]: this record writes `a` twice: the last one wins, so remove the one that does not count
```

## DuplicateForKey

Two items of a keyed `for` have the same key. Raised at runtime, where the list arrives; the second item is not drawn.

```byld,fails
View Main() {
    for name in ["a", "a"] key name { Text(name) }
}
```

```diagnostic
error[DuplicateForKey]: two items of this `for` have the key `"a"`, so there is no telling which is which: the second one is not drawn; make the key unique, for example `key item.id`
```

## DuplicateFunction

The same top-level `fn` is declared twice within one package's files.

```byld,fails
fn f(x: Int) -> Int => x
fn f(x: Int) -> Int => x + 1
View Main() { Text("{f(1)}") }
```

```diagnostic
error[DuplicateFunction]: function `f` is declared more than once in this project
```

## DuplicateMeasure

An element declared `on measure` twice. Both would fire and both would write, and which one won would be an ordering detail invisible in the source, so the second is refused and pointed at the first.

```byld,fails
View Main() {
    var a: Float = 0.0
    var b: Float = 0.0
    Column #[width: 100, height: 100] {
        on measure => a = it.w
        on measure => b = it.h
    }
    Text("{a} {b}")
}
```

```diagnostic
error[DuplicateMeasure]: this element already declares `on measure`; one element has one measured rect, so a second handler would only race the first
```

## DuplicateParam

The same callee parameter was bound twice, positional + named, or named twice.

```byld,fails
View Card(a: Int, a: Int) { Text("{a}") }
View Main() { Card(1, 2) }
```

```diagnostic
error[DuplicateParam]: view `Card` declares the parameter `a` more than once: rename or remove one of them
```

## DuplicateProvide

A view provides one context twice.

```byld,fails
context Tabs { current: Int = 0 }
View Main() {
    provide Tabs(current: 1)
    provide Tabs(current: 2)
    Text("x")
}
```

```diagnostic
error[DuplicateProvide]: this view already provides `Tabs`: write one `provide Tabs(…)` with every field you set
```

## DuplicateRecordType

Two record types of one name in a program (`type Place = …` twice).

```byld,fails
type P = { a: Int }
type P = { b: Int }
View Main() { Text("x") }
```

```diagnostic
error[DuplicateRecordType]: the type `P` is declared twice in this program
```

## DuplicateStateBlock

Two `on enter` (or two `on leave`) blocks on one element: an element has one way of arriving and one of going.

```byld,fails
View Main() {
    Box {
        on hover { bg: 0x333333 }
        on hover { bg: 0x444444 }
    }
}
```

```diagnostic
error[DuplicateStateBlock]: an element has one `on hover` block: write the values together in the first one
```

## DuplicateViewName

The same `View` name is declared twice within one package namespace (across its files), exports must be unambiguous.

```byld,fails
View A() { Text("a") }
View A() { Text("b") }
View Main() { A() }
```

```diagnostic
error[DuplicateViewName]: view `A` is declared more than once in this project
```

## DynamicStyleForbidden

A `style { … }` block reads a `var`. A style is fixed when the program is checked; a value that changes goes on the element itself, or in an `on` block.

```byld,fails
View V() {
 var c = 1
 style { .a #[bg: c] }
}
```

```diagnostic
error[DynamicStyleForbidden]: a `style` block cannot read a `var` (styles are static in Phase 2)
```

## EffectInPureContext

A controller call appeared in a pure context, a `let`, a memo, an attribute value. A call is an effect: it schedules work and returns nothing. Allowing one where a value is expected would make a projection depend on when it happened to be re-pulled, which is exactly what a projection must not do.

```byld,fails
View Main() {
    var n = 0
    Text("{n = 1}")
}
```

```diagnostic
error[EffectInPureContext]: an effect (a controller call, an assignment, `++`) cannot appear in a value position; do it from an action, and write what it gives to a `var`
```

## EffectInPureLambda

A collection lambda (`map`/`filter` argument) performed a side effect, an assignment or event action. Collection lambdas must be pure so a reactive pull can re-run them safely.

```byld,fails
View Main() {
    var xs = [1, 2]
    let bad = xs.map(x => { xs = [] })
    Text("x")
}
```

```diagnostic
error[EffectInPureLambda]: a `map`/`filter` lambda must be a pure expression, no assignment or action (it may re-run during a reactive pull)
```

## EnumShorthand

A case written alone (`.tonal`) where nothing says which enum it belongs to: it is short for `Variant.tonal` only as the argument of a parameter declared with the enum's type.

```byld,fails
enum Variant { filled, tonal }
View Main() {
    var v = Variant.filled
    Text("{v == .tonal}")
}
```

```diagnostic
error[EnumShorthand]: `.tonal` is short for `Variant.tonal` only as the argument of a parameter typed `Variant`; write `Variant.tonal` here
```

## EventHasNoValue

A handler named a value for an event that carries none, `tap(e)`: the name would read as empty.

```byld,fails
View Main() {
    var n = 0
    Box #[tap(e) => n++] {}
}
```

```diagnostic
error[EventHasNoValue]: `tap` carries no value to name; write `tap => …` without one (for a position, use `pointer_down(e)`)
```

## ExclusiveNeedsEdge

`exclusive: true` on a panel with no `edge` and fewer than three anchors: the protocol ignores a zone there.

```byld,fails
View Main() { Panel #[anchor: (top, left), exclusive: true] { Text("x") } }
```

```diagnostic
error[ExclusiveNeedsEdge]: `exclusive: true` reserves the panel's thickness along an edge, and this panel has no `edge` (or three anchors): the compositor ignores it. Write `edge: top` (or `bottom`, `left`, `right`), or give a number of pixels with `anchor`
```

## ForKeyNotComparable

The `key` of a `for` has a type that cannot be compared for equality: a list, a function, or a record with one inside.

```byld,fails
View Main() {
    for row in [[1], [2]] key row { Text("x") }
}
```

```diagnostic
error[ForKeyNotComparable]: a `for` key has to be something two values can be compared by, and `List<Int>` is not: key by a `Str`, an `Int`, a `Bool`, an enum or a record of those, for example `key item.id`
```

## GradientShapeMismatch

A `with` after a conditional of gradients whose leaves cannot be blended stop for stop: they differ in kind, or in how many colour stops they write.

## IconNameNotStatic

*A warning*: the program still runs.

A `Text` in an icon font whose name the check cannot work out. The bundle then ships the whole font, unless the manifest lists the names in `icons_include`.

## ImportAfterView

A `use` declaration appeared after the first `View`, imports are legal only at file top.

## ImportShadowsBuiltin

`use pkg.{Button}` would hide the built-in `Button` for the whole file; the package's view is reached as `pkg.Button` instead.

## ImpureConstant

A top-level constant writes state, reaches a controller, or reads a name that is none of the constants and functions: it is shared by every view, so it must be a pure expression.

```byld,fails
let started = now()
View Main() { Text(started) }
```

```diagnostic
error[ImpureConstant]: constant `started` must be pure: it may read only other constants and call functions, and may not assign or reach a controller, because it is shared by every view
```

## ImpureFunction

A top-level `fn` writes state, reaches a controller, or reads a name that is not one of its parameters. It is shared by every view, so it must be a pure expression of its parameters.

```byld,fails
fn stamp() -> Str => now()
View Main() { Text(stamp()) }
```

```diagnostic
error[ImpureFunction]: function `stamp` must be pure: it may read only its parameters (and call other functions), and may not assign or reach a controller, because it is shared by every view
```

## IndexOutOfRange

*A warning*: the program still runs.

An index written as a number that no item of the list written beside it has: `xs[5]` where `xs` is a `let` of one item. Reads as nothing.

```byld,fails
View Main() {
    let xs = [1]
    Text("{xs[5] + 1}")
}
```

```diagnostic
warning[IndexOutOfRange]: index 5 is not in a list of 1 item(s), so this reads as nothing: use `xs[i] ?? fallback`, or check `xs.len` first
```

## IntrinsicShadowed

A `View` is named like a built-in element. The built-in always wins, so the view could never be drawn.

```byld,fails
View Row() { Text("x") }
```

```diagnostic
error[IntrinsicShadowed]: view `Row` shadows the built-in intrinsic of the same name
```

## InvalidAnimation

A curve call had a malformed argument list: a missing duration, a non-numeric value, or an unknown parameter name.

```byld,fails
View Main() {
    var n = 0
    every 0ms => { n++ }
    Text("a")
}
```

```diagnostic
error[InvalidAnimation]: `every 0ms` never fires: a timer needs a duration above zero, such as `every 1s`
```

## InvalidAttributeCombination

Two attributes of one element that disagree, or one that means nothing without another: `row_gap` without `wrap`, `overflow` without `max_lines`, `lines` with `min_lines`.

```byld,fails
View Main() { Panel #[edge: top, anchor: (top, left)] { Text("x") } }
```

```diagnostic
error[InvalidAttributeCombination]: `edge` already anchors the panel (`edge: top` is `anchor: (top, left, right)`): write one of them
```

## InvalidGridTemplate

A `Grid` `columns:`/`rows:` template string could not be parsed. Accepted tracks are `Nfr` (flexible), a bare number (fixed px), `auto`, and `repeat(N, <track>)`.

```byld,fails
View C() {
 Grid #[columns: "1fr bogus"] { Box #[bg: 0xFF0000] {} }
}
```

```diagnostic
error[InvalidGridTemplate]: `1fr bogus` is not a valid grid template; use `Nfr`, a number (px), `auto`, or `repeat(N, <track>)` separated by spaces
```

## InvalidRoutePattern

A route pattern is not a well-formed path: a `:param` segment must name a parameter, `*` must stand alone as the final segment, and a pattern may not repeat a parameter name.

```byld,fails
View Main() {
    var path = "/"
    NavStack(path: path) {
        route "/" { Text("home") }
        route "/user/:id/:id" { Text("a") }
    }
}
```

```diagnostic
error[InvalidRoutePattern]: `/user/:id/:id` is not a valid route pattern: the parameter `id` appears twice
```

## KeyInCanvasFor

A `key` on a `for` inside a `canvas` or a `path`: shapes keep no state, so there is nothing for a key to keep.

```byld,fails
View Main() {
    Canvas #[width: 100, height: 100] { path(fill: 0xFF0000) { move(0, 0)
 for i in [1, 2] key i { line(i, 5) } } }
}
```

```diagnostic
error[KeyInCanvasFor]: a `key` keeps a row's state, and the shapes of a canvas or a path keep none, so here it would change nothing: remove `key`
```

## KeyNeedsFocus

A `key_down` or `key_up` on an element the keyboard cannot reach, so the handler could never run.

```byld,fails
View Main() {
    var n = 0
    Box #[key_down => n++] {}
}
```

```diagnostic
error[KeyNeedsFocus]: `key_down` needs an element the keyboard can reach, and a `Box` written like this is not one; give it a `focus => …` handler, or an `a11y_role` that takes focus (button, toggle_button, switch, checkbox, radio, tab, menu_item, link) together with a `tap`
```

## KeywordAsName

A keyword was written where a name goes (a view, function, type, parameter, `var`/`let` or record field).

```byld,fails
fn when(at: Str) -> Str => at
View Main() { Text("x") }
```

```diagnostic
error[KeywordAsName]: `when` is a keyword and cannot name a function
```

## LayoutInStateBlock

A layout attribute in a state block that names another element's states or `focus_within`: states change what is painted, and layout has been computed before them.

```byld,fails
View Main() {
    Box #[tap => {}] as field {}
    Box { on field.hover { width: 10 } }
}
```

```diagnostic
error[LayoutInStateBlock]: `width` is a layout attribute, and layout is computed before states: a block on `field.hover` only changes what is painted. Use `translate` or `scale` to move or resize it on screen, or a `var` to change the layout
```

## LayoutPropNotAnimatable

A `with` clause attached an animation to a layout property that has no motion: a keyword, a text shaping property, a flex share or a grid placement. The sizes, the gap, and the padding and margin families do move; the message says why this one cannot.

```byld,fails
View Main() {
    var on = false
    Box #[grow: on ? 1 : 0 with anim.spring()] { Text("a") }
}
```

```diagnostic
error[LayoutPropNotAnimatable]: `grow` is a layout property and cannot be animated with `with`: it is a share of the free space among siblings; animate the `width` or `height` it divides instead
```

## LetInElementBody

A `let` written inside an element's braces. A `let` goes at the top of the view, or inside a `when` or `for` body.

```byld,fails
View Main() {
    Column {
        var n = 3
        Text("{n}")
    }
}
```

```diagnostic
error[LetInElementBody]: `var n` is written inside `Column`, where it would not be a variable: put it at the top of the view
```

## MeasureFeedback

An element's `on measure` writes a binding its own layout-class property reads, the statically visible half of the cycle rule. The safe and common use, a parent measuring itself so a *child* can size to it, is untouched: the dependency runs one way and settles in a frame. This diagnostic is only the shape that cannot settle, where the measured size is an input to the measurement.

```byld,fails
View Main() {
    var w: Float = 100.0
    Column #[width: w, height: 100] {
        on measure => w = it.w
    }
}
```

```diagnostic
error[MeasureFeedback]: `on measure` writes `w`, which this element's own `width` reads: measuring a size that depends on the measurement cannot settle. Move the consumer into a child, which is what the measured size is for
```

## MisplacedAnchorTail

An anchor tail (`width: match(ref)`, `dismiss =>`) written where there is no anchor to resolve it against, or naming an element that is not tagged `as`. Both tails read the anchor's resolved rect: one to take its width, the other to know which press to ignore. On an element that anchors to nothing there is no rect, so the property would quietly do nothing, and a `width:` or a `dismiss =>` that quietly does nothing reads as a bug in the panel rather than as the mistake it is.

```byld,fails
View Main() {
    Box #[width: match(menu), height: 10]
}
```

```diagnostic
error[MisplacedAnchorTail]: `width: match(menu)` needs an `anchor_to:` on the same element: the width comes from the anchor's resolved rect, and an element that anchors to nothing has no rect to read
```

## MisplacedNavCase

A `route`/`tab` case was written outside its navigation container (`route` belongs to a `NavStack`, `tab` to a `NavHost`).

```byld,fails
View M() { tab "/a" { x } }
```

```diagnostic
error[MisplacedNavCase]: `tab` is only valid directly inside a `NavHost`
```

## MisplacedProvide

`provide` written somewhere other than the top of a view's body: it gives values to everything the view draws, so it cannot depend on a `when` or a `for`.

```byld,fails
context Tabs { current: Int = 0 }
View Main() {
    var on = true
    when on {
        provide Tabs(current: 1)
    }
    Text("a")
}
```

```diagnostic
error[MisplacedProvide]: `provide Tabs` goes at the top of a view's body, not inside a `when`, a `for` or an element: it gives values to everything the view draws
```

## MisplacedSurface

A `Window` written where a surface may not be: inside another element rather than at the top of the program.

```byld,fails
View Main() {
    Column {
        Window #[title: "B"] { Text("x") }
    }
}
```

```diagnostic
error[MisplacedSurface]: a `Window` goes at the top of the program, not inside another element; to show a panel from inside one, use an `Overlay`
```

## MissingAnnotation

A required type annotation is missing (`View` params and `fn` signatures must be annotated).

```byld,fails
fn f(x) => x
View Main() { Text("{f(1)}") }
```

```diagnostic
error[MissingAnnotation]: missing type annotation on function parameter
```

## MissingParam

A call leaves out a parameter that has no default.

```byld,fails
View Item(leading: Slot) { Row { leading } }
View Main() { Item() }
```

```diagnostic
error[MissingParam]: missing required argument `leading` for view `Item`
```

## MissingShapeParam

A shape command is missing a required geometry parameter (e.g. an `arc` without `r`).

```byld,fails
View V() { Canvas #[width: 10, height: 10] { arc(cx: 1, cy: 1) } }
```

```diagnostic
error[MissingShapeParam]: `arc` is missing its required `r` parameter
```

## MixedTernaryPrecedence

*A warning*: the program still runs.

`a || b ? x : y`: `?:` binds tighter than `||` and `&&` in Byld, so this is `a || (b ? x : y)`, which is usually not what the writer meant.

```byld,fails
View Main() {
    var a = false
    var b = true
    Text(a || b ? "on" : "off")
}
```

```diagnostic
warning[MixedTernaryPrecedence]: `?:` binds tighter than `||`, so this reads `a || (b ? x : y)`; write the parentheses you mean
```

## MorphMemberKind

A `morph:` sequence mixes body paths with other shapes, or holds a `path(d: …)`. Body paths morph command by command on the CPU; every other shape morphs as a distance field on the GPU. One sequence cannot be both.

```byld,fails
View Main() {
            Canvas #[width: 300, height: 200, morph: 0.5] {
                path(d: "M0 0 L10 0 L0 10 Z", fill: 0xFF3366CC)
                circle(cx: 50, cy: 50, r: 40, fill: 0xFF3366CC)
            }
        }
```

```diagnostic
error[MorphMemberKind]: `path(d: …)` cannot morph: write the path with a body of `move`/`line`/`quad`/`cubic`/`close` commands, which morph command by command
```

## MorphPathMismatch

Two neighbouring paths in a `morph:` sequence do not have the same command structure. Paths morph command by command, so the `k`-th command of one has to be the same kind as the `k`-th of the next. When they are not there is no canonical in-between, and falling back to a cut would look like an easing bug, so it is refused, naming the first command that disagrees.

```byld,fails
View Main() {
            Canvas #[width: 300, height: 200, morph: 0.5] {
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) line(0, 100) close() }
                path(fill: 0xFF3366CC) { move(0, 0) line(100, 0) cubic(50, 50, 20, 80, 0, 100) close() }
            }
        }
```

```diagnostic
error[MorphPathMismatch]: path 1 of this morph cannot follow the one before it: command 2 is `cubic` here and `line` there; paths morph command by command, so each needs the same commands in the same order, or set `morph_mode: resample` on the `Canvas` to morph outlines of any structure
```

## MorphSubpathMismatch

Two neighbouring paths in a `morph_mode: resample` sequence have a different number of subpaths. Resampling finds a correspondence between two outlines, not between a ring and a hole that one of them lacks: which of three holes becomes nothing is a guess, and a morph that guesses looks broken.

## NameCollision

A name became ambiguous, two imports (or an import and a local view) bind the same bare name. Resolution is deterministic and order-independent, so ambiguity is an error demanding an explicit alias.

```byld,fails
View Main() {
    var x = 1
    var x = 2
    Text("{x}")
}
```

```diagnostic
error[NameCollision]: `x` is declared twice in this view, as a `var` and as a `var`: the second would hide the first, so give one of them another name
```

## NavCaseRequired

A navigation container holds something other than its own cases (a `NavStack`'s children are `route` blocks, a `NavHost`'s are `tab` blocks, nothing else).

```byld,fails
View Main() {
    var path = "/"
    NavStack(path: path) {
        route "/" { Text("home") }
        Text("a")
    }
}
```

```diagnostic
error[NavCaseRequired]: a `NavStack` holds `route` blocks only; found the element `Text`
```

## NonDataControllerArg

A controller call is passed something that is not data: a `Signal`, a memo, a callback, a theme or another controller handle. A controller runs on another thread and receives values only; pass the value the signal holds.

## NonDataViewProp

A native view's prop is given something that is not data: a `Signal`, a memo, a callback, a theme or a controller handle. A native view receives values, read again each frame, as a built-in element's prop does; pass the value the signal holds.

## NonExhaustive

*A warning*: the program still runs.

A `match` that leaves a case out, or a chain of `when`s over an enum that does. Naming the cases it forgets is the reason to have `match`.

```byld,fails
enum S { a, b }
View Main() {
    var s = S.b
    Text(match s { .a => "a" })
}
```

```diagnostic
error[NonExhaustive]: this `match` leaves out `.b`: write an arm for each, or end with `else => …` for the rest
```

## NoneNeedsOptional

`none` compared with something that did not declare it: only a parameter written `= none`, or `content`, can be absent.

```byld,fails
View Main() {
    var n = 0
    when n != none { Text("x") }
}
```

```diagnostic
error[NoneNeedsOptional]: `n` cannot be compared with `none`: only a parameter declared with `= none` (`on_tap: Fn() = none`, `icon: Slot = none`), or `content`, can be absent
```

## NoneNotHandled

A value that may be none (`color: Color?`) read where a value is wanted, outside `??` and a `!= none` branch.

```byld,fails
View Pill(n: Int? = none) { Text("{n}") }
View Main() { Pill() }
```

```diagnostic
error[NoneNotHandled]: `n` may be none here. Give it a value for when it is: `n ?? fallback`, or read it where it is known, inside `when n != none { … }` or `n != none ? a : b`
```

## NotAStyle

A `..` spread's operand did not resolve to a `style { … }` value, e.g. `..x` where `x` is not a `let`-bound style.

```byld,fails
View Main() {
    let n = 5
    Box #[..n] { Text("a") }
}
```

```diagnostic
error[NotAStyle]: `..` can only spread a `style { … }` value
```

## NotAnimatable

A property that has no continuous interpolation was given a `with` clause. Distinct from [`LayoutPropNotAnimatable`](#layoutpropnotanimatable), and the distinction is the whole point of the diagnostic: `ngon`'s `n` is paint-class, it moves no geometry and costs no relayout, it simply has no meaningful value between 5 and 6. The message therefore names the construct that *does* change shape over time rather than only refusing.

```byld,fails
View Main() {
    let x = 1 with anim.spring()
    Text("{x}")
}
```

```diagnostic
error[NotAnimatable]: a `with` curve only moves an attribute's value, so on `x` it does nothing: write it on the attribute that shows it, as in `#[opacity: x with anim.spring()]`
```

## NotAssignable

An assignment (`=`, `+=`, `++`, …) whose target is not a `var`: a parameter, a `let`, a loop variable or a value.

```byld,fails
View Main() {
    on mount => { 5 = 1 }
    Text("x")
}
```

```diagnostic
error[NotAssignable]: this cannot be written to: only a `var`, or a `bind` parameter, can be assigned, not a `let`, a parameter, a loop variable or a literal
```

## OpeningSizeIsStatic

*A warning*: the program still runs.

A window's `width` or `height` that reads a value that changes: they are only the size the window opens at.

```byld,fails
View Main() {
    var w = 400
    Window #[title: "A", width: w, height: 200] { Text("x") }
}
```

```diagnostic
warning[OpeningSizeIsStatic]: `width` is only the size the window opens at, because after that the size is the user's: writing it later does not resize the window
```

## PackageCycle

Packages that depend on one another in a circle.

## PanelTakesNoKeys

*A warning*: the program still runs.

A `TextField` or a `key_down` handler inside a panel that never has the keyboard.

```byld,fails
View Main() {
    var q = ""
    Panel #[edge: top] { TextField #[bind: q] }
}
```

```diagnostic
warning[PanelTakesNoKeys]: a `TextField` in a panel that has `keyboard: none` (the default) can never receive a key: write `keyboard: on_demand` on the panel
```

## PathMustStartWithMove

A `path { … }` body's first command is not a `move`. A path has to start somewhere. Choosing for the author, the canvas origin, wherever the previous path ended, produces a shape that is almost right, which is worse than one that does not compile: a stray triangle from the corner of a chart is a bug people stare at.

```byld,fails
View Main() {
    Canvas #[width: 100, height: 100] {
        path {
            line(x: 10, y: 10)
        }
    }
}
```

```diagnostic
error[PathMustStartWithMove]: a `path` body starts with `move(x, y)`: the first command says where the path begins, and every command after it draws from there
```

## PathStrokeUnsupported

A `path(d: …)` command carries stroke properties. A path written as `d` is only filled; stroke it by drawing it with the shape commands, or give `d` an outlined path.

```byld,fails
View Main() { Canvas #[width: 100, height: 100] {
            path(stroke: 0xFF0000, dash: (4, 2)) { move(0, 0) line(50, 50) }
        } }
```

```diagnostic
error[PathStrokeUnsupported]: a `path(d: …)` renders fills only, and no path takes `dash`: write the path as a body (`path(stroke: …) { move(…) … }`) to stroke it, or use `arc`/`line`/`bezier` for a dashed line
```

## PredicateNotBool

A `filter`/`when` predicate did not evaluate to `Bool`.

```byld,fails
View Main() {
    let xs = [1, 2]
    Text("{xs.filter(x => x + 1).len}")
}
```

```diagnostic
error[PredicateNotBool]: this predicate must evaluate to `Bool`
```

## PrivateName

A name was used from another package that the package keeps private (`private View`, `private fn`, `private let`, `private enum`, `private type`).

## Project

A failure of the project rather than of a file: a broken `byard.toml`, a dependency that cannot be read, a damaged lock file, or a program with no `View`.

```byld,fails

```

```diagnostic
error[Project]: the program declares no `View`
```

## RecreatesSurface

*A warning*: the program still runs.

A panel's `namespace` or `screen` reads a value that changes: the protocol cannot change them, so a change destroys and makes the surface again.

```byld,fails
View Main() {
    var name = "bar"
    Panel #[edge: top, namespace: name] { Text("x") }
}
```

```diagnostic
warning[RecreatesSurface]: `namespace` is fixed when a panel is made, so when this value changes the panel is destroyed and made again, with its state: that is rarely meant
```

## RecursiveFunction

A function calls itself, directly or through others. Functions are inlined where they are called, so a cycle would never finish lowering.

```byld,fails
fn a(n: Int) -> Int => b(n)
fn b(n: Int) -> Int => a(n)
View Main() { Text("{a(1)}") }
```

```diagnostic
error[RecursiveFunction]: function `a` calls itself; functions are inlined where they are called, so recursion is not supported
```

## RecursiveView

A view that draws itself, directly or through others, with no `when` or `for` in between to stop it: drawing it would never end.

```byld,fails
View A() { B() }
View B() { A() }
View Main() { A() }
```

```diagnostic
error[RecursiveView]: recursive view cycle without a `when`/`for` guard: A → B → A
```

## ReservedControllerName

An app registered a controller under a name the framework reserves for one of its own capabilities.

## ShapeMismatch

A value written into a `var` of a record type does not have that type's shape: a reply the server sent without a field the type requires. Raised at runtime, where the value arrives, once per assignment.

## ShapeOutsideCanvas

A shape command (`arc`, `circle`, `line`, `rect`, `path`, `bezier`, canvas `text`) was used outside a `Canvas` body.

```byld,fails
View V() { arc(cx: 24, cy: 24, r: 20) }
```

```diagnostic
error[ShapeOutsideCanvas]: shape command `arc` is only valid inside a `Canvas` body
```

## SizeOnStretchedAxis

*A warning*: the program still runs.

A panel's `width` or `height` on an axis its anchors stretch: the compositor sizes it, so the value does nothing.

```byld,fails
View Main() { Panel #[edge: left, width: 64, height: 5] { Text("x") } }
```

```diagnostic
warning[SizeOnStretchedAxis]: this panel is anchored to top and bottom, so it spans that axis and the compositor sets its height: the value does nothing. Drop it, or anchor fewer edges (without `top` or `bottom`)
```

## SlotArgument

A value given to a `Slot` parameter, or a block of elements given to a parameter that is not one, or a block anywhere but a call's arguments.

```byld,fails
View Item(leading: Slot) { Row { leading } }
View Main() { Item(leading: 5) }
```

```diagnostic
error[SlotArgument]: `leading` of `Item` is a `Slot`: give it a block of elements, `leading: { … }`, not a value
```

## StoreProvided

`provide` of a `store`, which has the one instance the program has.

```byld,fails
store Settings { dark: Bool = false }
View Main() {
    provide Settings(dark: true)
    Text("a")
}
```

```diagnostic
error[StoreProvided]: `Settings` is a `store`: the program has one of it, which a view reads and writes with `inject Settings as …`, so there is nothing to provide: delete the `provide`
```

## StringNestingTooDeep

A string nests interpolated strings more than three levels deep. Move the inner part into a `let`.

```byld,fails
View Main() {
    var n = 0
    var on = true
    a "{"{"{"{"{"{"x"}"}"}"}"}"}" b
}
```

```diagnostic
error[StringNestingTooDeep]: string interpolation nested deeper than 3 levels
```

## StrokeInFusionGroup

*A warning*: the program still runs.

A shape after the first in a fusion group carries stroke properties of its own. The fused outline is the edge of the union, drawn once, from the first shape's stroke; a later shape's stroke would run through the inside of the shape the fusion makes seamless, so it is ignored, and the program still draws.

```byld,fails
View Main() {
    Canvas #[width: 140, height: 48, fuse: 16] {
        circle(cx: 24, cy: 24, r: 18, fill: 0x6750A4)
        circle(cx: 60, cy: 24, r: 14, fill: 0x6750A4, stroke: 0xFFFFFF, stroke_width: 2)
    }
}
```

```diagnostic
warning[StrokeInFusionGroup]: `stroke` is ignored here: a fused `Canvas` has one outline, drawn from the first shape's stroke, not one per shape
```

## SurfaceForNeedsKey

*A warning*: the program still runs.

A `for` without a `key` whose body makes a window: every change to its list would move windows, with their state, to other items.

```byld,fails
View Main() {
    for s in screens { Panel #[screen: s, edge: top, namespace: "bar"] { Text(s.name) } }
}
```

```diagnostic
warning[SurfaceForNeedsKey]: this `for` makes a window per item and has no `key`, so every change to its list moves windows, with their state, to other items: write `key` after the list, for example `for t in tabs key t.id`
```

## SvgTooComplexForMssdf

A `VectorIcon`'s SVG has more path segments than the icon pipeline takes (500 by default). Simplify the SVG.

## SvgUnsupportedFeatures

A `VectorIcon`'s SVG paints with a gradient, a pattern or a filter. A vector icon is one flat colour; use an `Image` for the rest.

## TextUsedAsType

`Text` was used where a type is expected; `Text` is the text *view*, the scalar string type is `Str`.

```byld,fails
View Card(label: Text) { Text("x") }
View Main() { Card("a") }
```

```diagnostic
error[TextUsedAsType]: `Text` is a view, not a type; use `Str` for strings
```

## TooManyGroupMembers

A shape group holds more than 8 shapes. The limit is what keeps the cost of drawing a group bounded, and drawing eight of nine would hide the mistake, so the program is refused instead.

```byld,fails
View Main() {
    Canvas #[width: 200, height: 48, morph: 0.0] {
        ngon(cx: 0, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 10, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 20, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 30, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 40, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 50, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 60, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 70, cy: 24, r: 8, n: 5, fill: 0x6750A4)
        ngon(cx: 80, cy: 24, r: 8, n: 5, fill: 0x6750A4)
    }
}
```

```diagnostic
error[TooManyGroupMembers]: a shape group holds at most 8 shapes; this one holds 9. Split it into several groups, or drop a shape
```

## TooManyKeyframes

An `anim.keyframes(…)` sequence has more than 8 steps.

```byld,fails
View Main() {
    Canvas #[width: 400, height: 10] {
        line(x1: 0, y1: 5, x2: anim.keyframes(0%: 0, 12%: 10, 25%: 20, 37%: 30, 50%: 40, 62%: 50, 75%: 60, 87%: 70, 100%: 80, duration: 1s), y2: 5, stroke: 0xFF0000)
    }
}
```

```diagnostic
error[TooManyKeyframes]: `anim.keyframes` takes at most 8 steps, found 9
```

## TypeMismatch

A binary operator was applied to incompatible operand types: e.g. `1 < "a"`, ordering (`<`) on a `List`/`Record`, or a `+` between a `Str` and a `List`.

```byld,fails
View Main() {
    Text("{!5}")
}
```

```diagnostic
error[TypeMismatch]: `!` takes `Bool`s, and this is a `Int`: compare it, as in `x != 0`
```

## UncheckableInject

*A warning*: the program still runs.

An `inject T as x` named a type no ambient provider declares, in a context that has **no controller registry at all** (`byard check`, a headless test). A warning, not an error, and the distinction is the point: the Rust half of a two-layer app registers its controllers at run time (`App::provide`), so a static check of the `.byd` file alone genuinely cannot know whether `T` exists. Reporting it as an error would make `byard check` fail on every correct two-layer app; reporting nothing would let a typo through in silence. Saying exactly what is unknown, and carrying on, is the only honest third option.

```byld,fails
View Main() {
    inject Greeter as greeter
    Text("x")
}
```

```diagnostic
warning[UncheckableInject]: `Greeter` is not an ambient value here; it must be a controller the Rust half registers with `App::provide`, which this check cannot see
```

## UnexpectedChar

A character that is no part of the language: no token starts with it, outside a string or a comment.

```byld,fails
View Main() {
    Text("a") @
}
```

```diagnostic
error[UnexpectedChar]: unexpected character
```

## UnexpectedChildren

Children were given to a childless built-in element (`Text`, `Spacer`, `Image`, `TextField`, `Toggle`, `Slider`).

```byld,fails
View Main() {
    Text("a") { Text("b") }
}
```

```diagnostic
error[UnexpectedChildren]: `Text` takes no children
```

## UnexpectedToken

The parser expected one thing and found another. The message says what it expected.

```byld,fails
View Main()
```

```diagnostic
error[UnexpectedToken]: unexpected token, expected '{'
```

## UnknownAnchor

An overlay's `anchor_to:` names an element that is not tagged `as` before it in the same view. Reported rather than ignored because the failure is otherwise silent: a misspelt anchor produces an overlay that simply sits wherever layout left it, which reads as a placement bug rather than a typo.

```byld,fails
View Main() {
    Overlay #[modal: false] {
        Box #[anchor_to: "me"] as me { Text("self") }
    }
}
```

```diagnostic
error[UnknownAnchor]: no element tagged `as me` before this overlay; an overlay may only anchor to a name declared earlier in the same view
```

## UnknownAnimation

A `with` clause named a curve that is not `anim.linear`/`ease`/`spring`; the message suggests the closest name.

```byld,fails
View Main() {
    Box #[opacity: 1.0 with anim.sprang()] { Text("a") }
}
```

```diagnostic
error[UnknownAnimation]: unknown animation curve `sprang` (did you mean `spring`?)
```

## UnknownAttribute

A built-in element did not recognize an attribute name (never silently ignored).

```byld,fails
View Main() {
    Text("a") #[bgg: 1]
}
```

```diagnostic
error[UnknownAttribute]: unknown attribute `bgg` (did you mean `bg`?)
```

## UnknownBreakpoint

A responsive block (`on width >= md { … }`) names a breakpoint the project does not declare in `[theme.breakpoints]`. Reported for the same reason an undeclared font is: the alternative is a block that silently never applies, which looks like a layout that ignores the window rather than like a typo.

```byld,fails
View Main() {
    Box { on width >= huge { p: 24 } }
}
```

```diagnostic
error[UnknownBreakpoint]: no breakpoint `huge` is declared; add it to `[theme.breakpoints]` in byard.toml, or write a width in logical pixels
```

## UnknownContext

`provide`, or `inject`, of a name that is no context.

```byld,fails
View Main() {
    provide Nope(current: 1)
    Text("x")
}
```

```diagnostic
error[UnknownContext]: `Nope` is not a context: declare it with `context Nope { … }`, or name a capability, a controller or `Theme`
```

## UnknownControllerMethod

A controller call names a method the controller does not have. It is found when the program runs, when the reply says so: the language does not know the app's Rust types ahead of time.

## UnknownEnumCase

An enum has no case of that name: `Variant.tonl`.

```byld,fails
enum V { filled, tonal }
View Main() { Text("{V.tonl}") }
```

```diagnostic
error[UnknownEnumCase]: enum `V` has no case `tonl` (did you mean `tonal`?)
```

## UnknownField

A record type has no field of that name: `place.nmae` where `place` is a `Place`.

```byld,fails
View Main() {
    let u = { name: "Ada" }
    Text("{u.nmae}")
}
```

```diagnostic
error[UnknownField]: a `{ name: Str }` has no field `nmae` (did you mean `name`?)
```

## UnknownFontFamily

A `font:` names a family the project does not declare in `[assets.fonts]`. Reported for the same reason a misspelt anchor is: the alternative is text that renders perfectly in the wrong face, which reads as a design decision rather than a typo. The fallbacks (a `typo:` token's family, then the system font) are silent by design; only a name nobody declared is an error.

```byld,fails
View Main() {
    Text("x") #[font: display]
}
```

```diagnostic
error[UnknownFontFamily]: no font family `display` is declared; add it to `[assets.fonts]` in byard.toml
```

## UnknownImportSymbol

A selective import or qualified reference named a `View` the package does not export; the message suggests the closest one it does export.

## UnknownMethod

A method call named a method the receiver type does not have; the message suggests the closest one.

```byld,fails
View Main() {
    Text("{3.squared()}")
}
```

```diagnostic
error[UnknownMethod]: `Int` has no method `squared`
```

## UnknownName

An expression read a name nothing declares: a misspelt `var`, a parameter that was renamed. At runtime such a name is its own text, which is how an enum word like `center` works and why a typo in a value drew the typo instead of failing.

```byld,fails
View Main() {
    Text("{nope}")
}
```

```diagnostic
error[UnknownName]: nothing here is named `nope`
```

## UnknownPackage

A `use` (or a qualified `alias.View` reference) named a package that is not in the resolved dependency set.

```byld,fails
use kit
View Main() { Text("a") }
```

```diagnostic
error[UnknownPackage]: unknown package `kit`
```

## UnknownParam

A named argument at a user-`View` call site does not match any declared parameter; the message suggests the closest name.

```byld,fails
View Main() {
    Text("a", nope: 1)
}
```

```diagnostic
error[UnknownParam]: `Text` has no argument `nope`: its attributes go in `#[ … ]`, as in `Text(…) #[nope: …]`
```

## UnknownShapeCommand

A `Canvas` child is not a recognized shape command (`Canvas` children are shape commands only, built-in elements, user views, declarations, and control flow are all rejected).

```byld,fails
View V() { Canvas #[width: 10, height: 10] { Text("no") } }
```

```diagnostic
error[UnknownShapeCommand]: `Text` is not a shape command; `Canvas` children are shape commands only (did you mean `text`?)
```

## UnknownShapeParam

A shape command did not recognize a parameter name (same never-silent contract as for attributes).

```byld,fails
View Main() {
    Canvas #[width: 100, height: 100] {
        circle(cx: 50, cy: 50, r: 10, radius: 4)
    }
}
```

```diagnostic
error[UnknownShapeParam]: `circle` does not recognize parameter `radius`
```

## UnknownStateTarget

`on name.state` where no element tagged `as name` comes before it, or an `anchor_to` or `a11y_described_by` that names one.

```byld,fails
View Main() {
    Box { on nobody.hover { opacity: 0.5 } }
}
```

```diagnostic
error[UnknownStateTarget]: no element in this view is tagged `as nobody`: tag the element whose states this block follows with `as nobody`
```

## UnknownStyleState

An `on <state> { … }` block named a state that isn't one of the four engine-owned interaction states, `hover`/`pressed`/ `focused`/`disabled`.

```byld,fails
View Main() {
    Box { on hovr { bg: 0x333333 } }
}
```

```diagnostic
error[UnknownStyleState]: unknown interaction state `hovr` (expected hover/pressed/focused/disabled) (did you mean `hover`?)
```

## UnknownThemeToken

A `theme.<field>` access named a token the active theme does not declare in any of its color / typography / shape namespaces.

```byld,fails
View Main() {
    inject Theme as t
    Text("x") #[color: t.primry]
}
```

```diagnostic
error[UnknownThemeToken]: theme `byard-base` declares no token `primry` (checked color, typography, shape, and motion namespaces)
```

## UnknownType

A type was written that is not built in and that nothing in the program declares: `Qux`, or `Map<Str, Int>`.

```byld,fails
View Main() {
    var q: Qux = 1
    Text("{q}")
}
```

```diagnostic
error[UnknownType]: `Qux` is not a type: the types are `Int`, `Float`, `Str`, `Bool`, `List<T>`, `Fn(…)`, and the `type`s and `enum`s the program declares
```

## UnknownView

An element name is neither a known built-in element nor a `View` in scope; the message suggests the closest name.

```byld,fails
View Main() { Colum { Text("hi") } }
```

```diagnostic
error[UnknownView]: unknown view `Colum` (did you mean `Column`?)
```

## UnmatchedRoute

A navigation container was pointed at a path no `route` in its table matches. A *runtime* condition, the container keeps showing the last matched route, reported once per distinct path rather than every frame.

```byld,fails
View Main() {
    var path = "/missing"
    NavStack(path: path) {
        route "/" { Text("home") }
    }
}
```

```diagnostic
error[UnmatchedRoute]: no route matches `/missing`; the navigation stays where it is
```

## UnresolvedInject

An `inject T as name` found no ambient `T` in any ancestor scope.

## UnterminatedComment

A `/*` comment was opened but never closed before end of input.

```byld,fails
View Main() { Text("a") }
/* never closed
```

```diagnostic
error[UnterminatedComment]: this `/*` comment is never closed: end it with `*/`
```

## UnterminatedString

A string literal was opened but never closed before end of input.

```byld,fails
View Main() {
    Text("abc)
}
```

```diagnostic
error[UnterminatedString]: unterminated string literal
```

## UntypedReply

*A warning* (lint `untyped-reply`, set in `[lints]` of `byard.toml`).

A field read of what a controller replied, whose shape nothing has checked: `v.count`
where `v` is what `json.parse` gave. The fix is `(v as Reply).count`, with a
`type Reply` that says what the reply should hold; after it, a misspelt field is an
error, and a reply without the field goes to `err`. An `Http` answer's own fields,
`status`, `ok`, `body` and `json`, are known: what is read out of `res.json` is not.

```byld,fails
View Main() {
    inject Json as json
    var n = 0
    on mount => {
        json.parse("\{\}")
            ok v => { n = v.count }
    }
    Text("{n}")
}
```

```diagnostic
warning[UntypedReply]: `count` is read from `v`, whose shape nothing checks
```

## UnusedParam

*A warning* (lint `unused-parameter`, set in `[lints]` of `byard.toml`).

A parameter that its view never reads.

```byld,fails
View Card(label: Str) { Text("x") }
View Main() { Card("a") }
```

```diagnostic
warning[UnusedParam]: parameter `label` of view `Card` is never read: use it, delete it, or start its name with `_` to say it is meant to be unused
```

## UnusedVariable

*A warning* (lint `unused-variable`, set in `[lints]` of `byard.toml`).

A `var` or `let` that nothing reads.

```byld,fails
View Main() {
    let spare = 1
    Text("x")
}
```

```diagnostic
warning[UnusedVariable]: `let spare` is never read: use it, delete it, or start its name with `_` to say it is meant to be unused
```

## UnusedView

*A warning* (lint `unused-view`, set in `[lints]` of `byard.toml`).

A `private View` that nothing in its package draws.

```byld,fails
private View Hidden() { Text("h") }
View Main() { Text("m") }
```

```diagnostic
warning[UnusedView]: `private View Hidden` is never drawn: use it, or delete it
```

## VariationWeightConflict

An element sets `weight:` and a `wght` in `variations:`. Both drive the one `wght` axis, so one would silently win.

```byld,fails
View Main() {
    Text("a") #[weight: bold, variations: (wght: 400)]
}
```

```diagnostic
error[VariationWeightConflict]: `weight:` and `variations: (wght: ..)` both set the weight axis; keep one of them
```

## VectorAssetNotStatic

A `VectorIcon`'s asset argument is not a compile-time-constant handle, so the AOT (`byard build`) packer cannot close the icon set statically. The fix is to make the handle a string literal, or to declare an explicit `[assets.vectors] include = [...]` list in `byard.toml`. A partial atlas is never shipped silently.

## ViewArityMismatch

A user-`View` call supplied more positional content arguments than the callee declares parameters.

```byld,fails
View Badge(label: Str) { Text(label) }
View Main() { Badge("a", "b") }
```

```diagnostic
error[ViewArityMismatch]: view `Badge` declares 1 parameter(s), found 2 positional argument(s)
```

## ViewCallAction

A user-view call carries an action the view cannot receive: a trailing `=> action`, or an event handler in its `#[...]`. A view's behaviour comes in through its `Fn(...)` parameters, so the action would have been dropped silently.

```byld,fails
View Main() {
    var n = 0
    Text("a") => n++
}
```

```diagnostic
error[ViewCallAction]: `Text` cannot be pressed, so `=> action` after it would never run: use a `Button`, or put the action on a `Box` or `Row` as `#[tap => …]`
```

## WrongAttributeSeparator

An attribute used the wrong separator for its kind, a property given `=>`, or an event given `:`.

```byld,fails
View Main() {
    Text("a") #[size = 12]
}
```

```diagnostic
error[WrongAttributeSeparator]: `size` is a property; use `:` not `=`
```
