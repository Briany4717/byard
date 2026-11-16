# Releasing

What it takes to publish a version of Byard. The steps that need a crates.io
token or push a tag are done by hand.

## Before

- `main` is green on all three platforms.
- Nothing waits on swash. Its variation-coordinate leak (dfrg/swash#149) is
  worked around in Byard's own code, so a published `byard` draws variable
  fonts correctly with whatever swash crates.io resolves. Watch the canary,
  `cargo test -p byard-core swash_still_leaks_variation_coordinates`. The
  `swash canary` workflow runs it every Monday against the newest swash on
  crates.io: when it fails, swash has fixed the leak, and its message says what
  to remove.
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

## VS Code extension

The extension in `editors/vscode` is published on its own tag, `vscode-v<version>`,
to the VS Code Marketplace and to Open VSX, as the publisher `byard`. CI builds
and packages it, so no one needs npm to release.

One time, by hand:

1. Create the publisher `byard` at marketplace.visualstudio.com/manage, and an
   Azure DevOps personal access token with the scope Marketplace, Manage
   (organization: all accessible organizations). Add it to the repository
   secrets as `VSCE_PAT`.
2. Sign in at open-vsx.org, create an access token and run
   `npx ovsx create-namespace byard -p <token>`. Add the token as `OVSX_PAT`.

A step whose secret is missing is skipped with a warning, so the workflow can
be tried with one registry first.

A release:

1. Set `version` in `editors/vscode/package.json` (and in its
   `package-lock.json`) and add the version to `editors/vscode/CHANGELOG.md`.
2. Merge, then tag and push: `git tag vscode-v0.1.0 && git push origin vscode-v0.1.0`.

The workflow refuses a tag that does not match `package.json`, packages one
`.vsix`, and publishes that file to both registries. The `.vsix` is also kept as
a workflow artifact.
