# Events and actions

```byld
View Main() {
    var count = 0
    var note = "nothing yet"

    Column #[bg: 0x14141C, p: 24, gap: 12, grow: 1] {
        Text("{count} taps, last was {note}") #[color: 0xFFFFFF, size: 18]

        Button("Add") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8] => count++

        Box #[bg: 0x1E1E2A, radius: 12, p: 16,
              tap => note = "a tap",
              double_tap => note = "a double tap",
              long_press => note = "a long press"] {
            Text("Tap, double tap or hold me") #[color: 0xFFFFFF]
        }

        Button("Reset") => {
            count = 0
            note = "a reset"
        }
    }
}
```

An *event* is something the user does to an element. An *action* is what you run in
answer. You attach one to the other with `=>`.

## Writing a handler

There are two spellings, and they mean the same:

```byld,fragment
var count = 0
Button("Add") => count++
Button("Add") #[tap => count++]
```

The first, `=>` after the element, is a shorthand for `tap`. The second names any
event inside the attributes. Use the shorthand for the one thing a button does, and
the named form for everything else, or when an element has more than one handler.

An action is one statement, or several in braces. It runs when the event fires, on
the logic thread, and everything it changes shows on the next frame.

## What an action can do

- **Assign**: `x = 1`, `x += 2`, `x -= 2`, and the shorthands `x++` and `x--`.
- **Write a list or record**: `todos = todos.push(item)`. See
  [expressions](expressions.md).
- **Run a named action**: `add(2)`, where `let add = (by: Int) => { … }`.
- **Branch** with `if`, `else if` and `else`.
- **Name a value** with `let`, which holds for the statements after it.
- **Call a controller or the network**, and handle the answer. See
  [data and I/O](data-and-io.md).
- **Navigate**: `back(path)`, `replace(path, "/x")`, or write a path variable. See
  [navigation](navigation.md).

```byld
View Main() {
    var count = 0
    var message = ""

    Column #[bg: 0x14141C, p: 24, gap: 12] {
        Text("{count} {message}") #[color: 0xFFFFFF, size: 18]
        Button("Add, up to 5") => {
            let next = count + 1
            if next > 5 {
                message = "that is the limit"
            } else {
                count = next
                message = ""
            }
        }
    }
}
```

`if` works in any action: a button's, a lifecycle one, a reply arm. It needs braces
around each branch. To do something for each item, write the loop in the
view (`for`) and put the action on the element it makes, or change the list with
`map` and `filter`.

## The events

Every element except `Spacer` takes the pointer events:

| Event | Fires |
|-------|-------|
| `tap` (also `click`) | A press and release in the same place, quickly |
| `double_tap` | Two taps close together |
| `long_press` | A press held without moving |
| `secondary` | A right click, or a two-finger tap |
| `pointer_down`, `pointer_up` | The raw press and release |
| `pointer_move` | The pointer moves over the element |
| `pointer_enter`, `pointer_leave` (or `pointer_exit`), `hover` | The pointer comes, goes, is over |
| `focus_in`, `focus_out` | Focus arrives in the element or anything inside it, and leaves all of it |
| `wheel` | A mouse wheel or trackpad notch |
| `drag_start`, `drag`, `drag_end` | A held press moves past a few pixels; moves on; is released |
| `focus`, `blur` | The element gains or loses keyboard focus |

`pointer_enter`, `pointer_leave`, `focus_in` and `focus_out` are about the element and
everything inside it, once per crossing and not for a move between its children, so a
wrapper around a slot hears them for whatever the slot holds. They do not make an element
focusable. `interactive: false` lets the pointer through an element.

Some elements add their own:

| Element | Events |
|---------|--------|
| `Button`, `TextField`, `Toggle`, `Slider`, `Checkbox`, `RadioButton`, and any element the keyboard can reach | `key_down`, `key_up` |
| `TextField` | `input`, `change`, `submit` |
| `Toggle`, `Slider`, `Checkbox`, `RadioButton` | `change` |
| `ScrollView` | `scroll`, `end_reached`, `page_change`, `scroll_end`, `refresh` |
| An overlay or its anchored child | `dismiss` |
| `NavStack` | `route_change` |

An event the element does not have is an error, with the nearest name suggested. A
`Box` takes `key_down` and `key_up` when the keyboard can reach it: it has a `focus` or
`blur` handler or a `focused:`, or a `tap` and an `a11y_role` that takes focus (`button`,
`switch`, `checkbox`, `radio`, `tab`, `menu_item`, `link`). On one it cannot reach, the error
says so and says how to fix it. Space and Enter press such a control as its role says and
do not reach `key_down`; every other key does.

