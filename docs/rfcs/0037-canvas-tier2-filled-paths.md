# RFC-0037: Canvas Tier-2 — filled curved paths, path gradients, clip masks

- **Status:** Active, filled paths and path gradients implemented 2026-08-05.
  `path { … }` tessellates through `lyon` on the logic thread, caches its mesh
  under a `to_bits` fingerprint of the commands, and draws through the
  `CanvasFill` pipeline, which is registered via RFC-0039 rather than wired
  into the encoder. Fills take a colour or the RFC-0035 gradient descriptor,
  read by the same parser and interpolated by the same shader block a box fill
  uses (`encoder/gradient.wgsl`, textually shared).

  **Clip masks: the rounded-rect half landed 2026-08-31; `clip(path)` did
  not.** `Clip #[rrect: r]` cuts its whole subtree to a rounded rectangle
  through an analytic SDF in the fragment shader — no stencil, no
  tessellation — which is the fast path this RFC's resolved question asks for.
  It is guarded in pixels (`clip_mask_readback.rs`) with a runnable example
  (`examples/clip_mask`).

  Two corrections to what this document predicted about it:

  - **It is a dynamic-offset uniform, not "a storage binding plus one lane per
    instance".** The lane was the wrong currency: `decorated_box` already
    declares fifteen of the sixteen vertex attributes every adapter
    guarantees, so the lane would have spent the last one on a value that
    never varies within a clip run. The clip is rebound per run instead, where
    the scissor already changes, at a cost of zero attributes.
  - **The test is shared textually** (`encoder/clip.wgsl`), the way
    `gradient.wgsl` is, because seven pipelines clip and a clip that rounded
    one pipeline's corners and not another's would show as a hairline where an
    image meets the card it is clipped to.

  **`clip(path)` remains deferred, and is underspecified here.** The guide
  writes it as `clip(path) { … }` with a literal ellipsis and never says where
  the path's commands come from — a `d:` string, a Canvas-style body, or a
  sibling shape are all consistent with the text. That surface has to be
  decided before it can be built.

  Its implementation blocker is smaller than this document assumed, though.
  The stencil attachment it needs is real — `DEPTH_FORMAT` is `Depth32Float`
  and carries no stencil bits — but switching to the universally available
  `Depth24PlusStencil8` is safe for this engine's draw-order scheme, which was
  the reason to fear the swap: depths are spaced `1/65536` apart (`draw_depth`)
  and a 24-bit unorm quantum is about `6e-8`, leaving roughly 256× headroom, so
  no primitive pair comes near z-fighting. What is left is genuinely
  cross-cutting — stencil state on six pipelines, a mask pipeline, and a clip
  stack keyed by stencil reference count — but not precision-risky.

  **Deltas against this document, as written:**

  - Paint is written in the command's parentheses
    (`path(gradient: (…), winding: even_odd) { … }`), not in an attribute
    block. Every canvas command carries its paint that way, and a shape with an
    attribute block is rejected by a rule that predates this RFC.
  - `gradient:` is the RFC-0035 tuple beside `fill:`, rather than
    `fill: gradient(…)`: sharing the descriptor means sharing its spelling too,
    and a second spelling is a second thing to keep in step.
  - A point is two numbers (`cubic(c1x, c1y, c2x, c2y, x, y)`), as in every
    other canvas command, rather than the `Vec2` arguments the guide sketches.
- **Author(s):** Briany4717
- **Created:** 2026-08-04
- **Last updated:** 2026-08-04

> **Placement:** `docs/rfcs/0037-canvas-tier2-filled-paths.md`

---

## Summary

RFC-0020 shipped Canvas **Tier-1**: analytic strokes for `arc`, `circle`,
`line`, `rect`, `ngon`. It routed `path(d: …)` and `bezier` to the MSDF vector
pipeline and marked general *filled* paths and clip masks as Tier-2, deferred.
This RFC delivers Tier-2: closed and open **filled** paths built from line and
cubic-Bézier segments, **gradient fills** on those paths (linear/radial/conic
from RFC-0035), and rectangular/rounded **clip masks**. It is the foundation the
weather-trends chart (`byard-charts`) stands on — the smooth area under a curve is
a filled Bézier path with a vertical gradient — and it generalizes to sparklines,
badges with custom silhouettes, and any bespoke vector fill. Canvas stays a core
intrinsic, but it earns its fill pipeline through the RFC-0039 pipeline-registration
mechanism, making it that mechanism's first in-tree proving ground.

