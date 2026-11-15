# Releasing

What it takes to publish a version of Byard. The steps that need a crates.io
token or push a tag are done by hand.

## Before

- `main` is green on all three platforms.
- Nothing waits on swash. Its variation-coordinate leak (dfrg/swash#149) is
  worked around in Byard's own code, so a published `byard` draws variable
  fonts correctly with whatever swash crates.io resolves. Watch the canary,
  `cargo test -p byard-core swash_still_leaks_variation_coordinates`: when it
  fails, swash has fixed the leak, and its message says what to remove.
- The workspace version in `Cargo.toml` (`[workspace.package]` and the
  internal crates in `[workspace.dependencies]`) is the one being released.
- `CHANGELOG.md` has the version and its date under `[Unreleased]`, and the
  links at the bottom point at the new tag.
- Every crate packages and builds from its archive:

  ```sh
  cargo package --locked -p byard-macro -p byard-core -p byard-compiler -p byard-project -p byard-platform -p byard-cli -p byard -p byld-lsp
  ```

  CI runs the same with `--no-verify`.

## Publish

Dependencies first. Each one has to be on crates.io before the next can
resolve it:

```sh
cargo login
cargo publish -p byard-macro
cargo publish -p byard-core
cargo publish -p byard-compiler
cargo publish -p byard-project
cargo publish -p byard-platform
cargo publish -p byard-cli
cargo publish -p byard
cargo publish -p byld-lsp
```

Then tag and push:

```sh
git tag -a v0.1.0 -m "Byard 0.1.0"
git push origin v0.1.0
```

and make the GitHub release from the tag, with that version's CHANGELOG
section as its notes.

## After

`cargo install byard` on a clean machine, `byard new hello`, `cd hello`,
`byard dev`. That is the path the README promises, so it is the one to try.
