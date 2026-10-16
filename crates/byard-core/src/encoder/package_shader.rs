//! A package's own shader, as a pipeline the engine builds and draws
//! (RFC-0039 §"Pipeline registration").
//!
//! A package describes a pipeline and never holds one. It names an instance
//! record, says what lanes the record has, and writes the WGSL that draws a
//! quad per instance. The engine builds the `wgpu` pipeline at startup, stages
//! the instances a native view emits through the same arena every core pool
//! goes through, and draws each batch in one call. No `wgpu` type reaches the
//! package, so there is nothing for it to keep past a frame.
//!
//! # What the shader is given
//!
//! The engine prepends a short prelude to the package's WGSL:
//!
//! ```wgsl
//! @group(0) @binding(0) var<uniform> viewport_size: vec2<f32>;
//! fn to_clip(logical: vec2<f32>, depth: f32) -> vec4<f32>
//! // and, for a shader that declares layers:
//! @group(1) @binding(0) var layers: texture_2d_array<f32>;
//! @group(1) @binding(1) var layer_sampler: sampler;
//! ```
//!
//! and the vertex stage receives the unit quad at location 0 (`vec2<f32>`,
//! `(0,0)` to `(1,1)`), the instance's lanes at locations 1 and up in the
//! order [`Shader::lanes`] lists them, and the batch's draw-order depth at
//! location 15 (`f32`), to be passed to `to_clip` unchanged.
//!
//! # Layers
//!
//! A shader that samples images declares a [`Layers`]: one array texture of
//! equal-sized layers, owned by the engine for the life of the pipeline. A
//! view writes pixels into a layer when it has them
//! ([`RenderCtx::write_layer`](crate::render::RenderCtx::write_layer)) and
//! puts the layer's index in its instances, so any number of images draw in
//! one batch. Which image lives in which layer is the view's own bookkeeping:
//! the engine keeps the pixels and nothing else.

use std::any::TypeId;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};

use super::pipeline::{BatchDraw, ErasedPipeline, PipelineCtx, RenderPipeline, SegmentDraw};
use crate::ByardError;

/// The shader location the batch's draw-order depth arrives at.
pub const DEPTH_LOCATION: u32 = 15;

/// One field of an instance record, as the shader sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// One `f32`.
    F32,
    /// `vec2<f32>`.
    Vec2,
    /// `vec3<f32>`.
    Vec3,
    /// `vec4<f32>`.
    Vec4,
    /// One `u32`.
    U32,
}

impl Lane {
    const fn format(self) -> wgpu::VertexFormat {
        match self {
            Self::F32 => wgpu::VertexFormat::Float32,
            Self::Vec2 => wgpu::VertexFormat::Float32x2,
            Self::Vec3 => wgpu::VertexFormat::Float32x3,
            Self::Vec4 => wgpu::VertexFormat::Float32x4,
            Self::U32 => wgpu::VertexFormat::Uint32,
        }
    }

    /// The lane's size in bytes.
    #[must_use]
    pub const fn size(self) -> u64 {
        match self {
            Self::F32 | Self::U32 => 4,
            Self::Vec2 => 8,
            Self::Vec3 => 12,
            Self::Vec4 => 16,
        }
    }
}

/// The array texture a shader samples: `count` layers of `width` by `height`
/// physical pixels each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layers {
    /// Width of every layer.
    pub width: u32,
    /// Height of every layer.
    pub height: u32,
    /// How many layers there are.
    pub count: u32,
}

/// A pipeline a package describes for the engine to build (RFC-0039).
pub trait Shader: 'static {
    /// The pipeline's name, for diagnostics.
    const NAME: &'static str;

    /// The array texture this shader samples, if it samples one.
    const LAYERS: Option<Layers> = None;

    /// The per-instance record. Its fields are, in order and with no padding
    /// between them, the lanes [`lanes`](Self::lanes) lists.
    type Instance: bytemuck::Pod;

    /// The record's fields, in order. They arrive at shader locations 1, 2, …
    fn lanes() -> &'static [Lane];

    /// The WGSL: a `vs_main` and an `fs_main`, written against the prelude
    /// described in this module's documentation.
    fn wgsl() -> &'static str;
}

