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
    /// `xy` origin, `zw` size, in logical pixels.
    rect: vec4<f32>,
    /// Per-corner radii `[tl, tr, br, bl]`, all zero for a plain rectangle.
    radii: vec4<f32>,
};

@group(0) @binding(1) var<uniform> clip_entry: ClipEntry;

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
    // `fwidth` is the boundary's width in this fragment's own screen space, so
    // the fade stays one pixel wide at any DPI and under any transform.
    let d = clip_sdf(p);
    let aa = max(fwidth(d), 1e-5);
    return 1.0 - smoothstep(-aa, aa, d);
}
