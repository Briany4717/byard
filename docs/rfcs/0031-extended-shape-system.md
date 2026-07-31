# RFC-0031: Extended Shape System — superelliptical corners, shape groups, organic fusion, and morphing

- **Status:** Draft
- **Author(s):** Briany4717
- **Created:** 2026-07-25
- **Last updated:** 2026-07-25
- **Depends on:**
  - RFC-0020 (`Canvas`, `CanvasShape`, the analytic per-fragment SDF evaluator in `canvas_shape.wgsl` — this RFC extends that shader rather than adding a pipeline). *Note: RFC-0020's status line still reads `Draft`; the Tier-1 pipeline is in fact landed (`frame::CanvasShape`, `RenderFrame::push_canvas_shape`, `encoder/canvas_shape.{rs,wgsl}`). That status line should be corrected when this RFC merges.*
  - RFC-0001 (§3.1 render pipelines and the rounded-box field clamp; `frame.rs` as the sole cross-subsystem boundary)
  - RFC-0010 / RFC-0025 (animatable scalars, `Motion`, `anim.keyframes` with its 8-step cap, `anim.spring`, `repeat`/`reverse`/`restart`)
  - RFC-0011 (paint-time transforms — inherited unchanged by every construct here)
  - RFC-0016 (style system — `radius` is a style property and `smooth` joins it)
  - RFC-0023 (paint effects — `blur`; fusion composes with it but does not depend on it)
- **Extends:** `encoder/canvas_shape.wgsl`, `encoder/decorated_box.wgsl`, `encoder/solid_box.wgsl`, `frame::{DecoratedBox, BoxInstance, CanvasShape}`, the `byld` `Canvas` block grammar.
- **Enables:** Material 3 Expressive's shape vocabulary and its morphing loading indicator; interaction-state shape transitions (square → circle on selection); metaball/blob effects; continuous-curvature corners across every box in the framework.
- **Explicitly out of scope:** backdrop refraction (a separate RFC — it belongs to the paint-effect stack of RFC-0023, not to shape definition) and layout-level motion (a separate RFC — it changes the layout/arena contract, which nothing here touches).

---

## Summary

Three additions that share one shader and one new frame primitive:

1. **Superelliptical corners (S1–S3).** `radius` gains a companion `smooth: 0…1`
   on every box-path intrinsic. Implementation is a single substitution in the
   rounded-box SDF — the L² norm becomes an Lⁿ norm — which is *exactly*
   backward-compatible: `smooth: 0` yields `n = 2` and reproduces today's field
   bit-for-bit.
2. **The shape group (S4–S6).** One `CanvasShape` instance may now reference a
   *range* of shape records in a storage buffer, combined by a declared mode.
   This is the single structural change in the RFC, and it is what both of the
   following features need.
3. **Fusion and morphing (S7–S11).** Two combine modes over a group:
   `fuse` (polynomial smooth-minimum union, with colour blended by the same
   blend factor) and `morph` (indexed pairwise SDF interpolation driven by one
   animatable scalar).

The unifying observation is that `canvas_shape.wgsl` is *already* an analytic
per-fragment SDF evaluator with a clean `eval_shape(p, kind, …) -> ShapeDist`
entry point, separate `stroke`/`fill` fields, correct inverse-transform mapping,
and an arc-length parameter for dashes. Fusion is a `smin` over repeated calls to
that function. Morphing is a `mix` over two calls to it. Neither is a new
subsystem; both are blocked only by the fact that instanced rendering gives each
fragment sight of exactly one shape. §2 removes that constraint, and does so once
for both.

Material 3 Expressive's loading indicator — seven shapes, 650 ms each, spring
`stiffness 200 / damping 0.6` — falls out as a group of seven shape records and
**one** animated scalar driven by machinery RFC-0025 already ships.

---

## Motivation

### Byard's boxes look like web boxes

Every surface the framework draws terminates in `sd_rounded_box`, whose corner is
a circular arc. A circular corner has discontinuous curvature at the point where
it meets the straight edge, and that discontinuity is visible — it is the single
strongest reason a UI reads as "web" rather than "native" at a glance. Apple's
platforms, Figma's shape tools, and Material 3's shape library all use
continuous-curvature corners instead.

