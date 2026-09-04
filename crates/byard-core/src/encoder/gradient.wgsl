// The gradient ramp, shared verbatim by every pipeline that paints one
// (RFC-0035, RFC-0037 resolved question "share the descriptor or fork it").
//
// This file is not a shader. It is prepended to the shaders that need it
// (`decorated_box.wgsl`, `canvas_fill.wgsl`), so a box gradient and a path
// gradient are not "the same algorithm", they are the same instructions. The
// alternative, two copies that agree today, is how a fill and a border end up
// a shade apart after somebody improves one of them.
//
// Everything here takes plain parameters rather than a pipeline's own
// `VertexOutput`, which is exactly what makes it shareable: a caller supplies
// the fragment's position in its shape's local space and the shape's
// half-extent, and how it arrived at those is its own business. For a box they
// come from the quad; for a filled path they come from the `uv` interpolated
// across its mesh, which is the same normalised space measured a different way.

const GRAD_LINEAR: u32 = 0u;
const GRAD_RADIAL: u32 = 1u;
const GRAD_CONIC: u32 = 2u;
const GRAD_NONE: u32 = 3u;
const TAU: f32 = 6.28318530718;

/// The ramp's parameter at this fragment, for a **linear** gradient
/// (RFC-0001 §3.1): projects the point onto the ramp's axis, normalized so `0`
/// is the shape's leading edge along that axis and `1` the trailing one,
/// shifted by `offset` and **wrapped**, which is what makes an animated offset
/// a seamless travelling sweep.
///
/// Kept expression for expression as it was written, because every gradient in
/// every file written before this was extracted takes this path and has to
/// keep producing the same bits (INV-22).
fn linear_t(axis: vec4<f32>, local: vec2<f32>, half_size: vec2<f32>) -> f32 {
    let dir = axis.xy;
    // Half-extent of the shape measured along `dir` (a box is convex, so the
    // support function is just the weighted sum of the half-sizes).
    let extent = max(abs(dir.x) * half_size.x + abs(dir.y) * half_size.y, 1e-5);
    let raw = (dot(local, dir) / extent) * 0.5 + 0.5 + axis.w;
    return fract(raw);
}

/// The ramp's parameter for a **radial** gradient (RFC-0035): distance from the
/// centre, in units of `radius` half-diagonals.
///
/// The offset from the centre is divided by the shape's half-size before it is
/// measured, so the falloff is circular in the shape's *own* aspect: a glow on
/// a 2:1 card stays a circle rather than being stretched into an ellipse by the
/// element it happens to live in.
fn radial_t(axis: vec4<f32>, local: vec2<f32>, half_size: vec2<f32>) -> f32 {
    let center = (axis.xy - vec2<f32>(0.5)) * 2.0 * half_size;
    let half = max(half_size, vec2<f32>(1e-5));
    let aspect = half / max(half.x, half.y);
    let d = (local - center) / half * aspect;
    return clamp(length(d) / max(axis.z, 1e-5), 0.0, 1.0);
}

/// The ramp's parameter for a **conic** gradient (RFC-0035): the fragment's
/// angle around the centre, measured from `start` and wrapped into `0..1`.
///
/// `fract` of a full turn is what makes the sweep meet itself: the stop
/// interpolation below is cyclic in `t`, so there is no seam at the start
/// angle as long as `from` and `to` are the same colour, which is what a dial
/// that wraps means.
fn conic_t(axis: vec4<f32>, local: vec2<f32>, half_size: vec2<f32>) -> f32 {
    let center = (axis.xy - vec2<f32>(0.5)) * 2.0 * half_size;
    let d = local - center;
    return fract((atan2(d.y, d.x) - axis.z) / TAU + 1.0);
}

/// The three-stop interpolation, given a ramp parameter somebody else
/// computed.
///
/// Split out from `gradient_color` for the one caller whose `t` cannot come
/// from a point in a box: a conic stroke on an `arc` measures the fragment's
/// angle within *the arc's own sweep*, so that the ring's colour tracks its
/// geometry rather than the rectangle the arc happens to sit in. That caller
/// still has to interpolate the stops the same way as everything else, and
/// this is how it does that without owning a second copy of them.
fn gradient_stops(
    t: f32,
    mid_pos: f32,
    grad_from: vec4<f32>,
    grad_mid: vec4<f32>,
    grad_to: vec4<f32>,
) -> vec4<f32> {
    if (t < mid_pos) {
        return mix(grad_from, grad_mid, t / max(mid_pos, 1e-5));
    }
    return mix(grad_mid, grad_to, (t - mid_pos) / max(1.0 - mid_pos, 1e-5));
}

/// The gradient's colour at this fragment: one scalar `t` per kind, then the
/// *same* three-stop interpolation for all of them. `mid` splits the ramp at
/// `mid_pos`, so a three-stop highlight band (transparent → bright →
/// transparent) is expressible, not just a two-stop fade.
fn gradient_color(
    kind: u32,
    grad_from: vec4<f32>,
    grad_mid: vec4<f32>,
    grad_to: vec4<f32>,
    axis: vec4<f32>,
    local: vec2<f32>,
    half_size: vec2<f32>,
) -> vec4<f32> {
    var t = 0.0;
    var mid_pos = 0.0;
    if (kind == GRAD_LINEAR) {
        t = linear_t(axis, local, half_size);
        mid_pos = clamp(axis.z, 0.0, 1.0);
    } else if (kind == GRAD_RADIAL) {
        t = radial_t(axis, local, half_size);
        mid_pos = clamp(axis.w, 0.0, 1.0);
    } else {
        t = conic_t(axis, local, half_size);
        mid_pos = clamp(axis.w, 0.0, 1.0);
    }
    return gradient_stops(t, mid_pos, grad_from, grad_mid, grad_to);
}
