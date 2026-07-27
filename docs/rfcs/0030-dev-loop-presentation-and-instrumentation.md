# RFC-0030: Dev-Loop Presentation & Instrumentation — terminal style, live statusline, real scopes, and the in-window HUD

- **Status:** Active — implemented. Instrumentation (§I1–§I3), the output
  grammar (§P1–§P4), the statusline (§P5–§P6), the expanded block and the frame
  budget (§V1–§V2), the in-window HUD and the reload flash (§V3–§V4, §V6), trace
  export (§V5) and RFC-0006's three commitments (§C1–§C3) have all landed.
  **Two amendments** carry what implementation found:
  [`0030-erratum-statusline-field-set.md`](0030-erratum-statusline-field-set.md)
  (the field set predates the `present.*` scopes and was unreadable as
  specified) and, because §V3 requires the HUD to use no privileged syntax,
  [`0020-erratum-canvas-shape-generators.md`](0020-erratum-canvas-shape-generators.md)
  (a `Canvas` body could not generate shapes from data).
  §V4's acceptance condition — `hud.render` ≤ 5 % of the frame budget — is
  **not met**: it measures ~12 %, dominated by the interpreter's own render
  walk rather than by anything the HUD does. That finding is recorded rather
  than mitigated away, per §V4's own instruction, and the HUD renders its
  verdict on itself.
