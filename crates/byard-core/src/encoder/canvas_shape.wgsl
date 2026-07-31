// CanvasShape pipeline (RFC-0020 §2, Tier 1): programmatic 2-D shapes — arcs,
// circles, lines, and (rounded) rectangles — rendered by evaluating each
// shape's closed-form signed-distance function analytically per fragment.
// Resolution-independent, atlas-free, and trivially animatable: an animated
// `sweep`/`dash_offset` is just new per-instance data, never a re-tessellation.
//
// Complex `path(d: …)` commands do not reach this shader — they rasterize
// through the VectorMSDF pipeline (RFC-0020 §2, Tier 2).

struct VertexInput {
    @location(0) quad_pos: vec2<f32>,
};

struct InstanceInput {
    // Quad bounds in logical px, already inflated to cover stroke + AA fringe
    // (`CanvasShape::bounds`).
    @location(1) rect: vec4<f32>,
    // Shape params, absolute logical px / radians (layout per kind — see
    // `frame::CANVAS_SHAPE_*`).
    @location(2) params0: vec4<f32>,
    @location(3) params1: vec4<f32>,
    @location(4) stroke_color: vec4<f32>,
    @location(5) fill_color: vec4<f32>,
    // (stroke_width, dash_len, dash_gap, dash_offset)
    @location(6) stroke_dash: vec4<f32>,
    // (opacity, draw-order depth, kind, cap) — kind/cap are small integers
    // carried exactly in f32.
    @location(7) misc: vec4<f32>,
    // Paint-time transform (RFC-0011); identity is a free no-op.
    @location(8) t_translate: vec2<f32>,
    @location(9) t_scale: vec2<f32>,
    @location(10) t_rotate: f32,
    @location(11) t_origin: vec2<f32>,
    // Shape group (RFC-0031 §S4): (mode, param, first_member, member_count).
    // `mode == GROUP_NONE` is every shape that existed before RFC-0031 and
    // takes the identical path below. `first_member` indexes `shape_records`
    // directly, so a group's records are ordinary per-instance data.
    @location(12) group: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    // Un-transformed logical-px position: interpolating it across the
    // *transformed* triangle is the standard inverse mapping, so the SDF is
    // evaluated in shape-local space and shapes rotate/scale correctly.
    @location(0) world_pos: vec2<f32>,
    @location(1) params0: vec4<f32>,
    @location(2) params1: vec4<f32>,
    @location(3) stroke_color: vec4<f32>,
    @location(4) fill_color: vec4<f32>,
    @location(5) stroke_dash: vec4<f32>,
    @location(6) misc: vec4<f32>,
    // Flat: a group's mode and member range are per-instance integers, and
    // interpolating them would address records that do not exist.
    @location(7) @interpolate(flat) group: vec4<f32>,
};

@group(0) @binding(0) var<uniform> viewport_size: vec2<f32>;

/// One member of a shape group (RFC-0031 §S4) — exactly the fields
/// `eval_shape` consumes. Mirrors `frame::ShapeRecord` field for field.
struct ShapeRecord {
    params0: vec4<f32>,
    params1: vec4<f32>,
    fill_color: vec4<f32>,
    stroke_color: vec4<f32>,
    // (kind, cap, 0, 0)
    misc: vec4<f32>,
};

/// This frame's shape-record pool, bound whole at offset zero — a group's
/// `first_member` is an index into it.
@group(1) @binding(0) var<storage, read> shape_records: array<ShapeRecord>;

const PI: f32 = 3.14159265358979;
const TAU: f32 = 6.28318530717959;
// A distance far past any quad extent — "no coverage from this term".
const FAR: f32 = 1e6;

const KIND_ARC: u32 = 0u;
const KIND_CIRCLE: u32 = 1u;
const KIND_LINE: u32 = 2u;
const KIND_RECT: u32 = 3u;
const KIND_NGON: u32 = 4u;

const GROUP_NONE: u32 = 0u;
const GROUP_FUSE: u32 = 1u;
const GROUP_MORPH: u32 = 2u;
// RFC-0031 §S5: a literal bound, so the loop unrolls and the per-fragment cost
// is provable rather than data-dependent.
const MAX_GROUP_MEMBERS: u32 = 8u;

