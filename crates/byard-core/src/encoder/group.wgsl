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
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) opacity: f32,
};

@group(0) @binding(0) var picture: texture_2d<f32>;

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
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // The picture was drawn with ordinary alpha blending over a cleared,
    // transparent target, which leaves its colour premultiplied by its own
    // alpha. Scaling all four channels by the opacity keeps it premultiplied,
    // and the pipeline's blend state is the premultiplied one to match.
    let texel = textureLoad(picture, vec2<i32>(floor(in.position.xy)), 0);
    return texel * in.opacity;
}
