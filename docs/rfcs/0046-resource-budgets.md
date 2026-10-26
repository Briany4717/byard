# RFC-0046: Resource budgets in CI

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-10-24
- **Last updated:** 2026-10-24
- **Depends on:** RFC-0013 (telemetry), RFC-0033 (the instance arena), the
  frame budget in `crates/byard-platform/tests/it/frame_budget.rs`.

> **Placement:** `docs/rfcs/0046-resource-budgets.md`

---

## Summary

Four numbers get a ceiling that CI enforces, the way `frame_budget.rs`
already holds allocations per frame: how often an idle app wakes its logic
thread, how long a headless start takes to its first frame, how large a
stripped release binary is, and how fast the engine's benchmarks run. A
change that moves one past its ceiling fails CI; a change that has a good
reason to raise one says so in the file that holds it.

## Motivation

Byard is meant to be the framework that spends least: least CPU, least
memory, least disk, fastest to start. Today only part of that is checked.

- #336 found the logic thread ticking about 2,400 times a second in a hidden
  window. Nothing failed; it was seen by hand in a trace.
- The accessibility bridge (#341) would have added 19% to every binary. It
  was caught because someone measured it, not because anything had to.
- `crates/byard-core/benches` measure the atlas, the evaluator and spatial
  queries, and nothing compares their numbers between commits.

A budget nobody checks is a wish. The frame budget works because it fails.

## Guide-level explanation

`ci/budgets.toml` holds the ceilings:

```toml
# Raise a ceiling only on purpose, and say why in the PR.
[idle]
logic_wakes_per_second = 0        # an app with nothing animating

[startup]
first_frame_ms = 1500             # headless, Aura Weather, debug build

[binary]
aura_weather_stripped_bytes = 22_500_000

[bench]
regression_percent = 10
```

Each is read by a test or a CI step that prints the measured value next to
the ceiling every run, so the margin is visible before it is gone.

## Reference-level explanation

### 1. Idle wakes

A test drives `examples/aura_weather` (and the `todo` example) headlessly
past its first frames and its first data, then counts logic-thread wakes over
one real second with nothing animating and nothing focused. The ceiling is
0. A focused text field's caret blink (two wakes a second, RFC-0041) is
measured separately against its own existing assertion.

### 2. First frame

A test times `Shot` from `App::new` to the first encoded frame for Aura
Weather with its HTTP answered from fixtures (the app's own test setup), in
the build the test suite uses. The ceiling is generous on purpose, about
three times what a CI runner measures today, because shared runners are
noisy; it exists to catch a doubling, not a 5% drift. The measured time is
printed every run.

### 3. Binary size

A job on `ubuntu-latest` builds `aura-weather` in release, strips a copy, and
compares its size with the ceiling. It runs on pull requests that touch
`Cargo.toml`, `Cargo.lock` or any `crates/` source, and on `main`. The
ceiling starts at today's size plus 5%.

### 4. Benchmarks

The criterion benches already in `byard-core` run on `main` pushes only, with
results cached as the baseline; a pull request runs them against that
baseline and fails past `regression_percent`. Shared runners make
microbenchmarks noisy, so this is the one budget that only warns (a CI
annotation) until a dedicated runner exists. Resolved in Q2.

## Cost

CI time only: one release build (about four minutes, cached between runs)
and two short tests. Nothing in the engine changes.

## Drawbacks

Ceilings with margins can still let a slow creep through. The printed
numbers are what catch that, and only if someone reads them.

## Rationale and alternatives

- **A dashboard of numbers over time** shows creep better but fails nothing.
  It can be added later from the same printed values.
- **Tight ceilings** would flake on shared runners and get raised by habit,
  which is worse than generous ones that hold.

## Resolved questions

### Q1, which app to measure

**Resolution:** Aura Weather. It is the reference app, it uses fonts, images,
HTTP, a native view and navigation, and its tests already answer its HTTP
from fixtures.

### Q2, whether benchmark regressions fail CI

**Options.** (a) fail; (b) warn. **Resolution: (b) until there is a runner
whose timings are stable.** A gate that fails at random gets ignored, and then
it guards nothing.

## Acceptance

- `ci/budgets.toml` exists and every ceiling in it is read by something.
- Each check fails when its ceiling is set below the measured value (shown in
  the PR by doing it once).
- CI prints every measured value next to its ceiling.

## Future possibilities

Memory high-water mark per frame; the same budgets for the CLI's `check` on a
large project; a dedicated benchmark runner.