/// The engine-built pipeline for the [`Shader`] `S`, which is what a view
/// names when it emits.
pub struct ShaderPipeline<S: Shader> {
    pipeline: wgpu::RenderPipeline,
    layers: Option<LayerStore>,
    _shader: PhantomData<fn() -> S>,
}

/// The array texture of a layered shader and the binding that samples it.
struct LayerStore {
    spec: Layers,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// The prelude every package shader is compiled with.
const PRELUDE: &str = "\
@group(0) @binding(0) var<uniform> viewport_size: vec2<f32>;

// A position in logical pixels, and a batch's draw-order depth, in clip space.
fn to_clip(logical: vec2<f32>, depth: f32) -> vec4<f32> {
    return vec4<f32>(
        (logical.x / viewport_size.x) * 2.0 - 1.0,
        1.0 - (logical.y / viewport_size.y) * 2.0,
        depth,
        1.0,
    );
}
";

/// What a layered shader is given on top of [`PRELUDE`].
const LAYER_PRELUDE: &str = "\
@group(1) @binding(0) var layers: texture_2d_array<f32>;
@group(1) @binding(1) var layer_sampler: sampler;
";

/// The WGSL the engine compiles for `S`: the prelude, then the package's own.
#[must_use]
pub fn assembled_wgsl<S: Shader>() -> String {
    let mut source = String::from(PRELUDE);
    if S::LAYERS.is_some() {
        source.push_str(LAYER_PRELUDE);
    }
    source.push_str(S::wgsl());
    source
}

/// The vertex attributes of `S`'s instance record, built once and kept: a
/// vertex layout borrows them for `'static`.
fn attributes<S: Shader>() -> &'static [wgpu::VertexAttribute] {
    static BUILT: OnceLock<Mutex<HashMap<TypeId, &'static [wgpu::VertexAttribute]>>> =
        OnceLock::new();
    let mut built = BUILT
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    built.entry(TypeId::of::<S>()).or_insert_with(|| {
        let mut offset = 0;
        let attrs: Vec<wgpu::VertexAttribute> = S::lanes()
            .iter()
            .zip(1_u32..)
            .map(|(lane, shader_location)| {
                let attr = wgpu::VertexAttribute {
                    offset,
                    shader_location,
                    format: lane.format(),
                };
                offset += lane.size();
                attr
            })
            .collect();
        Box::leak(attrs.into_boxed_slice())
    })
}

/// The bytes [`Shader::lanes`] add up to, which has to be the size of the
/// instance record.
fn lanes_size<S: Shader>() -> u64 {
    S::lanes().iter().map(|lane| lane.size()).sum()
}

fn depth_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: &[wgpu::VertexAttribute] = &[wgpu::VertexAttribute {
        offset: 0,
        shader_location: DEPTH_LOCATION,
        format: wgpu::VertexFormat::Float32,
    }];
    wgpu::VertexBufferLayout {
        array_stride: 4,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: ATTRS,
    }
}

fn quad_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: &[wgpu::VertexAttribute] = &[wgpu::VertexAttribute {
        offset: 0,
        shader_location: 0,
        format: wgpu::VertexFormat::Float32x2,
    }];
    wgpu::VertexBufferLayout {
        array_stride: 8,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: ATTRS,
    }
}

