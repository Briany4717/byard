//! Canvas shapes and paths (RFC-0020, RFC-0031, RFC-0037).
//!
//! A `Canvas` body is lowered to `CanvasItem`s and re-walked every tick:
//! each shape command's parameters are evaluated, so they are reactive and
//! animatable without any pooling, and the shape is emitted to the frame. Here
//! are the shape command parameters, filled paths and their tessellation
//! (cached across frames), shape groups with a combine mode, and the path
//! morph that blends two paths command by command on the CPU.

use super::animation::{hex_from_oklab, linear_to_srgb, oklab_from_hex};
use super::paint::dim_alpha;
use super::{CanvasItem, Interpreter};
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::Value;
use crate::parser::ast::{Attr, ElementNode, Expr, Member};

/// Where a canvas body's Tier-1 shapes go (RFC-0031 §S4).
///
/// A plain `Canvas` pushes each shape to the frame as its own instance, exactly
/// as it always has. A `Canvas` that declares a combine mode collects them into
/// one group instead, same walk, same per-tick re-evaluation of every
/// parameter expression, different destination, and the head is pushed once at
/// the end.
///
/// Routing this at the emission site rather than post-processing the frame's
/// shape pool is what keeps `for`/`when` working inside a group for free: the
/// members are whatever the walk produced, however it produced them.
#[derive(Debug, Default)]
pub(crate) struct ShapeGroupSink {
    /// The members collected so far, in declaration order.
    pub(super) members: Vec<byard_core::frame::ShapeRecord>,
    /// The union of their bounds, the head's quad (§S4).
    pub(super) bounds: Option<byard_core::frame::Rect>,
    /// The paint parameters the head carries on the members' behalf: a group
    /// has one stroke, one cap, one dash pattern and one opacity, taken from
    /// the first member that contributed (§S8 makes the head's stroke govern;
    /// there is no group-level stroke syntax, so the first shape's is it).
    pub(super) paint: Option<byard_core::frame::CanvasShape>,
    /// Whether more shapes were offered than [`MAX_GROUP_MEMBERS`] allows.
    ///
    /// [`MAX_GROUP_MEMBERS`]: byard_core::frame::MAX_GROUP_MEMBERS
    pub(super) overflowed: usize,
    /// `Some` in a `morph:` canvas: the body paths it holds, in order, which
    /// morph command by command on the CPU rather than as distance fields
    /// (RFC-0031 §S11).
    pub(super) morph_paths: Option<Vec<FilledPath>>,
}

/// A body path's evaluated geometry and paint, before tessellation
/// (RFC-0037), which is also one member of a path morph (RFC-0031 §S11).
#[derive(Clone, Debug)]
pub(crate) struct FilledPath {
    commands: Vec<PathCommand>,
    fill: [f32; 4],
    gradient: Option<byard_core::frame::Gradient>,
    even_odd: bool,
    /// The path's own `opacity:`, before the canvas's is applied.
    alpha: f32,
    span: Span,
}

impl ShapeGroupSink {
    /// Adds one shape as a member, unioning its bounds into the head's quad.
    fn push(&mut self, shape: &byard_core::frame::CanvasShape) {
        let b = shape.bounds();
        self.bounds = Some(match self.bounds {
            Some(u) => union_rect(u, b),
            None => b,
        });
        if self.paint.is_none() {
            self.paint = Some(shape.clone());
        }
        if self.members.len() < byard_core::frame::MAX_GROUP_MEMBERS {
            self.members
                .push(byard_core::frame::ShapeRecord::from_shape(shape));
        }
        self.overflowed += 1;
    }
}

/// A representative source position for a set of attributes, the first one's,
/// or the file start when there are none. Used where a diagnostic is raised
/// during the render walk, which has the element's attrs but not its node.
pub(super) fn attr_span(attrs: &[Attr]) -> Span {
    attrs.first().map_or(Span::new(0, 0), |a| a.span)
}

/// The smallest rect containing both.
fn union_rect(
    first: byard_core::frame::Rect,
    second: byard_core::frame::Rect,
) -> byard_core::frame::Rect {
    let left = first.x.min(second.x);
    let top = first.y.min(second.y);
    let right = (first.x + first.width).max(second.x + second.width);
    let bottom = (first.y + first.height).max(second.y + second.height);
    byard_core::frame::Rect::new(left, top, right - left, bottom - top)
}

/// Collects a `Canvas` body into [`CanvasItem`]s, keeping declaration order.
///
/// Anything that is neither a shape command nor `for`/`when` has already been
/// reported by `validate_canvas`, so it is dropped here rather than diagnosed
/// twice.
pub(super) fn lower_canvas_items(members: &[Member]) -> Vec<CanvasItem> {
    members
        .iter()
        .filter_map(|m| match m {
            Member::Element(c) if super::intrinsics::is_shape_command(c.name.as_str()) => {
                Some(CanvasItem::Shape(c.clone()))
            }
            Member::For {
                var,
                index,
                iter,
                body,
                ..
            } => Some(CanvasItem::For {
                var: var.clone(),
                index: index.clone(),
                iter: iter.clone(),
                body: lower_canvas_items(body),
            }),
            Member::When {
                cond, then, els, ..
            } => Some(CanvasItem::When {
                cond: cond.clone(),
                then: lower_canvas_items(then),
                els: els.as_deref().map(lower_canvas_items).unwrap_or_default(),
            }),
            _ => None,
        })
        .collect()
}

/// One tessellated path, and when it was last drawn (RFC-0037).
pub(super) struct CachedMesh {
    pub(super) mesh: std::sync::Arc<byard_core::frame::FillMesh>,
    /// The frame this mesh was last used on, so a path that has gone away
    /// stops being paid for.
    pub(super) last_used: u64,
}

/// How many frames an unused mesh is kept before it is dropped.
///
/// Long enough that a path alternating with a sibling (a `when` toggling two
/// charts) does not re-tessellate on every switch, short enough that a screen
/// nobody is on stops holding geometry. Frames rather than seconds because the
/// cost being bounded is memory per drawn frame, not wall-clock.
pub(super) const MESH_CACHE_FRAMES: u64 = 120;

/// One evaluated path command, in absolute logical pixels (RFC-0037).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PathCommand {
    /// Start a new subpath at this point.
    Move([f32; 2]),
    /// A straight segment to this point.
    Line([f32; 2]),
    /// A quadratic Bézier: control point, end point.
    Quad([f32; 2], [f32; 2]),
    /// A cubic Bézier: two control points, end point.
    Cubic([f32; 2], [f32; 2], [f32; 2]),
    /// Close the current subpath back to its start.
    Close,
}

impl PathCommand {
    /// The command as it is written in byld.
    const fn name(&self) -> &'static str {
        match self {
            Self::Move(_) => "move",
            Self::Line(_) => "line",
            Self::Quad(..) => "quad",
            Self::Cubic(..) => "cubic",
            Self::Close => "close",
        }
    }
}