- **Author(s):** Briany4717
- **Created:** 2026-07-25
- **Last updated:** 2026-07-27
- **Depends on:**
  - RFC-0006 (`byard` CLI and dev runner — every command whose output this RFC re-specifies; §3.4 error overlay; the two promises §"`byard dev`" makes that were never implemented)
  - RFC-0013 (zero-allocation telemetry — `profile_scope!`, `Sample`, `SampleBlock`, `ScopeId`, GPU timestamp queries, **P2**–**P5**; this RFC is the consumer that finally gives it data)
  - RFC-0001 (§5 concurrency and the `frame.rs` boundary — the HUD and the statusline read only what already crosses it; §7.3 Dev/Prod split, which is what the interpreter tax quantifies)
  - RFC-0017 (overlay & z-layer system — the in-window HUD is a layer, not a special case)
  - RFC-0023 (paint effects — `blur` is what makes both the HUD chrome and the error overlay's frosted backdrop possible)
  - RFC-0010 / RFC-0025 (animations — the HUD, the reload flash, and the idle indicator are ordinary animations, not bespoke code)
- **Extends:** `crates/byard-cli` (new `style` and `statusline` modules; every command's output), `byard-core::telemetry` (`Sample` gains `depth`; new `TraceWriter`), `byard-core::engine` (frame counters, `RenderFrame` census accessors), `crates/byard-cli/src/manifest.rs` (`[dev]` table).
- **Enables:** honest, continuously visible frame accounting in `byard dev`; Perfetto/Chrome-Trace export; a self-hosted dev HUD that is simultaneously the project's most demanding dogfooding test.
- **Does not change:** the `byld` grammar, the frame boundary, the arena model, or any subsystem dependency edge. Every number this RFC displays is either already produced or produced by a scope that lives inside the subsystem it measures.

---

## Summary

`byard dev` currently prints eleven lines of unstyled text, then spends the rest
of the session dumping a raw telemetry block to stderr once a second. This RFC
replaces that with a coherent presentation layer and — more importantly —
gives it something true to present.

Three things land together, in dependency order:

1. **Real instrumentation (I1–I3).** `profile_scope!` has exactly one call site
   in the entire engine, and it is inside a `#[cfg(test)]` block in `relay.rs`.
   RFC-0013's machinery is complete, tested, and starved. This RFC instruments
   the six scopes that actually constitute a frame, and fixes the double-count
   that nested scopes would otherwise introduce in `format_telemetry_overlay`.
2. **A presentation layer (P1–P6).** A dependency-free `style` module (role-based
   palette over the terminal's own 16 colours, `NO_COLOR`-aware, degrading to a
   zero-width no-op palette when not a TTY), one output grammar shared by all
   seven commands, and a **single-line statusline** anchored to the bottom of the
   terminal that redraws in place while the log scrolls above it.
3. **Two views onto the same data (V1–V4).** An expanded `--profile` block that
   charts each scope against the frame budget, and an **in-window HUD written in
   `byld`** and drawn by the engine itself — which is both the most useful
   surface and the most honest stress test the project can run against its own
   pitch.

Alongside these, three commitments RFC-0006 already made and never shipped are
closed: the `⟳ reload pending` indicator, the blurred last-good backdrop behind
the error overlay, and caret-anchored source context in diagnostics (the code for
which exists in `check.rs` under `#[allow(dead_code)]`).

The design constraint throughout: **decoration must not cost, and must not
lie.** Every element here is either free (reading an `AtomicBool` the runtime
already maintains), amortised (one write per second), or measured and subtracted
from the number it would otherwise distort (the HUD).

---

## Motivation

### The tool does not look like what it claims to be

Byard's thesis is performance as a structural floor, not an aspiration. The tool
a developer actually touches — `byard dev` — communicates none of that. It
prints:

```
  Byard 0.0.0 — dev mode
  Entry: main.byd
  Watching for changes…
  Loaded (12 views, 0 errors)
```

No timing, no adapter, no frame accounting, no indication that anything is
running. A framework whose entire argument is *measured* cost should not present
a developer loop that measures nothing on screen. This is not a cosmetic
complaint: the dev runner is the only place the interpreter tax is observable,
and RFC-0013 exists precisely so a developer can read off what the AOT build
will cost. That reading is currently unavailable in practice.

### The telemetry overlay is starved

`grep -rn 'profile_scope!(' crates/` returns one match outside `telemetry.rs`
itself, and it is in a test:

```rust
// crates/byard-core/src/relay.rs:532
crate::profile_scope!("relay.test.publish_drains_telemetry");
```

The GPU side is real — `encoder/gpu_timer.rs` interns proper `ScopeKind::Gpu`
scopes and resolves them asynchronously two frames later, exactly as RFC-0013
§"GPU timing" specifies. The CPU side does not exist. So
`format_telemetry_overlay` prints a header, a GPU row or two, and nothing else.
The `interpreter_tax_ns()` accessor returns zero. The whole `Interpreter` /
`Native` distinction — the single most valuable thing RFC-0013 designed — is
inert.

Every presentation improvement below is cosmetics over an empty set until this is
fixed. That is why instrumentation is **§1** of the reference section and not an
appendix.

### The overlay's output shape is actively harmful

`App::print_telemetry_overlay` calls `eprint!` on a multi-line block once per
second, forever. In a five-minute session that is 300 blocks of scrolling text.
Its practical effect is to **bury parse errors** — the one thing a developer
actually needs to read — under a wall of timing noise. Continuous metrics and
discrete events are different media and must not share a scroll region.

### The output grammar is inconsistent

Seven commands, four conventions:

| Command | Current shape |
|---|---|
| `new` | `  Created my_app/` (two-space indent, past tense) |
| `dev` | `  Byard 0.0.0 — dev mode` (two-space, banner style) |
| `check` | `Checking main.byd…` (zero indent, present participle) |
| `get` | `Resolving dependencies of \`my_app\`…` (zero indent) |
| `build` | `  Byard 0.0.0 — build (AOT vector atlas)` (two-space) |
| `clean` | `  removed .byard/generated` (two-space, lowercase) |
| `add` | ``  `material` resolved via the built-in index → …`` (two-space) |

There is no colour anywhere in the workspace: no `owo-colors`, no `termcolor`,
no hand-rolled escape. Nothing distinguishes an error from an informational line
except the reader's attention.

### RFC-0006 wrote cheques the implementation did not cash

Three, specifically:

- §"`byard dev`" — *"A small `⟳ reload pending` indicator is shown during the
  wait."* The `pending_reload: Option<(Vec<ViewDecl>, ReloadKind)>` field exists
  on `ByldRuntime` and gates correctly. Nothing renders it. A developer holding a
  drag while a structure-incompatible save waits behind the gate sees an
  application that has silently stopped responding to their edits.
- §"`byard dev`" — *"The last successfully-rendered view stays as a blurred
  background."* `dev.rs`'s `render_error_overlay` deliberately paints an opaque
  field instead, and the comment explains why honestly: the flat four-pass
  encoder draws all text in one global pass after every box, so app text would
  bleed over the scrim. That reasoning was correct when written. RFC-0017's
  z-layers and RFC-0023's backdrop blur have since landed, and it is no longer.
- `check.rs` carries `print_verbose`, which renders `source_map.render(err)` —
  the caret-anchored form — and is marked `#[allow(dead_code)]`. The pretty
  diagnostic renderer is written, tested, and unreachable; `run` calls
  `render_line` instead.

None of these is a large piece of work. All three are the difference between a
tool that behaves as documented and one that does not.

---

## Guide-level explanation

### 1. Starting the dev runner

```
  byard 0.0.0 · dev
  ▏ project   my_app
  ▏ entry     main.byd
  ▏ packages  material, icons
  ▏ gpu       Apple M1 Pro · Metal · timestamp-query ✓
  ────────────────────────────────────────────────────
  ok    12 views, 0 errors                        142ms
  ·     watching 3 paths
```

Four facts, a rule, a result, and a duration. The `gpu` line answers up front the
question a developer would otherwise ask twenty minutes in — *why is there no GPU
timing?* — by reporting `gpu_timing_available()` at startup rather than printing
`GPU timing unavailable` on every subsequent overlay.

The right-aligned duration column appears on every phase-completing line across
every command. It is one `Instant::now()` per phase.

### 2. The statusline

The bottom line of the terminal is claimed and redrawn in place. The log scrolls
above it; the statusline never scrolls.

```
 ● 60fps   cpu 2.8ms │ gpu 0.7ms │ interp 1.9ms   ▁▂▃▂▁▂▅▂▁▁▂▁   382 boxes   ↻14
```

| Field | Meaning | Source |
|---|---|---|
| `●` / `○` | animating / idle | `App::animating`, the `AtomicBool` `wants_frames` already reads |
| `60fps` | redraws in the last interval | a `u32` counter |
| `cpu` | sum of depth-0 scopes (inclusive) | `SampleBlock` |
| `gpu` | sum of resolved GPU scopes | `SampleBlock` |
| `interp` | `interpreter_tax_ns()` — self-time, §I2b | `SampleBlock` |
| sparkline | last 24 frame times | 48-byte ring |
| `382 boxes` | instance census of the last frame | `RenderFrame` |
| `↻14` | hot reloads this session | a `u32` |

The sparkline is the field that earns its place: a single dropped frame is
visible as a spike without reading a number, which is not true of any of the
scalar fields beside it. The `●` is filled while anything is animating and hollow
when the scene has settled — it is a direct read of the flag that decides whether
the event loop sleeps, so it costs one relaxed load and tells the developer
something the numbers do not.

When stdout is not a terminal, the statusline is not emitted at all and the
per-second summary is printed as one plain line. `byard dev 2>&1 | tee log`
produces a clean log.

### 3. `--profile`

```
  frame 3.5ms  ▏ budget 16.7ms  ▏ 21%
  interp.dispatch    0.2ms  █░░░░░░░░░░░░░░░░░░░   0 in release
  interp.tick        1.4ms  ███████░░░░░░░░░░░░░   0 in release
  interp.render      0.3ms  █░░░░░░░░░░░░░░░░░░░   0 in release   (0.9ms incl.)
    layout.taffy     0.6ms  ███░░░░░░░░░░░░░░░░░
  encode.frame       0.3ms  █░░░░░░░░░░░░░░░░░░░
  gpu.solid_box      0.5ms  ██░░░░░░░░░░░░░░░░░░   async −2f
  gpu.msdf           0.2ms  █░░░░░░░░░░░░░░░░░░░   async −2f
  ────────────────────────────────────────────────
  interpreter tax    1.9ms  self-time, 3 scopes
  AOT projection     2.2ms  −37%   (calibration 0.31×)
```

Bars are self-time; a parent that contains children reports its inclusive total
beside it in parentheses, so the two readings are never confusable. `layout.taffy`
is indented because it is depth 1 (§I1), and its 0.6 ms is excluded from the
interpreter tax even though its parent is interpreter-tagged (§I2b).

The block replaces the statusline (it is a superset) and redraws in place by
moving the cursor up. Bars are proportional to the **budget**, not to the
largest row, so a fast frame renders as visibly empty space — which is the point.
The budget defaults to the display's refresh interval and is overridable:

```toml
[dev]
frame_budget = "8ms"
```

Exceeding it colours the total red. This is what turns a performance claim into a
project-level contract rather than an observation.

The `AOT projection` row is RFC-0013 **P3**, which is opt-in by design: the
default statusline never shows it, and `format_telemetry_overlay` still never
calls `project_aot` itself.

### 4. The in-window HUD

`Mod+Shift+D` (`Cmd` on macOS, `Ctrl` elsewhere) toggles a HUD drawn **by the
engine, inside the window, from a `.byd` source**:

```
┌──────────────────────────────┐
│  60 fps        3.5 / 16.7ms  │
│  ▁▂▃▂▁▂▅▂▁▁▂▁▂▃▂▁▂▁▂▂▁▃▂▁    │
│  interp  1.9  layout 0.6     │
│  encode  0.3  gpu    0.7     │
│  382 boxes · 41 text · 12 vec│
│  hud cost 0.08ms (excluded)  │
└──────────────────────────────┘
```

It is an ordinary overlay layer (RFC-0017) with a frosted-glass backdrop
(RFC-0023) and an animated sparkline (RFC-0010). It is not special-cased anywhere
in the renderer.

The last line is not decoration. The HUD competes for the frame it measures, so
it is instrumented under its own `hud.render` scope and that scope is subtracted
from every number above it — and its cost is printed, so the subtraction is
auditable rather than a claim. A HUD that hides its own overhead is worse than no
HUD.

### 5. Diagnostics

```
error: unknown attribute `colour` on `Column`
 ──▶ main.byd:7:14
  7 │     Column #[colour: 0xFF0000] {
    │              ^^^^^^ did you mean `color`?
```

The gutter is dim, the caret and severity are red, the suggestion is cyan. This
is `source_map.render` — already written — with a palette applied.

### 6. Every other command

```
  byard new my_app
  my_app
  ├── byard.toml
  ├── main.byd
  └── .gitignore

  ok    3 files                                     4ms
  ·     run `cd my_app && byard dev`
```

```
  byard check
  ·     resolving module graph                      12ms
  ·     12 views, 3 files, 2 packages
  ok    0 errors                                    24ms
```

```
  byard get
  →     material  git+https://…#v0.3.1   ⠹
  ok    2 packages, byard.lock written              1.2s
```

The spinner in `get`/`build` is drawn from the working thread between units of
work, not from a background thread, and disappears entirely when not a TTY.

---

## Reference-level explanation

### 1. Instrumentation (I1–I3)

#### I1 — the scope set

Six CPU scopes, each declared **inside the subsystem it measures**, so no new
dependency edge is introduced and `frame.rs` remains the only shared boundary:

| Scope | Crate / module | `ScopeKind` | Depth |
|---|---|---|---|
| `interp.dispatch_events` | `byard-compiler::interp::eval` | `Interpreter` | 0 |
| `interp.tick` | `byard-compiler::interp::reactive` | `Interpreter` | 0 |
| `interp.render` | `byard-compiler::interp::eval` | `Interpreter` | 0 |
| `layout.taffy` | `byard-core::atlas::layout` | `Native` | **1** |
| `encode.frame` | `byard-core::encoder` | `Native` | 0 |
| `relay.publish` | `byard-core::relay` | `Native` | 0 |

The nesting is not hypothetical and is not optional: `Interpreter::render`
(`eval.rs:3349`) calls `self.atlas.compute_with_text(…)` at `eval.rs:3501`, so
`layout.taffy` is strictly contained by `interp.render`. The `Interpreter` owns
the `LayoutAtlas` (`eval.rs:932`), so this is structural, not an artefact of the
current call order. §I2 exists because of it.

`byard-compiler` gains an optional dependency on `byard-core`'s `telemetry`
feature only (it already depends on `byard-core` for `frame.rs`).

**Cost.** Each scope is two `Instant::now()` calls plus one write into a
preallocated thread-local slot — measured at 40–60 ns on the reference machine.
Six scopes against a 4 ms tick is ≈ 0.01 %. With `--no-default-features` the
macro expands to nothing and the cost is exactly zero, per RFC-0013's existing
gating.

**Ring capacity.** `RING_CAPACITY` is 4096 and the ring is non-circular (it drops
new samples when full rather than overwriting in-flight ones). Six samples per
tick against 4096 is not a concern; the GPU ring on the render thread is drained
every redraw already.

#### I2 — nested scopes and the double count

`format_telemetry_overlay` computes:

```rust
let cpu_total: u64 = cpu.samples.iter().map(Sample::duration_ns).sum();
```

This is correct only for a disjoint scope set. `interp.render` will nest inside
any future `frame.total`, and `layout.taffy` nests inside `interp.render` in the
current call order. A flat sum would report an 8 ms total for a 4 ms frame.

A profiler that overstates the frame it measures destroys the credibility of
every other number the project publishes. Two options were considered:

- **(A) Keep scopes disjoint by construction.** Cheap, but it is a convention
  enforced by review rather than by the type system, and it forbids the nested
  breakdown that makes a flame view possible at all.
- **(B) Give `Sample` a depth.** `Sample` already carries explicit padding for
  layout stability:

  ```rust
  pub struct Sample {
      pub scope: ScopeId,       // u16
      _reserved_a: u16,
      _reserved_b: u32,
      // start: u64, end: u64
  }
  ```

  `_reserved_a`'s low byte becomes `depth: u8`, maintained by a thread-local
  counter incremented in `Guard::new` and decremented in `Guard::drop`. Totals
  sum only `depth == 0`; children are rendered indented beneath their parent.
  `size_of::<Sample>()` is unchanged, the POD-ness that lets it cross the frame
  boundary is unchanged, and no allocation is added.

**Decision: (B).** It costs one byte that was already reserved, it makes the
correct total structurally guaranteed rather than review-enforced, and it is the
prerequisite for the indented `--profile` view and for the trace export in §8.

#### I2b — the interpreter tax must be *self*-time, not inclusive time

The nesting in I1 exposes a live correctness bug in RFC-0013's existing
accessor:

```rust
pub fn interpreter_tax_ns(&self) -> u64 { self.sum_by_kind(ScopeKind::Interpreter) }
```

`sum_by_kind` sums durations by kind. Since `layout.taffy` (`Native`) is
contained by `interp.render` (`Interpreter`), Taffy's cost is counted **inside**
the interpreter tax. But an AOT-transpiled build still pays for Taffy in full —
layout is not interpreter overhead. The tax is therefore overstated, and
`project_aot` — which computes `total − interpreter + (interpreter × ratio)` —
produces an AOT projection that is **optimistic by the entire cost of layout**.

That is the precise failure mode RFC-0013's motivation names: a profiler whose
numbers are an assertion rather than a measurement. It would also mislead the
RFC-0014 JIT decision in the direction of "the interpreter is the problem",
which is the expensive direction to be wrong in.

**Resolution.** With `depth` present, every scope gains a *self*-time:

```
self_ns(s) = duration_ns(s) − Σ duration_ns(direct children of s)
```

Direct children are recoverable from the flat block without a tree: samples are
pushed in `Drop` order, so a child always precedes its parent and the run of
samples at `depth+1` immediately before a sample at `depth` is exactly its child
set. This is a single reverse linear pass, no allocation.

`interpreter_tax_ns` is redefined as the sum of self-times of `Interpreter`-kind
scopes. `frame total` remains the sum of *inclusive* times at depth 0. Both the
`--profile` block and the HUD display self-time in the bar and inclusive time in
the parent row, labelled.

This is a behavioural change to a public accessor and is called out as such in
the changelog. No caller today reads a non-zero value from it, because nothing is
instrumented — so the fix lands before the bug can ever have been observed.

#### I3 — GPU scopes are already correct

`gpu_timer.rs` interns `ScopeKind::Gpu` scopes and pushes resolved durations onto
the render thread's own ring. No change. They are always depth 0 (a GPU pass does
not nest in a CPU scope in any meaningful sense) and continue to be summed
separately and labelled `async −2f`, per RFC-0013 **P5**.

### 2. `crates/byard-cli/src/style.rs` (P1–P3)

#### P1 — no dependency

Roughly 80 lines of `const &'static str` escape sequences plus a resolver. A
crate such as `owo-colors` or `anstyle` would be defensible, but a CLI whose
entire visual contract is ~20 escape codes does not need a dependency tree for
it, and the project's dependency budget is better spent elsewhere.

#### P2 — roles, not colours; the terminal's palette, not ours

```rust
pub struct Palette {
    pub ok: &'static str,      // SGR 32  — green
    pub warn: &'static str,    // SGR 33  — yellow
    pub err: &'static str,     // SGR 31  — red
    pub accent: &'static str,  // SGR 36  — cyan
    pub metric: &'static str,  // SGR 35  — magenta
    pub dim: &'static str,     // SGR 2
    pub bold: &'static str,    // SGR 1
    pub reset: &'static str,   // SGR 0
}
```

Call sites name roles (`p.err`), never colours. The codes are the **16 base ANSI
colours**, never 24-bit RGB: a terminal's palette is a user preference, and a
tool that hardcodes `#FF5555` overrides a choice the user made deliberately. This
also makes the output legible on light backgrounds without a second theme.

#### P3 — resolution once, branch never

```rust
static PALETTE: OnceLock<Palette> = OnceLock::new();

pub fn palette() -> &'static Palette {
    PALETTE.get_or_init(|| {
        if force_colour()      { Palette::ansi() }
        else if no_colour()    { Palette::plain() }
        else if is_terminal()  { Palette::ansi() }
        else                   { Palette::plain() }
    })
}
```

`Palette::plain()` sets every field to `""`. Formatting code interpolates
unconditionally — `write!(f, "{}{}{}", p.err, msg, p.reset)` — and pays exactly
one empty-string interpolation when colour is off. There is no `if colour` at any
call site.

Precedence, following the de-facto standard: `CLICOLOR_FORCE=1` ≻ `NO_COLOR` set
(any value) ≻ `TERM=dumb` ≻ `IsTerminal` (`std::io::IsTerminal`, stable since
1.70; the workspace MSRV is 1.86).

Glyphs resolve on the same axis: if neither `LC_ALL`, `LC_CTYPE` nor `LANG`
matches `UTF-8` (case-insensitive), box-drawing degrades to ASCII
(`├`→`+`, `─`→`-`, `│`→`|`, `▏`→`|`, block-eighths→`#`, braille spinner→`|/-\`).

### 3. The output grammar (P4)

One 5-column prefix, one right-aligned duration column at terminal width − 8
(clamped to a minimum total width of 60):

| Prefix | Role | Use |
|---|---|---|
| `ok` | `ok` | a phase completed successfully |
| `err` | `err` | a phase failed |
| `warn` | `warn` | a diagnostic that does not fail the phase |
| `·` | `dim` | informational detail |
| `→` | `accent` | an action in progress (paired with a spinner on a TTY) |
| `↻` | `accent` | a hot reload was applied |
| `▏` | `dim` | a key/value fact in a startup header |

All seven commands are converted. Diagnostics keep the rustc-compatible
`file:line:col: error[kind]: message` first line unchanged so editor problem
matchers keep working (RFC-0006 **C7**), and gain the caret block beneath it.

### 4. The statusline (P5–P6)

#### P5 — mechanics

Held on **stderr**; the event log goes to **stdout**. `byard dev 2>/dev/null`
therefore yields a clean, greppable event log with no control characters, and
`byard dev 1>/dev/null` yields the live display alone.

Each repaint is `\r\x1b[2K` + the composed line, with no trailing newline. The
line is composed into a reused `String` cleared (not reallocated) each time, and
truncated so no terminal ever wraps it into two lines and breaks the in-place
redraw. Repaint cadence is 10 Hz — fast enough to read fps changes, slow enough
that the write is invisible (≈ 120 bytes × 10/s).

**Width is composed to 80 columns, not detected.** `ioctl(TIOCGWINSZ)` requires
`unsafe`, and the workspace sets `unsafe_code = "deny"` at the top level; a
status line does not justify an exception to a workspace-wide soundness
invariant, and `terminal_size` is a dependency for one integer. `COLUMNS` is read
when present and used to widen the sparkline only; otherwise every field is laid
out to fit 80 columns. Below 80 the fields drop in a fixed order — sparkline,
then census, then reload count — so the line shortens rather than wraps.

Any log write must first clear the statusline; the writer is therefore funnelled
through a `StatusLine::log()` that emits `\r\x1b[2K`, the message, `\n`, then
repaints. This is the only ordering constraint the module imposes.

On drop (including on `Ctrl-C`, via the existing exit path) the statusline clears
itself and restores the cursor. A tool that leaves a terminal in a broken state
is unacceptable regardless of how good the display was.

#### P6 — the data, and what it costs

- **fps** — a `u32` incremented in `on_redraw`, divided by the elapsed interval.
- **frame-time ring** — `[u16; 24]` of microseconds (65 ms ceiling, saturating),
  48 bytes, written once per redraw. Rendered into a stack `[u8; 96]` with the
  eight block-eighth glyphs, scaled against the budget rather than against the
  window maximum so the sparkline's baseline is stable frame to frame — an
  auto-scaled sparkline animates constantly and communicates nothing.
- **instance census** — `RenderFrame` gains three `#[must_use] pub fn len`-style
  accessors (`instance_count`, `text_count`, `vector_count`). These read `Vec::len`
  on data that already crosses the boundary; no new field, no new traffic.
- **animating** — the existing `Arc<AtomicBool>`, one `load(Relaxed)`.
- **reload count** — a `u32` incremented where `apply_reload` is called.

Total added per-frame cost: one `u32` increment, one `u16` store, three
`Vec::len` reads. The per-second cost is one formatted write.

### 5. The expanded block (V1)

`--profile` (or `Mod+Shift+P` at runtime) replaces the statusline with an *N*-line block
redrawn via `\x1b[{N}A\r` + *N* cleared lines. `N` is fixed for a given scope set,
so no scrollback is consumed.

Rows are sorted by the fixed scope order (not by duration) so a row never jumps
position between repaints — a self-sorting profiler is unreadable in real time.
Children (`depth > 0`) are indented two spaces beneath their parent.

Bar width is 20 cells; fill is `duration / budget`, clamped, drawn with
`█`/`░`. A row exceeding the budget alone is drawn in `err`.

### 6. `[dev]` in `byard.toml` (V2)

```toml
[dev]
frame_budget = "8ms"           # default: the display's refresh interval
hud          = false           # start with the in-window HUD visible
statusline   = true            # default true on a TTY
hud_key      = "Mod+Shift+D"   # dev-runner chord overrides
profile_key  = "Mod+Shift+P"
```

`Manifest` gains an optional `dev` table. Every field has a default; an absent
`[dev]` table is not an error and does not change today's behaviour beyond this
RFC's own defaults. The resolved budget is printed in the startup header so it is
never ambiguous which number the bars are drawn against.

### 7. The in-window HUD (V3–V4)

#### V3 — construction

- A `View __ByardDevHud()` in `crates/byard-cli/src/hud/hud.byd`, embedded with
  `include_str!` and parsed once at startup. It is written in `byld` with no
  privileged syntax — if a construct the HUD needs does not exist, that is a gap
  in the language and should be filled there, not worked around here.
- Instantiated into a dedicated overlay layer above the user tree (RFC-0017), so
  it composites after the app and cannot be occluded by it.
- Fed through the environment: `inject DevTelemetry as t`, a `Record` (RFC-0027
  shapes) published by the logic thread each tick. The `inject` mechanism is
  RFC-0001's and needs no extension.
- Toggled by `Mod+Shift+D`, which `App::on_key` already receives; dev chords are
  consumed before `dispatch_events` so the app never sees them, and they are
  listed in the startup header so they are discoverable without documentation.
  Overridable via `[dev] hud_key`.
- The HUD is parsed into its **own** `Interpreter` instance, separate from the
  user's view registry. No name is reserved and no diagnostic is added, because
  a collision is not expressible — see §Resolved questions Q4.

#### V4 — the observer effect, handled explicitly

The HUD draws into the frame it reports on. This is the one place in this RFC
where decoration provably costs something, so the accounting is explicit:

1. HUD lowering and rendering run inside `profile_scope!("hud.render")`.
2. Every figure the HUD displays is computed from the **previous** frame's block
   with `hud.render` subtracted.
3. The subtracted amount is displayed. `hud cost 0.08ms (excluded)` is a line in
   the HUD, not a footnote in this document.
4. `--profile` in the terminal is unaffected either way, so a developer who
   distrusts the HUD's self-accounting has an out-of-band reading available at
   all times.

If `hud.render` ever exceeds ~5 % of the budget, the HUD has failed its own test
and the finding belongs in `DESICIONS.md`, not in a mitigation.

### 8. Trace export (V5)

`byard dev --trace out.json` writes the Chrome Trace Event format: one
`{"name","cat","ph":"X","ts","dur","pid","tid"}` object per sample, streamed to a
`BufWriter` on the render thread from the block that already arrives there each
frame. With `depth` (I2) present, nesting reconstructs correctly and Perfetto
renders a real flame chart with no further work.

This is the highest ratio of capability to code in the RFC — roughly 30 lines
over data structures that already exist — and it means Byard never needs to build
a flame-graph viewer.

### 9. Closing RFC-0006's open commitments (C1–C3)

- **C1 — `⟳ reload pending`.** `ByldRuntime` publishes an `AtomicBool`
  `reload_pending` alongside `animating`. The statusline shows `↻ pending` in
  `warn`; the in-window surface draws a 2 px amber inset border. Cleared where
  `pending_reload` is consumed.
- **C2 — blurred backdrop behind the error overlay.** The overlay moves onto an
  RFC-0017 overlay layer with an RFC-0023 `blur` backdrop, so the last good view
  is rendered normally beneath it and the global text pass no longer bleeds
  through a scrim — which was the stated and correct reason the original opaque
  fill was chosen. `OVERLAY_MAX_ERRORS` rises from 3 to a scroll-capable list
  once the overlay is a real view.
- **C3 — caret diagnostics.** `check::run` calls `print_verbose`
  (`source_map.render`) instead of `render_line`; `#[allow(dead_code)]` is
  removed. `--short` restores the one-line form for scripts.

### 10. The reload flash (V6)

On applying a hot reload, a 2 px inset border animates alpha 0 → 0.9 → 0 over
120 ms: `ok` green for reactive-compatible, `warn` amber for a deferred
structure-incompatible patch that has just landed. It is one `BoxInstance` driven
by an existing RFC-0010 curve — no new pipeline, no new state beyond the trigger.

Its value is that it confirms application of a change that produced no visible
diff, which is precisely the case where a developer currently cannot tell whether
hot reload is working.

### 11. Feature and flag matrix

| Surface | Gate | Default |
|---|---|---|
| CPU scopes | `byard-core/telemetry` (existing) | on |
| GPU scopes | `telemetry` + adapter `TIMESTAMP_QUERY` | on where supported |
| Colour | `NO_COLOR` / `CLICOLOR_FORCE` / TTY | auto |
| Statusline | TTY + `[dev] statusline` | on |
| Expanded block | `--profile` / `Mod+Shift+P` | off |
| In-window HUD | `Mod+Shift+D` / `[dev] hud` | off |
| Trace export | `--trace <path>` | off |

Everything above is dev-runner surface. `byard build` output is unaffected apart
from the shared grammar; nothing here enters a shipped application.

---

## Drawbacks

**The HUD costs frame time.** It is the only element that does. §7 V4 bounds and
discloses it, but the honest statement is that a developer with the HUD open is
not measuring quite the same frame as one without it — hence the terminal
readout remains authoritative and the HUD defaults to off.

**Six scopes is a permanent 0.01 % tax in dev builds.** Real, if small, and
removable with `--no-default-features`. The alternative is continuing to ship a
profiler that measures nothing.

**Terminal control sequences are a compatibility surface.** In-place redraw
interacts badly with terminals that reflow on resize, with `screen`/`tmux` under
some configurations, and with editors' integrated terminals. Mitigations: strict
width truncation, degradation to plain lines when not a TTY, and `[dev]
statusline = false`. It will still be imperfect on some terminal somewhere.

**`Sample` gains a semantic field.** `depth` is derived state maintained by the
guard, and an unbalanced guard (a `mem::forget`, a panic across a scope) would
corrupt it. The counter is reset at drain, so corruption is bounded to one tick
rather than persistent.

**Scope creep risk.** This RFC touches presentation across every command, the
telemetry data model, the manifest, and the renderer. It is sequenced (§Ordering)
precisely so it can land in slices, and the instrumentation slice is independently
valuable if the rest is deferred.

---

## Rationale and alternatives

**Why not a TUI framework (`ratatui`, `crossterm`)?** A full-screen TUI takes the
alternate screen buffer, which destroys scrollback — and scrollback is where
parse errors live. The design here is deliberately a *hybrid*: an ordinary
scrolling log, plus one anchored line. That is `cargo`'s model and `docker
build`'s model, and it is the correct one for a tool whose output is read both
live and after the fact. It also avoids a large dependency for what amounts to
three escape sequences.

**Why not just improve the once-a-second block?** Because the medium is wrong.
Continuous measurement and discrete events cannot share a scroll region without
one destroying the other. Any amount of formatting effort spent on the block
leaves parse errors buried.

**Why 16 colours instead of truecolor?** A truecolor palette looks better in the
screenshots the author takes and worse on a meaningful fraction of users'
terminals, because it silently overrides a deliberate preference. Byard's stated
posture is that correctness beats polish; a terminal theme is user data.

**Why put the HUD in `byld` rather than in Rust?** A Rust HUD would be easier to
write and would prove nothing. The `byld` HUD is a permanent, self-executing test
that the framework can render a non-trivial, continuously-updating, blurred,
animated overlay within its own frame budget — run on every developer's machine,
every session. If it cannot, the project needs to know that immediately, and this
is the cheapest possible way to find out.

**Why `depth` on `Sample` instead of a scope tree?** A tree means allocation and a
non-POD block, which would break the RFC-0001 §5 rule that only `Send` PODs cross
the frame boundary. One byte in existing padding preserves the boundary contract
exactly.

**Impact of not doing this.** RFC-0013's profiler continues to exist without
data; the interpreter-tax segmentation that justifies or rejects RFC-0014's JIT
stays unmeasured; and every performance claim in the project remains an assertion
rather than a reading — which RFC-0013's own motivation section names as the
thing it exists to prevent.

---

## Prior art

- **`cargo`** — the anchored progress line above a scrolling log; `NO_COLOR`
  handling; right-aligned timing on completion lines. The closest model to what
  is proposed here.
- **`rustc`** — the caret diagnostic layout adopted in §9 C3, and the discipline
  of one machine-readable first line followed by human context.
- **Zig's build system** — hierarchical in-place progress with strict degradation
  when not a TTY.
- **Tracy / Optick / Perfetto** — the scope/depth sample model, and the Chrome
  Trace interchange format §8 targets. Tracy in particular demonstrates that a
  profiler UI hosted *inside* the profiled application is viable when its own
  cost is measured and reported (V4 follows this directly).
- **Flutter DevTools / Chrome's FPS meter** — the in-window HUD with a
  frame-budget bar. Flutter's meter is also the cautionary example: it draws
  through the same pipeline it measures and does not disclose its own cost, so its
  readings are quietly optimistic under load.
- **`esbuild` / `vite`** — the expectation that a modern dev server reports
  per-phase timings by default, which is now table stakes rather than a
  differentiator.

---

## Resolved questions

### Q1 — HUD toggle key

**Question.** `F12` across macOS/Linux/Windows?

**Options.** (a) `F12`; (b) a `Mod`-modified chord; (c) a CLI flag only, no
runtime toggle.

**Resolution: (b), `Mod+Shift+D`** (`Cmd` on macOS, `Ctrl` elsewhere), with
`Mod+Shift+P` for the expanded terminal block, both overridable via `[dev]`.

Bare function keys are unusable as a default: macOS maps F-keys to system media
controls unless the user has flipped the "use F1–F12 as standard function keys"
preference, and several are claimed by the window manager on Linux desktops. A
`Mod+Shift` chord is free on all three platforms and — critically — cannot
collide with text entry in the app under test, which a bare key can. Dev chords
are consumed before `dispatch_events`, and are printed in the startup header so
they need no documentation.

### Q2 — which stream carries the statusline

**Question.** Statusline on stderr with the log on stdout, or the reverse?

**Options.** (a) statusline → stderr, log → stdout; (b) everything → stderr, as
`cargo` does; (c) statusline → stdout, log → stderr, the classic
"diagnostics-on-stderr" convention.

**Resolution: (a), and it is a `dev`-only rule.**

The convention that diagnostics belong on stderr exists so a program's *data*
output stays clean. `byard dev` has no data output — its log **is** its product,
and `byard dev > session.log` producing a readable, control-character-free log is
the ergonomic that matters. The ephemeral display is the side channel, so it
takes stderr.

`byard check` and `byard build` are the opposite case: they are CI tools whose
contract is "diagnostics on stderr, exit code non-zero" (RFC-0006 **C7**), and
that contract is preserved unchanged. Two commands, two contracts, each matching
what its consumer actually is. This is stated explicitly rather than left to
inference because it is the kind of asymmetry that gets "tidied up" later by
someone who did not read this paragraph.

### Q3 — `frame_budget` default

**Question.** Display refresh interval (adaptive) or fixed 16.7 ms (comparable)?

**Options.** (a) display refresh; (b) fixed 16.7 ms; (c) fixed, with a warning
when the display differs.

**Resolution: (a), display refresh interval, printed in the startup header.**

The budget answers "will this app drop frames on this machine". On a 120 Hz
panel that threshold is 8.3 ms, and a tool that drew bars against 16.7 ms there
would report a comfortable frame for one that visibly stutters — lying on exactly
the hardware where frame budget matters most. Cross-machine comparability is a
different job, served by `--trace` and by pinning `[dev] frame_budget` in CI,
both of which are explicit. Printing the resolved value in the header removes any
ambiguity about which number a bar is drawn against.

### Q4 — reserved view name for the HUD

**Question.** Is a `ReservedViewName` diagnostic for `__ByardDevHud` worth the
grammar surface?

**Options.** (a) reserve the name and diagnose collisions; (b) rely on a
leading-underscore convention; (c) make collision unexpressible.

**Resolution: (c) — no reserved name, no diagnostic, no grammar change.**

The HUD is parsed into its **own** `Interpreter`, separate from the user's view
registry, and instantiated into its own overlay layer. A user view named
`__ByardDevHud` therefore resolves to the user's view in the user's tree and to
the HUD in the HUD's tree, with no interaction between them. There is nothing to
diagnose because there is no collision.

This resolution *removes* work rather than adding it, and it is the better answer
for that reason: a reserved word is a permanent tax on the language's namespace
paid for a dev-only feature. Byard has exactly one hard architectural rule about
layering, and a dev tool leaking a keyword into the user's grammar is the sort of
small violation that is never worth it.

### Q5 — sparkline window

**Question.** 24 samples was a guess.

**Resolution: 24, fixed, not configurable.**

It is the width that fits alongside every other statusline field inside 80
columns (§P5), and at 60 fps it is 400 ms of history — long enough that a hitch
is still on screen when the developer looks up, short enough that the display
tracks the present. `COLUMNS`, when exported, widens it and nothing else. It is
not a manifest option: a configurable sparkline width is a knob with no correct
setting and therefore no reason to turn.

### Q6 — `depth` for GPU samples

**Question.** GPU samples are pushed from `gpu_timer.rs` outside any guard.

**Resolution: GPU samples are always depth 0**, set explicitly by
`Sample::gpu_duration` rather than left to the default so the value is
intentional at the construction site.

A GPU pass does not nest inside a CPU scope in any sense that a self-time
subtraction would be meaningful for — it resolves two frames later and belongs to
a different timeline. CPU and GPU totals are summed separately in the overlay
already (RFC-0013 **P5**), and this keeps that separation structural.

### Q7 — terminal width without a dependency

**Question.** `ioctl(TIOCGWINSZ)` vs. `COLUMNS` vs. a fixed assumption.

**Options.** (a) `ioctl` per platform; (b) the `terminal_size` crate; (c) compose
to a fixed 80 columns, widening opportunistically from `COLUMNS`.

**Resolution: (c).**

`ioctl` requires `unsafe`, and the workspace sets `unsafe_code = "deny"`. A
status line is nowhere near sufficient justification to open a hole in a
workspace-wide soundness invariant — that lint is load-bearing for the project's
central claim, and its value comes from being unconditional. `terminal_size`
would be a dependency and a supply-chain surface for one integer.

Composing to 80 needs no detection at all: every terminal in practical use is at
least 80 columns, the line only needs to not *exceed* the width, and it degrades
by dropping fields in a fixed order rather than wrapping. `COLUMNS`, when the
shell exports it, widens the sparkline and nothing else, so an unset or stale
`COLUMNS` is never a correctness problem.

### Q8 — nesting of `interp.render` and `layout.taffy`

**Question.** Do they nest, and is the ordering stable enough to hardcode?

**Resolution: they nest, structurally, and the ordering is hardcoded.**

`Interpreter::render` (`eval.rs:3349`) calls `self.atlas.compute_with_text(…)`
at `eval.rs:3501`; the `Interpreter` owns the `LayoutAtlas` (`eval.rs:932`). This
is not incidental call order — layout is a phase of render by construction, so
`layout.taffy` is depth 1 under `interp.render` and the `--profile` row order is
fixed accordingly.

Investigating this question is what produced §I2b, the interpreter-tax self-time
correction. That is the argument for resolving questions rather than filing them:
the answer was not a detail, it was a bug in an accessor RFC-0013 already ships.

### Q9 — suppressing the reload flash on a no-op diff

**Question.** Should the flash be skipped when the reload changed nothing
visible?

**Resolution: never suppressed.**

The flash exists precisely for the case where a save produced no visible
difference — that is the only situation in which a developer genuinely cannot
tell whether hot reload is working. Suppressing it there would remove the feature
from its sole justifying use case and leave it firing only when it is redundant.

---

Implementation-time decisions that surface after merge go to
`support/DESICIONS.md` as `IMPL-NN` entries, per that file's own rule. This RFC
carries no open questions.

---

## Future possibilities

- **`byard doctor`** — toolchain, adapter, wgpu feature support, lock coherence.
  Shares the entire §3 output grammar.
- **`byard dev --screenshot out.png`** — one headless frame to PNG, enabling
  visual regression in CI.
- **`byard check --watch`** — continuous validation with no window, for terminal-
  resident workflows.
- **`byard explain E0012`** — a rustc-style error catalogue, which the §9 C3
  diagnostic work makes a natural next step.
- **Historical budget tracking** — persisting per-scope medians to
  `.byard/cache/perf.json` and reporting *regressions* between sessions, which is
  the point at which the frame budget becomes CI-enforceable.
- **Remote HUD** — the `DevTelemetry` record shipped over a socket to a second
  window or a browser, once RFC-0029's `net` capability exists.
- **JIT justification** — RFC-0013 sequenced itself before RFC-0014 explicitly so
  the JIT would be a solution to a *quantified* problem. This RFC is what supplies
  the quantity.

---

## Ordering

The slices are independently valuable and should land in this order:

1. **I1–I3** — instrumentation and the `depth` fix. Nothing else has data without
   it, and it is worth landing alone.
2. **P1–P4** — `style.rs` and the output grammar across all seven commands.
3. **P5–P6** — the statusline. Highest impact per line of code in the RFC.
4. **V1–V2** — the expanded block and the frame budget.
5. **C1–C3** — RFC-0006's three outstanding commitments.
6. **V5** — trace export (small, and unblocks external tooling entirely).
7. **V3–V4, V6** — the in-window HUD and the reload flash.
