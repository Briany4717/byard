# Erratum to RFC-0001: the memory model and the dirty-flag model, as actually implemented

- **Status:** Active erratum (amends, does not replace, RFC-0001)
- **Author(s):** Briany4717
- **Created:** 2026-07-25
- **Applies to:** RFC-0001 §2 ("Memory model (Zero-GC)"), §2.1 (`ViewArena`),
  §2.2 (`Signal<T>` dirty flags), and §3.3 ("Dirty rectangles and scissor
  clipping") — specifically, any prose asserting that per-view arenas are on the
  per-frame path, or that a `Signal` mutation yields a minimal dirty-rectangle
  set.
- **Authority:** measurement (`cargo bench --bench atlas`) and the integration
  assertions in `crates/byard-compiler/tests/incremental_paths.rs`.

---

## Why this erratum exists

RFC-0001 §2 describes a memory and invalidation architecture in the present
tense. The mechanisms it names are all real, all correct, and all sound in
isolation — and **the per-frame path does not run through them.** An audit of the
incremental paths found the same shape three times over, and the honest response
is either to change the code or to change the document. This erratum changes the
document; the code side is tracked as a design change, not a bug fix, for the
reason given in C3.

RFC-0001 is **Active**, so its body is not edited: erasing the original text
would erase the record of a design that is still the intended destination. Where
RFC-0001 §2/§3.3 and this erratum disagree about what the engine *does today*,
**this erratum wins**. Where they disagree about what the engine *should
eventually do*, RFC-0001 still stands — nothing here retracts the goal.

This erratum does **not** touch: the four-subsystem split (§1), the frame
boundary and `Send`-POD rule (§5), the Z-bin batching model (§3.2), the layout
delegation to Taffy (§4.1), or the crate dependency direction (§9). All of those
are implemented as written.

---

## Correction

### C1 — `ViewArena` exists, is correct, and is not on the per-frame path

RFC-0001 §2.1 describes a `ViewArena` allocated per mounted view, holding that
view's signals, Taffy node references and spatial-grid entries, reclaimed in
`O(1)` on unmount.

`ViewArena` (`byard-core/src/evaluator/arena.rs`) is implemented exactly as
specified: a `bumpalo::Bump` with type-erased destructor registration, LIFO drop,
`!Send`/`!Sync`, with its own benchmarks. Production receives it and ignores it —
the dev runner binds it as `_arena`. Every piece of persistent interpreter state
(`ForPool`, `anim_clocks`, `scroll_targets`, `when_pools`, …) lives in ordinary
`Vec`s and `HashMap`s on the `Interpreter`; layout lives in Taffy's own
allocator; signals live in `ReactiveCtx`.

Read §2.1 as describing **a facility available for per-view scoping at
instantiation and teardown**, not as a description of where per-frame memory
comes from.

### C2 — "Zero-GC" is true; "no per-frame allocation" is not, and was never measured

RFC-0001 §2's opening claim — "No `Box::new` for per-view allocations… no
deferred collection, no latency spike" — has been read as a claim that the hot
path is allocation-free. That claim had never been measured. It is now:

| Tree (2-level flex) | Rebuild time | Allocations/frame | Bytes/frame |
|---|---|---|---|
| 50 leaves | ~30 µs | 111 | ~134 KB |
| 200 leaves | ~112 µs | 270 | ~562 KB |
| 800 leaves | ~424 µs | 664 | ~2.3 MB |

Measured on the sequence `Interpreter::render` actually runs — `atlas.clear()` →
rebuild every node → `set_root` → full `compute` — on a **reused** atlas, after a
200-frame warm-up, with the benchmark's own scratch buffers hoisted out of the
counted region. Of the 664 allocations at 800 leaves, **477 are the teardown and
reconstruction** and 187 remain on the retained path (Taffy's own per-layout
scratch).

So: there is no garbage collector, no deferred collection and no reference-count
cascade — those parts of §2 are accurate, and they are the parts that rule out
*pauses*. But the per-frame path performs hundreds of heap allocations, because
`TaffyTree::clear()` drops each node's children storage rather than retaining it,
and every node is then reconstructed. "Deterministic" is the defensible word;
"allocation-free" is not, and should not be used of the current implementation.

### C3 — §2.2's dirty flags do not exist for element attributes

RFC-0001 §2.2 says a `Signal` carries "a vector of atomic dirty flags pointing to
specific render or spatial subsystem entries", and §3.3 builds the scissor model
on top of that: a mutation yields a bounding box, and only intersecting
primitives are re-submitted.

The `TargetId` broadcast this describes is implemented, generation-validated and
tested, and the atlas, frame and encoder all have the machinery to consume it.
What does not exist is the **producer**. The interpreter stores element
attributes as raw expressions on the render node and re-evaluates them from
scratch every frame, so there is no reactive edge from a signal to a box's colour
or width. `ReactiveCtx` does track which value *bindings* changed each tick, but
bindings cover `Text` content and `Image` sources only — not the attribute
surface — and there is no binding→node map.

The consequences, all measured or asserted in
`crates/byard-compiler/tests/incremental_paths.rs`:

- `LayoutAtlas::populate_frame` is called with an empty dirty set, so the atlas
  contributes nothing to the frame's dirty union;
- `Interpreter::render` calls `atlas.clear()` every frame rather than
  `mark_dirty_all` + `recompute_dirty`, because without knowing what changed,
  rebuilding is the only *correct* option;
- every primitive the interpreter emits is hard-coded `dirty: true`, and the
  encoder treats every `BoxInstance` as dirty, so the scissor union covers
  essentially the whole frame and the incremental path decides by an
  instance-count heuristic instead.

These were previously classified as three independent defects. They are one:
**the evaluation model that got built does not produce the signal the
invalidation model consumes.** That is a design change, not a call-site fix, and
attempting it as a call-site fix is actively dangerous — a fast path that misses
an invalidation leaves a stale rect that hit-testing still answers from, i.e. an
element that looks like it moved but is tappable where it used to be.

Read §2.2 and §3.3 as **the target design**, not as a description of the current
runtime. RFC-0001's model is worth reaching; the measurements say so (the
retained layout path is 3.8–7.2× cheaper and avoids ~72 % of the per-frame
allocations). Reaching it needs its own RFC.

