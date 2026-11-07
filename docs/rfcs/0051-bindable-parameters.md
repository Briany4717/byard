# RFC-0051: Bindable parameters

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-06
- **Last updated:** 2026-11-06
- **Depends on:** RFC-0007 (user-view instantiation; §3 binds a call's
  arguments to parameters, one memo each, and is what this adds a second kind
  of parameter to), RFC-0003 (`bind:` and its write-back, E1), RFC-0018 (the
  group-var model: several readers of one `var`, no copy between them),
  RFC-0004 (the reactive graph this reads straight into), RFC-0019 (callback
  props, which this is the other half of), RFC-0044 (the formatter).

> **Placement:** `docs/rfcs/0051-bindable-parameters.md`

---

## Summary

A view can declare that a parameter is a **binding**: `bind on: Bool`. The
caller does not pass a value, it passes a `var`, `Switch(bind: wifi)`, and the
view reads that `var` and writes it as if it were its own: in a text, in an
action, in the `bind:` of a control, in the `bind:` of another view. There is
no copy and no callback between the two. The child holds the caller's signal,
so a write is seen by every reader in the same frame. Every built-in control
already works this way; this lets a view you write do the same, and it removes
the value-plus-callback pair a component library has to write for every
control it wraps.

A binding has no default. A caller names it with `bind`, as the view did. A
`let`, a literal, an expression or a loop variable where a binding is wanted is
`BindNotAVar`, as it is for a control, and a value where a binding is declared
(or the reverse) is an error that says which. Nothing is added to a program
that declares none.

## Motivation

`TextField #[bind: name]` is two lines shorter than any other way of writing a
field, and it is the way every control in the catalog is written (RFC-0003,
RFC-0018). A view that wraps one cannot be:

```byld
View Field(value: Str) {
    TextField #[bind: value]        // error[BindNotAVar]
}
```

A parameter is a copy of what the caller passed (RFC-0007 §3), so a write would
be lost, and the checker says so. The way out the guide gives is a pair:

```byld
View Field(value: Str, on_change: Fn(Str) = {|t| }) {
    var text = value
    on change(value) => text = value
    TextField #[bind: text, input(t) => on_change(t)]
}
// at every call site
Field(value: name, on_change: {|t| name = t })
```

It works, and it costs three things. Every call site repeats the variable
twice. The view keeps a second copy of the state (`text`) that follows the
caller's through an `on change`, so for a frame the two can disagree, and a
caller that changes the `var` and reads the copy in the same action reads the
old one. And every view in a component library (`Switch`, `Checkbox`,
`TextField`, `Slider`, the Material package of RFC-0050) is spelt that way, so
the library's call sites do not read like the catalog's. The spike for that
package found this as its third gap: `m.Switch(bind: wifi)` is `UnknownParam`
today, and the workaround was "two lines at every call site, an inner copy that
can lag the caller's var by a frame".

The pieces to do better already exist. A `var` is a signal the interpreter
holds by id (RFC-0004); a control's `bind:` is that id; a callback is a name
the caller wrote. What is missing is only a way for a parameter to *be* the
id and for the caller to say so.

## Guide-level explanation

```byld
View Switch(bind on: Bool, label: Str = "") {
    Row #[gap: 12, align: center] {
        Toggle #[bind: on]                  // a binding can be bound further
        Button(label) => on = !on           // and written in an action
    }
}

View Main() {
    var wifi = true
    Column #[p: 24, gap: 12] {
        Switch(bind: wifi, label: "Wi-Fi")  // the caller passes a var
        Text(wifi ? "on" : "off")
    }
}
```

`bind on: Bool` marks `on` as a binding. Inside `Switch` the name `on` is used
as a `var` is: read it anywhere on screen and the place follows it, write it in
an action with `=`, `+=` or `++`, give it to a control's `bind:`, pass it on to
another view's binding. Outside, the caller writes `bind:` where it would write
a name, and gives a `var` of its own. Tapping the `Button` or the `Toggle`
changes `wifi`, in `Main`, and the `Text` under the switch follows in the same
frame.

### Several bindings

With exactly one binding, `bind: wifi` is enough. With several, each is named,
with the word `bind` before the parameter's name, as it was declared:

```byld
View FormField(bind text: Str, bind valid: Bool, label: Str) {
    Column {
        Text(label)
        TextField #[bind: text, input(t) => valid = t.len > 2]
    }
}

View Main() {
    var name = ""
    var ok = false
    FormField(label: "Name", bind text: name, bind valid: ok)
}
```

`bind name: someVar` is the long form and always allowed, also for a view with
one binding. `bind: someVar` is its short form, and an error for a view with
several, since it would not say which.

