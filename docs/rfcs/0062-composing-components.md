# RFC-0062: Composing components

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0007 (user views and the `content` slot), RFC-0016 and
  RFC-0024 (state blocks and combined states), RFC-0018 (reactive `when` and
  `for`), RFC-0019 (callback parameters), RFC-0036 (`as name`), RFC-0052
  (composed controls, `focus_visible`), RFC-0059 (state blocks in an element's
  body), RFC-0050 (Byard Material, whose components are the reason).

> **Placement:** `docs/rfcs/0062-composing-components.md`

---

## Summary

Five things a component author cannot say today, found building Byard Material's
first wave:

1. A slot (`content`) can be placed anywhere in a view's body, in both branches of
   a `when` and inside a `for`. Today it is "unknown view `content`" in all of
   them.
2. A view can take **several slots**: a parameter of type `Slot`, filled at the
   call with a block, placed in the body by name, and testable (`when leading !=
   none`).
3. A callback can be **optional**: `on_tap: Fn() = none`, tested with `when on_tap
   != none`, and calling it when it was not given does nothing.
4. A **layout attribute on a view call**, `m.Icon("x") #[ml: 8, grow: 1]`, applies
   to the view's root element; any other attribute that is not a parameter says
   which are allowed.
5. **States that reach other elements.** An element named `as field` exposes its
   states, and its descendants and later siblings write `on field.focused { … }`,
   `on field.hover + pressed { … }`. `focus_within` is a new state of any
   element with focus inside it.

Everything is decided when a view is lowered, once. A view that uses none of it
takes the paths it took before.

## Motivation

Byard Material's wave 1 (`notes-material-feat-*`, `wave1-engine-gaps.md` items
5, 9, 12, 13, 14) wrote around each of these:

- **Card and ListItem need two roots.** A clickable `Card` is a `Box` with `tap`;
  a plain one must not be focusable or announced as a button. The body is
  `when on_tap != none { Box #[tap => on_tap()] { content } } else { Box { content
  } }`, and the first `content` is a compile error. The workaround, a transparent
  `ZStack` layer holding the tap, makes a trailing `Switch` in a clickable
  `ListItem` hard to reach.
- **A list item has a leading and a trailing slot.** `View ListItem(headline: Str,
  leading, trailing)` has one block, so the icon and the switch are passed as
  `Str` and `Bool` parameters and the item decides how to draw them. A `ListItem`
  that shows an avatar, an icon or a `Checkbox` is three views.
- **A callback cannot be tested.** `on_tap: Fn() = {}` is always present, so the
  component cannot ask whether it was given one, and every `Card` is a button.
- **`m.Icon("x") #[ml: 8]` is `UnknownParam`.** A component that places another
  needs a margin, a `grow` or an `align_self` on it and today wraps it in a
  `Box` that exists for nothing else.
- **A `TextField` keeps `var focused` and `var hovered`**, set from four handlers,
  because its floating label and its indicator are siblings of the field and a
  state block only speaks for the element it is written in. A `Switch`'s handle
  does the same for its track's press. RFC-0059 listed this as a future
  possibility; this is it.

## Guide-level explanation

### Slots anywhere, and several of them

```byld
View Card(on: Bool, content) {
    when on {
        Row #[tap => {}] { content }
    } else {
        Row { content }
    }
}

View ListItem(headline: Str, leading: Slot = none, trailing: Slot = none) {
    Row #[gap: 16, align: center, p: (12, 16)] {
        when leading != none { leading }
        Text(headline) #[grow: 1]
        when trailing != none { trailing }
    }
}

View App() {
    var on = true
    m.ListItem("Wi-Fi", leading: { Icon("wifi") }, trailing: { m.Switch(bind: on) })
    m.ListItem("About")
}
```

A parameter of type `Slot` holds a block of elements. It is filled at the call
with `name: { … }`, in the parentheses or in `#[ ]`, and placed in the body by
writing its name where the elements should go, as `content` is. The block is
written in the caller and sees the caller's names, whatever branch of whatever
`when` places it. The trailing block of a call is still `content`.

A `Slot` with `= none` may be left out; without a default the call must give it
(`MissingParam`). `content` is always optional. `leading != none` and `leading ==
none` ask whether the call gave the slot, and are decided when the view is
instantiated. A slot can be passed on, `Row(leading: leading)`, and placed more
than once.

### Optional callbacks

```byld
View Card(on_tap: Fn() = none, content) {
    when on_tap != none {
        Box #[tap => on_tap(), a11y_role: button] { content }
    } else {
        Box { content }
    }
}
```

