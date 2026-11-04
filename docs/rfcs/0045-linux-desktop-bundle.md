# RFC-0045: A Linux desktop bundle, and each app its own id

- **Status:** Active, implemented 2026-10-25 (#343). `[bundle]` has no
  homepage yet, which AppStream's check wants (#357).
- **Author(s):** Briany4717
- **Created:** 2026-10-24
- **Last updated:** 2026-10-26
- **Depends on:** `byard bundle` (#329: `[bundle]`, `--layout app|dir`, the
  project copy and the lookup beside the executable), the window icon (#325).
- **Enables:** a Byard app that appears in a Linux launcher with its own name
  and icon, that the compositor matches to its window, and that a distribution
  package (`.deb`, `.rpm`, Arch `PKGBUILD`, Flatpak) can install without
  rearranging.

> **Placement:** `docs/rfcs/0045-linux-desktop-bundle.md`

---

## Summary

`byard bundle --layout linux` writes a FreeDesktop install tree: the binary in
`bin/`, the project in `share/<exe>/project/`, a `.desktop` entry, the icon at
every hicolor size up to its own, and an AppStream metainfo file. `--archive`
also packs it as `name-version-linux-arch.tar.gz`. Every window now carries
its own app's id (Wayland `app_id`, X11 `WM_CLASS`) taken from `[bundle] id`,
instead of the `dev.byard.Byard` every Byard app shares today.

## Motivation

On Linux, `byard bundle` writes `name/exe` and `name/project/`. Nothing tells
the desktop the app exists: no launcher entry, no icon in the menu, nothing a
package can install into `/usr` without the packager inventing a layout. And
every Byard window announces itself as `dev.byard.Byard`
(`byard-platform/src/lib.rs`, `APP_ID`), so the compositor groups all Byard
apps as one application, the taskbar cannot find the right icon, and window
rules (`windowrule = ..., class:^(dev.byard.Byard)$` in Hyprland) cannot tell
two apps apart. Shipping to Linux, where most developers who choose a native
GPU framework run, is currently left to each app.

## Guide-level explanation

```bash
cargo build --release -p aura-weather
cargo run -p byard-cli -- bundle examples/aura_weather \
    --bin target/release/aura-weather --layout linux --archive
```

writes

```
.byard/bundle/aura-weather/
  bin/aura-weather
  share/aura-weather/project/          byard.toml, src/, fonts/, assets/
  share/applications/dev.byard.aura-weather.desktop
  share/icons/hicolor/{16,32,48,64,128,256,512}x…/apps/dev.byard.aura-weather.png
  share/metainfo/dev.byard.aura-weather.metainfo.xml
.byard/bundle/aura-weather-0.1.0-linux-x86_64.tar.gz
```

The tree is a prefix: `cp -r .byard/bundle/aura-weather/* ~/.local/` installs
it for one user (the launcher shows it after the desktop re-reads
`~/.local/share/applications`), and a packager maps it onto `/usr` as is.

The `.desktop` entry uses `[bundle] name` as `Name`, `[bundle] id` as the file
name and `StartupWMClass`, `Exec=aura-weather` (resolved through `PATH`, so
the prefix can be anywhere on it), and `Icon=<id>`. A new optional
`[bundle] categories = ["Utility"]`, `[bundle] summary = "…"` and
`[bundle] license = "MIT"` (an SPDX expression) fill `Categories`, `Comment`
and the metainfo's `project_license`; without them, `Categories=Utility;`, no
comment and no licence line.

## Reference-level explanation

### 1. The app's id reaches the window

`App` already has `app_id` (the store's name, defaulting to the executable's
name). The platform's window id becomes:

1. `[bundle] id` from the manifest, when present (`dev.byard.aura-weather`);
2. otherwise `dev.byard.<exe name>`, so two unbundled apps still differ.

`byard dev` keeps `dev.byard.Byard`, because it is the dev tool's window. The
id is passed into `WinitHost` instead of the `APP_ID` constant.

This is the id the `.desktop` file is named after, which is what lets GNOME,
KDE and Hyprland-based shells match a window to its entry and icon.

### 2. Finding the project

`App::new` looks, when its compile-time path is gone, beside the executable
(`<exe dir>/project`, `<exe dir>/../Resources/project`, as today) and now also
at `<exe dir>/../share/<exe name>/project`, the linux layout's place. One more
`stat` at start-up, only when the compile-time path is missing.

### 3. Icons

The hicolor sizes written are those of {16, 32, 48, 64, 128, 256, 512} not
larger than the source icon, resized with the `image` crate's Lanczos3 filter
(the `.icns` path already does this), so a 256 px icon produces seven files
up to 256. A non-square icon is refused, as for `.icns`.

### 4. Metainfo

A minimal AppStream component (`id`, `name`, `summary`, `metadata_license`
`CC0-1.0`, `project_license` from `[bundle] license` if set, `launchable` of
type `desktop-id`). It is what software centres and Flathub read; without it a
Flatpak cannot be published.

### 5. Validation

The command runs `desktop-file-validate` and `appstreamcli validate --no-net`
on what it wrote **when they are installed**, and prints their warnings. A
missing tool is not an error: the files are generated from a fixed template
that the tests validate.

## Cost

Run time: one path probe at start-up when the build path is gone. Build time:
icon resizing, once per bundle. Nothing for an app that is not bundled.

## Drawbacks

Changing every app's window id changes the class a user's existing window
rules match. Byard is pre-alpha and the old id named no app, so the rules it
breaks could not have targeted one app anyway; the CHANGELOG says so.

## Rationale and alternatives

- **AppImage**: one file, but it needs `appimagetool` (downloaded, not in
  distributions) and FUSE at run time. A prefix tree is the input every
  format, AppImage included, is built from; producing AppImage on top is a
  Future possibility.
- **Flatpak manifest**: needs a runtime and SDK choice per app; the metainfo
  file is the part every Flatpak needs, and this writes it.

## Resolved questions

### Q1, where the project goes in the tree

**Options.** (a) `share/<exe>/project`; (b) `lib/<exe>/project`; (c) beside
the binary in `bin/`. **Resolution: (a).** The project is architecture-
independent data (views, fonts, images), which is what `share/` is for under
the FHS; (c) puts non-executables in `bin/`, which packagers reject.

### Q2, the archive's name

**Resolution:** `<exe>-<bundle version>-linux-<std::env::consts::ARCH>.tar.gz`,
the convention GitHub release assets and `cargo-dist` use, so a release page
reads like every other one.

## Acceptance

- Integration tests (beside the existing `bundle.rs` ones): the tree's paths,
  the `.desktop` file's keys, every icon size up to the source and none above,
  the metainfo's id; the bundled tree resolves; the archive lists the same
  files.
- The generated `.desktop` and metainfo pass `desktop-file-validate` and
  `appstreamcli validate --no-net` in a test that skips when the tools are
  absent.
- An app whose project is only at `../share/<exe>/project` finds it.
- The platform passes the app's id: a unit test on the id choice (bundle id,
  then `dev.byard.<exe>`).
- **For a person to check:** bundle Aura Weather with `--layout linux`, copy
  the tree into `~/.local`, and see it in the launcher with its icon; while it
  runs, `hyprctl clients` shows `class: dev.byard.aura-weather`.

## Future possibilities

AppImage and Flatpak manifests from this tree; Windows `.ico` and an MSI;
code signing and notarisation on macOS.
