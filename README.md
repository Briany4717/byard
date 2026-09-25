# Byard

A UI framework for native apps, written in Rust and rendered straight to the
GPU.

[![CI](https://github.com/Briany4717/byard/actions/workflows/ci.yml/badge.svg)](https://github.com/Briany4717/byard/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Status: pre-alpha](https://img.shields.io/badge/status-pre--alpha-orange.svg)](#status)

You describe the interface in **byld**, a small typed language for views,
styles and state, and write everything else (networking, storage, business
rules) in ordinary Rust. The two halves meet through generated bindings, not a
bridge or a serializer. While you work, `byard dev` reloads the window on every
save and keeps your app's state.

```
View Counter() {
    var count = 0

    Column #[gap: 20, p: 32, align: center] {
        Text("{count} taps") #[size: 24]
        Button("+") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8] => count++
    }
}
```

## Status

Pre-alpha. The engine, the language and the tools work, and the repository
builds a multi-screen reference app ([Aura Weather](examples/aura_weather)).
The language and the Rust APIs will still change before a first release, and
nothing is published to crates.io yet.

It runs on macOS, Linux and Windows. Mobile hosts do not exist yet.

## Try it

Everything below runs from a clone of this repository, through Cargo.

```sh
# The reference app
cargo run -p aura-weather

# Any example, with live reload: edit its .byd files and save
cargo run -p byard-cli -- dev crates/byard-cli/examples/weather

# Check a project without opening a window (what CI runs)
cargo run -p byard-cli -- check crates/byard-cli/examples/todo

# Render a project to a PNG without opening a window
cargo run -p byard-cli -- shot examples/aura_weather -o aura.png
```

There are about forty small examples under
[`crates/byard-cli/examples`](crates/byard-cli/examples), one per feature:
`todo`, `navigation`, `theme`, `font_families`, `text_input`, `shape_morph`,
`frosted_glass` and so on. Each one's `main.byd` starts with what to look for
when you run it.

## What you get

- **Views and state.** `Row`, `Column`, `Grid`, `ZStack`, `ScrollView`,
  `Overlay`, text, images, icons, and the usual controls (`Button`,
  `TextField` with input-method support, `Toggle`, `Slider`, `Checkbox`,
  `RadioButton`). State is `var`; anything reading it updates when it changes.
- **Styling.** Reusable `style { … }` values, `on hover` / `on pressed` /
  `on selected` blocks, and responsive blocks such as `on width >= md`.
- **Themes.** Colours, type scale, shapes, breakpoints and fonts declared in
  `byard.toml`. A whole palette can be derived from one seed colour, and
  switching between light and dark can cross-fade.
- **Motion.** `with anim.spring()` on any paint property, looping and keyframe
  animations, and transforms that do not move layout.
- **Drawing.** A `Canvas` with shapes, strokes, filled paths, gradients,
  shape morphing and soft clip masks.
- **Navigation.** Stacks and tabs with history, transitions and deep links.
- **Data and I/O.** List operations in the language; HTTP, JSON, timers and
  persistent storage as built-in capabilities; your own Rust controllers for
  anything else, called asynchronously from a view.
- **Packages.** Share views, fonts and a theme as a package, from a path, git,
  or a registry directory (`byard publish`).
- **Custom widgets in Rust.** A widget that needs its own drawing (a chart, a
  map) is written in Rust and used from byld like any element.

## A project

```
my_app/
  byard.toml     # name, entry view, theme, fonts, dependencies
  src/main.byd   # the view named Main is what runs
  src/*.byd      # other views, visible to each other without imports
  src/main.rs    # only if the app has Rust controllers or widgets
```

A Rust app starts with

```rust
fn main() -> Result<(), byard::ByardError> {
    byard::App::new(env!("CARGO_MANIFEST_DIR"))
        .title("My app")
        .run()
}
```

and reads the project exactly as `byard dev` does.

## How it is built

Rendering is [`wgpu`](https://github.com/gfx-rs/wgpu), layout is
[`taffy`](https://github.com/DioxusLabs/taffy), text is
[`cosmic-text`](https://github.com/pop-os/cosmic-text) through
[`glyphon`](https://github.com/grovesNL/glyphon). A logic thread runs your
views and produces a frame description; the render thread draws the latest one.
Unchanged parts of a frame are not laid out or redrawn again.

| Crate | What it is |
|-------|------------|
| `byard` | What an app depends on: `App`, the controller and widget APIs |
| `byard-core` | The engine: rendering, layout, text, the frame and threading |
| `byard-compiler` | The byld parser, checker and interpreter |
| `byard-project` | Reading a project: manifest, theme, packages, lockfile |
| `byard-platform` | The window and input host (winit) |
| `byard-cli` | The `byard` command |
| `byard-macro` | The macros for controllers and native widgets |
| `byld-lsp` | Editor support (early) |

The design documents are in [`docs/rfcs`](docs/rfcs).

`third_party/swash` is a vendored copy of the `swash` crate with one fix to
how it handles variable fonts; its `BYARD.md` explains why and how to remove
it once the fix is released upstream.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Before opening a pull request, run

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

and, if you changed how anything is drawn, run the affected example and look
at it.

## License

Either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
Unless you state otherwise, any contribution you submit for inclusion is
dual-licensed the same way, without additional terms.
