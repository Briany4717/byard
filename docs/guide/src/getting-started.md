# Getting started

This chapter installs the tools, makes a project and runs it.

## What you need

- **Rust**, stable, version 1.86 or newer. [rustup](https://rustup.rs) installs
  it.
- **git**, to get the source.
- **A graphics driver** `wgpu` can use: Vulkan on Linux, Metal on macOS,
  Direct3D 12 on Windows.

Byard is not on crates.io yet, so you install it from a clone. This will change
when there is a release.

## Install

```sh
git clone https://github.com/Briany4717/byard
cd byard
cargo install --path crates/byard-cli
```

That builds the `byard` command and puts it in Cargo's bin directory, usually
`~/.cargo/bin`. Check it is there:

```sh
byard --version
```

The first build compiles the whole engine, so it takes a while. After that,
running `cargo install` again only rebuilds what changed.

## Make a project

```sh
byard new hello
```

```text
·    created hello/
·    created hello/byard.toml
·    created hello/main.byd
·    created hello/.gitignore
ok   run `cd hello && byard dev` to start
```

The name may use ASCII letters, digits, `-` and `_`, and cannot start with `.`
or `-`. The command refuses to overwrite a directory that exists.

You now have three files. The first is the manifest, `byard.toml`:

```toml
[project]
name  = "hello"
entry = "main.byd"
```

`name` is the project's name. `entry` is the file that holds the view that
runs. The second is `main.byd`, a starter that counts taps and edits a label:

```byld
// hello, starter view
// Edit and save; the window updates instantly.
View Main() {
    var count = 0
    var label = "hello"

    Column #[gap: 20, p: 32, align: center, justify: center] {
        Text("{label}, tapped {count} times") #[size: 24, color: 0xFFFFFF]

        Button("Tap me") #[bg: 0x3B82F6, radius: 8, p: (vertical: 10, horizontal: 20),
                           color: 0xFFFFFF, weight: bold] => count++

        TextField #[bg: 0x374151, radius: 6, p: (horizontal: 12), height: 36,
                    color: 0xFFFFFF, bind: label]
    }
}
```

The third, `.gitignore`, holds one line, `/target`, for the day you add Rust.

## Run it

```sh
cd hello
byard dev
```

A window opens. Tap the button and the count goes up. Type in the field and the
words at the top change as you type.

Now leave the window open, change `"hello"` to `"hi there"` in `main.byd` and
save. The window updates without closing, and `count` keeps its value. This is
hot reload, and it is how you will spend most of your time. The
[next chapter](dev-loop.md) covers it, and the other two commands you will use
all the time.

## The shape of a project

Small apps are one file. As an app grows, you split it into views, and views
into files. This is the layout the larger examples use:

```text
my_app/
  byard.toml       name, entry view, theme, fonts, dependencies
  src/main.byd     the view named Main is what runs
  src/*.byd        other views, visible to each other without imports
  src/main.rs      only if the app has Rust controllers or native views
```

`byard new` puts `main.byd` beside the manifest and not under `src/`. Both work.
What matters is the `entry` line, and that every `.byd` file under the entry's
directory belongs to the project.

That last part is the rule to remember. Every file in a project shares one
namespace, so a view written in `sidebar.byd` is used from `main.byd` by its name
and nothing else. There is no `import`.

```byld
View Main() {
    Column #[p: 24, gap: 12] {
        Greeting(name: "Ada")
        Greeting(name: "Grace")
    }
}

// In a project, this view could live in greeting.byd.
View Greeting(name: Str) {
    Text("Hello, {name}") #[size: 20]
}
```

Two views with the same name anywhere in the project are an error. Folders whose
name starts with a dot, and folders called `target`, are skipped.

The view named `Main` is the one that runs. If no view is named `Main`, the
first view in the project runs, the entry file's first.

## What else lives in a project

Byard writes a few things you did not ask for, and it is worth knowing which.

- `.byard/` holds caches and generated files. `byard clean` removes them.
  They are safe to delete, and should not be committed.
- `byard.lock` appears when you add a dependency. It pins every package to exact
  content. Commit it. [Packages](packages.md) explains it.
- `assets/` is only a convention. Fonts, the window icon and a theme's seed
  image are named by path in `byard.toml`, so they can live anywhere in the
  project.

## Where next

Learn the dev loop in [the next chapter](dev-loop.md). If you would rather start
with the language, jump to [views and parameters](views-and-parameters.md).
Either way, keep `byard dev` open: every sample in this guide can be pasted into
`main.byd` and seen at once.
