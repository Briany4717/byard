# Erratum to RFC-0030: the statusline's field set, and what the sparkline plots

- **Status:** Active erratum (amends, does not replace, RFC-0030)
- **Author(s):** Briany4717
- **Created:** 2026-07-26
- **Applies to:** RFC-0030 §"The statusline" (the guide-level field table), §P6
  ("the data, and what it costs"), and §V1's worked example block.
- **Authority:** the `present.acquire` / `present.submit` scopes, which landed
  after RFC-0030 was written, plus two readings taken from
  `crates/byard-cli/examples/profiling` while implementing §P5–P6.

---

## Why this erratum exists

RFC-0030 specified its statusline before the engine could tell a frame that
finished early from one that overran. The `present.*` scopes changed that, and
they change what the line should say. RFC-0030's own §I2b argument — *a profiler
whose numbers are an assertion rather than a measurement is the failure mode
worth designing against* — applies to its own field set, so the field set moves.

RFC-0030 is **Active**, so its body is not edited. Where it and this erratum
disagree about the statusline's fields, **this erratum wins**. Nothing here
touches §P1–P4 (the output grammar), §V5 (trace export), §V3–V4 (the HUD), §C1–C3
(RFC-0006's commitments), or any of the resolved questions.

---

## Correction 1 — `work` / `idle` replaces `cpu │ gpu │ interp`

RFC-0030 §"The statusline" specifies:

```
 ● 60fps   cpu 2.8ms │ gpu 0.7ms │ interp 1.9ms   ▁▂▃▂▁▂▅▂▁▁▂▁   382 boxes   ↻14
```

with `cpu` as "sum of depth-0 scopes (inclusive)". That field cannot move on a
vsync-bound app, which is most apps most of the time.

`present.acquire` established the distinction the whole readout turns on: under
FIFO, the acquire is where the driver parks the caller until the display is
ready. A frame that overran its budget and a frame that finished early and waited
produce the **same** depth-0 total, and differ entirely in how much of it was
spent waiting. Worse, once a scene is vsync-bound, every further engine win shows
up as *more waiting* rather than a smaller total — so a headline number built on
the total is a headline number that cannot improve. Optimising against it would
report no progress for real progress, which is precisely the direction a
performance project must not be wrong in.

**The field set is therefore:**

```
 ● 60fps  work 3.4ms · idle 15.0ms  ▁▂▃▂▁▂▅▂▁  382 boxes  ↻14  retained 64/64
```

- **`idle`** is `present.acquire`: wall time the display imposed.
- **`work`** is everything else — the engine's own cost, and the number that
  moves when the app or the engine gets faster or slower.

`gpu` leaves the line rather than being renamed. It resolves two frames later
against a different clock (§Q6), so putting it beside two figures that describe
*this* frame invites exactly the addition that is never valid. It keeps its row
in the `--profile` block and its own lane in the trace, both of which have room
to say what it is.

`interp` also leaves the line. The interpreter tax is a real and important
figure, but it is a *composition* question — how much of `work` an AOT build
would remove — and the block and the trace answer it with the nesting intact.
A third scalar on an 80-column line was buying less than the census beside it.

## Correction 2 — `retained N/M`, a field RFC-0030 could not have specified

RFC-0032's `path_counters` landed after RFC-0030 was written. The statusline
carries `retained 59/60`: how many of the last 64 rendered frames took the
retained layout path rather than tearing the Taffy tree down and rebuilding it.

This is the answer to *"am I on the fast path?"* **read rather than inferred**. It
was previously only available as an absence — a timing that failed to get
smaller — which is not a signal anybody notices. A view that rebuilds every frame
is now something a developer sees, and usually fixes.

## Correction 3 — the sparkline plots **work**, not the frame period

§P6 specifies the ring as "last 24 frame times", scaled against the budget. The
budget scaling is right and stays; plotting the frame *time* is wrong, and it is
wrong for the same reason as Correction 1.

Under vsync the frame period **is** the budget, by construction. Implemented as
written, a perfectly healthy 60 fps app drew a permanently full, permanently red
sparkline. That is the "an auto-scaled sparkline is always full and therefore
says nothing" failure RFC-0030 names in its own Traps section, reached from the
opposite direction: not by scaling to the window maximum, but by plotting a
quantity that is pinned to the scale.

The ring holds **per-frame work**. A healthy scene then shows visible headroom, a
hitch is a spike, and the field means the same thing as the `work` figure beside
it.

## Correction 4 — `work` is the pipeline's critical path, not a sum

Byard renders on two threads. Summing their depth-0 totals reported `work 5.1ms ·
idle 15.0ms` inside a 16.5 ms frame — twenty milliseconds of work in a sixteen
millisecond frame. That is RFC-0030 §I2's double count exactly, one level up:
between concurrent threads instead of between nested scopes.

`work` is therefore `max(logic_total, render_total − idle)`. The frame is late
when *either* thread overruns, so the critical path is the honest single figure:
it moves when the interpreter gets slower and when the encoder does, and it can
never claim more work than the frame contained. Taking only the render thread's
figure would have satisfied the arithmetic and hidden the interpreter, which is
the number a developer is usually looking for.

## Correction 5 — §V1's example block predates the sub-scopes

§V1's worked example lists `encode.frame` as one row. It now has a subtree —
`encode.uploads`, `encode.glyphs`, `encode.passes` with `encode.buffers` nested
inside it, and `encode.submit` — alongside `present.acquire` and
`present.submit`. The block's shape is unchanged; only the scope set it charts is
larger.

One consequence is worth stating because it is not obvious: `encode.buffers` is
sampled once per draw group, so its sample count varies frame to frame. The block
aggregates repeated samples of one scope into a single row carrying an `×N`
count. This is not cosmetic — the block redraws by moving the cursor up N lines,
and a row count that varied would erase the wrong lines and consume scrollback,
which is where parse errors live.

---

## What is unchanged

- **§Q2** — statusline on stderr, event log on stdout, and `byard check` /
  `byard build` keeping the opposite CI-shaped contract.
- **§Q5** — 24 samples, `COLUMNS` widening the sparkline and nothing else.
- **§Q7** — composed to 80 columns; no `ioctl`, no dependency.
- **§Q3** — the budget is the display's refresh interval, printed in the startup
  header, overridable by `[dev] frame_budget`.
- **The `●` / `○` indicator** and the `↻N` reload count, both as specified.
- **§P6's cost claim.** The per-frame cost is still counters and `Vec::len`
  reads; composition happens at most ten times a second.
