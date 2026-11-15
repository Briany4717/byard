# RFC-0065: Context and optional values

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-14
- **Last updated:** 2026-11-14
- **Depends on:** RFC-0007 (user views and slots), RFC-0019 (callback
  parameters), RFC-0022 (the theme, the first thing a view `inject`s), RFC-0027
  (records, `T?` and `??`), RFC-0038 (`on measure`), RFC-0051 (bindable
  parameters), RFC-0062 (named slots and `none`), RFC-0050 (Byard Material,
  whose wave 2 needed all three).

> **Placement:** `docs/rfcs/0065-context-and-optional-values.md`

---

## Summary

Three things a component library writes around today:

1. **Context.** A view can hand values down to everything it draws, slots
   included, and a view inside reads them with `inject`, the way it reads the
   theme: `provide TabsStyle(variant: variant)` in `Tabs`, `inject TabsStyle as s`
   in `Tab`.
2. **Optional values of any type.** `color: Color? = none` is a parameter that
   may be left out. It is read with `??` or inside `when color != none`, and a
   bare read of it is an error. A `bind` parameter may have a default, and then a
   caller that binds nothing gets a `var` the view keeps for itself.
3. **Where an element is.** `on measure` gives `it.x` and `it.y`, the
   element's position in its parent, as well as `it.w` and `it.h`.

None of them costs anything when unused.

## Motivation

Byard Material's wave 2 hit the same walls in four components:

- **Tabs, NavigationBar and NavigationRail** take their items as a slot. An item
  can't see the container that places it, so `variant`, `scrollable`,
  `horizontal` and `expanded` are written on every item, and a wrong one on one
  item gives a mixed bar. Compose has `CompositionLocal`, SwiftUI the
  environment, and Flutter `InheritedWidget`, all for this.
- **The vibrant toolbar** needs its IconButtons in `on_primary_container`.
  IconButton can't take a colour that is "the theme's unless I say", because
  only a `Slot` or an `Fn` may be left out (`= none`), and `Color?` isn't a
  parameter type. A context would solve this one too (the toolbar provides the
  content colour), but optional parameters are wanted on their own:
  `elevation: Shadow? = none`, `max: Float? = none`.
- **A ButtonGroup with no selection** and **Tabs not tied to a NavHost** still
  need a `var` from the caller, because `bind` can't have a default
  (`BadBindingParam`). Compose's controls take state when you hoist it and keep
  it when you don't, and Material's own examples rely on that.
- **A scrollable tab indicator** can't slide to the selected tab, because
  nothing knows where that tab is. `on measure` says how big an element is but
  not where.

## Guide-level explanation

### Handing values down

```byld
context TabsStyle {
    variant: TabVariant = .primary
    scrollable: Bool = false
    current: Int = 0
    place: Fn(Int, Float, Float) = none
}

View Tabs(bind current: Int = 0, variant: TabVariant = .primary,
          scrollable: Bool = false, content) {
    inject Theme as t
    var ix: Float = 0.0
    var iw: Float = 0.0
    provide TabsStyle(variant: variant, scrollable: scrollable, current: current,
                      place: {|i, x, w|
                          ix = i == current ? x : ix
                          iw = i == current ? w : iw
                      })
    ZStack #[alignment: bottom_start] {
        Row #[a11y_role: tab_list] { content }
        Box #[height: 3, width: iw with t.springSpatialFast,
              translate.x: ix with t.springSpatialFast, bg: t.primary]
    }
}

View Tab(label: Str, index: Int) {
    inject Theme as t
    inject TabsStyle as s
    var tx: Float = 0.0
    var tw: Float = 0.0
    on change(s.current) => s.place(index, tx, tw)
    Box #[a11y_role: tab, a11y_selected: index == s.current, p: 16] {
        on measure => { tx = it.x
            tw = it.w
            s.place(index, tx, tw) }
        Text(label) #[typo: s.variant == .primary ? t.titleSmall : t.labelLarge]
    }
}
```

`context` declares a set of values with a default for each. `provide` in a view
gives them for everything the view draws, its slots included, and `inject`
reads the nearest one above. A `Tab` drawn outside any `Tabs` gets the
defaults, so it still works alone. Provided values are live: change `variant`
on the `Tabs`, and every `Tab` follows. Each tab reports where it is through the
`place` callback (on a move, and again when the selection changes, since a tab
that does not move is not measured again), and the indicator slides to the one that is selected; the
`bind current: Int = 0` lets a caller own the selection or leave it to `Tabs`
(below).

### Leaving a value out

```byld
View IconButton(icon: Str, color: Color? = none) {
    inject Theme as t
    Icon(icon, color: color ?? t.onSurfaceVariant)
}
```

`Color?` is a colour or nothing (a colour is an `Int`; `Color` names it), and `= none` lets the call leave it out. You
read it with `??`, or inside `when color != none { … }`, where it is a colour.
A bare `bg: color` is an error that says to write one of the two.

A `bind` parameter with a default is a binding the caller may skip:

```byld
View ButtonGroup(bind selected: Int = 0, content) { … }

ButtonGroup { … }                      // the group keeps its own selection
ButtonGroup(bind: page) { … }          // the caller's `page` follows it
```

### Where an element is

`on measure => { x = it.x; w = it.w }`: `it.x` and `it.y` are the element's
offset in its parent, in points, as laid out (before `translate`). A sliding
indicator sits in that parent and moves to them with `with`.

