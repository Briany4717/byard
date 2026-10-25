# Shipping

An app reads its project when it starts: the views, the manifest, the fonts and the pictures.
Built with `App::new(env!("CARGO_MANIFEST_DIR"))`, the binary looks for them where it was
compiled, and on any other machine that path is not there. `byard bundle` packages the app
with a copy of its project, so it runs where neither the sources nor the Byard tools are.

This is for an app with a Rust half, one that has a program of its own. See
[the App](rust-app.md). A project that only runs under `byard dev` has no executable to ship.

## Bundling

Build the app, then bundle it:

```sh
cargo build --release
byard bundle --bin target/release/my-app
```

```text
▏ Bundle     /home/ada/my-app/.byard/bundle/my-app
▏ Name       my-app
▏ Id         local.byard.my-app
▏ Version    0.1.0
note: no [bundle] id in byard.toml, so the app is `local.byard.my-app`; set one before shipping, its settings are kept under it
```

The command runs in the project, or takes the project as an argument:
`byard bundle path/to/project --bin …`. It first checks the project, and refuses to bundle one
with errors, since a window that is blank on someone else's machine is worse than an error on
yours.

What it writes depends on the system:

```text
macOS (--layout app)                 elsewhere (--layout dir)
Name.app/Contents/                   name/
  Info.plist                           the executable
  MacOS/the executable                 project/
  Resources/icon.icns                    byard.toml, src/, fonts/, …
  Resources/project/
    byard.toml, src/, fonts/, …
```

