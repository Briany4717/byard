# The App

A project with no Rust half runs under `byard dev`, and that is all it needs. A project with
controllers, native views or shaders needs a program of its own, because `byard dev` cannot
link your Rust. That program is a few lines, and it is built around `App`.

```rust,ignore,excerpt
fn main() -> Result<(), byard::ByardError> {
    byard::App::new(env!("CARGO_MANIFEST_DIR"))
        .title("My app")
        .run()
}
```

`App::new` takes the project: its directory, its `byard.toml`, or a single `.byd` file.
`env!("CARGO_MANIFEST_DIR")` is the directory of your crate, so the same folder holds
`Cargo.toml`, `byard.toml` and `src/main.byd`. `run` opens the window and returns when it
closes. The app reads the project exactly as `byard dev` does: its theme, its fonts, every
`.byd` file and its packages.

The crate is an ordinary binary crate that depends on `byard`:

```toml
[package]
name = "my-app"
version = "0.1.0"
edition = "2024"

[dependencies]
byard = { path = "../byard/crates/byard" }
```

and you run it with `cargo run`.

## What you can set

Every step is a method on `App`, chained before `run`:

| Method | Does |
|--------|------|
| `title(…)` | The window title. Defaults to the name of the entry |
| `size(w, h)` | The starting size in logical pixels. Defaults to 1280 by 720 |
| `provide(controller)` | Registers a [controller](rust-controllers.md) a view can `inject` |
| `native_view::<V>()` | Registers a [native view](rust-native-views.md) |
| `shader::<S>()` | Registers a shader a native view draws through |
| `app_id(…)` | The identity this app's saved data is filed under |
| `hot_reload(bool)` | Follows the project's files while the app runs |
| `trace(path)` | Writes a Chrome trace of every frame |
| `without_default_capabilities()` | Drops `Http`, `Json`, `Store` and `Timer` |

A mistake in a registration, such as a controller under a reserved name or two native
views under one name, is not lost. `run` fails and says which.

### `app_id`

Saved data, what `Store` keeps, is filed under the app's identity. By default that is the
executable's name. Set it when the binary might be renamed, or shipped under several names,
and its data should follow the app and not the file:

```rust,ignore,excerpt
byard::App::new(env!("CARGO_MANIFEST_DIR"))
    .app_id("dev.byard.controller-demo")
    .run()
```

It is a decision about data, which is why it is separate from the title. Changing it points
the app at a different store.

### `hot_reload`

`byard dev` cannot link your Rust, but your app can follow its own files:

```rust,ignore,excerpt
let dev = std::env::args().any(|a| a == "--dev");
byard::App::new(env!("CARGO_MANIFEST_DIR")).hot_reload(dev).run()
```

With it on, an edit to a `.byd` file, the manifest or the theme applies to the open window, with
your controllers and native views still linked in. An edit that does not compile is reported on
standard error, and the last good program keeps running. It is for development. A shipped app has
no project beside it to follow.

### `trace`

`App::trace(path)` writes the same Chrome trace `byard dev --trace` does: the logic thread, the
render thread and the GPU's passes, frame by frame. Open it in Perfetto, `chrome://tracing` or
speedscope. The file is valid at every instant, so a run that is killed keeps its trace.

## A shipped app is not `byard dev`

`byard dev` watches files, re-reads the project on every save, paints an error overlay instead of
dying, and keeps the loop turning so a change appears without you moving the mouse. Each of those
costs something a shipped app should not pay, and the last is a battery drain.

`App` reads the entry once, opens a window, and waits. A still scene costs no frames. A controller's
answer, a timer or an input wakes it, and nothing else does.

## Drawing the app without a window

`byard shot` cannot draw an app with a Rust half. `App::shot` can:

```rust,ignore,excerpt
fn main() -> Result<(), byard::ByardError> {
    // `cargo run -p sparkline-view -- --shot out.png` draws the app to a PNG
    // instead of opening it, after a second of running.
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("--shot"), Some(out)) => app().shot(out, &byard::Shot::after(1000)),
        _ => app().run(),
    }
}
```

The same program `run` opens, with its controllers, native views and shaders, is run for a stretch
of real time and drawn into a PNG. Calls to controllers are really made, and their answers really
applied, so a screen that loads data shows the data. `Shot::after(ms)` says how long to run. The
plan can also script what happens first: `.tap(x, y)` opens the screen you want, `.wait(ms)` waits
for what a tap started, `.drag(from, to, frames)` pans or scrolls, `.system_dark(true)` reports a
dark system appearance to an app whose theme follows it, and `.trace(path)` writes a trace of the
run.

Because it needs no window, it runs in a test:

```rust,ignore,excerpt
        let out = std::env::temp_dir().join(format!("sparkline-{}.png", std::process::id()));
        match super::app().shot(&out, &byard::Shot::after(600)) {
            Ok(()) => {}
            Err(e) if e.to_string().contains("no GPU") => {
                eprintln!("no GPU adapter, skipping: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        }
```

An adapter-less machine, such as a CI box, reports `no GPU` and the test skips itself.

## When the app does not start

`run` and `shot` return a `ByardError`. When the views do not compile, the diagnostics have
already been printed by the time `main` sees the error, in the same form `byard check` prints
them, so `main` only has to decide how to exit. `ByardErrorExt` gets at what is inside:

```rust,ignore,excerpt
use byard::ByardErrorExt as _;

match byard::App::new(env!("CARGO_MANIFEST_DIR")).run() {
    Ok(()) => {}
    // Already printed; `report` has each diagnostic with its file and span.
    Err(e) if e.as_compile().is_some() => std::process::exit(2),
    Err(e) => {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
```

`as_compile()` gives the `CompileReport`, every diagnostic with the file it points into, and
`as_project()` the `ProjectError` behind a missing manifest or a dependency that cannot be
fetched. A test can assert on either instead of matching on the message.

## Packaging

A built app reads its project from the path it was compiled at. To run it on another machine, bundle
it. See [shipping](shipping.md).