/// The bind-group layout of a layered shader's group 1: the array texture
/// and its sampler.
fn layer_bind_group_layout(device: &wgpu::Device, name: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(name),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

impl<S: Shader> ShaderPipeline<S> {
    /// Builds the pipeline `S` describes.
    ///
    /// # Errors
    ///
    /// [`ByardError::PipelineCompilation`] naming the shader when its lanes do
    /// not add up to its instance record, when it uses the location reserved
    /// for depth, or when its WGSL does not compile against the layout it
    /// declared.
    pub async fn build(cx: &PipelineCtx<'_>) -> Result<Self, ByardError> {
        let fail = |reason: String| ByardError::PipelineCompilation {
            pipeline: S::NAME.to_string(),
            reason,
        };
        let record = std::mem::size_of::<S::Instance>() as u64;
        if lanes_size::<S>() != record {
            return Err(fail(format!(
                "its lanes are {} bytes and its instance record is {record}: the record's \
                 fields must be exactly the lanes, in order",
                lanes_size::<S>()
            )));
        }
        if S::lanes().len() as u64 >= u64::from(DEPTH_LOCATION) {
            return Err(fail(format!(
                "it has {} lanes, and location {DEPTH_LOCATION} is the batch's depth: at most {}",
                S::lanes().len(),
                DEPTH_LOCATION - 1
            )));
        }
        if let Some(layers) = S::LAYERS {
            if layers.width == 0 || layers.height == 0 || layers.count == 0 {
                return Err(fail("its layers have a zero dimension".to_string()));
            }
        }

        let scope = cx.device.push_error_scope(wgpu::ErrorFilter::Validation);

        let layer_layout = S::LAYERS.map(|_| layer_bind_group_layout(cx.device, S::NAME));
        let pipeline_layout = cx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(S::NAME),
                bind_group_layouts: &[Some(cx.viewport_layout), layer_layout.as_ref()],
                immediate_size: 0,
            });
        let module = cx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(S::NAME),
                source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(assembled_wgsl::<S>())),
            });
        let pipeline = cx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(S::NAME),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    buffers: &[quad_layout(), Self::vertex_layout(), depth_layout()],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: cx.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..wgpu::PrimitiveState::default()
                },
                // A package's fragments may be translucent, so they test the
                // draw order and do not write it, as the engine's own
                // translucent pipelines do.
                depth_stencil: Some(super::draw_depth_stencil_no_write()),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let layers = match (S::LAYERS, layer_layout) {
            (Some(spec), Some(layout)) => Some(LayerStore::new(cx.device, &layout, spec, S::NAME)),
            _ => None,
        };

        if let Some(error) = scope.pop().await {
            return Err(fail(error.to_string()));
        }
        Ok(Self {
            pipeline,
            layers,
            _shader: PhantomData,
        })
    }
}

impl LayerStore {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        spec: Layers,
        name: &str,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(name),
            size: wgpu::Extent3d {
                width: spec.width,
                height: spec.height,
                // A one-layer array is still an array to the shader, and some
                // backends only agree when it has at least two (the MSDF atlas
                // found this first).
                depth_or_array_layers: spec.count.max(2),
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..wgpu::TextureViewDescriptor::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(name),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(name),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self {
            spec,
            texture,
            bind_group,
        }
    }
}

impl<S: Shader> RenderPipeline for ShaderPipeline<S> {
    const NAME: &'static str = S::NAME;
    type Instance = S::Instance;

    fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<S::Instance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: attributes::<S>(),
        }
    }

    /// Nothing: the engine has no pool of a package's instances. Everything a
    /// package pipeline draws arrives as a native view's batch.
    fn draw(&self, _pass: &mut wgpu::RenderPass<'_>, _cx: &SegmentDraw<'_>) {}

    fn draw_batch(&self, pass: &mut wgpu::RenderPass<'_>, cx: &BatchDraw<'_>) {
        if cx.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, cx.viewport_bind_group, &[super::clip_offset(None)]);
        if let Some(layers) = &self.layers {
            pass.set_bind_group(1, &layers.bind_group, &[]);
        }
        pass.set_vertex_buffer(0, cx.quad_buffer.slice(..));
        pass.set_vertex_buffer(1, cx.arena.slice(cx.instances));
        pass.set_vertex_buffer(2, cx.arena.slice(cx.depths));
        pass.draw(0..4, 0..cx.count);
    }

    fn write_layer(&self, queue: &wgpu::Queue, layer: u32, pixels: &[u8]) -> bool {
        let Some(store) = &self.layers else {
            return false;
        };
        let spec = store.spec;
        let expected = u64::from(spec.width) * u64::from(spec.height) * 4;
        if layer >= spec.count || pixels.len() as u64 != expected {
            return false;
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &store.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(spec.width * 4),
                rows_per_image: Some(spec.height),
            },
            wgpu::Extent3d {
                width: spec.width,
                height: spec.height,
                depth_or_array_layers: 1,
            },
        );
        true
    }
}