/// The fingerprint a mesh is cached under (RFC-0037, RFC-0032's rule).
///
/// `to_bits` rather than the float itself, because hashing an `f32` directly
/// makes `NaN` permanently dirty and `-0.0`/`0.0` permanently clean: two
/// coordinates that are the same number would fingerprint differently, and a
/// coordinate that is no number at all would fingerprint differently every
/// frame. The tolerance and the fill rule are in the key too, since both
/// change the triangles the same commands produce.
/// The outline of a rounded rectangle, mapped through `t` (RFC-0011 ×
/// RFC-0037): what a `Clip` looks like once an ancestor has rotated it.
///
/// A rotated rectangle is not a rectangle, so the scissor and the clip
/// table's axis-aligned entry cannot express it; a path mask can. Each corner
/// is the standard four-point cubic approximation of a quarter circle
/// (`k = 0.5523`), which is within a fraction of a pixel of the SDF the
/// unrotated clip uses at any radius a UI draws, and the mapping is applied
/// to the control points, which is exact for an affine transform.
pub(super) fn transformed_rrect_outline(
    rect: crate::interp::intrinsics::Rect,
    radii: [f32; 4],
    t: &byard_core::frame::Transform,
) -> Vec<PathCommand> {
    const K: f32 = 0.552_284_8;
    let (x0, y0, x1, y1) = (rect.x, rect.y, rect.x + rect.w, rect.y + rect.h);
    let cap = (rect.w.min(rect.h) * 0.5).max(0.0);
    let [tl, tr, br, bl] = radii.map(|r| r.clamp(0.0, cap));
    let p = |x: f32, y: f32| t.apply_point([x, y]);
    let mut out = vec![PathCommand::Move(p(x0 + tl, y0))];
    out.push(PathCommand::Line(p(x1 - tr, y0)));
    if tr > 0.0 {
        out.push(PathCommand::Cubic(
            p(x1 - tr + tr * K, y0),
            p(x1, y0 + tr - tr * K),
            p(x1, y0 + tr),
        ));
    }
    out.push(PathCommand::Line(p(x1, y1 - br)));
    if br > 0.0 {
        out.push(PathCommand::Cubic(
            p(x1, y1 - br + br * K),
            p(x1 - br + br * K, y1),
            p(x1 - br, y1),
        ));
    }
    out.push(PathCommand::Line(p(x0 + bl, y1)));
    if bl > 0.0 {
        out.push(PathCommand::Cubic(
            p(x0 + bl - bl * K, y1),
            p(x0, y1 - bl + bl * K),
            p(x0, y1 - bl),
        ));
    }
    out.push(PathCommand::Line(p(x0, y0 + tl)));
    if tl > 0.0 {
        out.push(PathCommand::Cubic(
            p(x0, y0 + tl - tl * K),
            p(x0 + tl - tl * K, y0),
            p(x0 + tl, y0),
        ));
    }
    out.push(PathCommand::Close);
    out
}

pub(super) fn path_fingerprint(commands: &[PathCommand], tolerance: f32, even_odd: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tolerance.to_bits().hash(&mut hasher);
    even_odd.hash(&mut hasher);
    for command in commands {
        match command {
            PathCommand::Move(p) => (0u8, p[0].to_bits(), p[1].to_bits()).hash(&mut hasher),
            PathCommand::Line(p) => (1u8, p[0].to_bits(), p[1].to_bits()).hash(&mut hasher),
            PathCommand::Quad(c, p) => {
                (
                    2u8,
                    c[0].to_bits(),
                    c[1].to_bits(),
                    p[0].to_bits(),
                    p[1].to_bits(),
                )
                    .hash(&mut hasher);
            }
            PathCommand::Cubic(a, b, p) => {
                (
                    3u8,
                    a[0].to_bits(),
                    a[1].to_bits(),
                    b[0].to_bits(),
                    b[1].to_bits(),
                    p[0].to_bits(),
                    p[1].to_bits(),
                )
                    .hash(&mut hasher);
            }
            PathCommand::Close => 4u8.hash(&mut hasher),
        }
    }
    hasher.finish()
}

/// Turns a tessellated position into the vertex the pipeline reads: the
/// position it already had, plus where it sits inside the path's box.
///
/// The `uv` is what a gradient is measured in, so it is computed here, once
/// per vertex, rather than in the shader from bounds it would have to be told
/// separately (RFC-0037).
struct WithUv {
    bounds: [f32; 4],
}

impl lyon_tessellation::FillVertexConstructor<byard_core::encoder::canvas_fill::FillVertex>
    for WithUv
{
    fn new_vertex(
        &mut self,
        vertex: lyon_tessellation::FillVertex<'_>,
    ) -> byard_core::encoder::canvas_fill::FillVertex {
        let p = vertex.position();
        byard_core::encoder::canvas_fill::FillVertex {
            pos: [p.x, p.y],
            uv: [
                (p.x - self.bounds[0]) / self.bounds[2],
                (p.y - self.bounds[1]) / self.bounds[3],
            ],
        }
    }
}

/// Tessellates a path into the triangles the `CanvasFill` pipeline draws
/// (RFC-0037).
///
/// An unclosed fill is closed back to its subpath's start rather than
/// refused: an open filled path is almost always an oversight, and closing it
/// produces the obviously-intended shape (the RFC's resolved question). `lyon`
/// closes it for us, which is why there is no special case here to get wrong.
pub(super) fn tessellate_path(
    commands: &[PathCommand],
    tolerance: f32,
    even_odd: bool,
) -> byard_core::frame::FillMesh {
    use lyon_tessellation::geom::point;
    use lyon_tessellation::path::Path;
    use lyon_tessellation::{
        BuffersBuilder, FillOptions, FillRule, FillTessellator, VertexBuffers,
    };

    let mut builder = Path::builder();
    let mut open = false;
    for command in commands {
        match command {
            PathCommand::Move(p) => {
                if open {
                    builder.end(true);
                }
                builder.begin(point(p[0], p[1]));
                open = true;
            }
            PathCommand::Line(p) if open => {
                builder.line_to(point(p[0], p[1]));
            }
            PathCommand::Quad(c, p) if open => {
                builder.quadratic_bezier_to(point(c[0], c[1]), point(p[0], p[1]));
            }
            PathCommand::Cubic(a, b, p) if open => {
                builder.cubic_bezier_to(point(a[0], a[1]), point(b[0], b[1]), point(p[0], p[1]));
            }
            PathCommand::Close if open => {
                builder.end(true);
                open = false;
            }
            // A segment before any `move` is refused at check time
            // (`PathMustStartWithMove`); reaching here means the body was
            // built some other way, and dropping the segment is the only
            // answer that cannot invent geometry.
            _ => {}
        }
    }
    if open {
        builder.end(true);
    }
    let path = builder.build();

    // The bounds `uv` is normalised against: the path's own box, so a
    // gradient across a fill runs from its top to its bottom rather than from
    // the canvas' (RFC-0035 reused verbatim).
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for command in commands {
        let points: &[[f32; 2]] = match command {
            PathCommand::Move(p) | PathCommand::Line(p) => std::slice::from_ref(p),
            PathCommand::Quad(c, p) => &[*c, *p],
            PathCommand::Cubic(a, b, p) => &[*a, *b, *p],
            PathCommand::Close => &[],
        };
        for p in points {
            min_x = min_x.min(p[0]);
            min_y = min_y.min(p[1]);
            max_x = max_x.max(p[0]);
            max_y = max_y.max(p[1]);
        }
    }
    let bounds = [
        min_x,
        min_y,
        (max_x - min_x).max(1e-3),
        (max_y - min_y).max(1e-3),
    ];

    let mut buffers: VertexBuffers<byard_core::encoder::canvas_fill::FillVertex, u32> =
        VertexBuffers::new();
    let options = FillOptions::tolerance(tolerance).with_fill_rule(if even_odd {
        FillRule::EvenOdd
    } else {
        FillRule::NonZero
    });
    let ok = FillTessellator::new()
        .tessellate_path(
            &path,
            &options,
            &mut BuffersBuilder::new(&mut buffers, WithUv { bounds }),
        )
        .is_ok();
    if !ok {
        // A tessellation failure is a path the tessellator could not make
        // sense of. An empty mesh draws nothing, which is what a shape nobody
        // could triangulate looks like; the alternative is garbage triangles.
        return byard_core::frame::FillMesh::default();
    }
    byard_core::frame::FillMesh {
        vertices: buffers.vertices,
        indices: buffers.indices,
        bounds,
    }
}

