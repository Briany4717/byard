// Ripple pipeline (RFC-0023): the Material ink reveal. One instance is one
// expanding, fading circle, centred on the tap point, clipped to its
// element's rounded rect, composited *above* the element background and
// *below* its children (the instance's draw-order depth, stamped between the
// two by the evaluator's emission order, resolves that against the shared
// depth buffer).
//
// The logic thread samples the expansion/fade each tick (RFC-0010 model as
// landed) and re-emits `params.z` (radius) / `params.w` (fade alpha); this
// shader only rasterises the current circle analytically. Blending is
// premultiplied-alpha "over", so a dark ink darkens a light surface (pure
// addition could only ever brighten) while simultaneous ripples from rapid
// taps still accumulate where their circles overlap (RFC-0023 §1).

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
};

struct InstanceInput {
    // Element rect (x, y, w, h) in logical px, quad geometry and clip bounds.
    @location(1) rect: vec4<f32>,
    // (center_x, center_y, radius, fade_alpha); centre in absolute logical px.
    @location(2) params: vec4<f32>,
    // Ink colour; `a` is the ink's own peak alpha.
    @location(3) color: vec4<f32>,
    // Per-corner clip radii (tl, tr, br, bl), the element's border radii.
    @location(4) radii: vec4<f32>,
    // Paint-time transform (RFC-0011); identity is a free no-op.
    @location(5) t_translate: vec2<f32>,
    @location(6) t_scale: vec2<f32>,
    @location(7) t_rotate: f32,
    @location(8) t_origin: vec2<f32>,
    // Draw-order depth (NDC-z), stamped by `RenderFrame::push_ripple`.
    @location(9) depth: f32,
    // Corner smoothing 0..1 (RFC-0031 §S1), the ink's clip must follow the
    // element's own corner profile, not a circular approximation of it.
    @location(10) smooth_amount: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local_pos: vec2<f32>,
    @location(1) half_size: vec2<f32>,
    @location(2) center_local: vec2<f32>,
    @location(3) radius_alpha: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) radii: vec4<f32>,
    @location(6) @interpolate(flat) corner_n: f32,
};

@group(0) @binding(0) var<uniform> viewport_size: vec2<f32>;

// Anti-alias fringe head-room around the quad, matching `decorated_box.wgsl`.
const QUAD_PADDING: f32 = 2.0;

// Applies a paint-time transform (RFC-0011) to a world-space (logical-pixel)
// position: rotate + scale about `origin`, then translate, identical to
// `decorated_box.wgsl`'s helper, so a transformed element carries its ink
// with it.
fn apply_transform(
    world: vec2<f32>,
    translate: vec2<f32>,
    scale: vec2<f32>,
    rotate: f32,
    origin: vec2<f32>,
) -> vec2<f32> {
    let p = world - origin;
    let scaled = vec2<f32>(p.x * scale.x, p.y * scale.y);
    let c = cos(rotate);
    let s = sin(rotate);
    let rotated = vec2<f32>(scaled.x * c - scaled.y * s, scaled.x * s + scaled.y * c);
    return rotated + origin + translate;
}

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    var out: VertexOutput;

    let w = instance.rect.z;
    let h = instance.rect.w;
    out.half_size = vec2<f32>(w, h) * 0.5;

    let padded = vec2<f32>(w, h) + vec2<f32>(QUAD_PADDING) * 2.0;
    out.local_pos = (vertex.quad_pos - 0.5) * padded;
    let world_pos = instance.rect.xy - vec2<f32>(QUAD_PADDING) + vertex.quad_pos * padded;

    let transformed = apply_transform(
        world_pos,
        instance.t_translate,
        instance.t_scale,
        instance.t_rotate,
        instance.t_origin,
    );

    out.position = vec4<f32>(
        (transformed.x / viewport_size.x) * 2.0 - 1.0,
        1.0 - (transformed.y / viewport_size.y) * 2.0,
        instance.depth,
        1.0
    );

    // The tap point, in the same rect-centred local space as `local_pos`, 
    // the fragment circle SDF is then transform-agnostic (the transform moves
    // the whole quad, ink included).
    let rect_center = instance.rect.xy + out.half_size;
    out.center_local = instance.params.xy - rect_center;
    out.radius_alpha = instance.params.zw;
    out.color = instance.color;
    out.radii = instance.radii;
    out.corner_n = 2.0 + clamp(instance.smooth_amount, 0.0, 1.0) * 4.0;
    return out;
}