const CAP_BUTT: u32 = 0u;
const CAP_ROUND: u32 = 1u;
const CAP_SQUARE: u32 = 2u;

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

    let world_pos = instance.rect.xy + vertex.quad_pos * instance.rect.zw;
    out.world_pos = world_pos;

    let transformed = apply_transform(
        world_pos,
        instance.t_translate,
        instance.t_scale,
        instance.t_rotate,
        instance.t_origin,
    );

    // misc.y carries the draw-order depth (NDC-z, RFC-0011) so shapes honour
    // global paint order against every other pipeline.
    out.position = vec4<f32>(
        (transformed.x / viewport_size.x) * 2.0 - 1.0,
        1.0 - (transformed.y / viewport_size.y) * 2.0,
        instance.misc.y,
        1.0
    );

    out.params0 = instance.params0;
    out.params1 = instance.params1;
    out.stroke_color = instance.stroke_color;
    out.fill_color = instance.fill_color;
    out.stroke_dash = instance.stroke_dash;
    out.misc = instance.misc;
    out.group = instance.group;
    return out;
}

// Wraps an angle to [-PI, PI].
fn wrap_angle(a: f32) -> f32 {
    return a - TAU * round(a / TAU);
}

/// Lⁿ norm of a **non-negative** 2-vector, paired with the magnitude of its own
/// gradient (RFC-0031 §S1–S2).
///
/// `n == 2` is the Euclidean norm and its gradient is exactly 1 — the circular
/// corner this pipeline drew before RFC-0031. Above 2 the norm is *not* a true
/// signed distance: on the corner diagonal its gradient is `2^(1/n - 1/2)`,
/// which falls to ≈0.79 at `n = 6`, so the corner's fringe would come out ~26 %
/// wider than the edge's. Returning the gradient alongside the value lets
/// `sd_rounded_box` normalise the field once, at the source, so the fragment
/// stage's screen-space `aa` stays the single coverage rule for every shape
/// kind.
fn lp_norm(v: vec2<f32>, n: f32) -> vec2<f32> {
    let a = pow(v.x, n) + pow(v.y, n);
    if (a <= 0.0) {
        return vec2<f32>(0.0, 1.0);
    }
    let f = pow(a, 1.0 / n);
    let g = vec2<f32>(pow(v.x / f, n - 1.0), pow(v.y / f, n - 1.0));
    return vec2<f32>(f, max(length(g), 1e-4));
}

fn sd_rounded_box(p: vec2<f32>, b: vec2<f32>, r: f32, n: f32) -> f32 {
    // Clamped for the same reason as the box pipelines: past half the extent the
    // rounded-rect field folds in on itself (RFC-0001 §3.1).
    let rc = min(r, min(b.x, b.y));
    let q = abs(p) - b + vec2<f32>(rc);
    let corner = max(q, vec2<f32>(0.0));
    let inner = min(max(q.x, q.y), 0.0);
    // RFC-0031 §S1: the L² path, verbatim and unconditional at `smooth: 0`.
    if (n == 2.0) {
        return inner + length(corner) - rc;
    }
    // `inner` is non-zero only where one of `corner`'s components is zero, and
    // there `lp.y == 1` — so dividing the whole expression normalises exactly
    // the corner arc and leaves the straight edges untouched.
    let lp = lp_norm(corner, n);
    return (inner + lp.x - rc) / lp.y;
}

// Per-fragment shape evaluation: signed stroke distance (< 0 inside the
// stroked band), signed fill distance (< 0 inside the filled region), and the
// arc-length parameter `t` (logical px along the path) driving dashes.
struct ShapeDist {
    stroke: f32,
    fill: f32,
    t: f32,
};

