### What problem does this solve?

Cupertino's visual language is defined by layered translucency: navigation bars, tab bars, sheets and notification panels all show *blurred* content behind them. A `byard-cupertino` package without backdrop blur is visually unrecognisable as iOS — RFC-0023 calls this the platform's core visual identity, not a nice-to-have. Nothing at the `byld` level can fake it: the effect must sample pixels already rasterised behind the element, which is strictly a render-pipeline concern.

With the ripple slice of RFC-0023 already landed (#136), this is the remaining half: blur, tint, and vibrancy.

### Proposed solution

Implement the backdrop-blur slice of RFC-0023 (§2, with every resolved question honoured):

- Four paint-time style properties on the box render path: `blur: Float` (backdrop blur radius in logical px, clamped to 40, `0` disables), `backdrop_tint: Color` (blended over the blurred sample — with `blur` it is the vibrancy pair; without it, a plain translucent wash at zero GPU cost), `blur_saturation: Float` (vibrancy boost, default 1.8), and `blur_quality: auto | high | low` (resolved question "quality tiers": `high` forces the two-pass separable Gaussian at 0.75× resolution, `low` the single-pass box blur at 0.5×, `auto` probes the GPU at startup — Gaussian on discrete adapters).
- A blurred element is a **barrier**: the engine rasterises everything emitted behind it (its own background included, RFC-0023 §4 order), copies the region behind its rect (plus the blur halo), blurs it off-screen at reduced resolution (resolved question: 0.5×, 0.75× for forced-high, bilinear upscale on composite), then draws it as the element's background clipped to its border radius, saturated and tinted. Children render crisply on top.
- Overlapping panes stack in natural painter's order — the upper pane re-blurs the lower's output (resolved question: the physically-correct "double blur") — and ≥ 3 overlapping panes raise a runtime `PerfWarning::OverlappingBlurs { count }` surfaced by `byard dev` (resolved question: the perf diagnostic).
- Both `blur` and `backdrop_tint` animate through the existing RFC-0010 `with` chokepoints: `blur: 0 with anim.spring()` + `on hover { blur: 16 }` springs a card into frosted glass.
- `blur` on `Canvas`-adjacent and every other box-path intrinsic follows the shared `EFFECTS` prop surface (resolved question: blur is a style property, sampling the scene behind the bounding rect).

### Alternatives you have considered

- A global post-processing pass — rejected by RFC-0023 itself: effects are per-element; only elements with effects enabled pay anything.
- A general user-shader system — RFC-0023 rationale: ripple + blur cover the real-world effect needs of Material and Cupertino; a shader system can layer on later.
- Emitting the tint-only case through the blur pipeline — pointless copies; a tint without blur composites identically as a plain translucent fill, so it lowers to one.

### Which area does this affect?

Render subsystem (wgpu pipelines), Logic subsystem (signals / arenas), byld (DSL syntax / compiler)