> **Closed by RFC-0032.** The producer exists: two value fingerprints per
> element, hashed from the resolved values the render walk already computes.
> `Interpreter::render` takes the retained path on an eligible frame,
> `populate_frame` receives the layout-dirty set, and every primitive carries
> `dirty` derived from a comparison against its own resolved values last
> frame — so §2.2 and §3.3 now describe the runtime rather than the intent.
> Two things this erratum did **not** anticipate turned up on the way, and
> both are worth knowing:
>
> - The relay is latest-wins, so a dirty bit published in a frame the render
>   thread skips is simply lost. Harmless while everything was dirty always;
>   the single biggest cost once it is not. `Relay::publish` now merges an
>   unrendered frame's dirty bits into its replacement.
> - The scissor's per-primitive bounds under-covered in three places nobody
>   had reason to notice while the union spanned the whole frame: the
>   antialiased fringe of every analytic pipeline, a wrapping `Text`'s line
>   count, and a drop shadow's reach outside its box.
>
> C1 and C2 are unaffected: there is still no garbage collector, and the hot
> path still allocates. Retaining the Taffy tree removes the largest single
> source of that allocation but does not make the claim "allocation-free"
> true, and it should still not be used.

---

## What this erratum does not do

It does not retract any goal. Every mechanism §2 describes is still the
destination, and two of them are already built and waiting for a producer. The
correction is to the tense, not to the design.

It also does not license leaving the gap unmarked in code. `atlas/mod.rs` and
`LayoutAtlas::populate_frame` carry the same statement at the point a reader
would act on it, and `incremental_paths.rs` holds the acceptance criteria as
`#[ignore]`d tests so the gap appears in test output rather than only here.
