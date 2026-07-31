// DecoratedBox pipeline (M21, RFC-0001 §3.1): a rounded rectangle with an
// optional inner border, a blurred drop shadow, and an overall opacity. Plain
// solid fills (no border/shadow/opacity) stay on the SolidBox pipeline; this one
// is used only when the compiler promotes a box via `RenderFrame::push_decorated`.

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
};

struct InstanceInput {
    @location(1) rect: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) radii: vec4<f32>,
    @location(4) border_color: vec4<f32>,
    @location(5) shadow_color: vec4<f32>,
    // (border_width, shadow_dx, shadow_dy, shadow_blur)
    @location(6) params: vec4<f32>,
    // (opacity, depth, shadow_spread, smooth) — `misc.w` is the RFC-0031 §S1
    // corner smoothing. It used to be a gradient present/absent flag; that
    // question is now answered by `grad_axis.xy`, which is a unit direction
    // vector for a real ramp and all-zero otherwise, so the flag was redundant
    // and its lane was the spare one the RFC asked for.
    @location(7) misc: vec4<f32>,
    // Paint-time transform (RFC-0011); identity is a free no-op below.
    // `opacity` isn't part of this block — `misc.x` above stays authoritative.
    @location(8) t_translate: vec2<f32>,
    @location(9) t_scale: vec2<f32>,
    @location(10) t_rotate: f32,
    @location(11) t_origin: vec2<f32>,
    // Linear gradient (RFC-0001 §3.1), active only when `grad_axis.xy` is a
    // real direction vector (see `has_gradient` in the fragment stage).
    @location(12) grad_from: vec4<f32>,
    @location(13) grad_mid: vec4<f32>,
    @location(14) grad_to: vec4<f32>,
    // (dir_x, dir_y, mid_pos, offset)
    @location(15) grad_axis: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local_pos: vec2<f32>,
    @location(1) half_size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) radii: vec4<f32>,
    @location(4) border_color: vec4<f32>,
    @location(5) shadow_color: vec4<f32>,
    @location(6) params: vec4<f32>,
    @location(7) misc: vec4<f32>,
    @location(8) grad_from: vec4<f32>,
    @location(9) grad_mid: vec4<f32>,
    @location(10) grad_to: vec4<f32>,
    @location(11) grad_axis: vec4<f32>,
};

@group(0) @binding(0) var<uniform> viewport_size: vec2<f32>;

const QUAD_PADDING: f32 = 2.0;

/// Applies a paint-time transform (RFC-0011) to a world-space (logical-pixel)
/// position: rotate + scale about `origin`, then translate. Identity inputs
/// collapse to `world` unchanged.
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

    // Inflate the quad to cover the shape, its anti-alias fringe, and the full
    // shadow extent (offset + blur + positive spread), so no shadow fragment is
    // clipped away. `misc.z` is the shadow spread.
    let shadow_margin = abs(vec2<f32>(instance.params.y, instance.params.z))
        + vec2<f32>(instance.params.w)
        + vec2<f32>(max(instance.misc.z, 0.0));
    let margin = vec2<f32>(QUAD_PADDING) + shadow_margin;
    let padded = vec2<f32>(w, h) + margin * 2.0;

    out.local_pos = (vertex.quad_pos - 0.5) * padded;
    let world_pos = instance.rect.xy - margin + vertex.quad_pos * padded;

    let transformed = apply_transform(
        world_pos,
        instance.t_translate,
        instance.t_scale,
        instance.t_rotate,
        instance.t_origin,
    );

    // misc.y carries the draw-order depth (NDC-z); the encoder writes it per
    // instance so decorated boxes honour global paint order against solids/text.
    out.position = vec4<f32>(
        (transformed.x / viewport_size.x) * 2.0 - 1.0,
        1.0 - (transformed.y / viewport_size.y) * 2.0,
        instance.misc.y,
        1.0
    );

    out.color = instance.color;
    out.radii = instance.radii;
    out.border_color = instance.border_color;
    out.shadow_color = instance.shadow_color;
    out.params = instance.params;
    out.misc = instance.misc;
    out.grad_from = instance.grad_from;
    out.grad_mid = instance.grad_mid;
    out.grad_to = instance.grad_to;
    out.grad_axis = instance.grad_axis;
    return out;
}

/// The gradient's colour at this fragment (RFC-0001 §3.1): projects the point
/// onto the ramp's axis, normalized so `0` is the box's leading edge along that
/// axis and `1` the trailing one, shifted by `offset` and **wrapped** — which is
/// what makes an animated offset a seamless travelling sweep. `mid` splits the
/// ramp at `mid_pos`, so a three-stop highlight band (transparent → bright →
/// transparent) is expressible, not just a two-stop fade.
fn gradient_color(in: VertexOutput) -> vec4<f32> {
    let dir = in.grad_axis.xy;
    // Half-extent of the box measured along `dir` (a box is convex, so the
    // support function is just the weighted sum of the half-sizes).
    let extent = max(abs(dir.x) * in.half_size.x + abs(dir.y) * in.half_size.y, 1e-5);
    let raw = (dot(in.local_pos, dir) / extent) * 0.5 + 0.5 + in.grad_axis.w;
    let t = fract(raw);
    let mid_pos = clamp(in.grad_axis.z, 0.0, 1.0);
    if (t < mid_pos) {
        return mix(in.grad_from, in.grad_mid, t / max(mid_pos, 1e-5));
    }
    return mix(in.grad_mid, in.grad_to, (t - mid_pos) / max(1.0 - mid_pos, 1e-5));
}