// Rounded-box SDF, shared shape with `decorated_box.wgsl`, the clip must
// match the element's own outline exactly or the ink visibly bleeds past (or
// falls short of) a rounded corner.
/// Lⁿ norm of a **non-negative** 2-vector, paired with the magnitude of its own
/// gradient (RFC-0031 §S1–S2). `n == 2` is the Euclidean norm, whose gradient is
/// exactly 1, the circular corner this pipeline clipped to before RFC-0031, and
/// the reason an unset `smooth` is bit-identical. Above 2 the norm is not a true
/// signed distance: on the corner diagonal its gradient is `2^(1/n - 1/2)`,
/// ≈0.79 at `n = 6`. Normalising by the returned gradient keeps the clip's
/// anti-aliased fringe the same width at the corners as along the edges.
fn lp_norm(v: vec2<f32>, n: f32) -> vec2<f32> {
    let a = pow(v.x, n) + pow(v.y, n);
    if (a <= 0.0) {
        return vec2<f32>(0.0, 1.0);
    }
    let f = pow(a, 1.0 / n);
    let g = vec2<f32>(pow(v.x / f, n - 1.0), pow(v.y / f, n - 1.0));
    return vec2<f32>(f, max(length(g), 1e-4));
}

fn sd_rounded_box(p: vec2<f32>, b: vec2<f32>, r: vec4<f32>, n: f32) -> f32 {
    var r_corner = r.x;
    if (p.x > 0.0 && p.y < 0.0) { r_corner = r.y; }
    if (p.x > 0.0 && p.y > 0.0) { r_corner = r.z; }
    if (p.x < 0.0 && p.y > 0.0) { r_corner = r.w; }

    // A corner radius may never exceed half the box (RFC-0001 §3.1: the
    // rounded-rect SDF is only well-defined for `r <= min(half)`; beyond it the
    // field folds in on itself and the corners visibly deform, a `radius: 20`
    // pill on a 33px-tall button is the everyday case). Clamping here, at the
    // one place the radius is consumed, keeps every pipeline honest and matches
    // the CSS rule that an over-large radius is reduced to fit.
    r_corner = min(r_corner, min(b.x, b.y));
    let q = abs(p) - b + vec2<f32>(r_corner);
    let corner = max(q, vec2<f32>(0.0));
    let inner = min(max(q.x, q.y), 0.0);
    // RFC-0031 §S1: the L² path, verbatim and unconditional at `smooth: 0`.
    if (n == 2.0) {
        return inner + length(corner) - r_corner;
    }
    // `inner` is non-zero only where one of `corner`'s components is zero, and
    // there `lp.y == 1`, so dividing the whole expression normalises exactly
    // the corner arc and leaves the straight edges untouched.
    let lp = lp_norm(corner, n);
    return (inner + lp.x - r_corner) / lp.y;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Clip to the element's rounded rect (RFC-0023: always, no opt-out).
    // `smoothstep` edges stay in ascending order, descending edges are
    // undefined per the spec and DX12/HLSL resolves them differently from
    // Metal/Vulkan, and the `!(x > y)` discard also fires on NaN.
    let box_dist = sd_rounded_box(
        in.local_pos,
        in.half_size,
        in.radii,
        in.corner_n,
    );
    let box_soft = max(length(vec2<f32>(dpdx(box_dist), dpdy(box_dist))), 1e-5);
    let box_cov = 1.0 - smoothstep(0.0, box_soft, box_dist);

    // The expanding ink circle, anti-aliased over the same screen-space fringe.
    let circle_dist = length(in.local_pos - in.center_local) - in.radius_alpha.x;
    let circle_cov = 1.0 - smoothstep(0.0, box_soft, circle_dist);

    let a = box_cov * circle_cov * in.color.a * in.radius_alpha.y;
    if (!(a > 0.0)) {
        discard;
    }
    // Premultiplied output for the PREMULTIPLIED_ALPHA_BLENDING "over" state:
    // ink composites onto light and dark surfaces alike, and rapid taps pool
    // where circles overlap.
    let a_clipped = a * clip_coverage(in.position.xy);
    return vec4<f32>(in.color.rgb * a_clipped, a_clipped);
}