fn eval_shape(p: vec2<f32>, kind: u32, cap: u32, half_w: f32,
              params0: vec4<f32>, params1: vec4<f32>) -> ShapeDist {
    var out: ShapeDist;
    out.stroke = FAR;
    out.fill = FAR;
    out.t = 0.0;

    if (kind == KIND_CIRCLE) {
        let c = params0.xy;
        let r = max(params0.z, 0.0);
        let rel = p - c;
        let len = length(rel);
        out.stroke = abs(len - r) - half_w;
        out.fill = len - r;
        // Dash parameter: arc length from the +X axis; `dash_offset` sets the
        // phase, so the (arbitrary) zero point is not observable.
        out.t = (wrap_angle(atan2(rel.y, rel.x)) + PI) * r;
        return out;
    }

    if (kind == KIND_ARC) {
        let c = params0.xy;
        let r = max(params0.z, 0.0);
        let start = params0.w;
        let sweep = params1.x;
        let rel = p - c;
        let len = length(rel);
        let ang = atan2(rel.y, rel.x);

        var half_sweep = min(abs(sweep), TAU) * 0.5;
        // A square cap extends the arc by half the stroke width of arc
        // length past each endpoint, then ends flat like a butt cap.
        if (cap == CAP_SQUARE) {
            half_sweep = half_sweep + half_w / max(r, 1e-3);
        }
        let mid = start + sweep * 0.5;
        let delta = wrap_angle(ang - mid);
        let ring = abs(len - r) - half_w;

        if (abs(delta) <= half_sweep) {
            out.stroke = ring;
        } else if (cap == CAP_ROUND) {
            // Round caps: semicircular ends centred on the endpoints.
            let p0 = c + r * vec2<f32>(cos(start), sin(start));
            let p1 = c + r * vec2<f32>(cos(start + sweep), sin(start + sweep));
            out.stroke = min(length(p - p0), length(p - p1)) - half_w;
        } else {
            // Butt/square: flat angular cutoff. `(|delta| - half) * len`
            // approximates the px distance past the end plane, keeping the
            // cut anti-aliased instead of a hard step.
            out.stroke = max(ring, (abs(delta) - half_sweep) * max(len, 1e-3));
        }

        // Fill = the swept circular sector (pie wedge).
        out.fill = max(len - r, (abs(delta) - half_sweep) * max(len, 1e-3));

        // Arc length from the sweep's starting endpoint, following the sweep
        // direction, so dashes march from `start` regardless of sign.
        out.t = (half_sweep + delta * sign(sweep)) * r;
        return out;
    }

    if (kind == KIND_LINE) {
        let a = params0.xy;
        let b = params0.zw;
        let ba = b - a;
        let pa = p - a;
        let len2 = max(dot(ba, ba), 1e-6);
        let seg_len = sqrt(len2);
        let h = clamp(dot(pa, ba) / len2, 0.0, 1.0);
        // Perpendicular distance to the infinite line, and the overshoot past
        // either endpoint along it.
        let u = dot(pa, ba) / seg_len;
        let perp = abs(pa.x * ba.y - pa.y * ba.x) / seg_len;
        let over = max(max(-u, u - seg_len), 0.0);

        if (cap == CAP_ROUND) {
            out.stroke = length(pa - ba * h) - half_w;
        } else if (cap == CAP_SQUARE) {
            // Chebyshev-style: the stroke reaches half_w past the endpoints.
            out.stroke = max(perp, over) - half_w;
        } else {
            // Butt: the band ends exactly at the endpoints.
            out.stroke = max(perp - half_w, over);
        }
        out.t = clamp(u, 0.0, seg_len);
        return out;
    }

        if (kind == KIND_NGON) {
        // KIND_NGON (RFC-0031 §"`ngon`"): an n-fold symmetric rounded polygon
        // or star. params0 = (cx, cy, r, corner), params1 = (inner, rotate, n).
        //
        // Built by folding the angle into one 2·an sector and mirroring it, so
        // the whole boundary is a single segment: from the outer point at the
        // sector's axis to the inner notch at its edge. That fold is exact for
        // an *integer* n and only for an integer n — a fractional one leaves a
        // partial final sector whose seam sweeps the shape, which is §Q10's
        // reason `n` is not animatable and §S9's reason morphing interpolates
        // fields instead.
        let c = params0.xy;
        let r = max(params0.z, 0.0);
        let corner = clamp(params0.w, 0.0, r);
        let inner = clamp(params1.x, 0.0, 1.0);
        let n = max(floor(params1.z + 0.5), 3.0);
        let an = PI / n;

        // Into the shape's own axis, with an outer point at the top.
        let rot = -params1.y;
        let rc = cos(rot);
        let rs = sin(rot);
        let rel0 = p - c;
        let rel = vec2<f32>(rel0.x * rc - rel0.y * rs, rel0.x * rs + rel0.y * rc);

        let len = length(rel);
        // Angle from straight up, folded into [-an, an) and then mirrored.
        let ang = atan2(rel.x, -rel.y);
        let fold = ang - 2.0 * an * floor((ang + an) / (2.0 * an));
        let q = vec2<f32>(len * abs(sin(fold)), len * cos(fold));

        // `corner` is applied by building the sharp shape one `corner`
        // smaller and inflating the field by the same amount — so the outer
        // point lands back at exactly `r`, whatever the rounding.
        let r_out = max(r - corner, 0.0);
        let r_in = max(r * inner * cos(an) - corner, 0.0);
        let a_pt = vec2<f32>(0.0, r_out);
        let b_pt = vec2<f32>(r_in * sin(an), r_in * cos(an));
        let e = b_pt - a_pt;
        let w = q - a_pt;
        let h = clamp(dot(w, e) / max(dot(e, e), 1e-6), 0.0, 1.0);
        // The origin is always on the negative side of this edge (`e.x >= 0`
        // and `a_pt.y >= 0`), so a negative cross product is "inside".
        let cross = e.x * w.y - e.y * w.x;
        let sd_ngon = length(w - e * h) * sign(cross) - corner;

        out.stroke = abs(sd_ngon) - half_w;
        out.fill = sd_ngon;
        // Dashes along an ngon perimeter are not defined in v1, as for rects:
        // `t` stays 0 and the dash mask reads that as a solid stroke.
        return out;
    }

    // KIND_RECT: params0 = (x, y, w, h), params1.x = corner radius,
    // params1.y = corner smoothing 0..1 (RFC-0031 §S1/§S3).
    let half_size = max(params0.zw * 0.5, vec2<f32>(0.0));
    let center = params0.xy + half_size;
    let radius = clamp(params1.x, 0.0, min(half_size.x, half_size.y));
    let corner_n = 2.0 + clamp(params1.y, 0.0, 1.0) * 4.0;
    let sd = sd_rounded_box(p - center, half_size, radius, corner_n);
    out.stroke = abs(sd) - half_w;
    out.fill = sd;
    // Dashes are not defined along a rect perimeter in v1 (RFC-0020): `t`
    // stays 0, which the dash mask treats as "always on" → a solid stroke.
    return out;
}

