//! Opacity groups (RFC-0011 T4): a translucent container faded as one image.
//!
//! Per-instance opacity multiplies a container's alpha into every primitive it
//! draws, which is correct only while none of them overlap. A card's text sits
//! on its own background, so at 50 % the background is see-through *under the
//! text* and the text is see-through *over the background*, and the two
//! darken each other where they cross. Faded as one picture, they do not.
//!
//! The mechanism is the smallest one that is right: the group's primitives are
//! drawn, in their own order and with their own depths, into one offscreen
//! target the size of the frame, and a single composite puts that picture back
//! at the group's opacity and at a draw-order depth reserved before the group's
//! first primitive. Nothing about how any primitive is drawn changes; only
//! which target it lands in.
//!
//! One target serves every group in a frame. The segmentation guarantees a
//! frame pass between a group's last piece and the next group's first, which
//! is where the first group is composited, so its picture is always consumed
//! before the target is cleared for the next.

use super::instance_arena::{InstanceArena, Region};

/// One group's per-instance data: `(opacity, depth)`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GroupInstance {
    /// The alpha the picture is composited at, and its draw-order depth.
    pub params: [f32; 2],
}

/// The composite's per-instance attribute: `(opacity, depth)` at location 1.
const INSTANCE: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![1 => Float32x2];

/// The offscreen target and the composite pipeline.
pub struct GroupCompositor {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    target: Option<Target>,
}

/// The offscreen colour and depth the group's primitives are drawn into.
struct Target {
    size: (u32, u32),
    color_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    // Held so the views stay valid; never read directly.
    _color: wgpu::Texture,
    _depth: wgpu::Texture,
}

impl GroupCompositor {
    /// Builds the composite pipeline. No target is allocated until a frame
    /// with a group asks for one: an application with no translucent container
    /// holds no offscreen texture at all.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::PipelineCompilation`](crate::ByardError::PipelineCompilation)
    /// if the shader fails GPU-side validation.
    pub async fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        quad_layout: wgpu::VertexBufferLayout<'static>,
    ) -> Result<Self, crate::ByardError> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ByardCore - Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ByardCore - Group Pipeline Layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ByardCore - Group WGSL Shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "group.wgsl"
            ))),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ByardCore - Group Composite Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[
                    quad_layout,
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GroupInstance>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: INSTANCE,
                    },
                ],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Premultiplied: the picture's colour already carries its
                    // own alpha, so multiplying by `SrcAlpha` again would
                    // darken every translucent edge in it.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: Some(super::draw_depth_stencil()),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        if let Some(error) = scope.pop().await {
            return Err(crate::ByardError::PipelineCompilation {
                pipeline: "Group".to_string(),
                reason: error.to_string(),
            });
        }
        Ok(Self {
            pipeline,
            layout,
            target: None,
        })
    }

    /// Makes sure a target of `size` exists, allocating it on the first frame
    /// with a group and again only when the frame changes size.
    pub fn ensure(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat, size: (u32, u32)) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ByardCore - Group Picture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ByardCore - Group Depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: super::DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ByardCore - Group Bind Group"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&color_view),
            }],
        });
        self.target = Some(Target {
            size,
            color_view,
            depth_view,
            bind_group,
            _color: color,
            _depth: depth,
        });
    }

    /// The offscreen colour and depth views, once [`ensure`](Self::ensure)
    /// has run.
    #[must_use]
    pub fn views(&self) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        self.target.as_ref().map(|t| (&t.color_view, &t.depth_view))
    }

    /// Whether a target is currently allocated. Read by the fast-path
    /// assertion: a frame with no group must not have caused one.
    #[must_use]
    pub const fn has_target(&self) -> bool {
        self.target.is_some()
    }

    /// Draws one group's picture into the frame pass `pass`.
    pub fn draw_composite(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        arena: &InstanceArena,
        quad_buffer: &wgpu::Buffer,
        instance: Region,
    ) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &target.bind_group, &[]);
        pass.set_vertex_buffer(0, quad_buffer.slice(..));
        pass.set_vertex_buffer(1, arena.slice(instance));
        pass.draw(0..4, 0..1);
    }
}