The fix is one line of shader arithmetic and one style property. There is no
other change in this RFC with a comparable ratio of perceived quality to
implementation cost, and it applies to every element the framework draws rather
than to an opt-in primitive.

### The expressive shape vocabulary is unreachable

Material 3 Expressive shipped ~35 shapes — squircles, scallops, clovers, bursts,
pills — and built shape morphing into the system as a communicative device:
interaction states change shape, and the loading indicator is a continuous morph
through seven of them. Its implementation feature-matches two `RoundedPolygon`s
of cubic Béziers, aligning convex corners, concave corners, and flat edges, then
subdividing both to equal segment counts before interpolating control points.

Byard can express none of this. `CanvasShape` has four kinds — arc, circle, line,
rect — and no way to blend between them. Any design system built on Byard
(`byard-material` in particular, whose gap analysis already blocked on arc
drawing) is limited to the shapes a rounded rectangle can express.

### Fusion is blocked by instancing, not by mathematics

The smooth-minimum union that produces metaball and blob effects is six lines:

```wgsl
fn smin(a: f32, b: f32, k: f32) -> vec2<f32> {
    let h = max(k - abs(a - b), 0.0) / k;
    let m = h * h * 0.25;
    return vec2<f32>(min(a, b) - m * k, select(m, 1.0 - m, a < b));
}
```

The `.y` component is the blend weight, which means fused shapes of *different*
fill colours composite correctly for free rather than being restricted to a
shared colour. The stroke case is free too: the outline of a fused union is
`abs(d_union) - half_w`, which is what one visually wants.

What blocks it is that each `CanvasShape` is one instance with its own quad, so a
fragment sees one shape. That is the only obstacle, it is shared with morphing,
and §2 is its resolution.

### Why one RFC and not three

Corners are independent and could ship alone. Fusion and morphing cannot be
separated: both require N shapes visible to one fragment, both need the same
storage-buffer layout, the same bounded group cap, the same diagnostic, and the
same bounding-box union. Specifying them apart would force the first to
prejudge the group representation for the second, with no review of the
combined constraint. They are one design decision and belong in one document.

---

## Guide-level explanation

### 1. Continuous corners

```byld
Column #[bg: surface, radius: 24, smooth: 0.6, p: 20] { … }
```

`smooth` runs 0 to 1. `0` is the current circular corner and remains the default,
so no existing view changes. `0.6` is approximately the Apple continuous-corner
profile. `1.0` is a pronounced squircle.

It applies wherever `radius` applies — `BoxInstance`, `DecoratedBox`, and the
`rect` shape kind — including shadows, borders, gradients, and backdrop clipping,
because all of them already derive from the same field.

### 2. Shape groups

A `Canvas` block whose shapes should be considered *together* declares how:

```byld
// Two circles that merge as they approach.
Canvas #[width: 140, height: 48, fuse: 16] {
    circle(cx: 24,     cy: 24, r: 18, fill: primary)
    circle(cx: blob_x, cy: 24, r: 14, fill: secondary)
}
```

`fuse: <px>` is the smoothing radius: the distance over which two surfaces bridge
into one. `fuse: 0` or an absent `fuse` is exactly today's behaviour — each shape
is its own instance, unchanged.

### 3. Morphing

```byld
Canvas #[width: 48, height: 48, morph: phase] {
    ngon(n: 4, r: 20, corner: 8,              fill: primary)
    ngon(n: 6, r: 20, corner: 6, inner: 0.85, fill: primary)
    ngon(n: 7, r: 20, corner: 5, inner: 0.75, fill: primary)
}
```

`morph: <scalar>` reinterprets the group as a *sequence*. The scalar indexes it:
`floor(phase)` and `floor(phase) + 1` are the two shapes blended, `fract(phase)`
is the blend. It wraps, so a value sweeping 0 → N returns to the first shape.

Because `phase` is an ordinary animatable scalar, every RFC-0010/0025 modifier
applies with no new surface:

```byld
var phase = 0.0
// The Material 3 Expressive loading indicator: 7 shapes, 650ms each.
phase = 7.0 with anim.linear(4550ms, from: 0.0, repeat: infinite)
```

A spring gives the overshoot-and-settle character M3E uses for state changes:

```byld
phase = selected ? 1.0 : 0.0 with anim.spring(stiffness: 200, damping: 0.6)
```

### 4. `ngon` — the parametric shape kind

One new shape kind covers the great majority of the expressive vocabulary,
because those shapes are overwhelmingly *n*-fold rotationally symmetric rounded
polygons and stars:

```
ngon(n: 6, r: 20, corner: 4, inner: 0.8, rotate: 15deg)
```

| Parameter | Meaning |
|---|---|
| `n` | vertex count, an integer ≥ 3 |
| `r` | circumradius |
| `corner` | corner rounding radius |
| `inner` | inner-radius ratio; `1.0` (default) is a convex polygon, below `1.0` produces a star/scallop |
| `rotate` | rotation of the shape's own axis |

`inner: 0.8` with `n: 8` is a scallop; `inner: 0.4` with `n: 5` is a burst;
`n: 4, corner: r` is a circle approached from the other direction. Combined with
`smooth` on the corner these cover the M3E set without an asset pipeline.

---

## Reference-level explanation

### 1. Superelliptical corners (S1–S3)

#### S1 — the substitution

Today's rounded-box field (`decorated_box.wgsl:142`) ends with:

```wgsl
return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - r_corner;
```

`length()` is the L² norm, which is precisely what makes the corner a circular
arc. Replacing it with the Lⁿ norm makes the corner a superellipse:

```wgsl
fn lp_norm(v: vec2<f32>, n: f32) -> f32 {
    if (n == 2.0) { return length(v); }          // exact, and the default path
    let a = pow(max(v.x, 0.0), n) + pow(max(v.y, 0.0), n);
    return pow(a, 1.0 / n);
}
```

`n` is derived from the style property as `n = 2 + smooth * 4`, so `smooth ∈
[0, 1]` maps to `n ∈ [2, 6]`, with `smooth: 0` short-circuiting to the existing
`length()` call. **A view that does not set `smooth` produces bit-identical
output to today**, which is what makes this safe to apply framework-wide.

The existing clamp (`r_corner = min(r_corner, min(b.x, b.y))`, RFC-0001 §3.1)
is unchanged and still required — the field folds in on itself past half the box
regardless of the norm.

#### S2 — the antialiasing correction

For `n ≠ 2` the result is no longer a true signed distance: the gradient
magnitude deviates from 1 near the corner, reaching roughly 1.15 at `n = 6`. The
`fwidth`-based coverage would therefore produce a corner fringe slightly narrower
than the edge fringe — subtle, but visible as a hardening at the corners on a
large radius, which is the opposite of the effect being pursued.

The correction is to normalise by the analytic gradient rather than to raise the
sample count:

```wgsl
let g = max(length(vec2<f32>(dpdx(d), dpdy(d))), 1e-6);
let coverage = clamp(0.5 - d / g, 0.0, 1.0);
```

This is one extra pair of derivatives on a path that already computes `fwidth`,
and it is correct for `n = 2` as well, so it replaces rather than branches
against the existing path.

#### S3 — where it lands

`smooth` is a style property (RFC-0016), inherited nowhere and defaulting to
`0.0`. It occupies the spare `w` lane of an existing per-instance `vec4` on both
box pipelines — no new vertex attribute, no instance-size growth. `CanvasShape`'s
`rect` kind reads it from `params[4]`, which is currently unused for that kind.

Shadows use the same `n` as the shape that casts them, since a shadow with a
different corner profile than its caster reads as a rendering error.

### 2. The shape group (S4–S6)

#### S4 — representation

`CanvasShape` gains two fields:

```rust
pub struct CanvasShape {
    // … existing fields unchanged …
    /// Combine mode: `GROUP_NONE`, `GROUP_FUSE`, or `GROUP_MORPH`.
    pub group_mode: u32,
    /// `(param, member_count)` — smoothing radius `k` for `FUSE`,
    /// sequence position for `MORPH`; ignored when `NONE`.
    pub group: [f32; 2],
}
```

