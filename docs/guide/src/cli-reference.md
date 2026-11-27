# CLI reference

```text
byard <COMMAND>
```

| Command | Does |
|---------|------|
| [`new`](#byard-new) | Scaffold a new project |
| [`dev`](#byard-dev) | Open the app in a window that reloads as you save |
| [`check`](#byard-check) | Check the project without a window |
| [`shot`](#byard-shot) | Draw the project to a PNG without a window |
| [`test`](#byard-test) | Run the project's scenarios in `tests/` without a window or a GPU |
| [`fmt`](#byard-fmt) | Format `.byd` files in place |
| [`fix`](#byard-fix) | Move a project to the next edition |
| [`build`](#byard-build) | Bake the vector icon atlas |
| [`bundle`](#byard-bundle) | Package a built app with its project |
| [`add`](#byard-add) | Add a dependency, then fetch and lock it |
| [`get`](#byard-get) | Fetch dependencies and write `byard.lock` |
| [`publish`](#byard-publish) | Publish a package to a registry |
| [`clean`](#byard-clean) | Remove generated files and caches |
| [`explain`](#byard-explain) | Say what an error means |
| `help` | Print help for the program or a command |

`byard --help` and `byard <command> --help` print the same descriptions as this page. `byard
--version` prints the version.

Commands that read a project find it the same way. With no path, they look for `byard.toml` in the
current directory, then in each parent. If there is none, they fall back to a `main.byd` in the current
directory. With a path, it may be a project directory, a `byard.toml`, or a single `.byd` file. A
single `.byd` file is checked or run alone: its siblings are ignored, and it has no `[dependencies]`.

A command that fails exits with status 1 and says why. `check` exits 1 when it finds an error.

## `byard new`

```text
byard new <NAME>
```

Scaffolds a project in a new directory called `NAME`: a `byard.toml`, a starter `main.byd` and a
`.gitignore`.

| Argument | |
|----------|-|
| `<NAME>` | The project's name, which is also the directory's. ASCII letters, digits, `-` and `_`, not starting with `.` or `-`. |

It refuses a directory that exists, and removes what it made if it fails part way.

## `byard dev`

```text
byard dev [OPTIONS] [FILE]
```

Opens the app in a window and reloads it as you save. Not available in a library package.

| Argument or flag | |
|------------------|-|
| `[FILE]` | The project or `.byd` file to run. Defaults to the entry in `byard.toml`. |
| `--deep-link <URL>` | Delivers a URL at startup, as an operating system intent would: `--deep-link byard://item/42`. |
| `--trace <PATH>` | Writes a Chrome Trace Event file of every frame. Open it in Perfetto, `chrome://tracing` or speedscope. |
| `--profile` | Starts with the expanded per-scope profile block instead of the one-line status. Toggle it at run time with `Mod+Shift+P`. |

While it runs, `Mod+Shift+D` shows or hides the in-window HUD. The `[dev]` table in `byard.toml` sets
`frame_budget`, `hud` and `statusline`. See [the dev loop](dev-loop.md).

## `byard check`

```text
byard check [OPTIONS] [FILE]
```

Parses and validates the project, its siblings and its packages, without opening a window. Fit for CI.
In a library package, it checks the package's own files.

| Argument or flag | |
|------------------|-|
| `[FILE]` | The project or `.byd` file to check. Defaults to the entry in `byard.toml`. |
| `--short` | One line per diagnostic, with no caret block beneath it, for scripts. |

Each diagnostic starts with `file:line:column: error[Kind]: message`, in plain text. A warning is
printed in full and counted apart, and does not fail the command. Exit status 0 is no errors.
The warnings about names and replies (`unused-variable`, `unused-parameter`, `unused-view`, `untyped-reply`) can be switched off
in `byard.toml` under `[lints]`, as `unused-variable = "allow"`; `"warn"`, the default, keeps one on.
The `Kind` in brackets is the error's name, which stays the same from release to release, and
`byard explain Kind` says what it means.

## `byard shot`

```text
byard shot [OPTIONS] [PATH]
```

Draws the project to a PNG, with no window. It runs the same check as `byard check` first: a program
with errors prints them the way `check` does and exits non-zero, and no PNG is written (a warning is
printed and the picture is drawn). It cannot draw an app with a Rust half. Not available in a
library package.

| Argument or flag | Default | |
|------------------|---------|-|
| `[PATH]` | the current project | The project directory, `byard.toml` or `.byd` file. A `.byd` path reads no manifest, so none of its packages. |
| `--view <NAME>` | the root, `Main` | Draws this view instead. An unknown name is an error listing the closest views; a view with a required parameter is an error naming the parameters. |
| `-o`, `--out <PNG>` | `shot.png` | Where to write the picture. |
| `--size <SIZE>` | `440x880` | Logical size, as `WIDTHxHEIGHT`. |
| `--scale <SCALE>` | `2` | Device pixels per logical pixel. |
| `--at <MS>` | `1500` | Milliseconds of app time to run first, so animations settle. |
| `--now <ISO>` | the real clock | Fixes the time `now()` reads, such as `2026-11-05T09:00:00`. `--at` advances it. |
| `--dark` | | Forces the dark scheme. Conflicts with `--light`. |
| `--light` | | Forces the light scheme. |
| `--tap <X,Y>` | | Taps a logical point before the picture. Repeat it to tap more than once. |
| `--hold <X,Y>` | | Presses at a logical point and keeps the pointer down for the picture, so a press, a morph and a ripple show. `--at` is how long it is held. Repeatable. |
| `--hover <X,Y>` | | Moves the pointer to a logical point without pressing, so `on hover` shows. Repeatable. |
| `--drag <X,Y:X,Y>` | | Drags from one logical point to another: a press, eight moves a frame apart, a release. Repeatable. |
| `--key <KEY>` | | Presses a key before the picture, such as `Left`, `shift+Left` or `mod+a`. Repeat it to press more. |
| `--type <TEXT>` | | Types text before the picture. Repeat it to type more. |
| `--wait <MS>` | | Lets this many milliseconds of app time pass before the next input or the picture. Repeatable. |
| `--screens <SCREENS>` | one `screen-1` of `--size` | The screens a program of windows and panels is drawn on, `NAME:WxH[@SCALE][+X+Y]` separated by commas, like `"DP-1:2560x1440@1.5, HDMI-A-1:1920x1080"`. `WxH` is the mode in device pixels, so the logical size is that divided by the scale (1707 by 960 for the first); a screen with no `+X+Y` sits right of the one before it. They are what `screens` lists. A program with no `Window` or `Panel` refuses it. |
| `--screen <NAME>` | | The screen to draw alone, or, with `--surface`, the one whose instance of a surface made once per screen is meant. |
| `--surface <NAME>` | | In a program of windows and panels, the surface to draw alone: its `as` name, else its namespace (a panel) or its title (a window). It is drawn at its own size, `--size` is not used, and the taps and keys are for it, in its own coordinates. A surface made once per screen needs `--screen` too. A program with no `Window` or `Panel` refuses it. |
| `--a11y <FILE>` | | Writes the accessibility tree to a text file, one node a line. A control a screen reader could not name is marked `(unnamed)`. In a program of windows and panels, each surface starts with a `Window "title"`, `Panel "namespace" @ DP-1` or `Popup "name" @ DP-1` line and its tree is indented below it. |

`--tap`, `--hold`, `--hover`, `--drag`, `--key`, `--type` and `--wait` run in the order they are written on the command line. A key is a single
character or one of `Left`, `Right`, `Up`, `Down`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, `Backspace`, `Delete`, `Enter`,
`Tab`, `Escape` and `Space`, with any of `shift`, `ctrl`, `alt`, `meta` and `mod` before it, joined
by `+`. `mod` is Command on macOS and Control elsewhere. With `--a11y`, the picture is drawn only if
`-o` is given too, and the tree needs no GPU. See [the dev loop](dev-loop.md#byard-shot) and
[accessibility](accessibility.md).

Text that starts with `-`, such as a negative number, would be read as another option. Join it to the
option with `=`: `--type=-1.5`.

### Screens

Without `--surface`, a program of windows and panels is drawn as the desktop would show it: the
screens side by side at their positions, over a checkerboard where nothing is, with every surface
composited in layer order (`background`, `bottom`, windows, `top`, `overlay`, popups). Panels are
placed by layer shell's rules, anchors, margins and exclusive zones in the order the compositor
applies them, windows are centred in the area the panels left, and popups are positioned and
flipped as an `xdg_positioner` does. The picture is at `--scale` on every screen; a screen's own
scale decides its logical size and what `screens` reports. A program whose only surface is one
`Window`, with no `--screens`, is drawn whole, as before.

`--tap`, `--hover`, `--drag` and the rest take points of the screens, in logical pixels, and go to
the topmost surface whose input region holds the point, so a tap on the transparent part of a
panel reaches what is below. A press outside the popup that has the grab is the compositor
taking the popup away: its `dismiss` runs and the press goes no further. `--key` and `--type` go
to the popup that has the grab, else to the surface last tapped.

```sh
byard shot shell/ --screens "DP-1:1600x900, HDMI-A-1:1280x720" --scale 1 -o screens.png
byard shot shell/ --screens "DP-1:1600x900, HDMI-A-1:1280x720" --surface bar --screen DP-1 -o bar.png
byard shot shell/ --screens "DP-1:1600x900" --scale 1 --tap 40,30 -o menu.png
```

## `byard test`

```text
byard test [OPTIONS] [PATH]
```

Runs the scenario files `tests/*.toml` of a project: each test draws a view headlessly, drives it
with the inputs `shot` takes, and checks the accessibility tree. It opens no window and never needs a
GPU. Output is one line per test and a summary, as `cargo test` prints; the exit status is 1 if any
test failed or any scenario file is wrong. A project with no `tests/*.toml` is an error. The format is
in [the dev loop](dev-loop.md#byard-test). In a program of windows and panels, it sees every surface:
the dump starts each with a `Window "title"`, `Panel "namespace" @ DP-1` or `Popup "name" @ DP-1` line, a step that names an element finds it on whichever
surface it is and taps it there, a step that gives a point is a point of the screens, and `--shots` writes the screens as
`<file>-<name>.png` and a picture of each surface at its own size (`<file>-<name>-<surface>.png`). A
`screens = "DP-1:1600x900, …"` key, in the file or in a test, says the screens, as `--screens` does; see [several windows](windows.md)
and [panels and screens](panels.md).

| Argument or flag | Default | |
|------------------|---------|-|
| `[PATH]` | the current project | The project directory or `byard.toml`. A `.byd` file is refused. |
| `--filter <TEXT>` | | Runs the tests whose `file::name` contains the text. A filter that matches none is an error. |
| `--shots <DIR>` | | Also writes each test's final picture, `<file>-<name>.png`, at scale 2 in the test's scheme. Without a GPU it says so once and skips the pictures. |

## `byard fmt`

```text
byard fmt [OPTIONS] [PATHS]...
```

Formats `.byd` files in place, with the one layout there is. See [the dev loop](dev-loop.md#byard-fmt).

| Argument or flag | |
|------------------|-|
| `[PATHS]...` | Files or directories to format. A project directory covers its entry directory and its path dependencies. Any other directory covers every `.byd` under it. Defaults to the current project. |
| `--check` | Changes nothing. Lists the files that are not formatted, and exits 1 if there are any. |

A file that does not parse, or that formatting would change the meaning of, is reported and left as it
was, and the command exits 1. Finding no `.byd` file at all is an error.

## `byard fix`

```text
byard fix [PATH]
```

Moves a project to the next [edition](language-reference.md#editions): rewrites its `.byd` files so they
mean in the next edition what they mean in the one `byard.toml` names, prints each one it changed through
the formatter, and then writes the new `edition` into the manifest.

| Argument | |
|----------|-|
| `[PATH]` | The project directory. Defaults to the current project. |

Nothing is written until every file has been rewritten, so a file that does not parse leaves the project as
it was. Only the project's own files are rewritten: a path dependency is a project with an edition of its
own, and is fixed where it is. A project already on the newest edition says so and changes nothing.

## `byard build`

```text
byard build [FILE]
```

Bakes the project's vector icon atlas: it finds every `VectorIcon` the app uses, turns each into a
distance-field picture, packs them into one atlas, and writes it to `.byard/generated/`, as
`vector_atlas.bin` (the texels) and `vector_atlas.rs` (the coordinate table). An icon whose name is
worked out at run time is invisible to this, so list it under `[assets.vectors] include` in
`byard.toml`. Not available in a library package.

| Argument | |
|----------|-|
| `[FILE]` | The project or `.byd` file. Defaults to `byard.toml`. |

## `byard bundle`

```text
byard bundle [OPTIONS] --bin <EXE> [PATH]
```

Packages a built app with its project, to run where its sources are not: a `.app` on macOS, a
directory elsewhere, or with `--layout linux` a prefix with a launcher entry and icons. See
[shipping](shipping.md).

| Argument or flag | Default | |
|------------------|---------|-|
| `[PATH]` | the current project | The project directory or `byard.toml`. |
| `--bin <EXE>` | required | The app's executable, built, as by `cargo build --release`. |
| `-o`, `--out <DIR>` | `.byard/bundle/` in the project | Where to write the bundle. |
| `--layout <LAYOUT>` | the shape your system uses | `app`, a macOS application bundle, `dir`, a directory holding the executable and `project/` beside it, or `linux`, a freedesktop.org prefix with `bin/`, `share/`, a `.desktop` entry, icons and AppStream metainfo. |
| `--archive` | | Also packs the Linux layout as `<exe>-<version>-linux-<arch>.tar.gz` beside it. Needs `--layout linux`. |

## `byard add`

```text
byard add [OPTIONS] <NAME>
```

Adds a dependency to `byard.toml`, then runs `byard get` to fetch and lock it. `install` is another
name for it. Choose one source.

| Argument or flag | |
|------------------|-|
| `<NAME>` | The package's name, a byld identifier such as `kit`. |
| `--path <PATH>` | Use a local directory. |
| `--git <GIT>` | Use a git repository. Pin it with `--tag` or `--rev`. |
| `--tag <TAG>` | Pin the git source to a tag. Conflicts with `--rev`. |
| `--rev <REV>` | Pin the git source to an exact commit. |
| `--registry <REGISTRY>` | Use a registry, a directory or an `http(s)://` URL. |
| `--version <VERSION>` | The version requirement for `--registry`, by Cargo's rules, such as `^0.3`. Defaults to `^` the newest version published. |

`--version` applies only to `--registry`, and `--tag` and `--rev` only to `--git`. See
[packages](packages.md).

## `byard get`

```text
byard get
```

Fetches dependencies and writes `byard.lock`. It is the only command that writes the lockfile. A lock
that still satisfies `byard.toml` is kept, and no registry is contacted. It takes no arguments and works
on the project in the current directory.

## `byard publish`

```text
byard publish [OPTIONS] [PACKAGE]
```

Publishes the package in a directory to a registry directory. The package needs a `name` and a `version`
in `[package]`. It runs `byard check` on the package first and publishes nothing if that has errors; there is no
flag to skip it.

| Argument or flag | |
|------------------|-|
| `[PACKAGE]` | The package directory. Defaults to the current directory. |
| `--registry <DIR>` | The registry directory. Defaults to the `BYARD_REGISTRY` environment variable. |

Without either, it stops and says to pass `--registry` or set `BYARD_REGISTRY`.

## `byard clean`

```text
byard clean [FILE]
```

Removes `.byard/cache` and `.byard/generated` from the project. Package checkouts are left alone.

| Argument | |
|----------|-|
| `[FILE]` | The project or `.byd` file. Defaults to `byard.toml`. |

## `byard explain`

```text
byard explain <NAME>
```

Prints what the error `NAME` means, why it is one, and a program that has it: its section of the
[error index](errors.md). The name is the word in the brackets of a diagnostic, and
`byard explain error[UnknownAttribute]`, pasted as it was printed, works too. A name no error has is
an error that suggests the closest one.

| Argument | |
|----------|-|
| `<NAME>` | The error's name, as `UnknownAttribute`. |

## `byard.toml` at a glance

| Table | Holds | Chapter |
|-------|-------|---------|
| `[project]` | `name`, `entry`, `icon`, `edition` | [Getting started](getting-started.md) |
| `[package]` | `name`, `version`, `edition`, for a library | [Packages](packages.md) |
| `[dependencies]` | Packages the project uses | [Packages](packages.md) |
| `[theme]` | Scheme, seed, colours, type, shapes, breakpoints | [Theming](theming.md) |
| `[assets.fonts]` | Typefaces | [Theming](theming.md) |
| `[assets.vectors]` | Icons to bake that no literal names | `byard build` above |
| `[http]`, `[http.hosts]` | Where requests go | [Data and I/O](data-and-io.md) |
| `[native_views.Name]` | Elements the Rust half draws | [Native views](rust-native-views.md) |
| `[bundle]` | `name`, `id`, `version`, and for Linux `categories`, `summary`, `license` | [Shipping](shipping.md) |
| `[dev]` | `frame_budget`, `hud`, `statusline` | [The dev loop](dev-loop.md) |
| `[lints]` | `unused-variable`, `unused-parameter`, `unused-view`, `untyped-reply`, each `"warn"` or `"allow"` | `byard check` above |

An unknown key is an error that names it and suggests the closest known one, at the top level and inside
`[project]`, `[package]`, `[assets]`, `[theme]` (and `[theme.color]`), `[dependencies]`, `[dev]`, `[http]`,
`[bundle]`, `[lints]` and `[native_views]`.
