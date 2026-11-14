# Controls

```byld
View Main() {
    var name = ""
    var volume: Float = 0.4
    var notifications = true
    var terms = false
    var plan = "pro"

    Column #[bg: 0x14141C, p: 28, gap: 16, width: 420] {
        Text("Hello, {name == "" ? "stranger" : name}") #[color: 0xFFFFFF, size: 22]

        TextField #[bind: name, placeholder: "Your name", bg: 0x1E1E2A, color: 0xFFFFFF,
                    radius: 8, p: (horizontal: 12), height: 40]

        Row #[gap: 12, align: center] {
            Text("Volume") #[color: 0x9AA0AC]
            Slider #[bind: volume, min: 0.0, max: 1.0, step: 0.05, width: 220, height: 26]
            Text("{volume:.2}") #[color: 0xFFFFFF]
        }

        Row #[gap: 12, align: center] {
            Toggle #[bind: notifications, bg: 0x3B82F6, width: 52, height: 30]
            Text("Notifications are {notifications ? "on" : "off"}") #[color: 0xFFFFFF]
        }

        Row #[gap: 12, align: center] {
            Checkbox #[bind: terms, width: 22, height: 22]
            Text("I accept the terms") #[color: 0xFFFFFF]
        }

        Row #[gap: 16] {
            Row #[gap: 8, align: center] {
                RadioButton #[value: "free", bind: plan, width: 22, height: 22]
                Text("Free") #[color: 0xFFFFFF]
            }
            Row #[gap: 8, align: center] {
                RadioButton #[value: "pro", bind: plan, width: 22, height: 22]
                Text("Pro") #[color: 0xFFFFFF]
            }
        }

        Button("Sign up") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8, disabled: !terms]
            => name = ""
    }
}
```

Controls are the elements a user changes. Every one of them draws itself and handles
its own input, and none needs code from you to work. You bring a `var`, name it with
`bind`, and the control and the `var` stay in step.

## `bind`: two ways at once

`bind: name` reads the `var`, shows it, and writes it back when the user changes the
control. The `var` is the single source of truth: change it anywhere, in a button's
action or in a reply from a server, and the control shows the new value.

The type of the `var` has to match the control:

| Control | Binds a | Shows |
|---------|---------|-------|
| `TextField` | `Str` | Editable text |
| `Toggle` | `Bool` | A switch |
| `Checkbox` | `Bool` | A square, ticked when true |
| `Slider` | `Float` | A thumb on a track |
| `RadioButton` | `Str` | One of a group |

`value:` is the older name for `bind:` and does the same thing (RFC-0018). Both
need a `var` of the view the control is in, or a parameter the view declares with
`bind` (below). A `let`, a literal, a loop variable or a plain parameter cannot be
written, so each is an error that says so: the control would draw its default and
ignore every edit. A `change` handler runs an action
when the user changes it.

### A field in a view of its own

A parameter is a copy of what the caller passed, so a control cannot write it back.
To reuse a field, declare the parameter a *binding*: `bind value: Str`. The caller
passes a `var` of its own, and the view reads and writes that `var` as if it were its
own, so the control inside binds it like any other:

```byld
View Field(bind value: Str) {
    TextField #[bind: value, placeholder: "Type here", width: 220, height: 36]
}

View Main() {
    var name = "Ada"

    Column #[p: 16, gap: 8] {
        Field(bind: name)
        Button("Clear") => name = ""
        Text("name is {name}")
    }
}
```

There is no copy in between. Typing in the field writes `name` in `Main`, the line under
it follows in the same frame, and the button clearing `name` empties the field. This is
how every control above works, now for a view of your own. [Views and
parameters](views-and-parameters.md#binding-a-var) has the rules, and `bind` on a
parameter that is not a binding is the mistake the checker stops:

```byld,fails
View Field(value: Str) {
    TextField #[bind: value]
}

View Main() {
    var name = ""
    Field(value: name)
}
```

```diagnostic
error[BindNotAVar]: `TextField #[bind: value]` cannot change `value`: a parameter is a copy of what the caller passed
```

The message names the fix: declare it `bind value: Str`. A view that wants to keep a
copy of what it was given, to edit it and hand the result up when the user is done,
still takes a value and a callback. `examples/reusable_field` does.

## Button

```byld,fragment
var count = 0
Button("Tap me") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8] => count++
```

A `Button` takes its label as content. With no `p`, it pads the label by 10 above and
below and 24 left and right, and it is at least 40 pixels tall, so a bare button is
already something a finger can hit. Say `p`, `width` or `height` to change that. It
takes every layout, decoration and text attribute. Tab reaches it, and Space or Enter presses it,
as [the keyboard](#the-keyboard) describes.

## TextField

A `TextField` is a single line of editable text with a caret. It supports input
methods: a Japanese or Korean composition shows underlined as you type it, and the
candidate window follows the caret, even inside a scrolled view. Dead keys work. The
value does not change until a composition is committed.

```byld,fragment
var query = ""
TextField #[bind: query, placeholder: "Search", grow: 1, height: 40, bg: 0x1E1E2A,
            color: 0xFFFFFF, radius: 8, p: (horizontal: 12)]
