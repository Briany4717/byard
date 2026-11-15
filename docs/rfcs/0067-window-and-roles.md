# RFC-0067: The window as a value, spin buttons and grids, text styles on a canvas, aligned fields

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-14
- **Last updated:** 2026-11-14
- **Depends on:** RFC-0016 (responsive blocks and `[theme.breakpoints]`), RFC-0018
  (`when` and the structural pools), RFC-0020 (the `Canvas` and its `text`),
  RFC-0034 (`typo:`, weight and family), RFC-0041 (the text field's caret),
  RFC-0042 (the accessibility tree), RFC-0052 and RFC-0063 (roles, arrow-key
  groups, one Tab stop), RFC-0066 (`a11y_value_min`/`_max`/`_step`, menus as
  groups), RFC-0050 (Byard Material, whose last components are the reason).

> **Placement:** `docs/rfcs/0067-window-and-roles.md`

---

## Summary

Five small additions, each of which Byard Material's last components wrote
around:

1. **`window`.** A built-in record with `width` and `height`, in logical pixels,
   readable anywhere an expression is: `when window.width >= md { NavigationRail … }
   else { NavigationBar … }`. A bare name on the other side of such a comparison
   is a breakpoint from `[theme.breakpoints]`, the same names `on width >= md`
   takes. It is reactive, and an app that never writes `window` pays nothing.
2. **`a11y_role: spin_button`**, with a value, `a11y_value_min`, `a11y_value_max`
   and `a11y_value_step`, mapped to AccessKit's `SpinButton`.
3. **`grid`, `row` and `grid_cell` roles**, and a `grid` is an arrow-key group
   and one Tab stop: Left and Right move by cell, Up and Down by row, Home and End
   to the ends of the row. Every other key, PageUp and PageDown among them, is the
   control's.
4. **`typo:` on a `Canvas` `text`**, as on a `Text`.
5. **`text_align: start | center | end` on a `TextField`.**

None of them costs anything to a program that does not write it.

## Motivation

Byard Material wave 1 (`notes-material-feat-scaffold-and-audit.md`,
`notes-material-feat-time-picker.md`, `notes-material-feat-date-picker.md`; rows
44 to 47 and 51 of `wave2-engine-gaps.md`):

- **Scaffold** has to pick a navigation bar under the body on a phone and a rail
  beside it on a tablet. The window's width is visible only as `on width >= md` in
  a style block, which can change padding and direction but not which views are
  there, so Scaffold takes `rail: Bool` and every app writes the same
  breakpoint test by hand, outside the one place that knows the sizes.
- **TimePicker** has hour and minute boxes that a reader should call spin
  buttons (they have a value, a range, and Up and Down change them). There is no
  such role; they are `slider`s, which a screen reader says in a different way.
  Its dial numbers are drawn with Canvas `text`, which takes `size` and nothing
  else, so they come out in the fallback face and not in Roboto Flex, and its
  digit boxes are `TextField`s whose digits cannot be centred: the text starts at
  a fixed padding.
- **DatePicker** draws a month as a grid of days. There is no `grid`, `row` or
  `gridcell`, so the month is a `group` of `group`s, and the one Tab stop with
  arrow keys, which a `tab_list` and a `radio_group` have, is made by a hidden
  zero-size button that stands for the day the cursor is on. A reader hears a
  button whose name changes.

All five are things a screen reader or a user would expect of any such control,
and each is missing at the same level: a keyword, a property, a value.

## Guide-level explanation

### 1. The window

```byld
View Main() {
    Column #[grow: 1] {
        Text("Inbox")
        when window.width >= md {
            Row { Text("rail") Text("body") }
        } else {
            Column { Text("body") Text("bar") }
        }
    }
}
```

With `md = 600` in `[theme.breakpoints]`, the row is built while the window is at
least 600 logical pixels wide and the column below it, and the one that is not
wanted is not in the tree. Resize the window and it changes at exactly 600.

`window` is a record:

| Field | Type | |
|---|---|---|
| `window.width` | `Float` | the window's width in logical pixels |
| `window.height` | `Float` | its height |

It is read where any value is: a `when`, a `let`, an attribute (`width: window.width
* 0.8`), a text (`"{window.width}"`). A `let` that reads it follows it.

A name beside `window.width` or `window.height` in a comparison is a breakpoint,
checked as after `on width >=`:

```byld,fails
View Main() {
    when window.width >= huge { Text("wide") }
}
```

```diagnostic
error[UnknownBreakpoint]: no breakpoint `huge` is declared
```

A number works as well (`window.width >= 600`), and so does `<`. That is the whole
rule for the names: `md` on its own, anywhere else, is still `md`, an unknown
name. A view that has a `var` or a parameter named `window` reads that.

`window` is the window, not the element: for what an element's own box measures,
`on measure` stays the answer (RFC-0038).

### 2. A spin button

```byld
Box #[a11y_role: spin_button, a11y_label: "Hours", a11y_value: "{hour}",
      a11y_value_min: 1, a11y_value_max: 12, a11y_value_step: 1,
      focused: hours_focus,
      key_down(k) => { if k == "Up" { hour = hour % 12 + 1 } }] {
    Text("{hour}")
}
```

```text
SpinButton "Hours" = 4 in 1..12 step 1 [focusable]
```

A reader says "Hours, spin button, 4", and its Increment and Decrement send `change`
as they do for a `slider`. Up and Down are the control's: it is a `Box`, and the
keys are the `key_down` it writes, as RFC-0063 gave every control the keyboard
reaches.

### 3. A grid

```byld
Column #[a11y_role: grid, a11y_label: "November 2026"] {
    Row #[a11y_role: row] {
        for d in week1 {
            Box #[a11y_role: grid_cell, a11y_label: d.name, a11y_selected: d.on,
                  tap => pick(d), key_down(k) => paging(k)] { Text(d.label) }
        }
    }
    // … four more rows
}
```

Tab enters the grid once, at the selected cell (the first if none is), and leaves it
at the next Tab. Inside:

| Key | Moves focus |
|---|---|
| Right, Left | to the next or previous cell, on to the next or previous row at the end of one; not past the first or last cell |
| Down, Up | to the cell in the same column of the next or previous row (the last cell of a shorter row); not past the first or last row |
| Home, End | to the first or last cell of the row |

Every key the grid does not move with goes to the focused cell's `key_down`,
including **PageUp and PageDown**, which a calendar means as "a month back" and
a spreadsheet as "a screenful": the control knows which, so the grid does not
guess. So does an arrow that has nowhere to go: Up on the first row is the
cell's `key_down("Up")`, which is where a date picker turns the page.

A grid that is a layout `Grid` has no `row` elements to write:

```byld
Grid #[a11y_role: grid, a11y_label: "Days", columns: "repeat(7, 1fr)"] {
    for d in days {
        Box #[a11y_role: grid_cell, a11y_label: d.name, tap => pick(d)] { Text(d.label) }
    }
}
```

Its rows are as many cells as the template has columns.

### 4. Text styles on a canvas, and aligned fields

```byld
Canvas #[width: 256, height: 256] {
    text("12", x: 128, y: 28, typo: labelLarge, align: center, color: 0x1D1B20)
}

TextField #[bind: hour, width: 96, height: 72, typo: displayMedium, text_align: center]
```

`typo:` on a `text` takes what it does on a `Text`: the token's size, weight and
family. A `size:` written beside it wins over the token's size. `text_align`
places the text, the placeholder and the caret in the field's content box;
text longer than the box scrolls as it did.

## Reference-level explanation

### 1. `window`

**The value.** `window` is the built-in name of a record `(width: Float, height:
Float)`. It is resolved where a name is: after the view's own bindings, a lambda's
parameters, the event payload and the program's constants, so a binding called
`window` hides it. `window.width` and `window.height` are read as the reactive
reads of two signals; `window` alone gives the record.

**Where the signals live.** The interpreter holds `window: Option<(SignalId,
SignalId)>`. It is `None` until a program lowers a read of `window`, which creates
the pair with the viewport the interpreter is rendering at. At the top of every
frame, before the structural pass, the viewport is compared with the stored pair:
unchanged, nothing else happens (one comparison, and only when the pair exists);
changed, the two signals are written and the reactive context is pulled, so a
`when` that reads them is reconciled in the same frame the window changed in and
is never painted for a size it no longer has. A program that never writes
`window` has `None` and pays nothing.

**Breakpoints.** In a comparison (`<`, `<=`, `>`, `>=`, `==`, `!=`) one of whose
sides is exactly `window.width` or `window.height`, a bare name on the other side
that nothing binds is read as a breakpoint, by the lookup `on width >= md` does:
`[theme.breakpoints]` of the project (and of its packages), by the name as
written. Its value is the width in logical pixels, as a `Float`. A name that is not
declared is `UnknownBreakpoint` (the same error, the same hint), at the name, and
the comparison is `false`, which is what a block with an undeclared breakpoint
does (RFC-0016). A name the view does bind, or a number, is read as it always is.
No other position reads a breakpoint: the rule is as narrow as the thing it is
for, and `md + 1` is still an unknown name.

**The checker.** `window` is in the checker's list of names the engine provides.
`window.width` and `window.height` are `Float`; any other field is `UnknownField`
with the closest field as a hint. The bare name in the comparison is not an
`UnknownName` here: whether it is a breakpoint is the project's to say, and the
checker has no project. It is the interpreter's `UnknownBreakpoint`, which `byard
check` runs.

**Hot reload.** The pair of signals belongs to the interpreter and survives a
reload, as the theme's scheme does. A changed breakpoint in `byard.toml` is read
when the views are lowered again, as for a style block.

### 2. `spin_button`

`AccessRole::SpinButton` is named `spin_button` in `a11y_role:` (the keyword set
grows by it, and the closest-word hint with it) and in the dump. It is mapped to
AccessKit's `Role::SpinButton`.

It is a **value role** as `slider` and `progress_bar` are: `a11y_value_min`,
`a11y_value_max` and `a11y_value_step` are accepted on it (and still
`AttributeNeedsRole` on an element whose role cannot be one that has a range, now
saying "a `slider`, `spin_button` or `progress_bar`"), and when any is written the
node's value is a number, `a11y_value` read as one, with the range. A node with
the role and no range is a text value, as a `slider` is.

When the element has a `change` handler, a reader's Increment, Decrement and
SetValue are offered and run it, with the same payload a `slider`'s are
(RFC-0042 §5). The control does the rest: it is not a role that takes focus by
itself (`role_implies_focus` is unchanged), so a spin button is reachable by Tab
when it has `focus =>` or `focused:`, as a composed slider is, and its Up and Down
are the `key_down` it writes. The engine does not move a value it does not own.

The dump writes it as a slider is written: `SpinButton "Hours" = 4 in 1..12 step 1`.
A spin button is a node that needs a name (`(unnamed)` without one).

### 3. `grid`, `row`, `grid_cell`

**Roles.** Three keywords, `grid`, `row` and `grid_cell`, and three
`AccessRole`s (`Grid`, `Row`, `GridCell`), mapped to AccessKit's `Grid`, `Row`
and `GridCell`. A `grid` and a `grid_cell` need a name (a screen reader says the
month and the day); a `row` does not. A `grid_cell` is named from its text, like
a `tab`; `Text` inside it is part of its name. A `row` that is no node of a grid
is still a node: the role does not check where it is, as no role does.

A `grid_cell` is a role that implies focus: with a `tap` it is a Tab stop and Space
and Enter press it (RFC-0052 §2), like a `tab`. An `a11y_selected` or `a11y_checked`
on it, or `selected:`, says which cell is the one Tab enters at (RFC-0063 §4).

**The group.** An element with `a11y_role: grid` opens a group, as `tab_list`,
`radio_group` and `menu` do, of kind `Grid`; its members are the `grid_cell`s
inside it that are focus targets, in tree order. Each member records the row it
is in:

1. when the grid has `row` elements, the row is the `row` it is in. Cells
   outside any `row` are in the row that was last opened, or the first;
2. otherwise, if the grid is a layout `Grid` with a `columns:` template of `n`
   tracks, the row is the member's index divided by `n`;
3. otherwise, the grid is one row.

The group is one Tab stop, by the rule RFC-0063 §4 already has for a group:
the cell with focus, else the selected one, else the first enabled.

**The keys.** For ArrowLeft, ArrowRight, ArrowUp, ArrowDown, Home and End with no
Ctrl, Alt or Meta (the group's rule for every kind), on a focused member of a grid:

- Right and Left go to the next and previous **enabled** cell in tree order, which
  runs through the end of one row into the start of the next. At the first or last
  cell there is nowhere to go.
- Down and Up go to the cell with the same position in the row (its column) in the
  nearest row below or above that has an enabled cell there; when that row is
  shorter, its last cell. With no such row there is nowhere to go.
- Home and End go to the first and last enabled cell of the focused cell's row.

Focus moves as for any group (`steal_focus`, so `focus` handlers, `focused:`
bindings and the focus ring follow), and the key is used. **When there is nowhere
to go the key is not used**: it falls through to the focused element's `key_down`,
which is how a date picker turns the page at the edge of a month. Disabled cells
are skipped; a cell's column is counted among all the cells of its row, enabled or
not, so a disabled day does not shift the columns of the days after it. A grid
selects nothing as it moves (it is not a radio group): Space or Enter press the cell.

**What is left to the control.** PageUp, PageDown, and every key the table above
does not list, with or without modifiers, and Ctrl+Home and Ctrl+End, are not the
grid's: they go to `key_down` of the focused cell, as they do for a `tab`, whose
arrows are the router's and whose other keys are not. Nothing is added to
`key_down` for them: the key's name is the value, as RFC-0063 §1 has it.

**The tree.** Row and cell are nodes; layout containers between them are
flattened as always, so a `grid` holds `row`s that hold `grid_cell`s. A cell's
state (selected, disabled, focusable, focused) is the one every role has.

### 4. `typo:` on a canvas `text`

`typo` joins `size`, `color` and `align` as an optional named parameter of the
`text` command, of the type it has on a `Text` (a theme typography token, bare or
through the injected theme, or a `Str` holding one). Resolution, in the order a
`Text` uses:

- **size:** `size:` if written, else the token's size, else the theme's body size;
- **weight:** the token's, else 400;
- **family:** the token's, as a `Text`'s `resolve_family` gives it.

`font:` and `weight:` are not added. The family is used **only when `typo:` is
written**: a `text` without one is drawn and measured exactly as it was, so no
existing canvas changes. The text is measured with the weight and family it is
drawn in, so `align: center` and `align: end` are exact.

### 5. `text_align` on a `TextField`

`text_align` takes `start`, `center` or `end`, default `start` (a keyword set, so
a misspelling is an error with the closest word). It is paint class: it moves the
text inside the content box and not the box.

On a single-line field, the value (or the placeholder, when that is what shows),
the selection, the caret and the composition's preedit are drawn at an offset from
the start of the content box: `0` for `start`, half the free room for `center`
and all of it for `end`, where the free room is the content box's width minus the
width of what is drawn (value and preedit). When what is drawn is as wide as the box
or wider, the offset is `0` and the field scrolls to keep the caret in view, as it
did. The pointer places the caret by the same offset.

`text_align: center` or `end` on a multiline field (`lines`, `min_lines` or
`max_lines` above one) is `TextAlignNeedsOneLine`, at the attribute, which says
that a multiline field's lines are left-aligned; `start` is allowed anywhere.
An expression that is only known at run time is accepted and reported once, when the
field is drawn, if it names the keywords and the field is multiline.

## Cost

- **`window`:** one `Option` test per frame when unused. Used: one comparison of
  two floats per frame, and two signal writes and a pull on the frames the window
  changed size in, which are also frames that lay out again.
- **Roles:** one more arm in the role table and the keyword list. A grid registers
  as a group as a `tab_list` does, with a row number per member; an app with no
  grid does not enter that code.
- **Grid keys:** the group's key handler is run only for the six keys and only
  when a group exists, as before; the grid arm reads the members it has.
- **Canvas `typo`:** read for a `text` that writes it.
- **`text_align`:** read for a `TextField` that writes it; `start` is the path
  that was there. A non-start field measures the text it draws when it is not being
  edited, which it measured already to decide on the clip.

## Drawbacks

- **A name that is a number only here.** `md` is a number beside `window.width`
  and an unknown name elsewhere. The narrow rule is deliberate (Q1) and the
  alternative is a name that means two things everywhere.
- **Left and Right cross rows.** A data grid might want them to stop at the end of
  a row. A calendar, which is what the first user is, wants them to continue. A control
  that wants them to stop has no way to say so, since an arrow that moves does not
  reach its `key_down`. Q3 says why this is the rule anyway.
- **`window` is read, not subscribed to per element.** Every reader of `window.width`
  re-runs on every change of width while a window is being dragged to a new size.
  That is the cost of a value, and the frame already lays out again for it.
- **Roles that are not checked in place.** A `grid_cell` outside a `grid` is a
  focus target with no group, as a `tab` outside a `tab_list` is.

## Rationale and alternatives

- **Do nothing; keep `rail: Bool`.** It works, and puts the breakpoint in every
  app. Scaffold is the only component that wanted the window's size, but anything
  that picks a structure by size wants it, and `on width` cannot.
- **`window.md` as a `Bool` field per breakpoint.** It reads well and needs
  the record to carry the project's breakpoints, which makes the checker know the
  manifest. The comparison is the thing the guide already teaches (`on width >=
  md`), and `window.width >= 600` works with no breakpoints declared.
- **Any bare breakpoint name is a number.** `md` would then be a keyword the
  project declares, shadowed by anything the program called `md`, and a typo in a
  name would silently be a different one. The comparison with `window` is the one
  position where the meaning cannot be anything else.
- **A `when width >= md` form.** One more statement for what an expression already
  says, and a second place that knows about the window.
- **Making `spin_button` move its own value.** The engine does not know where a
  composed control keeps its number, and a built-in one would be another widget.
  `slider` already lets the control own the value and tells the reader the range.
- **A `gridcell` that stays on the row's columns** (Left and Right stop at the row
  end), as the APG's data grid does. The APG's date picker grid is the opposite and
  is the reason for this RFC. It is one rule with no option.
- **PageUp and PageDown moved by the grid** (a row count, a month). The grid cannot
  know which; the control can, in one line.
- **`typo:` also carrying `font:` and `weight:` on a canvas `text`.** `typo` is the
  one Material writes. The others are three more names for what a token says.
- **`text_align` as `align`** (as on `Text`). `align` on a field would be read as
  the field's alignment in its parent, and the one on a `Text` is the same word for
  the same thing, which is the reason to give the field its own word.

## Prior art

- **WAI-ARIA Authoring Practices:** the grid pattern (one Tab stop, arrows by
  cell, Home and End by row, Page keys the widget's), the date picker dialog's grid
  (Right on the last day of a week goes to the next), `spinbutton` (value, min,
  max; Up and Down are the widget's).
- **AccessKit:** `Role::SpinButton`, `Role::Grid`, `Role::Row`, `Role::GridCell`.
- **CSS and SwiftUI:** `window.innerWidth`, `matchMedia`; SwiftUI's
  `horizontalSizeClass` environment value, a value a view can read to choose
  structure.
- **Jetpack Compose:** `WindowSizeClass`, the name Material gives to the same idea.
- **Flutter:** `TextField.textAlign`, `TextStyle` on `TextPainter`.

## Resolved questions

### Q1, how a breakpoint name is read in an expression

**Question.** `window.width >= md`: where does `md` come from?

**Options.** (a) any unbound name that is a declared breakpoint is a number;
(b) fields of `window`, `window.md`; (c) a name beside `window.width` or
`window.height` in a comparison.

**Resolution: (c).** It is the same sentence as `on width >= md`, needs no new name
anywhere else, and a typo is an error with a hint instead of a different name. (a)
makes every unknown-name check depend on the manifest. (b) is a second spelling.

### Q2, what reads the window's size

**Question.** The window, the element, or the safe area.

**Resolution: the window, the viewport the interpreter renders at**, which is what
`on width` compares. An element's own size is `on measure`'s.

### Q3, do Left and Right cross rows

**Question.** At the last cell of a row, Right does what?

**Options.** (a) stops; (b) goes to the first cell of the next row; (c) wraps within
the row.

**Resolution: (b), and stops at the grid's ends.** The month grid is the first user
and a calendar is read as a sequence of days. (a) and (c) are the data grid's rules
and nothing in Byard needs them yet; if one does, a property on the grid chooses.

### Q4, what moves when there is nowhere to go

**Resolution: nothing, and the key is the control's.** The alternative, to consume
it, would leave a date picker with no way to turn the page from an arrow.

### Q5, does `text_align` on a multiline field do something

**Resolution: it is an error for `center` and `end`.** A multiline field's lines
are laid out by a different pass, and drawing the one the author asked for in a
place that is not what they asked is the silence Byard does not have.

## Unresolved

- **`window.scale`, `window.dark`, a safe area.** The two a program has needed are
  here; the rest wait for a component that needs one.
- **`grid` rows and columns in the tree** (`aria-rowcount`, `aria-colindex`).
  AccessKit has them; no screen reader has been run against Byard's yet (RFC-0042
  #353), and a month is small enough to be read by cell.
- **`text_align` for a multiline field,** per line.
- **A `row` property for Left and Right** (stop, cross, wrap), if a second grid
  wants another rule.