/// Mixes two linear RGBA colours in OKLab at factor `t`, alpha linearly, so
/// a path morph blends its fill the way every other colour animation does
/// (RFC-0031 §S10).
fn mix_rgba_oklab(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let hex = |c: [f32; 4]| -> i64 {
        let byte = |v: f32| i64::from((linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8);
        (byte(c[0]) << 16) | (byte(c[1]) << 8) | byte(c[2])
    };
    let (la, lb) = (oklab_from_hex(hex(a)), oklab_from_hex(hex(b)));
    let rgb = super::intrinsics::color_to_rgba(
        hex_from_oklab(std::array::from_fn(|i| la[i] + (lb[i] - la[i]) * t)),
        false,
    );
    [rgb[0], rgb[1], rgb[2], a[3] + (b[3] - a[3]) * t]
}

/// The two members a morph phase falls between and the blend factor, for a
/// sequence of `count` (RFC-0031 §S10): the phase wraps, so negative and
/// past-the-end values index the sequence cyclically.
fn morph_indices(phase: f32, count: usize) -> Option<(usize, usize, f32)> {
    if count == 0 || !phase.is_finite() {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = count as f32;
    let ph = phase.rem_euclid(n);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i0 = (ph.floor() as usize).min(count - 1);
    #[allow(clippy::cast_precision_loss)]
    let t = (ph - i0 as f32).clamp(0.0, 1.0);
    Some((i0, (i0 + 1) % count, t))
}

/// Two paths blended command by command at factor `t`; `Err` carries the
/// index of the first command that is not the same kind in both (or where
/// one path ends before the other).
#[allow(clippy::many_single_char_names)] // points of the two paths: p/q, c/d
fn lerp_path_commands(
    from: &[PathCommand],
    to: &[PathCommand],
    t: f32,
) -> Result<Vec<PathCommand>, usize> {
    let mix = |p: [f32; 2], q: [f32; 2]| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
    let mut out = Vec::with_capacity(from.len());
    for (i, pair) in from.iter().zip(to).enumerate() {
        out.push(match (*pair.0, *pair.1) {
            (PathCommand::Move(p), PathCommand::Move(q)) => PathCommand::Move(mix(p, q)),
            (PathCommand::Line(p), PathCommand::Line(q)) => PathCommand::Line(mix(p, q)),
            (PathCommand::Quad(c, p), PathCommand::Quad(d, q)) => {
                PathCommand::Quad(mix(c, d), mix(p, q))
            }
            (PathCommand::Cubic(c1, c2, p), PathCommand::Cubic(d1, d2, q)) => {
                PathCommand::Cubic(mix(c1, d1), mix(c2, d2), mix(p, q))
            }
            (PathCommand::Close, PathCommand::Close) => PathCommand::Close,
            _ => return Err(i),
        });
    }
    if from.len() == to.len() {
        Ok(out)
    } else {
        Err(from.len().min(to.len()))
    }
}

/// Why two paths of a morph could not be blended.
enum Mismatch {
    /// Strict: the index of the first command that differs.
    Command(usize),
    /// Resampled: the two subpath counts, previous path first.
    Subpaths((usize, usize)),
}

/// Spacing, in logical pixels, of the points a resampled morph places along
/// the longer of two outlines.
const RESAMPLE_SPACING: f32 = 2.0;

/// Fewest points per outline, so a small icon still morphs smoothly.
const RESAMPLE_MIN: usize = 24;

/// Most points per outline. Alignment is quadratic in this, so it is what
/// bounds a morph's per-frame cost; 192 points are finer than the tessellator
/// needs at any size a UI draws a morphing shape.
const RESAMPLE_MAX: usize = 192;

/// Segments a curve is flattened into before resampling. Only the flattened
/// arc length depends on it, and resampling spreads the points evenly after.
const CURVE_STEPS: usize = 16;

/// Two paths of any structure blended at factor `t` (RFC-0031 §S11,
/// `morph_mode: resample`).
///
/// Each subpath is flattened to a closed polyline and resampled to the same
/// number of points by arc length. The second is turned to the first's
/// winding, then its start is moved to the rotation that minimises the total
/// squared distance to the first: without that, a square morphing into a
/// square drawn from another corner would twist through its own centre.
/// `Err` carries the two subpath counts when they differ.
#[allow(clippy::many_single_char_names)] // outlines a/b, points p/q, count n, factor t
fn resample_path_commands(
    from: &[PathCommand],
    to: &[PathCommand],
    t: f32,
) -> Result<Vec<PathCommand>, (usize, usize)> {
    let (a, b) = (flatten_subpaths(from), flatten_subpaths(to));
    if a.len() != b.len() {
        return Err((a.len(), b.len()));
    }
    let mut out = Vec::new();
    for (sa, sb) in a.iter().zip(&b) {
        let longer = polyline_length(sa).max(polyline_length(sb));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = ((longer / RESAMPLE_SPACING).ceil() as usize).clamp(RESAMPLE_MIN, RESAMPLE_MAX);
        let ra = resample_closed(sa, n);
        let mut rb = resample_closed(sb, n);
        if signed_area(&ra) * signed_area(&rb) < 0.0 {
            rb.reverse();
        }
        let shift = best_rotation(&ra, &rb);
        let mix = |i: usize| {
            let (p, q) = (ra[i], rb[(i + shift) % n]);
            [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]
        };
        out.push(PathCommand::Move(mix(0)));
        out.extend((1..n).map(|i| PathCommand::Line(mix(i))));
        out.push(PathCommand::Close);
    }
    Ok(out)
}

/// A path's subpaths as polylines, curves flattened. Every subpath is
/// treated as closed, which a fill is anyway.
fn flatten_subpaths(commands: &[PathCommand]) -> Vec<Vec<[f32; 2]>> {
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut pen = [0.0, 0.0];
    #[allow(clippy::cast_precision_loss)]
    let steps = (1..=CURVE_STEPS).map(|k| k as f32 / CURVE_STEPS as f32);
    for cmd in commands {
        match *cmd {
            PathCommand::Move(p) => {
                out.push(vec![p]);
                pen = p;
            }
            PathCommand::Line(p) => {
                current(&mut out, pen).push(p);
                pen = p;
            }
            PathCommand::Quad(c, p) => {
                let sub = current(&mut out, pen);
                for s in steps.clone() {
                    let u = 1.0 - s;
                    sub.push(std::array::from_fn(|i| {
                        u * u * pen[i] + 2.0 * u * s * c[i] + s * s * p[i]
                    }));
                }
                pen = p;
            }
            PathCommand::Cubic(c1, c2, p) => {
                let sub = current(&mut out, pen);
                for s in steps.clone() {
                    let u = 1.0 - s;
                    sub.push(std::array::from_fn(|i| {
                        u * u * u * pen[i]
                            + 3.0 * u * u * s * c1[i]
                            + 3.0 * u * s * s * c2[i]
                            + s * s * s * p[i]
                    }));
                }
                pen = p;
            }
            PathCommand::Close => {
                if let Some(first) = out.last().and_then(|s| s.first()) {
                    pen = *first;
                }
            }
        }
    }
    out
}

/// The subpath being drawn, opening one at the pen when a path draws before
/// its first `move`.
fn current(subpaths: &mut Vec<Vec<[f32; 2]>>, pen: [f32; 2]) -> &mut Vec<[f32; 2]> {
    if subpaths.is_empty() {
        subpaths.push(vec![pen]);
    }
    let last = subpaths.len() - 1;
    &mut subpaths[last]
}

/// The perimeter of a closed polyline.
fn polyline_length(points: &[[f32; 2]]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (p, q) = (points[i], points[(i + 1) % n]);
            (q[0] - p[0]).hypot(q[1] - p[1])
        })
        .sum()
}

