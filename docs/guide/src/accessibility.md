# Accessibility

```byld
View Main() {
    var wifi = true
    var saved = 0

    Column #[bg: 0x14141C, p: 24, gap: 14, width: 400, grow: 1] {
        Text("Settings") #[color: 0xE6E1E5, size: 20]

        Row #[gap: 10] {
            Button("Save") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8,
                             p: (horizontal: 20)] => saved++
            Box #[a11y_label: "Close", width: 36, height: 36, bg: 0x2A2A36, radius: 8,
                  align: center, justify: center, tap => saved = 0] {
                Text("x") #[color: 0xFFFFFF, size: 16, a11y_hidden: true]
            }
        }

        Row #[gap: 12, align: center] {
            Toggle #[bind: wifi, width: 52, height: 30]
            Text("Wi-Fi") #[color: 0xCAC4D0, size: 16]
        }

        Text("{saved} saved") #[color: 0x938F99, size: 14]
    }
}
```

Byard draws every pixel itself, so the operating system sees one surface and nothing in
it. A screen reader says "window" and stops. To fix that, a Byard window can describe
itself: each control, label and image the user can read or operate becomes a node with a
role, a name, its bounds and its state, and the platform's accessibility API is handed
the tree. The reader's actions, such as press, focus, set a value, increment, decrement
and scroll into view, come back as the events a pointer or a keyboard would have sent. Your
handlers run as they do for a mouse.

Most of this you do not write. The sample above has a `Button` named "Save" by its own
text, a `Toggle` named "Wi-Fi" by the `Text` beside it, and one thing that needs help.

## Names

A node needs a name, because a reader says the name. Byard looks for one in this order:

1. `a11y_label`, if the element has one.
2. For a `TextField`, its `placeholder`.
3. The text inside the control, so `Button("Save")` is "Save".
4. For a `Toggle`, `Checkbox` or `RadioButton` with no text of its own, or a `Box` with the
   role `switch`, `checkbox` or `radio`, the `Text` that follows it in the same `Row`. This is the
   usual "control, then its label" layout.

The `Text` that named a control is not listed again, or a reader would say "Wi-Fi" twice.

A control with no text, such as an icon button, has no name unless you give it one:

```byld,fragment
var closed = false
Box #[a11y_label: "Close", width: 36, height: 36, bg: 0x2A2A36, radius: 8,
      align: center, justify: center, tap => closed = true] {
    Text("x") #[color: 0xFFFFFF, size: 16, a11y_hidden: true]
}
```

`a11y_label` is a string expression, so `a11y_label: "{count} unread"` is read out as the
count changes. It is allowed on any element. A `Slider` has no text of its own, so it is
the usual place for one.

## Hiding decoration

`a11y_hidden: true` takes an element and everything inside it out of the tree. Use it for
what a reader gains nothing from: a hero picture, a divider, or an icon inside a button
that is already named.

```byld,fragment
Image("hero.png") #[width: 352, height: 48, a11y_hidden: true]
```

`byard check` accepts both attributes on every element, and checks that `a11y_label` is a
string and `a11y_hidden` a bool.

## Controls you build from a Box

A `Switch` or a `Tab` that a package draws from a `Box` has no role of its own. Left alone,
a reader hears "button", with no state, for anything with a `tap`. Eight more attributes, on
any element, say what the control is:

| Attribute | Type | What a reader is told |
|-----------|------|-----------------------|
| `a11y_role` | a keyword, below | What it is |
| `a11y_value` | `Str` | Its value as text, such as `"40 percent"` |
| `a11y_description` | `Str` | What it says after the name: a field's supporting or error text |
| `a11y_checked` | `Bool` | On or off, for a `switch`, `checkbox`, `radio` and `toggle_button` |
| `a11y_mixed` | `Bool` | Neither: the mixed state of a checkbox. Wins over `a11y_checked` |
| `a11y_selected` | `Bool` | Selected, for a `tab` or a `list_item` |
| `a11y_expanded` | `Bool` | Expanded when true, collapsed when false. Leave it out if it does not expand |
| `a11y_disabled` | `Bool` | Disabled, for a reader only. `disabled:` does this and stops input too |

