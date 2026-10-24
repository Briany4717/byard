# The dev loop

Three commands carry most of the work: `byard dev` to see the app, `byard check`
to find mistakes, and `byard shot` to take a picture of it.

```sh
byard dev      # the window, reloading as you save
byard check    # every file, no window, exit code for CI
byard shot     # a PNG, no window
```

All three read the project they are run in. They look for `byard.toml` in the
current directory and then each parent, so you can run them from anywhere inside
a project. They also take a path: a project directory, a `byard.toml`, or a
single `.byd` file.

## `byard dev`

```sh
byard dev
```

It opens a window on your app and watches the project. Save a `.byd` file and
the window updates. Most edits keep your state: you are still on the same screen,
in the same tab, with the same text typed into the same field.

The exception is an edit to a view's *declarations*. Adding, removing, reordering
or retyping one of its parameters, `var`s, `let`s or `inject`s rebuilds that view,
and its state starts again. Changing what the view shows, its elements, texts,
styles and expressions, never does. Add a `var` and you will see that view reset
once. That is the cost of a changed shape, and the other views are untouched.

What it watches:

- every `.byd` file in the project,
- `byard.toml`, so a changed theme or breakpoint applies on save,
- images the app draws, so replacing a PNG updates every `Image` that shows it,
- the packages you depend on by path, so editing a package next door reloads
  too. Packages fetched from git or a registry are pinned and not watched.

### When you break something

Delete a closing brace and save. The window does not close and does not go
blank. The last good picture stays, blurred and darkened, and a panel in front
of it lists the errors. Fix the file, save again, and the app comes back with its
state intact.

A rebuilding edit, one that changes a view's declarations, cannot be applied
while you hold a press, because the element under your finger would be replaced.
It waits for you to let go, and the status line shows `↻ pending` while it does.

Not every mistake is shown in the window. `byard check`, below, runs more checks
than a reload does, so make it a habit before you commit.

### The status line and the HUD

While the app runs, `byard dev` keeps one line anchored in your terminal
with how long recent frames took, how many boxes are on screen and how many
reloads have happened. It is there to answer "is my change slow?" while you are
making it. Two chords change what you see in the window:

| Chord | What it does |
|-------|--------------|
| `Mod+Shift+D` | Shows or hides an in-window heads-up display |
| `Mod+Shift+P` | Switches the status line to an expanded per-scope profile |

`Mod` is Command on macOS and Control elsewhere.

### Options

| Flag | What it does |
|------|--------------|
| `--deep-link <URL>` | Delivers a URL at startup, as the operating system would for a link into your app: `--deep-link byard://detail/7`. See [navigation](navigation.md). |
| `--profile` | Starts with the expanded profile block instead of the one-line status. |
| `--trace <PATH>` | Writes a Chrome Trace Event file of every frame. Open it in Perfetto, `chrome://tracing` or speedscope. |

An optional `[dev]` table in `byard.toml` sets the same things for the whole
team. All three keys are optional, and an unknown key is an error.

```toml
[project]
name  = "hello"
entry = "main.byd"

[dev]
frame_budget = "8ms"   # what the bars and sparkline are drawn against
hud = true             # start with the HUD showing
statusline = false     # no anchored status line, for a terminal that draws it badly
```

Without `frame_budget`, the budget is the refresh interval of your display, which
is the right answer: on a 120 Hz screen a frame that takes 10 ms drops frames. A
shipped app has none of this. `byard dev` is a development tool, and it pays for
the watching and the reporting so your app does not have to.

## `byard check`

```sh
byard check
```

`check` reads the whole project, entry file, siblings and packages, and reports
every problem it can find without opening a window. It parses, resolves every
name, checks types and attributes, and lowers every view the way the app would.
It is what CI runs.

```text
→    checking /home/ada/hello/main.byd
ok   0 errors                                                              101ms
```

Make a typo, and it says where:

```text
main.byd:11:62: error[UnknownName]: nothing here is named `cuont` (did you mean `count`?)
   |
11 |                            color: 0xFFFFFF, weight: bold] => cuont++
   |                                                              ^^^^^
err  1 error
```

The first line is `file:line:column: error[Kind]: message`. It is plain text
with no colour, so an editor's problem matcher can read it. The block under it
points at the code. Pass `--short` to drop the block and get one line per
problem, for scripts.

The exit code is 0 when there are no errors and 1 otherwise. A *warning* is
printed in full and counted separately (`0 errors, 1 warning`), and does not
fail the check. Warnings are for things that are harmless but probably not what
you meant.

Here are two things it catches. Each of them checks clean in many frameworks
and then draws nothing. The first is a misspelt name:

```byld,fails
View Main() {
    var count = 0
    Column #[gap: 8] {
        Text("{cuont} taps")
        Button("+") => count++
    }
}
```

```diagnostic
error[UnknownName]: nothing here is named `cuont` (did you mean `count`?)
```

The second is an attribute the element does not take:

```byld,fails
View Main() {
    Column #[colour: 0xFF0000] {
        Text("red?")
    }
}
```

```diagnostic
error[UnknownAttribute]: unknown attribute `colour`
```

Other mistakes of this kind are a method that does not exist (`xs.pusj(1)`),
text where a number is wanted, and a view called with the wrong arguments. The
closest name is suggested when there is one.

`check` works in two stages. It reports problems with parsing, names and types
first. Once those are fixed it goes on to the checks that need every view
lowered, such as which attributes an element takes, so fixing one batch can
reveal the next. Run it again until it says `0 errors`.

`check` also works inside a library package, a directory with a `[package]`
table and no `[project]`. It checks the package's own files as an app that uses
it would see them. [Packages](packages.md) covers those.

## `byard shot`

```sh
byard shot -o home.png
```

`shot` draws the project to a PNG, headlessly. No window appears, so nothing
else on your screen can cover it and no stray click can change it. It runs the
same program `byard dev` runs, through the same renderer, so the picture is what
you would see.

It exists because looking at the result is the one check that catches a wrong
picture. Every test of a screen can pass while the screen is wrong.

| Flag | Default | What it does |
|------|---------|--------------|
| `-o`, `--out <PNG>` | `shot.png` | Where to write the picture |
| `--size <WIDTHxHEIGHT>` | `440x880` | The logical size of the window |
| `--scale <SCALE>` | `2` | Device pixels per logical pixel |
| `--at <MS>` | `1500` | Milliseconds of app time to run first, so animations settle |
| `--dark` / `--light` | the manifest's | Force a colour scheme |
| `--tap <X,Y>` | none | Tap a logical point before the picture. Repeat it for more taps |

Taps are how you reach a screen that needs a press. This opens the second tab of
an app, at logical point 120, 840, and draws it dark and wide:

```sh
byard shot --size 1000x700 --dark --tap 120,840 -o second-tab.png
```

Because it runs the app for real, a screen that fetches data draws the data, as
long as the network is there. `shot` cannot draw an app with a Rust half,
because it knows nothing of your controllers or native views. An app can draw
itself the same way from Rust. See [the Rust half](rust-app.md).

## `byard clean`

`byard clean` removes `.byard/cache` and `.byard/generated`. It is safe, and it
leaves package checkouts alone because they are immutable and slow to fetch
again.