/// `n` points spaced evenly by arc length round a closed polyline, the first
/// on its first vertex. A degenerate outline (one point, or no length)
/// becomes `n` copies of that point, which morphs as a shape growing from it.
fn resample_closed(points: &[[f32; 2]], n: usize) -> Vec<[f32; 2]> {
    let total = polyline_length(points);
    let Some(&first) = points.first() else {
        return vec![[0.0, 0.0]; n];
    };
    if total <= f32::EPSILON {
        return vec![first; n];
    }
    #[allow(clippy::cast_precision_loss)]
    let step = total / n as f32;
    let mut out = Vec::with_capacity(n);
    let (mut seg, mut walked) = (0, 0.0);
    let len = points.len();
    for k in 0..n {
        #[allow(clippy::cast_precision_loss)]
        let target = k as f32 * step;
        loop {
            let (p, q) = (points[seg % len], points[(seg + 1) % len]);
            let seg_len = (q[0] - p[0]).hypot(q[1] - p[1]);
            if walked + seg_len >= target || seg >= len {
                let u = if seg_len > 0.0 {
                    ((target - walked) / seg_len).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                out.push([p[0] + (q[0] - p[0]) * u, p[1] + (q[1] - p[1]) * u]);
                break;
            }
            walked += seg_len;
            seg += 1;
        }
    }
    out
}

/// Twice the signed area of a closed polyline; its sign is the winding.
fn signed_area(points: &[[f32; 2]]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (p, q) = (points[i], points[(i + 1) % n]);
            p[0] * q[1] - q[0] * p[1]
        })
        .sum()
}

/// The shift of `b`'s start that brings it closest to `a`, point for point,
/// by total squared distance. Ties go to the smallest shift, so the choice is
/// the same on every run.
#[allow(clippy::many_single_char_names)] // outlines a/b, points p/q, count n
fn best_rotation(a: &[[f32; 2]], b: &[[f32; 2]]) -> usize {
    let n = a.len();
    let cost = |shift: usize| -> f32 {
        (0..n)
            .map(|i| {
                let (p, q) = (a[i], b[(i + shift) % n]);
                (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)
            })
            .sum()
    };
    (0..n)
        .map(|shift| (shift, cost(shift)))
        .min_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)))
        .map_or(0, |(shift, _)| shift)
}

impl Interpreter {
    // ── Canvas shape lowering (RFC-0020) ────────────────────────────────────

