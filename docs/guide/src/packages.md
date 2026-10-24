# Packages

A package is a folder of views, and maybe a theme and some fonts, that other projects use. A
design system is a package. So is a set of widgets shared between two of your own apps.

This is a package, `kit`, with one view:

```text
kit/
  byard.toml
  src/lib.byd
```

```toml
[package]
name = "kit"
version = "0.1.0"
```

```byld
// src/lib.byd: a view with a parameter, for other projects to use.
View Card(label: Str) {
    Box #[bg: 0x262636, radius: 12, p: 14, border: 0x3C3C52] {
        Text(label) #[color: 0xE6E1E5, size: 15]
    }
}
```

An app uses it by declaring it and naming it:

```toml
[project]
name = "app"
entry = "src/main.byd"

[dependencies]
kit = { path = "../kit" }
```

```byld,ignore
// Needs the `kit` dependency from byard.toml above, which a lone sample cannot have.
use kit as k

View Main() {
    Column #[bg: 0x14141C, p: 24, gap: 12] {
        k.Card("hello from the kit package")
        Sidebar()
    }
}

View Sidebar() {
    Text("a view from this project, no prefix") #[color: 0x9AA0AC]
}
```

## What is in a package

A package's `byard.toml` has a `[package]` table with a `name` and, to be published, a
`version`. It has no `[project]` and no entry, because nothing runs it. Its views are in `src/`,
or in the folder itself when there is no `src/`. A package can also declare:

- `[theme]`, a palette, type scale and shapes that an app can build on,
- `[assets.fonts]`, typefaces, as paths inside the package,
- `[dependencies]`, packages of its own,
- a README and a licence file, which are published with it.

A package is one namespace: every view in all its files is visible to the others by bare name. Your
own project works the same way. A package's assets must live inside it, because it is fetched and
published as one directory, and a path that climbed out would name a file on the author's machine and
nowhere else.

To check a package alone, run `byard check` inside it. It checks the files an app would get, with its
fonts and theme named as that app sees them. `byard dev`, `shot` and `build` there say it is a library
and point you at `check`.

## Using one

A dependency is declared in `[dependencies]`, by where it comes from:

```toml
[dependencies]
kit = { path = "../kit" }
material = { git = "https://example.com/material.git", tag = "v0.1.0" }
brand = { registry = "https://packages.example.com", version = "^0.1" }
```

- **`path`** is a folder next to yours, relative to the manifest that names it. It is the right
  choice while you build a package and its app together, and `byard dev` watches it, so an edit in the
  package reloads the app.
- **`git`** with a `tag` or `rev` is a repository pinned to an exact ref. A floating branch would not
  be reproducible, so there is none.
- **`registry`** with a `version` is a published package. The registry is a folder, or the same layout
  served over HTTP. The version is a requirement with Cargo's rules: `"0.3"` and `"^0.3"` accept any
  compatible `0.3.x`, `"~1.2"` any `1.2.x`, `"=1.2.3"` only that one, and `">=1.2, <1.5"` a range.

A bare version string says nothing about where to get the package, so it is an error, and so is a
requirement that names a pre-release, because pre-releases are never chosen.

Then `use` it in a `.byd` file, before any view:

| Write | Then |
|-------|------|
| `use kit as k` | `k.Card(…)` |
| `use kit` | `kit.Card(…)` |
| `use kit.{Card}` | `Card(…)`, while no other `Card` exists |

Functions a package declares are used the same way: `k.describe(…)`. If two names collide, the
checker asks for an alias. Your own project's views never need a prefix.

### Fonts and themes from a package

A package's fonts are named `package/Family`, whatever the app calls the dependency:

```byld,ignore
// Needs a `brand` package that ships a font named Display.
use brand as b

View Main() {
    Text("headline") #[font: "brand/Display", size: 34]
}
```

and an app takes a package's whole theme with `extends`:

```toml
[dependencies]
brand = { path = "../brand" }

[theme]
extends = "brand"
```

The palette, type scale, shapes and typefaces all arrive. See
[theming](theming.md). The `package_theme` example shows a package that declares only a seed colour
and one font, and an app that has no colours or fonts of its own.

## `byard add`

```sh
byard add kit --path ../kit
byard add material --git https://example.com/material.git --tag v0.1.0
byard add brand --registry https://packages.example.com
byard add brand --registry ./registry --version "^0.1"
```

`add` writes the entry into `[dependencies]`, keeping the rest of the file as it was, and then runs
`byard get`. `install` is another name for the same command. Choose one source: `--path`, `--git`
(with `--tag` or `--rev`), or `--registry` (with an optional `--version`, which defaults to `^` the
newest version published). Mixing sources is an error.

## `byard get` and the lockfile

`byard get` makes the dependencies real. It fetches every package, picks one version for each registry
requirement, and writes `byard.lock`. It is the only command that writes the lockfile.

```toml
# Generated by `byard get`, do not edit by hand.
# Commit this file: it pins every dependency to exact content.
version = 1

[[package]]
name = "brand"
source = "registry+https://packages.example.com#brand"
version = "0.1.2"
checksum = "sha256:3f1c9a0d5e7b…"

[[package]]
name = "kit"
source = "path+../kit"
checksum = "sha256:9be2d4417a30…"
```

Each entry pins a package to exact *content*: a checksum over its sources. A git package also pins the
commit its tag resolved to, and a registry package the version chosen. Commit `byard.lock`. With it,
every machine builds the same program.

- `check` and `dev` build from the lock. They never solve, and never contact a registry.
- Git packages are fetched into a cache, `~/.byard/cache`, and are not fetched again once cached.
- When the lock already satisfies the manifest, `get` keeps it and does not contact the registry. When
  it does not, `get` solves again.
- When no version can satisfy every requirement, `get` says which requirements conflict and lists the
  versions the registry has.
- A package that is declared but not fetched is reported with the hint to run `byard get`. After
  cloning a project, run it once.

## `byard publish`

`byard publish` puts a package in a registry:

```sh
byard publish ../kit --registry ./registry
BYARD_REGISTRY=./registry byard publish
```

It needs `name` and `version` in `[package]`. It writes an archive of what the package is made of, its
manifest, sources, declared assets, README and licence, as `registry/kit/0.1.0.tar.gz`, and adds a line
to the registry's `index.toml` naming the archive, its hash and its dependencies.

- **Versions are immutable.** Publishing the same content again does nothing. Publishing different
  content under a version that exists is an error. Bump `version`.
- **A published package may depend only on registry packages.** A path or git dependency means
  nothing to someone who installs from a registry, so `publish` refuses it. A dependency on another
  registry is allowed only by its URL.
- **Assets stay inside.** A theme's seed image is published with the package, and has to live in it.

A registry is just files. Any static file server pointed at the folder `publish` wrote is a registry,
so `python3 -m http.server --directory registry` serves one, and a dependency can then name it:
`{ registry = "http://127.0.0.1:8765", version = "0.1" }`. `byard get` then picks the highest `0.1.x`.

## Shipping packages inside an app

An app that uses packages can be shipped to a machine with no cache and no `../` paths. `byard bundle`
copies each package into the bundle and points every manifest at the copy. See
[shipping](shipping.md).