`a11y_description` is read on an element that is in the tree: one with a role, a built-in
control or a `Text`. A `Box`, `Row` or `Column` with no role and no `tap` is not, so a
`a11y_description` or `a11y_label` on one is an error that says to give it an `a11y_role`. `byard shot --a11y` prints it after
the name:

```text
TextInput "Email" description "Enter a valid address" = "ada@" [focusable]
```

`a11y_checked` is a `Bool` and the mixed state its own attribute, so the common case is
`a11y_checked: on`, an expression you already have, and the rare one is a second line.

```byld
View Main() {
    var wifi = true
    var agree = false

    Column #[p: 24, gap: 14] {
        Row #[gap: 12, align: center] {
            Box #[a11y_role: switch, a11y_checked: wifi, width: 52, height: 32, radius: 16,
                  bg: (wifi ? 0x6750A4 : 0x49454F), tap => wifi = !wifi]
            Text("Wi-Fi") #[color: 0xCAC4D0, size: 16]
        }
        Box #[a11y_role: checkbox, a11y_checked: agree, width: 180, height: 28,
              tap => agree = !agree] {
            Text("I agree") #[color: 0xCAC4D0, size: 16]
        }
        Text("Settings") #[a11y_role: heading, color: 0xE6E1E5, size: 20]
    }
}
```

```text
Switch "Wi-Fi" = on [focusable] @ 24,24 52x32
CheckBox "I agree" [focusable] @ 24,70 180x28
Heading "Settings" @ 24,112 392x24
```

The switch is named by the `Text` beside it, as a built-in `Toggle` is. A `button`,
`toggle_button`, `switch`, `checkbox`, `radio`, `tab`, `menu_item` or `link` is named by the
`Text` inside it, like a `Button`. The role is a word, not a string, and the set is closed:

| Role | A reader says |
|------|---------------|
| `button`, `toggle_button` | Button; a toggle button is a button that is on or off |
| `switch`, `checkbox`, `radio`, `radio_group` | Switch, check box, radio button, radio group |
| `slider`, `progress_bar` | Slider, progress indicator |
| `tab`, `tab_list` | Tab, tab list |
| `menu_item`, `menu` | Menu item, menu |
| `link` | Link |
| `text_field` | Text input (names it only: a composed field has no caret or text editing) |
| `list`, `list_item` | List, list item |
| `dialog`, `alert` | Dialog, alert |
| `navigation`, `toolbar` | Navigation, toolbar |
| `group`, `image`, `heading` | Group, image, heading |

A misspelt one is an error that names the closest, so it cannot be a silent no-op:

```byld,fails
View Main() {
    Box #[a11y_role: chekbox, width: 24, height: 24] {}
}
```

```diagnostic
error[AttributeTypeMismatch]: expected one of ["button", "toggle_button", "switch", "checkbox", "radio", "radio_group", "slider", "tab", "tab_list", "menu_item", "menu", "link", "text_field", "progress_bar", "list", "list_item", "dialog", "alert", "navigation", "toolbar", "group", "image", "heading"] (did you mean `checkbox`?)
```

The role can come from a value, so a control that is a toggle button or a plain button
is one element: write a conditional of the words, or a `Str` that holds one.

```byld
View Main() {
    var starred = false
    Box #[a11y_role: starred ? toggle_button : button, a11y_checked: starred,
          width: 40, height: 40, bg: 0x6750A4, tap => starred = !starred]
}
```

