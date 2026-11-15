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

/// The WGSL the engine compiles for `S`: the prelude, then the package's own
/// with its `fs_main` wrapped so the colour it returns is encoded (RFC-0060).
///
/// The package's code keeps its line numbers: the prelude is the only thing
/// before it, and everything the engine adds (the transfer functions and the
/// wrapper) comes after. A source with no `fs_main` is returned as written, and
/// the pipeline's build says why.
#[must_use]
pub fn assembled_wgsl<S: Shader>() -> String {
    let mut source = String::from(PRELUDE);
    if S::LAYERS.is_some() {
        source.push_str(LAYER_PRELUDE);
    }
    match wrap_fragment(S::wgsl()) {
        Ok(wrapped) => {
            source.push_str(&wrapped.package);
            source.push('\n');
            source.push_str(crate::color::wgsl_transfer());
            source.push_str(&wrapped.entry);
        }
        Err(_) => source.push_str(S::wgsl()),
    }
    source
}

/// A package's source with its fragment stage renamed, and the entry point
/// that calls it.
struct Wrapped {
    /// The package's WGSL, `fs_main` now `package_fs_main` and an ordinary
    /// function: its attributes blanked out in place, so no line or column
    /// before the name moves.
    package: String,
    /// The engine's `fs_main`, with the package's signature.
    entry: String,
}

