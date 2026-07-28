# Erratum to RFC-0030: what the HUD costs, and who a scope's time belongs to

- **Status:** Active erratum (amends, does not replace, RFC-0030)
- **Author(s):** Briany4717
- **Created:** 2026-07-27
- **Applies to:** RFC-0030 §V4 (the observer effect), §I1 (the scope set), §I2b
  (self-time), §V1 (the expanded block), and the status line at the head of
  RFC-0030 that reports §V4's acceptance condition as met.
- **Authority:** measurements taken from `crates/byard-cli/examples/profiling`
  and from the permanent test this erratum adds
  (`hud::self_accounting` in `crates/byard-cli/src/hud/mod.rs`), in **both**
  build profiles. Both are recorded in `support/PERF_hud_baseline.md`.

---

## Why this erratum exists

RFC-0030 §V4 makes one promise about the in-window HUD:

> Every figure the HUD displays is computed from the previous frame's block
> with `hud.render` subtracted. **The subtracted amount is displayed** … A HUD
> that hides its own overhead is worse than no HUD.

The subtraction was displayed, and it was wrong. `hud.render` reported ~1.5 % of
the frame budget; the HUD's real cost was several times that. RFC-0030 is
**Active**, so its body is not edited: where it and this erratum disagree about
what the HUD costs and how a frame's time is attributed, **this erratum wins**.

Nothing here touches §P1–P4 (the output grammar), §V5 (trace export, beyond one
added argument), §V6 (the reload flash), §C1–C3, or the language.

---

## Correction 1 — a scope name says *what* ran, not *whose* it was

§I2b established that a profiler must not double-count across a **nesting**
boundary: a parent and a child summed together exceed the frame they measure.
The same failure exists one level out, between **peers**, and §V4 walked into
it.

When the HUD is open, two interpreters run in one frame. Both enter
`interp.render`. Both drive a `LayoutAtlas`, so both enter `layout.taffy`. A
consumer that aggregates by scope name merges them. The expanded block was
honest that a merge had happened — it printed `×2` — and silently wrong about
whose time it was: all of it landed in the app's rows, so a developer read the
HUD's interpreter as their own app getting slower.

**A sample therefore carries its owner.** `byard_core::telemetry::Owner` is
`App` or `DevTools`, stamped at scope entry from a thread-local set by
`telemetry::attribute_to`, and stored in a byte of `Sample`'s existing padding
— `Sample` is still 24 bytes and still crosses the frame boundary as plain
`Pod` data.

Two properties make this the right mechanism rather than a parameter threaded
through the engine:

- **It attributes code that has never heard of it.** The HUD's cost is not a
  call it makes; it is the interpreter, the layout atlas and the shaper doing
  their ordinary jobs on its behalf, in code shared with the app. A boundary
  attribution covers the whole subtree by construction. Threading an owner
  parameter through all of it would be invasive, easy to get wrong in one
  branch, and would have to be repeated for the next producer.
- **The buckets reconstruct the frame rather than exceeding it.**
  `owner_total_ns(App) + owner_total_ns(DevTools) == total_ns()` for any
  well-formed block, because the totals are self-time. This is §I2b's
  arithmetic, applied to owners.

### What follows from it

- **The expanded block's scope rows are `Owner::App` only**, and the whole of
  `Owner::DevTools` lands in the `hud.render` row. That row is consequently
  *not* the inclusive time of the scope whose name it carries — it is what the
  dev surfaces cost this frame, on every thread — and it says so
  (`dev, all threads`). §V4's 5 % gate can only be evaluated against
  that figure.
- **`work`, the statusline's headline, is `Owner::App` only.** It must not move
  when a developer opens the HUD; that would be the observer effect reported as
  an app regression.
- **The interpreter tax is `Owner::App` only.** It answers "what would an AOT
  build of *this app* stop paying". The HUD is `byld` too, so its interpreter
  cost is real — and somebody else's.
- **`--trace` tags dev-owned events** with a `dev` category and an
  `args.owner`, so hiding the profiler's own overhead is a checkbox in
  Perfetto. They stay on their parent's lane: `encode.glyphs.dev` really does
  nest inside `encode.glyphs`, and a separate `tid` would destroy the
  containment the trace format's nesting depends on (§V5).

---

## Correction 2 — the render thread's half, and the frame's dev partition

A logic-thread scope cannot enclose render-thread work; `hud.render` has been
dropped long before the encoder runs. By then the HUD's primitives are
anonymous entries in the same pools as the app's.

