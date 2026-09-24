//! Coverage masks for `clip(path)` (RFC-0037).
//!
//! # Why a coverage texture and not a stencil
//!
//! A stencil is cheaper, and the attachment switch this engine would need for
//! one is safe: depths are spaced `1/65536` apart and a 24-bit unorm quantum is
//! about `6e-8`, so nothing comes near z-fighting. That is the argument for a
//! stencil, and it is the wrong one to follow.
//!
//! **A stencil is one bit.** The rounded clip that shipped beside this cuts its
//! edge with an analytic SDF, so it is smooth. A stencil-based path clip would
//! put a hard, aliased edge on the one boundary the user actually drew, while
//! every boundary the engine generated stayed soft. That is a worse outcome
//! than not shipping the feature, and it is the class of defect that is correct
//! by every test and visibly wrong on screen.
//!
//! So each mask is rasterised into a multisampled R8 attachment and resolved
//! into a single-sample coverage texture, which the shared clip test multiplies
//! into the coverage it already computes (`encoder/clip.wgsl`). A path clip and
//! a rounded clip antialias by the same final expression rather than by two
//! implementations that agree today.
//!
//! # The strip
//!
//! Every mask in a frame goes into one texture, laid out left to right at each
//! mask's own bounding-box size. Masks are the outlines of cards and avatars,
//! so there are a handful of them and they are small; a viewport-sized layer
//! each would cost megabytes to hold a shape that occupies a few thousand
//! pixels. The strip grows to fit and is never shrunk within a session, on the
//! same reasoning the instance arena uses: a reallocation is the expensive
//! event, and the high-water mark of one session is a good predictor of the
//! next frame.

use crate::frame::ClipMask;

/// Samples per pixel in the mask attachment.
///
/// Four rather than two because the edges being resolved are arbitrary curves
/// rather than the near-axis-aligned ones a UI is mostly made of, and four is
/// the count every backend this engine targets supports for a single-channel
/// attachment. It is also the count `wgpu` guarantees without querying.
const SAMPLES: u32 = 4;

/// The largest strip this will allocate, in physical pixels on either axis.
///
/// Not a policy about how many masks are reasonable: it is the floor every
/// adapter guarantees for a 2-D texture, so a strip that would exceed it is
/// refused rather than handed to the driver as a validation error nobody can
/// act on.
const MAX_DIM: u32 = 8192;

/// Where one mask ended up in the strip, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaskSlot {
    /// Left edge in the strip.
    pub x: u32,
    /// Width in the strip.
    pub width: u32,
    /// Height in the strip.
    pub height: u32,
}

/// The GPU resources for this frame's clip masks.
pub struct ClipMaskAtlas {
    /// The resolved, single-sample coverage the clip test samples.
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The multisampled attachment the meshes are drawn into, resolved into
    /// `texture` at the end of the pass.
    msaa: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    /// Allocated size in physical pixels.
    size: (u32, u32),
    /// The pipeline that writes coverage, and the per-mask uniform it reads.
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Where each mask of the last [`prepare`](Self::prepare) landed.
    slots: Vec<MaskSlot>,
}