## Reference-level explanation

### 1. Context

- **Declaration.** `context Name { field: Type = default … }` at the top level of
  a file, public or `private` like a `type`. Every field has a default; for `Fn`
  it may be `none`. A `Slot` is not a field type (`BadContextField`): a block of
  elements goes to a parameter. Field types are the parameter types, `T?` among
  them. A context named like a capability, a controller or `Theme` is
  `DuplicateName`.
- **`provide Name(field: expr, …)`** is a body member of a view, written once per
  context per view (twice is `DuplicateProvide`). Fields not written keep their
  defaults; an unknown one is `UnknownField` with the closest name; a value of the
  wrong type is a type error. The expressions are in the view's scope and
  reactive, like an argument.
- **`inject Name as h`** resolves to the nearest view above in the *drawn* tree
  that provides `Name`, or to the defaults when there is none. A slot's elements
  are lowered in the caller's scope (RFC-0062 Q2), and they find context from the
  place they are drawn, inside the callee. That is the reason for the feature: a
  `Tab` written in the caller and drawn inside `Tabs` sees `Tabs`.
- **At run time** an instance keeps the contexts it provides, as signals, in a
  small map. Instantiation looks up the chain of instances that place it once,
  and binds the injected handle to those signals. Reads afterwards are signal
  reads. A view with no `provide` and no context `inject` has no map and no
  lookup.
- **Reload.** A changed `context` declaration rebuilds the views that inject it,
  as a changed parameter list does (dev-loop).

### 2. Optional values

- **`T?` as a parameter type**, with `= none` as its only default. A `T?`
  parameter without a default is required, and may be passed `none`.
- **Reading.** `x ?? d` is `x`, or `d` when it is none (RFC-0027's operator,
  unchanged). Inside `when x != none { … }` and in the true arm of
  `x != none ? a : b`, `x` has type `T`. Any other read of a `T?` where a `T` is
  wanted is `NoneNotHandled`, which points at the read and shows both ways to
  write it. `x == none` and `x != none` work as for slots (RFC-0062 §3), and
  `NoneNeedsOptional` now names `T?` too.
- **`bind name: T = default`.** `BadBindingParam` no longer applies when there
  is a default. A call with `bind:` binds the caller's `var` as now. A call
  without it makes the instance its own `var` with the default, as if the view had
  written `var name = default`, and the view reads and writes it the same way.
  The `var` belongs to the instance and is kept across reloads like any other.
  A `bind` without a default is required as before.

### 3. Position in `on measure`

`it.x` and `it.y` join `it.w` and `it.h`: the offset of the element's box from
its parent's box, from layout, before `translate` and `scale`. The record type of
`it` gains the two fields; nothing else changes. They are already computed by
layout and cost nothing to expose.

### 4. Diagnostics

| Error | When |
|---|---|
| `NoneNotHandled` | a `T?` read where a `T` is wanted, outside `??` or a `!= none` branch |
| `DuplicateProvide` | a view provides one context twice |
| `UnknownContext` | `provide` or `inject` of a name that is no context, capability, controller or `Theme` |
| `UnknownField` | a `provide` field the context doesn't declare (closest name suggested) |
| `ContextFieldNeedsDefault` | a context field without a default |
| `BadContextField` | a context field of type `Slot` |
| `MisplacedProvide` | a `provide` anywhere but the top of a view's body |
| `BadOptionalDefault` | a `T?` with a default other than `none`, or `= none` on a type that is not optional, `Fn` or `Slot` |

## Drawbacks

- **Context is action at a distance.** A `Tab` looks different inside a `Tabs`.
  That is the point, and `inject` at the top of the view says it can happen.
- **Two ways to pass a value** to a component's items: a parameter on each, or a
  context from the container. The guide says when to use which: context for what
  a container decides for all its items.
- **An optional `bind`** makes a component's state either the caller's or its
  own, which is one more thing to know when reading a call.

## Rationale and alternatives

- **Lexical context**, slots seeing the callee's names. Rejected in RFC-0062 Q2
  for good reasons: a slot would read names its writer can't see.
- **Pass the container's parameters into the slot** (a slot with parameters,
  `Slot(TabsStyle)`). That works for one level and needs every item to forward
  them. Context reaches any depth.
- **A default value for every type instead of `T?`**, like `color: Color =
  inherit`. A magic value per type, where `none` and `??` already exist.
- **Sibling geometry through `as name`** (`tab.x`). A `for` row declares its own
  name, so the container can't name "the selected tab". `on measure` plus a
  callback reaches any item.

## Prior art

Compose `CompositionLocal` and `staticCompositionLocalOf`; SwiftUI
`@Environment` and `EnvironmentKey`; Flutter `InheritedWidget` and `Theme.of`;
React context. Optional parameters with defaults and nullable types in Kotlin and
Swift. Compose's "state hoisting" with an internal fallback.

## Resolved questions

### Q1, does a context reach through a view that doesn't provide it?

**Resolution: yes.** `inject` finds the nearest provider at any depth. A view in
between doesn't need to know.

### Q2, can a context hold a callback?

**Resolution: yes,** an `Fn` field. It is how an item reports to its container
(the tab's position above).

## Future possibilities

- A `Theme` subset provided by a component (`provide Theme(...)`) for a surface
  that changes the content colour of everything on it, as Material's surfaces do.
- `it.x`/`it.y` relative to the window, if an overlay needs it.
