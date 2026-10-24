# Introduction

Here is a counter. It is a whole Byard app.

```byld
View Counter() {
    var count = 0

    Column #[gap: 20, p: 32, align: center] {
        Text("{count} taps") #[size: 24]
        Button("+") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8] => count++
    }
}
```

Tap the button and `count` goes up. The text reads `count`, so it changes with
it. Nothing else says so. There is no listener to attach and no state object to
update. That is the whole idea of the language, and the rest of this guide fills
in what surrounds it.

Byard is a framework for native apps, written in Rust and drawn straight to the
GPU. It runs on macOS, Linux and Windows. There is no web view, no browser
engine and no JavaScript.

## Two halves

An app has two parts, and each is written in the language that suits it.

**The interface is written in byld.** Byld is a small typed language for views,
state and styles. Its files end in `.byd`. A view says what is on the screen as
a function of its state, and the engine keeps the screen in step with it.

**Everything else is written in Rust.** Networking that is not plain HTTP, a
database, business rules, a chart that needs its own drawing code: these are
ordinary Rust. A *controller* is a Rust struct a view can call. A *native view*
is a Rust type that draws itself and is used from byld like any element.

The two meet through generated bindings. There is no bridge to serialise
through and no second runtime. A view calls a controller by name, the call runs
on a thread pool, and the answer comes back to the view as a value.

Many apps never need the Rust half. A weather app that reads a public API and
draws forecasts and charts is pure byld, and so is a todo list that keeps its
items on disk. You add Rust when you reach something byld does not do, not
before.

## Why it is built this way

**Efficiency.** A frame is described by the logic thread and drawn by the render
thread. Parts of the frame that did not change are not laid out or redrawn. When
nothing moves, the app draws nothing and waits.

**Low resource use.** An idle app costs next to nothing, because it parks until
an input, a timer or a reply wakes it. A looping animation lives and dies with
its element, so stopping one means removing it, and the loop goes with it.

**Speed.** Rendering is `wgpu`, layout is `taffy` and text is `cosmic-text`.
Boxes, text, shadows, blur and vector shapes all go to the GPU. Animation runs
there too: a property you animate does not trigger layout.

**Developer experience.** `byard dev` reloads the window on every save and keeps
your state through most edits. `byard check` reads a whole project and finds a misspelt name, a
wrong type or an unknown attribute without opening a window, so CI can run it.
`byard shot` draws a project to a PNG, also without a window.

## Status

Byard is pre-alpha. The engine, the language and the tools work, and the
repository builds a multi-screen reference app,
[Aura Weather](https://github.com/Briany4717/byard/tree/main/examples/aura_weather).
The language and the Rust APIs will still change before a first release, and
nothing is published to crates.io yet. This is why the guide installs Byard from
a clone.

## How to read this guide

If you are new, read in order. [Getting started](getting-started.md) installs the
tools and runs a first app. [The dev loop](dev-loop.md) covers `byard dev`,
`check` and `shot`, which you will use all day.

The language tour is one chapter per idea, and each starts with a small
complete example you can paste into `main.byd`. After it, theming, the Rust
half, packages and shipping follow, and the last chapter is a reference for
every command.

Every `byld` sample in this guide is a test. The test parses each one and runs
it through the same checks as `byard check`, so a sample that stops working
fails the build. Where a sample is meant to be wrong, to show an error, it says
so, and the error message you see is the one the compiler really prints.

The design documents behind all of this are the
[RFCs](https://github.com/Briany4717/byard/tree/main/docs/rfcs). They are written
for people working on Byard itself. You do not need them to use it.
