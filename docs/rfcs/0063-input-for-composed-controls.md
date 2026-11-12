# RFC-0063: Input for composed controls

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0052 (composed controls, roles, groups and the keyboard),
  RFC-0003 (events, focus, Tab order), RFC-0012 (`focus =>`, handler payloads),
  RFC-0024 (the `dragging` style state), RFC-0042 (the accessibility tree and
  its dump), RFC-0059 (interaction styles), RFC-0050 (the Material package,
  whose wave-1 components are the reason).

> **Placement:** `docs/rfcs/0063-input-for-composed-controls.md`

---

## Summary

A control a package builds out of a `Box` takes the same input a built-in one
does. Four changes:

1. `key_down` and `key_up` are accepted on **any element the keyboard can
   reach**: one with a `focus` or `blur` handler, a `focused:` binding, or an
   `a11y_role` that implies focus together with a `tap`. On an element it
   cannot reach they remain a check error, and the error now says how to make
   the element reachable.
2. **Pointer handlers get an event value.** `pointer_down(e)`, `pointer_up(e)`,
   `pointer_move(e)`, `pointer_enter(e)`, `pointer_exit(e)` and `hover(e)` see
   `e.x`, `e.y` (relative to the element), `e.width` and `e.height`. Three new
   events, `drag_start(e)`, `drag(e)` and `drag_end(e)`, add `e.dx` and `e.dy`
   (the travel since the press), and set the existing `dragging` state. Any
   other field of `e` is a check error, and so is naming a value on an event
   that carries none (`tap(e)`).
3. **Tab enters a `tab_list` or `radio_group` once, at its selected member**,
   and leaves it at the next Tab. The arrow keys move inside it, as before.
4. **`a11y_description`**, the supporting or error text of a control, read by a
   screen reader after the name and printed by `byard shot --a11y`.

A user `pointer_down`, `key_down` or `change` written on a built-in `Slider`,
which never ran, now runs, before the slider's own handling.

None of it costs anything to an element that uses none of it: the event value
is built only for a handler that names it, and a drag event is routed only
where one is registered.

## Motivation

Byard Material's wave 1 (RFC-0050) found these while building real components:

- **`InputChip`** removes itself on Backspace and Delete. It is a `Row` with
  `a11y_role: button` and a `tap`, which RFC-0052 made reachable by Tab, and
  `key_down` on it was `UnknownAttribute`: the guide said a `Box` takes no
  `key_down` "because it is not something the keyboard can reach", which stopped
  being true in RFC-0052. A composed `Slider` needs Home and End for the same
  reason.
- **The `Slider`** is drawn from a `Box` track, a `Box` thumb and an invisible
  built-in `Slider` laid over them to get the pointer, because a `Box` cannot
  say where the pointer is. Today `pointer_down(e) => x = e.x` *compiles*: `e`
  is bound to nothing, `e.x` is empty at run time and `x` is never written.
  That is the silent no-op Byard's rules forbid, and it is why the invisible
  control was needed.
- **A radio group** of five chips has five Tab stops; the checked one is not
  where Tab arrives. A person who tabs into "Size" lands on "S" while "L" is
  chosen, and has to find the checked one by arrowing, which also changes the
  choice (a `radio_group` selects what it reaches).
- **A text field's error text** ("Enter a valid email") is on screen and nowhere
  in the tree. The label names the field; nothing carries the rest
  (material-gaps 10).

It was also found that a `pointer_down` or `key_down` on a built-in `Slider`
never runs: the slider registers its own handlers and not the element's. That
is the same silence one level down.

## Guide-level explanation

### 1. Keys on a composed control

```byld
Row #[a11y_role: button, a11y_label: "Remove ada", tap => select(),
      key_down(k) => { if k == "Backspace" { remove() } }] {
    Text("ada")
}
```

`Row` can be reached by Tab (a role that implies focus, and a `tap`), so it
takes `key_down` and `key_up`. The key's name is the value, as for a `Button`.
Space and Enter press it as its role says (RFC-0052 §3) and never reach
`key_down`; every other key does.

An element Tab cannot reach says why:

```text
error[KeyNeedsFocus]: `key_down` needs an element the keyboard can reach, and a `Box` is not one
   |
 3 |     Box #[key_down(k) => remove()]
   |           ^^^^^^^^
help: give it a `focus => …` handler, or an `a11y_role` that takes focus
      (button, toggle_button, switch, checkbox, radio, tab, menu_item, link)
      together with a `tap`
```

### 2. Where the pointer is, and dragging

```byld
View Main() {
    var value = 0.3

    Box #[
        a11y_role: slider,
        width: 240, height: 40,
        pointer_down(e) => value = (e.x / e.width).max(0).min(1),
        drag(e) => value = (e.x / e.width).max(0).min(1),
    ] {
        Box #[width: 240 * value, height: 4, bg: 0x6750A4]
    }
}
```