/// `src` with comments blanked to spaces, so a search for code cannot land in
/// one and an index into it is an index into `src`.
fn blank_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let mut depth = 0_u32;
            while i < bytes.len() {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Index of the `)` that closes the `(` at `open`.
fn matching_paren(code: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0_u32;
    for (i, &b) in code.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Blanks every attribute (`@location(0)`, `@builtin(position)`,
/// `@interpolate(flat)`, `@invariant`) in `code[range]` into spaces, and
/// returns the range with the attributes gone, for reading names and types.
fn strip_attributes(code: &mut [u8], range: std::ops::Range<usize>) -> String {
    let mut i = range.start;
    while i < range.end {
        if code[i] == b'@' {
            let start = i;
            i += 1;
            while i < range.end && is_ident(code[i]) {
                i += 1;
            }
            let mut j = i;
            while j < range.end && code[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < range.end && code[j] == b'(' {
                if let Some(close) = matching_paren(code, j) {
                    i = close + 1;
                }
            }
            for b in &mut code[start..i] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8_lossy(&code[range]).into_owned()
}

/// Splits a parameter list at its top-level commas.
fn split_params(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut from) = (0_i32, 0);
    for (i, c) in list.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&list[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[from..]);
    parts.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// Renames the package's `fs_main` and builds the `fs_main` that encodes what
/// it returns (RFC-0060 section 5).
///
/// The package writes a linear colour with straight alpha, and may name any
/// inputs it likes: the wrapper takes the same parameters, attributes and
/// all, and hands them on by name. The package's own function is an ordinary
/// function, so the attributes only an entry point may carry are blanked in
/// place.
///
/// # Errors
///
/// A message when there is no `fn fs_main(`, or it does not return a
/// `vec4<f32>`.
fn wrap_fragment(src: &str) -> Result<Wrapped, String> {
    let masked = blank_comments(src);
    let code = masked.as_bytes();
    let mut found = None;
    let mut from = 0;
    while let Some(at) = masked[from..].find("fs_main") {
        let name = from + at;
        from = name + "fs_main".len();
        let before = masked[..name].trim_end();
        let after = masked[from..].trim_start();
        let is_fn = before.ends_with("fn")
            && before[..before.len() - 2]
                .bytes()
                .next_back()
                .is_none_or(|b| !is_ident(b));
        if is_fn && after.starts_with('(') {
            found = Some((name, masked.len() - after.len()));
            break;
        }
    }
    let Some((name, open)) = found else {
        return Err("it has no `fn fs_main(`: a package shader needs a fragment stage".to_string());
    };
    let close = matching_paren(code, open)
        .ok_or_else(|| "the parameter list of `fs_main` is never closed".to_string())?;
    let arrow = masked[close + 1..]
        .find("->")
        .map(|i| close + 1 + i)
        .filter(|&i| masked[close + 1..i].trim().is_empty())
        .ok_or_else(|| "`fs_main` must return a `vec4<f32>` colour".to_string())?;
    let brace = masked[arrow..]
        .find('{')
        .map(|i| arrow + i)
        .ok_or_else(|| "the body of `fs_main` is missing".to_string())?;

    let mut package = src.as_bytes().to_vec();
    // Blank each of the attributes in the return type and the parameters.
    let returns = strip_attributes(&mut package, arrow + 2..brace);
    if !matches!(returns.trim(), "vec4<f32>" | "vec4f") {
        return Err(format!(
            "`fs_main` must return a `vec4<f32>` colour, it returns `{}`",
            returns.trim()
        ));
    }
    let params = strip_attributes(&mut package, open + 1..close);
    let names: Vec<&str> = split_params(&params)
        .into_iter()
        .map(|p| p.split(':').next().unwrap_or(p).trim())
        .collect();
    // `@fragment` on the line above, if it is directly before the `fn`.
    let head = masked[..name].trim_end();
    let head = head.strip_suffix("fn").unwrap_or(head).trim_end();
    if let Some(at) = head.strip_suffix("@fragment").map(str::len) {
        for b in &mut package[at..at + "@fragment".len()] {
            *b = b' ';
        }
    }
    let mut package_src = String::from_utf8_lossy(&package).into_owned();
    package_src.replace_range(name..name + "fs_main".len(), "package_fs_main");

    let entry = format!(
        "\n@fragment\nfn fs_main({}) -> @location(0) vec4<f32> {{\n    return byard_out(package_fs_main({}));\n}}\n",
        &src[open + 1..close],
        names.join(", ")
    );
    Ok(Wrapped {
        package: package_src,
        entry,
    })
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

        if let Err(reason) = wrap_fragment(S::wgsl()) {
            return Err(fail(reason));
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
            // The package wrote `fs_main`; the engine renamed it.
            return Err(fail(
                error.to_string().replace("package_fs_main", "fs_main"),
            ));
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

    const SAMPLE: &str = "// fn fs_main(not: this) is a comment
struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@fragment
fn fs_main(
    in: Out,
    @builtin(front_facing) front: bool,
) -> @location(0) vec4<f32> {
    return vec4<f32>(in.uv, 0.0, 1.0);
}
";

    #[test]
    fn the_fragment_stage_is_wrapped_in_place() {
        let wrapped = wrap_fragment(SAMPLE).expect("a plain fs_main wraps");
        // Same lines, same columns up to the name: nothing a package wrote moves.
        assert_eq!(
            wrapped.package.lines().count(),
            SAMPLE.lines().count(),
            "the package's own lines are not shifted"
        );
        for (own, kept) in SAMPLE.lines().zip(wrapped.package.lines()) {
            assert!(
                own.len() <= kept.len(),
                "a line only ever grows, by the rename: {own:?} vs {kept:?}"
            );
        }
        assert!(wrapped.package.contains("fn package_fs_main("));
        assert!(
            !wrapped.package.contains("@fragment")
                && !wrapped.package.contains("@location(0) vec4"),
            "an ordinary function carries no entry-point attributes:\n{}",
            wrapped.package
        );
        assert!(
            wrapped
                .package
                .contains("// fn fs_main(not: this) is a comment"),
            "a comment is left as written"
        );
        assert!(wrapped.entry.contains("@fragment"));
        assert!(wrapped.entry.contains("@builtin(front_facing) front: bool"));
        assert!(
            wrapped
                .entry
                .contains("byard_out(package_fs_main(in, front))")
        );
    }

    #[test]
    fn a_package_without_a_fragment_stage_is_said_so() {
        let error = wrap_fragment("fn helper() -> f32 { return 1.0; }")
            .err()
            .unwrap();
        assert!(error.contains("fs_main"), "{error}");
        let error = wrap_fragment(
            "@fragment fn fs_main() -> @location(0) vec3<f32> { return vec3<f32>(1.0); }",
        )
        .err()
        .unwrap();
        assert!(
            error.contains("vec4<f32>") && error.contains("vec3<f32>"),
            "{error}"
        );
    }

    #[test]
    fn the_wrapped_module_is_valid() {
        struct Wrapped;
        impl Shader for Wrapped {
            const NAME: &'static str = "wrapped";
            type Instance = [f32; 4];
            fn lanes() -> &'static [Lane] {
                &[Lane::Vec4]
            }
            fn wgsl() -> &'static str {
                "
struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(
    @location(0) quad: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(15) depth: f32,
) -> Out {
    var out: Out;
    out.clip = to_clip(rect.xy + quad * rect.zw, depth);
    out.uv = quad;
    return out;
}
@fragment
fn fs_main(in: Out, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return vec4<f32>(in.uv, 0.0, 1.0);
}
"
            }
        }
        let source = assembled_wgsl::<Wrapped>();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
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
