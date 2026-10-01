// The opacity-group composite (RFC-0011 T4).
//
// A translucent container's subtree is drawn opaque into an offscreen target
// of the same size as the frame, and this draws that picture back once, at the
// group's opacity and the group's draw-order depth. Faded as one image, the
// subtree's own overlaps (text over its card, two siblings crossing) no longer
// show through each other, which is the whole difference from multiplying the
// alpha into every primitive.
//
// The quad covers the viewport and every fragment reads the texel at its own
// framebuffer position: the picture was rasterised in exactly this space, so
// there is nothing to map and nothing to filter. A texel with nothing in it is
// fully transparent, so the uncovered part of the quad writes nothing visible.

struct GroupInstance {
    // (opacity, depth)
    @location(1) params: vec2<f32>,
    // (cos, sin, pivot_x, pivot_y): the rotation the composite applies, pivot
    // in physical pixels. (1, 0, _, _) for a plain opacity group.
    @location(2) rotation: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) opacity: f32,
    @location(1) @interpolate(flat) rotation: vec4<f32>,
};

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var picture_sampler: sampler;

@vertex
fn vs_main(@location(0) quad_pos: vec2<f32>, instance: GroupInstance) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(
        quad_pos.x * 2.0 - 1.0,
        1.0 - quad_pos.y * 2.0,
        instance.params.y,
        1.0,
    );
    out.opacity = instance.params.x;
    out.rotation = instance.rotation;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // The picture was drawn with ordinary alpha blending over a cleared,
    // transparent target, which leaves its colour premultiplied by its own
    // alpha. Scaling all four channels by the opacity keeps it premultiplied,
    // and the pipeline's blend state is the premultiplied one to match.
    let c = in.rotation.x;
    let s = in.rotation.y;
    if (s == 0.0 && c == 1.0) {
        // Unrotated: the picture is in this pass's own pixel space.
        let texel = textureLoad(picture, vec2<i32>(floor(in.position.xy)), 0);
        return texel * in.opacity;
    }
    // Rotated (RFC-0011 text transforms): the picture was drawn upright, so
    // each screen pixel reads the upright position the rotation carried here,
    // turning back about the pivot. Between texel centres, so filtered.
    let pivot = in.rotation.zw;
    let d = in.position.xy - pivot;
    let upright = pivot + vec2<f32>(c * d.x + s * d.y, -s * d.x + c * d.y);
    let size = vec2<f32>(textureDimensions(picture));
    if (any(upright < vec2<f32>(0.0)) || any(upright >= size)) {
        return vec4<f32>(0.0);
    }
    let texel = textureSampleLevel(picture, picture_sampler, upright / size, 0.0);
    return texel * in.opacity;
}
