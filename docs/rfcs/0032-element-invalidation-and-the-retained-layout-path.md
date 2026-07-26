# RFC-0032: Element Invalidation — value fingerprints, the retained layout path, and a real dirty set

- **Status:** Active — implemented
- **Author(s):** Briany4717
- **Created:** 2026-07-25
- **Last updated:** 2026-07-26
- **Depends on:**
  - RFC-0001 (§2.2 dirty flags, §3.3 incremental scissor, §4.2 spatial grid — this RFC is what makes those three sections describe the runtime instead of the intent; see `0001-erratum-memory-and-dirty-model.md`)
  - RFC-0004 (Mark-and-Pull, and specifically **IMPL-02**'s frame-write value-equality cut, which this RFC generalises from bindings to element attributes)
  - RFC-0013 / RFC-0030 §I1 (the instrumentation that produced the numbers this RFC acts on)
  - RFC-0025 (`endpoint_key` — the existing, blessed precedent for "hash the raw bits to answer *did this change?*" without persisting the values)
  - RFC-0005 (the text measure protocol — the largest single beneficiary, and the largest correctness hazard)
- **Extends:** `byard-compiler::interp::eval` (fingerprint capture during the render walk), `byard-core::atlas::layout` (the retained path), `byard-core::frame` (`dirty` stops being hard-coded `true`), `byard-core::encoder` (the scissor receives a real union).
- **Supersedes in practice:** the `#[ignore]`d acceptance criteria in `crates/byard-compiler/tests/incremental_paths.rs`. Those tests are this RFC's definition of done.
- **Does not cover:** GPU buffer lifetime — that is RFC-0033, a separate and independent cause.

---

## Summary

`support/AUDIT_incremental_paths_and_memory_model.md` found three incremental
paths that production never takes. PR #148 measured them, refused to rewrite the
layout path, and identified why all three fail together: **the evaluation model
does not produce the signal the invalidation model consumes.** Element
attributes are raw `Expr`s re-evaluated from scratch every frame, so nothing can
answer *"did this element change?"* — and every consumer downstream therefore
assumes yes.

This RFC produces that signal, and does so with the **conservative** mechanism
rather than the clever one.

The obvious design is to lower each attribute to a reactive binding, so a signal
write marks it dirty. It is rejected (**R1**). A reactive graph answers "did this
change?" from a dependency edge, and a *missing* edge yields a false negative —
an element whose geometry is stale but whose grid entry is still queryable. That
failure mode is not a stale pixel: it is **an element that looks like it moved
but is tappable where it used to be**, which is exactly the hazard that stopped
PR #148.

Instead: **two 64-bit fingerprints per element**, hashed from the *resolved
values* the render walk already computes — one over layout-affecting attributes,
one over paint-affecting ones. A comparison of resolved values cannot have a
missing edge. Cost: 16 bytes per element (≈12 KB at 800 elements), one hash of a
handful of `f32` bits per element per frame, and no new graph.

The fingerprints then feed the three consumers that have been starved:

| Consumer | Today | With this RFC |
|---|---|---|
| `LayoutAtlas` | `clear()` + full rebuild, every frame | `mark_dirty_all` + `recompute_dirty` when the shape is stable |
| `populate_frame` | receives `&[]` | receives the layout-dirty set |
| Encoder scissor | every primitive hard-coded `dirty: true` | paint-dirty drives the union |

The measured prize is **not** the layout arithmetic. PR #148's cross-check found
`layout.taffy` is dominated by **glyph shaping inside the measure protocol**, not
by tree reconstruction — so the win is skipping the *text* work for clean
subtrees, plus 664 heap allocations and ~2.3 MB per frame at 800 leaves that
`TaffyTree::clear()` does not retain.

---

## Motivation

### The three findings are one finding

PR #148 established this and it is worth restating precisely, because the surface
reading is misleading in a way that costs work:

- `populate_frame(frame, &[])` looks like a one-line defect. It is not. `frame.rects()` and `frame.dirty()` **have no production consumer at all** — passing a correct set would change nothing observable, because it terminates at the frame boundary.
- Every primitive is emitted `dirty: true` and the encoder builds `instances_dirty = vec![true; instances.len()]`, so the scissor union covers the whole frame regardless.
- The retained layout path cannot be enabled because its fast-path condition would have to include *"no layout-affecting attribute changed"*, and that signal does not exist.

Three symptoms, one absence. Fixing any one in isolation is either inert (the
first two) or unsound (the third).

### The measurements that scope this RFC

From PR #148, on a two-level flex tree, Apple silicon:

| Leaves | Production rebuild | Retained (1 dirty leaf) | Ratio | Allocs/frame | Bytes/frame |
|---|---|---|---|---|---|
| 50 | 30.4 µs | 8.0 µs | 3.8× | 111 | ~134 KB |
| 200 | 111.9 µs | 20.1 µs | 5.6× | 270 | ~562 KB |
| 800 | 423.8 µs | 58.6 µs | 7.2× | 664 | ~2.3 MB |

`TaffyTree::clear()` does not retain capacity: `children` is a slotmap of
per-node `Vec<NodeId>`, and clearing drops every one, so each subsequent
`new_leaf` allocates a fresh buffer. ~72 % of the per-frame allocation at 800
leaves is avoidable by not tearing the tree down.

And the caveat that decides what to optimise: the live `layout.taffy` scope
reports **0.38–0.42 ms on a ~40-node tree** — an order of magnitude above the
30 µs the bench gives for 50 box leaves — because the live tree carries wrapping
`Text` and pays glyph shaping inside the measure protocol. **On any real
text-bearing tree, text measurement dominates layout, not tree reconstruction.**

This RFC is therefore justified by two things, in this order: the per-frame
allocation traffic (which contradicts a thesis claim), and the ability to skip
text measurement for clean subtrees (which is the actual time). The µs of tree
rebuilding is third and would not on its own be worth the risk.

---

## Guide-level explanation

Nothing in `byld` changes. No new attribute, no new keyword, no new diagnostic.
This is entirely an engine-internal change whose only user-visible effect is that
frames get cheaper.

What a developer *can* observe, through RFC-0030's telemetry:

```
  interp.render      0.3ms
    layout.taffy     0.04ms   ← was 0.40ms; the text leaves were clean
  encode.frame       0.9ms
```

and, in `byard dev --profile`, a new counter line:

```
  atlas   retained 58/60 frames · 2 full rebuilds · 3 nodes marked
```

which is the answer to "am I on the fast path?" being visible rather than
inferred. A view that rebuilds every frame is now something the developer can
*see*, and usually fix.

---

## Reference-level explanation

### R1 — Value fingerprints, not reactive bindings

**Options considered.**

**(a) Lower each attribute to a reactive binding.** `bind_value` already exists
(`eval.rs:1621`) and the reactive machinery is built. A signal write would mark
the attribute dirty through the existing Mark-and-Pull graph.

**(b) Fingerprint the resolved values.** Keep evaluating as today; hash what the
evaluation produced; compare against last frame.

**Decision: (b).**

Three reasons, in order of weight:

1. **A missing edge is unrecoverable; a value comparison cannot have one.** With
   (a), an attribute expression that reaches a reactive source through a path the
   lowering does not register produces a false "clean" — and the consequence is a
   stale rect that is *still in the spatial grid*, so hit-testing answers from it.
   An element that renders in its new position and responds to taps in its old
   one is a bug a user cannot diagnose and a developer cannot reproduce reliably.
   With (b) the comparison is over the values actually computed this frame; there
   is no edge to miss. For a system whose failure mode is silent and
   input-affecting, the conservative mechanism is the correct one even at a
   higher steady-state cost.
2. **The expensive work is downstream, not the evaluation.** The measurements say
   the cost is in Taffy tree reconstruction, glyph shaping and GPU buffer
   traffic — not in walking `Expr`s. (a) additionally saves the evaluation; (b)
   saves everything else. Paying the larger structural risk for the smaller
   remaining term is the wrong trade *at the currently measured proportions*. If
   `interp.render` self-time later becomes dominant, (a) is still available and
   composes on top of this — the fingerprint becomes a fallback for
   non-lowerable expressions.
3. **It is a generalisation of a decision the project already made.** RFC-0004
   **IMPL-02** chose "Mark-and-Pull with the frame-write value-equality cut only"
   over memo value-versioning, on the same reasoning. RFC-0025's `endpoint_key`
   hashes raw bits to answer "did this retarget?" without persisting values.
   This RFC applies the same shape one level out, which means it inherits an
   argument already reviewed rather than introducing a new one.

**Cost of (b), stated plainly:** every attribute is still evaluated every frame.
This RFC does not make `interp.render` cheaper. It makes everything after it
cheaper.

### R2 — The two fingerprints

```rust
/// Retained per element, indexed by the stable flat index (`flat_ids`).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct ElementFingerprint {
    /// Hash of every resolved value that can move or resize anything.
    layout: u64,
    /// Hash of every resolved value that only changes pixels.
    paint: u64,
}
```

16 bytes per element. At 800 elements, 12.8 KB — three orders of magnitude below
the 2.3 MB/frame this replaces.

Hashing follows `endpoint_key`'s existing pattern: `DefaultHasher` over
`f32::to_bits()` / discriminants, never over `f32` directly (`NaN != NaN` would
make an element permanently dirty, and `-0.0 == 0.0` would make it permanently
clean — both wrong, and the second silently).

**Classification is by attribute, at lower time, from a static table.** It is not
a heuristic and not per-value:

| Class | Attributes |
|---|---|
| `layout` | `width`, `height`, `gap`, `p`, `m`, `grow`, `align`, `justify`, `direction`, `wrap`, `font_size`, text content, `absolute`, grid tracks/placement, `scroll_axes` |
| `paint` | `bg`, `color`, `radius`, `smooth`, `border_*`, `shadow_*`, `opacity`, `gradient*`, `blur*`, `transform`, `dash*`, `fill`, `stroke` |

**INV-8 becomes enforceable.** RFC-0010 asserts "an animated property must never
trigger relayout"; nothing checks it today. With this table, an animated
attribute in the `layout` column is a lower-time diagnostic
(`AnimatedLayoutAttribute`), so the invariant is enforced at the surface rather
than assumed in prose. A `transform`-based motion is the supported alternative
and the diagnostic names it.

**Text content is layout-class.** This is the single most important row in the
table: it is what lets a clean text leaf skip glyph shaping, and it is also what
makes a missed classification produce un-wrapped text (see R5).

### R3 — Marking, and who owns geometry

The rule that makes this sound, and the reason PR #148's blocker does not apply:

> **Fingerprints decide what to *mark*. Taffy decides what to *recompute*. The
> grid is rebuilt from resolved rects, never from fingerprints.**

Concretely, per frame, when the retained path is eligible (R4):

1. Walk the tree as today, evaluating attributes and computing both fingerprints
   per element.
2. For each element whose `layout` fingerprint differs from last frame's,
   `mark_dirty_all` its `TargetId`.
3. `recompute_dirty(viewport)`. **Taffy's own dirty propagation handles the
   sibling-reflow case** — a node whose rect shifted only because a sibling
   resized is recomputed by Taffy, not by us. This is precisely the hazard
   IMPL-42 named as the reason not to hand-roll a partial grid update, and the
   resolution is to not hand-roll it: we never compute which *rects* changed, only
   which *inputs* changed.
4. `rebuild_grid` runs its full walk, unchanged, over the **actual resolved
   rects**. IMPL-42 measured this and kept it deliberately; that decision stands
   and is now load-bearing — it is what guarantees no grid entry can be stale.
5. `populate_frame` receives the layout-dirty target set.
6. Primitives carry `dirty = (paint fingerprint changed) || (layout dirty)`.

A false *positive* (marking something clean as dirty) costs a recompute. A false
*negative* is impossible for inputs, and for outputs cannot occur because step 4
does not consult fingerprints at all.

### R4 — Retained-path eligibility

The retained path is taken only when **all** of these hold. The list is
deliberately a whitelist and the default is the full rebuild:

- `reconcile_structure` returned `false` (no pool or slot changed) — the signal already exists at `eval.rs:3628` and is already driven to a fixed point at `eval.rs:3420`
- no resize
- no hot reload applied this tick
- no theme change
- no overlay or route mount/unmount
- the retained `flat_ids` length matches this frame's walk

`flat_ids` (`eval.rs:3436`) becomes a retained field rather than a local, and the
retained path **must reuse the stored `AtlasNodeId`s** — `next_target_index()` is
`nodes_by_index.len()`, so ids are assigned in build order and re-deriving them
would silently reassign.

> **Erratum, added while closing the phase.** Two corrections to this section,
> both found by asking the question INV-18 asks — *which assertion fails when
> production stops taking this path?* — of the whitelist itself.
>
> 1. **The overlay/route clause is defence in depth, not the sole guard.** It
>    was justified on the grounds that those pools do not travel through
>    `reconcile_structure`. They do: `reconcile_structure` descends into
>    `RenderNode::Nav`, and an overlay mounts behind a `when`. Every case the
>    suite can construct is rejected by both clauses, so neither is individually
>    necessary — and both stay, because a *deny* clause that fires redundantly
>    costs a rebuild, while removing one costs correctness.
> 2. **A frame this whitelist wrongly admits was indistinguishable from one it
>    rejected.** `end_retained_build` refuses the build and the caller clears
>    and rebuilds, landing on exactly the same `clears` / `full_computes` /
>    `retained_recomputes` as a clean rejection — so every clause here could be
>    deleted with the suite still green, while production walked the tree twice
>    on every overlay toggle. `path_counters` now records
>    `retained_attempts` and `retained_rollbacks`, and the eligibility tests
>    assert the first is zero. That is what makes this list load-bearing.

Anything not on this list forces the full path. New conditions are added one at a
time, each with its own test. **A condition may never be added because a
benchmark improved.**

### R5 — The text hazard, and why it gets its own section

`recompute_dirty` runs the measure protocol **with no sizer**, so a wrapping
`Text` leaf falls back to its natural single-line size. On a naive retained path
this silently un-wraps every wrapping text leaf on the frame after any retained
frame. It is documented in `DESICIONS.md` under the RFC-0005 wrap entry and is
the single most likely way this RFC ships a visible bug.

**Resolution:** `recompute_dirty` gains a `recompute_dirty_with_text(viewport,
sizer)` sibling, mirroring `compute` / `compute_with_text`, and the retained path
always uses it. The measure callback is only invoked by Taffy for nodes it is
actually recomputing, so a clean text leaf is never re-shaped — **which is the
principal win of this RFC**, and it falls out of Taffy's own dirty propagation
rather than from anything we have to be careful about.

The existing sizer-less `recompute_dirty` is retained for the benches and marked
`#[doc(hidden)]`-adjacent with a note that no production path may call it.

### R6 — What the fingerprint is computed *from*

The fingerprint must be over **resolved** values, not over the `Expr`. Two
consequences worth stating because getting them wrong is subtle:

- An animated attribute resolves to a new value each active frame, so its
  fingerprint changes and it marks itself dirty — no special case needed, and
  RFC-0025's offscreen pause stops the marking for free when the animation stops
  being sampled.
- A `theme.token` reference resolves through the current scheme, so a scheme flip
  changes every dependent fingerprint. Theme change is nonetheless on R4's
  forced-rebuild list, because a scheme flip typically changes nearly everything
  and the marking pass would cost more than the rebuild.

### R7 — Observability (INV-18)

The counters PR #148 added under `atlas::layout::path_counters` become the
acceptance surface:

- `full_computes` must be ~0 in a steady-state animating scene.
- `targets_received` must be > 0 on any value-only frame.
- `targets_matched / targets_received` near 1 — a low ratio means generation-stale
  targets, which is a caller bug the counters distinguish from "passed nothing"
  (PR #148 added `walk_and_push`'s matched count for exactly this).

These are surfaced in `byard dev --profile` (RFC-0030 §V1) so the fast path is
visible during development, not only in CI.

---

## Drawbacks

**Every attribute is still evaluated every frame.** This RFC does not reduce
`interp.render` self-time; it is explicitly the trade taken in R1. If that term
later dominates, a second RFC lowers attributes to bindings and uses fingerprints
as the fallback for what cannot be lowered.

**Hash collisions are theoretically possible.** A 64-bit collision on two
different value sets would produce a false clean. At one hash per element per
frame, the probability is negligible, but it is non-zero and it is the one path
by which the "cannot have a missing edge" claim is imperfect. Stated rather than
hidden. `DefaultHasher` is not adversary-resistant; nothing here is adversarial.

**Retained state must be invalidated correctly on every structural change.** R4's
whitelist is the whole safety argument, and a future contributor adding a
structural mutation without adding it to the list re-introduces the stale-rect
hazard. The mitigation is that the default is rebuild and the list is a
whitelist, so a *new* mutation that nobody classified falls through to the safe
side only if `reconcile_structure` reports it — which is why R4 leads with that
condition rather than treating it as one of several.

**The classification table is a maintenance surface.** A new attribute added
without a class is a latent bug. Mitigated by making the class a required field
of the attribute definition rather than a lookup — a new attribute does not
compile without one.

**Two mechanisms now answer "did this change?"** — the reactive graph for
structure, fingerprints for attributes. That is a real conceptual cost. It is
accepted because they answer different questions at different granularities, and
because merging them is what option (a) was.

---

## Rationale and alternatives

**Why not fix the encoder first?** RFC-0033 does, and the two are independent —
different causes, no shared code. PR #148's instrumentation put `encode.frame` an
order of magnitude above `layout.taffy`, so RFC-0033 is in fact the larger
immediate win. They are separate RFCs precisely so neither blocks the other.

**Why not just retain the Taffy tree without any dirty tracking?** i.e. rebuild
node styles in place every frame but never `clear()`. This removes the allocation
traffic (the largest measured harm) with none of R1–R6's complexity. It is a
legitimate smaller step and is called out in the implementation plan as a
fallback if R1–R6 prove riskier than projected — but it leaves the text-shaping
win, which is the actual time, entirely on the table.

**Why not per-subtree fingerprints instead of per-element?** Coarser marking,
fewer hashes, but a change anywhere in a subtree dirties all of it — including
every text leaf under it, which is the work we are trying to skip. The
granularity has to match the thing being skipped.

**Impact of not doing this.** The three incremental paths stay inert; RFC-0001
§2.2 and §3.3 keep describing an intent rather than a runtime; per-frame
allocation stays in the hundreds; and layout motion, shared-element transitions
and exit animations — all of which need per-element change information — stay
blocked behind the same absence.

---

## Prior art

- **RFC-0004 IMPL-02** (this project) — the frame-write value-equality cut chosen over memo versioning. Same reasoning, one level in.
- **RFC-0025 `endpoint_key`** (this project) — hashing raw bits to detect change without persisting values. This RFC reuses the pattern and its justification verbatim.
- **React's `memo` / shallow prop comparison** — the same conservative choice: compare resolved props rather than trust a dependency graph. React additionally demonstrates the failure mode of the alternative (stale closures from missing hook dependencies), which is (a)'s hazard in a language that cannot check it either.
- **SwiftUI's `Equatable` view modifier** — an explicit opt-in to exactly this comparison, which suggests the mechanism is sound but that making it *automatic* (as here) is the better default when the values are cheap to hash.
- **Flutter's `RenderObject.markNeedsLayout` / `markNeedsPaint` split** — the direct precedent for R2's two classes, including the invariant that a paint-only change must never mark layout. Flutter enforces it by having separate methods; this RFC enforces it by a lower-time diagnostic, which is stricter.
- **Taffy's own dirty propagation** — R3 delegates to it rather than reimplementing, which is the lesson IMPL-42 already recorded when it declined to hand-roll a partial grid update.

---

## Resolved questions

### Q1 — Reactive bindings or value fingerprints?

**Resolution: fingerprints (R1).** The decisive factor is not cost but failure
mode: a reactive graph's missing edge yields a stale-but-queryable rect, i.e. an
element tappable where it used to be. A value comparison has no edge to miss.
Bindings additionally save only the evaluation term, which the measurements say
is not the dominant one. Bindings remain available later and compose on top.

### Q2 — One fingerprint or two?

**Options.** (a) one combined; (b) layout + paint; (c) one per attribute.

**Resolution: (b), two.**

One combined fingerprint would mark layout dirty for a colour change, which
defeats the entire purpose — colour changes are the most common frame-to-frame
delta in a real UI. (c) is 8 bytes per attribute instead of 16 per element, an
order of magnitude more memory and more hashing, to enable a granularity nothing
consumes: Taffy marks whole nodes, and the scissor unions whole primitives.

### Q3 — What happens to `dirty: true` on primitives?

**Options.** (a) drive it from the paint fingerprint; (b) leave it hard-coded and
let the encoder decide; (c) remove the field.

**Resolution: (a).** (b) is the current state and is the finding. (c) is
tempting — the field would be redundant if the encoder computed its own union —
but the interpreter is the only party that knows *why* a primitive changed, and
moving that inference into the encoder would put a paint concern in a subsystem
that must not reason about `byld` semantics (RFC-0001's dependency direction).

### Q4 — Does the retained path need its own text sizer entry point?

**Resolution: yes — `recompute_dirty_with_text` (R5).** The alternative is
forcing the full path for any view containing a wrapping `Text`, which is nearly
every real view, and would reduce this RFC to a no-op on exactly the trees whose
text shaping it exists to skip.

### Q5 — What forces a full rebuild?

**Resolution: R4's whitelist, default-deny.** Structural change, resize, hot
reload, theme change, overlay/route mount-unmount, and any `flat_ids` length
mismatch. The default is the full path and conditions are added individually with
tests. Explicitly: **no condition may be added on the strength of a benchmark
alone** — the failure mode is silent and input-affecting, so cost is never a
sufficient argument.

### Q6 — Is a theme change marked or rebuilt?

**Options.** (a) mark every dependent element; (b) force a full rebuild.

**Resolution: (b), rebuild.** A scheme flip changes nearly every resolved value,
so the marking pass would visit everything and then recompute everything, costing
strictly more than the rebuild it replaced. It is also rare and user-initiated,
so a one-frame cost is imperceptible.

### Q7 — Hash function?

**Options.** (a) `DefaultHasher` (SipHash), as `endpoint_key` uses; (b) FxHash,
already a dependency via `rustc_hash` in `atlas/layout.rs`; (c) a hand-rolled
FNV.

**Resolution: (b), FxHash.** `endpoint_key` uses `DefaultHasher` and that
precedent argued for (a), but `endpoint_key` runs once per *animation*, while
this runs once per *element per frame* — a different order of magnitude. FxHash
is already in the dependency graph, is substantially faster on the short integer
sequences being hashed here, and nothing about this is adversarial. `endpoint_key`
is left as-is; changing it is out of scope and would need its own justification.

### Q8 — Can an animated attribute be layout-class?

**Resolution: no — `AnimatedLayoutAttribute`, a lower-time diagnostic.**

RFC-0010 INV-8 already states that an animated property must never trigger
relayout; nothing enforced it, and R2's table is the first structure that can.
Allowing it would mean an animation marks layout dirty every active frame,
recomputing the tree at the display rate — reproducing exactly the behaviour this
RFC removes, while looking like a feature. The diagnostic names `transform` as
the supported alternative, so the error teaches the correct construct.

---

Implementation-time decisions that surface after merge go to
`support/DESICIONS.md` as `IMPL-NN` entries. This RFC carries no open questions.

---

## Future possibilities

- **Attribute bindings (option (a)) as an additive layer**, if `interp.render`
  self-time ever dominates. Fingerprints become the fallback for expressions that
  cannot be lowered, so the conservative guarantee survives.
- **Layout motion** (a future RFC) needs per-element change information and
  retained per-element geometry. R2's fingerprints and R4's retained `flat_ids`
  are two of its three prerequisites; the third is content identity (RFC-0002
  **D7**'s keyed reconciliation, whose trigger has now fired).
- **Shared-element transitions** across routes — the same retained geometry, keyed
  across two trees instead of two frames.
- **A `byard dev` "why did this frame rebuild?" report** — R4's whitelist is a
  small enum; naming which condition fired turns a performance mystery into a
  one-line answer.
