# RFC-0036: Element-relative overlays — popovers, dropdowns, tooltips

- **Status:** Active, implemented 2026-09-03. An element tags itself `as
  <name>`; an overlay child names it with `anchor_to:` and places against its
  laid-out rect with `anchor_edge`/`anchor_align`/`anchor_gap`, flipping to the
  opposite edge when the first choice would leave the viewport. Guarded by
  `tests/anchored_overlay.rs` with a runnable `examples/anchored_overlay`.

  **Three deltas against this document, as written:**

  - The placement properties are spelled `anchor_edge` / `anchor_align` /
    `anchor_gap`, not the bare `edge` / `align` / `gap` the guide sketches.
    `align` and `gap` are already layout properties on every container, and a
    second meaning for them decided by whether an overlay happens to be the
    parent is exactly the context-dependence the intrinsic catalogue exists to
    prevent.
  - The reference is written as a string (`anchor_to: "searchField"`) rather
    than a bare identifier, which would otherwise parse as a variable read. The
    compile-time guarantee the RFC asks for is kept and is not weakened by
    this: a literal name is resolved against the `as` tags declared before it,
    and a miss is [`UnknownAnchor`] with a nearest-name hint. A *computed*
    reference is allowed through unchecked, deliberately — the check exists to
    catch typos, not to forbid the rare dynamic case.
  - Placement is a paint transform plus the matching hit-rect shift, not a
    second layout pass. The overlay child lays out at its own content size and
    is then moved, which is cheaper than relaying it out and rides two channels
    the engine already has. An anchored child's wrapper is pinned to the origin
    on both axes so it arrives at content size — a stretched panel would be
    placed correctly and still cover the screen.

  **Not implemented:** `width: match(ref)` sizing, and the `on dismiss`
  outside-press convenience (RFC-0017's `dismiss` already covers the scrim
  case). Both are additive and neither is needed by the placement story.

  [`UnknownAnchor`]: the compile error raised for an unknown or forward anchor.
- **Author(s):** Briany4717
- **Created:** 2026-08-04
- **Last updated:** 2026-09-03

---

## Summary

RFC-0017 shipped viewport-relative overlays (an overlay wrapper whose
`justify`/`align` place a child within the viewport) and explicitly deferred
**absolute `(x, y)`** and **`relative(ref)`** anchoring. This RFC delivers the
deferred half: an overlay can anchor to *another element's* laid-out box, so a
dropdown opens under its field, a tooltip points at its trigger, and a popover
hangs off a button — with automatic flipping when it would leave the viewport.
It is the mechanism the Aura Weather location search needs (a results list
anchored to the search field) and the map's hover tooltips.

## Motivation

The reference's top-bar search is an autocomplete: typing filters a list that
appears directly beneath the input, tracking it if layout shifts. Today Byard can
float a panel in the viewport (centered, or pinned to an edge) but cannot say
"below *this* field, left-aligned to it." The deferred `relative(ref)` anchoring
is confirmed absent from the tree (`eval.rs` §overlay wrappers realize an
`anchor` token against the *viewport*, not an element). Without it, every
dropdown, combobox, context menu, and tooltip in the app is either mis-placed or
faked with brittle manual offsets that break on window resize.

## Guide-level explanation

An element can be named as an anchor, and an overlay can target that name:

```
TextField #[value: query, placeholder: "Search locations…"] as searchField
    => query = it

overlay when query != "" {
    anchor(searchField, edge: below, align: start, gap: 6)

    Column #[bg: surface, radius: 12, shadow: lg, width: match(searchField)] {
        for r in results {
            Button(r.name) #[…] => selectLocation(r)
        }
    }
}
```

- `as <name>` tags any element as an anchorable reference (the same token an
  event `it` uses, promoted to a stable per-view id).
- `anchor(ref, edge, align, gap)` places the overlay's box relative to `ref`'s
  laid-out rect. `edge ∈ {above, below, before, after}`, `align ∈ {start,
  center, end}`, `gap` is the offset in px.
- `width: match(ref)` (and `height: match(ref)`) sizes the overlay to the
  anchor, which is what a dropdown wants.
- **Flip-on-overflow** is automatic and on by default: an overlay anchored
  `below` that would clip the viewport bottom flips to `above`. Opt out with
  `flip: false`.

Tooltips are the degenerate case — a small overlay `anchor(trigger, edge: above,
align: center)` shown `when hovered`.

## Reference-level explanation