A word that nothing in the view binds is still checked against the set, and text that comes to
something outside it is an error at the element, naming the closest role. Roles are chosen when
the element is drawn, so a control whose role changes becomes the other kind to a reader at once.
A layout container that is given one stops being flattened away: it is a node, and
what it holds hangs under it. For the keys such a control answers to, and the ring it
draws, see [the keyboard](controls.md#the-keyboard).

## Announcements and ranges

`a11y_live: polite` on an element makes a reader say its text when it appears or changes,
after what it is saying now; `assertive` interrupts, `off` is the default. It is how a
snackbar is heard without taking focus. `a11y_described_by: tip` points the element at the
one tagged `as tip` in the same view, which a reader says after the name; a name nothing
is tagged with is `UnknownStateTarget`. `a11y_value_min`, `a11y_value_max` and
`a11y_value_step` give a `slider` or a `progress_bar` built from a `Box` its range, next to
`a11y_value`; on any other role they are `AttributeNeedsRole`. The dump writes them as
`live polite`, `described_by "…"` and `= 40 in 0..100 step 5`. A leaving element is not in
the tree.

## What is in the tree

The tree is the controls and not the layout around them. `Row`, `Column`, `Box`, `Grid`
and `ZStack` have no role of their own, so they are left out, and what they hold is
attached to the nearest thing that has one. A reader does not walk through fifty nested
rows.

| Element | Role |
|---------|------|
| `Button` | Button |
| An element with a `tap` handler | Button |
| An element with `a11y_role` | That role, [as below](#controls-you-build-from-a-box) |
| `TextField` | TextInput, with its text |
| `Toggle` | Switch, on or off |
| `Checkbox` | CheckBox, checked or not |
| `RadioButton` | RadioButton, checked or not |
| `Slider` | Slider, with its value, range and step |
| `Text` | Label |
| `Image`, `VectorIcon` | Image |
| `ScrollView` | ScrollView |
| A `NavHost` and its tabs | TabList and Tab |
| A modal `Overlay` | Dialog, unless what it holds is itself a `dialog` or an `alert`: that one carries the modal flag and the overlay adds no node |

A `Canvas` is in the tree only if it has an `a11y_label`. What a `ScrollView` has scrolled
out of sight is not in the tree either. A `Slider` also writes its `var` when a reader
sets it, not only when the pointer drags it.

A native view, one you draw from Rust, is a plain group unless it says what it is:
`#[byard::native_view(role = "image")]`. See [native views](rust-native-views.md).

## Looking at the tree without a screen reader

`byard shot --a11y` writes the tree as text, one node a line, with no window, no GPU
and no screen reader:

```sh
byard shot --a11y tree.txt
```

For the sample at the top of this page, it writes:

```text
Label "Settings" @ 24,24 352x24
Button "Save" @ 24,62 71x40
Button "Close" @ 105,62 36x36
Switch "Wi-Fi" = on @ 24,116 52x30
Label "0 saved" @ 24,160 352x17
```

Each line has the role, the name, the value or state, and where it is, as `x,y` and
`width x height` in logical pixels. A control a reader could not name is marked
`(unnamed)`:

```text
Button (unnamed) @ 170,124 36x36
```

That line is where an `a11y_label` is missing, and a reader would say only "button".
Look for it before you ship. With `--a11y`, no picture is drawn unless you also say `-o`.

The `accessibility` example has one of each control, a deliberately unnamed button, and its
dump checked in beside it. See
[`examples/accessibility`](https://github.com/Briany4717/byard/tree/main/crates/byard-cli/examples/accessibility).

## Trying it with a screen reader

Building the tree and handing it to the platform are two steps. The tree, the attributes
and `byard shot --a11y` are in every build. Handing the tree to a screen reader is opt-in,
through the `a11y` cargo feature, because it pulls in AccessKit and its platform adapters and
adds about a fifth to a stripped release binary. An app that does not ask for it carries
none of that.

To try an example under a reader, run the dev window with the feature:

```sh
cargo run -p byard-cli --features a11y -- dev crates/byard-cli/examples/accessibility
```

Run it under Orca on Linux, NVDA or Narrator on Windows, or VoiceOver on macOS, and move
through it with the reader's own commands.

For your own app, turn the feature on in its `Cargo.toml`:

```toml
[dependencies]
byard = { version = "0.1", features = ["a11y"] }
```

The tree is built only while a screen reader is connected. With none, an app pays one
atomic load a frame and nothing else.

One limit to know. The names Byard works out are a guess from the layout. They are wrong
for some layouts, and `a11y_label` is the fix. Every control, a `Button`, `Toggle` and
`Slider` among them, is in the Tab order and is marked `[focusable]` in the dump.
