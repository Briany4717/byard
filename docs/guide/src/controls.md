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
need a `var`: given a `let` or a literal, a control draws its default instead of
that value. A `change` handler runs an action when the user changes it.

## Button

```byld,fragment
var count = 0
Button("Tap me") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8] => count++
```

A `Button` takes its label as content. With no `p`, it pads the label by 10 above and
below and 24 left and right, and it is at least 40 pixels tall, so a bare button is
already something a finger can hit. Say `p`, `width` or `height` to change that. It
takes every layout, decoration and text attribute. It is not in the Tab order yet.

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
fainter. `p` sets where the text starts, and a `typo` gives its size. In a `Row`,
`grow: 1` makes it fill the rest. Without it, a field keeps its width.

Its events are `input` and `change`, which run after each edit, and `submit`, which
runs on Enter. Each can name the text, as `input(text) => …`. See
[events](events-and-actions.md#handlers-that-take-a-value). An edit that changes nothing,
such as Backspace at the start, runs nothing.

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
A paste is one line: its line breaks become spaces. The clipboard is the system's.

Typing is undone a word at a time, and a paste, a cut or a deleted selection is a step of
its own. When the `var` is written from somewhere else, such as a button's action or a
reply from a server, the caret goes to the end of the new text and the undo history starts
again. A text wider than the field scrolls to keep the caret in view. The caret blinks
while the field and the window have focus.

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
and the `on checked` style state is active when it is on.

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
a good way to try out a number: bind one to anything and drag it.

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
