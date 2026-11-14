# RFC-0066: Overlays, motion in and out, and what a reader is told

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-14
- **Last updated:** 2026-11-14 (what the build found: focus returns when an overlay starts to leave; rows leave from the tail; a pair moves on a plain curve)
- **Depends on:** RFC-0010 (implicit animations and `from:`), RFC-0017 (overlays,
  scrims and the focus trap), RFC-0025 (`anim.stagger`), RFC-0036 (anchored
  overlays and light dismiss), RFC-0042 (the accessibility tree), RFC-0052 and
  RFC-0063 (composed controls, arrow-key groups), RFC-0053 (motion tokens),
  RFC-0059 (state blocks), RFC-0050 (Byard Material, whose wave 2 found all of
  this).

> **Placement:** `docs/rfcs/0066-overlays-motion-and-announcements.md`

---

## Summary

What a dialog, a menu, a sheet, a snackbar and a slider need and Byard doesn't
have yet:

1. **Focus goes in and comes back.** A dialog or a menu that opens takes focus
   (the element marked `autofocus`, or the first one Tab would reach), keeps it
   while open, and gives it back to whatever had it when it closes. `focused: v`
   becomes two-way: writing `true` moves focus there.
2. **Menus are arrow-key groups**, like a tab list, with submenus. Escape closes
   only the topmost overlay.
3. **`on enter` and `on leave`.** Two state blocks for how an element arrives and
   how it goes. An element that leaves stays drawn, out of reach, until its
   motion ends.
4. **Announcements and ranges.** `a11y_live` makes a screen reader say a change
   (a snackbar). `a11y_described_by` points an element at what describes it (a
   tooltip). `a11y_value_min`, `a11y_value_max` and `a11y_value_step` tell it a
   composed slider's range.
5. **Tooltips.** `anchor_to` names an element per instance, so a hundred tooltips
   don't need a hundred names. `pointer_enter`, `pointer_leave`, `focus_in` and
   `focus_out` say when the pointer or focus arrives on an element or anything
   inside it. `interactive: false` lets the pointer through an element.

## Motivation

Byard Material wave 2 (`notes-material-feat-overlays.md`,
`notes-material-refactor-actions-and-chips.md`,
`notes-material-refactor-selection-and-field.md`):

- **Dialog.** Material and WAI-ARIA: focus moves into a dialog when it opens and
  back to the opener when it closes. A Byard dialog can trap Tab but can't move
  focus in, so the first Tab is what enters it, and on close focus is lost.
- **Menu.** Up and Down move between items, Enter picks, Escape closes. Only a
  `tab_list` and a `radio_group` get arrows (RFC-0063 §4), so Material's menu
  announces its items as tabs. A submenu can't be entered, and Escape closes the
  whole stack of menus at once.
- **FabMenu, sheets, dialogs and snackbars** come in with motion and go out
  without any. `when open` removes an element on the frame `open` turns false.
  `from:` gives an entrance, and motion tokens can't take it (RFC-0053), so the
  FabMenu's stagger uses literal springs.
- **Snackbar.** A reader should say its message when it appears, without moving
  focus. There is no way to ask.
- **Slider.** The composed Slider (RFC-0063) tells a reader its value but not its
  range or step, so "40" has no "out of 100".
- **Tooltip** (`notes-material-feat-communication.md`). `anchor_to: "name"` is a
  string looked up in the whole window, so every tooltip instance needs a name of
  its own; the package generates six copies of its view. A tooltip shows when
  the pointer rests on its target or focus reaches it, and the target is a slot
  the tooltip can't put handlers on; it can only style itself with
  `focus_within`, so the rich tooltip never shows from the keyboard. Once shown,
  a plain tooltip catches the pointer it is drawn under, though it is only text.

## Guide-level explanation

### A dialog that takes focus

```byld
when open {
    Overlay #[dismiss => open = false] {
        Column #[a11y_role: dialog, a11y_label: "Discard draft?", anchor: center] {
            Text("Discard draft?")
            Row {
                m.Button("Cancel", variant: .text, on_tap: { open = false })
                m.Button("Discard", variant: .text, autofocus: true, on_tap: { discard() })
            }
        }
    }
}
```

When the overlay appears, focus moves to the element with `autofocus: true`, or
to the first element Tab would reach in it. When it goes, focus returns to the
element that had it before, if that element is still drawn. A modal overlay
does this always. A non-modal one does it when its content is a `dialog` or a
`menu`, so a tooltip or a dropdown of suggestions leaves focus where it is.