/// Lⁿ norm of a **non-negative** 2-vector, paired with the magnitude of its own
/// gradient (RFC-0031 §S1–S2).
///
/// `n == 2` is the Euclidean norm and its gradient is exactly 1 — the circular
/// corner every pipeline drew before RFC-0031. Above 2 the norm is *not* a true
/// signed distance: on the corner diagonal its gradient is `2^(1/n - 1/2)`,
/// which falls to ≈0.79 at `n = 6`, so the corner's anti-aliased fringe would
/// come out ~26 % wider than the edge's — a smeared corner on exactly the
/// shapes the property exists to sharpen the *profile* of. Returning the
/// gradient alongside the value lets the caller normalise the field once, at
/// the source, so *every* consumer of it (edge coverage, the border band,
/// shadow blur falloff) is corrected by the same division.
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
    // field folds in on itself and the corners visibly deform — a `radius: 20`
    // pill on a 33px-tall button is the everyday case). Clamping here, at the
    // one place the radius is consumed, keeps every pipeline honest and matches
    // the CSS rule that an over-large radius is reduced to fit. The clamp is
    // norm-independent: the fold happens past half the extent whatever `n` is.
    r_corner = min(r_corner, min(b.x, b.y));
    let q = abs(p) - b + vec2<f32>(r_corner);
    let corner = max(q, vec2<f32>(0.0));
    let inner = min(max(q.x, q.y), 0.0);
    // RFC-0031 §S1: the L² path, verbatim and unconditional at `smooth: 0`.
    if (n == 2.0) {
        return inner + length(corner) - r_corner;
    }
    // `inner` is non-zero only where one of `corner`'s components is zero, and
    // there `lp.y == 1` — so dividing the whole expression normalises exactly
    // the corner arc and leaves the straight edges untouched.
    let lp = lp_norm(corner, n);
    return (inner + lp.x - r_corner) / lp.y;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let border_width = in.params.x;
    let shadow_offset = vec2<f32>(in.params.y, in.params.z);
    let shadow_blur = in.params.w;
    let shadow_spread = in.misc.z;
    let opacity = in.misc.x;
    // RFC-0031 §S1. Clamped here (rather than trusted) for the same reason the
    // corner radius is: one consumption site, one rule.
    let corner_n = 2.0 + clamp(in.misc.w, 0.0, 1.0) * 4.0;
    // A ramp's axis is `(cos θ, sin θ)` — always unit length — and an absent
    // ramp leaves `grad_axis` all-zero, so the direction vector *is* the
    // present/absent answer. That is what freed `misc.w` for `smooth`.
    let has_gradient = dot(in.grad_axis.xy, in.grad_axis.xy) > 0.25;

    // ── Drop shadow (drawn beneath the surface) ───────────────────────────
    // `spread` grows/shrinks the shadow shape (and its corner radii) before the
    // blur, matching CSS `box-shadow` spread; clamp to non-negative extents.
    var shadow_a = 0.0;
    if (in.shadow_color.a > 0.0 && (shadow_blur > 0.0 || shadow_spread != 0.0
        || abs(shadow_offset.x) > 0.0 || abs(shadow_offset.y) > 0.0)) {
        let s_half = max(in.half_size + vec2<f32>(shadow_spread), vec2<f32>(0.0));
        let s_radii = max(in.radii + vec4<f32>(shadow_spread), vec4<f32>(0.0));
        // §Q2: a shadow uses the same `n` as its caster — a shadow with a
        // different corner profile than the shape casting it reads as a bug.
        let sdist = sd_rounded_box(in.local_pos - shadow_offset, s_half, s_radii, corner_n);
        let soft = max(shadow_blur, 0.5);
        shadow_a = (1.0 - smoothstep(0.0, soft, sdist)) * in.shadow_color.a;
    }

    // ── Surface fill + inner border ───────────────────────────────────────
    let fdist = sd_rounded_box(in.local_pos, in.half_size, in.radii, corner_n);
    let edge_softness = max(length(vec2<f32>(dpdx(fdist), dpdy(fdist))), 1e-5);
    let fill_cov = smoothstep(edge_softness, 0.0, fdist);

    // The border occupies the band `-border_width < fdist < 0` (inside the outer
    // edge). Blend interior→border across the *inner* edge with the same
    // screen-space `edge_softness` used for the outer edge, so both edges of the
    // ring are SDF-anti-aliased — a hard `fdist > -border_width` test left the
    // inner edge jagged (visible on thin rings like `RadioButton`).
    var surface = in.color;
    // The ramp composites over the element's own fill (straight-alpha src-over),
    // so a translucent ramp brightens/darkens the surface instead of replacing
    // it — the shimmer case — while an opaque one paints the surface outright.
    if (has_gradient) {
        let g = gradient_color(in);
        let a = g.a + surface.a * (1.0 - g.a);
        if (a > 0.0) {
            surface = vec4<f32>(
                (g.rgb * g.a + surface.rgb * surface.a * (1.0 - g.a)) / a,
                a,
            );
        }
    }
    if (border_width > 0.0) {
        let border_cov = smoothstep(
            -border_width - edge_softness,
            -border_width + edge_softness,
            fdist,
        );
        surface = mix(in.color, in.border_color, border_cov);
    }

    let a_top = fill_cov * surface.a;
    let a_bot = shadow_a * (1.0 - a_top);
    let out_a = a_top + a_bot;
    if (out_a <= 0.0) {
        discard;
    }
    let out_rgb = (surface.rgb * a_top + in.shadow_color.rgb * a_bot) / out_a;
    return vec4<f32>(out_rgb, out_a * opacity);
}
