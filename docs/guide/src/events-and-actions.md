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
| `pointer_enter`, `pointer_exit`, `hover` | The pointer comes, goes, is over |
| `wheel` | A mouse wheel or trackpad notch |
| `focus`, `blur` | The element gains or loses keyboard focus |

Some elements add their own:

| Element | Events |
|---------|--------|
| `Button`, `TextField`, `Toggle`, `Slider`, `Checkbox`, `RadioButton` | `key_down`, `key_up` |
| `TextField` | `input`, `change`, `submit` |
| `Toggle`, `Slider`, `Checkbox`, `RadioButton` | `change` |
| `ScrollView` | `scroll`, `end_reached`, `page_change`, `scroll_end`, `refresh` |
| An overlay or its anchored child | `dismiss` |
| `NavStack` | `route_change` |

An event the element does not have is an error, with the nearest name suggested. A
`Box` takes no `key_down`, because it is not something the keyboard can reach. Give
it a `focus` handler and it becomes focusable.

Three things are worth knowing:

- **The topmost element that listens gets the event.** A press does not bubble to
  the elements under it. To close a menu on a click outside it, use `dismiss`.
- **Small targets are still tappable.** Anything with a handler gets a hit area of
  at least 44 by 44 pixels, so a small icon is easy to hit.
- **A double tap is only a double tap where someone listens.** Without a
  `double_tap` handler under the pointer, two quick taps are two taps.

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