`focused: v` already reports focus. Now writing it moves focus: `v = true` focuses
the element (if it can take focus and is drawn), `v = false` takes focus away.

### A menu

```byld
Column #[a11y_role: menu] {
    MenuItem("Copy", on_tap: { copy() })
    MenuItem("Share", submenu: true) { … }
}
```

A `menu` is one stop. Up and Down move between its `menu_item`s and wrap, Home and
End go to the ends, Enter and Space press. An item that opens a submenu
(`a11y_expanded`) opens it with Right, and the submenu takes focus. Left or Escape
closes it and returns to the item. Escape closes the topmost overlay only, so a
submenu closes before its menu.

### Coming and going

```byld
when open {
    Column #[opacity: 1.0 with t.springEffectsFast,
             translate: (0, 0) with t.springSpatialFast] {
        on enter { opacity: 0.0, translate: (0, 24) }
        on leave { opacity: 0.0, translate: (0, 24) }
        …
    }
}
```

`on enter` gives the values the element starts at on its first frame. From there
it moves to its own values with their own `with`. `on leave` gives the values it
goes to when a `when`, a `for` or a closing overlay removes it. It stays drawn
until those motions end, and meanwhile takes no input, is not in the Tab order and
is not in the accessibility tree. Both work with tokens and with
`anim.stagger(t.springSpatialFast, 40ms, i)`, so a menu's items cascade in and out.

### What a reader is told

`a11y_live: polite` on an element makes a reader say its text when it appears or
changes, after what it is saying now. `assertive` interrupts. `a11y_value_min`,
`a11y_value_max` and `a11y_value_step` go with `a11y_value` on a `slider` or a
`progress_bar`.

## Reference-level explanation

### 1. Focus scopes

- An `Overlay` is a **focus scope** when it is modal, or when the first element in
  its content with a role has the role `dialog` or `menu`.
- **On mount**, the router records the focused element (its identity) as the
  scope's *return target*, then focuses the scope's element with `autofocus: true`
  (the first in tree order if several), or its first focusable. With none, focus
  is cleared, so keys go to the scope and not the page. A focus move shows the ring
  (`focus_visible`) when the overlay was opened by a key, and not when it was
  opened by a press, the same rule as a Tab after a click (RFC-0052).
- **While mounted**, Tab and Shift-Tab cycle inside the topmost scope (RFC-0017's
  trap, extended to the non-modal scopes).
- **On unmount** (when the overlay goes, or, with an `on leave`, when it starts to leave: a leaving element takes no input, so focus on it would be somewhere no key reaches for the length of the motion), focus returns to the
  return target if it is still drawn and focusable; otherwise to the nearest
  focusable ancestor of where it was; otherwise it is cleared.
- **`autofocus: Bool`** is accepted on any element that can take focus. On one
  that can't, it is `AutofocusNeedsFocus`, like `KeyNeedsFocus`.
- **`focused: v` is two-way.** A write of `true` to `v` focuses the element at the
  next frame, if it is focusable and drawn. A write of `false` blurs it if it has
  focus. Several writes in one frame: the last wins.

### 2. Menus and submenus

- `menu` joins `tab_list` and `radio_group` as an **arrow group** (RFC-0063 §4).
  It is one Tab stop entered at the selected item (`a11y_selected` or
  `a11y_checked`), else the first. Its axis is vertical: Up and Down move and
  wrap, Home and End go to the ends. A `menu` whose element is a `Row` is
  horizontal (a menu bar), with Left and Right.
- A `menu_item` with `a11y_expanded: false` is a **submenu opener**. Right (Left in
  a right-to-left layout) and Enter press it. Its submenu is an overlay with a
  `menu` inside, and it takes focus by §1. Left in a submenu dismisses it.
- **Escape** goes to the topmost overlay that has a `dismiss`, modal or anchored,
  and nothing else. Today it goes to the topmost modal and to every anchored one.

### 3. `on enter` and `on leave`

- They are state blocks (RFC-0059) with two new states. An element can have one
  of each. Only paint attributes and `translate`, `scale`, `rotate`, `opacity` are
  allowed in them. A layout attribute is `LayoutInStateBlock`, as for other blocks.
- **Enter.** On an element's first frame, each attribute named in `on enter`
  starts at that value and moves to its resolved value with the attribute's `with`
  (or instantly without one). An element without `on enter` is unchanged.
