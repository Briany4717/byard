//! `CanvasFill`, the filled-path pipeline (RFC-0037), registered through the
//! extension ABI rather than wired into the encoder (RFC-0039).
//!
//! # Why a mesh and not a distance field
//!
//! The vector pipeline (`vector_msdf`) is built for shapes that are baked once
//! and reused: an icon, a glyph, a logo. Its whole value is amortising a bake.
//! A chart's area fill is the opposite kind of shape, its geometry changes as
//! the data does, so re-baking an atlas per frame would pay the bake over and
//! over and still lose sharpness at large sizes (the note this RFC closes).
//!
//! So a filled path is a triangle mesh: tessellated on the logic thread, where
//! the rest of frame assembly happens, and cached by a fingerprint of the path
//! commands so an unchanged chart never tessellates twice.
//!
//! # Why it registers instead of being wired in
//!
//! Because the extension ABI has to carry a real pipeline before packages
//! depend on it, and this is the first one that is not a refactor of something
//! already here. Everything `CanvasFill` needs from the encoder, it gets the
//! way a package's pipeline would, which is how a gap in the ABI shows up as a
//! problem here rather than in somebody's chart library.

use crate::ByardError;
use crate::frame::{CanvasFill, Transform};

/// One tessellated vertex of a filled path.
///
/// `uv` is the path's normalised bounding box, `(0,0)` at its top-left and
/// `(1,1)` at its bottom-right, which is what drives a gradient across the
/// fill: a vertical fade under a curve is `uv.y`, exactly as a box gradient's
/// is (RFC-0035, shared verbatim).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FillVertex {
    /// Position in the canvas' logical-pixel space.
    pub pos: [f32; 2],
    /// Normalised position within the path's bounds.
    pub uv: [f32; 2],
}

/// The per-path record: what to paint the mesh with, and where it sits.
///
/// One instance per path, so a path with ten thousand triangles is one
/// instance and one draw, and the gradient block is read once per path rather
/// than carried on every vertex.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FillInstance {
    /// Solid fill colour, linear RGBA. Ignored when a gradient is present.
    pub color: [f32; 4],
    /// Gradient start colour (RFC-0035), all-zero without one.
    pub grad_from: [f32; 4],
    /// Gradient mid colour.
    pub grad_mid: [f32; 4],
    /// Gradient end colour.
    pub grad_to: [f32; 4],
    /// The four gradient control floats, meaning per kind
    /// ([`Gradient::axis`](crate::frame::Gradient::axis)).
    pub grad_axis: [f32; 4],
    /// `(opacity, depth, 0, 0)`.
    pub misc: [f32; 4],
    /// Paint-time transform translate (RFC-0011).
    pub t_translate: [f32; 2],
    /// Paint-time transform per-axis scale.
    pub t_scale: [f32; 2],
    /// Paint-time transform rotation, radians.
    pub t_rotate: f32,
    /// Paint-time transform pivot.
    pub t_origin: [f32; 2],
    /// Which shape the gradient paints, or
    /// [`GRADIENT_NONE`](crate::frame::GRADIENT_NONE) for a solid fill.
    pub grad_kind: u32,
}

impl FillInstance {
    /// The record for one path, with its draw-order depth stamped in.
    #[must_use]
    pub fn new(fill: &CanvasFill, depth: f32) -> Self {
        let t: Transform = fill.transform;
        Self {
            color: fill.color,
            grad_from: fill.gradient.map_or([0.0; 4], |g| g.from),
            grad_mid: fill.gradient.map_or([0.0; 4], |g| g.mid),
            grad_to: fill.gradient.map_or([0.0; 4], |g| g.to),
            grad_axis: fill.gradient.map_or([0.0; 4], |g| g.axis()),
            misc: [fill.opacity, depth, 0.0, 0.0],
            t_translate: t.translate,
            t_scale: t.scale,
            t_rotate: t.rotate,
            t_origin: t.origin,
            grad_kind: fill
                .gradient
                .map_or(crate::frame::GRADIENT_NONE, |g| g.kind as u32),
        }
    }

