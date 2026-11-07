# RFC-0008 multi-file / package example

A tiny two-package workspace showing user-view instantiation (RFC-0007) across
files and packages (RFC-0008):

- `app/`, the project. Its `src/main.byd` uses a sibling file (`widgets.byd`,
  same project namespace) and a `path` dependency `kit` via `use kit as k`.
- `kit/`, a local package exposing `Card` and `Button`.

It also shows what RFC-0055 added to the language:

- a constant, `let gap = 8` in `kit/src/tokens.byd`, used by the package and
  by the app as `k.gap`; the app has one of its own, `limit`;
- an enum, `Variant`, as the parameter of `k.Button`, written `k.Variant.outlined`
  or, where the parameter says which enum it is, `variant: .tonal`;
- private names: `kit`'s `fill` function and `Caption` view are used inside the
  package and cannot be reached from the app (`k.Caption(…)` is `PrivateName`);
- `else when`, in the app's `Inbox` view, one arm per case.

Check the whole module graph (from inside `app/`):

```
cd crates/byard-cli/examples/workspace/app
cargo run -p byard-cli -- check          # → 5 file(s), 1 package(s); ok (0 errors)
```

Or point the CLI at the project directory or its manifest:

```
cargo run -p byard-cli -- check crates/byard-cli/examples/workspace/app
```

Draw it to a PNG without a window:

```
cargo run -p byard-cli -- shot crates/byard-cli/examples/workspace/app --size 440x420 -o workspace.png
```

The package alone checks too, private names included:

```
cargo run -p byard-cli -- check crates/byard-cli/examples/workspace/kit
```
