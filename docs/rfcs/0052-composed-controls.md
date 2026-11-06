# RFC-0052: Composed controls

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-06
- **Last updated:** 2026-11-06
- **Depends on:** RFC-0042 (the accessibility tree; §2 roles, §3 names),
  RFC-0003 (events, focus, Tab order), RFC-0012 (`focus =>` and `blur =>`,
  `disabled:`), RFC-0016 and RFC-0024 (style states), RFC-0050 (the Material
  package, whose spike found these gaps).

> **Placement:** `docs/rfcs/0052-composed-controls.md`

---

## Summary

A control a package builds out of a `Box` (a `Switch`, a `Checkbox`, a `Tab`)
becomes as good as a built-in one for the keyboard and for a screen reader.
Three things change:

1. Every element takes `a11y_role`, `a11y_value`, `a11y_checked`,
   `a11y_mixed`, `a11y_selected`, `a11y_expanded` and `a11y_disabled`, and they
   reach the accessibility tree. `a11y_role` is a closed set of 23 keywords; a
   misspelt one is a check error that names the closest.
2. A focusable element with a `tap` handler is pressed by Space and Enter, and
   which of them follows its role. A `tab_list` or `radio_group` moves focus
   with the arrow keys.
3. A new style state, `on focus_visible`, is set only when focus arrived by the
   keyboard. A pointer press focuses without it.

None of it costs anything to an element that uses none of it.

## Motivation

RFC-0050's spike built Material's `Switch` from a `Box`. What a reader got was
`Button "C box" [focusable]`, and the gaps were these (material-gaps 4 and 19):

- `a11y_role`, `a11y_value` and `a11y_checked` were `UnknownAttribute`. A
  control the user can see is a switch has no way to say so.