    /// Vertex layout of the mesh itself (locations 0..=1, per vertex).
    #[must_use]
    pub fn mesh_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![
            0 => Float32x2, // pos
            1 => Float32x2, // uv
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<FillVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: ATTRS,
        }
    }

    /// Vertex layout of the per-path record (locations 2..=10, per instance).
    ///
    /// Eleven attributes in total with the mesh's two, inside the sixteen every
    /// adapter guarantees, which registration checks rather than trusts.
    #[must_use]
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![
            2 => Float32x4, // color
            3 => Float32x4, // gradient from
            4 => Float32x4, // gradient mid
            5 => Float32x4, // gradient to
            6 => Float32x4, // gradient axis
            7 => Float32x4, // misc (opacity, depth, …)
            8 => Float32x4, // transform.translate ++ transform.scale
            9 => Float32x3, // transform.rotate ++ transform.origin
            10 => Uint32,   // gradient kind
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<FillInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: ATTRS,
        }
    }
}

/// The shared gradient interpolation, textually included by both this
/// pipeline's shader and `decorated_box`'s (RFC-0037 resolved question: share
/// the descriptor *and* the fragment block, so a path gradient and a box
/// gradient cannot drift).
const GRADIENT_WGSL: &str = include_str!("gradient.wgsl");

/// Compiles the shader and assembles the `CanvasFill` pipeline.
///
/// # Errors
///
/// [`ByardError::PipelineCompilation`] if the shader or pipeline fails GPU-side
/// validation, never a panic, never a software fallback.
pub async fn build_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    depth_stencil: wgpu::DepthStencilState,
) -> Result<wgpu::RenderPipeline, ByardError> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ByardCore - CanvasFill Pipeline Layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });

    let source = format!(
        "{}\n{GRADIENT_WGSL}\n{}",
        include_str!("clip.wgsl"),
        include_str!("canvas_fill.wgsl")
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ByardCore - CanvasFill WGSL Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(source)),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ByardCore - CanvasFill Render Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[FillInstance::mesh_layout(), FillInstance::layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            // A tessellator emits both windings depending on the path's
            // direction, and a fill has no back face to cull: culling here
            // would silently drop half of a perfectly good chart.
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(depth_stencil),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    if let Some(error) = scope.pop().await {
        return Err(crate::encoder::pipeline::build_error(
            "CanvasFill",
            &error.to_string(),
        ));
    }

    Ok(pipeline)
}

/// One filled path, staged into the arena and ready to draw.
#[derive(Clone, Copy, Default)]
pub struct StagedFill {
    /// The mesh's vertices.
    pub vertices: super::instance_arena::Region,
    /// The mesh's indices (`u32`).
    pub indices: super::instance_arena::Region,
    /// The per-path record.
    pub record: super::instance_arena::Region,
    /// How many indices to draw.
    pub index_count: u32,
}

/// Stages this segment's filled paths into the arena (RFC-0033 §G1).
///
/// Three regions per path rather than one pool for all of them, because a
/// mesh is variable-length: two paths' vertices cannot share an instance
/// stride, and an index buffer is bound as a range rather than indexed into.
pub fn stage(
    arena: &mut super::instance_arena::InstanceArena,
    out: &mut Vec<StagedFill>,
    fills: &[CanvasFill],
    depths: &[f32],
) {
    for (i, fill) in fills.iter().enumerate() {
        let mesh = &fill.mesh;
        if mesh.indices.is_empty() {
            continue;
        }
        let depth = depths
            .get(i)
            .copied()
            .unwrap_or(crate::frame::DRAW_DEPTH_CLEAR);
        out.push(StagedFill {
            vertices: arena.push_vertex(&mesh.vertices),
            indices: arena.push_index(&mesh.indices),
            record: arena.push_vertex(&[FillInstance::new(fill, depth)]),
            index_count: u32::try_from(mesh.indices.len()).unwrap_or(u32::MAX),
        });
    }
}

/// Draws this segment's staged fills.
pub fn draw(
    render_pass: &mut wgpu::RenderPass<'_>,
    arena: &super::instance_arena::InstanceArena,
    staged: &[StagedFill],
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) {
    if staged.is_empty() {
        return;
    }
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, bind_group, &[super::clip_offset(None)]);
    for fill in staged {
        render_pass.set_vertex_buffer(0, arena.slice(fill.vertices));
        render_pass.set_vertex_buffer(1, arena.slice(fill.record));
        render_pass.set_index_buffer(arena.slice(fill.indices), wgpu::IndexFormat::Uint32);
        render_pass.draw_indexed(0..fill.index_count, 0, 0..1);
    }
}