Three things are worth knowing:

- **The topmost element that listens gets the event.** A press does not bubble to
  the elements under it, so a `tap` on a panel does not run when a field inside it
  is pressed. To close a menu on a click outside it, use `dismiss`.
- **Small targets are still tappable.** Anything with a handler gets a hit area of
  at least 44 by 44 pixels, so a small icon is easy to hit.
- **A double tap is only a double tap where someone listens.** Without a
  `double_tap` handler under the pointer, two quick taps are two taps.

### Where the pointer is, and dragging

A pointer handler can name an event value and read where the pointer is, in the element's
own space, from its top-left corner:

```byld
View Main() {
    var value = 0.3

    Box #[
        a11y_role: slider, a11y_label: "Level", width: 240, height: 40, bg: 0x2A2A36,
        pointer_down(e) => value = (e.x / e.width).max(0).min(1),
        drag(e) => value = (e.x / e.width).max(0).min(1),
    ] {
        Box #[width: 240 * value, height: 40, bg: 0x6750A4]
    }
}
```

`pointer_down`, `pointer_up`, `pointer_move`, `pointer_enter`, `pointer_exit` and `hover`
give `e.x`, `e.y`, `e.width` and `e.height`. `drag_start` fires once when a held press
has moved more than a few pixels, `drag` on every move after, and `drag_end` on the
release; they add `e.dx` and `e.dy`, how far the pointer has travelled since the press, and
set the `dragging` state. The element the press began on gets all three wherever the pointer
goes, so `e.x` can be negative or past `e.width`: clamp it. A press that does not move
past the slop is a tap and fires none of them.

Any other field, `e.z` or `e.dx` on a `pointer_down`, is an error with the nearest name, and
so is naming a value on an event that has none: `tap` can come from a key, so `tap(e)` is an
error that points to `pointer_down(e)`. A `pointer_down`, `key_down` or `change` written on a
built-in `Slider` runs before the slider's own handling.

### Handlers that take a value

A handler can name what it is told. On a `TextField`, `input` and `change` run after
each edit and see the text as it is now:

```byld
View Main() {
    var query = ""
    var searches = 0

    Column #[bg: 0x14141C, p: 24, gap: 12, grow: 1] {
        TextField #[bind: query, placeholder: "Search",
                    input(text) => searches++, submit => query = ""]
        Text("{searches} edits so far, typed \"{query}\"") #[color: 0xFFFFFF]
    }
}
```

`input(text) => …` names the value `text` for that handler. `submit` runs on Enter.
The edits of a composition, such as an input method building a character, count
once the character is committed.

## Two-way attributes

Controls whose value the user changes take `bind:`, which is the short way to say
"show this `var` and write it when the user changes it":

```byld,fragment
var name = ""
TextField #[bind: name]
```

[Controls](controls.md) covers them. `offset:` on a `ScrollView` and `path:` on a
navigation stack work the same way.

## Events of a view

Views have a life: they appear and they go. Four lines tie an action to it.

```byld
View Main() {
    var shown = true
    var city = "Tokyo"

    Column #[bg: 0x14141C, p: 24, gap: 12, grow: 1] {
        Row #[gap: 8] {
            Button("Tokyo") => city = "Tokyo"
            Button("Lima") => city = "Lima"
            Button(shown ? "Hide" : "Show") => shown = !shown
        }
        when shown {
            Loader(place: city)
        }
    }
}

View Loader(place: Str) {
    var loaded = ""
    var loads = 0
    let load = () => {
        loaded = place
        loads++
    }

    on mount => load()
    on change(place) => load()

    Text("{loaded}: loaded {loads} times since it appeared") #[color: 0xFFFFFF]
}
```

| Write | Runs |
|-------|------|
| `on mount => …` | Once, when the view appears |
| `on unmount => …` | Once, when it goes |
| `on change(a, b) => …` | When `a` or `b` changes. Not on mount |
| `every 5min => …` | Repeatedly, while the view is up |
| `after 2s => …` | Once, a while after it appears |

`on change` runs once per change. It does not run when a value is written to what it
already was, and not while the view is off screen. Coming back on screen is a mount,
not a change. In the sample, `Loader` loads once when it mounts and again each time
`place` changes, and pressing the city already selected does nothing.

These lines can sit inside a `when` too. They are tied to whatever they are
written in: a timer inside a `when` stops when the `when` turns
false and starts fresh when it comes back. That is the point of writing them in the
tree, and why there is nothing to remember to stop.

`on measure => …` runs when the element it is in has been laid out. See
[layout](layout.md#knowing-your-own-size).