- `Box #[tap => n++]` is not in the Tab order unless it has a `focus =>`
  handler, and when it is, Space does nothing: only the built-in `Button`
  and `Toggle` were made pressable by the keyboard (#347).
- `on focused` also matches after a mouse press. Material, like every browser
  since `:focus-visible`, draws the ring for keyboard focus only.

The workaround was a transparent real `Button` laid over the drawn control,
with the state written into its name ("Wi-Fi, on"). It costs an extra node per
control, a reader says "button", and it has to be sized to what it covers.
A package should not have to fake a control to get a keyboard.

## Guide-level explanation

```byld
View Main() {
    var wifi = true

    let ring = style {
        on focus_visible { border: 0xD0BCFF, border_width: 3 }
    }

    Box #[
        ..ring,
        a11y_role: switch,
        a11y_checked: wifi,
        a11y_label: "Wi-Fi",
        width: 52, height: 32, radius: 16, bg: (wifi ? 0x6750A4 : 0x79747E),
        tap => wifi = !wifi,
    ] {
        Box #[width: 24, height: 24, radius: 12, bg: 0xFFFFFF, a11y_hidden: true]
    }
}
```

The `tap` and the role are all it takes: a role that implies focus makes the
`Box` a stop in the Tab order. The dump of that, from `byard shot --a11y`,
reads `Switch "Wi-Fi" = on [focusable]`, where it said `Button "Wi-Fi"` with
no state at all. Tab reaches it, Space and Enter
flip it, a reader says "Wi-Fi, switch, on", and a mouse press draws no ring
while Tab does.

A role is a keyword, not a string, so the checker can hold it to the set:

```text
error[AttributeTypeMismatch]: expected one of ["button", "toggle_button", ...]
  (did you mean `checkbox`?)
   |
 3 |     Box #[a11y_role: chekbox]
```

A composed tab strip:

```byld
Row #[a11y_role: tab_list] {
    for t in tabs {
        Box #[a11y_role: tab, a11y_selected: t == current,
              tap => current = t] { Text(t) }
    }
}
```

Tab enters the strip, the arrow keys move between the tabs, Enter or Space
selects the one with focus.

## Reference-level explanation

### 1. The attributes

All are accepted on every element (`add_universal`, paint class: none moves
anything) and all are read only while a tree is being built, so with no
assistive technology listening they cost nothing but the check.

| Attribute | Type | Becomes |
|---|---|---|
| `a11y_role` | keyword, §2 | the node's role, replacing the one the element would have |
| `a11y_value` | `Str` | the node's text value (`AccessValue::Text`), a progress bar's "40 percent" |
| `a11y_checked` | `Bool` | checked / unchecked, for `switch`, `checkbox`, `radio`, `toggle_button` |
| `a11y_mixed` | `Bool` | the mixed state; wins over `a11y_checked` |
| `a11y_selected` | `Bool` | selected (a `tab`, a `list_item`) |
| `a11y_expanded` | `Bool` | expanded when `true`, collapsed when `false`; absent means not expandable |
| `a11y_disabled` | `Bool` | disabled, for a reader only; `disabled:` already does it and also stops input |

`a11y_label` and `a11y_hidden` (RFC-0042) are unchanged.

**`a11y_checked` is a `Bool`, with `a11y_mixed` beside it, not a tri-state.**
A tri-state would have to be `true | false | mixed`, which is neither a `Bool`
nor a keyword that can be reactive: Material would write
`a11y_checked: on ? yes : no`, and a `Bool` expression is what every composed
control already has in hand. The mixed state is rare and `Checkbox` already
spells it as a separate flag (`indeterminate:`), so `a11y_mixed` is the same
shape. Both are accesskit's `Toggled::{True, False, Mixed}` on the bridge.

`a11y_role` is a keyword read from the source, like `snap: page`, so it is
fixed for the element and cannot be a `var` (a role that changes is a different
control, which is a `when`). A string literal of the same word is the same
keyword.

An attribute that does not apply to the role (`a11y_checked` on a `heading`) is
passed through and the bridge ignores it, as a browser does with a misplaced
`aria-checked`. `a11y_checked` on a `switch` sets its on/off value; absent, a
switch is off.

Every attribute is read on any node the tree builds: a `Box`, a `Text` (so
`Text("Title") #[a11y_role: heading]` is a heading), an `Image` and a native
view. A layout container with a role stops being flattened away (RFC-0042 §2):
it becomes a node, and what it holds attaches to it.

### 2. Roles

| Keyword | `AccessRole` | AccessKit `Role` | Focus implied |
|---|---|---|---|
| `button` | `Button` | `Button` | yes |
| `toggle_button` | `ToggleButton` | `Button`, with `toggled` | yes |
| `switch` | `Switch` | `Switch` | yes |
| `checkbox` | `CheckBox` | `CheckBox` | yes |
| `radio` | `RadioButton` | `RadioButton` | yes |
| `radio_group` | `RadioGroup` | `RadioGroup` | arrows (§3) |
| `slider` | `Slider` | `Slider` | no |
| `tab` | `Tab` | `Tab` | yes |
| `tab_list` | `TabList` | `TabList` | arrows (§3) |
| `menu_item` | `MenuItem` | `MenuItem` | yes |
| `menu` | `Menu` | `Menu` | no |
| `link` | `Link` | `Link` | yes |
| `text_field` | `TextInput` | `TextInput` | no |
| `progress_bar` | `ProgressBar` | `ProgressIndicator` | no |
| `list` | `List` | `List` | no |
| `list_item` | `ListItem` | `ListItem` | no |
| `dialog` | `Dialog` | `Dialog` | no |
| `alert` | `Alert` | `Alert` | no |
| `navigation` | `Navigation` | `Navigation` | no |
| `toolbar` | `Toolbar` | `Toolbar` | no |
| `group` | `Group` | `Group` | no |
| `image` | `Image` | `Image` | no |
| `heading` | `Heading` | `Heading` | no |

`radio_group` is beyond the minimum: the arrow keys of §3 need something to
group by, and AccessKit has the role. The set is closed (`AccessRole::KEYWORDS`
is the one list the checker, the parser of the keyword and the tests share).
`toggle_button` has no AccessKit role of its own, so the bridge sends a
`Button` with its `toggled` state, which is what AT-SPI, UIA and NSAccessibility
all call a toggle button. A role that names its own text names the node:
`button`, `toggle_button`, `switch`, `checkbox`, `radio`, `tab`, `menu_item`
and `link` take the `Text` inside them as their name (RFC-0042 §3), and the
"label beside it" rule still names the unnamed ones.

**Focus implied** means an element with that role and a `tap` handler is a
focus target without a `focus =>` handler. Without a `tap` there is nothing to
press and the role alone does not put it in the Tab order, the rule the
built-in `Button` already follows. An element with a `focus =>`, `blur =>` or
`focused:` is a focus target whatever its role, as before. An element with only
a `tap` and no role is still not in the Tab order: that would change the order
of every existing app.

A misspelt role is `AttributeTypeMismatch` with the closest keyword, found
with the same `closest_match` the other enums use. A role written as a string
literal is held to the set too, which also closes a quiet hole in the other
enum props (`fit: "covr"` had been accepted and ignored).

### 3. The keyboard

A focus target that has a `tap` handler is pressed by a key, once however long
the key is held, by firing its `tap` (the path `Button` and `Toggle` already
take). The keys follow the role, after the WAI-ARIA Authoring Practices and the
native controls of the three desktops:

| Role | Space | Enter | Why |
|---|---|---|---|
| none, `button`, `toggle_button`, `menu_item` | yes | yes | a button takes both |
| `switch` | yes | yes | APG: Space, and Enter optional; the built-in `Toggle` takes both |
| `checkbox`, `radio` | yes | no | a native checkbox is Space only; Enter in a form submits it |
| `tab` | yes | yes | manual activation: focus moves, a key selects |
| `link` | no | yes | a link is followed by Enter only; Space scrolls a page |
| any other role with `tap` and a focus handler | yes | yes | |

A key a role does not take is not swallowed: it goes on to the element's own
`key_down` handler, as it does for a control that does not use it.

**Arrow keys in a group.** An element with the role `tab_list` or `radio_group`
groups the elements with the role `tab` or `radio` that are focus targets
inside it (not through a nested group). With focus on a member:

| Key | Does |
|---|---|
| Right, Down | focus the next member, wrapping |
| Left, Up | focus the previous member, wrapping |
| Home, End | the first, the last (`tab_list` only) |

All four arrows work in both groups: the groups have no orientation attribute,
and a horizontal strip taking Down is harmless. A disabled member is skipped. In
a `radio_group` the member that gets focus is also pressed (its `tap` runs), as
the built-in `RadioButton`s select with the arrows; in a `tab_list` it is not
(manual activation: Enter or Space selects), and an app that wants a strip to
follow focus writes `focus =>` on its tabs. The cost is one `Vec` of ids per
group per frame, nothing for an app with none.

### 4. `focus_visible`

`StyleState::FOCUS_VISIBLE` and `StyleStateKind::FocusVisible`, written
`on focus_visible { … }` and combinable (`on focus_visible+hover`). It is set
when the element has focus **and** focus got there by:

- Tab or Shift+Tab, or the arrow keys of §3;
- an assistive technology's `Focus` action, and a focus set at start up;
- a pointer press on a `TextField`, which shows its focus whatever moved it
  there, as `:focus-visible` does in a browser (you are typing, so you need
  the caret and the ring).

A pointer press on anything else focuses it and leaves `focus_visible` off.
`on focused` is unchanged: it still matches in every case, so nothing written
before this stops working. The flag is the router's, one `bool`, cleared when
focus leaves; focusing the element it already has, with a key, turns it on
(Tab on the only control after clicking it).

**The built-in controls get it too, and their ring does not move, because they
have none.** Focus draws nothing by itself (`controls.md`), so there is no
default to migrate. A `Button` or a composed element styles `on focus_visible`
as it would `on focused`. `Toggle`, `Checkbox`, `RadioButton` and `Slider` draw
their own parts and have no box to style. The one example that draws a
ring on a `Button` with `on focused`, `keyboard_focus`, moves to
`focus_visible`, which is the CHANGELOG's "Changed" line: a mouse click on it
no longer leaves a ring. `number_input` and `style_states` style a
`TextField`, which is focus-visible however it was reached, and stay as they are.

### 5. The accessibility dump and the bridge

`AccessRole` grows `ToggleButton`, `RadioGroup`, `MenuItem`, `Menu`, `Link`,
`ProgressBar`, `List`, `ListItem`, `Alert`, `Navigation`, `Toolbar` and
`Heading`; `AccessFlags` grows `COLLAPSED` (so `expanded` and `collapsed` are
both statements), and the dump prints it. `byard shot --a11y` prints them as it
does the rest: `Switch "Wi-Fi" = on`, `Tab "Home" [focusable selected]`,
`Link "Docs" [focusable]`. `byard-platform` maps each role in the table above
and sets `toggled` for a `ToggleButton`, `CheckBox` and `RadioButton`.

## Cost

- No attribute is read unless a tree is being built. A frame with the flag off
  allocates nothing extra (the RFC-0042 assertion still holds).
- The paint walk gains one scan of an element's attributes for `a11y_role`,
  only for an element that is already a focus candidate or sits in a group's
  path. A plain `Box` pays a comparison per attribute it already has.
- The keyboard adds a `Vec` push for each pressable element and each group
  member; both `Vec`s are cleared with the other handler tables each frame.
- `focus_visible` is one `bool` and one style bit.

## Drawbacks

- 23 keywords to learn, and a second way (`a11y_role: button`) beside the
  intrinsic `Button` to say the same thing. The intrinsic remains the right
  choice when its look is wanted.
- `a11y_role` cannot be reactive. A control that changes role (a `Button` that
  becomes a `toggle_button`) is two elements under a `when`.
- A tapped `Box` with no role is a `Button` in the tree but is not in the Tab
  order, so it can be heard and not reached. Changing it breaks the order of
  existing apps; the answer is the role, or `focus =>`.
- `focus_visible` is a heuristic on pointer-then-keyboard use, as it is in a
  browser; a person who clicks a button and then presses Space sees no ring,
  and sees it after pressing Tab.

## Rationale and alternatives

- **Every tapped `Box` focusable.** The most direct fix, and the one that
  reorders Tab in every app written since #347. A role is an explicit opt-in.
- **`a11y_role` as a string.** Would take a `var`, and could not be held to a
  set by the checker; a misspelling would be a silent `Group`.
- **A tri-state `a11y_checked`.** See §1.
- **`on focus_visible` replacing `on focused`.** Breaks every existing ring,
  and a bound `focused: var` still wants every kind of focus.
- **Focus-visible by input device rather than by how focus moved.** Browsers
  lean on "the last input was the keyboard". Here the router knows who moved
  focus, which is the same answer without tracking a modality.
- **Roving tabindex** (only one member of a group in the Tab order). The
  correct APG behaviour, and a change to Tab traversal that needs a group to
  remember its active member. Left to Unresolved; today every member is a stop.

## Prior art

WAI-ARIA 1.2 roles and the Authoring Practices keyboard tables; CSS
`:focus-visible` (the heuristic of §4 is its "text inputs always match"
clause); Flutter's `Semantics` widget (`button`, `checked`, `toggled`,
`selected`, `expanded` and a closed role set); SwiftUI's `accessibilityAddTraits`.

