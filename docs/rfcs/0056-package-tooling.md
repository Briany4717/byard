# RFC-0056: Package tooling, held presses in `shot`, and a lighter lock

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0008 (packages, the lockfile, registries), RFC-0041 §7
  (`byard shot` input steps), RFC-0023 (ripple), RFC-0046 (resource budgets).

> **Placement:** `docs/rfcs/0056-package-tooling.md`

---

## Summary

Four small changes to the tools around packages and pictures, found while
building the Material package, and one to a test:

1. `byard shot --hold X,Y` presses the pointer and keeps it down for the
   picture, and `--hover X,Y` moves it there without a press, so `on pressed`,
   a press morph, a ripple and `on hover` can be seen.
2. `byard.lock` no longer pins a `path` dependency by checksum. A path
   dependency is the files on disk now; there is nothing to go stale.
3. A git dependency is cached as the files `byard publish` would archive, not
   as the whole clone.
4. A directory registry is written into the manifest and the lock relative to
   the project, so the lock is the same on every machine.
5. The Stockroom timing test is `#[ignore]`d from the normal run and run by
   name in CI's budgets step.

None of it adds syntax or cost to a program that does not use it.

## Motivation

**Pictures of a held press.** `--tap` presses and releases in one frame, so
`on pressed`, the morph and the ripple are over before the picture. The Material
spike worked round it with a `preview:` parameter on every component, three
extra names in a public API only so a picture could be taken, and the real
pointer path stayed untested.

**A stale pin.** `byard.lock` pins a `path` dependency by content checksum, and
only `byard get` writes it. Edit the dependency and `byard check` stays green on
the old hash; run `byard get` and the lock changes. In a package repository
with a `gallery/` app that depends on `..`, that is a lock diff on every commit,
and a committed lock that is wrong the moment it is committed.

**A whole repository in the cache.** A git dependency is a full clone:
`gallery/`, `.github/`, fonts the package does not declare. For Material that is
15 MB where the published archive is 4.5 MB compressed.

**A lock that names `/tmp`.** `byard add material --registry /tmp/x/registry`
writes that absolute path into `byard.toml`, and the lock repeats it:
`source = "registry+/tmp/x/registry#material"`, a place that exists on one
machine.

**A flaky test.** `examples/inventory` has a timed budget test that fails under
load (411 ms against 400 ms) and blocks unrelated work.

## Guide-level explanation

```
byard shot examples/ripple --hold 200,80 --at 120 -o pressed.png
byard shot examples/ripple --hover 200,80 -o hovered.png
```

`--hold` moves the pointer to the point and presses it, and does not release.
The app then runs on to `--at` with the button down, so a ripple has spread for
that many milliseconds and a press morph has settled. `--hover` moves the
pointer there with nothing pressed. Both go in the order they are written with
`--tap`, `--key` and `--type`. A `--hold` is never released; to draw a release
afterwards use `--tap` elsewhere (it presses again) or leave it, since a
picture is the last frame.

`byard.lock` lists a path dependency without a checksum:

```toml
[[package]]
name = "material"
source = "path+.."
```

Edit the package and `check` builds the edited files, as it always did, because
a path dependency was never read from the lock. Nothing to regenerate, nothing
to commit.

`byard add pkg --registry ../registry` writes `{ registry = "../registry", ... }`
relative to the project whatever the shell's directory was, and an absolute path
is turned into one. If a manifest already names a directory registry by an
absolute path, `byard get` says the lock will only work on this machine and how
to fix it. An `https://` registry is written as given, for example
`byard add material --registry https://briany4717.github.io/byard-registry`,
with or without a trailing slash, and with a path under the host.

## Reference-level explanation

### 1. `--hold` and `--hover`

`Step` gains `Hold(f32, f32)` and `Hover(f32, f32)`. `Hold` is one frame of
events: `PointerMove` then `PointerDown` at the point. `Hover` is one frame with
`PointerMove`. A real pointer moves before it presses, and hover state is driven
by the move. Neither produces `PointerUp`. The steps are ordered with the other
flags by clap's recorded indices, as `--tap` is. The time flag is the existing
`--at`, and the frames after the last step run with the pointer where it was, so
the result is deterministic: the clock is the 16 ms step the command already
uses.

