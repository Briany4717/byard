// CanvasFill (RFC-0037): a tessellated path, filled with a colour or with the
// same gradient ramp a box is filled with.
//
// `gradient.wgsl` is prepended to this file at build time, so `gradient_color`
// below is the one the decorated box calls, not a copy of it.

struct VertexInput {
    // Position in the canvas' logical-pixel space, as tessellated.
    @location(0) pos: vec2<f32>,
    // Normalised position inside the path's bounding box, which is what the
    // gradient is measured in: a vertical area fade is `uv.y`.
    @location(1) uv: vec2<f32>,
};

struct InstanceInput {
    @location(2) color: vec4<f32>,
    @location(3) grad_from: vec4<f32>,
    @location(4) grad_mid: vec4<f32>,
    @location(5) grad_to: vec4<f32>,
    @location(6) grad_axis: vec4<f32>,
    // (opacity, depth, 0, 0)
    @location(7) misc: vec4<f32>,
    // Paint-time transform (RFC-0011): (translate.xy, scale.xy) and
    // (rotate, origin.xy), packed for the same reason the decorated box packs
    // them, sixteen vertex attributes is the portable floor.
    @location(8) t_translate_scale: vec4<f32>,
    @location(9) t_rotate_origin: vec3<f32>,
    // 0 = linear, 1 = radial, 2 = conic, 3 = no gradient at all.
    @location(10) grad_kind: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) grad_from: vec4<f32>,
    @location(3) grad_mid: vec4<f32>,
    @location(4) grad_to: vec4<f32>,
    @location(5) grad_axis: vec4<f32>,
    @location(6) misc: vec4<f32>,
    @location(7) @interpolate(flat) grad_kind: u32,
};

@group(0) @binding(0) var<uniform> viewport_size: vec2<f32>;

/// Applies a paint-time transform (RFC-0011) to a world-space position:
/// rotate + scale about `origin`, then translate. Identity inputs collapse to
/// `world` unchanged.
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

    let world = apply_transform(
        vertex.pos,
        instance.t_translate_scale.xy,
        instance.t_translate_scale.zw,
        instance.t_rotate_origin.x,
        instance.t_rotate_origin.yz,
    );
    out.position = vec4<f32>(
        (world.x / viewport_size.x) * 2.0 - 1.0,
        1.0 - (world.y / viewport_size.y) * 2.0,
        instance.misc.y,
        1.0
    );
    out.uv = vertex.uv;
    out.color = instance.color;
    out.grad_from = instance.grad_from;
    out.grad_mid = instance.grad_mid;
    out.grad_to = instance.grad_to;
    out.grad_axis = instance.grad_axis;
    out.misc = instance.misc;
    out.grad_kind = instance.grad_kind;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var surface = in.color;
    if (in.grad_kind != GRAD_NONE) {
        // The shared ramp measures a fragment in its shape's *local* space,
        // centred on the shape with a half-extent. A path's `uv` is already
        // that space, normalised: `(uv - 0.5)` centres it and a half-extent of
        // `0.5` keeps the units, so the same call that fills a box fills this.
        surface = gradient_color(
            in.grad_kind,
            in.grad_from,
            in.grad_mid,
            in.grad_to,
            in.grad_axis,
            in.uv - vec2<f32>(0.5),
            vec2<f32>(0.5),
        );
    }
    let alpha = surface.a * in.misc.x;
    if (alpha <= 0.0) {
        discard;
    }
    return vec4<f32>(surface.rgb, alpha * clip_coverage(in.position.xy));
}
