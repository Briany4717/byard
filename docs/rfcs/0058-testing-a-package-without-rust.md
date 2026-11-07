# RFC-0058: Testing a package without Rust

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0041 (`byard shot` presses keys and types, §7), RFC-0042
  (the accessibility tree and its text dump, §6), RFC-0056 (`--hold` and
  `--hover`), RFC-0008 (packages), RFC-0056 §package tooling (what `shot` and
  `check` do in a package).

> **Placement:** `docs/rfcs/0058-testing-a-package-without-rust.md`

---

## Summary

`byard shot` gets `--view <Name>`, to draw any parameterless view of the project
instead of `Main`, and `--wait <MS>`, a step that lets app time pass between
inputs. A new command, `byard test`, runs the scenario files `tests/*.toml` of a
project: each scenario names a view, drives it with the same inputs `shot` takes
(tap, hold, hover, key, type, wait) and checks the accessibility tree, line by
line, after any step. It never needs a GPU, and with `--shots <DIR>` it also
writes each scenario's final picture when there is one. A package written only
in Byld can now be drawn, driven and checked by its own CI on Linux, macOS and
Windows with no Rust toolchain.

## Motivation

A package of components, such as Byard Material, lives in its own repository
and is shown by a `gallery/` app that depends on it by path, with one page per
component. Its CI has three things to do and the engine does none of them:

- **Draw one page at a time.** `byard shot` draws `Main` and nothing else. The
  gallery would need a `Main` that picks its page from something a script can
  set, or one project per page. The views are already there, named.
- **Drive a component.** `shot --tap 120,300` presses a point, but the next
  check is a human looking at a PNG. Nothing says "after the tap, the switch is
  on".
- **Do it on three systems, in Byld.** The checks that exist for the engine are
  Rust tests that run the binary. A package author does not have, and should not
  need, a Rust toolchain. And most CI runners have no GPU, which is no reason
  not to know that a `Switch` named "Wi-Fi" reads `on` after a tap.

The accessibility tree (RFC-0042) already answers "what is on screen, what is it
called, what state is it in" as text, with no GPU. It is the right thing to
assert on: it is what a screen reader would be told, so a test written against
it protects users, and it does not change when a colour does.

## Guide-level explanation

### Drawing a named view

```text
byard shot gallery --view Switches --dark -o switches-dark.png
```

draws the project's `Switches` view instead of `Main`. It works in an app that
depends on packages, the manifest being read as always. A name that is not a
view is an error that names the closest ones; a view that needs arguments
cannot be drawn alone and the error says which:

```text
error: no view `Swithces` in this project; closest: Switches, SwitchList
error: view `Card` has required parameters (title, body), so it cannot be drawn
       on its own; give them defaults or draw a view that has none
```

### Testing

A project's tests are the files `tests/*.toml`. This one is `tests/switches.toml`:

```toml
view = "Switches"          # the default for every test in this file
size = "440x880"

[[test]]
name = "a tap turns the switch on"
steps = [
    { tap = 'Switch "Wi-Fi"' },
    { expect = ['Switch "Wi-Fi" = on'] },
]
expect = ['Label "1 on"']
not = ['Label "0 on"']

[[test]]
name = "space does the same from the keyboard"
scheme = "dark"
steps = [
    { key = "Tab" },
    { key = "Space" },
]
exact = ['Switch "Wi-Fi" = on [focusable focused]']
```

```text
$ byard test gallery
running 2 tests
test switches::a tap turns the switch on ... ok
test switches::space does the same from the keyboard ... FAILED

failures:

---- switches::space does the same from the keyboard ----
after the last step (key Space):
  expected a line equal to
    Switch "Wi-Fi" = on [focusable focused]
the tree was:
    Label "Switches" @ 24,24 392x24
    Switch "Wi-Fi" = off [focusable] @ 24,64 52x30
    ...

test result: FAILED. 1 passed; 1 failed; 0 filtered out
```

The exit status is 0 when every test passes and 1 otherwise.

`byard test gallery --filter switch` runs the tests whose name contains the
text. `byard test gallery --shots pictures/` also writes each test's last
frame to `pictures/switches-a-tap-turns-the-switch-on.png`, in the scheme the
test says; with no GPU the tests run and the pictures are skipped with one note.

## Reference-level explanation

### `byard shot --view <Name>`

`Name` is looked up among the views of the whole resolved program, the
project's and its packages' that are in scope, in place of `root_view`. When it
is absent the root is chosen as before. The check `shot` already runs over the
whole program is unchanged and runs first.

Two refusals, both before anything is drawn:

- **Unknown name.** The message names up to five existing views, nearest by edit
  distance first (RFC-0002 D4's measure), whether or not any is "close enough":
  an author with a typo and an author who forgot the name both get a list.
- **Required parameters.** A parameter with no default makes the view
  undrawable alone. The message lists them. A `bind` parameter without a
  default is required.

`--wait <MS>` is a step like `--tap`: in command-line order it lets the stated
milliseconds of app time pass, a frame (16 ms) at a time, as `--at` does at the
end. `byard dev --view` is not part of this: the dev runner's reload path
compares the root against every changed view in several places, and choosing
another root there is its own change (see Future possibilities).

### Scenario files

`tests/*.toml`, read in file-name order; each test in a file in written order.
Nothing in a project without a `tests/` directory is touched.

A file's top-level keys are defaults for its tests; a test may set any of them
itself:

| Key | Type | Default | |
|-----|------|---------|-|
| `view` | string | `Main` | The view to draw, as `shot --view`. |
| `size` | `"WxH"` | `"440x880"` | Logical size. |
| `scheme` | `"light"` or `"dark"` | the manifest's | The colour scheme. |
| `at` | integer, ms | `0` | App time to have passed before the last expectations. |
| `now` | ISO string | the real clock | Fixes `now()`, as `shot --now`. |

`at` defaults to `0` and not to `shot`'s 1500: a test that needs motion to
finish says `{ wait = 400 }` where it matters, so it is also the place a reader
sees that the result depends on time. (`shot`'s default exists because a picture
of a half-finished entrance is never wanted.)

A test has a `name` (required, unique in its file) and the optional `steps`, an
ordered array. Each step is a table with exactly one of these keys:

| Step | Value | Does |
|------|-------|------|
| `tap` | `[x, y]` or a *line* | Presses and releases at the point. |
| `hold` | `[x, y]` or a line | Presses and keeps the pointer down. |
| `hover` | `[x, y]` or a line | Moves the pointer there, pressing nothing. |
| `key` | chord string | `Left`, `shift+Left`, `mod+a`, the chords of `--key`. |
| `type` | string | Types the text. |
| `wait` | integer, ms | Lets app time pass. |

or any of the expectation keys below, which makes it a *check*. A line given to
`tap`, `hold` or `hover` is a selector: the pointer goes to the centre of the
node whose line in the current tree contains the text. It is an error if no
line contains it or if more than one does; the message lists the lines. Points
are for what the tree cannot name, and break when a layout moves; a line is
what the test means.

### Expectations

The tree is the text `byard shot --a11y` writes: one node a line, indented by
depth, ending in ` @ x,y WxH`. A *line*, for the purpose of a check, is that
text with its indentation removed. Four keys, each an array of strings (a lone
string is accepted for one):

| Key | Passes when |
|-----|-------------|
| `expect` | each string is a substring of some line |
| `exact` | each string equals some line, with the trailing ` @ x,y WxH` removed |
| `not` | no line contains any of the strings |
| `not_exact` | no line equals any of the strings, with the trailing bounds removed |

Bounds are left out of `exact` because they change when anything above moves;
`expect` is the form that can include them (`'Button "Save" [focusable] @ 24,124'`).

Expectation keys written on the test itself are checked after the last step,
and after `at` has passed, as if they were a final check step. A check step
runs on the frame the previous step produced after one more frame with no input,
so a late-landing text is in. A test with no steps and some expectations checks
the first frame, which is what a gallery page's "it shows" test is.

A test with no expectation anywhere is an error: a test that cannot fail is not
a test, and the silent no-op is the failure mode this command exists to avoid.

### Running

All scenario files are parsed and validated before the first test runs: an
unknown key, a step with two actions, a bad chord, a duplicate name, an unknown
or parametered view, or a malformed size is reported with the file and the test
(and the step), all of them at once, and nothing runs. They are mistakes in the
test, not failures of the program.

The project is loaded and checked once: the manifest, the packages, the same
`check` as `shot`, whose errors stop the run. Each test then builds its own
interpreter, so tests share no state, and runs without a window, a GPU or the
network. The app is a project with a window to draw (a library package is
refused, as for `shot`), and a gallery is such a project.

Output is `cargo test`'s: `running N tests`, a line `test <file>::<name> ... ok`
or `FAILED` for each, then a `failures:` section for each failed test, then
`test result: ok. N passed; M failed; K filtered out`. A failure names the
test, the step (`step 3 (a check, after tap "Switch")`, `step 3 (tap "Switch")
could not run`, or `after the last step (key Space)`), the
unmet expectation and the whole tree at that point. The first failing check
ends the test; all the unmet strings of that check are listed.

`--filter TEXT` keeps the tests whose `<file>::<name>` contains `TEXT`. A filter
that keeps nothing is an error, as is a project with no `tests/*.toml`.

`PATH` is a project directory or its `byard.toml`, defaulting to the current
project. A bare `.byd` path is refused with a message saying so: it has no
`tests/`, and (as for `shot`) a path to a `.byd` file reads no manifest, so none
of its packages. Name the project.

### `--shots <DIR>`

After a test passes (and after it fails, too, since the picture of the failure
is the useful one), its final frame is drawn at scale 2 in the test's scheme and
written to `<DIR>/<file>-<name>.png`, the name lowercased with runs of
characters other than letters and digits turned into `-`. The GPU is opened once,
on the first picture. If there is no adapter, one line says pictures are skipped
and the rest runs; the exit status is not affected. The directory is created.
Without `--shots` nothing about the GPU is touched.

### Cost

Nothing runs when unused: `--view` is one lookup, `--wait` one more step kind,
`test` a subcommand. The tree is requested on every frame of a test (a flag the
platform sets for a screen reader on the same frames); a test does not draw.

## Drawbacks

- **A second file format**, TOML, next to Byld. Tests in Byld would need
  language support for asserting on a tree and for stepping the app, which is a
  much larger change. TOML is already the manifest's language and a gallery
  author knows it.
- **The tree is text.** An assertion on a colour or a pixel cannot be written.
  That is what `--shots` is for (a person, or a golden-image tool of the
  author's, looks); the tree is what can be asserted on every machine.
- **No GPU means no render checks**, such as "this does not draw outside its
  box". They stay with the engine's own tests.
- **Selectors are substrings**, so a tree where two nodes read alike needs more
  text in the selector. The error for that lists the lines, which is the fix.

## Rationale and alternatives

- **A `Main` in the gallery that reads its page from the environment.** It puts
  test plumbing in the shipped app and needs a way to set the environment on all
  three systems. `--view` is one flag and the gallery stays an app.
- **A project per page.** A manifest, a lock and a dependency line per
  component, for what is a name.
- **Golden trees, as the `accessibility` example does** (a committed `tree.txt`).
  Good for one page of an example, wrong for a package: any added node anywhere
  fails every page. Asserting the lines that matter keeps a test about one
  thing.
- **Tests in Byld** (`test "..." { tap(...) expect(...) }`). The better end
  state, and a language feature: new syntax, a checker, an interpreter mode. TOML
  is deliberately the part that can be dropped when that exists; the runner,
  the steps and the tree format carry over unchanged.
- **JSON or YAML.** The project already reads TOML, and its inline tables are
  the readable way to write a step.
- **Coordinates only for `tap`.** The brief of `shot` is a point, but a test
  written with `tap = [212, 318]` is wrong the day a padding changes, and the
  tree has the node's bounds right there.
- **`at` defaulting to 1500.** See above.

## Prior art

- **`cargo test`** for the output and the exit status.
- **Testing Library and Playwright** assert on the accessibility tree and find
  elements by role and name, for the same reason: it is what users get and does
  not move with styling.
- **Flutter's golden and widget tests** separate "find, tap, expect" (no GPU)
  from pictures (a renderer); so does this.

## Resolved questions

### Q1, do `byard dev` and `byard shot` both get `--view`?

**Resolution:** `shot` only. `dev` keeps its root in several reload paths and
the gallery is looked at with `shot` and `test`; `dev gallery` shows `Main`,
which a gallery makes the index.

### Q2, may an expectation be checked mid-way?

**Resolution:** yes, as a step. It is a table of the same four keys, so there is
nothing more to learn, and the test's own keys are the same check after the
last step.

### Q3, is a test with a failed check stopped?

**Resolution:** yes. Later steps run on a state the author did not expect, and
their failures would bury the first.

### Q4, does a filter that matches nothing pass?

**Resolution:** no, it is an error. A typo in a filter that exits 0 is a CI job
that stopped testing and says it passed.

## Unresolved questions

- Should `scheme = "both"` run a test twice, once per scheme, for the gallery's
  two pictures? Two tests say it today at the price of duplication.

## Future possibilities

- `byard dev --view <Name>`, with the reload path taking its root from the
  options.
- Tests written in Byld, with this runner underneath.
- Selecting by role and name (`Switch "Wi-Fi"`) as well as by line, and
  `focused` as a condition.
- Comparing `--shots` pictures with committed ones.
