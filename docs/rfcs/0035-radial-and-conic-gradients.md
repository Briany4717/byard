# RFC-0035: Radial and conic gradient fills

- **Status:** Active, implemented. `decorated_box` landed 2026-08-04; the
  §"Canvas arc strokes" half landed later, and it needed three corrections of
  its own.

  - **`canvas_shape` had no gradient block at all.** This document says it
    "gains the same kind tag on its stroke colour input", which reads as an
    extension to something that existed. It did not: the Tier-1 shader read a
    flat stroke colour and nothing else. What landed is the whole descriptor,
    read by the same parser and interpolated by `gradient.wgsl`, included
    textually the way `canvas_fill` includes it.
  - **The property is `stroke_gradient:`, not `gradient:`.** A `path`'s
    `gradient:` paints its fill. One name meaning the fill's ramp on one
    command and the stroke's on another would be a rule to remember rather
    than a name to read.
  - **An arc's conic is measured over the arc's own sweep**, not over a full
    turn about the centre of its box. This document's sentence ("the shader
    maps the fragment's angle within the arc's sweep to `t` directly") is
    exactly right and is worth restating as a *correction* to the obvious
    implementation, which is to reuse the shared `conic_t`: over a 180° arc
    that one reaches `t = 0.5` at the far end and spends the rest of the ramp
    behind the shape. It looks nearly right. The shared three-stop
    interpolation is still shared; only the ramp parameter is the arc's.

  Two corrections were found while building the box half and are recorded
  inline below, with their reasoning in the phase's erratum: the kind tag
  cannot live in `misc.w`, and the surface it extends is the existing
  `gradient:` property rather than a `gradient(…)` value inside `bg:`.
- **Author(s):** Briany4717
- **Created:** 2026-08-04
- **Last updated:** 2026-09-04

---

## Summary

`decorated_box` carries exactly one gradient shape today: a directional
three-stop **linear** ramp (`grad_from`, `grad_mid`, `grad_to`, and a
`grad_axis` of `[dir_x, dir_y, mid_pos, offset]`). This RFC generalizes the
gradient lane to also express **radial** (elliptical, center + radius) and
**conic/sweep** (angular, center + start-angle) fills, selected by a one-byte
kind tag that lives in the already-present gradient slot. No new pipeline, no new
instance lane beyond the tag — the fragment shader branches on the tag.

## Motivation

Two elements in the Aura Weather reference cannot be drawn with a linear ramp:

1. The **Air Quality** screen has a soft green radial glow in the top-right of
   the page background. A linear ramp cannot produce a centered falloff; today
   this would require a pre-baked blurred PNG, which defeats theming and scales
   badly across window sizes.
2. The **AQI ring** sweeps colour around its arc (a red-through-green dial). That
   is a conic gradient along the stroke. Approximating it with dozens of solid
   arc segments is possible but wasteful and never quite continuous.

The gradient machinery to support these already exists structurally — three
colour stops and a four-float control vector are encoded per decorated box
(`encoder/decorated_box.rs`, lanes 12–15). What is missing is the interpretation:
the shader only knows how to read the control vector as a linear axis.

## Guide-level explanation

Gradients gain a `kind` and shape-specific geometry, expressed in `byld` as a
gradient literal:

```
// Linear (unchanged; this is today's behaviour spelled explicitly)
bg: gradient(linear, 0x34D399, 0xF9A8A8, angle: 90deg)

// Radial glow, centered top-right, fading to transparent
bg: gradient(radial, 0x0E3D2F, 0x000000,
             center: (1.0, 0.0), radius: 0.9)

// Conic sweep for a dial, starting at 12 o'clock
stroke: gradient(conic, 0xEF4444, 0xF59E0B, 0x34D399,
                 center: (0.5, 0.5), start: -90deg)
```

`center` is in normalized element space (`0..1`), `radius` is a fraction of the
element's half-diagonal, `angle`/`start` are angular. A two-colour call omits the
mid stop (it interpolates a midpoint). The existing bare `bg: (0xAAA, 0xBBB)`
tuple shorthand keeps meaning `gradient(linear, …, angle: 0)` for back-compat.

Conic gradients also apply to Canvas `arc` strokes (RFC-0020 Tier-1), which is
what makes the AQI ring a single stroked arc with a sweep fill rather than a
segment soup.

## Reference-level explanation

**Instance data.** The `grad_axis` lane (`misc`-adjacent `Float32x4`) is
reinterpreted per kind, and a **`grad_kind` lane of its own** carries the tag:
`0=linear`, `1=radial`, `2=conic`, `3=no gradient`.

*(Corrected: this RFC originally put the tag in "the currently-unused high bits
of the gradient present/absent flag (`misc.w`)". `misc.w` is RFC-0031's corner
smoothing, and there is no present/absent flag — presence was inferred from
`grad_axis.xy` being a unit vector, which only worked while every gradient was
linear. The tag needs a lane with exactly one owner, INV-28.)*

The geometry still re-uses `grad_axis`:

| kind   | grad_axis = `[a, b, c, d]`                     |
|--------|------------------------------------------------|
| linear | `[dir_x, dir_y, mid_pos, offset]` (unchanged)  |
| radial | `[center_x, center_y, radius, mid_pos]`        |
| conic  | `[center_x, center_y, start_angle, mid_pos]`   |

