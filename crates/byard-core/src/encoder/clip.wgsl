// The active clip mask, shared by every pipeline's fragment shader
// (RFC-0037 clip masks).
//
// Prepended to each shader that can be clipped, the same way `gradient.wgsl`
// is shared: one declaration of the binding and one definition of the test, so
// a clip means the same thing in every pipeline. Two shaders each spelling the
// rounded-rect test their own way is exactly how a clip comes to cut one
// pipeline's corners and not another's.
//
// The entry is selected by the dynamic offset the clip-run walker sets, so this
// is always "the clip in force for the run being drawn". Entry 0 is a sentinel
// large enough to contain any viewport, which is why there is no "is anything
// clipping" branch here: an unclipped fragment takes the same path and passes.

struct ClipEntry {
    /// `xy` origin, `zw` size, in physical pixels.
    rect: vec4<f32>,
    /// Per-corner radii `[tl, tr, br, bl]`, all zero for a plain rectangle.
    radii: vec4<f32>,
    /// The path mask's bounds in physical pixels (RFC-0037 `clip(path)`), or
    /// all zero when this clip has no path. A zero-width rect contains
    /// nothing, which is what makes "is there a mask" a width test rather
    /// than a separate flag to keep in step.
    mask_rect: vec4<f32>,
    /// Where that mask was rasterised in the coverage strip, `(u0, v0, u1, v1)`.
    mask_uv: vec4<f32>,
    /// Padding out to the 256-byte stride the entries are spaced at.
    ///
    /// The binding is the whole stride rather than the 32 bytes of payload,
    /// which keeps it clear of D3D12's rule that a constant-buffer view be a
    /// multiple of 256 bytes in offset *and* size. This padding is what keeps
    /// the shader's idea of an entry the same size as the binding's.
    ///
    /// Honest provenance: this arrived while chasing a Windows failure that
    /// turned out to be #234, a pre-existing defect in `solid_box`, not this.
    /// So the wider binding is not known to be *required* — it is correct,
    /// it costs nothing, and the smaller one was never seen green on D3D12.
    _pad: array<vec4<f32>, 12>,
};

@group(0) @binding(1) var<uniform> clip_entry: ClipEntry;
/// The coverage strip every path mask of the frame is rasterised into, and
/// the sampler it is read with. Always bound: a frame with no path clip binds
/// a one-texel placeholder, so an unclipped fragment and a path-clipped one
/// run the same instructions.
@group(0) @binding(2) var clip_mask_tex: texture_2d<f32>;
@group(0) @binding(3) var clip_mask_samp: sampler;

/// Signed distance to the clip's rounded rectangle: negative inside.
///
/// Deliberately the same construction as `decorated_box.wgsl`'s corner SDF —
/// a box distance with the corner's radius subtracted — so a clip's corner and
/// a box's corner of the same radius land on the same curve. If they diverged,
/// an image clipped to a card's radius would show a hairline of background
/// against the card it sits in.
fn clip_sdf(p: vec2<f32>) -> f32 {
    let half = clip_entry.rect.zw * 0.5;
    let centre = clip_entry.rect.xy + half;
    let q = p - centre;

    // Pick the radius belonging to the quadrant this fragment is in, in the
    // same `[tl, tr, br, bl]` order the rest of the engine uses.
    var r = clip_entry.radii.x;
    if (q.x > 0.0 && q.y < 0.0) { r = clip_entry.radii.y; }
    if (q.x > 0.0 && q.y > 0.0) { r = clip_entry.radii.z; }
    if (q.x < 0.0 && q.y > 0.0) { r = clip_entry.radii.w; }

    // A radius past half the box folds the field in on itself, so it is
    // clamped where it is consumed — the same rule, and the same reason, as
    // the decorated box's corners.
    r = min(r, min(half.x, half.y));

    let d = abs(q) - half + vec2<f32>(r);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - r;
}

/// Coverage of `p` by the clip: 1 inside, 0 outside, smoothed across roughly
/// one pixel at the boundary.
///
/// Coverage rather than `discard`, because a clip edge is a shape edge and
/// every other edge this engine draws is antialiased. A hard `discard` would
/// make the one boundary the user did not draw the only jagged one on screen.
fn clip_coverage(p: vec2<f32>) -> f32 {
    // The band is half a pixel either side, as a constant, because this test
    // runs in physical pixels: `p` is `@builtin(position)` and the entry was
    // uploaded scaled, so one unit *is* one device pixel and a screen-space
    // distance field has a gradient of one by construction. There is nothing
    // for a derivative to tell us that the units do not already say.
    //
    // An earlier version called `fwidth` here and was changed on the theory
    // that it explained a Windows failure. It did not — that was #234 — so
    // this stands on the reasoning above and not on that story.
    let d = clip_sdf(p);
    let rounded = saturate(1.0 - smoothstep(-0.5, 0.5, d));
    return rounded * clip_path_coverage(p);
}

/// Coverage of `p` by this clip's path mask (RFC-0037 `clip(path)`), or 1 when
/// the clip has none.
///
/// Multiplied into the rounded coverage rather than replacing it, so a path
/// clip nested inside a rounded one is cut by both, and the antialiasing of
/// either edge survives the other. The mask's own edge was antialiased when it
/// was rasterised (a multisampled attachment, resolved); this reads it back
/// linearly, which is what keeps that softness when a fragment falls between
/// texels.
///
/// `textureSampleLevel` rather than `textureSample`: the clip test is called
/// from inside non-uniform control flow in several pipelines, where implicit
/// derivatives are not defined, and a mask has one mip level regardless.
fn clip_path_coverage(p: vec2<f32>) -> f32 {
    let r = clip_entry.mask_rect;
    let uv_rect = clip_entry.mask_uv;
    let t = (p - r.xy) / max(r.zw, vec2<f32>(1e-5));
    let uv = mix(uv_rect.xy, uv_rect.zw, t);
    let inside = f32(all(t >= vec2<f32>(0.0)) && all(t <= vec2<f32>(1.0)));
    let sampled = textureSampleLevel(clip_mask_tex, clip_mask_samp, uv, 0.0).r;
    // `select` keeps a clip with no mask at exactly 1.0, whatever the
    // placeholder texel holds; a masked clip reads 0 outside its own region.
    return select(1.0, sampled * inside, r.z > 0.0);
}
