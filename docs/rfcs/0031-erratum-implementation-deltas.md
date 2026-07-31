# Erratum to RFC-0031: five things the design got wrong, and what replaced them

- **Status:** Active erratum (amends, does not replace, RFC-0031)
- **Author(s):** Briany4717
- **Created:** 2026-07-31
- **Applies to:** RFC-0031 §S2 (the antialiasing correction), §S3 (where
  `smooth` lands), §S4 (the group's dirtiness), §S7 (the `smin` snippet), §S8
  (where a fused group's stroke comes from), §S10 (the loader's syntax), and
  §Q10's relationship to the existing `LayoutPropNotAnimatable` diagnostic.
- **Authority:** the implementation, and the tests named against each
  correction below. RFC-0031 was **Draft** while this work landed, so its body
  has been corrected in place; this erratum exists because a reader who only
  sees the corrected text learns nothing about why it changed.

---

## Why this erratum exists

RFC-0031 shipped as written in every decision that mattered — the Lⁿ
substitution, the shape group, field interpolation over fractional `n`, the
polynomial `smin`, the eight-member cap, OKLab colour, the wrapping phase. Its
Resolved questions were re-derived during implementation and none of them
moved.

What did move is smaller and more specific: two numbers that were the wrong sign
or the wrong size, two places where a described mechanism did not exist, and one
transcription bug that would have shipped as a silent colour inversion. All five
are the kind of thing that is only discoverable by building the thing, which is
what an erratum is for.

Three of them share a shape worth naming, because it is the same shape as the
defect RFC-0030's erratum found: **the RFC described the correct behaviour and
attached it to a mechanism that could not deliver it.** §S2 corrects
anti-aliasing in a place that leaves shadows uncorrected; §S3 puts a value in a
lane that does not exist; §S8 reads a property from a syntax that was never
written. In each case the *intent* was right and the *anchor* was not.

---

## Correction 1 — §S2 has the artefact backwards, and corrects it in the wrong place

§S2 says:

> For `n ≠ 2` the result is no longer a true signed distance: the gradient
> magnitude deviates from 1 near the corner, **reaching roughly 1.15 at `n = 6`**.
> The `fwidth`-based coverage would therefore produce a corner fringe slightly
> **narrower** than the edge fringe.

Both halves are wrong, and the second is the important one.

**The number.** On the corner diagonal the Lⁿ norm's gradient magnitude is
exactly `2^(1/n − 1/2)`. At `n = 2` that is 1, as it must be. It *falls* from
there — 0.79 at `n = 6`, asymptotically 0.707 — so an uncorrected field draws
the corner's anti-aliased fringe about **26 % wider** than the edge's. The
artefact is a smeared corner, not a hardened one: the opposite of what §S2
predicts, on the shapes the property exists to give a cleaner profile.

**The place.** §S2's remedy is a coverage formula in the fragment stage:

```wgsl
let g = max(length(vec2<f32>(dpdx(d), dpdy(d))), 1e-6);
let coverage = clamp(0.5 - d / g, 0.0, 1.0);
```

That corrects anti-aliasing and nothing else. RFC-0031's own Drawbacks section
concedes as much — "the field's non-unit gradient also very slightly affects
shadow blur falloff near corners … not corrected" — and it also leaves the
border band, the backdrop clip and the ripple clip reading an unnormalised
field.

**What landed instead.** The gradient is computed *analytically* alongside the
norm, and the field is normalised by it inside `sd_rounded_box`:

```wgsl
let lp = lp_norm(corner, n);
return (inner + lp.x - r_corner) / lp.y;
```

One division, at the source, and every consumer is corrected together: edge
coverage, the border band, shadow blur falloff, and all three clip masks. It is
also free on the default path, because the `n == 2` short-circuit returns before
it — and it needs no per-consumer bookkeeping, because there is nothing left for
a consumer to know.

The division is exact where it matters and a no-op everywhere else: `inner` is
non-zero only where one of the corner components is zero, and there the gradient
is 1 by construction. So the straight edges are untouched, bit for bit.

**Tests.** `the_corner_fringe_is_as_wide_as_the_edge_fringe` (CPU, in
`encoder::tests`) asserts unit slope after normalisation *and* that the
uncorrected field undershoots it — so the tolerance is not vacuous. The
same-named GPU readback test measures the two fringes on a real device and
requires them within one physical pixel.

---

## Correction 2 — §S3's "spare `w` lane" does not exist on either box pipeline

§S3 says:

> It occupies the spare `w` lane of an existing per-instance `vec4` on both box
> pipelines — no new vertex attribute, no instance-size growth.

There was no spare lane on either. `DecoratedInstance`'s `params` is
`(border_width, shadow_dx, shadow_dy, shadow_blur)`, its `misc` is `(opacity,
depth, shadow_spread, gradient_flag)`, and its `grad_axis` is `(dir_x, dir_y,
mid_pos, offset)`. `BoxInstance` is `rect`, `color`, `radii` and a full
`Transform`.

**`DecoratedBox` gets a lane back rather than growing one.** The gradient
present/absent flag in `misc.w` was redundant: `grad_axis` is `(cos θ, sin θ,
…)` for a real ramp and all-zero without one, so the shader can answer the same
question from data it already reads —

```wgsl
let has_gradient = dot(in.grad_axis.xy, in.grad_axis.xy) > 0.25;
```

— and `misc.w` now carries `smooth` at zero cost. This is the outcome §S3
assumed, arrived at by removing something rather than by finding it.

**`BoxInstance` grows by one `f32`** (80 → 84 bytes), declared last so every
existing field offset is unchanged and the hand-written vertex layout keeps its
literal offsets. The alternative — promoting any smoothed box to the
`DecoratedBox` pipeline, as translucency and gradients already do — was
rejected: it costs a whole extra pipeline pass per use, on exactly the case the
property exists to serve, in exchange for four bytes on an instance buffer that
is already small.

---

## Correction 3 — §S3 stops one pipeline short of what §Q2 requires

§S3 lists where `smooth` lands: "`BoxInstance`, `DecoratedBox`, and the `rect`
shape kind — including shadows, borders, gradients, and **backdrop clipping**".
§Q2's reason is stated in one sentence and it generalises further than the list:
*a shadow whose corner profile differs from its caster's reads as a rendering
bug.*

So does a blur pane. So does an ink ripple that squares off inside a squircle
button, and so does a rounded `Image` sitting in a squircle card. All four clip
to an element's outline and all four had their own copy of `sd_rounded_box`.

`smooth` therefore reaches six shaders, not three: `solid_box`,
`decorated_box`, `canvas_shape`, `backdrop`, `ripple` and `texture_sampler`.
Three of them had a genuinely spare lane (`backdrop`'s `params.w`,
`texture_sampler`'s `misc.z`); `RippleInstance` gained one `f32` beside its
depth.

**What is deliberately *not* included**, and the rule that decides it:
**`smooth` applies to the surfaces whose `radius` the author controls.**
Widget-owned geometry — a `Toggle` track, a `Slider` thumb, a `RadioButton` dot,
a `Checkbox` container — derives its own radii and is drawn as pills and circles
rather than rounded rectangles. Extending the property there is a separate
design question with its own answer, not a consequence of this one.

---

## Correction 4 — §S4's representation admits a stale group (INV-26)

§S4 defines a group head and its members and says nothing about dirtiness. That
is a real gap, not an omission of detail.

`PaintDigest` compares a primitive by its own resolved bytes at its own pool
position. A head's bytes are its mode, its parameter, its colours and its rect.
**Its members are not in it** — they live in the shape-record storage buffer. So
a fusion group with a static `k` whose member circle moves has an unchanged
head, is judged clean, and renders the previous frame's shape.

A `morph` group escapes this by accident, because its parameter is the phase and
the phase moves. The accident is not something to rely on: it is exactly the
kind of thing that works in the example and fails in an app.

**The head folds a hash of its member records**, computed on the same pass that
appends them, so there is no way to add a member and forget it. The hash is over
raw bytes, for the reason RFC-0032's fingerprints already documented: `NaN !=
NaN` makes a group permanently dirty, which is wasteful and visible, while
`-0.0 == 0.0` makes it permanently *clean*, which is silent and wrong.
`group_first` is deliberately excluded — it is a pool position, not a value.

This generalises past this RFC, so it is stated as an invariant:

> **INV-26 — a primitive's dirtiness must cover everything that determines its
> pixels.** Any data a shader reads that lives *outside* the primitive — a
> storage-buffer record, an atlas entry, a uniform — must be folded into that
> primitive's digest, or a change to it renders stale.

The shape group is the first construct in the engine where a shader reads
per-draw data that is not in the instance itself, which is why the invariant did
not need writing down before.

**Tests.** `a_group_whose_only_change_is_a_member_is_dirty` is demonstrated red
in the same test, by taking the member hash back out of the head and showing the
digest then reports the group clean. `an_animated_member_repaints_its_fusion_group`
is the end-to-end form: the moving circle stays inside the union two fixed ones
already span, so every byte of the head is identical frame to frame, and the
test asserts that byte-identity *and* that the group is still dirty.

---

## Correction 5 — §S7's `smin` snippet inverts every fused colour

§S7 quotes the polynomial smooth-minimum, transcribed from GLSL:

```wgsl
fn smin(a: f32, b: f32, k: f32) -> vec2<f32> {
    let h = max(k - abs(a - b), 0.0) / k;
    let m = h * h * 0.25;
    return vec2<f32>(min(a, b) - m * k, select(m, 1.0 - m, a < b));
}
```

GLSL's ternary is `cond ? a : b`. WGSL's is `select(false_value, true_value,
cond)` — **the arms are reversed** — and the transcription keeps the GLSL order.

The `.y` component is the weight *towards `b`*, so it must be small where `a` is
the closer surface. With the arms swapped it returns ≈1 exactly there: the far
member's colour paints the near member's body, and every fused colour inverts.

The geometry is unaffected — `min(a, b) - m * k` is correct either way — so
nothing about the silhouette hints at it. It is caught only by looking at what
colour a fused body actually is.

Corrected to `select(1.0 - m, m, a < b)`, with the reason commented at the site.
Caught by `fusion_bridges_nearby_shapes_and_carries_their_colours_across`, which
is the argument for that test asserting colour and not only coverage.

---

## Correction 6 — §S8 reads a stroke from a syntax that does not exist

§S8 decides that under fusion "the head's `stroke_width`, `stroke_color`, `cap`
and dash parameters govern", and §Q5 makes a *member's* stroke an inert property
with a warning.

There is no group-level stroke syntax. `fuse:` takes a length; a `Canvas` has no
`stroke:`; the head is synthesised by the evaluator and has nothing to read.

**The group's one outline comes from the first shape's stroke** — the only place
it can come from — and only a *later* member's is inert. That is what the
`StrokeInFusionGroup` warning says, and it is why the first shape's stroke is
silent while the second's is not.

The alternative was adding `stroke`/`stroke_width`/`cap` to `Canvas`, which is
more new grammar than the RFC has and which would have needed its own design
discussion about what those properties mean on an *ungrouped* canvas. Deferred
to a future possibility rather than invented here.

---

## Correction 7 — §S10's loader uses a statement form the grammar does not have

§S10 and the Guide-level explanation both write the Material 3 loader as:

```byld
var phase = 0.0
phase = 7.0 with anim.linear(4550ms, from: 0.0, repeat: infinite)
```

`with` lives in *attribute* position. There is no statement-level `with`, and
the example in the repository uses the attribute form, which is equivalent:

```byld
Canvas #[width: 120, height: 120,
         morph: 7.0 with anim.linear(4550ms, from: 0.0, repeat: infinite)] { … }
```

Recorded rather than fixed in the language: adding a statement-level `with`
would give the same animation two spellings whose retarget and delay semantics
would have to be kept identical forever, which is a language decision and not a
shape one.

---

## Correction 8 — §Q10's diagnostic is a new one, not an existing one

§Q10 resolves that `ngon`'s `n` is not animatable and that "attempting `with` on
`n` is a `NotAnimatable` diagnostic pointing at `morph`". RFC-0031 predates the
`AttrClass` work, and the compiler's existing refusal is
`LayoutPropNotAnimatable` — which would be the wrong diagnostic here, not merely
a differently-named one.

`n` is **paint-class**: it moves no geometry and costs no relayout. It simply
has no value between a pentagon and a hexagon. `LayoutPropNotAnimatable` exists
to say "this would relayout the tree at the display rate; use a transform", and
sending an author towards a transform is unhelpful advice for a vertex count.

`NotAnimatable { prop, use_instead }` is therefore its own diagnostic, and it
names `morph`. The distinction is asserted directly:
`animating_ngons_vertex_count_is_refused_and_names_morph` requires the message
to contain `morph` **and** requires that the layout diagnostic is *not* raised.

`ngon`'s continuous parameters — `corner`, `inner`, `rotate` — still animate, and
there is a test that they do.

---

## One addition RFC-0031 did not anticipate: diagnostic severity

§Q5 asks for a **warning**, "not an error — the shape still renders correctly,
the property is simply ignored, and failing a build over an inert attribute is
disproportionate".

Every diagnostic in the compiler was fatal. `CompileError::is_warning` is the
severity that makes §Q5 expressible, and it is deliberately small: it defaults
to `false`, so a diagnostic added without a thought about severity stays fatal;
`render_line` prints `warning[Code]` rather than `error[Code]`, because a reader
should not have to know the code to know whether the build failed; and `byard
check` prints warnings in full, counts them separately, and exits 0.

`StrokeInFusionGroup` is the only warning today. `ConflictingGroupMode`,
`DashOnFusedStroke`, `TooManyGroupMembers` and `NotAnimatable` are all fatal,
because each names something that cannot be drawn at all.

---

## What did *not* change

Recorded because the value of an erratum depends on it being specific.

- **The Lⁿ substitution and the `n == 2` short-circuit** (§S1) landed exactly as
  written, and the short-circuit is a bitwise-verified correctness requirement
  rather than an optimisation.
- **The shape group's shape** (§S4–§S6): one instance, a contiguous range of
  POD records, a per-frame `Vec` cleared and refilled like every other frame
  vector, an eight-member cap with an unrolled literal loop bound.
- **Field interpolation over fractional `n`** (§S9/§Q7), including its stated
  weakness for shapes of very different scale.
- **The wrapping phase** (§Q9), including that negative phases wrap too.
- **OKLab colour** (§Q8), verified against the linear midpoint on a real device.
- **The eight-member cap** (§Q3), refused rather than truncated.
- **Mutual exclusion of `fuse` and `morph`** (§Q4).
- **Dashes refused on a fused stroke** (§Q6).