When `group_mode != GROUP_NONE`, the instance is the **group head**: its
`params` are ignored and its `rect` is the union of its members' bounds, inflated
by `k` for `FUSE` (fusion bulges outward by up to the smoothing radius, and an
under-inflated quad would clip it). Members are the next `member_count` records
in a per-frame shape storage buffer, appended contiguously by
`RenderFrame::push_shape_group`.

The storage buffer is a `Vec<ShapeRecord>` on `RenderFrame`, cleared and refilled
each frame exactly as the existing instance vectors are. `ShapeRecord` is a POD
of the fields `eval_shape` already consumes — `kind`, `params0`, `params1`,
`stroke_color`, `fill_color`, `stroke_dash`, `misc` — so it crosses the RFC-0001
§5 boundary under the same rules as everything else in `frame.rs`. No new
lifetime, no allocation per shape, no `Box`.

#### S5 — the cap

`MAX_GROUP_MEMBERS = 8`, matching RFC-0025's keyframe cap and chosen the same
way: it is the point past which the per-fragment loop stops being free, and past
which a designer is describing something a group is the wrong tool for. Exceeding
it is a compile-time `TooManyGroupMembers` diagnostic pointing at the ninth
shape, not a silent truncation.

The loop is written with a literal bound so it unrolls:

```wgsl
for (var i = 0u; i < 8u; i = i + 1u) {
    if (i >= count) { break; }
    …
}
```

#### S6 — why one instance and not an off-screen field pass

The alternative is to render each member's distance into an `R16Float` target and
resolve in a second pass. It generalises past 8 members and past a single quad.
It also allocates a render target per group, adds a pass, and abandons the
"resolution-independent, atlas-free" property `canvas_shape.wgsl` declares in its
own header comment. For a construct whose entire purpose is small clusters of
nearby shapes, it trades the pipeline's defining characteristic for a
generality nothing has asked for. Rejected.

### 3. Fusion (S7–S8)

#### S7 — the combine

```wgsl
var d_fill = FAR;
var d_stroke = FAR;
var col = vec4<f32>(0.0);
for (…members…) {
    let s = eval_shape(p, rec.kind, cap, half_w, rec.params0, rec.params1);
    if (first) { d_fill = s.fill; col = rec.fill_color; }
    else {
        let r = smin(d_fill, s.fill, k);
        d_fill = r.x;
        col = mix(col, rec.fill_color, r.y);
    }
    d_stroke = min(d_stroke, s.stroke);   // see S8
}
```

Colour follows the same blend factor that produced the geometry, so the colour
transition and the surface bridge are the same event. This is what makes
differently-coloured fusion look deliberate rather than like a z-fighting bug.

#### S8 — strokes under fusion

A per-member stroke minimum (above) draws each member's own outline, including
the parts now interior to the union — visually wrong. The correct fused outline
is the boundary of the union:

```wgsl
d_stroke = abs(d_fill) - half_w;
```

**Decision:** when `group_mode == GROUP_FUSE`, the stroke is derived from the
fused fill field and members' individual stroke widths are ignored; the head's
`stroke_width`, `stroke_color`, `cap` and dash parameters govern. Per-member
strokes inside a fusion group are a `StrokeInFusionGroup` warning (not an error —
the shape still renders correctly, the property is simply inert).

The dash arc-length parameter `t` is not defined on a fused boundary — there is
no closed-form arc length for the union of arbitrary SDFs. Dashes are therefore
unsupported on a fused stroke, diagnosed as `DashOnFusedStroke`. Approximating
`t` would produce dashes that slide unpredictably as the fusion changes, which is
worse than not offering it.

### 4. Morphing (S9–S11)

#### S9 — SDF interpolation, not vertex correspondence

Two designs were evaluated.

**(A) Fractional-`n` polygon.** Make `n` continuous and animate it. Rejected on
inspection: the polar-fold formulation folds `atan2` into a sector of width
`2π/n`, and a non-integral `n` leaves a partial final sector, producing a visible
seam that sweeps around the shape as `n` animates. The artefact is worst exactly
during the animation, which is the only time it exists.