/// Linear sRGB → OKLab (RFC-0031 §Q8). Same coefficients RFC-0025's keyframe
/// blending and RFC-0010's `bg`/`color` transitions use, so a morph blending
/// two colours beside a `with` clause blending the same two does not visibly
/// disagree inside one frame.
fn linear_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
    let m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
    let s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
    let l_ = pow(max(l, 0.0), 1.0 / 3.0);
    let m_ = pow(max(m, 0.0), 1.0 / 3.0);
    let s_ = pow(max(s, 0.0), 1.0 / 3.0);
    return vec3<f32>(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    );
}

/// OKLab → linear sRGB.
fn oklab_to_linear(c: vec3<f32>) -> vec3<f32> {
    let l_ = c.x + 0.3963377774 * c.y + 0.2158037573 * c.z;
    let m_ = c.x - 0.1055613458 * c.y - 0.0638541728 * c.z;
    let s_ = c.x - 0.0894841775 * c.y - 1.2914855480 * c.z;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    return vec3<f32>(
        max(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s, 0.0),
        max(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s, 0.0),
        max(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s, 0.0),
    );
}

/// Blends two straight-alpha linear colours in OKLab (§Q8), alpha linearly.
fn mix_oklab(a: vec4<f32>, b: vec4<f32>, t: f32) -> vec4<f32> {
    let lab = mix(linear_to_oklab(a.rgb), linear_to_oklab(b.rgb), t);
    return vec4<f32>(oklab_to_linear(lab), mix(a.a, b.a, t));
}

