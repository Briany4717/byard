//! `DecoratedBox` render pipeline (M21, RFC-0001 §3.1).
//!
//! Draws a rounded rectangle with an optional inner border, a blurred drop
//! shadow, and an overall opacity. The compiler promotes a box to this pipeline
//! (via [`RenderFrame::push_decorated`](crate::frame::RenderFrame::push_decorated))
//! only when one of those decoration fields is non-trivial; plain solid fills
//! (radius alone included) stay on the cheaper `SolidBox` pipeline.

use crate::ByardError;
use crate::frame::DecoratedBox;

/// GPU-side per-instance data for the `DecoratedBox` pipeline. The field layout
/// must match `decorated_box.wgsl`'s `InstanceInput` exactly.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DecoratedInstance {
    /// `[x, y, width, height]` in logical pixels.
    pub rect: [f32; 4],
    /// Fill colour `[r, g, b, a]`.
    pub color: [f32; 4],
    /// Per-corner radii `[tl, tr, br, bl]`.
    pub radii: [f32; 4],
    /// Border colour `[r, g, b, a]`.
    pub border_color: [f32; 4],
    /// Shadow colour `[r, g, b, a]`.
    pub shadow_color: [f32; 4],
    /// `[border_width, shadow_dx, shadow_dy, shadow_blur]`.
    pub params: [f32; 4],
    /// `[opacity, depth, shadow_spread, smooth]`, `misc.w` is the RFC-0031
    /// §S1 corner smoothing, carried in the lane a gradient present/absent flag
    /// used to occupy. The flag was redundant: `grad_axis` is `(cos θ, sin θ,
    /// …)` for a real ramp and all-zero without one, so the shader answers the
    /// same question from data it already reads, and the lane became the spare
    /// one RFC-0031 §S3 asked for.
    pub misc: [f32; 4],
    /// Paint-time transform translate (RFC-0011), from `d.base.transform`.
    /// Only the geometric fields (`translate`/`scale`/`rotate`/`origin`) are
    /// read here, `d.base.transform.opacity` is **not** consulted; `misc.x`
    /// (above) is the authoritative opacity for decorated boxes, unchanged
    /// since M21.
    pub t_translate: [f32; 2],
    /// Paint-time transform per-axis scale (RFC-0011).
    pub t_scale: [f32; 2],
    /// Paint-time transform rotation in radians (RFC-0011).
    pub t_rotate: f32,
    /// Paint-time transform pivot (RFC-0011).
    pub t_origin: [f32; 2],
    /// Gradient start colour (RFC-0001 §3.1); all-zero when `misc.w == 0`.
    pub grad_from: [f32; 4],
    /// Gradient mid colour.
    pub grad_mid: [f32; 4],
    /// Gradient end colour.
    pub grad_to: [f32; 4],
    /// `[dir_x, dir_y, mid_pos, offset]`, the ramp's axis, its middle stop's
    /// position, and its wrapping phase shift.
    pub grad_axis: [f32; 4],
}

impl From<&DecoratedBox> for DecoratedInstance {
    fn from(d: &DecoratedBox) -> Self {
        Self {
            rect: d.base.rect,
            color: d.base.color,
            radii: d.base.radii,
            border_color: d.border_color,
            shadow_color: d.shadow_color,
            params: [d.border_width, d.shadow_dx, d.shadow_dy, d.shadow_blur],
            // misc.y (depth) is stamped per-instance in `draw`; misc.z carries
            // the shadow spread (RFC-0011) and misc.w the corner smoothing
            // (RFC-0031 §S1), which the shader turns into the Lⁿ exponent.
            misc: [d.opacity, 0.0, d.shadow_spread, d.base.smooth],
            t_translate: d.base.transform.translate,
            t_scale: d.base.transform.scale,
            t_rotate: d.base.transform.rotate,
            t_origin: d.base.transform.origin,
            grad_from: d.gradient.map_or([0.0; 4], |g| g.from),
            grad_mid: d.gradient.map_or([0.0; 4], |g| g.mid),
            grad_to: d.gradient.map_or([0.0; 4], |g| g.to),
            grad_axis: d.gradient.map_or([0.0; 4], |g| {
                let [dx, dy] = g.direction();
                [dx, dy, g.mid_pos, g.offset]
            }),
        }
    }
}