**(B) Interpolate the fields.** `d = mix(d_a, d_b, t)` over two full
`eval_shape` calls. No seam, works between *any* two kinds (a circle can morph to
a 7-pointed star), costs one extra shape evaluation, and requires nothing of the
shapes being related.

**Decision: (B).** Its known weakness is that for two shapes of very different
scale or position the intermediate reads as a melt rather than as a
shape-interpolation, because the linear blend of two distance fields is not the
distance field of any intermediate shape. For the actual use — same-centre,
same-scale members of a design system's shape set — it is indistinguishable from
correspondence morphing, and it is roughly two orders of magnitude less
implementation.

#### S10 — sequence indexing

`group[0]` carries `phase`. With `count` members:

```wgsl
let ph = fract(phase / f32(count)) * f32(count);   // wraps; negatives handled
let i0 = u32(floor(ph));
let i1 = (i0 + 1u) % count;
let t  = fract(ph);
```

Two members and a `phase` in `[0, 1]` is the interaction-state case; seven
members and a `phase` sweeping `[0, 7)` on a linear infinite curve is the M3E
loader. Both are the same code path.

Fill colour is `mix(rec[i0].fill_color, rec[i1].fill_color, t)`, so a morph that
also changes colour is one animation rather than two that can desynchronise.

Colour interpolation uses OKLab, consistent with RFC-0025's keyframe colour
blending and RFC-0010's `bg`/`color` transitions. A morph that blended colour in
sRGB while an adjacent `with` clause blended the same two colours in OKLab would
be a visible inconsistency inside one frame.

#### S11 — why not compile-time feature correspondence

The Material 3 approach — feature-match, align, subdivide, interpolate control
points — is more faithful and would morph arbitrary author-supplied paths, not
just `ngon`s. The Byard-shaped version of it is appealing: run the correspondence
in `byard build`, emit fixed-length control-point arrays exactly as
`vector_atlas.rs` already emits `[BakedGlyph; N]`, and reduce runtime to a lerp
of `[f32; N]` — the same "expensive at build, arithmetic at runtime" posture as
`#[byard_controller]`.

It is deferred, not rejected, for one specific reason: the morphed path cannot be
rendered through the Tier-2 VectorMSDF pipeline, because regenerating an MSDF
field every frame is precisely the cost RFC-0009 exists to avoid. It needs a new
analytic closed-cubic-path SDF — a Tier-1.5 pipeline — which is a larger piece of
work than everything else in this RFC combined and which nothing currently
demands. `ngon` plus field interpolation covers the M3E vocabulary; the general
case can be built when a real use case names it, and S9's grammar accommodates it
without change (a `path` member in a morph group).

### 5. Grammar

Two attributes on `Canvas` and one new shape command:

```
canvas_attr  := … | "fuse" ":" length | "morph" ":" expr
shape_cmd    := … | "ngon" "(" ngon_args ")"
ngon_args    := "n" ":" int ("," "r" ":" length)
                ("," "corner" ":" length)? ("," "inner" ":" float)?
                ("," "rotate" ":" angle)? ("," paint_args)?
style_prop   := … | "smooth" ":" float
```

`fuse` and `morph` are mutually exclusive on one `Canvas`
(`ConflictingGroupMode`). A nested group is not expressible — `Canvas` blocks do
not nest — which bounds the design at one level by construction rather than by a
check.

### 6. Cost

| Construct | Per-fragment cost | Per-frame CPU |
|---|---|---|
| `smooth: 0` | unchanged (short-circuits to `length`) | none |
| `smooth: > 0` | 2 `pow` + 2 derivatives | none |
| `ngon` | ~1 `atan2`, 1 `mod`, ~12 ALU | none |
| `fuse` (m members) | m × `eval_shape` + (m−1) × `smin` | one bbox union |
| `morph` | 2 × `eval_shape` + 2 `mix` | none |