### Forwarding

A binding passed on is the same binding. Three views deep, the last one writes
the first one's `var`:

```byld
View Outer() { var on = false  Middle(bind: on)  Text("{on}") }
View Middle(bind flag: Bool) { Inner(bind: flag) }
View Inner(bind flag: Bool) { Button("flip") => flag = !flag }
```

### What the checker says

```byld,fails
View Main() {
    var wifi = true
    let off = !wifi
    Switch(bind: off)           // BindNotAVar: `off` is a `let`
    Switch(bind: true)          // BindNotAVar: a literal
    Switch(on: wifi)            // the parameter `on` is a binding: write `bind: wifi`
}
```

A parameter of the view that is not a binding still cannot be bound, and the
error that has said "a parameter is a copy" since RFC-0003 now names the fix:
declare it `bind value: Str`.

## Reference-level explanation

### 1. Declaration

```
param := ("bind")? IDENT ":" type ("=" expr)?
```

`bind` is contextual. It is a keyword only when another name follows it, so
`View V(bind: Int)` still declares an ordinary parameter called `bind`; the
checker then rejects that name (below), because at the call site `bind:` is
the short form and the two readings would collide.

A binding parameter is checked at its declaration, all four as
`BadBindingParam` naming the parameter:

- it has **no default**. A default would have to be a `var` the view makes for a
  caller who passed none, which is a second meaning (an optional binding, with
  state that belongs to the view and not to the caller) for a parameter whose
  point is that the state is the caller's. A view that wants that writes the
  `var` itself and takes a plain parameter for the initial value;
- it has a **type**, the same rule every parameter has (`MissingAnnotation`
  already says it, and the binding's type is what the caller's `var` is
  compared with);
- it is **not a callback**: a binding holds a value, and `Fn(...)` parameters
  keep their own path (RFC-0019);
- it is not called **`bind`**.

### 2. The call site

In the parentheses of a view call, an argument is one of

```
IDENT ":" expr            // a value, by name
expr                      // a value, by position
"bind" ":" expr           // the view's only binding
"bind" IDENT ":" expr     // the binding called IDENT
```

`bind IDENT ":"` is two names in a row followed by a colon, which is nothing
else in the grammar, so it needs no look at the declaration to parse. In
`#[...]` attributes only `bind: expr` is accepted: an attribute list is for
styling a call, and a second spelling for the long form there would be one more
place for the formatter and the checker to agree. A view with several bindings
takes them in the parentheses.

A binding is never positional: positional arguments fill the value parameters
in order, exactly as they skip a callback, so adding a binding to a view does
not move the arguments of its existing calls.

The argument of a binding is one of:

- a `var` of the view making the call;
- a binding of the view making the call (forwarding);
- `theme.dark`, the scheme flag, which `Toggle #[bind: theme.dark]` already
  accepts (RFC-0022).

Anything else is `BindNotAVar`, with the same four kinds the controls have:
a parameter that is a value (the message points at `bind` as the fix), a
`let`, a literal, and "anything else that is not a `var`" (a loop variable, a
lambda's parameter, an event's value), plus a fifth, an **expression**, for
`bind: a + b` and `bind: row.done`. The message reads `Switch(bind: off)`
where it is a view call and `TextField #[bind: off]` where it is a control.

Three more errors cover the forms:

| Error | When |
|---|---|
| `BindArgMismatch` | `Switch(on: wifi)` where `on` is a binding, and `bind label: x` where `label` is a value. It says which the parameter is, and what to write. |
| `BindNeedsName` | `bind: x` for a view with two or more bindings. It lists them. |
| `BindOutsideViewCall` | `bind name: x` in a function call, a tuple, or an intrinsic's content: the word means something only for a user view. |

A binding the call does not pass is `MissingParam`, as any required parameter,
and `UnknownParam` covers `bind: x` for a view with no binding. Passing a `var` of the wrong type is
`ArgumentTypeMismatch`, with the types of both.

### 3. What a binding is inside the view

When a view is instantiated (RFC-0007 §2) a value parameter is pushed into the
instance's environment as a memo over the *caller's* scope. A binding is
pushed as the **caller's signal**, `Value::Signal(id)`, under the parameter's
name. That is the whole mechanism: a `var` is a `Value::Signal` already, so
every place that treats a name as a `var` (a read, an assignment, `bind:`, a
`page:` or `focused:` that names a `var`, a captured callback, a `for`'s
environment snapshot) treats a binding as one, with no new case and no extra
indirection. It costs a lookup at instantiation and nothing in a frame.