/// Polynomial smooth-minimum (RFC-0031 §S7), returning both the blended
/// distance **and** the blend weight.
///
/// The `.y` component is what makes differently-coloured fusion look
/// deliberate rather than like a z-fighting bug: colour is mixed by the same
/// factor that produced the geometry, so the surface bridge and the colour
/// transition are the same event.
fn smin(a: f32, b: f32, k: f32) -> vec2<f32> {
    let h = max(k - abs(a - b), 0.0) / k;
    let m = h * h * 0.25;
    // `.y` is the weight *towards `b`*, so it must be small where `a` is the
    // closer surface. WGSL's `select(false_value, true_value, condition)`
    // reverses GLSL's ternary, and getting that backwards inverts every fused
    // colour — the far member's colour would paint the near member's body.
    return vec2<f32>(min(a, b) - m * k, select(1.0 - m, m, a < b));
}

/// One member record, evaluated at `p`.
fn eval_record(rec: ShapeRecord, p: vec2<f32>, half_w: f32) -> ShapeDist {
    return eval_shape(
        p,
        u32(rec.misc.x + 0.5),
        u32(rec.misc.y + 0.5),
        half_w,
        rec.params0,
        rec.params1,
    );
}

/// What one fragment resolves a shape — or a whole group — to: the two signed
/// fields, the dash parameter, and the fill colour, which a group may have
/// *computed* rather than simply carried (RFC-0031 §S7/§S10 blend colours by
/// the same factor that produced the geometry).
struct Resolved {
    stroke: f32,
    fill: f32,
    t: f32,
    fill_color: vec4<f32>,
    stroke_color: vec4<f32>,
};