impl DecoratedInstance {
    /// Vertex buffer layout for the per-instance step (locations 1..=15; the
    /// static quad occupies location 0).
    #[must_use]
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![
            1 => Float32x4, // rect
            2 => Float32x4, // color
            3 => Float32x4, // radii
            4 => Float32x4, // border_color
            5 => Float32x4, // shadow_color
            6 => Float32x4, // params
            7 => Float32x4, // misc
            8 => Float32x2, // transform.translate
            9 => Float32x2, // transform.scale
            10 => Float32, // transform.rotate
            11 => Float32x2, // transform.origin
            12 => Float32x4, // gradient from
            13 => Float32x4, // gradient mid
            14 => Float32x4, // gradient to
            15 => Float32x4, // gradient axis (dir, mid_pos, offset)
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<DecoratedInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: ATTRS,
        }
    }
}

/// Compiles the WGSL shader and assembles the `DecoratedBox` pipeline, wrapping
/// the whole create sequence in a single validation error scope (RFC-0001 §8).
///
/// # Errors
///
/// [`ByardError::PipelineCompilation`] if the shader or pipeline fails GPU-side
/// validation, never a panic, never a software fallback.
pub async fn build_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    quad_layout: wgpu::VertexBufferLayout<'static>,
    surface_format: wgpu::TextureFormat,
    depth_stencil: wgpu::DepthStencilState,
) -> Result<wgpu::RenderPipeline, ByardError> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ByardCore - DecoratedBox Pipeline Layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ByardCore - DecoratedBox WGSL Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
            "decorated_box.wgsl"
        ))),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ByardCore - DecoratedBox Render Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[quad_layout, DecoratedInstance::layout()],
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
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            strip_index_format: None,
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
        return Err(ByardError::PipelineCompilation {
            pipeline: "DecoratedBox".to_string(),
            reason: error.to_string(),
        });
    }

    Ok(pipeline)
}

/// Draws every [`DecoratedBox`], scissored to its content clip (RFC-0005). The
/// active GPU scissor bounds which pixels are actually touched.
///
/// The whole decorated pass is **transparent geometry**, shadows (blurred, a
/// transparent fill), borders (a stroked ring over a transparent interior), and
/// translucent fills (`opacity < 1`). Its pipeline therefore *tests* the
/// draw-order depth buffer (so a nearer opaque surface still occludes a border
/// or a scrim) but never *writes* to it. That is the standard opaque/transparent
/// split: only opaque geometry (the `SolidBox`/`TextureSampler`/`VectorMSDF`
/// passes) writes depth. Writing depth here would make a translucent box stamp a
/// nearer z across its whole rect and cull every earlier-emitted primitive drawn
/// in a later pass, most visibly all the app text beneath an overlay scrim or a
/// shadow's halo, which would simply vanish (RFC-0017). Opaque fills never reach
/// this pass; the compiler routes them to `SolidBox`, which occludes correctly.
/// Stages this batch's instances into the frame's arena (RFC-0033 §G1).
///
/// Split from [`draw`] because `wgpu` binds a buffer *range* eagerly: the
/// arena's GPU buffer has to be final, and therefore fully staged and
/// uploaded, before the first render pass opens.
pub fn stage(
    arena: &mut super::instance_arena::InstanceArena,
    scratch: &mut Vec<DecoratedInstance>,
    boxes: &[DecoratedBox],
    depths: &[f32],
) -> super::instance_arena::Region {
    if boxes.is_empty() {
        return super::instance_arena::Region::default();
    }
    // Stamp each instance's draw-order depth into `misc.y` (the shader reads it
    // as NDC-z). `depths` is parallel to `boxes`; a short/empty slice falls back
    // to the far plane so a missing depth can't push a box in front of others.
    scratch.clear();
    scratch.extend(boxes.iter().enumerate().map(|(i, b)| {
        let mut inst = DecoratedInstance::from(b);
        inst.misc[1] = depths
            .get(i)
            .copied()
            .unwrap_or(crate::frame::DRAW_DEPTH_CLEAR);
        inst
    }));
    arena.push_vertex(scratch)
}

/// Draws this segment's staged decorations, in content-clip runs (RFC-0005).
#[allow(clippy::too_many_arguments)]
pub fn draw(
    render_pass: &mut wgpu::RenderPass<'_>,
    arena: &super::instance_arena::InstanceArena,
    region: super::instance_arena::Region,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    quad_buffer: &wgpu::Buffer,
    count: usize,
    clip_slice: &[Option<u16>],
    ctx: super::ClipCtx<'_>,
) {
    if region.is_empty() {
        return;
    }
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, bind_group, &[]);
    render_pass.set_vertex_buffer(0, quad_buffer.slice(..));
    render_pass.set_vertex_buffer(1, arena.slice(region));
    // Content-clip runs (RFC-0005): scissor each run to its ScrollView viewport.
    super::for_each_clip_run(render_pass, count, clip_slice, ctx, |p, s, e| {
        p.draw(0..4, s..e);
    });
}