### 2. Path dependencies are not pinned

A `LockedPackage` for a path source has an empty `checksum`. `Lockfile::write`
omits an empty checksum and `Lockfile::read` accepts its absence for a `path+`
source only; a git or registry entry without one is still an error. An old lock
that has the checksum is read as before and the checksum is ignored for a path
source; the next `byard get` rewrites the entry without it.

The lock keeps the entry (`name`, `source`), so the set of packages is still
visible in review and a changed `source` still shows in the diff.

### 3. The git cache holds the archive's files

`fetch_git` clones into `<cache entry>.partial`, then copies exactly the files
of `publishable_files` (the function `byard publish` archives from) into a
staging directory, removes the clone, and renames the staging directory to the
cache entry. The commit the ref resolved to is written beside it, in
`<cache entry>.commit`, since the copy is not a repository. The checksum is
`package_checksum` over those files, the same function that hashes a path
source and a registry archive's unpacked files, so the same content has one
checksum from all three. A cache entry made by an older `byard`, a whole clone,
is still valid: it has `.git`, so its commit is read from it, and its checksum
is unchanged.

### 4. Portable directory registries

`byard add --registry <dir>` resolves `<dir>` against the shell's directory,
then writes it relative to the project root (using `..` as needed). Only a path
on another root, such as another drive, is left absolute. `byard get` warns,
naming the dependency, when a dependency's registry is an absolute directory,
and says to use a relative path or a URL. The lock source already uses the
registry as the manifest wrote it (`registry+../registry#material`), so with a
relative manifest entry the lock is the same everywhere. There is no
`[registries]` table in the manifest today and this RFC adds none.

An HTTP registry is `RegistryLocation::Http`, read with `GET <base>/index.toml`
and `GET <base>/<archive>`, over rustls; a trailing slash is dropped. A test
serves a registry on a loopback server under a sub-path with a trailing slash in
the URL, the shape of a GitHub Pages site.

### 5. The Stockroom budget

The timed assertions move to a test marked
`#[ignore = "timed; ..."]`. The counters (signals per keystroke, live scopes)
are measured in the normal run, in-process. CI's Linux budgets step runs the
timed test with `--ignored`.

## Drawbacks

- **A path dependency's content is not pinned.** It never was enforced: `check`
  read the files on disk. The checksum only gave a false sense that it was.
  Anyone who wants an immutable copy uses git or a registry.
- **A shot ends with the button down.** A release cannot be pictured with
  `--hold`; `--tap` already covers press and release.
- **A git dependency is still cloned in full** before it is cut down. Only the
  cache is lighter; the transfer is not.
- **The `.commit` file** is a second thing in the cache per git entry.

## Rationale and alternatives

- **Recompute path checksums in `check` and report a stale lock.** It needs a
  hash of the dependency on every `check`, which reads every file, and it makes
  every edit to the package a stale lock until `byard get` runs. For a package
  with a `gallery/` on `..`, that is a lock diff per commit or a red CI. Not
  pinning is smaller, cheaper (no hashing of path packages in `get`, and nothing
  at all in `check`), and says what is true.
- **Sparse or partial clone for git.** `--filter=blob:none --sparse` would also
  cut the transfer, but the file list depends on the manifest (declared assets),
  which is inside the repository, so it takes two fetches and depends on server
  support. Possible later.
- **A `.byard-commit` file inside the cache directory.** It would be a file that
  is not in the archive, breaking "the same files as publish". Beside it keeps
  the directory exact.
- **A `[registries]` table naming registries.** A good idea once there is more
  than one hosted registry; today a relative path or a URL is written once per
  dependency and nothing needs a name.
- **`--hold` with a duration.** `--at` already is the duration.

## Prior art

Cargo's path dependencies are not checksummed in `Cargo.lock` either, and its
git sources are checked out from a database, not kept whole. Cargo's registry
config accepts `sparse+https://` URLs; Byard's already reads a static site.

## Unresolved questions

- Should `--hold` be able to release (`--release`) so the end of a press can be
  drawn? Nothing needs it yet.
- Should `byard get` offer to rewrite an absolute registry path in the manifest,
  rather than warn?