impl ClipMaskAtlas {
    /// Builds the pipeline and a 1×1 placeholder strip.
    ///
    /// The placeholder is not an optimisation, it is what lets the clip test
    /// have exactly one shape: the binding always exists and always samples
    /// something, so an unclipped fragment and a path-clipped one take the same
    /// instructions. A frame with no path clips allocates a single texel and
    /// never draws into it.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::PipelineCompilation`](crate::ByardError::PipelineCompilation)
    /// if the mask shader fails GPU-side validation.
    pub async fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, crate::ByardError> {
        let (pipeline, layout) = build_pipeline(device).await?;

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ByardCore - ClipMask Sampler"),
            // Linear, so a fragment between two texels reads the coverage in
            // between. The resolve has already done the antialiasing; this is
            // what keeps it when the mask is sampled at a slightly different
            // rate than it was rasterised at.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            // Clamped, so a fragment that lands outside the mask's own region
            // reads its edge rather than the neighbouring mask in the strip.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let (texture, view, msaa, msaa_view) = allocate(device, (1, 1));
        // Written once so the placeholder samples as "fully covered" rather
        // than as whatever the driver left in it: an unused mask binding must
        // not be able to erase anything.
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255_u8],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        Ok(Self {
            texture,
            view,
            msaa,
            msaa_view,
            size: (1, 1),
            pipeline,
            layout,
            sampler,
            slots: Vec::new(),
        })
    }

    /// The resolved coverage texture, for the shared viewport bind group.
    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The sampler the clip test reads it with.
    #[must_use]
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    /// Where each of the last prepared masks landed in the strip.
    #[must_use]
    pub fn slots(&self) -> &[MaskSlot] {
        &self.slots
    }

    /// The strip's allocated size in physical pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Rasterises `masks` into the strip, growing it if needed, and returns
    /// whether the atlas texture was reallocated (so the caller can rebuild the
    /// bind group that names it).
    ///
    /// Draws nothing and reallocates nothing when `masks` is empty, which is
    /// the frame every application without a path clip renders.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        masks: &[ClipMask],
        scale: f32,
    ) -> bool {
        self.slots.clear();
        if masks.is_empty() {
            return false;
        }
        let reallocated = self.layout_strip(device, masks, scale);

        let bind_group = self.mask_uniforms(device, queue, masks);

        // The meshes, concatenated into one vertex and one index buffer, so
        // the pass is one binding and N draws rather than N of each.
        let mut vertices: Vec<super::canvas_fill::FillVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut ranges: Vec<(u32, u32, i32)> = Vec::new();
        for m in masks {
            let base_vertex = i32::try_from(vertices.len()).unwrap_or(0);
            let first_index = u32::try_from(indices.len()).unwrap_or(0);
            vertices.extend_from_slice(&m.mesh.vertices);
            indices.extend_from_slice(&m.mesh.indices);
            let count = u32::try_from(m.mesh.indices.len()).unwrap_or(0);
            ranges.push((first_index, count, base_vertex));
        }
        if indices.is_empty() {
            return reallocated;
        }
        let vertex_buffer = create_init_buffer(
            device,
            "ByardCore - ClipMask Vertices",
            bytemuck::cast_slice(&vertices),
            wgpu::BufferUsages::VERTEX,
        );
        let index_buffer = create_init_buffer(
            device,
            "ByardCore - ClipMask Indices",
            bytemuck::cast_slice(&indices),
            wgpu::BufferUsages::INDEX,
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ByardCore - ClipMask Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa_view,
                depth_slice: None,
                resolve_target: Some(&self.view),
                ops: wgpu::Operations {
                    // Cleared to zero: everything outside a mask's triangles is
                    // uncovered, and the whole strip is rewritten every frame
                    // it is used at all.
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        #[allow(clippy::cast_precision_loss)]
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.width == 0 || slot.height == 0 {
                continue;
            }
            // The viewport *is* the placement: the vertex shader maps a mask's
            // bounds onto the whole of clip space, and this decides which part
            // of the strip that whole lands on.
            pass.set_viewport(
                slot.x as f32,
                0.0,
                slot.width as f32,
                slot.height as f32,
                0.0,
                1.0,
            );
            #[allow(clippy::cast_possible_truncation)]
            let offset = UNIFORM_STRIDE as u32 * u32::try_from(i).unwrap_or(0);
            pass.set_bind_group(0, Some(&bind_group), &[offset]);
            let (first, count, base) = ranges[i];
            if count > 0 {
                pass.draw_indexed(first..first + count, base, 0..1);
            }
        }
        drop(pass);
        reallocated
    }

    /// Lays the strip out left to right at each mask's own size and grows the
    /// texture if the layout no longer fits, returning whether it grew.
    ///
    /// The sizes decide whether the texture has to grow, and growing it after
    /// the slots are known is one branch instead of two.
    fn layout_strip(&mut self, device: &wgpu::Device, masks: &[ClipMask], scale: f32) -> bool {
        // Lay the strip out first: the sizes decide whether the texture has to
        // grow, and growing it after the slots are known is one branch instead
        // of two.
        let mut x = 0_u32;
        let mut height = 1_u32;
        for m in masks {
            let w = physical(m.bounds.width, scale);
            let h = physical(m.bounds.height, scale);
            self.slots.push(MaskSlot {
                x,
                width: w,
                height: h,
            });
            x = x.saturating_add(w);
            height = height.max(h);
        }
        let needed = (x.clamp(1, MAX_DIM), height.min(MAX_DIM));

        if needed.0 > self.size.0 || needed.1 > self.size.1 {
            // Grown to the high-water mark rather than to exactly what this
            // frame needs, so a mask that changes size by a pixel does not
            // reallocate a texture every frame.
            let size = (
                needed.0.max(self.size.0).next_power_of_two().min(MAX_DIM),
                needed.1.max(self.size.1).next_power_of_two().min(MAX_DIM),
            );
            let (texture, view, msaa, msaa_view) = allocate(device, size);
            self.texture = texture;
            self.view = view;
            self.msaa = msaa;
            self.msaa_view = msaa_view;
            self.size = size;
            return true;
        }
        false
    }

    /// One uniform per mask at the dynamic-offset stride, and the bind group
    /// that names them.
    fn mask_uniforms(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        masks: &[ClipMask],
    ) -> wgpu::BindGroup {
        let stride = usize::try_from(UNIFORM_STRIDE).unwrap_or(256);
        let mut uniforms = vec![0.0_f32; (stride / 4) * masks.len()];
        for (i, m) in masks.iter().enumerate() {
            let base = (stride / 4) * i;
            uniforms[base..base + 4].copy_from_slice(&[
                m.bounds.x,
                m.bounds.y,
                m.bounds.width,
                m.bounds.height,
            ]);
        }
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ByardCore - ClipMask Uniforms"),
            size: UNIFORM_STRIDE * masks.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buffer, 0, bytemuck::cast_slice(&uniforms));
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ByardCore - ClipMask Bind Group"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(UNIFORM_STRIDE),
                }),
            }],
        })
    }
}