So the **frame** carries the partition. `RenderFrame::set_dev_base` records a
pool-cursor snapshot taken before the first dev surface emits; dev surfaces are
always emitted last and always open their own z-layer (RFC-0017), so in every
pool they are a contiguous suffix and one cursor answers "is this one theirs?"
for every pool at once. `None` — the default, and every frame of a shipped app
— means the frame carries no dev surfaces at all.

The encoder charges three things across that boundary, each under its own scope
so the block still adds up:

| Scope | What it covers |
|---|---|
| `encode.glyphs.dev` | shaping the dev surfaces' text, and staging their layers' glyph vertices |
| `encode.buffers.dev` | appending their instance data to the arena (RFC-0033) |
| `encode.passes.dev` | recording their render passes — including, for the HUD, the copy/blur/composite of its frosted pane |

None of these are rows in the expanded block. That is deliberate: a row per dev
scope would make the block's line count depend on whether the HUD is open, and
§V1's in-place redraw requires a constant count. They are counted by owner
instead, so nothing is lost.

### The residual, stated rather than absorbed

One term is genuinely not separable. `encode.finish` is a single `wgpu` call
that validates and assembles the whole frame's command buffer; it grows with
the passes and draws an overlay adds, and splitting it would mean splitting the
command buffer — a change to the most delicate part of the renderer, with real
visual risk, for attribution accuracy in a dev-only path. It is declined.

**It is not small, and this document previously said it was.** An earlier draft
reported it as ~55 µs whether the HUD was open or not, and that figure came
from a batched measurement whose baseline was taken cold (see
`support/PERF_hud_baseline.md`). Measured in pairs, `encode.finish` grows by
**~50 µs in release and ~570 µs in debug** when the HUD opens, which is ~30 %
and ~42 % of the delta respectively.

So the honest statement of what §V4 now reports is:

> The dev-owner total accounts for **~65 % of what the HUD costs in release**
> (~35 % in debug). The remainder is `encode.finish`, in full: dev-owner total
> plus the measured `encode.finish` delta reconstructs the frame delta to
> within 1–8 %.

The issue's acceptance asked for the reported figure to be within ~10 % of the
delta. **It is not, and that is recorded rather than closed over.** What is
achieved instead — and what the permanent test asserts, in both profiles — is
the complete accounting identity: every nanosecond the HUD adds is either
attributed to the dev runner or inside one named scope that has its own row and
can be watched moving. That is a weaker claim than the issue asked for and a
much stronger one than the block used to make, and it is checkable rather than
quoted.

Closing the remaining gap means recording dev segments into a second command
encoder and submitting both in order. That is the only mechanism that would
work, it is available, and it is declined on the trade rather than on
difficulty: it changes the renderer's submission path for every frame of every
dev session, and the failure mode is visual corruption. If `encode.finish` ever
becomes the largest term in a release frame, the decision should be revisited
on its own merits.

---

## Correction 3 — INV-24's third mitigation was correct and inert

§V4's INV-24 lists three mitigations for the HUD defeating the retained text
path, the third being fixed-width numeric formatting so a changing value does
not change a text leaf's fingerprint. It was implemented, and it saved nothing:
`encode.glyphs` still roughly quadrupled the moment the HUD opened.

The mitigation was not at fault. The **shaper never asked**. Its re-shape gate
consulted `TextLine::dirty`, and that flag's producer is the interpreter, which
re-walks the tree and re-emits every leaf each tick with no per-element change
signal — so it sets `dirty: true` on every line of every frame. "Trust the
flag, hash nothing" bought zero skips in a real `byld` app and degenerated to
*re-shape everything, every frame*.

**The glyph cache is now content-addressed.** A line is re-shaped if and only if
its `shape_key` — `(text, font_size, wrap)` — differs from the key its cached
buffer was shaped from, or the viewport changed, or index identity is not
comparable at all. Colour and position are excluded: neither reaches the
shaper, so folding either in would re-shape a run for a change that provably
cannot alter a glyph — the same paint-class/layout-class distinction INV-24's
sparkline rests on, applied to the cache key.

The trade is the opposite of what it looked like. An `FxHasher` pass over a
short string is tens of nanoseconds; shaping it is tens of microseconds. And it
is strictly more robust: a producer that changes a line and forgets to set
`dirty` used to render stale glyphs in release, in silence, and now renders
correctly, because the key is derived from the content rather than asserted
about it.

`dirty` is still consulted — it is what `dirty_text_bounds` unions into the
incremental redraw scissor, so a change with the flag unset is now shaped
correctly and may still be clipped out of the repaint. The debug-only
consistency check therefore stays, with that as its stated meaning.

