# Several windows

A program draws into one window unless it says otherwise. A `Window` element says
otherwise: the program then has the windows it writes, all of them sharing its state.

```byld
View Main() {
    var count = 0
    var inspecting = true

    Window #[title: "Counter", width: 360, height: 240] {
        Column #[p: 24, gap: 16, grow: 1] {
            Text("{count}") #[size: 48]
            Button("Add one") => count += 1
            Button("Inspector") => inspecting = !inspecting
        }
    }

    when inspecting {
        Window #[title: "Inspector", width: 300, height: 160, close => inspecting = false] {
            Column #[p: 24, gap: 8, grow: 1] {
                Text("The counter reads {count}")
                Text(count % 2 == 0 ? "That is even." : "That is odd.")
            }
        }
    }
}
```

The first window is always open. The second is open while `inspecting` is true, and it
closes when that becomes false. A press in one window writes `count`, and the other
shows it in the same tick: there is one program and one state, not one per window.

A program with no `Window` is a single window, as before: the entry view is its
content, and the project's title and size are its own. Nothing in this chapter costs
such a program anything.

## What a window takes

| Attribute | Type | Default | |
|-----------|------|---------|-|
| `title` | `Str` | the project's name | The title bar and the task list. Reactive. |
| `width`, `height` | `Float` | 1280 × 720 | The size it opens at, in logical pixels. |
| `min_width`, `min_height`, `max_width`, `max_height` | `Float` | none | The bounds a user can resize it within. Reactive. |
| `resizable` | `Bool` | `true` | Reactive. |
| `decorations` | `Bool` | `true` | The platform's title bar and borders. Reactive. |
| `transparent` | `Bool` | `false` | Undrawn pixels show what is behind the window. |
| `close =>` | action | ends the program | The close button, Alt+F4, Cmd+W. |

`width` and `height` are only the size the window opens at. After that the size is the
user's, and writing them again does not resize the window; the checker warns when they
read something that changes (`OpeningSizeIsStatic`). The window's content is laid out as
the one window's always was, on the theme's `surface`.

`Window ... as inspector` gives the window a name, which `byard shot --surface` finds
it by; without one, its title is the name.

## Closing

The platform's close request runs the window's `close =>`, which usually sets the
`when` that opened it to false. A window with no `close =>` ends the program when it is
closed, as the one window of a program does. The program also ends when its last window
closes.

A window whose `when` becomes false is closed, and one that opens again is a new window
showing what the `when` kept of its content: the `var`s inside it keep their values, as
they do for any element under a `when`.

## `window` is the window's own

Inside a window, `window.width` and `window.height` are that window's size, so
the same view used in two windows reads each one's own:

```byld
View Main() {
    Window #[title: "Narrow", width: 300, height: 200] {
        Page()
    }
    Window #[title: "Wide", width: 900, height: 200] {
        Page()
    }
}

View Page() {
    when window.width >= 600 {
        Text("wide layout")
    } else {
        Text("narrow layout")
    }
}
```

## A window for every item

A `for` can make a window per item. Give it a `key`, so that an item that is still in the
list keeps its window, and the state in it, when the list changes
([state](state.md#rows-that-keep-their-state-key)):

```byld
type Doc = { id: Int, name: Str }

View Main() {
    var docs: List<Doc> = [{ id: 1, name: "Notes" }, { id: 2, name: "Plan" }]
    for d in docs key d.id {
        Window #[title: d.name, width: 480, height: 320] {
            Text("Editing {d.name}") #[p: 24]
        }
    }
}
```

Reorder `docs` and no window closes or opens; remove an item and exactly its window
closes. A `for` of windows with no `key` is the warning `SurfaceForNeedsKey`, because a
change to its list would hand each window to another item.

## Where a window may be written

A `Window` goes at the top of the program: in the entry view's body, in the body of a
`when` or a `for` there, or at the root of a view that is called there. Anywhere else
is `MisplacedSurface`:

```byld,fails
View Main() {
    Window #[title: "Notes"] {
        Column {
            Window #[title: "Settings"] {
                Text("x")
            }
        }
    }
}
```

```diagnostic
error[MisplacedSurface]: a `Window` goes at the top of the program, not inside another element
```

A program that writes `Window` says where every window is, so everything else at its top
has to be one too. A `Text` between two windows would have no window to be drawn in:

```byld,fails
View Main() {
    Text("loose")
    Window #[title: "Notes"] {
        Text("x")
    }
}
```

```diagnostic
error[ContentOutsideSurface]: this `Text` is outside every window
```

State is not content: a `var`, a `let`, a `fn`, a `provide` or an `inject` at the top is
shared by every window, which is the point.

## Input and focus

Every pointer event goes to the window it happened in, and is hit-tested in that
window's own coordinates: a press is captured by its window until it is released, and
hover is a window's own. Keyboard focus is one for the whole program, wherever the
platform put it. Tab and Shift+Tab move within the window that has it, and never
cross to another, the way two apps' windows behave.

## Drawing and testing without a window

A window is drawn only when something in it changed, or it was resized. A program that is
still draws nothing, and one that changes a `var` only one window reads draws that window
alone.

`byard shot` draws one window at its own size. With several, name the one to draw
(the taps and keys of the shot are for it too), and without a name the command lists them:

```sh
byard shot crates/byard-cli/examples/two_windows --surface counter -o counter.png
byard shot crates/byard-cli/examples/two_windows --surface inspector -o inspector.png
```

`byard test` sees every window. Its accessibility dump starts each window with a
`Window "title"` line, with the window's tree indented below it, and a step that names an
element finds it on whichever window it is and taps it there:

```toml
[[test]]
name = "a press in one window changes the other"
steps = [{ tap = 'Button "Add one"' }]
expect = ['Window "Inspector"', 'Label "The counter reads 1"']
```

`byard dev` and a built app open each window on the desktop. They run on winit, one native
window each, from one event loop: a press in one window changes what another shows, closing a
window with a `close =>` runs it, and a window whose `when` goes false closes. While `byard dev`
runs, an edit keeps the windows that are still written open, where they were; the error
overlay, the HUD and the reload flash appear over the window that has the keyboard (the first
one, when none does). An edit that adds the program's first `Window`, or removes the last,
needs `byard dev` to be started again. A program with a `Panel` does not start yet.