/// The coverage pipeline and the layout of its per-mask uniform, built inside
/// one validation scope so a shader error is reported as this pipeline's.
async fn build_pipeline(
    device: &wgpu::Device,
) -> Result<(wgpu::RenderPipeline, wgpu::BindGroupLayout), crate::ByardError> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ByardCore - ClipMask Layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(UNIFORM_STRIDE),
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ByardCore - ClipMask Pipeline Layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ByardCore - ClipMask WGSL Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
            "clip_mask.wgsl"
        ))),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ByardCore - ClipMask Render Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[super::canvas_fill::FillInstance::mesh_layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R8Unorm,
                // No blending: a mask is a union of triangles and the
                // tessellator already resolved the winding, so overlapping
                // triangles must read as covered once rather than twice.
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            // Both windings kept: `lyon` emits whichever the path implies,
            // and culling one of them would silently drop half of a shape
            // authored counter-clockwise.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: SAMPLES,
            ..Default::default()
        },
        multiview_mask: None,
        cache: None,
    });

    if let Some(error) = scope.pop().await {
        return Err(crate::ByardError::PipelineCompilation {
            pipeline: "ClipMask".to_string(),
            reason: error.to_string(),
        });
    }
    Ok((pipeline, layout))
}

/// Stride between per-mask uniforms: the 256 every adapter's dynamic-offset
/// alignment divides, the same reasoning the clip table's stride follows.
const UNIFORM_STRIDE: u64 = 256;

/// A logical extent in physical pixels, at least one so a hairline mask still
/// has a texel to be rasterised into.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn physical(logical: f32, scale: f32) -> u32 {
    ((logical * scale).ceil().max(1.0)) as u32
}

fn allocate(
    device: &wgpu::Device,
    size: (u32, u32),
) -> (
    wgpu::Texture,
    wgpu::TextureView,
    wgpu::Texture,
    wgpu::TextureView,
) {
    let extent = wgpu::Extent3d {
        width: size.0.max(1),
        height: size.1.max(1),
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ByardCore - ClipMask Coverage"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let msaa = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ByardCore - ClipMask Coverage MSAA"),
        size: extent,
        mip_level_count: 1,
        sample_count: SAMPLES,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let msaa_view = msaa.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view, msaa, msaa_view)
}

fn create_init_buffer(
    device: &wgpu::Device,
    label: &str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage,
    })
}
