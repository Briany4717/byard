# Byard

**A high-performance, cross-platform UI framework with direct-to-GPU rendering, written in Rust 🦀**

[![CI](https://github.com/Briany4717/byard/actions/workflows/ci.yml/badge.svg)](https://github.com/Briany4717/byard/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Status: pre-alpha](https://img.shields.io/badge/status-pre--alpha-orange.svg)](#project-status)

## Project status

Byard is pre-alpha. The engine, compiler, and dev toolchain are functional and
tested: you can write a `.byd` file today and run it live-reloading in a native
window.

What works end to end right now: interactive widgets (`Toggle`, `Slider`,
`TextField`, `Checkbox`, `RadioButton`) with focus and keyboard input; layout
with `Grid` and `ZStack` alongside `Row`/`Column`; decorated rendering (border,
shadow, opacity, per-corner `radius`, continuous-curvature `smooth` corners);
paint-time transforms (`translate`, `scale`, `rotate`); a theme system; the
`#[byard_controller]` boundary into Rust; navigation and routing; data and
collection operations; the MSDF vector and icon pipeline; paint effects (ripple,
backdrop blur); an incremental redraw path with a real per-element dirty set; and
a zero-allocation frame profiler with an in-window HUD.

Still in progress or not yet started: parts of the animation runtime (GPU
springs), the hierarchical transform stack, a dev-mode bytecode JIT, a polyglot
controller bridge, and the byld-facing async I/O capabilities. Public APIs and
the `byld` syntax will change before the first stable release.

For the exact state of each subsystem, checked against the code rather than the
design notes, see [`support/STATUS_RFCS.md`](support/STATUS_RFCS.md).

## What is Byard?

Byard is a UI framework built on one rule: the declarative layer and the systems
layer never live in the same file.

`byld` is a statically typed DSL used only to declare UI structure, styling, and
visual reactivity. Rust is used only for business logic: networking, disk,
cryptography, OS integration, and anything that touches the real world. The two
communicate through compile-time-generated, zero-cost bindings. There is no IPC,
no serialization boundary, and no runtime glue.

Byard renders directly to the GPU through [`wgpu`](https://github.com/gfx-rs/wgpu),
lays out with [`taffy`](https://github.com/DioxusLabs/taffy), and rasterizes text
with [`glyphon`](https://github.com/grovesNL/glyphon). It has no garbage
collector. Memory is owned by view-scoped arenas that are released in a single
linear pass when a view unmounts, so there are no GC pauses and no VRAM spikes.

## Why does Byard exist?

Existing UI stacks each carry a structural cost Byard is designed to avoid. The
web and DOM reach everywhere but force a document model to behave like an
interactive one, which is expensive in RAM and CPU. Flutter has an excellent
cross-platform story but leans on deep wrapper trees. Pure Rust UI gives you
memory safety and real concurrency but often at the cost of fighting the borrow
checker for layout.

Byard aims for the ergonomics and readability of React or SwiftUI with the memory
safety, concurrency, and low-level control of Rust, and for deterministic
performance: stable frame times, no GC pauses, no VRAM spikes. Performance is
treated as a correctness criterion, not an aspiration.

## Design principles

Byard keeps `byld` for design and Rust for logic, and never mixes them in one
file. It manages memory through Rust ownership and view-scoped arenas rather than
a garbage collector. It treats a stable frame rate and bounded VRAM as
first-class correctness criteria. It keeps raw math out of the view: the
declarative layer exposes views, signals, and environments, never graphs,
pointers, or z-indices. And it reloads live by default, so `byard dev` reflects
every save in the running window, preserving state on reactive-compatible changes
and staying gesture-safe on structural ones.

## A taste of `byld`

```
View Counter() {
    var count = 0

    Column #[gap: 20, p: 32, align: center, justify: center] {
        Text("{count} taps") #[size: 24, color: 0xFFFFFF]

        Button("+") #[bg: 0x3B82F6, radius: 8, p: 10,
                      color: 0xFFFFFF, weight: bold] => count++
    }
}
```

Wrapper components like `Padding` or `Align` are intentionally absent. Spatial and
decorative properties are inline arguments on the element they affect.

`radius` also takes a positional 4-tuple for independent per-corner control,
`radius: (4, 8, 12, 16)` in CSS-style top-left, top-right, bottom-right,
bottom-left order, with a plain scalar broadcasting to all four corners.

Paint-time transforms move, resize, and rotate an element visually without
touching layout. A hover-to-lift card is just `scale: hovered ? 1.03 : 1.0`, and
its siblings never reflow.

## Getting started

```sh
# Scaffold a new project
byard new my_app
cd my_app

# Start the live-reload dev window
byard dev

# Validate without opening a window (CI-friendly)
byard check
```

Edit `main.byd` and save. The window updates within one frame, with no recompile,
no `cargo run`, and no hot key.

Running from this repo (pre-release): `byard` is not published yet, so there is no
`byard` on your `PATH`. Invoke the CLI through Cargo and pass a `.byd` path
directly (no `byard.toml` needed):

```sh
# Live-reload dev window for the bundled demo
cargo run -p byard-cli -- dev crates/byard-compiler/examples/hello_world.byd

# Validate only (no window)
cargo run -p byard-cli -- check crates/byard-compiler/examples/hello_world.byd
```

There are runnable examples under
[`crates/byard-cli/examples/`](crates/byard-cli/examples/), from `todo` and
`navigation` to `frosted_glass`, `shape_morph`, and `profiling`.

## Architecture at a glance

The engine is a set of concurrent subsystems. A logic subsystem interprets state
(`var`/`let` signals) and owns the per-view memory arenas on a dedicated thread. A
spatial subsystem pairs Taffy layout with a spatial hash grid for O(1)
hit-testing, decoupled from the UI tree. A render subsystem is a multi-pipeline
`wgpu` command dispatcher (solid boxes, text glyphs, decorated boxes, textures,
vector MSDF, canvas shapes, ripple, and backdrop blur) with a real per-element
dirty set and GPU scissor clipping, so an incremental frame only repaints what
actually changed. A concurrency layer carries double-buffered visual state, the
Relay signal bus, and a Tokio pool for async I/O.

The `byard-cli` dev runner wires these together behind a `notify` file watcher. On
every save the view's shape is diffed: reactive-compatible patches apply instantly
with signal state preserved, and structure-incompatible patches are held past any
in-flight pointer gesture, then applied cleanly.

The design is specified in the RFCs under [`docs/rfcs/`](docs/rfcs/). The core set:

| RFC | Topic |
|-----|-------|
| [0001](docs/rfcs/0001-core-architecture.md) | Core architecture, crate layering, memory model, `PlatformHost` |
| [0002](docs/rfcs/0002-byld-language-and-compiler-pipeline.md) | `byld` language, compiler pipeline, hot-reload boundary |
| [0003](docs/rfcs/0003-interactive-events-and-view-mutation.md) | Event system, gesture recognition, write-back |
| [0004](docs/rfcs/0004-reactive-interpreter.md) | Reactive core: Mark-and-Pull, memos, structural scopes |
| [0005](docs/rfcs/0005-intrinsic-view-catalog.md) | Built-in view catalog (`Column`, `Button`, `TextField`, and more) |
| [0006](docs/rfcs/0006-cli-and-dev-runner.md) | `byard` CLI, dev runner, live-reload wiring |

The feature RFCs (0007 through 0033) cover user-view composition, packages,
vectors, animations, transforms, styling, telemetry, overlays, navigation, data
operations, paint effects, shapes, and the invalidation model. Their real state
is tracked in [`support/STATUS_RFCS.md`](support/STATUS_RFCS.md).

## Crate layout

```
crates/
  byard/          the public facade crate an app depends on
  byard-core/     engine subsystems (renderer, atlas/layout, relay, frame)
  byard-compiler/ byld lexer, parser, reactive interpreter, hot-reload logic
  byard-platform/ PlatformHost implementation (winit + wgpu)
  byard-cli/      the byard binary (new / dev / check / build / add / get / clean)
  byard-macro/    the #[byard_controller] proc-macro (the byld to Rust boundary)
  byld-lsp/       language server (in progress)
```

## Roadmap

| Phase | Status | Scope |
|-------|--------|-------|
| 0, Design | complete | Core architecture, crate layering, memory model |
| 1, Engine core | complete | `wgpu` renderer, Taffy layout, Relay threading, `PlatformHost` |
| 2, byld compiler and dev toolchain | complete | Lexer, parser, reactive interpreter, event router, hot-reload, `byard-cli` |
| 3, Interactive widgets and rendering polish | complete | Value widgets, focus and keyboard input, `for`/`when`, decorated rendering, theming, the controller boundary, dirty-rect redraw, async assets |
| 4, Motion and interactive styling | mostly complete | Paint-time transforms, the frame profiler, the style system, and interactive states have landed; the GPU spring path and the hierarchical transform stack remain |
| 5, Composition and packages | complete | User-view composition within and across files, modules, dependencies, distribution |
| 6, Extended widgets and rendering | complete | Checkbox/radio/grid/zstack, vectors, overlays, navigation, data operations, paint effects, the shape system, the invalidation model |
| 7, Production transpiler | planned | `byld` to native Rust AOT compilation, the dev-mode JIT, a polyglot controller bridge, and an accessibility bridge |

## Contributing

Byard is open to contributions. Read the relevant RFC before touching a subsystem;
the design decisions are the contract and the code is the implementation. Keep
`cargo test --workspace` and `cargo clippy --workspace` green. One house rule
worth calling out early: do not use the em dash character anywhere in the
repository, in prose or in code comments. Use a comma, colon, parentheses, or a
plain hyphen instead.

## License

Licensed under either of Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or MIT License ([LICENSE-MIT](LICENSE-MIT)) at
your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall be
dual-licensed as above, without any additional terms or conditions.