## Motivation

The Weather Trends card fills the region under a smooth temperature curve with a
vertical fade. That is a closed path — the curve on top, the baseline on the
bottom — filled with a gradient. Byard cannot draw it: Tier-1 does strokes, not
fills, and the Tier-2 fill path is explicitly absent
(`encoder/canvas_shape.rs` header: "`path(d: …)` commands never reach this
pipeline; they rasterize through `VectorMSDF` … Tier 2"). MSDF is built for
resolution-independent *glyph-like* shapes from a static atlas, not for a path
whose geometry changes every frame as data updates. A per-frame data chart needs
a fill pipeline that tessellates dynamic geometry cheaply, which is what this RFC
adds.

Clip masks are the second half: rounded-corner clipping of arbitrary content
(an image inside a squircle, a chart that must not paint past its card) currently
only exists as the decorated-box corner radius, which cannot clip children.

## Guide-level explanation

Inside a `Canvas`, paths gain fills:

```
Canvas #[width: match, height: 160] {
    // The area under the curve: move to baseline, trace the smooth top, close.
    path {
        move(0, baseline)
        line(0, y[0])
        for i in 1..points.len {
            // Catmull-Rom control points → cubic bézier, smoothing the series.
            cubic(c1[i], c2[i], (x[i], y[i]))
        }
        line(w, baseline)
        close()
    } #[fill: gradient(linear, 0xF9A8A8, 0x00000000, angle: 90deg)]

    // The line itself, stroked on top (Tier-1, unchanged).
    path { … } #[stroke: 0xF9A8A8, width: 3, cap: round, join: round]
}
```

Clipping wraps a subtree:

```
clip(rrect: 16) {          // rounded-rect clip, radius 16
    Image("radar.png") #[fit: cover]
}
clip(path) { … }           // clip to an arbitrary closed path
```

A `path` may be `fill`ed, `stroke`d, or both (fill paints first). Fills honour
the `even_odd` vs `nonzero` winding rule via a `winding:` prop, default
`nonzero`.

## Reference-level explanation

**Tessellation.** Filled paths tessellate on the CPU with `lyon`
(`lyon_tessellation`), already common in the wgpu ecosystem. Cubic segments
flatten to line segments at a tolerance derived from the path's on-screen size
(so a small sparkline flattens coarsely, a large chart finely), producing a
triangle mesh (`FillVertex { pos, uv }`). This runs on the logic/build thread
during frame assembly, not in the paint hot path.

**A seventh pipeline, registered not bolted-in.** Rendering is a new `CanvasFill`
pipeline parallel to the Tier-1 `CanvasShape` one: a vertex buffer of tessellated
triangles plus a per-path uniform carrying fill colour *or* a gradient descriptor
(the RFC-0035 `{kind, stops, geom}` block, reused verbatim — a path gradient and a
box gradient share the same fragment interpolation). The `uv` interpolated across
the mesh drives the gradient `t`, so a vertical area gradient is `uv.y`.
`CanvasFill` registers through the **pipeline-registration mechanism of RFC-0039**
(the native render-extension ABI), not directly into the encoder. Canvas Tier-2 is
thus the first in-tree consumer of that mechanism and its proving ground: if a
core-owned pipeline cannot be expressed cleanly through the registration API, the
API is not yet sufficient for the package pipelines (charts, maps) that depend on
it. Canvas remains a core intrinsic because it is general-purpose and broadly used,
but it earns its pipeline the same way a package would.

**Retained geometry.** Tessellated meshes are cached in the persistent instance
arena (RFC-0033), keyed by a fingerprint of the path commands. A chart whose data
did not change reuses last frame's mesh (RFC-0032 dirty model); only a path whose
commands changed re-tessellates. This is what keeps a live, animating chart
within frame budget: the expensive step (tessellation) is skipped on unchanged
frames.

**Clip masks.** `clip(rrect: r)` and `clip(path)` push a mask onto a clip stack
realized with the stencil buffer: the mask shape writes stencil, the clipped
subtree renders with a stencil test, the mask pops on exit. Nesting composes by
stencil reference count. Rounded-rect clips take an analytic fast path (no
tessellation, an SDF test in the fragment shader) because they are by far the
common case (clipping an image or card content); arbitrary-path clips tessellate
like fills. Clip regions integrate with the existing z-layer/overlay stacking
(RFC-0017) by scoping to the subtree's layer.

**Compiler.** `path { … }` gains `fill` (colour or `gradient(...)`), `winding`,
and keeps Tier-1 `stroke`/`width`/`cap`/`join`. `clip(kind) { children }` is a
new container form. Path command builders (`move`, `line`, `cubic`, `quad`,
`arc`, `close`) are validated for a well-formed subpath (a `fill` on an unclosed
path implicitly closes to the start point with a diagnostic note).

## Drawbacks

- CPU tessellation adds a dependency (`lyon`) and a per-changed-path cost. Bounded
  by caching (unchanged paths skip it) and by size-adaptive flattening tolerance,
  but a pathological path that changes every frame at large size pays every frame.
- Stencil-based clipping consumes a stencil buffer and adds pipeline state
  changes at clip boundaries. Rounded-rect clips avoid the stencil via SDF, so
  the common case is cheap; arbitrary-path clips are the ones that pay.
- Two ways to get vector shapes on screen now exist (MSDF for static
  glyph/icon-like art, Tier-2 fills for dynamic geometry). The dividing line is
  documented: static and reused → MSDF atlas; dynamic per-frame → Tier-2 fill.

## Rationale and alternatives

- **Why `lyon` CPU tessellation over compute-shader tessellation or MSDF?** MSDF
  is wrong for geometry that changes every frame (it bakes an atlas). GPU compute
  tessellation is more machinery than the reference needs and complicates the
  no-`!Send` logic-thread model. `lyon` is proven, runs off the paint thread, and
  its output caches cleanly in the existing instance arena.
- **Why reuse the RFC-0035 gradient block for fills?** A path fill and a box fill
  want identical gradient semantics; sharing the descriptor and the fragment
  interpolation avoids a second gradient implementation and keeps the two in
  lockstep.
- **Why register the pipeline through RFC-0039 instead of hard-wiring it?** It
  dogfoods the extension ABI with a core pipeline before packages depend on it; a
  registration API that cannot carry Canvas Tier-2 is not ready to carry a chart or
  a map, and finding that out in-tree is cheaper than finding it out in a package.
- **Why stencil clipping with an SDF fast path rather than always-SDF?** Only
  rounded rects have a cheap closed-form SDF; arbitrary paths do not, so a general
  mechanism (stencil) is required, but the overwhelmingly common rounded-rect clip
  should not pay for it.
- **Rejected: extend MSDF to dynamic paths.** Re-baking an atlas per frame is far
  more expensive than tessellating a changed path, and MSDF's whole value is
  amortizing a static bake.

## Prior art

`lyon` (the de-facto Rust 2D tessellator, used by Iced and others); Skia's path
fill + `SkPath` winding rules; Flutter `Canvas.drawPath` + `Path.clip`; HTML
Canvas `fill()`/`clip()` with `nonzero`/`evenodd`. The static-atlas-vs-dynamic-
tessellation split mirrors how game UIs separate baked vector icons from dynamic
vector charts.

## Resolved questions

**CPU (`lyon`) or GPU tessellation?** Resolved: CPU `lyon` on the build thread,
cached in the instance arena. Reasoning: the reference's dynamic geometry is small
(a chart is tens of segments), CPU tessellation is trivially within budget when
cached, and it avoids adding compute-shader complexity to a rendering model that
prizes determinism and a `!Send` logic thread.

**Share the RFC-0035 gradient descriptor for path fills, or a separate one?**
Resolved: share it verbatim. Reasoning: path and box gradients want identical
behaviour; one descriptor and one fragment interpolation means they can never
drift and there is half as much shader to test.

**Own encoder hook or register through the RFC-0039 mechanism?** Resolved: register
through RFC-0039. Reasoning: Canvas Tier-2 is a natural first consumer that validates
the extension ABI in-tree before package pipelines rely on it; a mechanism that
cannot express a core pipeline is not ready for package ones, and dogfooding surfaces
that gap where it is cheapest to fix.

**Stencil clip for everything, or an SDF fast path for rounded rects?** Resolved:
SDF fast path for rounded rects, stencil for arbitrary paths. Reasoning: rounded-
rect clipping (image/card content) is the dominant case and has a cheap analytic
SDF; forcing it through the stencil would tax the common path to serve the rare
one.

**Does an unclosed `fill` path error or auto-close?** Resolved: auto-close to the
subpath start with a build-time diagnostic note. Reasoning: an open filled path is
almost always an oversight, auto-closing produces the obviously-intended shape,
and the note makes the implicit edge visible without failing the build.