/// What building a shader's pipeline resolves to.
type Building<'a> = Pin<Box<dyn Future<Output = Result<Box<dyn ErasedPipeline>, ByardError>> + 'a>>;

/// Builds one registered shader's pipeline.
type Builder = for<'a> fn(&'a PipelineCtx<'a>) -> Building<'a>;

fn builders() -> &'static Mutex<Vec<(TypeId, Builder)>> {
    static BUILDERS: OnceLock<Mutex<Vec<(TypeId, Builder)>>> = OnceLock::new();
    BUILDERS.get_or_init(Mutex::default)
}

fn build_erased<'a, S: Shader>(cx: &'a PipelineCtx<'a>) -> Building<'a> {
    Box::pin(async move {
        let pipeline = ShaderPipeline::<S>::build(cx).await?;
        Ok(Box::new(pipeline) as Box<dyn ErasedPipeline>)
    })
}

/// Registers the shader `S`, so the next encoder builds its pipeline.
///
/// Registering one shader twice is one registration. Returns whether this
/// call added it.
pub fn register<S: Shader>() -> bool {
    let mut list = builders()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let id = TypeId::of::<S>();
    if list.iter().any(|(known, _)| *known == id) {
        return false;
    }
    list.push((id, build_erased::<S>));
    true
}

/// Builds every registered shader's pipeline, in registration order.
pub(super) async fn build_registered(
    cx: &PipelineCtx<'_>,
) -> Result<Vec<Box<dyn ErasedPipeline>>, ByardError> {
    let list: Vec<Builder> = builders()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(_, build)| *build)
        .collect();
    let mut built = Vec::with_capacity(list.len());
    for build in list {
        built.push(build(cx).await?);
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Tile {
        rect: [f32; 4],
        layer: u32,
        fade: f32,
    }

    struct Tiles;

    impl Shader for Tiles {
        const NAME: &'static str = "tiles";
        const LAYERS: Option<Layers> = Some(Layers {
            width: 4,
            height: 4,
            count: 3,
        });
        type Instance = Tile;
        fn lanes() -> &'static [Lane] {
            &[Lane::Vec4, Lane::U32, Lane::F32]
        }
        fn wgsl() -> &'static str {
            "// the package's own"
        }
    }

    #[test]
    fn lanes_become_attributes_at_locations_one_and_up() {
        let attrs = attributes::<Tiles>();
        let seen: Vec<(u32, u64)> = attrs
            .iter()
            .map(|a| (a.shader_location, a.offset))
            .collect();
        assert_eq!(seen, [(1, 0), (2, 16), (3, 20)]);
        assert_eq!(lanes_size::<Tiles>(), std::mem::size_of::<Tile>() as u64);
    }

    #[test]
    fn a_layered_shader_is_compiled_with_the_layer_bindings() {
        let source = assembled_wgsl::<Tiles>();
        assert!(source.contains("fn to_clip"));
        assert!(source.contains("var layers: texture_2d_array<f32>"));
        assert!(source.ends_with("// the package's own"));
    }

    #[test]
    fn registering_a_shader_twice_is_one_registration() {
        // The registry is the process's, so every encoder a later test builds
        // compiles this too: it has to be a shader that builds.
        struct Once;
        impl Shader for Once {
            const NAME: &'static str = "once";
            type Instance = [f32; 4];
            fn lanes() -> &'static [Lane] {
                &[Lane::Vec4]
            }
            fn wgsl() -> &'static str {
                "
@vertex
fn vs_main(
    @location(0) quad: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(15) depth: f32,
) -> @builtin(position) vec4<f32> {
    return to_clip(rect.xy + quad * rect.zw, depth);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
"
            }
        }
        assert!(register::<Once>());
        assert!(!register::<Once>());
    }
}