```

Its `color` colours the text and the caret, and the placeholder is the same colour
fainter. `p` sets where the text starts, and a `typo` gives its size.

A field's width follows what you write. With `width` it is that wide. With `grow: 1` and no
`width` it has no width of its own and takes whatever room is left, so a field between two
24 wide icons in a `Row` leaves them 24 wide, and in a `Column` it spans the column. With neither, a
field falls back to 200. Its height is 36 unless `height` says otherwise. The text sits 10 from the
left edge unless `p` or `pl` says otherwise; that inset is part of the field and stays at 10,
so a design that wants none writes `p: (horizontal: 0)`.

While it has focus a field draws a 2 pixel line in the theme's `primary` along its bottom edge.
`indicator: none` leaves it out, for a design that draws its own, and `indicator: line` is the
default. Whatever else a focused field should look like is the field's own `border` and
`on focused`, as for any element:

```byld,fragment
TextField #[bind: query, grow: 1, height: 24, p: (horizontal: 0), indicator: none,
            color: 0xFFFFFF]
```

Its events are `input` and `change`, which run after each edit, and `submit`, which
runs on Enter. Each can name the text, as `input(text) => …`. See
[events](events-and-actions.md#handlers-that-take-a-value). An edit that changes nothing,
such as Backspace at the start, runs nothing.

### More than one line

`lines` makes a field several lines high, and `min_lines` and `max_lines` let it grow with
what it holds:

```byld,fragment
TextField #[bind: notes, lines: 3, width: 320]                      // always 3 lines
TextField #[bind: notes, min_lines: 2, max_lines: 6, width: 320]    // 2 to 6, then it scrolls
```

Long lines wrap at the field's width. Enter inserts a line break and does not run `submit`;
`Mod`+Enter runs it instead, without a break. A paste keeps its breaks. Up and
Down move between lines and keep the column, Home and End go to the ends of a line, and
`Mod`+Home and `Mod`+End to the ends of the text (`Cmd`+Up and `Cmd`+Down on macOS), and PageUp and
PageDown a page of lines. The wheel and a trackpad scroll a field that holds more lines than it
shows (at its top or bottom the wheel goes on to a `ScrollView` around it), and the text stays where it was scrolled until the caret moves or something is typed. A
screen reader is told the field has several lines. The
selection is painted a rectangle to a line, and the text scrolls to keep the caret in view
once it is longer than the field. The vertical padding is 8 unless `p`, `pt` or `pb` says
otherwise, an explicit `height` beats `lines`, and `min_lines` alone has no ceiling.

`lines: 1`, `max_lines: 1` and writing none of them are the single-line field, so a view can
forward `lines: lines` with a default of 1. `lines` together with `min_lines` or `max_lines`,
a minimum above the maximum, and a count below 1 are errors.

### Editing

A field edits like a text field anywhere else, and the `byld` above needs nothing
more for it. While it has focus:

| Input | Does |
|-------|------|
| Left, Right | Move the caret one character. With a selection, collapse it to its start or end. |
| `Ctrl`+Left, `Ctrl`+Right | Move by word. `Option` on macOS. |
| Home, End | Go to the start or the end. `Cmd`+Left and `Cmd`+Right do the same on macOS. |
| `Shift` with any of these | Select instead of moving. |
| Backspace, Delete | Remove the selection, or the character before or after the caret. |
| `Ctrl`+Backspace, `Ctrl`+Delete | Remove a word. `Option` on macOS. |
| `Mod`+A | Select all. |
| `Mod`+C, `Mod`+X, `Mod`+V | Copy, cut and paste. |
| `Mod`+Z | Undo. |
| `Mod`+`Shift`+Z, `Mod`+Y | Redo. |
| A press | Place the caret under the pointer. |
| A drag | Select from the press to the pointer. |
| A double press | Select the word under the pointer. |

`Mod` is Command on macOS and Control elsewhere. A character is a whole grapheme, so
an accented letter or an emoji moves and deletes as one. Typing replaces the selection.
A paste is one line: its line breaks become spaces (a [multiline field](#more-than-one-line) keeps them). The clipboard is the system's.

Typing is undone a word at a time, and a paste, a cut or a deleted selection is a step of
its own. When the `var` is written from somewhere else, such as a button's action or a
reply from a server, the caret goes to the end of the new text and the undo history starts
again. A text wider than the field scrolls to keep the caret in view. The caret blinks
while the field and the window have focus.

A field keeps its focus, its caret, its selection and its undo history for as long as
it is on screen, however the things around it change: a message that appears above it
as you type, or a list before it that grows or shrinks, does not move any of that.
A field that a `when` removes lets go of focus, and its `focused:` var goes back to
`false`.

The selection is painted in the theme's `primary` colour at 30 % opacity. Give the
field `selection:` to choose another:

```byld
View Main() {
    var note = "select some of this text"

    Column #[bg: 0x14141C, p: 28, width: 400, grow: 1] {
        TextField #[bind: note, bg: 0x1E1E2A, color: 0xFFFFFF, radius: 8,
                    p: (horizontal: 12), height: 40, selection: 0x66E4A11B]
    }
}
```

To see a caret and a selection without a window, give `byard shot` the keys to press and
the text to type. See [the dev loop](dev-loop.md#taps-keys-and-typing).

## Toggle

```byld,fragment
var on = false
Toggle #[bind: on, bg: 0x3B82F6, width: 52, height: 30]
```

A switch. It draws its own track and thumb, so `bg` is the colour of the *on* track,
and the `on checked` style state is active when it is on. Space or Enter flips it while it
has focus.

## Checkbox

A box with a tick. `bg` is the colour it fills with when checked, and `on checked`
styles it. `indeterminate: true` draws the mixed dash for a parent of partly chosen
items. It is a display state, so give it no `bind`:

```byld
View Main() {
    var wifi = true

    Row #[bg: 0x14141C, p: 20, gap: 12, align: center] {
        Checkbox #[bind: wifi, bg: 0x7DD3A8, width: 22, height: 22]
        Checkbox #[indeterminate: true, bg: 0xD0BCFF, width: 22, height: 22]
        Text("Wi-Fi is {wifi ? "on" : "off"}") #[color: 0xFFFFFF]
    }
}
```

## Slider

A `Slider` binds a `Float`. `min` and `max` give the range, `0.0` to `1.0` by default,
and `step` snaps it. Without `step` it is continuous.

```byld,fragment
var percent: Float = 65.0
Slider #[bind: percent, min: 0.0, max: 100.0, step: 1.0, width: 300, height: 26]
```

Bind it to a `Float`, not an `Int`, and write its bounds as floats. A slider is also
a good way to try out a number: bind one to anything and drag it. With focus, the
arrow keys move it by `step`, or by a tenth of the range when there is none.

## RadioButton

A group of radio buttons is one `var`. Each button names its own `value`, and all of
them `bind` the same `var`. A button is selected when the `var` equals its `value`,
and pressing it writes its `value`, which deselects the others by itself. The arrow
keys move the selection through a group.

```byld
View Main() {
    var plan = "pro"

    Column #[bg: 0x14141C, p: 28, gap: 14, width: 320] {
        Text("Choose a plan") #[color: 0xE6E1E5, size: 20]
        for option in ["free", "pro", "team"] {
            Row #[gap: 12, align: center] {
                RadioButton #[value: option, bind: plan, width: 22, height: 22]
                Text(option) #[color: 0xCAC4D0, size: 16]
            }
        }
        Text("Selected: {plan}") #[color: 0x938F99, size: 13]
    }
}
```

## The keyboard

Tab moves focus to the next control and Shift+Tab to the one before, in the order the
controls are written, wrapping at the ends. A `Button` (with something to do), a
`Toggle`, a `Slider`, a `Checkbox`, a `RadioButton` and a `TextField` are all in that
order, and a disabled control is skipped, by Tab and by a click.

What a focused control does with a key:

| Control | Key | Does |
|---------|-----|------|
| `Button` | Space, Enter | Presses it, once however long the key is held |
| `Toggle`, `Checkbox` | Space (and Enter for a `Toggle`) | Flips it |
| `Slider` | Arrow keys | Steps the value, up to its `min` and `max` |
| `RadioButton` | Arrow keys | Selects the neighbour in its group |
| `TextField` | Space types a space; Enter submits | Nothing else is pressed |

Focus draws nothing by itself. Style it with `on focus_visible`, as in
`style { on focus_visible { border: 0xB69DF8, border_width: 2 } }`, and a control that does
not shows no ring. `focus_visible` is the state for a ring: it is set when the keyboard
moved focus and not when a mouse press did, so clicking a button does not leave a ring on it
and pressing Tab draws one. A `TextField` is always focus-visible, since you are typing in
it. `on focused` is the state for every kind of focus, a click included, and still works as
it did. `examples/keyboard_focus` draws the ring, and
`byard shot crates/byard-cli/examples/keyboard_focus --key Tab --key Space -o after.png`
draws it with the first button focused and pressed.

### A control you build from a Box

A `Box` with a `tap` is not in the Tab order by itself, and a reader hears a button with
no state. Give it a role and it is a control like the built-in ones: `a11y_role` says what
it is, and an element with a role that implies focus and a `tap` is in the Tab order, pressed
by the keys its role takes.

```byld
View Main() {
    var wifi = true
    let ring = style { on focus_visible { border: 0xD0BCFF, border_width: 3 } }

    Row #[p: 24, gap: 12, align: center] {
        Box #[..ring, a11y_role: switch, a11y_checked: wifi, width: 52, height: 32,
              radius: 16, bg: (wifi ? 0x6750A4 : 0x49454F), tap => wifi = !wifi]
        Text("Wi-Fi") #[color: 0xCAC4D0, size: 16]
    }
}
```

A role that implies focus is `button`, `toggle_button`, `switch`, `checkbox`, `radio`,
`tab`, `menu_item` and `link`. A role alone does not put an element in the Tab order if it
has nothing to press; an element with a `focus =>`, a `blur =>` or a `focused:` is in it
whatever its role, and a `tap` there is pressed by the keys too. A `Box` with only a `tap`
and no role stays out of the order. What presses it follows the role:

| Role | Space | Enter |
|------|-------|-------|
| none, `button`, `toggle_button`, `menu_item`, `tab` | presses | presses |
| `switch` | presses | presses |
| `checkbox`, `radio` | presses | does nothing |
| `link` | does nothing | presses |

A key a role does not take goes on to the element's own `key_down`, which any element Tab
can reach takes (see [Events and actions](events-and-actions.md)). Holding a key presses
once.

A row with `a11y_role: tab_list`, or `radio_group`, holds the elements with the role `tab`
or `radio` inside it as a group, and the arrow keys walk it:

| Key | Does |
|-----|------|
| Right, Down | Focus the next, wrapping at the end |
| Left, Up | Focus the previous, wrapping |
| Home, End | Focus the first, the last (a `tab_list` only) |

A group is one stop for Tab, as the WAI-ARIA pattern for tab lists and radio groups has it. Tab enters it at the member that is selected (`a11y_selected`
or `a11y_checked` true), or at the first when none is, and the next Tab leaves it;
Shift+Tab comes back to the selected one. The arrows move inside it.

A disabled member is skipped. In a `radio_group` the one that gets focus is also pressed, as
the arrow keys select among built-in `RadioButton`s. In a `tab_list` it is not: Enter or
Space selects, and a tab that should select as focus arrives says `focus =>`. The arrows
draw the ring, like Tab. [Accessibility](accessibility.md) has the roles and what each
attribute tells a reader.

## Moving focus yourself

`autofocus: true` names the element focus goes to when its [overlay opens](layout.md#where-focus-goes);
on one that is not in an overlay it takes focus the first time it is drawn, if nothing has it. It
is `AutofocusNeedsFocus` on an element nothing can focus. `focused: v` goes both ways: a write of
`true` to `v` focuses the element if it can take focus and is drawn, `false` blurs it, and when
several are written in one frame the last wins.

## Disabled, focused, invalid

Any control can say `disabled: true`, and it stops taking input and shows its
`on disabled` style. `focused: someVar` binds keyboard focus to a `var`, so you can
read it. `invalid: true` turns on the `on invalid` style,
for a field whose value you have judged wrong. You decide when, with an expression:

```byld
View Main() {
    var email = ""

    Column #[bg: 0x14141C, p: 28, gap: 8, width: 360] {
        TextField #[bind: email, placeholder: "Email", bg: 0x1E1E2A, color: 0xFFFFFF,
                    radius: 8, p: (horizontal: 12), height: 40,
                    invalid: email != "" && email.len < 5]
        Button("Send") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8,
                         disabled: email.len < 5] => email = ""
    }
}
```
