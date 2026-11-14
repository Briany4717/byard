# RFC-0060: Blending like the design tools

- **Status:** Draft
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0039 (native render extensions, whose package shaders this
  changes), RFC-0057 (colour motion in OKLab, which this leaves alone), RFC-0050
  (Byard Material, where the difference was measured).

## Summary

Byard writes to an sRGB surface, so the GPU blends every translucent colour in
linear light. Android, every browser, Figma and the Material specification blend
in sRGB. A translucent layer therefore comes out stronger in Byard than the
design it was copied from: Material's 10% state layer looks like 19%. This RFC
blends in sRGB, like the tools a design comes from. Opaque colours don't change
by a single byte. What changes is anything with alpha: overlays, scrims, shadows,
antialiased edges and text.

## Motivation

Found auditing Byard Material's IconButton. Its pressed state is the content
colour at 10% over the container. On the baseline primary `#6750A4` with white
content:

| Blended in | Result | Strength |
|---|---|---|
| sRGB (Android, browsers, Figma) | `(118, 98, 173)` | 10% |
| linear light (Byard today) | `(130, 116, 176)` | about 19% in sRGB terms |

Every number in Material that is an opacity was chosen by a designer looking at
sRGB blending: state layers at 8, 10 and 16%, disabled content at 38% and
containers at 12%, the scrim at 32%, the elevation shadows. The same holds for
every design a team brings to Byard from Figma. Writing those numbers into Byard
gives a different picture, and no number fixes it, because the error depends on
what is behind the layer. A light layer on a dark background is lifted most.

Linear blending is the physically correct choice for light: it is what a
renderer of photographs or 3D scenes wants. It isn't what a UI is designed
against. Byard is a UI framework, and RFC-0050 makes it the proof that it can
carry a design system. A design system it can't reproduce isn't carried.

## Guide-level explanation

Nothing to write differently. A colour still means what the design tool says,
and now `overlay: t.onPrimary.alpha(0.1)` looks like 10% of white, as in the
design. A screenshot of a Byard app and of the same design in a browser match in
their translucent parts as well as their opaque ones.

Gradients and colour animations don't change. They are mixed in OKLab
(RFC-0057), which the engine computes itself; only the final write changes.

A package that draws with its own shader (RFC-0039) still returns a linear
colour from `fs_main`. The engine encodes it, as it does for its own shaders.

## Reference-level explanation

### 1. The target

The surface, the shot texture, the layer textures of `group` and the backdrop
copies use the `Unorm` twin of the format they use now (`Bgra8Unorm` for
`Bgra8UnormSrgb`). On a surface that only offers an sRGB format, the engine
asks for the `Unorm` one as a view format (`view_formats`), which every backend
Byard supports allows. A pixel in the target is then the sRGB-encoded byte, and
the fixed-function blend mixes those bytes, which is sRGB blending.

### 2. Shaders write encoded colour

Colours stay linear everywhere they are computed: `RenderFrame`, the vertex
data, OKLab mixing, shadows and blur weights. Each engine fragment shader ends
by passing its colour through one shared function:

```wgsl
fn byard_out(c: vec4<f32>) -> vec4<f32>   // straight alpha in, encoded out
fn byard_out_premul(c: vec4<f32>) -> vec4<f32>  // premultiplied in, premultiplied encoded out
```

The premultiplied form divides by alpha, encodes and multiplies back, with zero
alpha kept as zero. These are the 11 fragment shaders that write colour
(`clip_mask` writes coverage and is unchanged). The transfer is the same
piecewise IEC 61966-2-1 curve as `byard_core::color`. That module stays the one
place it is written: the WGSL is generated from the same constants, and a test
compares the two.

### 3. Reading the target back

`backdrop` and `blur` sample the target. Through a `Unorm` view they read the
encoded byte. They decode on read, so blur is still a mix of light, and encode
on write. CSS's `backdrop-filter` blurs in sRGB, so this is a choice. Decoding
costs about as much as not decoding, and it keeps a blurred edge as light as the
one drawn now.

Images are uploaded as `Rgba8UnormSrgb` and decoded on sampling as before, then
encoded by `byard_out`, so an opaque image is byte for byte what it was.

### 4. Text

glyphon is created with `ColorMode::Web`, its mode for a `Unorm` target. Glyph
coverage then blends in sRGB, as in browsers. Light text on a dark background
gets a little thinner and dark text on light a little heavier than now, which is
how the same font looks in Chrome.

### 5. Package shaders

The prelude gains `byard_out` and `byard_out_premul`. The engine renames the
package's `fs_main` to `package_fs_main` and appends an `fs_main` with the same
signature that returns `byard_out(package_fs_main(...))`. The signature is read
from the module naga already parses for validation. A package therefore still
writes linear colour, with straight alpha as package shaders are blended now, and
gets the same treatment as the engine's own shaders.

### 6. Cost

One `pow` per channel per fragment written, two for a blend that reads back.
That's measured on the stockroom and aura_weather budgets before this is
accepted. On the GPUs Byard's CI runs, `pow` is an `exp2` and a `log2`.

## Drawbacks

- **Every picture with alpha in it changes.** Tests that compare pixels of
  translucent output need new numbers. Opaque pixels and every check on the
  accessibility tree stay as they are.
- **The package shader wrapper** is a rewrite of a package's source. The rename
  is mechanical, but an error message inside the package's code now points at
  `package_fs_main`, and the engine has to map it back.
- **Linear blending is lost** for anyone who wanted it, such as a physically
  based glow in a native view. Section *Future possibilities* covers this.

## Rationale and alternatives

- **Keep linear and document it.** Every design would look stronger than its
  source, by an amount that depends on the background, and Byard Material
  wouldn't match Material. This is the status quo.
- **Correct each opacity.** Lower Material's 10% to whatever looks like 10%.
  It isn't one number. The right alpha depends on both colours, so it works on
  one background and is wrong on the next.
- **A float target and a final pass.** Render in `Rgba16Float` and blend in
  sRGB terms by encoding before every blend. Fixed-function blending can't do
  that, and doing it in shaders means reading the target for every translucent
  primitive. It costs more memory and bandwidth than this.
- **An option per app.** `[render] blending = "srgb" | "linear"`. Possible
  later. Two answers to "what does 10% look like" in one ecosystem make every
  package ask which one it runs under, so this RFC picks one.

## Prior art

Browsers composite in sRGB. Android's `Canvas`, and Compose on top of it, blend
in sRGB. Figma and Sketch blend in sRGB. Game engines blend in linear, which is
where Byard's current choice comes from.

## Unresolved questions

- Whether `backdrop`/`blur` should blur in sRGB to match CSS exactly, or in
  light as proposed.
- The exact cost on the slowest CI GPU, which decides whether `byard_out` may use
  an approximation of the curve. Opaque pixels must stay byte-exact either way.

## Future possibilities

- `blend: linear` on one element, for a glow or a light effect that should add
  light, done as an offscreen layer composited by the engine.