## Resolved questions

### Q1, whether `a11y_checked` is a `Bool` or a tri-state

**Resolution: a `Bool` and an `a11y_mixed` beside it.** §1.

### Q2, does the built-in controls' default ring move to `focus_visible`

**Resolution: there is no default ring.** The examples' rings move, and a
`TextField` is always focus-visible. §4.

### Q3, does an arrow key select in a tab list

**Resolution: not by default.** Radio groups select, tab lists do not (APG's
manual activation), and `focus =>` makes a strip follow focus. §3.

## Unresolved

- **Roving tabindex.** Tab should perhaps enter a group once, at its selected
  member, and leave it at the next Tab. Needs the group to know its selected
  member, which is `a11y_checked` or `a11y_selected` of the members: doable,
  but it changes where Tab stops for the built-in `RadioButton`s too.
- **A composed `slider`.** It has a role but no keys: the arrows would need to
  write a value, and a numeric `a11y_value` (min, max, step) is not a `Str`.
  A later `a11y_range: (min, max, step)` and a `change` handler can carry it.
- **Heading levels and live regions** (`a11y_level`, `a11y_live`), already
  listed by RFC-0042 as future.
- **`a11y_role` on a `TextField`'s look-alike** (`text_field` composed from a
  `Box`) names the node but gives it no caret or text editing; a composed text
  field is not supported.
- **`byard check` warning for a tapped `Box` with no role and no focus.**
  It is the one that can be heard and not reached.
