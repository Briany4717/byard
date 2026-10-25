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
4. For a `Toggle`, `Checkbox` or `RadioButton` with no text of its own, the `Text` that
   follows it in the same `Row`. This is the usual "control, then its label" layout.

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

## What is in the tree

The tree is the controls and not the layout around them. `Row`, `Column`, `Box`, `Grid`
and `ZStack` have no role of their own, so they are left out, and what they hold is
attached to the nearest thing that has one. A reader does not walk through fifty nested
rows.

| Element | Role |
|---------|------|
| `Button` | Button |
| An element with a `tap` handler | Button |
| `TextField` | TextInput, with its text |
| `Toggle` | Switch, on or off |
| `Checkbox` | CheckBox, checked or not |
| `RadioButton` | RadioButton, checked or not |
| `Slider` | Slider, with its value, range and step |
| `Text` | Label |
| `Image`, `VectorIcon` | Image |
| `ScrollView` | ScrollView |
| A `NavHost` and its tabs | TabList and Tab |
| A modal `Overlay` | Dialog |

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
byard = { path = "../byard/crates/byard", features = ["a11y"] }
```

The tree is built only while a screen reader is connected. With none, an app pays one
atomic load a frame and nothing else.

Two limits to know. A `Button`, `Toggle` and `Slider` are not in the Tab order yet, so a
reader reaches them through its own navigation, not through Tab. And the names Byard works
out are a guess from the layout. They are wrong for some layouts, and `a11y_label` is the
fix.
