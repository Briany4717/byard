// The clip-coverage rasteriser (RFC-0037 `clip(path)`).
//
// One job, and deliberately the smallest shader in the engine: take the
// triangles of a tessellated path and write full coverage where they land.
// The antialiasing is not done here — it comes from the multisampled
// attachment this draws into, which is resolved down to the single-sample
// coverage texture the clip test samples.
//
// Doing it that way rather than analytically is what lets an *arbitrary*
// outline have the same soft edge the rounded clip's SDF gives it. A path has
// no closed form to evaluate, and a hard-edged mask beside a smooth one would
// make the boundary the user actually drew the only jagged one on screen.

struct MaskUniform {
    /// The mask's logical-pixel bounds `(x, y, w, h)`: what the vertices are
    /// measured against, and what maps them onto this pass's viewport.
    bounds: vec4<f32>,
};

@group(0) @binding(0) var<uniform> mask: MaskUniform;

struct VertexInput {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(v: VertexInput) -> @builtin(position) vec4<f32> {
    // The vertices arrive in the canvas' logical pixels; the target is this
    // mask's own region, so the bounds are the whole of the transform.
    let local = (v.pos - mask.bounds.xy) / max(mask.bounds.zw, vec2<f32>(1e-5));
    return vec4<f32>(local.x * 2.0 - 1.0, 1.0 - local.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) f32 {
    return 1.0;
}