    /// The named argument `name` of a shape command, if present.
    fn shape_arg<'e>(el: &'e ElementNode, name: &str) -> Option<&'e Expr> {
        el.content
            .iter()
            .find(|a| a.name.as_ref().is_some_and(|n| n.as_str() == name))
            .map(|a| &a.value)
    }

    /// Evaluates a numeric shape parameter (reactive + `with`-animatable via
    /// `eval_pure`'s animation chokepoint, RFC-0010).
    fn shape_num(&mut self, el: &ElementNode, name: &str) -> Option<f32> {
        Self::shape_arg(el, name).map(|e| {
            let e = e.clone();
            self.eval_num(&e)
        })
    }

    /// Evaluates a shape color parameter. The alpha byte is auto-detected
    /// (the lexer's >6-digit tag, or magnitude for computed values),
    /// matching every alpha-aware colour consumer (RFC-0011).
    fn shape_color(&mut self, el: &ElementNode, name: &str) -> Option<[f32; 4]> {
        let e = Self::shape_arg(el, name)?.clone();
        let packed = self.eval_pure(&e).as_int()?;
        Some(super::intrinsics::color_rgba_auto(packed))
    }

    /// Evaluates a `(a, b)` shape parameter (the `dash` pattern).
    fn shape_vec2(&mut self, el: &ElementNode, name: &str) -> Option<[f32; 2]> {
        let e = Self::shape_arg(el, name)?.clone();
        match self.eval_pure(&e) {
            Value::Tuple(items) if items.len() == 2 => {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let f = |v: &Value| match v {
                    Value::Int(n) => *n as f32,
                    Value::Float(x) => *x as f32,
                    _ => 0.0,
                };
                Some([f(&items[0].1), f(&items[1].1)])
            }
            _ => None,
        }
    }

    /// Reads a bare-token shape parameter (`cap: round`), an enum token is a
    /// syntactic identifier, never an env lookup, mirroring how `align:` and
    /// `fit:` tokens read elsewhere.
    pub(super) fn shape_token(el: &ElementNode, name: &str) -> Option<String> {
        match Self::shape_arg(el, name)? {
            Expr::Ident(sym, _) | Expr::ClassRef(sym, _) => Some(sym.as_str().to_string()),
            _ => None,
        }
    }

    /// Evaluates a string shape parameter (`path`'s `d`).
    fn shape_string(&mut self, el: &ElementNode, name: &str) -> Option<String> {
        let e = Self::shape_arg(el, name)?.clone();
        match self.eval_pure(&e) {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Emits one shape command into the frame (RFC-0020). `canvas` is the
    /// canvas's resolved rect: shape coordinates are canvas-local and offset
    /// by its origin here. Tier 1 (`arc`/`circle`/`line`/`rect`, plus
    /// `bezier` flattened to line segments) goes to the `CanvasShape`
    /// pipeline; `path` rasterizes through `VectorMSDF` (Tier 2); `text`
    /// lowers to a `TextLine`.
    /// Emits a canvas body, expanding `for`/`when` against the current
    /// environment.
    ///
    /// Bindings are pushed and truncated per iteration rather than snapshotted,
    /// because the whole body runs inside the canvas's already-restored
    /// instance environment, the loop variable is the only thing that changes.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_canvas_items(
        &mut self,
        items: &[CanvasItem],
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        // `Some` while a grouped `Canvas` is collecting its members
        // (RFC-0031 §S4); `None` for the ordinary one-instance-per-shape path.
        group: Option<&mut ShapeGroupSink>,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        // Reborrowed per item rather than moved, so the same sink threads
        // through nested `for`/`when` bodies.
        let mut group = group;
        for item in items {
            match item {
                CanvasItem::Shape(el) => {
                    self.emit_canvas_shape(
                        el,
                        canvas,
                        opacity,
                        transform,
                        group.as_deref_mut(),
                        frame,
                    );
                }
                CanvasItem::For {
                    var,
                    index,
                    iter,
                    body,
                } => {
                    // Cloned because the body's expressions are evaluated with
                    // `&mut self`, and the list lives in the environment the
                    // evaluation may touch. A canvas's data is a handful of
                    // records, not an app-state list.
                    let Some(items) = self.eval_pure(iter).as_list().map(<[Value]>::to_vec) else {
                        continue;
                    };
                    let base = self.env.len();
                    for (i, value) in items.into_iter().enumerate() {
                        self.env.truncate(base);
                        if let Some(index) = index {
                            self.env.push(
                                index.clone(),
                                Value::Int(i64::try_from(i).unwrap_or(i64::MAX)),
                            );
                        }
                        self.env.push(var.clone(), value);
                        self.emit_canvas_items(
                            body,
                            canvas,
                            opacity,
                            transform,
                            group.as_deref_mut(),
                            frame,
                        );
                    }
                    self.env.truncate(base);
                }
                CanvasItem::When { cond, then, els } => {
                    let taken = if self.eval_pure(cond).as_bool().unwrap_or(false) {
                        then
                    } else {
                        els
                    };
                    self.emit_canvas_items(
                        taken,
                        canvas,
                        opacity,
                        transform,
                        group.as_deref_mut(),
                        frame,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn emit_canvas_shape(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        mut group: Option<&mut ShapeGroupSink>,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{
            CANVAS_CAP_BUTT, CANVAS_CAP_ROUND, CANVAS_CAP_SQUARE, CANVAS_SHAPE_ARC,
            CANVAS_SHAPE_CIRCLE, CANVAS_SHAPE_LINE, CANVAS_SHAPE_NGON, CANVAS_SHAPE_RECT,
            CanvasShape,
        };

        /// One resolved Tier-1 shape: into the group being collected, or
        /// straight onto the frame as its own instance.
        fn emit(
            group: &mut Option<&mut ShapeGroupSink>,
            frame: &mut byard_core::frame::RenderFrame,
            shape: CanvasShape,
        ) {
            match group {
                Some(sink) => sink.push(&shape),
                None => frame.push_canvas_shape(shape),
            }
        }

        let name = el.name.as_str();
        if name == "text" {
            self.emit_canvas_text(el, canvas, opacity, transform, frame);
            return;
        }
        if name == "path" {
            // In a `morph:` canvas a body path is a member of the sequence,
            // blended with its neighbour once the whole body has been read.
            if let Some(paths) = group
                .as_deref_mut()
                .and_then(|sink| sink.morph_paths.as_mut())
            {
                if !el.children.is_empty() {
                    if let Some(path) = self.eval_filled_path(el, canvas) {
                        paths.push(path);
                    }
                    return;
                }
            }
            self.emit_canvas_path(el, canvas, opacity, transform, frame);
            return;
        }

        // Shared paint parameters (RFC-0020 §"Stroke and fill"). A shape with
        // neither stroke nor fill paints nothing, skip it entirely.
        let stroke_color = self.shape_color(el, "stroke").unwrap_or([0.0; 4]);
        let fill_color = self.shape_color(el, "fill").unwrap_or([0.0; 4]);
        // RFC-0035 §"Canvas arc strokes": a ramp along the stroke, read by the
        // same parser a box gradient and a filled path use, so there is one
        // spelling of a gradient rather than three that agree today.
        let stroke_gradient = Self::shape_arg(el, "stroke_gradient")
            .cloned()
            .and_then(|expr| self.resolve_gradient_expr(&expr, 0.0));
        // A shape with neither stroke nor fill paints nothing; skip it
        // entirely. A stroke *gradient* counts as a stroke: writing one and
        // no `stroke:` is a reasonable thing to do, and dropping the shape
        // because the flat colour it never set is transparent would be the
        // silent failure this check exists to avoid, wearing the check's own
        // clothes.
        if stroke_color[3] <= 0.0 && fill_color[3] <= 0.0 && stroke_gradient.is_none() {
            return;
        }
        let stroke_width = self.shape_num(el, "stroke_width").unwrap_or(1.0);
        let cap = match Self::shape_token(el, "cap").as_deref() {
            Some("round") => CANVAS_CAP_ROUND,
            Some("square") => CANVAS_CAP_SQUARE,
            _ => CANVAS_CAP_BUTT,
        };
        let dash = self.shape_vec2(el, "dash").unwrap_or([0.0, 0.0]);
        let dash_offset = self.shape_num(el, "dash_offset").unwrap_or(0.0);
        let shape_opacity = opacity * self.shape_num(el, "opacity").unwrap_or(1.0);
        let (ox, oy) = (canvas.x, canvas.y);

        let base = CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [0.0; 8],
            stroke_color,
            fill_color,
            stroke_width,
            cap,
            dash,
            dash_offset,
            opacity: shape_opacity,
            transform,
            stroke_gradient,
            dirty: true,
            ..CanvasShape::default()
        };

        match name {
            "arc" | "circle" => {
                let cx = ox + self.shape_num(el, "cx").unwrap_or(0.0);
                let cy = oy + self.shape_num(el, "cy").unwrap_or(0.0);
                let r = self.shape_num(el, "r").unwrap_or(0.0);
                // Angles are authored in degrees (RFC-0020 examples:
                // `start: -90, sweep: 270`); the GPU wants radians. An
                // unswept `arc` defaults to a full circle, `circle` is the
                // explicit sugar for exactly that (RFC-0020 §"Shape commands").
                let start = self.shape_num(el, "start").unwrap_or(0.0);
                let sweep = if name == "circle" {
                    360.0
                } else {
                    self.shape_num(el, "sweep").unwrap_or(360.0)
                };
                let full = sweep.abs() >= 360.0;
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: if full {
                            CANVAS_SHAPE_CIRCLE
                        } else {
                            CANVAS_SHAPE_ARC
                        },
                        params: [
                            cx,
                            cy,
                            r,
                            start.to_radians(),
                            sweep.to_radians(),
                            0.0,
                            0.0,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            "line" => {
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: CANVAS_SHAPE_LINE,
                        params: [
                            ox + self.shape_num(el, "x1").unwrap_or(0.0),
                            oy + self.shape_num(el, "y1").unwrap_or(0.0),
                            ox + self.shape_num(el, "x2").unwrap_or(0.0),
                            oy + self.shape_num(el, "y2").unwrap_or(0.0),
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            "rect" => {
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: CANVAS_SHAPE_RECT,
                        params: [
                            ox + self.shape_num(el, "x").unwrap_or(0.0),
                            oy + self.shape_num(el, "y").unwrap_or(0.0),
                            self.shape_num(el, "w").unwrap_or(0.0),
                            self.shape_num(el, "h").unwrap_or(0.0),
                            self.shape_num(el, "radius").unwrap_or(0.0),
                            // RFC-0031 §S3: the `rect` kind's corner smoothing,
                            // clamped like every other consumer of the property.
                            self.shape_num(el, "smooth").unwrap_or(0.0).clamp(0.0, 1.0),
                            0.0,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            // RFC-0031 §"`ngon`": one parametric kind for the n-fold symmetric
            // half of the expressive vocabulary.
            "ngon" => {
                #[allow(clippy::cast_precision_loss)]
                let n = self.shape_num(el, "n").unwrap_or(3.0).round().max(3.0);
                let r = self.shape_num(el, "r").unwrap_or(0.0).max(0.0);
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: CANVAS_SHAPE_NGON,
                        params: [
                            ox + self.shape_num(el, "cx").unwrap_or(0.0),
                            oy + self.shape_num(el, "cy").unwrap_or(0.0),
                            r,
                            // A corner larger than the circumradius has no
                            // meaning; clamped at the one place it is read,
                            // like `radius` on a box.
                            self.shape_num(el, "corner").unwrap_or(0.0).clamp(0.0, r),
                            // `1.0` is the convex regular polygon; below it the
                            // notches pull in towards the centre.
                            self.shape_num(el, "inner").unwrap_or(1.0).clamp(0.0, 1.0),
                            // Already canonicalized to radians by the lexer.
                            self.shape_num(el, "rotate").unwrap_or(0.0),
                            n,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            "bezier" => {
                // Flattened CPU-side into round-capped line segments on the
                // same Tier-1 pipeline, cheaper and *fully animatable*,
                // unlike an MSDF re-rasterization (see the RFC-0020 notes in
                // the design record). Round caps hide the joints; the curve
                // has no fill.
                if let Some(c) = self.bezier_coords(el) {
                    let p = |t: f32| -> [f32; 2] {
                        let u = 1.0 - t;
                        let b0 = u * u * u;
                        let b1 = 3.0 * u * u * t;
                        let b2 = 3.0 * u * t * t;
                        let b3 = t * t * t;
                        [
                            ox + b0 * c[0] + b1 * c[2] + b2 * c[4] + b3 * c[6],
                            oy + b0 * c[1] + b1 * c[3] + b2 * c[5] + b3 * c[7],
                        ]
                    };
                    // Segment count scales with the control polygon's length:
                    // ~one segment per 6 logical px, clamped to [8, 48].
                    let poly_len = ((c[2] - c[0]).hypot(c[3] - c[1])
                        + (c[4] - c[2]).hypot(c[5] - c[3])
                        + (c[6] - c[4]).hypot(c[7] - c[5]))
                    .max(1.0);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let segments = ((poly_len / 6.0) as usize).clamp(8, 48);
                    let mut prev = p(0.0);
                    #[allow(clippy::cast_precision_loss)]
                    for i in 1..=segments {
                        let next = p(i as f32 / segments as f32);
                        emit(
                            &mut group,
                            frame,
                            CanvasShape {
                                kind: CANVAS_SHAPE_LINE,
                                params: [prev[0], prev[1], next[0], next[1], 0.0, 0.0, 0.0, 0.0],
                                cap: CANVAS_CAP_ROUND,
                                fill_color: [0.0; 4],
                                ..base.clone()
                            },
                        );
                        prev = next;
                    }
                }
            }
            _ => {}
        }
    }

    /// The 8 cubic-bezier coordinates, from either the terse positional form
    /// (`bezier(10, 90, 40, 10, …)`) or the named form (`x1:`, `cy2:`, …).
    fn bezier_coords(&mut self, el: &ElementNode) -> Option<[f32; 8]> {
        const NAMES: [&str; 8] = ["x1", "y1", "cx1", "cy1", "cx2", "cy2", "x2", "y2"];
        let positional: Vec<Expr> = el
            .content
            .iter()
            .filter(|a| a.name.is_none())
            .map(|a| a.value.clone())
            .collect();
        let mut out = [0.0f32; 8];
        if positional.len() == 8 {
            for (slot, expr) in out.iter_mut().zip(&positional) {
                *slot = self.eval_num(expr);
            }
            return Some(out);
        }
        for (slot, name) in out.iter_mut().zip(NAMES) {
            *slot = self.shape_num(el, name)?;
        }
        Some(out)
    }

    /// RFC-0020 §2 Tier 2: a `path(d: …)` command rasterized through the
    /// MSDF pipeline. The synthetic SVG's viewBox equals the canvas size, so
    /// `d` coordinates are canvas-local 1:1; the resulting glyph is drawn
    /// over the whole canvas rect and tinted by `fill`. Content-keyed, so a
    /// re-render of an unchanged path is a pure cache hit; only a genuinely
    /// new `d` (or canvas size) dispatches a generation.
    fn emit_canvas_path(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        // RFC-0037: a `path` with a body is Tier-2, tessellated and filled by
        // the `CanvasFill` pipeline. A `path(d: …)` with no body is Tier-1,
        // baked into the MSDF atlas below. Both are the same command, drawn by
        // whichever pipeline suits the shape: static art amortises a bake,
        // geometry that changes with the data does not.
        if !el.children.is_empty() {
            self.emit_filled_path(el, canvas, opacity, transform, frame);
            return;
        }
        let Some(fill) = self.shape_color(el, "fill") else {
            return; // no fill → nothing to rasterize (stroke is rejected upstream)
        };
        let Some(d) = self.shape_string(el, "d") else {
            return;
        };
        let shape_opacity = opacity * self.shape_num(el, "opacity").unwrap_or(1.0);
        let (w, h) = (canvas.w.max(1.0), canvas.h.max(1.0));

        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            d.hash(&mut hasher);
            w.to_bits().hash(&mut hasher);
            h.to_bits().hash(&mut hasher);
            format!("canvas-path:{:016x}", hasher.finish())
        };
        let glyph = self.vector_jit.lookup_or_dispatch_svg(&key, || {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}"><path d="{d}" fill="#000000"/></svg>"##
            )
            .into_bytes()
        });
        // Cache miss: skip this tick (the render thread never blocks on
        // generation, so the frame ships without stalling); the generated field
        // lands via the ordinary JIT drain.
        let Some(glyph) = glyph else { return };

        // A `VectorInstance` carries no transform: bake translate/scale into
        // the rect like `Image` does (rotation stays a box-primitive feature).
        let tl = transform.apply_point([canvas.x, canvas.y]);
        let rgb = [fill[0], fill[1], fill[2], fill[3] * shape_opacity];
        frame.push_vector(byard_core::frame::VectorInstance::new(
            byard_core::frame::Rect::new(
                tl[0],
                tl[1],
                canvas.w * transform.scale[0],
                canvas.h * transform.scale[1],
            ),
            glyph.uv_rect,
            rgb,
            glyph.px_range,
            glyph.layer,
        ));
    }

    /// A Tier-2 filled path (RFC-0037): evaluate the commands, tessellate them
    /// if they changed, and push the mesh.
    ///
    /// The evaluation happens every tick, because a command's coordinates are
    /// ordinary expressions and a chart's are ordinary data. The
    /// *tessellation* happens only when the numbers those expressions produced
    /// differ from last time, which is what keeps a live chart inside the
    /// frame budget: the expensive step is the one that is skipped
    /// (RFC-0032's dirty model: invalidation decides what work runs, never
    /// what the geometry is).
    fn emit_filled_path(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        if let Some(path) = self.eval_filled_path(el, canvas) {
            self.push_filled_path(&path, canvas, opacity, transform, frame);
        }
    }

    /// Evaluates a body path's commands and paint; `None` when there is
    /// nothing to draw.
    fn eval_filled_path(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
    ) -> Option<FilledPath> {
        let commands = self.eval_path_commands(&el.children, canvas);
        if commands.len() < 2 {
            // One point is not a shape. Silent rather than an error, because
            // an empty `for` over an empty series is a perfectly ordinary
            // frame of a chart that has no data yet.
            return None;
        }
        let fill = self.shape_color(el, "fill").unwrap_or([0.0; 4]);
        let gradient = Self::shape_arg(el, "gradient")
            .cloned()
            .and_then(|expr| self.resolve_gradient_expr(&expr, 0.0));
        if gradient.is_none() && fill[3] <= 0.0 {
            return None; // nothing to paint
        }
        Some(FilledPath {
            commands,
            fill,
            gradient,
            even_odd: Self::shape_token(el, "winding").as_deref() == Some("even_odd"),
            alpha: self.shape_num(el, "opacity").unwrap_or(1.0),
            span: el.span,
        })
    }

    /// Tessellates an evaluated path if its numbers changed, and pushes the
    /// mesh.
    fn push_filled_path(
        &mut self,
        path: &FilledPath,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        // The flattening tolerance is derived from the path's on-screen size:
        // a sparkline in a 40px card and a chart across a 4K window are the
        // same commands and want very different triangle counts (RFC-0037).
        let scale = transform.scale[0]
            .abs()
            .max(transform.scale[1].abs())
            .max(0.01);
        let extent = (canvas.w.max(canvas.h) * scale).max(1.0);
        let tolerance = (extent / 800.0).clamp(0.05, 0.5);

        let key = path_fingerprint(&path.commands, tolerance, path.even_odd);
        let mesh = if let Some(cached) = self.path_meshes.get_mut(&key) {
            cached.last_used = self.frame_seq;
            std::sync::Arc::clone(&cached.mesh)
        } else {
            let mesh =
                std::sync::Arc::new(tessellate_path(&path.commands, tolerance, path.even_odd));
            self.tessellations += 1;
            self.path_meshes.insert(
                key,
                CachedMesh {
                    mesh: std::sync::Arc::clone(&mesh),
                    last_used: self.frame_seq,
                },
            );
            mesh
        };
        if mesh.indices.is_empty() {
            return;
        }
        frame.push_fill(byard_core::frame::CanvasFill {
            mesh,
            color: path.fill,
            gradient: path.gradient,
            transform,
            opacity: opacity * path.alpha,
            dirty: true,
        });
    }

    /// Draws a `morph:` canvas's body paths (RFC-0031 §S11): the two members
    /// the phase falls between, blended command by command, as one filled
    /// path.
    ///
    /// The phase indexes the sequence exactly as §S10's does for distance
    /// fields, wrapping, so a scalar sweeping `0..N` returns to the first path.
    /// The blend is geometry, so it is tessellated like any path whose numbers
    /// changed: once per frame while the phase moves, and not at all once it
    /// settles, because a settled phase produces the same numbers and the
    /// mesh cache answers.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_morph_paths(
        &mut self,
        paths: &[FilledPath],
        phase: f32,
        resample: bool,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let Some((i0, i1, t)) = morph_indices(phase, paths.len()) else {
            return;
        };
        let (a, b) = (&paths[i0], &paths[i1]);
        if i0 == i1 || t <= 0.0 {
            self.push_filled_path(a, canvas, opacity, transform, frame);
            return;
        }
        let blended = if resample {
            resample_path_commands(&a.commands, &b.commands, t).map_err(Mismatch::Subpaths)
        } else {
            lerp_path_commands(&a.commands, &b.commands, t).map_err(Mismatch::Command)
        };
        match blended {
            Ok(commands) => {
                let blended = FilledPath {
                    commands,
                    fill: mix_rgba_oklab(a.fill, b.fill, t),
                    gradient: if t < 0.5 { a.gradient } else { b.gradient },
                    even_odd: a.even_odd,
                    alpha: a.alpha + (b.alpha - a.alpha) * t,
                    span: a.span,
                };
                self.push_filled_path(&blended, canvas, opacity, transform, frame);
            }
            Err(mismatch) => {
                // The check pass refuses this for a body of literal paths; a
                // `when` or a `for` can still bring two unlike paths together,
                // and only here is it known which two. Reported once, and the
                // nearer path drawn whole, so the error is what the author
                // sees rather than a shape that stops moving.
                let reported = self.errors.iter().any(|e| match e {
                    CompileError::MorphPathMismatch { span, .. }
                    | CompileError::MorphSubpathMismatch { span, .. } => *span == b.span,
                    _ => false,
                });
                if !reported {
                    let name = |c: Option<&PathCommand>| {
                        c.map_or_else(
                            || "the end of the path".to_string(),
                            |c| c.name().to_string(),
                        )
                    };
                    self.errors.push(match mismatch {
                        Mismatch::Command(index) => CompileError::MorphPathMismatch {
                            span: b.span,
                            member: i1,
                            index,
                            expected: name(a.commands.get(index)),
                            found: name(b.commands.get(index)),
                        },
                        Mismatch::Subpaths((expected, found)) => {
                            CompileError::MorphSubpathMismatch {
                                span: b.span,
                                member: i1,
                                expected,
                                found,
                            }
                        }
                    });
                }
                let nearer = if t < 0.5 { a } else { b };
                self.push_filled_path(nearer, canvas, opacity, transform, frame);
            }
        }
    }

    /// Evaluates a path body into absolute points (RFC-0037).
    ///
    /// Absolute, and offset by the canvas' own origin, because everything
    /// downstream of here works in the frame's logical-pixel space and a mesh
    /// that remembered it was relative would have to be re-tessellated every
    /// time its canvas moved.
    pub(super) fn eval_path_commands(
        &mut self,
        members: &[Member],
        canvas: crate::interp::intrinsics::Rect,
    ) -> Vec<PathCommand> {
        let mut out = Vec::new();
        self.push_path_commands(members, canvas, &mut out);
        out
    }

    /// The commands `members` write, in order, into `out`. A `for` writes its
    /// body once per item, with the loop variable (and index) bound for it,
    /// and a `when` writes the branch it takes: a path drawn from data, a
    /// chart's curve, is a `for` over the samples.
    fn push_path_commands(
        &mut self,
        members: &[Member],
        canvas: crate::interp::intrinsics::Rect,
        out: &mut Vec<PathCommand>,
    ) {
        for member in members {
            let cmd = match member {
                Member::Element(cmd) => cmd,
                Member::For {
                    var,
                    index,
                    iter,
                    body,
                    ..
                } => {
                    // Cloned for the reason a canvas's own `for` clones: the
                    // body is evaluated with `&mut self`.
                    let Some(items) = self.eval_pure(iter).as_list().map(<[Value]>::to_vec) else {
                        continue;
                    };
                    let base = self.env.len();
                    for (i, value) in items.into_iter().enumerate() {
                        self.env.truncate(base);
                        if let Some(index) = index {
                            self.env.push(
                                index.clone(),
                                Value::Int(i64::try_from(i).unwrap_or(i64::MAX)),
                            );
                        }
                        self.env.push(var.clone(), value);
                        self.push_path_commands(body, canvas, out);
                    }
                    self.env.truncate(base);
                    continue;
                }
                Member::When {
                    cond, then, els, ..
                } => {
                    if self.eval_pure(cond).as_bool().unwrap_or(false) {
                        self.push_path_commands(then, canvas, out);
                    } else if let Some(els) = els {
                        self.push_path_commands(els, canvas, out);
                    }
                    continue;
                }
                _ => continue,
            };
            let coords: Vec<f32> = {
                let params = super::intrinsics::path_command_params(cmd.name.as_str());
                let mut values = Vec::with_capacity(params.len());
                for (i, (pname, _)) in params.iter().enumerate() {
                    let expr = cmd
                        .content
                        .iter()
                        .find(|a| a.name.as_ref().is_some_and(|n| n.as_str() == *pname))
                        .or_else(|| cmd.content.iter().filter(|a| a.name.is_none()).nth(i))
                        .map(|a| a.value.clone());
                    values.push(expr.map_or(0.0, |e| self.eval_num(&e)));
                }
                values
            };
            let at = |i: usize| [canvas.x + coords[i], canvas.y + coords[i + 1]];
            match cmd.name.as_str() {
                "move" => out.push(PathCommand::Move(at(0))),
                "line" => out.push(PathCommand::Line(at(0))),
                "quad" => out.push(PathCommand::Quad(at(0), at(2))),
                "cubic" => out.push(PathCommand::Cubic(at(0), at(2), at(4))),
                "close" => out.push(PathCommand::Close),
                _ => {}
            }
        }
    }

    /// How many paths this interpreter has tessellated, ever (RFC-0037). A
    /// test reads it to prove the cached path is the one production takes.
    ///
    /// The number the caching claim is made of: a chart whose data did not
    /// change must not move it. Exposed rather than inferred, because "the
    /// frame was fast" is not evidence that the expensive step was skipped.
    #[must_use]
    pub const fn tessellations(&self) -> u64 {
        self.tessellations
    }

    /// A canvas `text(…)` command: a `TextLine` anchored at `(x, y)` with
    /// optional `align` (start/center/end around `x`), `y` is the vertical
    /// center of the run, matching the RFC's centred-label example.
    fn emit_canvas_text(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let Some(content) = el.content.iter().find(|a| a.name.is_none()) else {
            return;
        };
        let expr = content.value.clone();
        let text = match self.eval_pure(&expr) {
            Value::Str(s) => s,
            other => format!("{other:?}"),
        };
        if text.is_empty() {
            return;
        }
        let size = self.shape_num(el, "size").unwrap_or(self.theme.font_size);
        let color = self
            .shape_color(el, "color")
            // Theme tokens are `#RRGGBB` only (the manifest rejects anything
            // else), so the fallback is opaque by construction.
            .unwrap_or_else(|| super::intrinsics::color_to_rgba(self.theme.on_surface(), false));
        let x = canvas.x + self.shape_num(el, "x").unwrap_or(0.0);
        let y = canvas.y + self.shape_num(el, "y").unwrap_or(0.0);
        let measured = self.measure_text(&text, size).0;
        let tx = match Self::shape_token(el, "align").as_deref() {
            Some("center") => x - measured / 2.0,
            Some("end") => x - measured,
            _ => x,
        };
        // Anchor `y` at the run's vertical center (≈0.6em above the top of
        // the em box reads optically centred for Latin text).
        let ty = y - size * 0.6;
        let anchor = transform.apply_point([tx, ty]);
        frame.push_text(byard_core::TextLine {
            x: anchor[0],
            y: anchor[1],
            text,
            font_size: size * transform.uniform_scale(),
            weight: 400,
            family: None,
            color: dim_alpha(color, opacity),
            dirty: true,
        });
    }

    /// The combine mode a `Canvas` declares, and its parameter (RFC-0031 §S4).
    ///
    /// `fuse: <px>` is the smoothing radius, the distance over which two
    /// surfaces bridge into one. `morph: <scalar>` is the sequence mode: the
    /// canvas's shapes become an ordered set and the scalar indexes it. They
    /// are mutually exclusive (§Q4, diagnosed at check time); `fuse` wins here
    /// so a source that slipped past the check still renders one thing rather
    /// than something mode-dependent.
    ///
    /// Both read through `eval_float_prop`, which puts them on the RFC-0010
    /// animation chokepoint, the whole design. The Material 3 loader is seven
    /// shapes and *one* animated scalar; an animating fusion is new
    /// per-instance data and never a re-tessellation.
    pub(super) fn resolve_group_mode(&mut self, attrs: &[Attr]) -> Option<(u32, f32)> {
        #[allow(clippy::cast_possible_truncation)]
        if let Some(k) = self.eval_float_prop(attrs, "fuse") {
            return Some((byard_core::frame::GROUP_FUSE, (k as f32).max(0.0)));
        }
        #[allow(clippy::cast_possible_truncation)]
        self.eval_float_prop(attrs, "morph")
            .map(|phase| (byard_core::frame::GROUP_MORPH, phase as f32))
    }

    /// Pushes a collected shape group's head (RFC-0031 §S4).
    ///
    /// The head's `params` are the union of its members' bounds expressed as a
    /// rect, because `CanvasShape::bounds` is what sizes the instance quad and
    /// §S4 makes a head's quad the union rather than its own geometry. Its
    /// stroke, cap, dash and opacity come from the first member: a group has
    /// one outline (§S8) and there is no group-level stroke syntax, so the
    /// first shape's paint is the group's.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_shape_group(
        &mut self,
        mode: u32,
        param: f32,
        sink: ShapeGroupSink,
        opacity: f32,
        transform: byard_core::frame::Transform,
        span: Span,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{CANVAS_SHAPE_RECT, CanvasShape, MAX_GROUP_MEMBERS};
        let (Some(bounds), Some(paint)) = (sink.bounds, sink.paint) else {
            return; // an empty group draws nothing
        };
        // §S4: fusion bulges *outward* by up to the smoothing radius, so the
        // union quad is inflated by `k`, an under-inflated quad clips exactly
        // the bridge the feature exists to draw. Morphing never leaves its
        // members' union, so it pays nothing for this.
        let inflate = if mode == byard_core::frame::GROUP_FUSE {
            param.max(0.0)
        } else {
            0.0
        };
        let bounds = byard_core::frame::Rect::new(
            bounds.x - inflate,
            bounds.y - inflate,
            bounds.width + inflate * 2.0,
            bounds.height + inflate * 2.0,
        );
        // The written-shape count is diagnosed with a source position by
        // `validate_canvas`. This is the other half: a `for` inside the body
        // can generate members from data, and how many is only knowable here.
        // Deduped, because the render walk runs every tick and a diagnostic
        // that repeats sixty times a second is noise rather than information.
        if sink.overflowed > MAX_GROUP_MEMBERS
            && !self
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::TooManyGroupMembers { .. }))
        {
            self.errors.push(CompileError::TooManyGroupMembers {
                span,
                max: MAX_GROUP_MEMBERS,
                found: sink.overflowed,
            });
        }
        frame.push_shape_group(
            CanvasShape {
                // The head's own kind is never evaluated, the shader branches
                // on the combine mode first, but `bounds()` is, so a rect
                // carrying the union is exactly the quad §S4 asks for.
                kind: CANVAS_SHAPE_RECT,
                params: [
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ],
                stroke_color: paint.stroke_color,
                fill_color: paint.fill_color,
                stroke_width: paint.stroke_width,
                cap: paint.cap,
                dash: paint.dash,
                dash_offset: paint.dash_offset,
                opacity,
                transform,
                group_mode: mode,
                group_param: param,
                dirty: true,
                ..CanvasShape::default()
            },
            &sink.members,
        );
    }

    /// The element's corner smoothing (RFC-0031 §S1), clamped to `0..=1`.
    ///
    /// It resolves through `eval_float_prop`, so it passes the RFC-0010
    /// animation chokepoint like every other paint scalar: `smooth: 0.6 with
    /// anim.spring()` interpolates with no plumbing of its own, and, being
    /// paint-class, never marks the layout tree.
    ///
    /// Absent means `0.0`, which the shaders short-circuit to the L² field they
    /// evaluated before this property existed.
    #[allow(clippy::cast_possible_truncation)]
    pub(super) fn resolve_smooth(&mut self, attrs: &[Attr]) -> f32 {
        self.eval_float_prop(attrs, "smooth")
            .map_or(0.0, |v| (v as f32).clamp(0.0, 1.0))
    }
}