**Anchor identity.** `as <name>` lowers to a stable `AnchorId` allocated in the
view's arena, recorded on the element's atlas node. Because ids are arena-scoped,
they die with the view; there is no global registry and no cross-view anchoring
(an overlay can only anchor to a reference declared in the same view, checked at
compile time).

**Two-phase placement.** Overlays already lay out after main content
(`eval.rs`: overlays hang off one absolute wrapper). This RFC adds a
*post-layout resolve* for anchored overlays: after the anchor element's rect is
known for the frame, the overlay wrapper's offset is computed from
`edge/align/gap` against that rect, then the overlay subtree lays out at that
offset. This is one extra positioning pass over only the anchored overlays, not a
second full layout — it reuses the retained-layout fingerprints (RFC-0032) so an
overlay whose anchor did not move is not repositioned.

**Flip logic.** After the offset is computed, the overlay's resulting rect is
tested against the viewport. On overflow along the anchor edge, the edge inverts
(`below↔above`, `after↔before`) and the offset recomputes once. A single flip is
guaranteed to terminate; if both orientations overflow (tiny viewport), the side
with more room wins and the overlay is clamped and made scrollable.

**`match(ref)` sizing.** Resolves to the anchor's border-box main-axis extent,
fed into the overlay child's `width`/`height` as a concrete px value during the
post-layout resolve. It is not a layout constraint that could feed back into the
anchor (no cycle): the anchor lays out first and unconditionally.

**Interaction & dismissal.** An anchored overlay participates in hit-testing
above its layer (RFC-0017 z-layers). Light-dismiss (click-outside closes) is
expressed by the author with the existing event model — the overlay's `when`
condition is theirs to control — but this RFC adds an `on dismiss` convenience
event fired when a press lands outside both the overlay and its anchor, so the
common case is one line.

## Drawbacks

- The post-layout resolve introduces an ordering dependency: an overlay cannot
  anchor to an element laid out *after* it. Resolved by requiring the anchor be
  declared lexically before the overlay in the same view (compile-checked),
  which matches how authors read the code anyway.
- Flip + clamp can, in a pathological small viewport, place a dropdown over its
  own field. This is the documented last-resort behaviour and is preferable to
  rendering off-screen.

## Rationale and alternatives

- **Why a post-layout resolve rather than a constraint solver?** Byard keeps raw
  math and graph-solving out of layout deliberately. A one-pass "anchor rect is
  known, place relative to it" is O(overlays), deterministic, and needs no
  solver. A general constraint system would be far more than the reference
  requires and would fight the single-pass philosophy.
- **Why lexical-before-overlay rather than arbitrary references?** It removes the
  possibility of anchor cycles entirely at compile time, so there is no runtime
  cycle detection and no ambiguous frame where an anchor's rect is stale.
- **Rejected: manual pixel offsets (status quo).** Breaks on resize, on font
  changes, and on content-driven layout shifts; not viable for autocomplete.

## Prior art

The web's Popover API + CSS Anchor Positioning (`anchor()`, `position-try` flip);
Floating UI / Popper.js (the flip-and-shift middleware model this borrows);
SwiftUI `.popover`/`.overlay(alignment:)`; Flutter `OverlayPortal` +
`CompositedTransformFollower`. CSS Anchor Positioning is the closest match and
validates `edge/align/gap + auto-flip` as the right primitive set.

## Resolved questions

**Anchor by lexical reference or by arbitrary id lookup?** Resolved: lexical
`as <name>` referenced only within the same view, and only for elements declared
before the overlay. Reasoning: it makes anchor cycles impossible at compile time,
keeps ids arena-scoped (no global registry, no lifetime hazard), and matches the
reading order authors already rely on.

**Is flip-on-overflow default-on or opt-in?** Resolved: default-on, opt-out via
`flip: false`. Reasoning: an autocomplete that silently renders off the bottom of
the window is a bug in the overwhelming majority of cases; the rare fixed-side
overlay is the exception and should be the one that asks.

**Does `match(ref)` create a layout cycle?** Resolved: no — the anchor lays out
unconditionally first, and `match` reads its finished extent during the
post-layout resolve. Reasoning: making anchoring strictly one-directional
(overlay depends on anchor, never the reverse) is what guarantees the single
extra pass terminates and stays deterministic.

**Where does light-dismiss live — engine or author?** Resolved: the author owns
the `when` visibility condition; the engine provides an `on dismiss` event for
outside-press so the common case is one line. Reasoning: overlay visibility is
already reactive state the author controls; inventing an engine-owned "open
state" would duplicate and fight that, whereas an event fits the existing model.
