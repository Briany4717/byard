# The dev loop

Four commands carry most of the work: `byard dev` to see the app, `byard check`
to find mistakes, `byard shot` to take a picture of it and `byard fmt` to lay the files out.

```sh
byard dev      # the window, reloading as you save
byard check    # every file, no window, exit code for CI
byard shot     # a PNG, no window
byard fmt      # one layout for every .byd file
```

All four read the project they are run in. They look for `byard.toml` in the
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
| `--key <KEY>` | none | Press a key before the picture, such as `Left`, `shift+Left` or `mod+a`. Repeatable |
| `--type <TEXT>` | none | Type text before the picture. Repeatable |
| `--a11y <FILE>` | none | Write the accessibility tree as text. See [accessibility](accessibility.md) |

Taps are how you reach a screen that needs a press. This opens the second tab of
an app, at logical point 120, 840, and draws it dark and wide:

```sh
byard shot --size 1000x700 --dark --tap 120,840 -o second-tab.png
```

### Taps, keys and typing

`--tap`, `--key` and `--type` run in the order you write them on the command line, so a
picture can be taken after a whole interaction. This taps into a field, types a word with
a typo in it, steps back three characters and selects two, so the picture shows a
selection in a text field:

```sh
byard shot crates/byard-cli/examples/text_editing \
  --tap 200,120 --type "hello wrold" --key Left --key Left --key Left \
  --key shift+Left --key shift+Left -o editing.png
```

A key is a single character or one of `Left`, `Right`, `Up`, `Down`, `Home`, `End`,
`Backspace`, `Delete`, `Enter`, `Tab`, `Escape` and `Space`, in any case. In front of it go
modifiers, joined by `+`: `shift`, `ctrl`, `alt` (or `option`), `meta` (or `cmd`) and `mod`.
`mod` is Command on macOS and Control elsewhere, as for the keys in
[controls](controls.md#editing). Typed text goes in as it would from a keyboard, as one
burst, so undo takes it back in one step. The clipboard in a shot is private to it, so a
paste never reads yours.

### The accessibility tree

`--a11y tree.txt` writes what a screen reader would be told, one node a line, instead of
the picture. It needs no screen reader and no GPU, and marks every control with no name as
`(unnamed)`. With it, a PNG is drawn only if you also pass `-o`. See
[accessibility](accessibility.md).

Because it runs the app for real, a screen that fetches data draws the data, as
long as the network is there. `shot` cannot draw an app with a Rust half,
because it knows nothing of your controllers or native views. An app can draw
itself the same way from Rust. See [the Rust half](rust-app.md).

## `byard fmt`

```sh
byard fmt
```

`fmt` rewrites `.byd` files with one layout. It has no options to learn and nothing to
configure, so every file in a project, and every contributor's editor, agrees. Given
no path it formats the project it is in: the entry directory and the packages it takes by
path. Given a `.byd` file it formats that file, and given a directory with no `byard.toml`
in it, every `.byd` under it.

```text
ok   14 files, 3 changed                                                  12ms
```

The layout is this:

- four spaces of indentation, one level for each open `{`, `(` or `[`,
- one space around operators, `=>` and `=`, and after `,` and `:`,
- an `#[ … ]` list on one line when it fits in 100 columns, and otherwise one attribute a
  line with a comma after each,
- at most one blank line in a row,
- comments where they were, and strings copied byte for byte.

Before a file is written, `fmt` reads its own output and compares it with the input. A
file that would change meaning, or that does not parse, is reported and left alone, and
the others are still formatted.

For CI, `byard fmt --check` changes nothing. It lists the files that are not formatted,
one a line, and exits 1 if there are any:

```text
src/main.byd
error: 1 of 14 files not formatted
```

### In your editor

The language server, `byld-lsp`, formats with the same code, so format on save in an
editor gives what `byard fmt` gives. It ignores the editor's tab size. Build it with
`cargo build --release -p byld-lsp` and point your editor's `.byd` language server at the
binary. It speaks the Language Server Protocol over standard input and output, and besides
formatting it offers completion, go to definition, rename, document symbols, semantic
colours and quick fixes. It is early, and it will change.

## `byard clean`

`byard clean` removes `.byard/cache` and `.byard/generated`. It is safe, and it
leaves package checkouts alone because they are immutable and slow to fetch
again.
