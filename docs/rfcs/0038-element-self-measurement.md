# RFC-0038: Element self-measurement

- **Status:** Active, implemented. One correction was found while building it,
  and is recorded inline below and in the phase's erratum: the value is
  delivered in the frame's own coordinate space, not in physical pixels, because
  in this engine those are not the same thing and the RFC's stated reason
  ("the space paint and hit-testing use") describes the frame's space.
- **Author(s):** Briany4717
- **Created:** 2026-08-04
- **Last updated:** 2026-08-04

> **Placement:** `docs/rfcs/0038-element-self-measurement.md`

---

## Summary

A small core primitive: a way for an element to observe its own laid-out size as a
reactive value, `on measure => size = it`, where `it` is the resolved content rect
in the frame's coordinate space. It is the one missing piece that lets package widgets — charts,
maps, anything that maps data or tiles onto its own pixel extent — be built in
userspace instead of core, because today a `byld` element cannot know how large it
was laid out unless the author hard-codes a size. It is deliberately tiny: one event,
one value type, no new layout behaviour.

## Motivation

RFC-0037 (Canvas Tier-2) and RFC-0039 (native render extensions) make it *possible*
to draw a chart or a map in a package. But both need the same thing that `byld`
cannot currently express: the widget's own resolved pixel size. A chart maps a value
range onto its height; a map computes how many tiles cover its width; a responsive
component switches layout past a breakpoint. When the size is a literal (`height:
180`) the author can thread it through by hand, but the moment a widget uses `match`
(grow-to-fit) — which every card in the Aura Weather reference does — its real size
is only known after layout, and there is no signal that carries it back. Without this
primitive, "put the chart/map in a package" quietly requires "and always pass explicit
dimensions," which is the kind of papercut that pushes widgets back toward core.

## Guide-level explanation

Any element can observe its measured rect:

```
Column #[width: match, height: match] {
    on measure => size = it        // it : Size { w, h }, the frame's own space

    Chart #[width: size.w, height: size.h] { … }
}
```

- `on measure` fires after the element's rect is resolved for a frame, and again
  whenever that rect changes (a window resize, a sibling reflow). It does **not** fire
  when the rect is unchanged (it rides the RFC-0032 dirty model).
- `it` is a `Size { w, h }` in the frame's coordinate space, the same space paint
  primitives and hit rects are emitted in, and the same one every other size
  value in `byld` is written in. The DPI scale is applied once, to all of them
  together, at the encoder boundary: handing back a pre-scaled number would
  make `Chart #[width: size.w]` twice the size of its parent on a 2× display.
- Writing a `var` from `on measure` feeds the normal reactive path (RFC-0004); a
  native view (RFC-0039) receives the same rect directly in its `measure`/`render`,
  so this event is primarily for `byld`-level widgets and composition.

## Reference-level explanation

**Where the value comes from.** Layout already computes every element's rect via taffy
each frame (`atlas/layout.rs`). This primitive exposes that resolved rect to the
element's own reactive scope as an event fired in a **post-layout** step, after rects
are final for the frame and before paint. It reuses the retained-layout fingerprints
(RFC-0032): an element whose rect fingerprint is unchanged does not fire, so a static
layout costs nothing and there is no per-frame event storm.

**No feedback loop.** The measured size is an *output* of layout, not an input to it.
`on measure` writing a `var` that a *child* consumes (a chart sizing itself to its
parent) is safe because the parent's rect is resolved before its children paint. An
author who feeds a measured size back into the *same* element's own size constraint
creates a layout cycle; this is detected and reported as a compile-time diagnostic
where statically visible, and at runtime the second-order change is clamped to one
resolve per frame (it cannot oscillate unbounded) with a dev-mode warning.

**Cost.** One optional event slot per element that declares `on measure`; elements that
do not declare it pay nothing. The value is read straight from the already-computed
layout rect — no measurement pass is added.

## Drawbacks

- It exposes a post-layout timing point to `byld`, a phase authors did not previously
  see. Bounded to a single read-only value and gated behind an explicit `on measure`,
  so it does not complicate the common case.
- Misused as a same-element size feedback, it can create a layout cycle. Mitigated by
  compile-time detection where visible and a one-resolve-per-frame clamp otherwise.

## Rationale and alternatives

- **Why an event rather than a readable `self.size` expression?** An event fits the
  existing reactive model (writes flow through RFC-0004) and makes the "fires only on
  change" semantics natural. A magically-readable `self.size` would either be a frame
  behind or require the same post-layout hook with fuzzier timing.
- **Why the frame's space?** It is the space charts, maps, and paint already work
  in; handing back a value in any other unit would force every consumer to
  convert it back.
- **Rejected: require explicit dimensions on all package widgets.** Works for fixed-size
  cases but breaks `match`/grow-to-fit, which the reference uses everywhere; it would
  make the package widgets feel second-class next to intrinsics.

## Prior art

SwiftUI `GeometryReader` (a view that reads its own resolved size into the tree); CSS
`ResizeObserver`; Flutter `LayoutBuilder`. `GeometryReader` is the closest match:
size-as-a-readable-output feeding the subtree, with the framework guaranteeing it is
resolved before children build.

## Resolved questions

**Event or readable property?** Resolved: an `on measure` event delivering `it : Size`.
Reasoning: it composes with the existing reactive write path and gives clean
fire-on-change semantics via the RFC-0032 dirty model; a readable property would need
the same hook with muddier timing guarantees.

**Which pixel space?** Resolved: the frame's own, which is what paint, hit-testing
and tile math already use. Reasoning: every consumer of a measured size feeds it
back into a size, a position or a canvas extent, all of which are written in that
space; a value in any other unit would be converted back immediately by every
widget that reads it. The original wording ("physical pixels") named the same
intent through a premise this engine does not hold, that the frame is
post-DPI, and is corrected in the phase erratum.

**How are same-element layout cycles handled?** Resolved: compile-time diagnostic where
statically visible, and a runtime one-resolve-per-frame clamp with a dev warning
otherwise. Reasoning: the safe and common use (parent size feeding a child) must stay
ergonomic, so an outright ban is wrong; the dangerous use must be prevented from
oscillating, so it is caught early when possible and bounded when not.
