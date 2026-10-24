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

Each also accepts `value:` to show a value without writing it back, and a `change`
handler to run an action when the user changes it. `bind` is what you want almost
always.

## Button

```byld,fragment
var count = 0
Button("Tap me") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8] => count++
```

A `Button` takes its label as content. With no `p`, it pads the label by 10 above and
below and 24 left and right, and it is at least 40 pixels tall, so a bare button is
already something a finger can hit. Say `p`, `width` or `height` to change that. It
takes every layout, decoration and text attribute. It is focusable, so Tab reaches it.

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
[events](events-and-actions.md#handlers-that-take-a-value).

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