Consequences, which the tests hold:

- **No copy, no lag.** There is one cell. A write in the child is a
  `write_signal` on the caller's signal, and the caller's readers are marked by
  it in the same synchronous cascade (RFC-0004). In an action,
  `on = !on` followed by a read of `on` reads the new value, in the child and
  in the caller.
- **Reactive in the child.** A read of the binding subscribes the reading scope
  to the caller's signal directly. There is no memo between.
- **Forwarding is free.** The caller's environment holds a `Signal` whether the
  name there is a `var` or a binding, so the argument is looked up and passed.
  Three views deep is one id.
- **Resolved in the caller.** The argument is resolved in the caller's
  environment *before* the callee's parameters are pushed, as value arguments
  are, so `Switch(bind: on)` from a view that has its own `on` finds that one.
- **Per instance, as ever.** Two calls bound to two `var`s are two cells; two
  bound to the same `var` share it. This is the group-var model of RFC-0018 for
  any view.

The checker treats a binding as a `var` (a name `bind:` may take) and as its
declared type. A binding in a style value or an animation reads like a `var`.

### 4. Loops

A `for` row's variable is a copy of the list's element in a slot of the row
(RFC-0018), and `row.done` is a field of that copy. Neither is a place a write
can reach: there is no assignment to a list element or to a record's field in
`byld` (RFC-0027), for a control or for anything else. So `Switch(bind: row)`
and `Switch(bind: row.done)` are `BindNotAVar`, the first of the "anything else"
kind and the second of the expression kind, and the message says the cell the
row was read from is what to bind: a `var` in the row's own view. The way to
edit a list's item from a row is the one that exists, an action that replaces
the list, passed as a callback; this RFC does not widen it (see Unresolved).

A `var` declared *in* the row's view, or in a view the row calls, is a
`var`, and binds as any other.

### 5. Lifetimes

A `var` lives in a cell that the interpreter keeps for the whole run: signals
are not recycled, and what is retired when a `when` branch, a `for` row or a
route goes away is the *scopes* that read them (RFC-0026, `retire`). So a
binding cannot dangle:

- If the view that owns the `var` is unmounted while a child that holds it
  lives (an action captured by something kept longer, a timer), the child's
  reads return the last value and its writes are stored in the cell. Nothing
  reads it any more, nothing is reported and nothing panics. If the owner is
  mounted again, it starts fresh, as any instance does (state belongs to an
  instance), with a new cell, which the old child does not hold.
- If it is the *child* that goes away, its scopes are retired, the caller's cell
  is untouched, and the caller keeps its value.
- A `for` row's own slot is the one signal that is reset when its row is
  retired, and a row's variable cannot be bound (§4), so the case does not
  arise.

The decision is "keep working, quietly", not an error, because an unmount is an
ordinary event in a program (a `when` flipping), and a diagnostic at run time
for it would be the only place the language blames a program for a thing it
cannot see in the source.

### 6. Hot reload