- **Leave.** When the element is removed (its `when` turns false, its `for` row
  goes, its overlay closes; rows of a `for` are positional, so it is the last rows that leave), it is kept in a *leaving* list with its last layout
  and its subtree. The attributes in `on leave` move to those values with their
  `with`. When every one of them has settled, it is removed. A leaving element
  draws where it was, takes no input, is skipped by Tab and hit-testing, and is not
  in the accessibility tree. If its `when` turns true again before it is gone, it
  comes back from where it is, as the same element, with its state.
- **Tokens with a start.** With `on enter`, a start value is written once, so a
  token (`with t.springSpatialFast`) and `anim.stagger(token, step, i)` need no
  `from:`. `InvalidAnimation`'s message points to `on enter` when a token is given
  `from:`.
- **Plain curves move pairs.** A `with` after a pair (`translate`, `scale`) with a
  plain curve or a token moves each axis; until now only a delayed or staggered one
  did. A delayed, staggered or repeating `with` with no `from:` starts from the
  target it had before, so `on enter`'s start value is all a token or a stagger needs.
- **Cost.** Nothing for an element without these blocks. A leaving element is
  drawn from its retained instances for as long as its motion runs.

### 4. Announcements and ranges

- `a11y_live: polite | assertive | off` on any element with a role or a text. The
  platform's live-region flag is set (AccessKit `Live`). The default is `off`.
- `a11y_value_min`, `a11y_value_max` and `a11y_value_step` (`Float`) on an element
  whose role is `slider` or `progress_bar`. On any other role they are
  `AttributeNeedsRole`. The built-in `Slider` already reports them.

### 5. Tooltips: anchors, crossings and a pointer that passes through

- **`anchor_to: name`** takes the name an `as` declared (RFC-0062 §5), resolved
  like `on name.state`: in scope, per instance. A string is still accepted and is
  looked up in the window as before, with a warning that points at the name form.
- **`pointer_enter =>`, `pointer_leave =>`, `focus_in =>`, `focus_out =>`** on any
  element. They fire when the pointer, or focus, arrives in or leaves the element's
  subtree, once per crossing, not for moves between its children. A wrapper around
  a slot gets them for whatever the slot holds. They don't make an element
  focusable.
- **`interactive: false`** takes the element and its subtree out of hit-testing:
  presses, hover and the wheel go to what is under it. It stays visible and in the
  accessibility tree.
- **`a11y_described_by: name`** links an element to the one that describes it
  (AccessKit `described_by`). A reader says the description after the name.

### 6. Diagnostics

| Error | When |
|---|---|
| `AutofocusNeedsFocus` | `autofocus` on an element nothing can focus |
| `LayoutInStateBlock` | a layout attribute in `on enter` or `on leave` |
| `DuplicateStateBlock` | two `on enter` (or `on leave`) blocks on one element |
| `AttributeNeedsRole` | a range attribute on a role that has no value |
| `UnknownStateTarget` | `anchor_to` or `a11y_described_by` with a name nothing is tagged with |

## Drawbacks

- **A leaving element is still drawn**, so a test that expects it gone right away
  has to wait for its motion (`--wait`), and `byard test` has to say so in its
  failure when an element is leaving.
- **Focus that moves on its own** surprises a sighted mouse user if done wrong;
  the ring rule in §1 keeps it quiet for pointer opens.

## Rationale and alternatives

- **A `transition:` attribute** with named presets (`fade`, `slide`). Fewer words
  for the common case, but a design system has its own motions. A preset can be a
  `let` with the blocks in a package.
- **An `AnimatedVisibility`-style wrapper element.** One more node per animated
  element, and `when` already says when.
- **Focus by command** (`focus(name)`). `focused:` is already the binding for
  focus, and two-way is what `bind` means elsewhere.

## Prior art

WAI-ARIA Authoring Practices for dialog (focus in and back) and menu (arrow keys,
submenus, Escape). Compose `AnimatedVisibility` (enter and exit), SwiftUI
`.transition`, Flutter `AnimatedSwitcher`. AccessKit `Live`, ARIA `aria-live` and
`aria-valuemin`/`max`.

## Unresolved questions

None.

## Future possibilities

- Type-ahead in a menu (a letter jumps to the item that starts with it).
- `on leave` for a whole `NavStack` route, to replace the route transitions'
  own mechanism.
