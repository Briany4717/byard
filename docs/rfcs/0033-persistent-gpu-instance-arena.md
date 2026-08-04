# RFC-0033: Persistent GPU Instance Arena, one buffer, one upload, no per-frame VRAM churn

- **Status:** Active, implemented
- **Author(s):** Briany4717
- **Created:** 2026-07-25
- **Last updated:** 2026-07-26
- **Depends on:**
  - RFC-0001 (§2 the memory thesis, specifically *"sin spikes de VRAM"*, which this RFC is what makes true on the GPU side; §3.1 the pipeline set; §5 the render-thread boundary)
  - RFC-0030 §I1 (the `encode.frame` scope that surfaced the cost)
  - `0001-erratum-memory-and-dirty-model.md` (PR #148, which corrected the *CPU* half of the same claim)
- **Extends:** `byard-core::encoder`, every pipeline's per-draw buffer handling.
- **Independent of:** RFC-0032. Different cause, no shared code, either may land first.

---

## Summary

Every render pipeline in the encoder creates its GPU instance buffer **from
scratch on every frame**:

```
backdrop.rs:652      canvas_shape.rs:204     decorated_box.rs:236
ripple.rs:144        texture_sampler.rs:480  vector_msdf.rs:340
mod.rs:2464/2474 (clear quad: instance + depth)
mod.rs:2509/2520 (solid box: instance + depth)
backdrop.rs:415  (uniform)
```

Nine or more `create_buffer_init` calls per frame, more with multiple draw
batches. The correct pattern, a persistent buffer written with
`queue.write_buffer`, exists in the crate in **exactly one place**
(`viewport_buffer`, `mod.rs:726`, the only `write_buffer` in `byard-core`).

RFC-0030's instrumentation put `encode.frame` at **~6 ms**, an order of magnitude
above `layout.taffy`, making it the largest single term in the frame. Buffer
creation is the leading suspect: in `wgpu` each `create_buffer_init` is a device
allocation, a validation pass, a staging allocation, a copy, and a device lock, 
per pipeline, per frame.

> **Erratum, added on implementation.** The suspect was wrong. Sub-scoping
> `encode.frame` (RFC-0030 §I1, second pass) put every `create_buffer_init` in
> the encoder combined at **0.3–3.4 %** of it; the remaining 84–98 % was glyph
> shaping, which RFC-0032 addresses. Measured after landing, this RFC is worth
> about **0.1 ms**, within the revised projection and roughly fifty times
> below the figure in the paragraph above.
>
> The reasoning error is worth naming because it is easy to repeat: the RFC
> argued from a *mechanism* to a *magnitude* without measuring the magnitude.
> Every step listed above is real; nine of them per frame simply do not add up
> to milliseconds. What justifies this RFC is §"Why this is a thesis problem"
> below, and the fact that "zero buffer creations per frame" is a
> **deterministic** assertion where a timing is not.

This RFC replaces all of it with **one persistent GPU buffer, one CPU-side
staging vector, and one `write_buffer` per frame**, with per-pipeline draws
reading from offsets into it. Buffers grow to a high-water mark and are never
recreated in steady state.

Beyond the time, this closes the GPU half of a thesis claim. PR #148's erratum
already corrected the CPU half, *"zero-GC stands, allocation-free hot path does
not"*. RFC-0001 §2 also says **"sin spikes de VRAM"**, and recreating every
instance buffer each frame is precisely VRAM churn. This RFC is what lets that
sentence stand as written, rather than being corrected too.

---

## Motivation

### The pattern is known-correct and applied once

`viewport_buffer` is created once at init (`mod.rs:416`) and written with
`queue.write_buffer` (`mod.rs:726`). Every other buffer in the encoder is
recreated per use. There is no design reason for the asymmetry, the viewport
buffer is not special, it is simply the one that was written correctly.

### Why per-frame buffer creation is expensive

Not merely "an allocation". Each `create_buffer_init` in `wgpu`:

1. allocates from the device's memory allocator (may take an internal lock),
2. runs descriptor validation,
3. allocates a staging buffer,
4. `memcpy`s the instance data into it,
5. records a copy into the queue,
6. registers a new resource in the device's tracker, which must later be
   reclaimed, reclamation the driver performs at an unpredictable time, i.e.
   *exactly* the kind of non-deterministic pause the project's thesis is built to
   avoid.

Steps 1, 2 and 6 are pure overhead that a persistent buffer pays zero times.

### Why this is a thesis problem, not only a performance one

RFC-0001 §2's claim has three parts: no garbage collector, no pauses, no VRAM
spikes. PR #148 established that the second was overstated on the CPU side and
corrected the document. The third is currently overstated on the GPU side, and
the honest options are the same two: fix the code or correct the doc.

Here the code is fixable and the fix is small and low-risk, so correcting the doc
would be the wrong choice. **A framework whose central claim is deterministic
memory should not be allocating and freeing GPU resources at the display rate.**

---

## Guide-level explanation

No user-visible surface changes. No `byld` change, no API change, no
configuration.

What changes is what RFC-0030's telemetry reports:

```
  encode.frame       0.6ms      ← was ~6ms
```

and a new counter in `byard dev --profile`:

```
  gpu arena  1.4 MB · high-water 1.4 MB · 0 grows · 1 upload/frame
```

`0 grows` in steady state is the acceptance condition made visible: a nonzero
value after the first seconds means something is churning.

---

## Reference-level explanation

### G1, One arena, not one buffer per pipeline

**Options.** (a) a persistent buffer per pipeline; (b) one shared arena with
per-pipeline offsets.

**Decision: (b).**

(a) is simpler and captures most of the win, but (b) is strictly better on the
term that matters most and is barely harder:

- **One upload per frame instead of nine.** All instance data is staged into a
  single reused CPU-side `Vec<u8>` and uploaded with one `queue.write_buffer`.
  Nine uploads become one; nine staging allocations become zero.
- **One growth policy, one high-water mark**, rather than nine independently
  slack-holding buffers.
- It is the GPU analogue of `ViewArena`, which makes RFC-0001 §2's memory model
  describe both sides of the boundary rather than one.

`wgpu` supports this directly: `set_vertex_buffer(slot, arena.slice(offset..))`
binds a range of one buffer, so a pipeline's draw reads its own region.

```rust
pub struct InstanceArena {
    gpu: wgpu::Buffer,              // usage: VERTEX | UNIFORM | COPY_DST
    staging: Vec<u8>,               // reused; cleared, never reallocated in steady state
    capacity: u64,                  // high-water mark
    grows_this_session: u32,        // telemetry
}
```

Per frame: `staging.clear()` → each pipeline appends its instances and records
`(offset, len)` → one `write_buffer(&gpu, 0, &staging)` → each draw binds its
slice.

### G2, Alignment

Two constraints, and the second is the trap:

- **Vertex buffer offsets** must be a multiple of 4. Every instance struct in
  `frame.rs` is `#[repr(C)]` with explicit padding to 4-byte-or-better alignment
  already, so this is satisfied by construction; the arena asserts it in debug.
- **Uniform buffer offsets** must be a multiple of
  `limits.min_uniform_buffer_offset_alignment`, which is **256 on many
  backends**, not 4, and not a compile-time constant. `backdrop.rs:415`'s
  per-frame uniform is the affected case. The arena pads to the device's reported
  alignment before appending any uniform region, and the value is read from
  `device.limits()` at init rather than hardcoded.

Getting this wrong produces a validation error on some devices and silently wrong
data on others, so both are asserted in debug and covered by a test that runs
against the actual device limits rather than an assumed 256.

### G3, Growth policy

Grow-only, doubling, never shrink within a session.

- Growth reallocates the GPU buffer, which is the operation being eliminated, 
  so it must happen a handful of times at startup and then never.
- **Never shrinking is deliberate.** Shrinking after a large frame would
  reintroduce exactly the recreation churn this RFC removes, at the least
  predictable moment. A UI's instance high-water mark is bounded by the UI; it is
  not an unbounded workload.
- Initial capacity is sized from the first frame's actual usage rounded up to the
  next power of two, so a small app never reserves for a large one.
- `grows_this_session` is exposed for G5's assertion.

### G4, Depth buffers

`mod.rs:2474` and `mod.rs:2520` create separate "depth" buffers alongside the
instance buffers. These carry per-instance draw-order depth (RFC-0011's NDC-z),
not a depth texture. They are ordinary per-instance data and go into the same
arena as another region, there is no reason for them to be separate allocations,
and merging them removes two of the nine per-frame creations outright.

### G5, Observability (INV-18)

The same discipline PR #148 established: this path must be assertable, not
merely benchmarkable.

- A counting allocator test asserts a steady-state frame performs **zero**
  `create_buffer*` calls after warm-up. Implemented with a `#[cfg(test)]` /
  `telemetry`-gated counter on the arena, not by intercepting `wgpu`.
- `grows_this_session == 0` after warm-up on a fixed scene.
- **A pixel-parity test**: the same frame rendered through the arena and through
  the current per-frame-buffer path produces byte-identical output. This is the
  load-bearing test, the change is a pure refactor of *where bytes live*, so any
  pixel difference is a bug, and a golden-image comparison is the only honest way
  to say so.

### G6, What this does *not* change

- No pipeline's vertex layout, shader, or instance struct changes. `frame.rs` is
  untouched.
- The `wgpu` submit path, the scissor decision, and the GPU timer are untouched.
- `RFC-0032`'s dirty set is orthogonal: it reduces *how many* instances are
  uploaded; this reduces *what it costs* to upload any number. They compound and
  neither is a prerequisite for the other.

---

## Drawbacks

**One buffer means one lifetime.** A bug in offset accounting corrupts a
different pipeline's data rather than only its own, so a mistake is less
localised than with per-pipeline buffers. Mitigated by debug assertions on every
region append (offset alignment, region bounds, no overlap) and by G5's pixel
parity test, which catches cross-region corruption immediately and visibly.

**Uniform alignment is device-dependent.** G2 handles it, but it is genuinely the
part most likely to work on the development machine and fail elsewhere. The test
must read `device.limits()`, not assume 256.

**Grow-only holds VRAM at the session high-water mark.** For a UI this is
bounded and small; for a pathological view that momentarily instantiates a very
large number of primitives, the arena keeps that peak reserved. Accepted, and
observable through the counter, so it is diagnosable rather than mysterious.

**The win is projected, not yet measured per-term.** `encode.frame` is ~6 ms in
total; buffer creation is the leading suspect but the sub-scope breakdown does not
exist yet. The implementation plan therefore measures first (see
`IMPLEMENTATION_10.md` M87). If the sub-scopes show buffer creation is a minor
term, this RFC is still correct on thesis grounds but drops in priority, and
that outcome must be recorded rather than quietly ignored.

---

## Rationale and alternatives

**Why not just make each pipeline's buffer persistent (option (a))?** It captures
the allocation win and none of the upload consolidation. Given that the code
touched is the same set of call sites either way, taking the smaller win for the
same edit surface is not a saving.

**Why not a staging-belt / ring of mapped buffers?** The standard high-throughput
pattern, and genuinely better for workloads that saturate PCIe. A UI's instance
data is kilobytes per frame; `write_buffer` is not the bottleneck at that volume,
and a ring introduces fence management and in-flight-frame tracking, real
complexity for a term that is not hot. Reachable later if measurement ever says
so.

**Why not keep `create_buffer_init` and rely on `wgpu`'s internal pooling?**
`wgpu` does not pool buffer allocations across frames in a way that is
guaranteed or portable across backends. Relying on it would make the project's
determinism claim depend on an implementation detail of a dependency, which is
the class of assumption this whole audit exists to remove.

**Impact of not doing this.** The largest term in the frame stays unexplained;
the engine keeps allocating and freeing GPU resources at the display rate; and
RFC-0001 §2's "sin spikes de VRAM" needs an erratum like its neighbours rather
than being true.

---

## Prior art

- **`viewport_buffer` in this crate**, the correct pattern, already present,
  applied once. This RFC is largely "do what `mod.rs:726` does, everywhere".
- **wgpu's own examples and `wgpu_glyph` / `egui-wgpu`**, both use a persistent,
  grow-to-high-water-mark instance buffer with `write_buffer`, for exactly these
  reasons. `egui-wgpu` additionally demonstrates the single-staging-vector
  consolidation.
- **Bevy's `BufferVec` / `RawBufferVec`**, the canonical Rust implementation of
  G1 and G3: a CPU-side `Vec` mirrored to a GPU buffer that grows and never
  shrinks. Worth reading before implementing; the growth and alignment handling
  are the parts to copy.
- **`ViewArena` in this project**, the CPU-side statement of the same idea. G1's
  framing as "the GPU analogue" is deliberate: after PR #148's erratum, the arena
  thesis has no live representative on the hot path, and this gives it one.

---

## Resolved questions

### Q1, One arena or one buffer per pipeline?

**Resolution: one arena (G1).** Same edit surface, additionally consolidates nine
uploads into one and nine staging allocations into zero, and gives RFC-0001 §2's
memory model a GPU-side implementation.

### Q2, How is uniform alignment handled?

**Resolution: read `device.limits().min_uniform_buffer_offset_alignment` at init
and pad every uniform region to it (G2).** Not hardcoded to 256, because it is a
device limit and the value that works on the development machine is the least
useful one to assume. Debug-asserted per append; tested against the real device
limit.

### Q3, Does the arena ever shrink?

**Resolution: no, within a session (G3).** Shrinking recreates the buffer, which
is the operation being removed, at an unpredictable moment. The high-water mark
of a UI is bounded. The reserved size is exposed as a counter so it is
diagnosable rather than hidden.

### Q4, Do the per-instance depth buffers stay separate?

**Resolution: no, they join the arena as ordinary regions (G4).** They are
per-instance vertex data, not depth attachments; nothing justified their separate
allocation, and merging removes two of the nine per-frame creations.

### Q5, Should this land before or after RFC-0032?

**Resolution: either; they are independent (G6).** Different causes, no shared
code. On the current numbers `encode.frame` (~6 ms) is an order of magnitude
above `layout.taffy` (~0.4 ms), so on impact alone this one is first, but
`IMPLEMENTATION_10.md` M87 measures the sub-terms before committing, because
"leading suspect" is not a measurement and this project does not act on those.

### Q6, How is correctness established for a pure-refactor change?

**Resolution: golden-image parity (G5).** Since no pixel is intended to change,
any pixel that changes is a bug, and byte-identical output is both the strictest
available criterion and the cheapest to check. Allocation counters prove the
*performance* claim; the parity test proves the *correctness* one, and neither
substitutes for the other.

---

Implementation-time decisions that surface after merge go to
`support/DESICIONS.md` as `IMPL-NN` entries. This RFC carries no open questions.

---

## Future possibilities

- **Persistent bind groups.** Bind groups are currently rebuilt alongside the
  buffers; with a stable arena they can be created once. Smaller win, natural
  follow-up, and only assessable once the arena exists.
- **A staging belt**, if instance volume ever grows to where `write_buffer`
  matters.
- **Sub-allocating the vector atlas and texture cache from the same arena**,
  which would give the engine a single VRAM budget number to report and enforce, 
  and would make "no VRAM spikes" a testable assertion rather than a design
  intention.