`= none` is a default for a `Fn(…)` or `Slot` parameter. `on_tap != none` is true
when the call passed one. Calling a callback that was not given is a no-op, so
`tap => on_tap()` needs no guard. Comparing anything else to `none` is a check
error: `none` is for a parameter that declared it, and says so.

### Layout on a view call

```byld
m.Icon("wifi") #[ml: 8, grow: 1, align_self: center]
```

The attributes a parent reads from its children (`width`, `height`, their
`min_` and `max_` bounds (RFC-0064), `m`, `mx`, `my`, `mt`, `mr`, `mb`, `ml`, `grow`,
`shrink`, `basis`, `align_self`) apply to the
view's root element, as if written on it. A view whose body has more than one
root, or a `when` at its root, applies them to each root that is drawn. A
parameter of the same name wins: a view with `width: Int` gets `width` as that.
Any other name is `UnknownParam`, and its message lists these.

### States that reach other elements

```byld
View Field(bind value: Str, label: Str) {
    ZStack {
        TextField #[bind: value] as field
        Text(label) #[color: 0x79747E, translate.y: 0] {
            on field.focused { color: 0x6750A4, translate.y: -12 with anim.spring() }
        }
        Box #[height: 1, bg: 0x79747E] {
            on field.focused + hover { bg: 0x6750A4 }
        }
    }
}
```

An element named with `as name` publishes its interaction states. A state block
written on a later element of the same view, or on a descendant, can name them:
`on name.hover`, `on name.pressed`, `on name.focused`, `on name.focus_visible`, and
every other state, joined with `+` to each other and to the element's own
(`on field.focused + hover`). A name belongs to the whole view, so a label may be
written before its field. A name no element of the view is tagged with is
`UnknownStateTarget`, with the closest one.

A block on another element's states only changes what is painted, and layout
was computed before. A layout attribute in one (`width`, `height`, `p`, `m…`,
`grow`, `align`, …) is `LayoutInStateBlock`, which says to use `translate` or
`scale`; the same holds for `focus_within`.

`focus_within` is a state of every element: it holds while the element or any
element inside it has the keyboard focus.

```byld
Row #[bg: 0xEADDFF] {
    on focus_within { outline: 0x6750A4, outline_width: 2 }
    TextField #[bind: query]
}
```

## Reference-level explanation

### 1. A slot in a deferred branch

`when` branches and `for` bodies are lowered lazily, the first time they are
drawn, from the AST and a snapshot of the environment taken where they were
written. The slot frame of the instance they sit in was not part of that
snapshot, and was gone when they were lowered, so `content` there resolved to
nothing and was reported as an unknown view. The frame is now captured when the
pool is created (one reference-counted pointer, only when the enclosing view
declares a slot) and put back while the branch is lowered. A pool created inside
a branch captures the frame it finds, so nesting works to any depth. Navigation
routes capture it the same way.