Every group is one draw call, one instance, and one contiguous storage-buffer
append. There is no allocation on the per-frame path: the shape buffer is a
`Vec` cleared and refilled like every other frame vector, so it reaches steady
state after the first few frames and never grows again.

The animation cost is zero by construction. `fuse`'s `k` and `morph`'s `phase`
are ordinary animatable scalars — the same `Motion` values RFC-0010 already
samples on the CPU each frame. An animating morph produces new per-instance data
and never a re-tessellation, re-rasterisation, or cache invalidation. This is the
property `canvas_shape.wgsl`'s header comment already claims for `sweep` and
`dash_offset`, extended to shape identity itself.

---

## Drawbacks

**`mix` of two SDFs is not the SDF of an intermediate shape.** S9 accepts this
knowingly. Between dissimilar shapes the intermediate can bulge or thin in ways a
true correspondence morph would not. It is bounded — both operands are valid
fields, so the result stays continuous and never self-intersects — but it is an
approximation and a designer working outside the same-centre/same-scale case will
notice.

**Superelliptical fields are approximate.** S2 corrects the antialiasing, but the
field's non-unit gradient also very slightly affects shadow blur falloff near
corners at high `smooth`. Under one pixel at practical values, and not corrected.

**Fusion inflates the quad.** A group's bounding box grows by `k` in every
direction, and every fragment in it evaluates the full member loop. Two shapes
far apart with a large `k` therefore pay for a large mostly-empty quad. Bounded
by the 8-member cap and by the fact that widely-separated shapes do not fuse
anyway, but it is a real way to write something slow.

**Fused strokes lose dashes and per-member widths.** S8 diagnoses rather than
approximates. Correct, and still a capability a designer might reach for and not
find.

**`ngon` does not cover everything.** The M3E set is mostly *n*-fold symmetric;
"mostly" is not "entirely". Asymmetric shapes wait for S11's deferred general
path, and this RFC does not pretend otherwise.

**Three features, three pipelines touched.** Corners touch both box shaders,
which is the entire framework's rendering surface. The `smooth: 0` short-circuit
is what makes that acceptable, and it is the first thing a review should check.

---

## Rationale and alternatives

**Why extend `canvas_shape.wgsl` rather than add a pipeline?** Because it is
already an analytic SDF evaluator with the exact structure these features need:
`eval_shape` is a pure function of position and parameters, stroke and fill are
separate fields, and the inverse transform is already applied so evaluation
happens in shape-local space. Fusion is that function called in a loop; morphing
is it called twice. A new pipeline would duplicate all of it.

**Why not tessellate?** A tessellated morph re-generates geometry every frame,
which is the cost model Byard is built to avoid, and it makes shape identity a
CPU-side allocation problem. The analytic path keeps an animating shape as pure
per-instance data.

**Why `smooth` as a separate property rather than a new `radius` syntax?**
`radius` already carries per-corner values and a `Len` form; overloading it would
complicate a heavily-used property to express something orthogonal. A separate
scalar also makes `smooth: 0`'s bit-exact backward compatibility obvious at the
call site rather than buried in a parse rule.

**Why a shape group rather than a `fuse`/`morph` intrinsic each?** Because they
differ only in the combine function. Two intrinsics would mean two storage
layouts, two caps, two diagnostics, and two bounding-box rules for one structural
idea. The mode is a `u32`.

**Impact of not doing this.** `byard-material` cannot implement M3 Expressive's
shape system or its loading indicator, which are not peripheral to that design
language but central to it. And every box Byard draws keeps a corner profile the
platforms it competes with abandoned.

---

## Prior art

- **Inigo Quilez's 2D distance-function catalogue** — the source of the
  polynomial `smin` in S7 (including the blend-factor return used for colour) and
  of the rounded-polygon formulation `ngon` follows.
- **Material 3 Expressive** — the ~35-shape library, and the morphing
  `LoadingIndicator` / `ContainedLoadingIndicator`. Its `androidx.graphics.shapes`
  implementation is the correspondence-morph approach S11 defers: `RoundedPolygon`
  of cubic Béziers, `Morph` feature-matching convex corners, concave corners and
  flat edges, subdividing to equal segment counts. Its timing — 650 ms per shape,
  spring `stiffness 200 / damping 0.6` — is reproducible exactly with S10 plus
  RFC-0025.