**Shader.** The decorated-box fragment shader computes a scalar `t ∈ [0,1]` per
kind, then runs the *same* three-stop interpolation it already runs:

- linear: `t = dot(uv - 0.5, dir) + offset` (today's path).
- radial: `t = length((uv - center) * aspect) / radius`, clamped.
- conic:  `t = fract((atan2(uv.y - cy, uv.x - cx) - start) / TAU)`.

`aspect` corrects the ellipse to the element's real width/height so a radial on a
wide card stays circular unless the author wants otherwise (a future `radius: (rx,
ry)` two-tuple, noted below as deliberately out of scope). Cost is a handful of
ALU ops in a branch already taken for gradients; no extra texture reads, no
overdraw.

**Canvas arc strokes.** `canvas_shape.rs` (Tier-1) carries the whole gradient
descriptor on a new `stroke_gradient:` parameter, read by the same parser a box
gradient goes through and interpolated by the same shared block. For a conic
stroke on an `arc`, the shader maps the fragment's angle within the arc's sweep
to `t` directly, so the ring's colour tracks its geometry. This reuses the arc's
existing analytic angle, adding no tessellation.

*(Corrected: this section was written as though the Tier-1 shader already had a
gradient to tag. It did not. The pipeline is now at exactly the sixteen vertex
attributes every adapter guarantees, which is what paid for the descriptor: the
transform was packed into two attributes rather than four, the way
`canvas_fill` already packs the same seven floats. The next thing added to that
pipeline moves to the record pool.)*

**Compiler.** The existing `gradient:` property gains `kind`, `center`, `radius`
and `start` fields, parsing to a `Gradient { kind, angle, center, radius, stops,
mid_pos, offset }`. `angle`/`start` accept `deg`/`rad` literals;
`center`/`radius` accept normalized floats. Validation: a `radial` `radius` of
zero is refused (it paints a flat wash of the last stop); `conic` normalizes
`start` into `[0, TAU)`, so two spellings of one sweep are the same bytes;
two-stop gradients synthesize the mid stop at `0.5`.

*(Corrected: this RFC wrote the surface as `bg: gradient(linear, …)`. `bg` is a
colour and has never taken a gradient; the gradient has always been its own
property. Adding a second spelling for one concept would have been a larger
change than the feature, for no capability.)*

## Drawbacks

- Conic gradients are the most expensive branch (`atan2` per fragment). It is
  bounded (one transcendental in an already-gated branch) and only paid by
  elements that opt into a conic fill, but it is not free.
- Reinterpreting `grad_axis` per kind means the lane's meaning is tag-dependent,
  which is a small readability cost in the encoder. Documented in the table
  above and asserted in tests.

## Rationale and alternatives

- **Why reuse `grad_axis` instead of adding lanes?** RFC-0033's persistent
  instance arena makes every added lane a per-instance VRAM cost across the whole
  scene. Three kinds fit the existing four-float control vector exactly, so the
  tag-plus-reuse design costs two bits, not sixteen bytes per box.
- **Why put the kind in `misc.w` bits rather than a new flag lane?** `misc.w`
  already encodes gradient present/absent; widening it to a small enum is the
  minimal change and keeps the "gradient" concept in one place.
- **Rejected: pre-baked PNG glows.** Breaks theming (colour tokens can't reach a
  raster), does not scale crisply with window size, and adds texture memory.
- **Rejected: conic-as-segments.** Never continuous, and its cost grows with
  segment count; an analytic conic is O(1) per fragment.

## Prior art

CSS `radial-gradient()` and `conic-gradient()`; SwiftUI `RadialGradient` /
`AngularGradient`; Skia's `SkGradientShader` (linear/radial/sweep share one
shader family, exactly this design). Flutter `SweepGradient`/`RadialGradient`.
The two-bit-tag-plus-shared-geometry approach is how Skia keeps its gradient
shader count down.

## Resolved questions

**Elliptical radial via a two-component radius now, or later?** Resolved: later;
ship a scalar `radius` with automatic aspect correction so circles stay circular.
Reasoning: the reference needs only a circular glow; a `radius: (rx, ry)` form is
a pure additive extension that does not change the lane layout, so deferring it
costs nothing and keeps this RFC focused.

**Reuse `grad_axis` or add a lane?** Resolved: reuse `grad_axis` for the
geometry, with the kind in a four-byte lane of its own. Reasoning: the
persistent instance arena (RFC-0033) makes lanes expensive at scene scale and
three gradient kinds fit the existing control vector exactly, so the sixteen
bytes this RFC set out to avoid are avoided; the tag itself has nowhere honest
to hide, and a lane with two owners is how an encoder starts lying about its own
bytes.

**Conic support on Canvas arcs, or only on boxes?** Resolved: both, because the
AQI ring is an arc stroke, not a box fill, and forcing it into a box would lose
the analytic geometry that makes the arc cheap. Reasoning: the arc already
computes a per-fragment angle; mapping that to gradient `t` is nearly free and
avoids a segmented approximation.

**What does a `gradient:` written before this RFC mean now?** Resolved: exactly
what it meant before, a linear ramp. Reasoning: silent behaviour changes to
existing `.byd` files are unacceptable; the new geometry is opt-in through an
explicit `kind:` and nothing else.