**This is not a HUD fix.** It applies to every `byld` app, and it removes what
RFC-0030's own §I1 example calls "the largest single number on this readout".

### `N/M shaped`, read rather than inferred

`encode.glyphs` now carries the count on its row. "Is the glyph cache working?"
and "was the frame fast?" are different questions, and only one of them is
answerable at a glance from a duration. `0/412 shaped` on a steady scene is
the retained text path working; `412/412` every frame is the failure it exists
to remove.

---

## Correction 4 — `encode.frame`'s breakdown did not add up

§I1's standard is that a parent's self-time plus its children's inclusive times
equal its own inclusive time, and that the parts are *readable*. `encode.frame`
met the arithmetic and failed the intent: on a text-heavy frame the largest and
second-largest terms inside it were in no sub-scope at all, so they appeared
only as self-time that no row explained.

Three scopes are added, all `Owner::App`, all children of `encode.frame`:

- **`encode.scissor`** — the dirty-region computation: a linear scan of every
  pool, unioning what is dirty with where it was last frame.
- **`encode.bookkeeping`** — its matched pair, recording this frame's bounds for
  the next one's scissor.
- **`encode.finish`** — `wgpu` validating and assembling the command buffer.
  The largest of the three, and the term Correction 2 cannot attribute.

---

## Correction 5 — a missed vsync is not an over-budget frame

Several frames reported 197–200 % of budget with `present.acquire` at ~30 ms —
one frame short of exactly two vsync intervals — while the engine's own work on
those frames was under a millisecond. Under FIFO that is a frame that missed a
vsync and waited for the next one. The block rendered it in `err`, which is the
misreading the `work`/`idle` split exists to prevent, and a developer who
chases it is chasing the compositor.

The expanded block's header now colours on **`work`**, not on the frame period:

- `err` — the engine overran: `work > budget`.
- `warn` + `waited` — the period overran and the engine did not. The frame *was*
  late, which is worth seeing, and it is a different fact with a different
  cause.
- `ok` — neither.

The period and its percentage are printed unchanged. Only the verdict on them
moved.

---

## What the numbers are now

Medians of 15 steady-state frames on the same scene and build, Apple M2,
`hud::self_accounting`. The full table, with the debug/release ratio per scope,
is in `support/PERF_hud_baseline.md`.

| | debug | release |
|---|---|---|
| **the HUD's real cost** (the measured delta) | 1.31–1.44 ms | **0.163 ms** |
| reported by the dev-owner total | 0.76–0.82 ms (42 % low) | **0.107 ms (35 % low)** |
| dev-owner total **+ measured `encode.finish`** | 1.30–1.41 ms (1–2 % off) | **0.153 ms (1–7 % off)** |
| reported by `hud.render` alone (the old rule) | 0.54–0.58 ms (59 % low) | 0.083 ms (49 % low) |

Neither figure meets the ~10 % the issue asked for on its own. The **accounting
identity** does, in both profiles: the attribution plus the one named
unsplittable term reconstructs the delta. That is what the permanent test
asserts.

**Which settles the question §V4 could not answer.** RFC-0030's status line
reports the 5 % gate as met at 3.6 %. Against the *measured* cost — not against
what the profiler says about itself, which is the only way a self-accounting
gate can be checked without begging the question — on a scene that is redrawing
either way, the HUD is **0.163 ms of a 16.667 ms budget: 1.0 %**. It passes,
with room to spare, and for the first time the figure is the whole cost rather
than one scope's share of it.

The HUD *displays* ~0.107 ms (0.6 %), because that is what it can attribute.
Both numbers are true and they are not the same number; the difference has its
own row.

One further finding is recorded rather than fixed, because it is a property of
the skip and not of the HUD: on a **static** scene the encoder skips the draw
path entirely, and opening the HUD un-skips it. Most of what a HUD costs such
an app is therefore the app's own drawing resuming, not the HUD's work. §V4's
figure is the latter, and the measurement above holds the former constant by
changing one app-owned line per frame.

---

## Consequences for RFC-0030's status line

RFC-0030's header says §V4's acceptance condition "**is met**: 0.60 ms p50
against 16.667 ms, 3.6 %". That figure was `hud.render`'s inclusive time on a
debug build. It should be read as: the gate is met, at **1.0 % in release**,
measured against the frame delta; the 3.6 % was a debug reading of a partial
figure, and both halves of that sentence were wrong in the same direction the
erratum before this one warns about.