- **Apple's continuous corners** — the superellipse corner profile S1 targets;
  the `n ≈ 4`–`5` region of the family.
- **Figma's corner smoothing** — the same superellipse family exposed as a 0–100 %
  slider, which is the precedent for `smooth` being a normalised scalar rather
  than a raw exponent.
- **Flutter's `ShapeBorder` / `MorphableShape`** — the counter-example: morphing
  by path interpolation on the CPU, re-tessellating per frame. Expressive, and
  the exact cost model this RFC exists to avoid.
- **Metaball / blob rendering in demoscene and TouchDesigner work** — decades of
  evidence that `smin` over a small bounded set is a real-time-viable technique,
  and that the interesting parameter is the smoothing radius rather than the
  member count.

---

## Resolved questions

### Q1 — Should `smooth` be a normalised 0–1 scalar or the raw exponent `n`?

**Options.** (a) `smooth: 0…1` mapped to `n ∈ [2, 6]`; (b) expose `n` directly;
(c) named presets (`circular`, `continuous`, `squircle`).

**Resolution: (a).** The exponent is an implementation artefact of the Lⁿ norm and
has no meaning to a designer; values below 2 produce concave corners nobody wants
and would need clamping anyway. A normalised scalar interpolates predictably,
animates sensibly under `with`, and matches Figma's slider, which is the mental
model most users arrive with. Presets were rejected because the interesting values
are between them.

### Q2 — Does `smooth` apply to shadows and borders, or only to the fill?

**Options.** (a) everything derived from the box field; (b) fill only; (c)
independently controllable per layer.

**Resolution: (a).** Shadow, border, gradient clip and backdrop clip all already
derive from the same `sd_rounded_box` call, so applying `n` uniformly is both the
smaller change and the only one that looks correct — a shadow whose corner
profile differs from its caster's reads as a rendering bug. (c) is three more
properties to express something no design system asks for.

### Q3 — What is the group member cap?

**Options.** 4 / 8 / 16 / uncapped with a storage-buffer bound.

**Resolution: 8**, as a compile-time `TooManyGroupMembers` error.

It matches RFC-0025's keyframe cap and is chosen the same way. 4 is too few for
the seven-shape M3E loader, which is the RFC's motivating use case. 16 doubles
the worst-case fragment loop for cases that are better expressed as several
groups. Uncapped removes the unrolled loop bound and makes the cost unbounded on
a per-fragment path, which is not a property this project should give up for
generality nobody requested.

### Q4 — Can `fuse` and `morph` combine on one group?

**Options.** (a) mutually exclusive; (b) morph between two *fused* sub-groups;
(c) fuse a morphing shape with a static one.

**Resolution: (a), diagnosed as `ConflictingGroupMode`.**

(b) requires nested groups, which requires a member to itself be a group head,
which turns a flat contiguous range into a tree and the unrolled loop into
recursion — none of which a fragment shader does well. (c) is expressible today
by placing a morph group and a static shape in the same `Canvas` without fusion
between them, which covers the visual intent at no cost. The flat, bounded,
single-level group is the property that keeps the per-fragment cost provable, and
it is worth more than the composition.

### Q5 — How do strokes behave under fusion?

**Options.** (a) per-member strokes, unioned; (b) the fused boundary only, from
the head's stroke properties; (c) both, selectable.

**Resolution: (b)**, with per-member stroke properties inert and diagnosed as a
`StrokeInFusionGroup` warning.

(a) draws outlines through the interior of the fused body, which is visually
wrong in every case — nobody fuses shapes in order to see the seams. (c) is a
mode switch for an option with no correct use. Warning rather than error because
the shape still renders correctly; the property is merely ignored, and failing a
build over an inert attribute is disproportionate.

### Q6 — Do dashes work on a fused stroke?

**Options.** (a) yes, approximating `t`; (b) no, diagnosed; (c) yes, by
arc-length integration along the fused boundary.

**Resolution: (b), `DashOnFusedStroke`.**

