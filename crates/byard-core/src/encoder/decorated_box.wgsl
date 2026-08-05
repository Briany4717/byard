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
    // (opacity, depth, shadow_spread, smooth), `misc.w` is the RFC-0031 §S1
    // corner smoothing, and nothing else: the gradient's own tag lives in
    // `grad_kind` below, one lane one owner (INV-28).
    @location(7) misc: vec4<f32>,
    // Paint-time transform (RFC-0011); identity is a free no-op below. Two
    // attributes rather than four, because 16 locations is the portable floor
    // and this pipeline needs every one of them: (translate.xy, scale.xy) and
    // (rotate, origin.xy) are already contiguous in the uploaded bytes, so
    // reading them wide is free. `opacity` isn't part of this block, `misc.x`
    // above stays authoritative.
    @location(8) t_translate_scale: vec4<f32>,
    @location(9) t_rotate_origin: vec3<f32>,
    // The gradient (RFC-0001 §3.1, RFC-0035): three stops, four control floats
    // whose meaning depends on the kind, and the kind itself.
    @location(10) grad_from: vec4<f32>,
    @location(11) grad_mid: vec4<f32>,
    @location(12) grad_to: vec4<f32>,
    // linear: (dir_x, dir_y, mid_pos, offset)
    // radial: (center_x, center_y, radius, mid_pos)
    // conic:  (center_x, center_y, start_angle, mid_pos)
    @location(13) grad_axis: vec4<f32>,
    // 0 = linear, 1 = radial, 2 = conic, 3 = no gradient at all.
    @location(14) grad_kind: u32,
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
    // Constant across the primitive: an integer cannot be interpolated, and
    // there is nothing here to interpolate anyway.
    @location(12) @interpolate(flat) grad_kind: u32,
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
        instance.t_translate_scale.xy,
        instance.t_translate_scale.zw,
        instance.t_rotate_origin.x,
        instance.t_rotate_origin.yz,
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
    out.grad_kind = instance.grad_kind;
    return out;
}

/// Lⁿ norm of a **non-negative** 2-vector, paired with the magnitude of its own
/// gradient (RFC-0031 §S1–S2).
///
/// `n == 2` is the Euclidean norm and its gradient is exactly 1, the circular
/// corner every pipeline drew before RFC-0031. Above 2 the norm is *not* a true
/// signed distance: on the corner diagonal its gradient is `2^(1/n - 1/2)`,
/// which falls to ≈0.79 at `n = 6`, so the corner's anti-aliased fringe would
/// come out ~26 % wider than the edge's, a smeared corner on exactly the
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
    // field folds in on itself and the corners visibly deform, a `radius: 20`
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
    // there `lp.y == 1`, so dividing the whole expression normalises exactly
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
    // The tag answers it directly (RFC-0035). It used to be inferred from
    // `grad_axis.xy` being a unit vector, which only worked while every
    // gradient was linear: a radial centred on the box's top-left corner is
    // `(0, 0)` and would have read as "no gradient".
    let has_gradient = in.grad_kind != GRAD_NONE;

    // ── Drop shadow (drawn beneath the surface) ───────────────────────────
    // `spread` grows/shrinks the shadow shape (and its corner radii) before the
    // blur, matching CSS `box-shadow` spread; clamp to non-negative extents.
    var shadow_a = 0.0;
    if (in.shadow_color.a > 0.0 && (shadow_blur > 0.0 || shadow_spread != 0.0
        || abs(shadow_offset.x) > 0.0 || abs(shadow_offset.y) > 0.0)) {
        let s_half = max(in.half_size + vec2<f32>(shadow_spread), vec2<f32>(0.0));
        let s_radii = max(in.radii + vec4<f32>(shadow_spread), vec4<f32>(0.0));
        // §Q2: a shadow uses the same `n` as its caster, a shadow with a
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
    // ring are SDF-anti-aliased, a hard `fdist > -border_width` test left the
    // inner edge jagged (visible on thin rings like `RadioButton`).
    var surface = in.color;
    // The ramp composites over the element's own fill (straight-alpha src-over),
    // so a translucent ramp brightens/darkens the surface instead of replacing
    // it, the shimmer case, while an opaque one paints the surface outright.
    if (has_gradient) {
        let g = gradient_color(
            in.grad_kind,
            in.grad_from,
            in.grad_mid,
            in.grad_to,
            in.grad_axis,
            in.local_pos,
            in.half_size,
        );
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