`e.x` and `e.y` are the pointer in the element's own space, from its top-left
corner, in logical pixels. While a drag is held the pointer can be outside the
element, and then `e.x` is negative or larger than `e.width`: clamp it. The
element the press began on keeps the drag, wherever the pointer goes.

| Event | Fires | `e` has |
|---|---|---|
| `pointer_down`, `pointer_up`, `pointer_move`, `pointer_enter`, `pointer_exit`, `hover` | as before | `x`, `y`, `width`, `height` |
| `drag_start` | once, when a held press has moved far enough to be a drag (more than 8 px) | those, and `dx`, `dy` |
| `drag` | on every move while the press is held, after it started | those, and `dx`, `dy` |
| `drag_end` | when the press that was a drag is released | those, and `dx`, `dy` |

`dx` and `dy` are how far the pointer has travelled from the press point. The
element reports `on dragging` from `drag_start` to the release, as before.
A press that never moves past the slop is a tap and fires none of the three.

Asking for what is not there is a check error:

```text
error[UnknownField]: a `PointerEvent` has no field `z` (did you mean `y`?)
 3 |     pointer_down(e) => depth = e.z
```

```text
error[EventHasNoValue]: `tap` carries no value; `pointer_down(e)` has the position
 3 |     Box #[tap(e) => x = e.x]
```

### 3. Tab and a group

```byld
Row #[a11y_role: radio_group, a11y_label: "Size"] {
    Box #[a11y_role: radio, a11y_checked: size == "s", tap => size = "s"] { Text("S") }
    Box #[a11y_role: radio, a11y_checked: size == "m", tap => size = "m"] { Text("M") }
    Box #[a11y_role: radio, a11y_checked: size == "l", tap => size = "l"] { Text("L") }
}
```

With `size == "m"`, Tab from the control before the group lands on M, not S.
The next Tab leaves the group; Shift+Tab from after it comes back to M. The
arrow keys move within it as they did. With nothing checked, Tab enters at the
first member.

### 4. A description

```byld
TextField #[bind: email, a11y_label: "Email",
            a11y_description: invalid ? "Enter a valid address" : "We never share it"]
```

```text
TextInput "Email" description "Enter a valid address" = "ada@" [focusable]
```

## Reference-level explanation

### 1. Keys

`key_down` and `key_up` are allowed on an element by the **checker**, from the
attributes it writes, by this rule (`focus_reachable`):

- it has a `focus` or a `blur` handler, or a `focused:` binding; or
- it has a `tap` (or `click`) and an `a11y_role` whose value is one of the
  roles RFC-0052 §2 marks "focus implied". A role chosen with `?:` counts if
  any branch is one; a role that is an expression the checker cannot read
  counts (the run-time rule of RFC-0052 decides, and a role that turns out not
  to take focus leaves a handler that never runs, as a `focus =>` on a disabled
  element does).

A built-in control that is focusable by default (`Button`, `TextField`,
`Toggle`, `Slider`, `Checkbox`, `RadioButton`) keeps taking them. Anything else
is the new `CompileError::KeyNeedsFocus { span, element, name }`, which names
the element and the attribute and gives the two ways out. `UnknownAttribute`
is no longer the answer, because the attribute is not unknown.

At run time nothing changes: the focused element's `KeyDown` handler is the
one RFC-0052 §3 already falls through to when a role does not take the key.
`Tab` and the arrow keys of a group are still the router's first.

### 2. The event value

**Shape.** A handler that names a value (`pointer_down(e) => …`) is given a
`Value::Record`. Its fields are `Float`s, in logical pixels:

| Record | Fields |
|---|---|
| `PointerEvent` | `x`, `y`, `width`, `height` |
| `DragEvent` | `x`, `y`, `width`, `height`, `dx`, `dy` |

`x` and `y` are the pointer relative to the **element's** rectangle (not its
44 px hit area), after the ancestors' scroll and transform are undone: the same
mapping a text field uses to place its caret (`local_point`, RFC-0041). `width`
and `height` are the element's laid-out size.

**How it is built.** Only when the handler names a value. `register_event_attrs`
wraps the lowered action in a closure that reads the pointer from the event
being dispatched (`CURRENT_EVENT_POS`, which the router sets for every event) and
the element's bounds, captured at registration, and builds the record. A
handler with no name is registered as before, unwrapped. `dx` and `dy` read
a second thread-local the router sets, `CURRENT_PRESS_POS`, the point the press
began at. Neither adds work to an event nobody listens to.

**The router.** `EventKind` gains `DragStart`, `Drag` and `DragEnd`
(internal; no platform event is one). When a `PointerMove` arrives with a press
held:

1. The existing latch is unchanged: once the pointer is more than `TAP_SLOP`
   from the press point, `dragging` is set to the press target.
2. When that latch **rises**, `DragStart` is fired on the press target.
3. Then, if the press is latched, `Drag` is fired on the press target. (The
   first move that crosses the slop fires `drag_start` then `drag`.)

On `PointerUp`, if the gesture was latched, `pointer_up` fires as before, then
`DragEnd` on the press target, and then `dragging` clears. The press target is
the element the press began on: `drag`, `drag_start` and `drag_end` are
delivered to it wherever the pointer is, which is what a slider needs. A
disabled target receives none (RFC-0012 S5, RFC-0059 §8). A press that never
passes the slop fires none of the three, so a tap is untouched.

`DragStart`, `Drag` and `DragEnd` count as press kinds (`is_press`): an element
that listens to a drag owns the press, and the element under it does not
receive `pointer_down` or `tap` for the same press. They count for
`claims_pointer`, so a `ScrollView` defers its own drag-to-scroll to an
element that listens, as it does for a built-in `Slider`.

`pointer_down` itself is unchanged: it fires on the press, before any drag, with
the pointer's position at that moment. A control that wants a value from the
press as well as from the drag (a slider jumps to the click) writes both.

**Vocabulary.** `drag_start`, `drag` and `drag_end` are accepted on every
element that takes the pointer events (every element except `Spacer`), like
`pointer_down`.

**The check.** The payload name of a handler is bound, in the checker, to the
named type `PointerEvent` (for `pointer_down`, `pointer_up`, `pointer_move`,
`pointer_enter`, `pointer_exit`, `hover`) or `DragEvent` (for the three drag
events), and a field read of it is checked by the same path as a record's
(`record_field`): a field outside the table above is `UnknownField`, with the
closest field as a hint. A program that declares its own record of either name
keeps its own: the built-in table is used only when the name is not declared.

An event that carries no value takes none. `tap`, `click`, `double_tap`,
`long_press`, `secondary`, `focus` and `blur` with a named value
(`tap(e) => …`) are the new `CompileError::EventHasNoValue { span, event, hint }`.
`tap` can come from a key, so it has no position; the hint says to use
`pointer_down(e)`. The events that do carry one (`input(text)`, `change(v)`,
`key_down(k)`, `wheel`, `page_change(i)`, …) are not touched.

### 3. The built-in `Slider`

`render_slider` registered its own `PointerDown`, `PointerDrag`, `Change` and
`KeyDown` handlers and never registered the ones the element wrote, so a user
`pointer_down` or `key_down` on a `Slider` never ran, and neither did `tap`,
`hover`, `pointer_up` or any other. Now:

- every event the element writes is registered, as for any element;
- for `pointer_down`, `key_down` and `change`, which the slider also handles,
  the user's actions run **first**, then the slider's own, from the same
  dispatch. The router fires one handler per kind, so they are composed into
  one: the user's can observe the value before it changes, and cannot stop the
  slider's. (A handler that wants the slider not to move writes `disabled:`.)
- `pointer_down(e)` on a `Slider` sees the same `PointerEvent` any element does.

`Toggle`, `Checkbox` and `RadioButton` had the same gap and get the same fix:
every event written on them is registered, and for `tap`, `key_down` and
`change` (which they handle) the user's actions run first. A `TextField`'s
`pointer_down(e)` and `pointer_up(e)` are given the pointer value as well.
`e.x` and `e.y` are relative to the element as drawn, scroll offset applied.

### 4. Tab enters a group once, at its selected member

A **group** is an element with `a11y_role: tab_list` or `radio_group`, as in
RFC-0052 §3; its members are the focus targets with the role `tab` or `radio`
inside it (not through a nested group).

For Tab and Shift+Tab, a group is **one stop**. The member that is the stop is:

1. the member with focus, if focus is in the group (so that Tab leaves from where
   the arrow keys left it); otherwise
2. the first member that is **selected**: `a11y_selected` or `a11y_checked`
   evaluates `true`, or the universal `selected:` prop does; otherwise
3. the first enabled member.