/// The registered `CanvasFill` pipeline (RFC-0039).
pub struct CanvasFillPipeline {
    pipeline: wgpu::RenderPipeline,
}

impl CanvasFillPipeline {
    /// Wraps a built pipeline for registration.
    #[must_use]
    pub const fn new(pipeline: wgpu::RenderPipeline) -> Self {
        Self { pipeline }
    }
}

impl super::pipeline::RenderPipeline for CanvasFillPipeline {
    const NAME: &'static str = "canvas_fill";
    type Instance = FillInstance;

    fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
        FillInstance::layout()
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, cx: &super::pipeline::SegmentDraw<'_>) {
        draw(
            pass,
            cx.arena,
            &cx.staged.fills,
            &self.pipeline,
            cx.viewport_bind_group,
        );
    }

    fn draw_batch(&self, pass: &mut wgpu::RenderPass<'_>, cx: &super::pipeline::BatchDraw<'_>) {
        // A native view emitting fill records without a mesh has nothing to
        // draw: the geometry is the mesh, and a batch carries only records.
        // A package tessellating its own paths registers a pipeline of its own
        // and owns both halves, which is the shape this ABI is for.
        let _ = (pass, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FillMesh, Gradient, GradientKind};

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "exact carriage: these are the numbers that were written, not a computation"
    )]
    fn a_record_carries_the_gradient_the_fill_declared() {
        let mesh = std::sync::Arc::new(FillMesh::default());
        let fill = CanvasFill {
            mesh,
            color: [1.0, 0.0, 0.0, 1.0],
            gradient: Some(Gradient {
                kind: GradientKind::Linear,
                from: [1.0, 0.0, 0.0, 1.0],
                mid: [0.0, 1.0, 0.0, 1.0],
                to: [0.0, 0.0, 1.0, 1.0],
                angle: std::f32::consts::FRAC_PI_2,
                offset: 0.0,
                mid_pos: 0.5,
                center: [0.5, 0.5],
                radius: 1.0,
            }),
            transform: Transform::IDENTITY,
            opacity: 0.5,
            dirty: true,
        };
        let record = FillInstance::new(&fill, 0.25);
        assert_eq!(record.grad_kind, GradientKind::Linear as u32);
        assert_eq!(record.grad_from, [1.0, 0.0, 0.0, 1.0]);
        assert!((record.misc[0] - 0.5).abs() < f32::EPSILON);
        assert!((record.misc[1] - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "exact carriage: an absent gradient is all-zero, not approximately zero"
    )]
    fn a_solid_fill_says_it_has_no_gradient_rather_than_leaving_it_to_be_inferred() {
        // The same lane-with-one-owner rule the decorated box learned:
        // presence is answered, never inferred from whether the axis happens
        // to look like a direction (INV-28).
        let fill = CanvasFill {
            mesh: std::sync::Arc::new(FillMesh::default()),
            color: [0.0, 0.0, 1.0, 1.0],
            gradient: None,
            transform: Transform::IDENTITY,
            opacity: 1.0,
            dirty: true,
        };
        let record = FillInstance::new(&fill, 0.0);
        assert_eq!(record.grad_kind, crate::frame::GRADIENT_NONE);
        assert_eq!(record.grad_axis, [0.0; 4]);
    }

    #[test]
    fn the_vertex_state_fits_the_portable_attribute_floor() {
        // Two per-vertex attributes and nine per-instance, which is eleven of
        // the sixteen every adapter guarantees. Registration checks the
        // instance half; this checks the pair, because the budget is per
        // pipeline (the lesson the decorated box paid for).
        super::super::pipeline::check_portable_layout("canvas_fill", &FillInstance::layout())
            .expect("the record layout is portable");
        let total =
            FillInstance::mesh_layout().attributes.len() + FillInstance::layout().attributes.len();
        assert!(total <= 16, "{total} attributes is past the floor");
    }
}