/// Evaluates this instance: one shape, or the group it heads (RFC-0031 §S4).
///
/// `GROUP_NONE` is every shape that existed before RFC-0031 and is the branch
/// taken by every shape that does not opt in — one `eval_shape` call and the
/// instance's own colours, exactly as before.
fn resolve(in: VertexOutput, kind: u32, cap: u32, half_w: f32) -> Resolved {
    var out: Resolved;
    out.fill_color = in.fill_color;
    out.stroke_color = in.stroke_color;

    let mode = u32(in.group.x + 0.5);
    if (mode == GROUP_NONE) {
        let d = eval_shape(in.world_pos, kind, cap, half_w, in.params0, in.params1);
        out.stroke = d.stroke;
        out.fill = d.fill;
        out.t = d.t;
        return out;
    }

    let first = u32(in.group.z + 0.5);
    let count = min(u32(in.group.w + 0.5), MAX_GROUP_MEMBERS);
    if (count == 0u) {
        out.stroke = FAR;
        out.fill = FAR;
        out.t = 0.0;
        return out;
    }

    if (mode == GROUP_FUSE) {
        // §S7: a smooth-minimum union over the members, with colour carried by
        // the same blend factor. `k <= 0` degenerates to a hard `min`, which is
        // the plain union — `fuse: 0` is exactly "these shapes, unfused".
        let k = max(in.group.y, 1e-4);
        var d_fill = FAR;
        var col = vec4<f32>(0.0);
        for (var i = 0u; i < MAX_GROUP_MEMBERS; i = i + 1u) {
            if (i >= count) { break; }
            let rec = shape_records[first + i];
            let s = eval_record(rec, in.world_pos, half_w);
            if (i == 0u) {
                d_fill = s.fill;
                col = rec.fill_color;
            } else {
                let r = smin(d_fill, s.fill, k);
                d_fill = r.x;
                col = mix(col, rec.fill_color, r.y);
            }
        }
        out.fill = d_fill;
        // §S8: the fused outline is the boundary of the *union*, not a union of
        // the members' own outlines — which would draw seams through the
        // interior of a body whose entire purpose is not to have any. The
        // head's stroke width and colour govern; per-member strokes are inert
        // and diagnosed at compile time.
        out.stroke = abs(d_fill) - half_w;
        // §Q6: the dash parameter is not defined on a fused boundary (there is
        // no closed-form arc length for the union of arbitrary SDFs), so it
        // stays 0 — which the dash mask reads as a solid stroke. Asking for
        // dashes here is a compile-time error rather than a crawling
        // approximation.
        out.t = 0.0;
        out.fill_color = col;
        return out;
    }

    if (mode == GROUP_MORPH) {
        // §S10: the group is a *sequence*, and `phase` indexes it.
        // `fract` before scaling handles negative phases as well as
        // overshooting ones, so the wrap is total: sweeping 0 → count returns
        // to the first shape with no discontinuity, which is what makes the
        // Material 3 loader a loop rather than a stall (§Q9).
        let cf = f32(count);
        let ph = fract(in.group.y / cf) * cf;
        let i0 = min(u32(floor(ph)), count - 1u);
        let i1 = (i0 + 1u) % count;
        let t = ph - floor(ph);

        // §S9: interpolate the *fields*, not the vertex counts. `mix` of two
        // evaluated distances has no seam, works between any two kinds — a
        // circle can morph to a seven-pointed star — and asks nothing of the
        // shapes being related.
        let ra = shape_records[first + i0];
        let rb = shape_records[first + i1];
        let da = eval_record(ra, in.world_pos, half_w);
        let db = eval_record(rb, in.world_pos, half_w);
        out.stroke = mix(da.stroke, db.stroke, t);
        out.fill = mix(da.fill, db.fill, t);
        out.t = mix(da.t, db.t, t);
        // Colour rides the same scalar, so a morph that also changes colour is
        // one animation rather than two that can desynchronise.
        out.fill_color = mix_oklab(ra.fill_color, rb.fill_color, t);
        out.stroke_color = mix_oklab(ra.stroke_color, rb.stroke_color, t);
        return out;
    }

    // An unrecognised mode resolves to its first member rather than to a hole
    // in the frame.
    let rec = shape_records[first];
    let d = eval_record(rec, in.world_pos, half_w);
    out.stroke = d.stroke;
    out.fill = d.fill;
    out.t = d.t;
    out.fill_color = rec.fill_color;
    out.stroke_color = rec.stroke_color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let opacity = in.misc.x;
    let kind = u32(in.misc.z + 0.5);
    let cap = u32(in.misc.w + 0.5);
    let stroke_width = in.stroke_dash.x;
    let dash_len = in.stroke_dash.y;
    let dash_gap = in.stroke_dash.z;
    let dash_offset = in.stroke_dash.w;

    // Screen-space AA width in logical px (`fwidth`-based — RFC-0020 §"AA
    // quality"), from the interpolated world position so it scales with DPI
    // and any paint-time transform.
    let aa = max(fwidth(in.world_pos.x) + fwidth(in.world_pos.y), 1e-4) * 0.5;

    // Sub-pixel strokes: clamp the rendered half-width up to one AA unit and
    // scale alpha down proportionally, so a 0.5px stroke renders as a fainter
    // 1px line instead of shimmering in and out of coverage.
    let half_w = stroke_width * 0.5;
    let half_w_eff = max(half_w, aa);
    let thin_alpha = clamp(half_w / half_w_eff, 0.0, 1.0);

    let d = resolve(in, kind, cap, half_w_eff);

    // ── Coverages ─────────────────────────────────────────────────────────
    var stroke_cov = 1.0 - smoothstep(0.0, aa, d.stroke);
    let fill_cov = 1.0 - smoothstep(0.0, aa, d.fill);

    // Dash mask along the path parameter (logical px, so `aa` applies).
    if (dash_len > 0.0 && stroke_cov > 0.0) {
        let period = dash_len + max(dash_gap, 0.0);
        let s = fract((d.t + dash_offset) / period) * period;
        // > 0 inside the "on" interval [0, dash_len], < 0 in the gap.
        let edge = min(s, dash_len - s);
        stroke_cov = stroke_cov * smoothstep(-aa, aa, edge);
    }

    // ── Composite: stroke over fill ───────────────────────────────────────
    let a_stroke = stroke_cov * d.stroke_color.a * thin_alpha;
    let a_fill = fill_cov * d.fill_color.a * (1.0 - a_stroke);
    let out_a = a_stroke + a_fill;
    if (out_a <= 0.001) {
        discard;
    }
    let rgb = (d.stroke_color.rgb * a_stroke + d.fill_color.rgb * a_fill) / out_a;
    return vec4<f32>(rgb, out_a * opacity);
}