This is the WAI-ARIA roving pattern, decided over the alternative of entering at
the selected member and still stopping at every one. The others are not Tab stops; they stay focus targets (a press focuses them, the
arrow keys and an assistive technology's `Focus` reach them). Disabled members
are never the stop. The router is told each member's selected state when it
registers the member (`group_member(elem, selected)`), which evaluates the
three props only for a member of a group. An element in no group pays nothing.

The built-in `RadioButton` is not in a group of this kind (RFC-0018 groups it
by its bound `var`) and is unchanged.

### 5. `a11y_description`

A universal `Str` attribute (`add_universal`, paint class: moves nothing),
read only while a tree is being built. It sets the node's `description`
(`AccessNode::description`, which AccessKit's `description` already carries on
the bridge, RFC-0042), after the name. An empty string says nothing. On a
`TextField` it replaces the placeholder-as-description of RFC-0042 §3.

The dump (`byard shot --a11y`) already prints a description after the name:
` description "…"`; it now does so for this too.

The attribute is read on an element that is a node: a built-in control, a
`Text`, an `Image`, or an element with an `a11y_role`. A layout container
(`Box`, `Column`, `Row`, `Grid`, `ZStack`, `Clip`) with **no role, no `tap` and
no `=>`** is not in the tree, so a label or description written on it would be
read by nothing. That is the new check error `A11yNeedsRole`, for `a11y_description`
and for `a11y_label`, which says to give it an `a11y_role`. An element with a
`..style` spread is not checked (the style may carry the role). The one program
in the repository this found, a `Box` in `examples/view_animations` with a label
and no role, now says `a11y_role: image`.

## Cost

- Checker: one scan of an element's attributes when it writes `key_down` or
  `key_up` on an element that is not focusable by default. Nothing otherwise.
- Event value: a closure and a `Record` of four to six `Float`s per event, only
  for a handler that names the value. An element with none registers as before.
- Router: drag latching adds a branch to `PointerMove` with a press held (it
  already computed the distance), and two optional lookups of a handler by kind
  on the press target. Nothing for a pointer that is not pressed.
- Tab: `tab_focus` gains a filter over the groups' members when `groups` is not
  empty; an app with no group does not enter it.
- `a11y_description` is read while a tree is built, which is only when an
  assistive technology listens.

## Drawbacks

- A third kind of event with a position (`drag*`) beside `pointer_move` and
  `PointerDrag` (the internal one the built-in `Slider` uses). The built-in could
  move to `Drag` and `PointerDrag` could go; left alone to keep this change out of
  the slider's own behaviour.
- `tap(e)` becoming an error is a break for a program that wrote it and got
  nothing from `e`. That program was already wrong, but compiled.
- Roving focus changes where Tab stops for an app with a `tab_list` or
  `radio_group`: the unselected members are no longer stops. The arrow keys
  were the way between members already, and the order is the one the APG
  prescribes.
- `x` and `y` are relative to the element's rectangle, not its inflated hit
  area. A tiny control the 44 px area makes tappable can report a negative `e.x`.

## Rationale and alternatives

- **One event value for every handler.** Rejected: `tap` can come from a key and
  has no position; giving it a made-up one is the same silence in a new place.
- **`e.x` in the page, not the element.** Rejected: a composed control wants the
  fraction of its own width; the page position means subtracting the element's,
  which the control does not know.
- **A `PointerDrag` handler in the language.** It exists for the built-in
  `Slider` and fires where the pointer is. The new events go to the pressed
  element, which is what every drag handle wants, and have their own names.
- **`drag_start` at the press.** Then every press would be a drag start, and
  `dragging` (RFC-0024) would not agree with it. The slop is what tells a tap
  from a drag, so the events follow it.
- **Letting `key_down` make an element focusable.** It would reorder Tab for
  any element that wrote one. A control opts in with a role or `focus =>`; the
  error says so.
- **The old rule, with a better hint.** `UnknownAttribute` with a hint says
  the attribute is unknown, which it is not.
- **Keeping every group member a stop, entering at the selected one.** Tab would
  walk the unselected members after the selected one, and the ones before it
  would be reachable only backwards. A group that is one stop is the pattern.

## Prior art

WAI-ARIA Authoring Practices: roving tabindex for radio groups and tab lists;
DOM `PointerEvent` (`offsetX`, `offsetY`) and the Pointer Events capture rule;
Flutter's `GestureDetector` (`onPanStart`, `onPanUpdate`, `onPanEnd`, with
`localPosition`); SwiftUI's `DragGesture` (`translation`); `aria-description`.

## Resolved questions

### Q1, do `drag` handlers see the pointer outside the element

**Resolution: yes.** The press target keeps the drag. A slider must follow the
pointer past the end of its track. §2.

### Q2, does the user's handler on a built-in slider run first or last

**Resolution: first.** It sees the value as it was. §3.

### Q3, is the check for `key_down` on the element's attributes or on its run-time focusability

**Resolution: the attributes.** A check cannot see run-time state, and the
conservative reading (an expression role counts) never rejects a valid program.
§1.

## Unresolved

- **The built-in `Slider` on `Drag`** instead of the internal `PointerDrag`.
- **Pointer capture for `pointer_up`.** It goes to the element under the pointer,
  not the one pressed; `drag_end` is the one that is guaranteed.
