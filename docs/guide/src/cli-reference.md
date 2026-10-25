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
| [`fmt`](#byard-fmt) | Format `.byd` files in place |
| [`build`](#byard-build) | Bake the vector icon atlas |
| [`bundle`](#byard-bundle) | Package a built app with its project |
| [`add`](#byard-add) | Add a dependency, then fetch and lock it |
| [`get`](#byard-get) | Fetch dependencies and write `byard.lock` |
| [`publish`](#byard-publish) | Publish a package to a registry |
| [`clean`](#byard-clean) | Remove generated files and caches |
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

## `byard shot`

```text
byard shot [OPTIONS] [PATH]
```

Draws the project to a PNG, with no window. It cannot draw an app with a Rust half. Not available in a
library package.

| Argument or flag | Default | |
|------------------|---------|-|
| `[PATH]` | the current project | The project directory, `byard.toml` or `.byd` file. |
| `-o`, `--out <PNG>` | `shot.png` | Where to write the picture. |
| `--size <SIZE>` | `440x880` | Logical size, as `WIDTHxHEIGHT`. |
| `--scale <SCALE>` | `2` | Device pixels per logical pixel. |
| `--at <MS>` | `1500` | Milliseconds of app time to run first, so animations settle. |
| `--dark` | | Forces the dark scheme. Conflicts with `--light`. |
| `--light` | | Forces the light scheme. |
| `--tap <X,Y>` | | Taps a logical point before the picture. Repeat it to tap more than once. |
| `--key <KEY>` | | Presses a key before the picture, such as `Left`, `shift+Left` or `mod+a`. Repeat it to press more. |
| `--type <TEXT>` | | Types text before the picture. Repeat it to type more. |
| `--a11y <FILE>` | | Writes the accessibility tree to a text file, one node a line. A control a screen reader could not name is marked `(unnamed)`. |

`--tap`, `--key` and `--type` run in the order they are written on the command line. A key is a single
character or one of `Left`, `Right`, `Up`, `Down`, `Home`, `End`, `Backspace`, `Delete`, `Enter`,
`Tab`, `Escape` and `Space`, with any of `shift`, `ctrl`, `alt`, `meta` and `mod` before it, joined
by `+`. `mod` is Command on macOS and Control elsewhere. With `--a11y`, the picture is drawn only if
`-o` is given too, and the tree needs no GPU. See [the dev loop](dev-loop.md#byard-shot) and
[accessibility](accessibility.md).

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
in `[package]`.

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

## `byard.toml` at a glance

| Table | Holds | Chapter |
|-------|-------|---------|
| `[project]` | `name`, `entry`, `icon` | [Getting started](getting-started.md) |
| `[package]` | `name`, `version`, for a library | [Packages](packages.md) |
| `[dependencies]` | Packages the project uses | [Packages](packages.md) |
| `[theme]` | Scheme, seed, colours, type, shapes, breakpoints | [Theming](theming.md) |
| `[assets.fonts]` | Typefaces | [Theming](theming.md) |
| `[assets.vectors]` | Icons to bake that no literal names | `byard build` above |
| `[http]`, `[http.hosts]` | Where requests go | [Data and I/O](data-and-io.md) |
| `[native_views.Name]` | Elements the Rust half draws | [Native views](rust-native-views.md) |
| `[bundle]` | `name`, `id`, `version`, and for Linux `categories`, `summary`, `license` | [Shipping](shipping.md) |
| `[dev]` | `frame_budget`, `hud`, `statusline` | [The dev loop](dev-loop.md) |

An unknown top-level table is reported as a warning and ignored. Inside `[dependencies]`, `[dev]`,
`[http]`, `[bundle]` and `[native_views]`, an unknown or malformed key is an error.