A slot is kept as the block the caller wrote, with the names visible at the call
(captured as a callback's are) and the view instance that wrote it. It is
lowered where the callee places it, once for each placement, as that instance
and among those names. A slot placed in every row of a `for`, or twice in a
body, is therefore a set of elements of its own each time: its own identity
(so its own focus and editing state), and its own pools if it holds a `when` or
a `for`. A call still has what it wrote in its slots checked when the callee's
body is not lowered again for it.

### 2. Named slots

- A parameter is a slot when its name is `content` or its type is `Slot`. Slot
  parameters take no positional argument, no value and no `bind`.
- **Parsing.** Whether a `{ … }` argument holds actions (an `Fn`'s callback) or
  elements (a `Slot`'s block) is decided by the callee's parameter, never by what
  the block starts with: controllers are capitalised too (`{ Api.save() }`). The
  parser cannot see a callee's signature (a package's views included), so a `{`
  that starts a call's argument or attribute value is read both ways, as actions
  and as the members of an element body. If only one reading is valid, that is
  the node (`Expr::Lambda`, or `Expr::SlotBlock` for elements). If both are, it
  is the callback with the elements riding along (`Expr::Lambda { slot }`), and
  the call settles it once the callee is known: elements for a `Slot`, actions for
  an `Fn`, elements (so that the checker can say a `Slot` was wanted) for any
  other parameter. A callback passed to an intrinsic is never touched. If neither
  reading is valid, `BlockFitsNeither` gives what each expected. Only the chosen
  reading is resolved and reported on: the resolver knows every view's
  parameters, so it settles a call's blocks as soon as the callee is named and
  rewrites the package names of the elements or of the actions, never both (an
  injected `http.get(…)` is not a package, and `m.Icon("x")` is not a function). `{}` is the empty
  callback and is accepted as an empty slot.
- **At the call.** For each slot parameter the call's argument of that name is
  lowered in the caller's scope to a list of nodes, a `Slot` argument that is a
  slot of the caller's own (`leading: leading`) is forwarded, and the trailing
  block fills `content`. The instance's slot frame lists, for each slot, whether
  it was given and its nodes.
- **In the body.** A name that is a slot of the view being lowered, written as an
  element, splices the nodes of the current frame. A slot not given splices
  nothing. In an expression, a slot parameter reads as a value that `== none` /
  `!= none` can test; it is decided per instance, so the untaken branch of `when
  leading != none` is not drawn, and checking still lowers both.
- **Checks.** A value given to a `Slot` parameter (an action block is a value
  here), or a block of elements to anything else, is `SlotArgument`; a block outside a call's arguments is the same error. A name
  that is a slot of the view and is written with arguments or attributes is
  `UnknownView`, as `content` with them is today. Checking a view with a
  slot lowers it with every slot absent.
- A call is a repeat of one already checked only if the same slots are given, so
  `already_expanded` counts the presence of the trailing block and of each slot
  block in a call's shape.

### 3. Optional callbacks and `none`

`none` is a default for a `Fn(…)` parameter or a `Slot` one, and a side of `==`
and `!=` against a name that declared it. A callback not given is bound to nothing
(the unit value), and a call of a name bound to nothing already does nothing, so
`on_tap()` is a no-op with no new branch. A callback forwarded from a view where it
was not given (`Card(on_tap: on_tap)`) is not given there either.

`x == none` and `x != none` are constants per instance. The checker reports
`NoneNeedsOptional` for a comparison of `none` with anything that is not a
parameter declared `= none` or `content`: a required callback, a `var`, a literal.
A `Fn` or `Slot` default other than `none` (`= {}`) makes the parameter always
present, which is a comparison the checker also reports, since its answer is known.

### 4. Layout attributes on a view call

When a name in a user-view call is not a parameter of the callee and is one of
`width height min_width max_width min_height max_height m mx my mt mr mb ml grow
shrink basis align_self`, its value is
projected in the caller's scope (as every argument is) and bound in the instance
under a name no program can write. The callee's body is then lowered with that
attribute appended to each root element: an intrinsic, or a user view called at
the root (which receives it as its own layout attribute). A root `when` or `for`
passes it to the roots of its branches and bodies. A root that is a slot placement
or a state block ignores it.

The appended attribute wins over one the root wrote, since the caller knows its
neighbours and the callee does not. The cost is a copy of the callee's root
members when, and only when, such an attribute is written; a call without one
lowers the shared body as before. The attribute is an ordinary one afterwards, so
`with` and a conditional work on the call: `#[grow: wide ? 1 : 0]`.

`min_width`, `max_width` and their kin join the list when the layout RFC that adds
them lands; the list is one constant.

### 5. States that reach other elements

- **`as name`** already tags an element. Before a scope is lowered (a view's
  body, a `when` branch, a `for` body, a route, a slot's block) the elements in it
  tagged `as` are found, without entering a nested `when`, `for` or route, and
  each name is bound in the environment to the identity (`uid`) its element will
  have. A reference may therefore come before its element. Because the
  environment is what lazily lowered branches capture, a name declared outside a
  `when` or `for` is visible inside it, and one declared inside is not visible
  outside; a `for` row declares its own. Two elements of one scope with the same
  name share it, which is not supported. The scan walks the scope's elements once
  and finds nothing when none is named.
- **`on name.state`** is parsed in a state block's list as `IDENT "." state`. A
  block keeps its own states (`states`) and the remote ones (`remote`: name, state,
  and the id once resolved). Lowering resolves each name through the environment;
  one that is not there is `UnknownStateTarget` with the closest declared name,
  and a note when the name is declared later in the view. An unknown state after the
  dot is `UnknownStyleState` as ever. A block's specificity is the number of states
  it names, own and remote.
- **At paint.** A block with remote states applies when every named element's
  mask contains its state. The table of masks, keyed by identity, is cleared at the
  start of a frame, so an element not drawn is in no state. Only an element some
  block names publishes anything. Right after layout, before anything paints, each
  such element's states are published: the router's pointer and focus states
  (`hover`, `pressed`, `focused`, `focus_visible`, `focus_within`, `dragging`) and
  its value states (`checked`, `selected`, `invalid`, `indeterminate`, `disabled`),
  which layout reads from the element's attributes while they are in scope. So an
  element written before the named one sees this frame's, which is what a
  `TextField`'s label needs. The element replaces the entry with the same mask when
  it paints.
- **A layout attribute in such a block is `LayoutInStateBlock`.** Layout runs once
  with no state; `translate` and `scale` are what a label that floats moves with.
  The check is made where the block is lowered, for every block that names another
  element's state or `focus_within`.
- **`focus_within`** is a bit of `StyleState` beside `FOCUSED`. The set of elements
  that have the focused one inside is the focused element and its ancestors in the
  layout tree, computed the first time a frame asks and only when a block that
  names `focus_within` was lowered.
- **Layout does not see them.** A state block changes what is painted; layout runs
  once with no state, as it does for `hover`. A label that floats moves with
  `translate` and `scale`, not with `top`.
- **A disabled element** has no hover, press or focus (RFC-0059 §8), and so
  publishes none of them.

## Drawbacks

- **A `{ … }` argument is parsed twice.** Once as actions and once as elements, to
  wait for the callee's signature. Blocks are small and the second parse is
  discarded unless it is the only valid one, but a slot nested in a slot parses
  each level twice.
- **A slot is lowered at every placement.** A slot placed in a `for` row is the
  work of lowering it once per row, as the loop body already is, in exchange for
  each row having elements of its own.
- **`none` is not a value.** It is only a default and one side of a comparison,
  decided per instance. A view cannot store it or pass it as an argument other than
  to a parameter that declares it.
- **A named element's value states are read during layout** as well as at paint,
  for the elements a block names, and only for them.
- **A name belongs to the whole view**, so a typo that happens to name another
  element's tag is not caught as unknown.

## Rationale and alternatives

- **A slot is a parameter, not a new kind of child.** `leading: { … }` is the
  argument syntax a callback already has, so a call reads the same for both and the
  formatter, the resolver and the language server already walk arguments.
- **Presence as `!= none`, not a built-in `has(leading)`.** It reads as the
  question it is, works for callbacks with the same word, and costs one comparison
  the checker already knows how to type.
- **Layout attributes forwarded to the root, not a wrapper.** A wrapper `Box`
  changes the tree the component author wrote and the one the accessibility tree
  reports. A forwarded attribute changes only what the root is told by its parent.
- **A general `hovered` value readable in expressions** (the alternative RFC-0059
  raised). It would let any attribute depend on another element's state, at the
  price of a dependency from paint back to lowering and of re-lowering the reader.
  A state block resolves at paint, where states already live.
- **A `Context` of states by tree position** (the parent's state, with no name).
  Shorter for the common case and wrong for the sibling label, which is the case
  this exists for.
- **Making `focus_within` a pseudo-name** (`on self.within`). One more word for
  something that is a state like the others.

## Prior art

- **CSS**: `:focus-within`, and `:has()` and the sibling combinators that let a rule
  reach a sibling's state; here by name instead of by selector.
- **Compose** and **SwiftUI**: slot parameters are `@Composable () -> Unit`
  arguments and `@ViewBuilder` closures; an optional one is nullable and tested
  with `!= null`.
- **Vue / Svelte**: named slots, with `$slots.leading` / `$$slots.leading` to ask
  whether one was given.
- **Flutter**: `Expanded` and `Align` wrap a child to give it layout; the
  attribute-on-call here is that without the wrapper.

## Resolved questions

### Q1, how is a slot told apart from a callback body?

**Options.** (a) by the parameter's type, which the parser does not know; (b) a
distinct opener (`slot { … }`, `<…>`); (c) by the first token, a capital letter
meaning elements.

**Resolution: (a), by reading the block both ways until the callee is known.**
(c) was tried first and is wrong: controllers are capitalised (`{ Api.save() }`,
`{ Store.count += 1 }`), so a real callback was a slot. The type is what the
author wrote to say which they meant, and the call settles it, including for a
package's views.

### Q2, does a slot argument see the callee's names?

**Resolution: no.** It is written in the caller and lowered there, like `content`
(RFC-0007 D-A). The callee places it and cannot read it.

### Q3, where do layout attributes go when the view has several roots?

**Resolution: each root.** A component with two roots is a `when` choosing one of
two, and the attributes are about the one that is drawn. A view that wants them on
something else takes a parameter of that name.

### Q4, can `on name.state` name an element written later?

**Resolution: yes.** A scope finds its `as` names before it is lowered, and the
named element's pointer and focus states are published after layout and before
paint. A block cannot follow the value states of an element written after it.

## Future possibilities

- `min_width`, `max_width`, `min_height`, `max_height` in the layout list when they
  exist.
- A `for` over a slot's children, and slots with parameters (`Slot(Item)`), if a
  component needs to hand a row its index.
- `as name` on a `Text`, so that a label can publish its own states.