On macOS you get a `.app` you can drag into Applications. Elsewhere you get a directory with the
executable and a `project/` folder beside it. `--layout app` and `--layout dir` choose the shape
explicitly, and by default it is the one your system uses. A third shape, `--layout linux`, is for
a Linux desktop. See [A Linux desktop](#a-linux-desktop) below.

The bundle goes under `.byard/bundle/` unless you say `-o <dir>`.

## What is in the copy

The copy is the project directory as it is, less what is not read at run time:

- build output, `target/`,
- hidden entries such as `.byard/` and `.git`,
- the Rust half, which is already in the binary: `*.rs`, `Cargo.toml` and `Cargo.lock`,
- `byard.lock`.

The project's packages come too. Each resolved package is copied under `.byard/deps/` in the
bundle, and every manifest that names it is rewritten to point there, so the bundle needs neither
`~/.byard/cache` nor the `../` a path dependency used on your machine.

The bundle is then loaded the way the app will load it, and a bundle that does not is an error on
your machine rather than a blank window on someone else's. A file named from outside the project,
such as a font at `../../fonts/x.ttf`, is left behind, which is the usual way this fails. Keep what
the app reads inside the project.

At startup, an app whose build path is gone looks for its project beside its executable: `project/`
next to it, `../Resources/project/` inside a `.app`, or `../share/<name>/project/` in a Linux
prefix.

## A Linux desktop

A directory gives a Linux desktop nothing to see: no entry in the launcher, no icon, and no layout a
package can install. `--layout linux` writes a tree the desktop understands:

```sh
byard bundle --bin target/release/aura-weather --layout linux
```

```text
aura-weather/
  bin/aura-weather
  share/aura-weather/project/
    byard.toml, src/, fonts/, …
  share/applications/dev.byard.aura-weather.desktop
  share/icons/hicolor/16x16/apps/dev.byard.aura-weather.png
  share/icons/hicolor/…/apps/dev.byard.aura-weather.png
  share/metainfo/dev.byard.aura-weather.metainfo.xml
```

The tree is a prefix, laid out as `/usr` is. To install it for one user, copy it into `~/.local`:

```sh
cp -r .byard/bundle/aura-weather/* ~/.local/
```

The launcher shows the app once the desktop reads `~/.local/share/applications` again. A
distribution package maps the same tree onto `/usr` as it is.

What is in it:

- the **`.desktop` entry** names the app with `[bundle] name`, starts it with the executable's
  name, which is looked up on `PATH`, and carries the icon, the categories and, if you wrote one,
  the summary as its comment. The file is named for the id,
- the **icon**, from `[project] icon`, at 16, 32, 48, 64, 128, 256 and 512 pixels, up to the size of
  your own picture, so a 256 pixel icon gives six files. Without an icon there are none,
- the **AppStream file**, which software centres read, with the id, the name, the summary and the
  licence.

Add `--archive` to also pack the tree as one file next to it:

```sh
byard bundle --bin target/release/aura-weather --layout linux --archive
```

```text
.byard/bundle/aura-weather-0.1.0-linux-x86_64.tar.gz
```

It is `<exe>-<version>-linux-<arch>.tar.gz`, and the paths in it start at the prefix, so a user
can unpack it where they like: `tar xzf aura-weather-0.1.0-linux-x86_64.tar.gz -C ~/.local`.
`--archive` goes with `--layout linux` and nothing else.

If `desktop-file-validate` or `appstreamcli` is installed, `bundle` runs it over what it wrote
and prints the warnings. Neither is needed.

The id matters more here. On Linux a window announces its app's id to the compositor, as the
Wayland `app_id` and the X11 `WM_CLASS`, and the desktop uses it to match the window to its
`.desktop` entry and icon. Every app's window carries its `[bundle] id`, or
`dev.byard.<executable name>` if there is none. `byard dev` keeps `dev.byard.Byard`, as the
window is the tool's. A window rule that matches an app by class can now tell two apps apart.

## Naming it

A `[bundle]` table says what the system calls the app:

```toml
[bundle]
name = "Aura Weather"
id = "dev.byard.aura-weather"
version = "0.1.0"
categories = ["Utility", "Weather"]
summary = "Weather with a map"
license = "MIT"
```

- **`name`** is what the Finder and the Dock show. It defaults to the project's name.
- **`id`** is a reverse-DNS identifier: two or more parts of letters, digits and `-`, separated by
  dots. The system keeps the app's settings and permissions under it, so set it before you ship, and
  do not change it afterwards. Without one, the id is `local.byard.<project name>`, or
  `dev.byard.<executable name>` in the Linux layout, and `bundle` says so.
- **`version`** is the version the package reports. It defaults to `0.1.0`.
- **`categories`** is a list of freedesktop.org menu categories, such as `"Utility"` or `"Game"`,
  that say where a Linux launcher files the app. Without it, the entry is filed under `Utility`.
- **`summary`** is one line saying what the app is. A Linux launcher shows it as the entry's
  comment, and a software centre as the listing's summary.
- **`license`** is the app's licence as an SPDX expression, such as `"MIT OR Apache-2.0"`. It goes
  into the AppStream file. Byard does not check it beyond reading it.

The last three are used by the Linux layout and ignored by the others.

An id that is not reverse-DNS is an error that shows an example, and an unknown key in `[bundle]` is
one too. So is a `summary` or `license` that is empty, and `categories` that is not a list of names
without a `;` in them.

## The icon

```toml
[project]
name = "aura_weather"
entry = "src/main.byd"
icon = "assets/icon.png"
```

`icon` is a square PNG. On Windows and Linux the window shows it in the title bar and the taskbar.
macOS takes an app's icon from its bundle, so `byard bundle` makes an `.icns` from it, with the
picture at every size up to its own. A path that is not there is a manifest error, and a picture
that is not square is a bundle error.

## Saved data

What `Store` keeps is filed under the app's identity. A shipped app's identity is its executable's
name, unless the program says `.app_id("…")`. Decide it on purpose, before the first release,
because changing it points the app at a different store. See [data and I/O](data-and-io.md).

## One more thing before you ship

Run `byard check` on the project, and look at the app on a real display at more than one window
size. `byard shot` can take the picture without one. The checks catch what the compiler can know.
The picture catches the rest.