An instance's state is kept across a reload that does not change the view's
parameters, `var`s, `let`s or `inject`s (the guide's rule). A binding is a
parameter, and a view's shape is its parameter names and its `var`s, so adding or
removing one resets that view's instance, as any parameter does. Editing the body
of `Switch`, or the caller's, keeps the caller's `var` and every binding to it,
and turning a value parameter into a binding (or back) in the same edit as its
callers is a patch like any other: every caller of a changed view is lowered
again, which looks the same `var` up again.

### 7. Tooling

- **Checker.** As above, in `infer`: the binding is an `Origin::Var`, the call
  is checked against the callee's parameters, and a program that declares no
  binding runs none of it.
- **Formatter** (RFC-0044). `bind` is a name followed by a name: it takes one
  space, as `var x` does, in a declaration and in a call. Nothing else changes.
- **Language server.** Hover on a binding, at its declaration or at any use in
  the view, shows `bind on: Bool`; a package view's signature in hover lists
  `bind on`; completion of a call's arguments inserts `bind on: ` for a
  binding. The semantic tokens colour `bind` in a declaration as a keyword and
  the name after it as the parameter.
- **`byard shot`** and the rest of the CLI need nothing: they run the same
  interpreter.

### 8. Cost

A program with no binding pays for a boolean on every parameter and every
argument, and for none of the checks (they are entered only when the callee
has a binding, or an argument is written with `bind`). A program with one pays
one `HashMap` lookup per binding per instantiation and a signal read where it
used to read a memo, which is cheaper.

## Drawbacks

- **A second kind of parameter.** Every parameter used to be a value, and the
  guide's one-line rule, "a parameter is a copy of the caller's value", now
  has an exception. The exception is marked on the declaration (`bind`) and at
  every call, so the reader never has to look up whether a name is a copy.
- **A view that can change its caller's state.** The reader of `Main` no
  longer sees every write to `wifi` in `Main`. The caller opted in at the call
  (`bind:`), which is what keeps this from being the implicit aliasing of a
  mutable reference; a control already asks for exactly this.
- **No default means no optional binding.** A `Switch` that works with or
  without a caller's `var` has to be two views, or take a value and a callback.
  Most uses want the state to be the caller's; the exception is rare enough to
  stay a pattern.
- **A binding cannot be positional.** `Switch(wifi)` is a type error against
  the first value parameter, not a binding. The message for that case says what
  was meant when the argument names a `var`.
- **No list items.** The most requested use after a switch is a checkbox per
  row of a list. It stays the existing pattern for now (Unresolved).

## Rationale and alternatives

- **`Switch(on: bind wifi)`, the marker on the value.** It reads well, but it
  makes `bind` an expression-level prefix operator, and the declaration
  (`bind on: Bool`) and the call would put the word in different places. With
  the word before the name in both, one rule serves: *`bind` introduces the
  name of a binding*. It is also a tokens-in-a-row form with no ambiguity and
  no new operator precedence.
- **`Switch(on: &wifi)` or `$wifi`.** A sigil is a new character in the lexer
  for one construct, and the language has no other pointer-flavoured symbol.
  Neither reads in a guide for people who have not met the idea.
- **Always `bind name:`.** Uniform, and then the catalog's `#[bind: on]` and a
  library's `Switch(bind on: wifi)` would differ for no gain. The short form is
  what a one-binding view (nearly all of them) is read by; it is the same
  spelling a control has, so replacing `Toggle #[bind: x]` with `Switch(bind: x)`
  is a rename.
- **Infer the binding from the callee, so `Switch(on: wifi)` is a binding.**
  Then a call site cannot be read without the declaration, and the mistake of
  passing a value where a binding is declared would silently become aliasing.
  The explicit word is the check.
- **A default that makes a local `var`.** Rejected under Declaration: it is a
  second meaning, and the state would be the view's.
- **Copy-in, copy-out** (write the child's copy back to the caller at the end of
  the action). This is what the value-plus-callback pattern does by hand, and
  the lag and the stale read the Motivation describes come with it.
- **Two-way props as a runtime flag on a value parameter.** The same check can
  not be made statically, and a `let` passed to one would be a no-op.

## Prior art

- **SwiftUI's `@Binding`** is this: a view declares `@Binding var on: Bool`, and
  the caller passes `$wifi`. It is the model for "the child holds the parent's
  state". Swift marks the caller with a `$`; here it is a word, because `byld`
  prefers words, and it is the same word the controls use.
- **Svelte's `bind:`** on a component prop (`<Switch bind:on={wifi} />`) is the
  closest in spelling and in who asks for what: the declaring side exports a
  prop, the caller opts in with `bind:`.
- **Vue's `v-model`** on a component is a value prop and an `update:` event,
  generated; it is the callback pattern with the boilerplate removed, and has
  the copy.
- **React** has none: a value and a callback is the model, which is the
  pattern this RFC replaces for the cases where it is noise.
- **Flutter's `ValueNotifier`** passed down is a binding in all but name.

## Unresolved questions

### Binding a list item or a record field

`for row in rows { Checkbox #[bind: row.done] }` is the form a todo list wants.
It needs a place to write: an index into a `var` list, or an assignment to a
record's field (`rows[i].done = true`), neither of which `byld` has (RFC-0027).
It should be decided together with a way to write them, not here. Until then it
is `BindNotAVar`.

### An optional binding

If a library finds it wants `TextField` with or without a caller's `var`, the
shape could be a default that is a `var` expression (`bind text: Str = ""`),
owned by the instance. The reasons against are above; this is where to start if
the pattern proves common.

### Bindings in `#[...]`

`Switch #[bind: wifi]` is accepted for the short form. Whether `#[bind on: wifi]`
should be, for views with several bindings, depends on whether authors write
their calls in the parentheses or the attribute list; the parentheses are what
the guide shows today.

## Future possibilities

- Binding an item of a list in a `for` (above), the missing half.
- A `bind` on `fn` parameters, for helper actions that write a caller's `var`
  (`fn toggle(bind flag: Bool)`), if actions grow past what a callback says.
- Package views (RFC-0050) using it: `m.Switch(bind: wifi)`,
  `m.TextField(bind: name)`, with the controls inside bound the same way.