There is no closed form for arc length along the union of arbitrary SDFs. Any
approximation makes dash positions shift unpredictably as the fusion parameter
animates — dashes that crawl and jitter for no reason the author can see, which
is worse than a clear diagnostic. (c) is a per-fragment integration and is not
affordable.

### Q7 — Fractional `n` or field interpolation for morphing?

**Options.** (a) continuous `n` with polar-fold; (b) `mix` of two evaluated
fields; (c) compile-time feature correspondence, per Material 3.

**Resolution: (b)** — see S9 for the seam artefact that eliminates (a), and S11
for why (c) is deferred rather than rejected.

The decisive point for (b) over (a) is that (a)'s artefact appears *only while
animating*, which is the only time the feature is used. The decisive point for
(b) over (c) is that (c) requires a new analytic cubic-path pipeline to render
its output at all, since re-baking an MSDF field per frame is the cost RFC-0009
exists to eliminate. (c) remains reachable: S5's grammar admits a `path` member
without change.

### Q8 — Which colour space for morph and fusion colour blending?

**Options.** (a) OKLab; (b) linear sRGB; (c) match whatever the surrounding
animation uses.

**Resolution: (a), OKLab**, matching RFC-0025's keyframe colour blending and
RFC-0010's `bg`/`color` transitions.

A morph blending in sRGB beside a `with` clause blending the same two colours in
OKLab would desynchronise visibly within one frame. (c) is not implementable — a
shape group has no access to what its neighbours are doing, and it should not.

### Q9 — Where does the morph phase wrap?

**Options.** (a) wrap at `count`, so the last shape morphs back to the first;
(b) clamp at `count - 1`; (c) ping-pong.

**Resolution: (a), wrap.**

The M3E loader is a *loop* through its shapes, which requires the last to return
to the first. Clamping makes an infinite curve stall on the final shape. A
ping-pong is expressible on top of wrapping via RFC-0025's existing
`reverse: true`, so building it into the indexing would duplicate a modifier that
already exists.

### Q10 — Is `ngon`'s `n` animatable?

**Options.** (a) no, `n` is a compile-time integer; (b) yes, via fractional `n`.

**Resolution: (a).** This is Q7 restated at the property level, and the same seam
artefact settles it. `n` is an integer literal; changing shape is what `morph`
is for. Attempting `with` on `n` is a `NotAnimatable` diagnostic pointing at
`morph`, so the error teaches the correct construct rather than merely refusing.

---

Implementation-time decisions that surface after merge go to
`support/DESICIONS.md` as `IMPL-NN` entries. This RFC carries no open questions.

---

## Future possibilities

- **General path morphing (S11).** Compile-time feature correspondence emitted as
  fixed-length control-point arrays, plus the Tier-1.5 analytic cubic-path SDF
  needed to render the result. The grammar already accommodates it.
- **Backdrop refraction.** Displacing the backdrop sample by the shape field's
  gradient produces edge refraction. It composes with fusion to give the current
  "liquid glass" idiom, but it belongs to RFC-0023's paint-effect stack rather
  than to shape definition, and gets its own RFC.
- **Fusion across elements, not only within a `Canvas`.** A dock icon merging
  with its label, or two notification pills bridging. This requires shapes from
  different layout nodes in one group, which crosses into layout territory and
  should wait for the layout-motion work.
- **`ngon` as a clip mask.** RFC-0020 §4 already designs for clip masks; an
  `ngon`-shaped avatar clip is nearly free once they exist.
- **A shape token set in the theme.** RFC-0022's design tokens could carry named
  shapes (`shape.expressive.clover`), making the M3E vocabulary a theme concern
  rather than something each view re-derives.

---

## Ordering

1. **S1–S3** — superelliptical corners. Independent of everything else, touches
   the most surface, and is worth landing alone.
2. **S4–S6** — the shape group and its storage buffer. Structural, no
   user-visible feature by itself.
3. **S9–S10** + `ngon` — morphing. Delivers the M3E loader.
4. **S7–S8** — fusion. Last because it is the only piece with a real overdraw
   cost and benefits from the group representation being settled first.
