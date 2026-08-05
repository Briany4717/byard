//! # Encoder
//!
//! Multi-pipeline `wgpu` command dispatch.
//!
//! This subsystem owns the specialised render pipelines compiled at startup:
//!
//! - **`SolidBox`**, Axis-aligned rectangles with solid fill **and per-corner
//!   `border-radius` via an analytical SDF**. This absorbs the basic rounded-rect
//!   case from the RFC §3.1 `DecoratedBox` column by design: a single instanced
//!   pipeline handles both plain and rounded rectangles with zero extra GPU state
//!   switches, because the radius parameters are part of the per-instance vertex
//!   data rather than a pipeline variant.
//! - **`DecoratedBox`**, Rectangles with gradients, box-shadows, and parametric
//!   decorations (future sub-issue).
//! - **`TextGlyph`**, Text rendering via a `glyphon` glyph atlas (future).
//! - **`TextureSampler`**, UV-mapped quads for decoded images and icons (future).
//!
//! Primitives are batched into Z-bins (stacking contexts) and ordered first by
//! pipeline, then by local Z-index, to minimise GPU context switches.
//!
//! Pipeline creation is wrapped in `Device::push_error_scope` (returns an
//! `ErrorScopeGuard` in wgpu 28+) and `scope.pop().await` with
//! `ErrorFilter::Validation`. Failures are surfaced as
//! [`ByardError::PipelineCompilation`](crate::ByardError::PipelineCompilation)
//!, the engine never panics on a GPU error.
//!
//! # Where the encode time actually goes (RFC-0030 §I1)
//!
//! `encode.frame` used to be a single row on the `byard dev` readout, and the
//! standing assumption about it, including in RFC-0033's summary, was that
//! it was dominated by the per-frame `create_buffer_init` calls each pipeline
//! made. It was not. Measured before either RFC landed, on Apple M2, debug
//! build with `telemetry`, steady state:
//!
//! | Scope | `examples/profiling` | 12 wrapping paragraphs |
//! |---|---|---|
//! | `encode.frame` (inclusive) | 6.55 ms | 45.78 ms |
//! | ↳ `encode.glyphs` | **5.69 ms (84 %)** | **45.13 ms (98 %)** |
//! | ↳ `encode.passes` (inclusive) | 0.25 ms | 0.17 ms |
//! | ↳↳ `encode.buffers` (Σ) | 0.23 ms (3.4 %) | 0.15 ms (0.3 %) |
//! | ↳ `encode.uploads` | 0.00 ms | 0.00 ms |
//!
//! **The encoder's cost is glyph shaping.** And it is paid for text that did
//! not change: the second scene alters exactly one value per frame, a
//! rotation angle, which touches no text at all, yet every paragraph is
//! re-shaped, because the interpreter emits every [`TextLine`] with
//! `dirty: true` and the text pipeline has no other signal to go on. That is
//! the encoder-side price of the dirty set described in RFC-0001 §2.2 not
//! existing yet (see `0001-erratum-memory-and-dirty-model.md` and RFC-0032).
//!
//! Two consequences for anyone optimising here:
//!
//! - Removing per-frame buffer creation (RFC-0033, landed) was worth doing on
//!   determinism grounds, the engine should not allocate and free GPU
//!   resources at the display rate, but it was a **0.1–0.2 ms** change, not a
//!   5 ms one. Do not cite it as a frame-time fix.
//! - The 84–98 % term is now handled by RFC-0032's dirty set: a `TextLine`
//!   that did not change is reported clean and is never re-shaped. On the
//!   text-heavy scene above that took `encode.glyphs` from 45.1 ms to 2.0 ms.
//! - `encode.buffers` is now a **single sample per frame** covering the whole
//!   staging pass. It was one per draw group before the arena, and never one
//!   per allocation: [`crate::telemetry::SampleBlock::self_ns`] recovers direct
//!   children from a bounded scan, so a per-iteration scope inside a loop over
//!   an unbounded primitive list would make the *parent's* self-time wrong on
//!   exactly the frames worth reading.

pub mod backdrop;
pub mod canvas_shape;
pub mod decorated_box;
pub mod gpu_timer;
pub mod instance_arena;
pub mod ripple;
pub mod text_glyph;
pub mod texture_sampler;
pub mod vector_msdf;

pub use gpu_timer::GpuTimer;

/// Name of the single GPU pass this codebase currently times (RFC-0013 §"GPU
/// timing"): `SolidBox`, `DecoratedBox`, `TextureSampler`, `VectorMSDF`,
/// `CanvasShape`, and `TextGlyph` all draw within one `wgpu::RenderPass` (see
/// [`draw_ui_pass`]), so, unlike the RFC's four-pipeline illustration,
/// there is exactly one pass boundary to time today. Per-pipeline GPU timing
/// needs the encoder to split that pass first; tracked as a follow-up, not
/// attempted here.
pub const GPU_UI_PASS_SCOPE: &str = "gpu.ui_pass";

use std::sync::Arc;

use bytemuck;
use wgpu::util::DeviceExt;

use crate::ByardError;
use crate::frame::{
    AtlasUpload, BackdropInstance, LayerMark, Rect, RenderFrame, RippleInstance, Transform,
    VectorInstance, Viewport,
};
use text_glyph::{TextGlyphPipeline, TextLine};
use vector_msdf::VectorAtlas;

/// Re-exported from [`crate::frame`], the canonical definition now lives
/// there so the Logic thread can populate [`RenderFrame::instances`] without
/// importing from the Encoder subsystem (RFC-0001 §9).
pub use crate::frame::BoxInstance;

/// Re-exported so the engine's render-thread drain (M29) can downcast the
/// type-erased I/O result back to a decoded image and hand it to
/// [`EncoderSubsystem::apply_decoded`].
pub use texture_sampler::DecodedImage;

/// The async-decode plumbing the engine hands the encoder (M29), cloned out of
/// `Relay`: a Tokio handle to spawn the blocking `image::open` decode on, and
/// the type-erased result sender those tasks report back through. Held as a
/// plain struct (not a `relay` import) so the encoder never depends on the
/// relay subsystem (RFC-0001 §9 / INV-11).
struct IoContext {
    handle: tokio::runtime::Handle,
    tx: texture_sampler::DecodeResultSender,
}

impl BoxInstance {
    /// Returns the `wgpu` vertex buffer layout for the instance buffer.
    ///
    /// Step mode is [`wgpu::VertexStepMode::Instance`], the GPU advances one
    /// entry per drawn rectangle, not per vertex of the shared unit quad.
    #[must_use]
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                // rect: [x, y, w, h]
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                // color: [r, g, b, a]
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
                // radii: [tl, tr, br, bl]
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4,
                },
                // transform.translate
                wgpu::VertexAttribute {
                    offset: 48,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // transform.scale
                wgpu::VertexAttribute {
                    offset: 56,
                    shader_location: 5,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // transform.rotate
                wgpu::VertexAttribute {
                    offset: 64,
                    shader_location: 6,
                    format: wgpu::VertexFormat::Float32,
                },
                // transform.origin
                wgpu::VertexAttribute {
                    offset: 68,
                    shader_location: 7,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // transform.opacity
                wgpu::VertexAttribute {
                    offset: 76,
                    shader_location: 8,
                    format: wgpu::VertexFormat::Float32,
                },
                // smooth (RFC-0031 §S1). Location 10, not 9: the parallel
                // draw-order depth buffer already owns 9 (`solid_depth_layout`).
                wgpu::VertexAttribute {
                    offset: 80,
                    shader_location: 10,
                    format: wgpu::VertexFormat::Float32,
                },
            ],
        }
    }
}

/// The `CanvasShape` pipeline's shape-record binding (RFC-0031 §S4).
///
/// The pool lives in the instance arena, bound whole at offset zero, so a group
/// head's member index is an element index into the entire buffer. That keeps
/// the bind group *stable*: it only has to be rebuilt when the arena replaces
/// its buffer, which RFC-0033 bounds to a handful of times per session. A
/// dynamic offset would instead have meant a fresh bind group on every frame
/// whose staging order shifted, a per-frame GPU allocation, which is the exact
/// thing RFC-0033 removed from this encoder.
struct CanvasRecordBinding {
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    /// The arena's `buffer_creations` count when `bind_group` was built. Not a
    /// pointer comparison: `buffer_creations` is the arena's own public record
    /// of "the buffer you were given is no longer the buffer I have".
    creations: u32,
}

impl CanvasRecordBinding {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        arena: &instance_arena::InstanceArena,
    ) -> Self {
        let bind_group = canvas_shape::records_bind_group(device, layout, arena.buffer());
        Self {
            layout: layout.clone(),
            bind_group,
            creations: arena.buffer_creations(),
        }
    }

    /// Rebuilds the bind group iff the arena replaced its buffer since the last
    /// call. Must run **after** `arena.upload`, which is where a growth
    /// happens, and before any draw binds it.
    fn refresh(&mut self, device: &wgpu::Device, arena: &instance_arena::InstanceArena) {
        if self.creations != arena.buffer_creations() {
            self.bind_group =
                canvas_shape::records_bind_group(device, &self.layout, arena.buffer());
            self.creations = arena.buffer_creations();
        }
    }
}

/// Vertices of the shared unit quad (two triangles via `TriangleStrip`).
///
/// All rectangle instances share this single buffer. The vertex shader scales
/// and translates the quad to match each instance's `rect` field.
const QUAD_VERTICES: &[f32] = &[
    0.0, 0.0, // Top-Left
    1.0, 0.0, // Top-Right
    0.0, 1.0, // Bottom-Left
    1.0, 1.0, // Bottom-Right
];

/// Owns all wgpu resources for the `SolidBox` render pipeline.
///
/// Initialised once via [`EncoderSubsystem::init`]. Holds `Arc` handles to
/// the device and queue so the render thread can submit commands without
/// locking the logic thread.
// The four bools (`viewport_dirty`, `needs_full_redraw`, `blur_auto_capable`,
// `gpu_timing_pending`) are orthogonal, independently-set flags owned by
// different subsystem concerns, folding them into a state machine would
// invent states that cannot exist and couple concerns RFC-0001 §9 keeps
// apart, so the lint's suggestion does not apply here.
#[allow(clippy::struct_excessive_bools)]
pub struct EncoderSubsystem {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    render_pipeline: wgpu::RenderPipeline,
    /// No-blend variant of `SolidBox`'s pipeline, used only to paint a fully
    /// transparent "clear quad" over a dirty rect before it is repainted.
    ///
    /// `render_pipeline`'s `ALPHA_BLENDING` state means
    /// `dst_new = src.rgb * src.a + dst.rgb * (1 - src.a)`, wherever a
    /// fragment's alpha is 0 (most of a glyph's bounding box, since
    /// letterforms are sparse), the destination is left **unchanged**.
    /// Combined with `LoadOp::Load` on an incremental frame, that means old
    /// ink can never be erased by simply redrawing new content with
    /// standard "over" blending.
    /// `clear_pipeline` uses `blend: None`, so the fragment shader's output
    /// unconditionally **replaces** the destination regardless of its
    /// alpha, making it possible to genuinely wipe a rect.
    clear_pipeline: wgpu::RenderPipeline,
    quad_buffer: wgpu::Buffer,
    viewport_buffer: wgpu::Buffer,
    viewport_bind_group: wgpu::BindGroup,
    /// Text rendering pipeline, shares the UI render pass with `SolidBox`.
    text_pipeline: TextGlyphPipeline,
    /// `DecoratedBox` pipeline (M21), border/shadow/opacity boxes. Shares the
    /// viewport bind group (group 0) with `SolidBox`.
    decorated_pipeline: wgpu::RenderPipeline,
    /// `TextureSampler` pipeline (M21), `Image` quads.
    texture_pipeline: wgpu::RenderPipeline,
    /// Texture+sampler bind group layout (group 1) for `texture_pipeline`.
    texture_bind_group_layout: wgpu::BindGroupLayout,
    /// Shared linear sampler for all sampled images.
    image_sampler: wgpu::Sampler,
    /// Path-keyed cache of decoded image textures (M21).
    texture_cache: texture_sampler::TextureCache,
    /// `VectorMSDF` pipeline (RFC-0009 §1, the fifth pipeline), samples
    /// [`vector_atlas`](Self::vector_atlas) to draw crisp monochrome icons.
    vector_pipeline: wgpu::RenderPipeline,
    /// `CanvasShape` pipeline (RFC-0020, the sixth pipeline), analytic-SDF
    /// arcs/circles/lines/rects from `Canvas` shape commands. Shares the
    /// viewport bind group (group 0); transparent geometry, so it tests but
    /// never writes the draw-order depth buffer (RFC-0017 split).
    canvas_pipeline: wgpu::RenderPipeline,
    /// The `CanvasShape` pipeline's shape-record binding (RFC-0031 §S4).
    canvas_records: CanvasRecordBinding,
    /// `Ripple` pipeline (RFC-0023, the seventh pipeline), the Material ink
    /// reveal: an expanding, fading circle clipped in-shader to its element's
    /// rounded rect, composited with premultiplied-alpha "over" blending (so
    /// ink works on light and dark surfaces alike). Shares the viewport bind
    /// group (group 0); transparent geometry, so it tests but never writes
    /// the draw-order depth buffer (RFC-0017 split).
    ripple_pipeline: wgpu::RenderPipeline,
    /// Backdrop-blur pipelines (RFC-0023 §2, the eighth pipeline pair): the
    /// off-screen blur passes plus the in-pass frosted-glass composite.
    backdrop_pipelines: backdrop::BackdropPipelines,
    /// Per-backdrop-slot blur scratch textures, cached across frames and
    /// recreated only when a pane's region size changes.
    blur_scratch: backdrop::ScratchCache,
    /// Whether the `blur_quality: auto` tier runs at the capable 0.5× base
    /// resolution on this device (RFC-0023 resolved question "blur quality
    /// tiers"; the kernel is always the separable Gaussian, tiers differ in
    /// resolution only). Set by the engine from the adapter probe
    /// ([`set_blur_auto_capable`](Self::set_blur_auto_capable)); defaults to
    /// the cheap 0.25× tier, so a bare encoder (tests) is deterministic.
    blur_auto_capable: bool,
    /// The MSDF atlas: an array texture uploaded to by the JIT/AOT paths via
    /// [`RenderFrame::atlas_uploads`] (RFC-0009 §2-C, INV-8, this is the only
    /// place `Queue::write_texture` is called for it).
    vector_atlas: VectorAtlas,
    /// Reports applied-upload ids back to whoever is re-sending unconfirmed
    /// `AtlasUpload`s (the dev JIT cache), installed via
    /// [`set_vector_ack_sender`](Self::set_vector_ack_sender). `None` skips
    /// acknowledgment (e.g. a bare encoder with no JIT wired at all).
    vector_ack_tx: Option<crossbeam_channel::Sender<u64>>,
    /// Async-decode plumbing (M29): the relay's I/O runtime handle and the
    /// type-erased result sender, installed by the engine via
    /// [`set_io_context`](Self::set_io_context). `None` for a bare encoder
    /// constructed without a relay (e.g. GPU-readback tests that never load
    /// images), in that case [`encode_frame_with_decorations`] decodes
    /// synchronously, since there is no I/O pool to offload to.
    io: Option<IoContext>,
    /// DPI scale factor, derived once per resize from the OS-reported value.
    ///
    /// Stored here so `encode_frame` can pass it to `TextGlyphPipeline::prepare`
    /// without requiring the caller to supply it per-frame.
    scale_factor: f32,
    /// Set by [`update_viewport`](EncoderSubsystem::update_viewport) and
    /// consumed (cleared) by [`encode_frame`](EncoderSubsystem::encode_frame).
    ///
    /// Forces `TextGlyphPipeline::prepare` to re-prepare even when no text
    /// content has changed, necessary after a viewport resize because glyphon's
    /// `Viewport` resolution changed.
    viewport_dirty: bool,
    /// Persistent off-screen colour target that incremental (scissored) draws
    /// actually land on.
    ///
    /// RFC §3.3's scissor clipping only makes sense against a render target
    /// with *retained* content across frames. The swapchain image returned by
    /// `wgpu::Surface::get_current_texture` does not offer that guarantee
    /// under multi-buffering, wgpu is free to rotate in a stale or
    /// uninitialised image on any given frame. `persistent_color` is the
    /// real, always-retained surface that `LoadOp::Load` + `set_scissor_rect`
    /// draw into; the swapchain image only ever receives a full, unscissored
    /// copy of this texture's current contents once per frame (see
    /// `encode_frame`'s final `copy_texture_to_texture` call).
    persistent_color: wgpu::Texture,
    /// View of [`persistent_color`](Self::persistent_color), cached to avoid
    /// recreating it every frame.
    persistent_view: wgpu::TextureView,
    /// Frame-local draw-order depth buffer (RFC-0011 cross-pass paint order):
    /// cleared to the far plane every pass and rebuilt from the current
    /// primitive list, so it needs no cross-frame bookkeeping. Recreated
    /// alongside `persistent_color` on resize so the two always match in size.
    persistent_depth_view: wgpu::TextureView,
    /// Pixel format shared by `persistent_color` and the swapchain surface.
    ///
    /// Stored so [`update_viewport`](Self::update_viewport) can recreate
    /// `persistent_color` at the new size on resize without requiring the
    /// caller to pass the format again every time.
    surface_format: wgpu::TextureFormat,
    /// Physical-pixel width of [`persistent_color`](Self::persistent_color).
    phys_w: u32,
    /// Physical-pixel height of [`persistent_color`](Self::persistent_color).
    phys_h: u32,
    /// `true` when the next [`encode_frame`](EncoderSubsystem::encode_frame)
    /// must draw everything, ignoring per-`TextLine` dirty bits.
    ///
    /// Set on construction (nothing has been drawn into `persistent_color`
    /// yet) and whenever the surface is resized (the recreated texture's
    /// contents are undefined). Cleared at the end of every `encode_frame`
    /// call. This is intentionally independent of `EvaluatorTick`'s dirty
    /// collection: a freshly registered `Signal` reports an empty dirty set
    /// on its very first collection (nothing has mutated yet), so relying on
    /// `TextLine::dirty` alone would mean the first frame draws nothing.
    needs_full_redraw: bool,
    /// Number of `BoxInstance`s passed to the previous `encode_frame` call.
    ///
    /// `BoxInstance`s carry no per-instance dirty bit (nothing in the current
    /// codebase mutates a `BoxInstance` after construction), so a *count*
    /// change is the only structural signal
    /// available that the instance list changed shape. A mismatch forces a
    /// full redraw so a future caller that does start mutating the instance
    /// list cannot silently lose a newly added box to the scissor rect.
    last_instance_count: usize,
    /// Number of `TextLine`s passed to the previous `encode_frame` call, for
    /// the same structural-change reasoning as
    /// [`last_instance_count`](Self::last_instance_count).
    last_text_count: usize,
    /// Per-line bounding boxes (logical pixels) from the previous
    /// `encode_frame` call, positionally aligned with that call's `texts`
    /// slice.
    ///
    /// A dirty line's scissor contribution must cover **both** its current
    /// bounds and its bounds from the previous frame: if a line shrinks or
    /// moves, its old footprint can fall entirely outside the new bounds,
    /// leaving stale ink permanently outside the scissor rect (and
    /// therefore never cleared). See [`dirty_text_bounds`].
    last_text_bounds: Vec<Rect>,
    /// Per-`BoxInstance` bounding boxes from the previous `encode_frame` call,
    /// positionally aligned with that call's `instances` slice. Mirrors
    /// [`last_text_bounds`](Self::last_text_bounds) for the solid-box pipeline
    /// (M26) so a moved/shrunk box still clears its old footprint.
    last_box_bounds: Vec<Rect>,
    /// Per-`DecoratedBox` bounding boxes from the previous call (M27), aligned
    /// with that call's `decorated` slice. Same shrink/move-safety contract.
    last_decorated_bounds: Vec<Rect>,
    /// Previous-frame bounds of every `CanvasShape` (RFC-0020), for the
    /// incremental dirty-scissor union, same contract as
    /// [`last_decorated_bounds`](Self::last_decorated_bounds).
    last_canvas_bounds: Vec<Rect>,
    /// Previous-frame bounds of every `RippleInstance` (RFC-0023), for the
    /// incremental dirty-scissor union. A ripple animates every frame it is
    /// alive (all instances are treated dirty, like solids) and its element
    /// rect must keep repainting until the last frame *after* it fades, the
    /// previous-bounds union is what erases the final frame of ink.
    last_ripple_bounds: Vec<Rect>,
    /// Previous-frame bounds of every `BackdropInstance` (RFC-0023 §2), same
    /// contract. A backdrop re-samples the scene behind it whenever anything
    /// in the frame changed, so its pane is treated always-dirty like solids.
    last_backdrop_bounds: Vec<Rect>,
    /// Per-`TextureSampler` bounding boxes from the previous call (M27),
    /// aligned with that call's `textures` slice.
    last_texture_bounds: Vec<Rect>,
    /// The [`RenderFrame::version`] (the relay's publish sequence number) of
    /// the last frame rendered via
    /// [`encode_frame_from_relay`](Self::encode_frame_from_relay).
    ///
    /// Diagnostic only. It used to drive a forced full redraw whenever the
    /// sequence jumped, on the reasoning that a skipped frame's dirty bits
    /// were lost; `Relay::publish` now merges those bits forward instead, so
    /// nothing is lost and nothing needs compensating. Kept because "which
    /// published frame is on screen" is the first thing anyone debugging a
    /// stale-frame report wants to know.
    last_relay_version: u64,
    /// The one GPU buffer every instanced pipeline's per-frame data lives in
    /// (RFC-0033).
    arena: instance_arena::InstanceArena,
    /// Reused per-frame staging bookkeeping for that arena.
    staging: FrameStaging,
    /// Whether the last encoded frame was drawn under an incremental scissor
    /// rather than as a full redraw (RFC-0001 §3.3).
    ///
    /// The third of the audit's inert incremental layers, made assertable: a
    /// frame that changed one colour must not repaint the window, and there is
    /// no way to see that from a timing.
    last_frame_scissored: bool,
    /// Async GPU pass timing (RFC-0013 §"GPU timing"), or `None` if the
    /// device lacks `wgpu::Features::TIMESTAMP_QUERY` (P5), checked once at
    /// construction, never re-probed per frame.
    gpu_timer: Option<GpuTimer>,
    /// This frame's `Gpu`-tagged samples, drained from `gpu_timer` at the
    /// start of the next [`encode_frame_with_decorations`](Self::encode_frame_with_decorations)
    /// call and pushed onto the calling (render) thread's telemetry ring,
    /// reused across calls so draining never allocates once warmed up.
    gpu_samples_scratch: Vec<crate::telemetry::Sample>,
    /// Set when [`GpuTimer::resolve_and_copy`] ran during the last
    /// [`encode_frame_with_decorations`](Self::encode_frame_with_decorations)
    /// call (i.e. a pass was actually timed this frame); consumed by
    /// [`submit`](Self::submit), which must only call
    /// [`GpuTimer::request_map`] when there is a fresh copy to map.
    gpu_timing_pending: bool,
}

impl EncoderSubsystem {
    /// Compiles all GPU pipelines using an already-created device and queue.
    ///
    /// Adapter selection and device creation are the responsibility of the
    /// caller (typically [`Engine::init`](crate::engine::Engine::init)), which
    /// also configures the `wgpu::Surface` before calling this method.
    ///
    /// Shader compilation and pipeline creation are wrapped in a
    /// `push_error_scope` / `pop_error_scope` pair (RFC §8). Any GPU-side
    /// validation failure is returned as
    /// [`ByardError::PipelineCompilation`](crate::ByardError::PipelineCompilation)
    ///, this method never panics on a GPU error.
    ///
    /// `width`/`height` are the surface's initial dimensions in **physical
    /// pixels**, used to allocate the persistent intermediate colour target
    /// (see [`persistent_color`](Self::persistent_color)) at construction
    /// time so the very first [`encode_frame`](Self::encode_frame) call has
    /// somewhere to draw into.
    ///
    /// # Errors
    ///
    /// - [`ByardError::PipelineCompilation`], the WGSL shader or the pipeline
    ///   descriptor failed GPU-side validation.
    // A resource-wiring constructor: it allocates the quad/viewport buffers,
    // five pipelines, the persistent target and the texture cache. Splitting it
    // further would scatter one cohesive setup across helpers with no clarity
    // gain, so the line-count lint is allowed here specifically.
    #[allow(clippy::too_many_lines)]
    pub async fn init(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        surface_format: wgpu::TextureFormat,
        scale_factor: f32,
        width: u32,
        height: u32,
    ) -> Result<Self, ByardError> {
        // Static geometry shared by every SolidBox instance.
        let quad_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ByardCore - Static Quad Buffer"),
            contents: bytemuck::cast_slice(QUAD_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ByardCore - Viewport Uniform Buffer"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ByardCore - Viewport Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            }],
        });

        let viewport_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ByardCore - Viewport Bind Group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buffer.as_entire_binding(),
            }],
        });

        let quad_layout = quad_vertex_layout();

        // `bind_group_layout` is passed into the helper so that `pipeline_layout`
        // can be created inside the error scope alongside the shader and pipeline,
        // matching the full sequence required by RFC §8.
        let render_pipeline = build_solid_box_pipeline(
            &device,
            &bind_group_layout,
            quad_layout,
            surface_format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            draw_depth_stencil(),
            "ByardCore - SolidBox Render Pipeline",
        )
        .await?;

        // See `EncoderSubsystem::clear_pipeline`'s doc comment, same shader
        // and layout, only the blend state differs (no blending → the
        // fragment output unconditionally replaces the destination).
        let clear_pipeline = build_solid_box_pipeline(
            &device,
            &bind_group_layout,
            quad_vertex_layout(),
            surface_format,
            None,
            clear_depth_stencil(),
            "ByardCore - SolidBox Clear Pipeline",
        )
        .await?;

        let text_pipeline = TextGlyphPipeline::new(&device, &queue, surface_format).await?;

        // M21 pipelines (RFC-0001 §3.1).
        let (decorated_pipeline, texture_pipeline, texture_bind_group_layout, image_sampler) =
            build_m21_pipelines(&device, &bind_group_layout, surface_format).await?;

        // `VectorMSDF` pipeline (RFC-0009 §1, the fifth pipeline).
        let vector_atlas_layout = vector_msdf::bind_group_layout(&device);
        let vector_sampler = vector_msdf::sampler(&device);
        let vector_pipeline = vector_msdf::build_pipeline(
            &device,
            &bind_group_layout,
            &vector_atlas_layout,
            wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                }],
            },
            surface_format,
        )
        .await?;
        // `CanvasShape` pipeline (RFC-0020, the sixth pipeline). Transparent
        // geometry (AA strokes/fills), so it uses the no-write depth state,
        // the same opaque/transparent split as `DecoratedBox` (RFC-0017).
        let canvas_records_layout = canvas_shape::records_bind_group_layout(&device);
        let canvas_pipeline = canvas_shape::build_pipeline(
            &device,
            &bind_group_layout,
            &canvas_records_layout,
            wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                }],
            },
            surface_format,
            draw_depth_stencil_no_write(),
        )
        .await?;
        // `Ripple` pipeline (RFC-0023, the seventh pipeline). Transparent
        // geometry (the ink reveal), so it also uses the no-write depth
        // state, its stamped depth places it between an element's
        // background and its children without ever culling either.
        let ripple_pipeline = ripple::build_pipeline(
            &device,
            &bind_group_layout,
            wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                }],
            },
            surface_format,
            draw_depth_stencil_no_write(),
        )
        .await?;
        // Backdrop blur + composite pipelines (RFC-0023 §2). The composite is
        // transparent geometry like the decorated pass (no-write depth).
        let backdrop_pipelines = backdrop::build_pipelines(
            &device,
            &bind_group_layout,
            wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                }],
            },
            surface_format,
            draw_depth_stencil_no_write(),
        )
        .await?;

        // The atlas is an *array* texture (`vector_msdf::ATLAS_LAYERS` layers):
        // it must be a real `GL_TEXTURE_2D_ARRAY` on the GL backend, so a single
        // layer is not an option, see `ATLAS_LAYERS`. The dev allocator (M48)
        // grows layers on top of this on demand.
        let vector_atlas = VectorAtlas::new(
            &device,
            &vector_atlas_layout,
            &vector_sampler,
            vector_msdf::ATLAS_SIZE,
            vector_msdf::ATLAS_LAYERS,
        );

        let (persistent_color, persistent_view) =
            create_persistent_target(&device, surface_format, width, height);
        let persistent_depth_view = create_depth_target(&device, width, height);

        let gpu_timer = GpuTimer::new(&device, &queue, &[GPU_UI_PASS_SCOPE]);
        let arena = instance_arena::InstanceArena::new(&device);
        let canvas_records = CanvasRecordBinding::new(&device, &canvas_records_layout, &arena);

        Ok(Self {
            device,
            queue,
            render_pipeline,
            clear_pipeline,
            quad_buffer,
            viewport_buffer,
            viewport_bind_group,
            text_pipeline,
            decorated_pipeline,
            texture_pipeline,
            texture_bind_group_layout,
            image_sampler,
            texture_cache: texture_sampler::TextureCache::default(),
            vector_pipeline,
            canvas_pipeline,
            canvas_records,
            ripple_pipeline,
            backdrop_pipelines,
            blur_scratch: backdrop::ScratchCache::new(),
            blur_auto_capable: false,
            vector_atlas,
            vector_ack_tx: None,
            io: None,
            scale_factor,
            viewport_dirty: false,
            persistent_depth_view,
            persistent_color,
            persistent_view,
            surface_format,
            phys_w: width,
            phys_h: height,
            // Nothing has been drawn into `persistent_color` yet, the first
            // `encode_frame` call must draw everything unconditionally.
            needs_full_redraw: true,
            last_instance_count: 0,
            last_text_count: 0,
            last_text_bounds: Vec::new(),
            last_box_bounds: Vec::new(),
            last_decorated_bounds: Vec::new(),
            last_canvas_bounds: Vec::new(),
            last_ripple_bounds: Vec::new(),
            last_backdrop_bounds: Vec::new(),
            last_texture_bounds: Vec::new(),
            last_relay_version: 0,
            arena,
            staging: FrameStaging::default(),
            last_frame_scissored: false,
            gpu_timer,
            gpu_samples_scratch: Vec::new(),
            gpu_timing_pending: false,
        })
    }

    /// Returns a reference to the underlying `wgpu` device.
    ///
    /// Used by [`Engine`](crate::engine::Engine) to configure and reconfigure
    /// the `wgpu::Surface` without duplicating the device handle.
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Resolves the `blur_quality: auto` tier for this device (RFC-0023
    /// resolved question "blur quality tiers"): `true` selects the 0.5× base
    /// resolution, `false` the cheap 0.25× one, the kernel is always the
    /// separable Gaussian. The engine calls this once after adapter probing
    /// (capable on everything except software/virtual adapters); a
    /// per-element `blur_quality: high | low` always overrides it.
    pub fn set_blur_auto_capable(&mut self, capable: bool) {
        self.blur_auto_capable = capable;
    }

    /// Whether the last encoded frame took the incremental scissored path
    /// instead of redrawing the whole target (RFC-0001 §3.3).
    #[must_use]
    pub const fn last_frame_scissored(&self) -> bool {
        self.last_frame_scissored
    }

    /// The frame's shared instance arena (RFC-0033), for the assertions that
    /// keep it honest: zero GPU buffer creations and zero growths on a
    /// steady-state frame.
    #[must_use]
    pub const fn arena(&self) -> &instance_arena::InstanceArena {
        &self.arena
    }

    /// Whether this encoder's GPU pass timing is active (RFC-0013 **P5**),
    /// `false` when the device lacks `wgpu::Features::TIMESTAMP_QUERY`. Used
    /// by the overlay/CLI to show a clear "GPU timing unavailable" notice
    /// instead of a silently empty GPU section.
    #[must_use]
    pub fn gpu_timing_available(&self) -> bool {
        self.gpu_timer.is_some()
    }

    /// How many text lines the last encoded frame actually re-shaped
    /// (RFC-0032's win, made readable).
    ///
    /// Glyph shaping is the largest per-primitive term in the frame, and the
    /// only honest answer to "did the cache help this frame" is a count of what
    /// it skipped. Timing it says how long the frame took, which is a different
    /// question with a dozen other inputs.
    #[must_use]
    pub const fn last_text_reshapes(&self) -> usize {
        self.text_pipeline.reshaped_lines()
    }

    /// Submits a command buffer to the GPU queue.
    ///
    /// Thin wrapper around `queue.submit` so that callers outside this module
    /// do not need to hold a separate reference to the queue. Also requests
    /// this frame's GPU-timing readback map, if a pass was timed, `wgpu`
    /// requires the `map_async` request to happen only after the command
    /// buffer that writes the mapped buffer has actually been submitted
    /// (see [`GpuTimer::resolve_and_copy`]'s doc comment).
    pub(crate) fn submit(&mut self, buffer: wgpu::CommandBuffer) {
        // RFC-0030 §I1: a top-level scope rather than a child of
        // `encode.frame`, because the submission happens after that scope has
        // closed, the caller owns the command buffer in between. It is also
        // where `queue.write_buffer` traffic staged during encoding is
        // flushed, so a rise here is the honest place to look for upload cost
        // that the pipelines themselves do not pay.
        crate::profile_scope!("encode.submit");
        self.queue.submit(std::iter::once(buffer));
        if self.gpu_timing_pending {
            self.gpu_timing_pending = false;
            if let Some(timer) = &mut self.gpu_timer {
                timer.request_map();
            }
        }
    }

    /// Drains any GPU pass timings that finished resolving since the last
    /// call (RFC-0013 "GPU timing": never blocks, so a slot may still be
    /// pending, it is simply checked again next time) and pushes them onto
    /// this (render) thread's own telemetry ring, alongside whatever this
    /// thread profiles directly, the overlay drains both. Extracted out of
    /// [`encode_frame_with_decorations`](Self::encode_frame_with_decorations)
    /// purely to keep that function under clippy's line-count threshold.
    fn drain_gpu_samples_into_telemetry(&mut self) {
        let Some(timer) = &mut self.gpu_timer else {
            return;
        };
        self.gpu_samples_scratch.clear();
        timer.drain_ready(&self.device, &mut self.gpu_samples_scratch);
        for sample in self.gpu_samples_scratch.drain(..) {
            crate::telemetry::push_sample(sample);
        }
    }

    /// Uploads updated viewport dimensions to the GPU uniform buffer and
    /// notifies the text pipeline of the new resolution.
    ///
    /// `phys_w`/`phys_h` are the new surface dimensions in **physical pixels**.
    /// `scale` is the OS DPI scale factor; it is stored so that `encode_frame`
    /// can pass the correct value to [`TextGlyphPipeline::prepare`] without
    /// requiring the caller to supply it per-frame.
    ///
    /// Must be called whenever the surface is resized before the next frame.
    ///
    /// If `phys_w`/`phys_h` differ from the currently allocated
    /// [`persistent_color`](Self::persistent_color) size, that texture is
    /// recreated at the new size and `needs_full_redraw` is set, the
    /// recreated texture's contents are undefined, so the next
    /// `encode_frame` must repopulate it in full rather than trying to
    /// incrementally patch stale (or garbage) pixels.
    pub fn update_viewport(&mut self, viewport: Viewport, phys_w: u32, phys_h: u32, scale: f32) {
        // SolidBox viewport uniform (logical pixels, padded to 16 bytes).
        let size_data = [viewport.width, viewport.height, 0.0_f32, 0.0];
        self.queue
            .write_buffer(&self.viewport_buffer, 0, bytemuck::cast_slice(&size_data));

        // glyphon Viewport (physical pixels, glyphon always operates in physical px).
        self.text_pipeline
            .update_resolution(&self.queue, phys_w, phys_h);

        self.scale_factor = scale;
        self.viewport_dirty = true;

        if phys_w != self.phys_w || phys_h != self.phys_h {
            let (persistent_color, persistent_view) =
                create_persistent_target(&self.device, self.surface_format, phys_w, phys_h);
            self.persistent_color = persistent_color;
            self.persistent_view = persistent_view;
            self.persistent_depth_view = create_depth_target(&self.device, phys_w, phys_h);
            self.phys_w = phys_w;
            self.phys_h = phys_h;
            self.needs_full_redraw = true;
        }
    }

    /// Encodes a single UI frame into a `CommandBuffer` ready for queue submission.
    ///
    /// Implements RFC-0001 §3.3 (dirty rectangles and scissor clipping). The
    /// actual incremental drawing target is **not** `target`, it is
    /// [`persistent_color`](Self::persistent_color), an off-screen texture
    /// that, unlike the swapchain image, is guaranteed to retain its
    /// contents across frames. See that field's doc comment for why this
    /// indirection exists.
    ///
    /// Three cases, decided by [`needs_full_redraw_this_frame`]:
    ///
    /// - **Full redraw** (first call, or after a resize, or the
    ///   instance/text count changed shape since the previous call): the
    ///   inner pass clears `persistent_color` and draws every `BoxInstance`
    ///   and every `TextLine` unscissored, identical to this function's
    ///   pre-#31 behaviour.
    /// - **Incremental** (not a full redraw, and at least one `TextLine` is
    ///   dirty): the inner pass loads (does not clear) `persistent_color`,
    ///   restricts fragment writes to `wgpu::RenderPass::set_scissor_rect`
    ///   for the union of the dirty lines' *current and previous* bounding
    ///   boxes (see [`dirty_text_bounds`]), then draws a fully transparent
    ///   "clear quad" over exactly that rect via [`clear_pipeline`](
    ///   Self::clear_pipeline) before redrawing, standard alpha blending
    ///   alone cannot erase stale content (see that field's doc comment),
    ///   so this step is required, not optional. Every `BoxInstance` is
    ///   then redrawn too (the clear quad may have wiped one), bounded by
    ///   the active scissor rect. `TextGlyphPipeline::prepare`/`render_layer`
    ///   still receive the **full, unfiltered** `texts` slice (partitioned by
    ///   z-layer, never filtered); see the note above the `render_layer` call
    ///   in [`draw_ui_pass`] for why.
    /// - **Nothing dirty**: the inner pass is skipped entirely, zero GPU
    ///   work beyond the mandatory composite step below.
    ///
    /// In every case, the frame ends with an unscissored
    /// `copy_texture_to_texture` of `persistent_color`'s current contents
    /// onto `target` (the swapchain image), since the swapchain's own
    /// previous contents are never assumed valid.
    ///
    /// # Instance buffer lifetime
    ///
    /// The `SolidBox` instance buffer is allocated per call and dropped after
    /// `encoder.finish()`. A persistent ring-buffer strategy is a future sub-issue.
    ///
    /// # Errors
    ///
    /// - [`ByardError::TextPrepare`], glyphon atlas upload failed.
    /// - [`ByardError::TextRender`], glyphon render recording failed.
    pub fn encode_frame(
        &mut self,
        target: &wgpu::Texture,
        instances: &[BoxInstance],
        texts: &[TextLine],
    ) -> Result<wgpu::CommandBuffer, ByardError> {
        // No `RenderFrame` here (raw solid+text convenience path): empty depths
        // → far-plane fallback, i.e. the pre-depth type-grouped pass order;
        // empty layer marks → one z-layer, the pre-layering draw stream.
        self.encode_frame_with_decorations(
            target,
            instances,
            texts,
            &[],
            &[],
            &[],
            (&[], &[]),
            &[],
            &[],
            (&[], &[]),
            DrawDepths::default(),
            FrameClips::default(),
            FrameDirty::default(),
            &[],
            // This convenience path has no `RenderFrame` and therefore no dev
            // surfaces: every primitive in it is the caller's own.
            None,
        )
    }

    /// Full encode path including the M21 `DecoratedBox`/`TextureSampler`
    /// primitives. [`encode_frame`](Self::encode_frame) forwards here with empty
    /// decoration slices, keeping the common (solid + text) path byte-identical.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn encode_frame_with_decorations(
        &mut self,
        target: &wgpu::Texture,
        instances: &[BoxInstance],
        texts: &[TextLine],
        decorated: &[crate::frame::DecoratedBox],
        textures: &[crate::frame::TextureSampler],
        vectors: &[VectorInstance],
        // The `Canvas` pool and the shape-record pool its group heads index
        // into (RFC-0031 §S4), bundled because one is meaningless without the
        // other, a head whose members were not uploaded draws nothing.
        (canvas_shapes, shape_records): (
            &[crate::frame::CanvasShape],
            &[crate::frame::ShapeRecord],
        ),
        ripples: &[RippleInstance],
        atlas_uploads: &[AtlasUpload],
        // The backdrop pool and its parallel barrier snapshots (RFC-0023 §2),
        // bundled to stay within the argument-count lint.
        (backdrops, backdrop_marks): (&[BackdropInstance], &[LayerMark]),
        depths: DrawDepths<'_>,
        clips: FrameClips<'_>,
        dirty: FrameDirty<'_>,
        layers: &[crate::frame::LayerMark],
        // Where the dev runner's own surfaces begin in every pool
        // (`RenderFrame::dev_base`); `None` for a frame that carries none,
        // which is every frame of a shipped app. Passed explicitly rather than
        // folded into one of the bundles above because it is the one thing
        // here that answers a question about *ownership* rather than about
        // geometry (RFC-0030 erratum "self-accounting").
        dev_base: Option<LayerMark>,
    ) -> Result<wgpu::CommandBuffer, ByardError> {
        // RFC-0030 §I1: the render thread's own frame cost, pipeline
        // preparation, the scissor decision, glyph shaping and command
        // encoding, as distinct from the GPU pass timings resolved
        // asynchronously two frames later by `gpu_timer.rs`. Both land on
        // this thread's ring; the overlay separates them by `ScopeKind`.
        crate::profile_scope!("encode.frame");
        {
            // RFC-0030 §I1 sub-scope: everything this frame hands to the GPU
            // that is *not* instance data, the vector MSDF atlas layers and
            // the texture cache's decoded images. Both are texture writes, so
            // they scale with content churn rather than with node count, and
            // separating them is what tells a one-off upload spike apart from
            // a steady per-frame cost.
            crate::profile_scope!("encode.uploads");
            // RFC-0009 §2-C / INV-8: the single place this atlas is ever written
            // to. Applied unconditionally (not gated on `should_draw` below) so a
            // pending upload is never silently dropped on a skip-frame.
            let applied = self.vector_atlas.apply_uploads(&self.queue, atlas_uploads);
            if let Some(tx) = &self.vector_ack_tx {
                for id in applied {
                    // The dev JIT cache may have already dropped this entry (a
                    // hot-reload invalidated it); a disconnected/full receiver is
                    // not this encoder's problem, so ignore the send result.
                    let _ = tx.send(id);
                }
            }

            self.request_textures(textures);
        }

        self.drain_gpu_samples_into_telemetry();

        let full_redraw = needs_full_redraw_this_frame(
            self.needs_full_redraw || dirty.full,
            self.last_instance_count,
            instances.len(),
            self.last_text_count,
            texts.len(),
        );
        // M27: `DecoratedBox`/`TextureSampler` no longer force a full,
        // unscissored redraw just by being present, they now carry their own
        // `dirty` bit and contribute to the same incremental scissor union as
        // text and solid boxes (RFC-0001 §3.3).

        // The per-instance dirty bits the frame carries (RFC-0032 §R3 step 6).
        //
        // This used to be `vec![true; instances.len()]`, and the comment here
        // used to explain why that was the only honest answer: `BoxInstance`
        // is a GPU `Pod` type with no room for a dirty bit, and the lowering
        // re-emitted every instance each tick, so "all of them might have
        // changed" was the truth. It is no longer, the interpreter now
        // compares each instance's resolved values against last frame's and
        // says which ones actually moved, so the scissor union can be the
        // dirty region instead of the whole frame.
        //
        // The fallback is deliberate rather than defensive: a caller that
        // hands over a frame whose bits were never computed (a hand-built
        // frame in a test, a subsystem populating the pools directly) gets the
        // old all-dirty behaviour and a correct picture, not a silently
        // under-drawn one.
        let frame_dirty = dirty.instances;
        let fallback;
        let instances_dirty: &[bool] = if frame_dirty.len() == instances.len() {
            frame_dirty
        } else {
            fallback = vec![true; instances.len()];
            &fallback
        };

        // Only meaningful on a non-full-redraw frame, every primitive is
        // drawn regardless of its dirty bit when `full_redraw` is true.
        //
        // RFC-0030 §I1 sub-scope. This is a linear scan of **every** pool,
        // unioning the bounds of everything dirty with where it was last
        // frame, and it was previously invisible: it lives directly in
        // `encode.frame`, so its cost showed up only as self-time that no row
        // explained. §I1's own standard, a breakdown whose parts add up to
        // its parent, was not being met, and the gap was large enough to
        // matter (it is the second-largest term in the frame on a text-heavy
        // scene). Naming it is what makes "the frame got slower because there
        // are more primitives to *consider*, not more to draw" a readable
        // sentence rather than an inference.
        let scissor = if full_redraw {
            None
        } else {
            crate::profile_scope!("encode.scissor");
            compute_scissor(
                &ScissorInputs {
                    texts,
                    text_wrap: clips.text_wrap,
                    prev_texts: &self.last_text_bounds,
                    instances,
                    instances_dirty,
                    prev_boxes: &self.last_box_bounds,
                    decorated,
                    prev_decorated: &self.last_decorated_bounds,
                    textures,
                    prev_textures: &self.last_texture_bounds,
                    canvas_shapes,
                    prev_canvas: &self.last_canvas_bounds,
                    ripples,
                    prev_ripples: &self.last_ripple_bounds,
                    backdrops,
                    prev_backdrops: &self.last_backdrop_bounds,
                },
                self.scale_factor,
                self.phys_w,
                self.phys_h,
            )
        };

        // Nothing to (re)draw into `persistent_color` this frame: not a full
        // redraw, no `TextLine` is dirty, and no vector glyph just landed. The
        // swapchain still gets a fresh copy of `persistent_color` below, just
        // unchanged from last frame. A fresh `atlas_uploads` entry forces a
        // draw even with an empty scissor, since a placeholder→resident
        // transition changes a `VectorInstance`'s content but not its rect,
        // the scissor union (rect-based) would otherwise miss it entirely.
        let should_draw = full_redraw || scissor.is_some() || !atlas_uploads.is_empty();
        self.last_frame_scissored = !full_redraw && scissor.is_some();

        // ── Pass segmentation (RFC-0017 z-layers × RFC-0023 backdrops) ────────
        let totals = LayerMark {
            solid: u32::try_from(instances.len()).unwrap_or(u32::MAX),
            decorated: u32::try_from(decorated.len()).unwrap_or(u32::MAX),
            texture: u32::try_from(textures.len()).unwrap_or(u32::MAX),
            vector: u32::try_from(vectors.len()).unwrap_or(u32::MAX),
            text: u32::try_from(texts.len()).unwrap_or(u32::MAX),
            canvas: u32::try_from(canvas_shapes.len()).unwrap_or(u32::MAX),
            ripple: u32::try_from(ripples.len()).unwrap_or(u32::MAX),
            backdrop: u32::try_from(backdrops.len()).unwrap_or(u32::MAX),
        };
        let segments = compute_segments(layers, backdrop_marks, &totals);

        // ── Who owns what, for the rest of this frame ─────────────────────────
        //
        // Dev surfaces are always emitted last and always open their own
        // z-layer, so in every pool they are a *suffix*, and so, therefore,
        // are the segments that draw them. Two indices are all the attribution
        // below needs, and both degrade to "there are none" when the frame
        // carries no dev surfaces.
        let dev_text_start = dev_base.map_or(texts.len(), |b| {
            usize::try_from(b.text)
                .unwrap_or(usize::MAX)
                .min(texts.len())
        });
        let dev_segment_start = dev_base.map_or(segments.len(), |b| {
            segments
                .iter()
                .position(|s| segment_belongs_to(s, b))
                .unwrap_or(segments.len())
        });

        // ── Text prepare (before the render pass) ─────────────────────────────
        if should_draw {
            // RFC-0030 §I1 sub-scope: glyph shaping, atlas residency and the
            // text pipeline's own vertex staging. It is the term RFC-0032
            // exists to skip for clean subtrees, so it has to be readable on
            // its own rather than folded into the pass recording next to it.
            crate::profile_scope!("encode.glyphs");
            let viewport_dirty = self.viewport_dirty;
            // One glyph batch per pass segment (z-layer batches, further split
            // at backdrop barriers), shaping inside `prepare` stays global,
            // so this partition costs nothing beyond one extra small vertex
            // buffer per extra segment.
            let text_ranges: Vec<std::ops::Range<usize>> =
                segments.iter().map(|s| s.text.clone()).collect();
            self.text_pipeline.prepare(
                &self.device,
                &self.queue,
                texts,
                depths.text,
                self.scale_factor,
                viewport_dirty,
                clips.table,
                clips.text,
                clips.text_wrap,
                &text_ranges,
                dev_text_start,
            )?;
        }
        self.viewport_dirty = false;

        // ── Command encoding ──────────────────────────────────────────────────
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ByardCore - Frame Command Encoder"),
            });

        if should_draw {
            let mut backdrop_draw = BackdropDraw {
                pipes: &self.backdrop_pipelines,
                scratch: &mut self.blur_scratch,
                persistent: &self.persistent_color,
                format: self.surface_format,
                auto_capable: self.blur_auto_capable,
                backdrops,
            };
            draw_ui_pass(
                &mut encoder,
                &self.persistent_view,
                &self.persistent_depth_view,
                &self.device,
                &DrawPipelines {
                    solid: &self.render_pipeline,
                    clear: &self.clear_pipeline,
                    decorated: &self.decorated_pipeline,
                    texture: &self.texture_pipeline,
                    vector: &self.vector_pipeline,
                    canvas: &self.canvas_pipeline,
                    ripple: &self.ripple_pipeline,
                },
                &self.viewport_bind_group,
                &self.quad_buffer,
                &mut self.text_pipeline,
                full_redraw,
                scissor,
                (self.scale_factor, self.phys_w, self.phys_h),
                &DrawPrimitives {
                    instances,
                    decorated,
                    textures,
                    texture_cache: &self.texture_cache,
                    vectors,
                    vector_atlas: &self.vector_atlas,
                    canvas_shapes,
                    shape_records,
                    ripples,
                    solid_depths: depths.solid,
                    decorated_depths: depths.decorated,
                    texture_depths: depths.texture,
                    canvas_depths: depths.canvas,
                    clips,
                },
                &segments,
                dev_segment_start,
                &mut backdrop_draw,
                self.gpu_timer.as_ref(),
                &mut self.arena,
                &mut self.canvas_records,
                &mut self.staging,
                &self.queue,
            )?;
            // Only when a pass actually ran this frame, resolving an
            // untouched query set would read stale or never-written slots.
            // The matching `request_map` happens in `submit`, once this
            // encoder's command buffer has actually reached the queue.
            if let Some(timer) = &mut self.gpu_timer {
                timer.resolve_and_copy(&mut encoder);
                self.gpu_timing_pending = true;
            }
        }

        // ── Composite onto the swapchain image ────────────────────────────────
        //
        // Always a full, unscissored copy, every frame, regardless of
        // `should_draw`, see `persistent_color`'s doc comment for why the
        // swapchain's own previous contents can never be assumed valid.
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.persistent_color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.phys_w,
                height: self.phys_h,
                depth_or_array_layers: 1,
            },
        );

        // RFC-0030 §I1 sub-scope. Recording every primitive's bounds for next
        // frame's scissor is another whole-pool linear pass that used to sit
        // in `encode.frame`'s unexplained self-time, next to `encode.scissor`
        //, the two are a matched pair, one reading last frame's record and
        // one writing this frame's, and neither was visible.
        {
            crate::profile_scope!("encode.bookkeeping");
            update_frame_bookkeeping(
                self,
                instances,
                texts,
                decorated,
                textures,
                canvas_shapes,
                ripples,
                backdrops,
            );
        }

        // RFC-0030 §I1 sub-scope. `finish` is where `wgpu` validates and
        // assembles the whole command buffer, so it scales with how many
        // passes and draws the frame recorded, which is precisely what an
        // overlay with its own layer and its own blurred pane adds. It was the
        // single largest unexplained term in `encode.frame`.
        crate::profile_scope!("encode.finish");
        Ok(encoder.finish())
    }

    /// Encodes a frame from a [`RenderFrame`] published by the Relay.
    ///
    /// # Skipped frames
    ///
    /// The relay is latest-wins, so most published frames never reach here.
    /// That used to be handled at this end: [`RenderFrame::version`] carried a
    /// counter, a gap meant "a frame with dirty text was dropped", and the
    /// response was to force a full redraw with every line marked dirty.
    ///
    /// It is handled at the *other* end now. `Relay::publish` folds an
    /// unrendered frame's dirty bits into its replacement
    /// ([`RenderFrame::merge_dirty_from`]), so no dirty bit is ever lost and
    /// there is nothing here to compensate for. The difference is not
    /// cosmetic: a logic thread that outruns the display, i.e. every logic
    /// thread, skipped frames constantly, so the old rule fired on nearly
    /// every frame and handed back the entire benefit of RFC-0032's dirty set.
    ///
    /// # Errors
    ///
    /// Same error variants as [`encode_frame`](Self::encode_frame).
    pub fn encode_frame_from_relay(
        &mut self,
        target: &wgpu::Texture,
        frame: &RenderFrame,
    ) -> Result<wgpu::CommandBuffer, ByardError> {
        let cmd = self.encode_frame_with_decorations(
            target,
            frame.instances(),
            frame.texts(),
            frame.decorated(),
            frame.textures(),
            frame.vector_instances(),
            (frame.canvas_shapes(), frame.shape_records()),
            frame.ripples(),
            frame.atlas_uploads(),
            (frame.backdrops(), frame.backdrop_marks()),
            frame_draw_depths(frame),
            frame_clips(frame),
            FrameDirty {
                instances: frame.instances_dirty(),
                full: frame.wants_full_redraw(),
            },
            frame.layer_marks(),
            frame.dev_base(),
        )?;
        self.last_relay_version = frame.version();
        Ok(cmd)
    }

    /// Requests decode of every texture source in `textures` before the render
    /// pass (M29). With an I/O context the decode runs on the relay's pool and
    /// the upload happens later via [`apply_decoded`](Self::apply_decoded), so
    /// the render thread never blocks here (INV-12). A bare encoder with no
    /// relay falls back to a synchronous decode+upload, used only by
    /// GPU-readback tests, which never carry images, so this branch is a safety
    /// net rather than a hot path.
    fn request_textures(&mut self, textures: &[crate::frame::TextureSampler]) {
        if let Some(io) = &self.io {
            let handle = io.handle.clone();
            let tx = io.tx.clone();
            for t in textures {
                self.texture_cache.ensure(&handle, &tx, &t.src);
            }
        } else {
            for t in textures {
                let decoded = texture_sampler::DecodedImage {
                    src: t.src.clone(),
                    result: texture_sampler::decode_rgba(&t.src),
                };
                self.texture_cache.apply_decoded(
                    &self.device,
                    &self.queue,
                    &self.texture_bind_group_layout,
                    &self.image_sampler,
                    decoded,
                );
            }
        }
    }

    /// Installs the async-decode plumbing (M29): the relay's I/O runtime handle
    /// (decode tasks are spawned here) and the type-erased result sender (they
    /// report decoded pixels back through it). Called once by the engine after
    /// it has both a `Relay` and this encoder. Until set, image decode falls
    /// back to a synchronous path (see [`encode_frame_with_decorations`]).
    pub fn set_io_context(
        &mut self,
        handle: tokio::runtime::Handle,
        tx: texture_sampler::DecodeResultSender,
    ) {
        self.io = Some(IoContext { handle, tx });
    }

    /// Installs the channel this encoder reports applied vector-atlas-upload
    /// ids through (RFC-0009 §2-C), so the dev JIT cache stops re-sending an
    /// upload once it knows the render thread actually applied it.
    pub fn set_vector_ack_sender(&mut self, tx: crossbeam_channel::Sender<u64>) {
        self.vector_ack_tx = Some(tx);
    }

    /// Uploads one async decode result on the render thread (M29). Called by
    /// the engine for each [`DecodedImage`] drained from the relay's I/O
    /// channel, before encoding the next frame. The GPU upload is fast; the
    /// expensive decode already happened off-thread. Because every primitive is
    /// re-emitted dirty each tick, the newly-`Ready` texture repaints
    /// on the next frame without any extra dirty signal.
    pub fn apply_decoded(&mut self, decoded: DecodedImage) {
        self.texture_cache.apply_decoded(
            &self.device,
            &self.queue,
            &self.texture_bind_group_layout,
            &self.image_sampler,
            decoded,
        );
    }
}

/// Updates the structural-change bookkeeping consulted by
/// [`needs_full_redraw_this_frame`] and [`compute_scissor`] on the *next*
/// `encode_frame` call.
///
/// Extracted out of `encode_frame` purely to keep that function under
/// clippy's line-count threshold.
#[allow(clippy::too_many_arguments)]
fn update_frame_bookkeeping(
    state: &mut EncoderSubsystem,
    instances: &[BoxInstance],
    texts: &[TextLine],
    decorated: &[crate::frame::DecoratedBox],
    textures: &[crate::frame::TextureSampler],
    canvas_shapes: &[crate::frame::CanvasShape],
    ripples: &[RippleInstance],
    backdrops: &[BackdropInstance],
) {
    state.needs_full_redraw = false;
    state.last_instance_count = instances.len();
    state.last_text_count = texts.len();
    // Recomputed for every primitive (not just dirty ones), a clean
    // primitive's bounds are unchanged from last frame anyway, so this is a
    // no-op for it, and it keeps each `last_*_bounds` positionally aligned with
    // its slice without needing a separate "did this move" check.
    state.last_text_bounds = texts.iter().map(text_line_bounds).collect();
    state.last_box_bounds = instances.iter().map(|b| rect_of(b.rect)).collect();
    state.last_decorated_bounds = decorated.iter().map(|d| rect_of(d.base.rect)).collect();
    state.last_canvas_bounds = canvas_shapes
        .iter()
        .map(crate::frame::CanvasShape::bounds)
        .collect();
    state.last_ripple_bounds = ripples.iter().map(|r| rect_of(r.rect)).collect();
    state.last_backdrop_bounds = backdrops.iter().map(|b| rect_of(b.rect)).collect();
    state.last_texture_bounds = textures.iter().map(|t| rect_of(t.rect)).collect();
}

/// Draws the UI render pass: a scissored clear quad (incremental frames
/// only), every `SolidBox` instance, then every `TextLine`, see
/// `EncoderSubsystem::encode_frame`'s doc comment for the full three-case
/// behaviour this implements.
///
/// Extracted out of `encode_frame` purely to keep that function under
/// clippy's line-count threshold. Every parameter here is a field
/// `encode_frame` already owns or borrows, the long parameter list is
/// mechanical (one argument per resource the pass needs), not a sign of
/// fresh coupling between subsystems, so `too_many_arguments` is allowed
/// rather than worked around with an ad-hoc bundling struct.
/// The four UI pipelines `draw_ui_pass` needs, bundled to keep its argument
/// count within the lint threshold (mechanical grouping, not fresh coupling).
#[derive(Clone, Copy)]
struct DrawPipelines<'a> {
    solid: &'a wgpu::RenderPipeline,
    clear: &'a wgpu::RenderPipeline,
    decorated: &'a wgpu::RenderPipeline,
    texture: &'a wgpu::RenderPipeline,
    vector: &'a wgpu::RenderPipeline,
    canvas: &'a wgpu::RenderPipeline,
    ripple: &'a wgpu::RenderPipeline,
}

/// Draw-order depth slices for one frame (RFC-0011 cross-pass paint order),
/// parallel to the four primitive pools. Bundled so the encode entry points
/// stay within the argument-count lint. An empty slice means "no depth info"
/// and falls back to the far plane, reproducing the pre-depth pass order.
#[derive(Clone, Copy, Default)]
pub struct DrawDepths<'a> {
    /// Parallel to solid `BoxInstance`s.
    pub solid: &'a [f32],
    /// Parallel to `DecoratedBox`es.
    pub decorated: &'a [f32],
    /// Parallel to `TextureSampler`s.
    pub texture: &'a [f32],
    /// Parallel to `TextLine`s (applied via glyphon per-glyph metadata).
    pub text: &'a [f32],
    /// Parallel to `CanvasShape`s (RFC-0020).
    pub canvas: &'a [f32],
}

/// Everything one frame stages into the [`instance_arena::InstanceArena`]
/// before any render pass opens (RFC-0033 §G1).
///
/// Owned by the encoder and reused, so a steady-state frame reallocates none
/// of it, which is the point: an arena that removed nine GPU allocations by
/// adding nine CPU ones would not be an improvement.
#[derive(Default)]
pub(crate) struct FrameStaging {
    /// One entry per pass segment, in segment order.
    segments: Vec<SegmentStaging>,
    /// The incremental frame's clear quad: instance + depth regions.
    clear_quad: Option<(instance_arena::Region, instance_arena::Region)>,
    /// One reservation per backdrop in the pool (RFC-0033 §G2's alignment
    /// case), indexed by backdrop slot.
    backdrops: Vec<backdrop::BackdropRegions>,
    /// Reused conversion buffers, so the per-pipeline instance builds do not
    /// allocate either.
    scratch: StagingScratch,
}

/// The reusable conversion buffers, grouped so one segment's staging can take
/// them as a single `&mut` alongside a `&mut` into `FrameStaging::segments`,
/// which is what lets [`stage_segment`] be a free function shared by the app's
/// and the dev runner's halves of the staging loop.
#[derive(Default)]
pub(crate) struct StagingScratch {
    decorated: Vec<decorated_box::DecoratedInstance>,
    canvas: Vec<canvas_shape::CanvasShapeInstance>,
    depth: Vec<f32>,
}

/// One pass segment's staged regions.
#[derive(Default)]
struct SegmentStaging {
    solid: (instance_arena::Region, instance_arena::Region),
    decorated: instance_arena::Region,
    ripple: instance_arena::Region,
    canvas: instance_arena::Region,
    vector: instance_arena::Region,
    textures: Vec<texture_sampler::StagedImage>,
}

impl FrameStaging {
    /// Resets for a new frame while keeping every buffer's capacity.
    fn begin(&mut self, segment_count: usize) {
        self.clear_quad = None;
        self.backdrops.clear();
        // Grown, never shrunk, and each segment's own `Vec` is cleared in
        // place rather than dropped, the same reasoning as the arena's
        // grow-only policy, one layer up.
        while self.segments.len() < segment_count {
            self.segments.push(SegmentStaging::default());
        }
        for seg in &mut self.segments[..segment_count] {
            seg.solid = Default::default();
            seg.decorated = instance_arena::Region::default();
            seg.ripple = instance_arena::Region::default();
            seg.canvas = instance_arena::Region::default();
            seg.vector = instance_arena::Region::default();
            seg.textures.clear();
        }
    }
}

/// The per-primitive dirty bits a frame carries that do not fit on the
/// primitive itself (RFC-0032 §R3 step 6), today, solid boxes, because
/// [`BoxInstance`] is a GPU `Pod` vertex type with nowhere to put one.
///
/// [`Default`] is an **empty** slice, not an all-true one: a length that does
/// not match the instance pool is treated as "no information" by
/// [`encode_frame_with_decorations`](EncoderSubsystem::encode_frame_with_decorations),
/// which then falls back to redrawing everything. Erring towards over-draw is
/// the only safe direction here, the alternative is a frame that is quietly
/// missing pixels.
#[derive(Clone, Copy, Default)]
pub struct FrameDirty<'a> {
    /// Parallel to solid `BoxInstance`s.
    pub instances: &'a [bool],
    /// The frame asked for a full, unscissored redraw
    /// ([`RenderFrame::request_full_redraw`]).
    pub full: bool,
}

/// A frame's content-clip table plus the parallel per-pool clip slices
/// (RFC-0005 `ScrollView`), the [`DrawDepths`] analogue for clips.
#[derive(Clone, Copy, Default)]
pub struct FrameClips<'a> {
    /// The clip-rect table; a pool's `Option<u16>` indexes into this.
    pub table: &'a [crate::frame::ClipRect],
    /// Parallel to solid `BoxInstance`s.
    pub solid: &'a [Option<u16>],
    /// Parallel to `DecoratedBox`es.
    pub decorated: &'a [Option<u16>],
    /// Parallel to `TextureSampler`s.
    pub texture: &'a [Option<u16>],
    /// Parallel to `TextLine`s (applied via glyphon `TextBounds`).
    pub text: &'a [Option<u16>],
    /// Parallel to `VectorInstance`s.
    pub vector: &'a [Option<u16>],
    /// Parallel to `CanvasShape`s (RFC-0020).
    pub canvas: &'a [Option<u16>],
    /// Parallel to `RippleInstance`s (RFC-0023).
    pub ripple: &'a [Option<u16>],
    /// Parallel to `BackdropInstance`s (RFC-0023 §2).
    pub backdrop: &'a [Option<u16>],
    /// Per-`TextLine` wrap width in logical px (RFC-0018 text wrap); `Some(w)`
    /// shapes that line bounded to `w` so it wraps. Carried alongside the clip
    /// slices because it is another per-text-line parallel slice consumed by the
    /// same text `prepare` call.
    pub text_wrap: &'a [Option<f32>],
}

/// Bundles a frame's clip table and per-pool clip slices into a [`FrameClips`].
fn frame_clips(frame: &RenderFrame) -> FrameClips<'_> {
    FrameClips {
        table: frame.clips(),
        solid: frame.solid_clips(),
        decorated: frame.decorated_clips(),
        texture: frame.texture_clips(),
        text: frame.text_clips(),
        vector: frame.vector_clips(),
        canvas: frame.canvas_clips(),
        ripple: frame.ripple_clips(),
        backdrop: frame.backdrop_clips(),
        text_wrap: frame.text_wraps(),
    }
}

/// Bundles a frame's parallel draw-order depth slices into a [`DrawDepths`].
fn frame_draw_depths(frame: &RenderFrame) -> DrawDepths<'_> {
    DrawDepths {
        solid: frame.solid_depths(),
        decorated: frame.decorated_depths(),
        texture: frame.texture_depths(),
        text: frame.text_depths(),
        canvas: frame.canvas_depths(),
    }
}

/// The per-frame primitive lists `draw_ui_pass` draws, similarly bundled.
#[derive(Clone, Copy)]
struct DrawPrimitives<'a> {
    instances: &'a [BoxInstance],
    decorated: &'a [crate::frame::DecoratedBox],
    textures: &'a [crate::frame::TextureSampler],
    texture_cache: &'a texture_sampler::TextureCache,
    /// MSDF vector-glyph instances (RFC-0009 §1).
    vectors: &'a [VectorInstance],
    /// The MSDF atlas these `vectors` sample; not drawn without one.
    vector_atlas: &'a VectorAtlas,
    /// `Canvas` shape primitives (RFC-0020, the sixth pipeline).
    canvas_shapes: &'a [crate::frame::CanvasShape],
    /// This frame's shape-record pool (RFC-0031 §S4), staged once, whole,
    /// before any segment, because a group head's member index is global to
    /// the frame rather than to its segment.
    shape_records: &'a [crate::frame::ShapeRecord],
    /// Ripple ink reveals (RFC-0023, the seventh pipeline); depth is a field
    /// on `RippleInstance` itself (stamped by `RenderFrame::push_ripple`),
    /// like `vectors`.
    ripples: &'a [RippleInstance],
    /// Draw-order depths, parallel to `instances`/`decorated`/`textures`
    /// respectively (RFC-0011 cross-pass paint order). `texts` depth is applied
    /// inside `TextGlyphPipeline::prepare` via glyphon's per-glyph metadata;
    /// `vectors`' depth is a field on `VectorInstance` itself (stamped by
    /// `RenderFrame::push_vector`), not a parallel slice like these three.
    solid_depths: &'a [f32],
    decorated_depths: &'a [f32],
    texture_depths: &'a [f32],
    canvas_depths: &'a [f32],
    /// Content-clip table + per-pool clip slices (RFC-0005 `ScrollView`).
    clips: FrameClips<'a>,
}

/// One contiguous slice of the frame's draw stream, rendered inside a single
/// `wgpu` render pass: per-pool index ranges plus an optional RFC-0023 §2
/// backdrop barrier to honour after drawing it.
///
/// Segments are the product of two partitions of the same emission-ordered
/// stream: RFC-0017 z-layer boundaries (which never split the pass, they
/// only order batches within it) and RFC-0023 backdrop barriers (which *do*
/// split the pass, because the pane must sample everything drawn before it).
/// A frame with no layers and no backdrops is exactly one segment, the
/// classic single-pass draw stream, byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SegmentRanges {
    solid: std::ops::Range<usize>,
    decorated: std::ops::Range<usize>,
    texture: std::ops::Range<usize>,
    vector: std::ops::Range<usize>,
    text: std::ops::Range<usize>,
    canvas: std::ops::Range<usize>,
    ripple: std::ops::Range<usize>,
    /// `Some(b)`: after drawing this segment, end the pass, blur what has
    /// been rasterised so far for backdrop `b`, and composite `b` at the
    /// start of the next segment's pass.
    backdrop_after: Option<usize>,
}

/// The scope charged with a dev surface's own render pass, interned once.
///
/// Cached exactly as [`profile_scope!`](crate::profile_scope) caches its own:
/// the scope registry is a mutex, and a per-segment loop is not the place to
/// lock it.
static DEV_SEGMENT_SCOPE: std::sync::OnceLock<crate::telemetry::ScopeId> =
    std::sync::OnceLock::new();

/// Appends one pass segment's instance data to the arena (RFC-0033 §G1).
///
/// A free function rather than an inlined loop body so the app's segments and
/// the dev runner's can go through *identical* code under different
/// attribution. Nothing here knows about owners; the caller opens the
/// attribution, which is what makes it impossible for the two halves to drift.
fn stage_segment(
    seg: &SegmentRanges,
    out: &mut SegmentStaging,
    arena: &mut instance_arena::InstanceArena,
    scratch: &mut StagingScratch,
    primitives: &DrawPrimitives<'_>,
    // Where this frame's shape records landed in the arena, as an element
    // index (RFC-0031 §S4). Added to each group head's own member offset, so
    // the shader indexes the whole buffer without a dynamic offset.
    record_base: u32,
    clip_ctx: ClipCtx<'_>,
) {
    out.solid = stage_solid_box_instances(
        arena,
        &mut scratch.depth,
        &primitives.instances[seg.solid.clone()],
        sub_slice(primitives.solid_depths, &seg.solid),
    );
    out.decorated = decorated_box::stage(
        arena,
        &mut scratch.decorated,
        &primitives.decorated[seg.decorated.clone()],
        sub_slice(primitives.decorated_depths, &seg.decorated),
    );
    out.ripple = arena.push_vertex(&primitives.ripples[seg.ripple.clone()]);
    out.canvas = canvas_shape::stage(
        arena,
        &mut scratch.canvas,
        &primitives.canvas_shapes[seg.canvas.clone()],
        sub_slice(primitives.canvas_depths, &seg.canvas),
        record_base,
    );
    out.vector = arena.push_vertex(&primitives.vectors[seg.vector.clone()]);
    texture_sampler::stage(
        arena,
        &mut out.textures,
        primitives.texture_cache,
        &primitives.textures[seg.texture.clone()],
        sub_slice(primitives.texture_depths, &seg.texture),
        sub_slice(primitives.clips.texture, &seg.texture),
        clip_ctx,
    );
}

/// Whether every primitive this segment draws sits at or after `base` in its
/// own pool, i.e. whether the segment belongs entirely to the dev runner
/// (RFC-0030 erratum "self-accounting").
///
/// Dev surfaces open their own z-layer (RFC-0017) before emitting anything, so
/// a segment boundary always falls exactly on `base` and no segment is ever
/// half one owner's. A frosted pane inside a dev surface splits that layer
/// further, and both halves still start at or after `base`, which is why this
/// is a per-pool comparison rather than a layer-index one.
///
/// The conservative direction is deliberate: a segment that fails this test is
/// left with the app. Over-attributing to the dev runner would let a profiler
/// hide the app's cost inside its own row, which is the failure this whole
/// erratum exists to remove, in the opposite direction.
fn segment_belongs_to(seg: &SegmentRanges, base: LayerMark) -> bool {
    let at_or_after =
        |start: usize, cursor: u32| start >= usize::try_from(cursor).unwrap_or(usize::MAX);
    at_or_after(seg.solid.start, base.solid)
        && at_or_after(seg.decorated.start, base.decorated)
        && at_or_after(seg.texture.start, base.texture)
        && at_or_after(seg.vector.start, base.vector)
        && at_or_after(seg.text.start, base.text)
        && at_or_after(seg.canvas.start, base.canvas)
        && at_or_after(seg.ripple.start, base.ripple)
}

/// Field-wise clamp of a pool-cursor snapshot into `[lo, hi]`, the same
/// monotonic-degrade contract as the old per-pool range partitioning: a
/// decreasing or overshooting cursor (a logic-thread bug) collapses to an
/// empty sub-range on the render thread, never a panic or an out-of-bounds
/// slice.
fn mark_clamped(m: &LayerMark, lo: &LayerMark, hi: &LayerMark) -> LayerMark {
    LayerMark {
        solid: m.solid.clamp(lo.solid, hi.solid),
        decorated: m.decorated.clamp(lo.decorated, hi.decorated),
        texture: m.texture.clamp(lo.texture, hi.texture),
        vector: m.vector.clamp(lo.vector, hi.vector),
        text: m.text.clamp(lo.text, hi.text),
        canvas: m.canvas.clamp(lo.canvas, hi.canvas),
        ripple: m.ripple.clamp(lo.ripple, hi.ripple),
        backdrop: m.backdrop.clamp(lo.backdrop, hi.backdrop),
    }
}

/// The [`SegmentRanges`] between two clamped cursor snapshots.
fn ranges_between(a: &LayerMark, b: &LayerMark, backdrop_after: Option<usize>) -> SegmentRanges {
    SegmentRanges {
        solid: a.solid as usize..b.solid as usize,
        decorated: a.decorated as usize..b.decorated as usize,
        texture: a.texture as usize..b.texture as usize,
        vector: a.vector as usize..b.vector as usize,
        text: a.text as usize..b.text as usize,
        canvas: a.canvas as usize..b.canvas as usize,
        ripple: a.ripple as usize..b.ripple as usize,
        backdrop_after,
    }
}

/// Partitions the frame's draw stream into render-pass segments: one per
/// z-layer (RFC-0017), further split at every RFC-0023 backdrop barrier that
/// falls inside that layer. Pure and unit-testable without any GPU state,
/// per the project's CPU-mirror pattern. Always returns at least one segment
/// covering `0..totals` for every pool.
fn compute_segments(
    layers: &[LayerMark],
    backdrop_marks: &[LayerMark],
    totals: &LayerMark,
) -> Vec<SegmentRanges> {
    let mut segments = Vec::with_capacity(layers.len() + backdrop_marks.len() + 1);
    let mut cursor = LayerMark::default();
    for l in 0..=layers.len() {
        let end = mark_clamped(layers.get(l).unwrap_or(totals), &cursor, totals);
        // Backdrops emitted inside this layer, in emission order.
        let b_start = cursor.backdrop as usize;
        let b_end = (end.backdrop as usize).min(backdrop_marks.len());
        for (b, mark) in backdrop_marks.iter().enumerate().take(b_end).skip(b_start) {
            let mut mid = mark_clamped(mark, &cursor, &end);
            segments.push(ranges_between(&cursor, &mid, Some(b)));
            mid.backdrop = u32::try_from(b + 1).unwrap_or(u32::MAX);
            cursor = mid;
        }
        segments.push(ranges_between(&cursor, &end, None));
        cursor = end;
    }
    segments
}

/// Everything `draw_ui_pass` needs to honour RFC-0023 backdrop barriers,
/// bundled (with the mutable scratch cache) to stay within the
/// argument-count lint.
struct BackdropDraw<'a> {
    /// The blur + composite pipelines.
    pipes: &'a backdrop::BackdropPipelines,
    /// Per-slot scratch texture cache (mutable: recreated on size change).
    scratch: &'a mut backdrop::ScratchCache,
    /// The persistent colour target the regions are copied out of.
    persistent: &'a wgpu::Texture,
    /// Its pixel format (scratch textures match it).
    format: wgpu::TextureFormat,
    /// The startup GPU probe's answer for the `auto` quality tier.
    auto_capable: bool,
    /// The frame's backdrop pool.
    backdrops: &'a [BackdropInstance],
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn draw_ui_pass(
    encoder: &mut wgpu::CommandEncoder,
    persistent_view: &wgpu::TextureView,
    depth_view: &wgpu::TextureView,
    device: &wgpu::Device,
    pipelines: &DrawPipelines<'_>,
    viewport_bind_group: &wgpu::BindGroup,
    quad_buffer: &wgpu::Buffer,
    text_pipeline: &mut TextGlyphPipeline,
    full_redraw: bool,
    scissor: Option<(Rect, u32, u32, u32, u32)>,
    // (scale_factor, physical width, physical height), for clip→scissor math.
    dims: (f32, u32, u32),
    primitives: &DrawPrimitives<'_>,
    // The pass segmentation (z-layers × backdrop barriers) computed by
    // `encode_frame_with_decorations`, also the text batch partition.
    segments: &[SegmentRanges],
    // The first segment that belongs to the dev runner; `segments.len()` when
    // none do. Dev surfaces are a suffix of every pool, so this is a split
    // point rather than a per-segment predicate.
    dev_segment_start: usize,
    bd: &mut BackdropDraw<'_>,
    gpu_timer: Option<&GpuTimer>,
    arena: &mut instance_arena::InstanceArena,
    records: &mut CanvasRecordBinding,
    staging: &mut FrameStaging,
    queue: &wgpu::Queue,
) -> Result<(), ByardError> {
    // RFC-0030 §I1 sub-scope: render-pass recording and draw-call submission
    // for every segment. Its one `encode.buffers` child is the staging pass
    // below, since RFC-0033 there are no per-draw buffer creations left to
    // measure, only one arena append per pipeline and a single upload.
    crate::profile_scope!("encode.passes");
    let DrawPipelines {
        solid: render_pipeline,
        clear: clear_pipeline,
        decorated: decorated_pipeline,
        texture: texture_pipeline,
        vector: vector_pipeline,
        canvas: canvas_pipeline,
        ripple: ripple_pipeline,
    } = *pipelines;
    // Only what the *recording* half still reads directly. The pools and their
    // depth slices are consumed by `stage_segment`, which takes `primitives`
    // whole so the two owner-split staging loops share one body.
    let DrawPrimitives {
        textures,
        texture_cache,
        vector_atlas,
        clips,
        ..
    } = *primitives;
    // The base scissor every clipped draw intersects with: the dirty region on
    // an incremental frame, or the whole physical target on a full redraw.
    let (scale, phys_w, phys_h) = dims;
    let base_scissor: Scissor = match scissor {
        Some((_, x, y, w, h)) => (x, y, w, h),
        None => (0, 0, phys_w, phys_h),
    };
    let clip_ctx = ClipCtx {
        clips: clips.table,
        base: base_scissor,
        scale,
        phys_w,
        phys_h,
    };
    // ── Segmented draw (RFC-0017 z-layers × RFC-0023 backdrop barriers) ───────
    //
    // One iteration per segment. Without backdrops there is one segment per
    // z-layer and, critically, every segment after the first is only ever
    // *created* when a backdrop barrier or additional layer exists, so the
    // no-effects frame still renders inside batches of the very first pass:
    // the classic single-pass stream, byte for byte. A backdrop barrier ends
    // the pass (everything behind the pane is now rasterised), records the
    // region copy + blur passes, and the next segment's pass opens by
    // compositing the pane before drawing its own primitives. Within any
    // pass the shared depth buffer keeps resolving paint order exactly as
    // before; across passes the depth buffer is stored and re-loaded so
    // occlusion still spans the whole frame.
    // ── Staging (RFC-0033 §G1) ───────────────────────────────────────────────
    //
    // Every pipeline's instance data for every segment is appended to the one
    // arena, and uploaded in a single `write_buffer`, **before** the first
    // render pass opens. This ordering is a hard requirement rather than a
    // preference: `wgpu` binds a buffer range eagerly, and growing the arena
    // replaces the buffer, so a draw recorded before the upload could be
    // pointing at a buffer that no longer exists.
    {
        crate::profile_scope!("encode.buffers");
        arena.begin_frame();
        staging.begin(segments.len());
        // RFC-0031 §S4: the shape-record pool, staged whole and first. First
        // because every canvas instance's member index is relative to this
        // base, and whole because a group's members are frame-global, a head
        // in segment 3 may index records appended while walking segment 1.
        let record_base = arena.push_storage(primitives.shape_records).unwrap_or(0);
        if let Some((bounds, ..)) = scissor {
            staging.clear_quad = Some(stage_clear_quad(arena, bounds));
        }
        // Two halves, split at the dev boundary, running identical code, the
        // second under `Owner::DevTools`, so a dev overlay's instance staging
        // is charged to the dev runner instead of to the app's row. The guard
        // is entered once rather than per segment, and the shared body is a
        // free function so the two halves cannot drift apart.
        for (seg, out) in segments
            .iter()
            .zip(staging.segments.iter_mut())
            .take(dev_segment_start)
        {
            stage_segment(
                seg,
                out,
                arena,
                &mut staging.scratch,
                primitives,
                record_base,
                clip_ctx,
            );
        }
        if dev_segment_start < segments.len() {
            let _dev = crate::telemetry::attribute_to(crate::telemetry::Owner::DevTools);
            crate::profile_scope!("encode.buffers.dev");
            for (seg, out) in segments
                .iter()
                .zip(staging.segments.iter_mut())
                .skip(dev_segment_start)
            {
                stage_segment(
                    seg,
                    out,
                    arena,
                    &mut staging.scratch,
                    primitives,
                    record_base,
                    clip_ctx,
                );
            }
        }
        for _ in 0..bd.backdrops.len() {
            let regions = backdrop::reserve(arena);
            staging.backdrops.push(regions);
        }
        arena.upload(device, queue);
        // After the upload, because that is where a growth replaces the buffer
        // the bind group points at, and before the first pass opens, because
        // `wgpu` resolves a binding eagerly.
        records.refresh(device, arena);
    }

    let mut pending: Option<(usize, backdrop::PreparedBackdrop)> = None;
    let seg_count = segments.len();
    for (i, seg) in segments.iter().enumerate() {
        // A dev surface's segment records its own pass, including, for the
        // HUD, the copy/blur/composite of its frosted pane, which is by far
        // the largest thing it asks the GPU to do. Charging that to the app
        // was most of what §V4 was under-reporting after the glyph half was
        // fixed. The loop body is untouched: only who it is billed to moves.
        let _dev = (i >= dev_segment_start).then(|| {
            crate::telemetry::attributed_scope(
                *DEV_SEGMENT_SCOPE.get_or_init(|| crate::telemetry::scope_id("encode.passes.dev")),
                crate::telemetry::Owner::DevTools,
            )
        });
        let first = i == 0;
        let last = i + 1 == seg_count;
        // Composite prepared between the previous segment and this one; held
        // in a local so it outlives this pass's recording.
        let taken = pending.take();
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ByardCore - UI Render Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: persistent_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if first && full_redraw {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
                // wgpu 29: new field; None = standard 2-D rendering.
                depth_slice: None,
            })],
            // The draw-order depth buffer is frame-local scratch: cleared to
            // the far plane at the first pass of every frame, carried across
            // backdrop pass-splits (Store + Load) so occlusion spans the
            // whole frame, and discarded after the last segment.
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: if first {
                        wgpu::LoadOp::Clear(crate::frame::DRAW_DEPTH_CLEAR)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: if last {
                        wgpu::StoreOp::Discard
                    } else {
                        wgpu::StoreOp::Store
                    },
                }),
                stencil_ops: None,
            }),
            // The single GPU-timed scope brackets the first segment (the
            // whole frame when no backdrop splits the pass).
            timestamp_writes: if first {
                gpu_timer.and_then(|t| t.timestamp_writes(GPU_UI_PASS_SCOPE))
            } else {
                None
            },
            occlusion_query_set: None,
            // wgpu 28: new required field; None disables multiview rendering.
            multiview_mask: None,
        });

        // Restrict fragment writes to the dirty region on an incremental
        // frame, and wipe exactly that region first (see
        // `EncoderSubsystem::clear_pipeline`), first segment only: later
        // segments keep drawing into the already-cleared region (every draw
        // below re-establishes its own scissor via the clip runs).
        if first {
            if let (Some((_, x, y, w, h)), Some(regions)) = (scissor, staging.clear_quad) {
                render_pass.set_scissor_rect(x, y, w, h);
                draw_clear_quad(
                    &mut render_pass,
                    arena,
                    regions,
                    clear_pipeline,
                    viewport_bind_group,
                    quad_buffer,
                );
            }
        }

        // The frosted-glass pane whose barrier ended the previous segment:
        // its blurred sample is ready, composite it before this segment's
        // own primitives (its stamped depth keeps it below them anyway).
        if let Some((bidx, prep)) = taken.as_ref() {
            backdrop::draw_composite(
                &mut render_pass,
                arena,
                bd.pipes,
                viewport_bind_group,
                quad_buffer,
                prep,
                clips.backdrop.get(*bidx).copied().flatten(),
                clip_ctx,
            );
        }

        let staged = &staging.segments[i];
        let sr = &seg.solid;
        let dr = &seg.decorated;
        let tr = &seg.texture;
        let vr = &seg.vector;
        let xr = &seg.text;
        let cr = &seg.canvas;
        let rr = &seg.ripple;

        // Drawn on every call to this function, not just a full redraw, the
        // clear quad above can wipe a box's area on an incremental frame, so
        // boxes must be repainted afterwards or they would stay erased. The
        // active GPU scissor rect (set above, incremental frames only) bounds
        // which pixels this actually touches, so the cost is still
        // proportional to the dirty region, not the full instance list.
        if !sr.is_empty() {
            draw_solid_box_instances(
                &mut render_pass,
                arena,
                staged.solid,
                render_pipeline,
                viewport_bind_group,
                quad_buffer,
                sr.len(),
                sub_slice(clips.solid, sr),
                clip_ctx,
            );
        }

        // M21: decorated boxes (border/shadow/opacity), then textured images.
        // The order within a layer is unchanged; the shared depth buffer (each
        // primitive carrying its emission-order z) resolves visibility, so a
        // container's border no longer paints over a child that was emitted
        // after it, and text (below) no longer sits unconditionally on top.
        decorated_box::draw(
            &mut render_pass,
            arena,
            staged.decorated,
            decorated_pipeline,
            viewport_bind_group,
            quad_buffer,
            dr.len(),
            sub_slice(clips.decorated, dr),
            clip_ctx,
        );
        // RFC-0023: ripple ink reveals. Transparent geometry, its stamped
        // depth (between an element's background and its children) resolves
        // the compositing slot against the shared depth buffer, so draw
        // order within the layer doesn't matter.
        ripple::draw(
            &mut render_pass,
            arena,
            staged.ripple,
            ripple_pipeline,
            viewport_bind_group,
            quad_buffer,
            rr.len(),
            sub_slice(clips.ripple, rr),
            clip_ctx,
        );
        // RFC-0020: programmatic `Canvas` shapes (arcs/circles/lines/rects),
        // analytic SDF. Transparent geometry like the decorated pass, tests
        // the draw-order depth buffer, never writes it.
        canvas_shape::draw(
            &mut render_pass,
            arena,
            staged.canvas,
            canvas_pipeline,
            viewport_bind_group,
            &records.bind_group,
            quad_buffer,
            cr.len(),
            sub_slice(clips.canvas, cr),
            clip_ctx,
        );
        texture_sampler::draw(
            &mut render_pass,
            arena,
            &staged.textures,
            texture_pipeline,
            viewport_bind_group,
            quad_buffer,
            texture_cache,
            &textures[tr.clone()],
        );
        // RFC-0009 §1: crisp monochrome icons, sampled from the same MSDF
        // atlas the JIT/AOT paths upload to. Each instance carries its own
        // draw-order depth (RFC-0011), so paint order across pipelines is
        // honoured here too.
        vector_msdf::draw(
            &mut render_pass,
            arena,
            staged.vector,
            vector_pipeline,
            viewport_bind_group,
            quad_buffer,
            vector_atlas,
            vr.len(),
            sub_slice(clips.vector, vr),
            clip_ctx,
        );

        // Restore the base scissor before this layer's text: the pool draws
        // above left the GPU scissor at their last clip run, but text is
        // clipped per-line via glyphon's own `TextBounds` (set in `prepare`),
        // so its render must run under the full base region, not a stale
        // ScrollView run.
        {
            let (x, y, w, h) = base_scissor;
            if w > 0 && h > 0 {
                render_pass.set_scissor_rect(x, y, w, h);
            }
        }

        // This segment's glyph batch. `prepare` (called before any pass
        // began) saw the full, unfiltered `texts` slice partitioned by the
        // same segment ranges, its internal cache is positionally
        // index-aligned with that slice, so filtering to only the dirty
        // lines here would silently associate a non-dirty line's cached
        // glyph buffer with the wrong line. The scissor rect set above (on
        // incremental frames) is what actually limits which pixels this
        // call may write, not the slice contents.
        if !xr.is_empty() {
            text_pipeline.render_layer(&mut render_pass, i)?;
        }

        // A backdrop barrier: end this pass (dropping the recorder), record
        // the region copy + blur for the pane, and let the next segment's
        // pass composite it (RFC-0023 §2 steps 2–4).
        drop(render_pass);
        if let Some(b) = seg.backdrop_after {
            if let Some(instance) = bd.backdrops.get(b) {
                pending = backdrop::prepare(
                    encoder,
                    device,
                    queue,
                    arena,
                    staging.backdrops.get(b).copied().unwrap_or_default(),
                    bd.pipes,
                    bd.scratch,
                    b,
                    bd.persistent,
                    instance,
                    scale,
                    (phys_w, phys_h),
                    bd.format,
                    bd.auto_capable,
                )
                .map(|p| (b, p));
            }
        }
    }

    Ok(())
}

/// Clamped subslice: `slice[range]` where the range is first clamped to the
/// slice's bounds. The per-layer pool ranges are computed against the pool's
/// own length, but the parallel depth/clip slices may legitimately be shorter
/// (their contracts allow it, falling back to far-plane / unclipped), so this
/// keeps that leniency instead of panicking on the render thread.
fn sub_slice<'a, T>(slice: &'a [T], range: &std::ops::Range<usize>) -> &'a [T] {
    let start = range.start.min(slice.len());
    let end = range.end.clamp(start, slice.len());
    &slice[start..end]
}

/// Creates the persistent off-screen colour target and its view.
///
/// Shared by [`EncoderSubsystem::init`] and
/// [`EncoderSubsystem::update_viewport`] so both call sites build the
/// texture identically. `RENDER_ATTACHMENT` lets the UI render pass draw
/// into it; `COPY_SRC` lets [`EncoderSubsystem::encode_frame`] copy its
/// contents onto the swapchain image every frame.
fn create_persistent_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    // wgpu textures must be at least 1×1; a window can in principle report
    // a momentary zero-sized client area mid-resize.
    let width = width.max(1);
    let height = height.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ByardCore - Persistent UI Colour Target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Creates the frame-local draw-order depth target's view (RFC-0011). Sized to
/// match [`create_persistent_target`]; `RENDER_ATTACHMENT` only (never copied to
/// the swapchain, depth is scratch, discarded at the end of each pass).
fn create_depth_target(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let width = width.max(1);
    let height = height.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ByardCore - Draw-Order Depth Target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Deliberately generous (over-estimating) bounding box for a [`TextLine`],
/// in logical pixels.
///
/// `TextLine` exposes no measured glyph extents, the shaped
/// `glyphon::Buffer` is private to `TextGlyphPipeline`, so this estimates
/// width from `font_size` and character count using a generous per-character
/// advance and line-height multiplier. Over-estimating only wastes a little
/// scissor-rect area (a little extra fragment-write bandwidth);
/// under-estimating would visibly clip glyphs, which is a real correctness
/// bug. A measured-extent version is a natural follow-up once `TextLine` (or
/// the Atlas) carries real shaped-glyph bounds.
// A `TextLine` will never hold remotely enough characters (2^24 = 16M+) to
// make this cast lossy in practice; the line's logical-pixel width would
// exceed any real display by many orders of magnitude well before that.
#[allow(clippy::cast_precision_loss)]
fn text_line_bounds(line: &TextLine) -> Rect {
    text_line_bounds_wrapped(line, None)
}

/// [`text_line_bounds`] for a line that may wrap.
///
/// A wrapping `Text` occupies `wrap` pixels of width and *several* lines of
/// height, and estimating it as one line leaves the tail of every paragraph
/// outside the dirty union, stale glyphs below the first line whenever a
/// paragraph moves or is edited. Invisible while every primitive was dirty and
/// the union spanned the frame; a defect the moment the union is real.
///
/// The line-count estimate deliberately uses a **wider** per-character
/// estimate than the width estimate does. Over-counting lines costs a slightly
/// larger redraw; under-counting leaves pixels on screen that should not be
/// there, and this is not a place to be precise and occasionally wrong.
// Same rationale as `text_line_bounds`: a `TextLine` will never hold enough
// characters (2^24) for this cast to lose precision.
#[allow(clippy::cast_precision_loss)]
fn text_line_bounds_wrapped(line: &TextLine, wrap: Option<f32>) -> Rect {
    /// Generous enough to cover the widest glyphs in typical Latin text.
    const CHAR_WIDTH_FACTOR: f32 = 0.75;
    /// Deliberately above [`CHAR_WIDTH_FACTOR`]: a *narrower* assumed glyph
    /// would under-count lines, and a paragraph one line shorter than it
    /// really is leaves its last line outside the scissor.
    const LINE_COUNT_CHAR_WIDTH: f32 = 0.95;
    /// Generous enough to cover ascender + descender.
    const LINE_HEIGHT_FACTOR: f32 = 1.5;

    let char_count = line.text.chars().count() as f32;
    let natural_width = line.font_size * CHAR_WIDTH_FACTOR * char_count;
    let (width, lines) = match wrap {
        Some(w) if w > 0.0 => {
            let wide = line.font_size * LINE_COUNT_CHAR_WIDTH * char_count;
            (w.max(natural_width.min(w)), (wide / w).ceil().max(1.0))
        }
        _ => (natural_width, 1.0),
    };
    Rect::new(
        line.x,
        line.y,
        width,
        lines * line.font_size * LINE_HEIGHT_FACTOR,
    )
}

/// Computes the merged bounding box, RFC §3.3's "bounding box of the
/// affected region", over every **dirty** entry in `texts`, unioned with
/// that same line's bounds from the *previous* frame (`previous`,
/// positionally aligned with `texts`).
///
/// The previous-frame union matters because a line's bounds can shrink or
/// move between frames (e.g. a reactive label whose text gets shorter):
/// without it, the new (smaller) bounds would leave the line's old
/// footprint entirely outside the computed scissor rect, and that old
/// content would never be cleared.
///
/// Returns `None` when no entry is dirty, the caller's signal to skip the
/// incremental render pass entirely for this frame. Multiple simultaneously
/// dirty lines are merged into a single bounding box via repeated
/// [`Rect::union`] rather than issued as separate scissored sub-passes,
/// one scissor + draw call instead of N, at the cost of a marginally larger
/// over-draw region when the dirty lines are far apart on screen.
fn dirty_text_bounds(texts: &[TextLine], wraps: &[Option<f32>], previous: &[Rect]) -> Option<Rect> {
    texts
        .iter()
        .enumerate()
        .filter(|(_, line)| line.dirty)
        .map(|(i, line)| {
            let current = text_line_bounds_wrapped(line, wraps.get(i).copied().flatten());
            match previous.get(i) {
                Some(prev) => current.union(prev),
                None => current,
            }
        })
        .reduce(|acc, r| acc.union(&r))
}

/// Builds a [`Rect`] from a primitive's `[x, y, width, height]` paint rect.
const fn rect_of(rect: [f32; 4]) -> Rect {
    Rect::new(rect[0], rect[1], rect[2], rect[3])
}

/// The generalisation of [`dirty_text_bounds`] to any primitive type
/// (RFC-0001 §3.3): unions the current bounds of every **dirty** item with
/// that item's bounds from the previous frame (`previous`, positionally
/// aligned), so a shrunk or moved primitive still clears its old footprint.
///
/// `items` yields `(current_bounds, is_dirty)` in slice order. Returns `None`
/// when nothing is dirty.
fn union_dirty_rects(items: impl Iterator<Item = (Rect, bool)>, previous: &[Rect]) -> Option<Rect> {
    items
        .enumerate()
        .filter(|(_, (_, dirty))| *dirty)
        .map(|(i, (current, _))| match previous.get(i) {
            Some(prev) => current.union(prev),
            None => current,
        })
        .reduce(|acc, r| acc.union(&r))
}

/// `dirty_text_bounds` for the `SolidBox` pipeline (M26).
///
/// `dirty` is positionally aligned with `instances`. Each box's bounds are its
/// paint rect directly (a solid box's geometry *is* its bounds, unlike text
/// whose extents are heuristically estimated). See [`union_dirty_rects`] for
/// the previous-frame-union contract this shares with text.
fn dirty_box_bounds(instances: &[BoxInstance], dirty: &[bool], previous: &[Rect]) -> Option<Rect> {
    union_dirty_rects(
        instances
            .iter()
            .zip(dirty.iter())
            .map(|(b, d)| (rect_of(b.rect), *d)),
        previous,
    )
}

/// `dirty_text_bounds` for the `DecoratedBox` pipeline (M27); dirtiness comes
/// from each decoration's own [`DecoratedBox::dirty`](crate::frame::DecoratedBox::dirty)
/// bit and its bounds from its `base` rect.
fn dirty_decorated_bounds(
    decorated: &[crate::frame::DecoratedBox],
    previous: &[Rect],
) -> Option<Rect> {
    union_dirty_rects(
        decorated
            .iter()
            .map(|d| (decorated_paint_bounds(d), d.dirty)),
        previous,
    )
}

/// A decoration's **painted** bounds: its rect grown by the drop shadow's
/// offset, spread and blur.
///
/// A shadow paints outside the box it belongs to by construction, so unioning
/// `base.rect` alone leaves the shadow's outer half stale whenever the box
/// moves. Like the wrapped-text estimate above, this only became reachable
/// once the dirty union stopped covering the whole frame.
fn decorated_paint_bounds(d: &crate::frame::DecoratedBox) -> Rect {
    let rect = rect_of(d.base.rect);
    if d.shadow_color[3] <= 0.0 {
        return rect;
    }
    let reach = d.shadow_blur.max(0.0) + d.shadow_spread.max(0.0);
    let x0 = (rect.x + d.shadow_dx.min(0.0)) - reach;
    let y0 = (rect.y + d.shadow_dy.min(0.0)) - reach;
    let x1 = (rect.x + rect.width + d.shadow_dx.max(0.0)) + reach;
    let y1 = (rect.y + rect.height + d.shadow_dy.max(0.0)) + reach;
    Rect::new(x0, y0, x1 - x0, y1 - y0).union(&rect)
}

/// `dirty_text_bounds` for the `CanvasShape` pipeline (RFC-0020); dirtiness
/// comes from each shape's own
/// [`CanvasShape::dirty`](crate::frame::CanvasShape::dirty) bit and its bounds
/// from [`CanvasShape::bounds`](crate::frame::CanvasShape::bounds) (stroke and
/// caps included), so an animated arc repaints without a full redraw.
fn dirty_canvas_bounds(shapes: &[crate::frame::CanvasShape], previous: &[Rect]) -> Option<Rect> {
    union_dirty_rects(shapes.iter().map(|s| (s.bounds(), s.dirty)), previous)
}

/// `dirty_text_bounds` for the `Ripple` pipeline (RFC-0023). A live ripple
/// animates every frame by definition (its radius/alpha are re-sampled each
/// tick), so every instance is treated dirty, like solids, and the
/// previous-frame bounds keep the element repainting on the frame *after* the
/// last ripple fades, erasing its final ink.
fn dirty_ripple_bounds(ripples: &[RippleInstance], previous: &[Rect]) -> Option<Rect> {
    union_dirty_rects(ripples.iter().map(|r| (rect_of(r.rect), true)), previous)
}

/// `dirty_text_bounds` for the `Backdrop` pipeline (RFC-0023 §2). A pane
/// re-samples whatever is behind it on every drawn frame, so it is treated
/// always-dirty like solids: whenever *anything* in the frame changed, the
/// pane's region joins the union and it re-blurs, and the previous-frame
/// bounds erase a pane that shrank or unmounted.
fn dirty_backdrop_bounds(backdrops: &[BackdropInstance], previous: &[Rect]) -> Option<Rect> {
    union_dirty_rects(backdrops.iter().map(|b| (rect_of(b.rect), true)), previous)
}

/// `dirty_text_bounds` for the `TextureSampler` pipeline (M27); dirtiness comes
/// from each sampler's own [`TextureSampler::dirty`](crate::frame::TextureSampler::dirty)
/// bit.
fn dirty_texture_bounds(
    textures: &[crate::frame::TextureSampler],
    previous: &[Rect],
) -> Option<Rect> {
    union_dirty_rects(
        textures.iter().map(|t| (rect_of(t.rect), t.dirty)),
        previous,
    )
}

/// Unions two optional rects: `Some ∪ Some` merges, `Some ∪ None` (either
/// order) passes the `Some` through, `None ∪ None` is `None`.
fn union_opt(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.union(&y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Every per-primitive dirty input [`compute_scissor`] unions into one
/// scissor rect, bundled so the call site stays under the argument-count lint
/// and reads as one coherent "what changed this frame" snapshot. Each
/// `prev_*` slice is positionally aligned with its primitive slice (the
/// previous frame's bounds) for the shrink/move-safety contract in
/// [`union_dirty_rects`].
struct ScissorInputs<'a> {
    texts: &'a [TextLine],
    /// Per-line wrap width, parallel to `texts`, a wrapped paragraph is
    /// several lines tall and its bounds must say so.
    text_wrap: &'a [Option<f32>],
    prev_texts: &'a [Rect],
    instances: &'a [BoxInstance],
    instances_dirty: &'a [bool],
    prev_boxes: &'a [Rect],
    decorated: &'a [crate::frame::DecoratedBox],
    prev_decorated: &'a [Rect],
    textures: &'a [crate::frame::TextureSampler],
    prev_textures: &'a [Rect],
    canvas_shapes: &'a [crate::frame::CanvasShape],
    prev_canvas: &'a [Rect],
    ripples: &'a [RippleInstance],
    prev_ripples: &'a [Rect],
    backdrops: &'a [BackdropInstance],
    prev_backdrops: &'a [Rect],
}

#[cfg(test)]
impl<'a> ScissorInputs<'a> {
    /// Builds inputs that carry only text (no boxes/decorations/textures),
    /// the original text-only `compute_scissor` shape, kept so the text-path
    /// unit tests read unchanged.
    fn text_only(texts: &'a [TextLine], prev_texts: &'a [Rect]) -> Self {
        Self {
            texts,
            text_wrap: &[],
            prev_texts,
            instances: &[],
            instances_dirty: &[],
            prev_boxes: &[],
            decorated: &[],
            prev_decorated: &[],
            textures: &[],
            prev_textures: &[],
            canvas_shapes: &[],
            ripples: &[],
            prev_ripples: &[],
            backdrops: &[],
            prev_backdrops: &[],
            prev_canvas: &[],
        }
    }
}

/// A physical-pixel scissor tuple `(x, y, w, h)` as
/// `wgpu::RenderPass::set_scissor_rect` expects.
pub(crate) type Scissor = (u32, u32, u32, u32);

/// The per-frame context a clipped draw needs (RFC-0005 `ScrollView`): the clip
/// table, the frame's base scissor (the dirty region on an incremental frame,
/// or the whole target on a full one), and the physical-pixel conversion. Small
/// and `Copy` so it threads cheaply into every pool draw.
#[derive(Clone, Copy)]
pub struct ClipCtx<'a> {
    clips: &'a [crate::frame::ClipRect],
    base: Scissor,
    scale: f32,
    phys_w: u32,
    phys_h: u32,
}

/// Axis-aligned intersection of two physical scissor rects; `None` if empty
/// (wgpu rejects a zero-size scissor).
fn intersect_scissor(a: Scissor, b: Scissor) -> Option<Scissor> {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// The physical scissor for a primitive with clip index `clip`: the frame's
/// base scissor when unclipped, else that base intersected with the clip's
/// viewport. `None` means fully clipped away (draw nothing).
pub(crate) fn clip_scissor(ctx: ClipCtx<'_>, clip: Option<u16>) -> Option<Scissor> {
    match clip {
        None => Some(ctx.base),
        Some(idx) => ctx.clips.get(idx as usize).and_then(|c| {
            let cr = logical_rect_to_physical_scissor(c.rect, ctx.scale, ctx.phys_w, ctx.phys_h);
            intersect_scissor(ctx.base, cr)
        }),
    }
}

/// Draws `count` instances grouped by content clip (RFC-0005 `ScrollView`).
/// Walks maximal runs of equal clip in `clip_slice`, which is contiguous
/// because emission is tree-order, and for each run sets the scissor to
/// `clip ∩ base` (physical) and invokes `draw_range(pass, start, end)`. A run
/// whose effective scissor is empty is skipped entirely: an off-screen scroll
/// row costs zero fragments (a bonus cull on top of emission culling). Callers
/// bind the pipeline/buffers once, before this.
pub(crate) fn for_each_clip_run(
    pass: &mut wgpu::RenderPass<'_>,
    count: usize,
    clip_slice: &[Option<u16>],
    ctx: ClipCtx<'_>,
    mut draw_range: impl FnMut(&mut wgpu::RenderPass<'_>, u32, u32),
) {
    let clip_at = |i: usize| clip_slice.get(i).copied().flatten();
    let mut i = 0;
    while i < count {
        let cur = clip_at(i);
        let mut j = i + 1;
        while j < count && clip_at(j) == cur {
            j += 1;
        }
        if let Some((x, y, w, h)) = clip_scissor(ctx, cur) {
            #[allow(clippy::cast_possible_truncation)]
            {
                pass.set_scissor_rect(x, y, w, h);
                draw_range(pass, i as u32, j as u32);
            }
        }
        i = j;
    }
}

/// Unions every dirty primitive's bounds (text + solid boxes + decorations +
/// textures, RFC-0001 §3.3) and converts the result into the single
/// [`EncoderSubsystem::encode_frame`] needs to scissor an incremental frame:
/// the logical bounds (needed to size the clear quad) alongside the physical
/// `(x, y, width, height)` tuple `wgpu::RenderPass::set_scissor_rect` expects.
///
/// Pure and unit-testable independent of any `wgpu` state, following the
/// project's established pattern of extracting CPU-mirror decision logic
/// into free functions (see `text_glyph::needs_reshape`). Returns `None`
/// both when nothing is dirty and when the dirty bounds degenerate to a
/// zero-size physical rect (wgpu rejects a zero-size scissor rect).
/// How far, in logical pixels, a primitive's paint can fall outside its own
/// rect.
///
/// Every rounded/analytic pipeline in this encoder antialiases its edge with a
/// smoothstep about half a pixel wide, so a box's paint bleeds *just* past its
/// geometric rect. While every primitive was emitted dirty this never showed:
/// the union spanned the whole frame and there was no boundary to fall off.
/// With a real dirty union, a scissor cut exactly at the rect clips the
/// antialiased fringe and leaves a one-pixel halo of the previous frame, a
/// defect that is visible, is not the interpreter's fault, and is not
/// something a caller can compensate for.
///
/// Two pixels rather than one: the fringe is under a pixel wide in logical
/// space, and rounding to physical pixels can move the boundary by another.
/// The cost of being generous here is a marginally larger redraw region; the
/// cost of being exact and wrong is a stale outline around everything that
/// moves.
const AA_MARGIN_PX: f32 = 2.0;

/// Grows `rect` by `margin` on every side.
fn inflate(rect: Rect, margin: f32) -> Rect {
    Rect::new(
        rect.x - margin,
        rect.y - margin,
        rect.width + margin * 2.0,
        rect.height + margin * 2.0,
    )
}

fn compute_scissor(
    inputs: &ScissorInputs<'_>,
    scale: f32,
    max_w: u32,
    max_h: u32,
) -> Option<(Rect, u32, u32, u32, u32)> {
    let bounds = union_opt(
        union_opt(
            union_opt(
                dirty_text_bounds(inputs.texts, inputs.text_wrap, inputs.prev_texts),
                dirty_box_bounds(inputs.instances, inputs.instances_dirty, inputs.prev_boxes),
            ),
            union_opt(
                dirty_decorated_bounds(inputs.decorated, inputs.prev_decorated),
                dirty_texture_bounds(inputs.textures, inputs.prev_textures),
            ),
        ),
        union_opt(
            dirty_canvas_bounds(inputs.canvas_shapes, inputs.prev_canvas),
            union_opt(
                dirty_ripple_bounds(inputs.ripples, inputs.prev_ripples),
                dirty_backdrop_bounds(inputs.backdrops, inputs.prev_backdrops),
            ),
        ),
    )?;
    let bounds = inflate(bounds, AA_MARGIN_PX);
    let (x, y, w, h) = logical_rect_to_physical_scissor(bounds, scale, max_w, max_h);
    if w > 0 && h > 0 {
        Some((bounds, x, y, w, h))
    } else {
        None
    }
}

/// Converts a logical-pixel `Rect` into the `(x, y, width, height)` tuple
/// expected by `wgpu::RenderPass::set_scissor_rect`, in physical pixels,
/// clamped to `[0, max_w] × [0, max_h]`.
///
/// wgpu validates that a scissor rect lies entirely within the render
/// target's bounds, a rect computed from logical coordinates can overshoot
/// the physical target by a few pixels from rounding (`x * scale` truncation
/// at the high end), so clamping here is required, not defensive cruft.
// `max_w_f`/`max_h_f` are intentionally parallel names for parallel
// quantities (the f32 form of `max_w`/`max_h`, used only for the `.min`
// clamp below), not a real ambiguity risk. The u32 → f32 cast is lossless
// in practice: a physical surface dimension exceeding 2^24px (16M+) does
// not exist on any real display.
#[allow(clippy::similar_names, clippy::cast_precision_loss)]
fn logical_rect_to_physical_scissor(
    rect: Rect,
    scale: f32,
    max_w: u32,
    max_h: u32,
) -> (u32, u32, u32, u32) {
    let x0 = (rect.x * scale).floor().max(0.0);
    let y0 = (rect.y * scale).floor().max(0.0);
    let x1 = ((rect.x + rect.width) * scale).ceil().max(x0);
    let y1 = ((rect.y + rect.height) * scale).ceil().max(y0);

    let max_w_f = max_w as f32;
    let max_h_f = max_h as f32;

    let x0 = x0.min(max_w_f);
    let y0 = y0.min(max_h_f);
    let x1 = x1.min(max_w_f);
    let y1 = y1.min(max_h_f);

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
}

/// Decides whether [`EncoderSubsystem::encode_frame`] must perform a full
/// redraw this frame.
///
/// Pure and unit-testable independent of any `wgpu` state, following the
/// project's established pattern of extracting CPU-mirror decision logic
/// into free functions (see `text_glyph::needs_reshape`).
///
/// A full redraw is forced by `sticky` (set on construction, after a resize,
/// see [`EncoderSubsystem::needs_full_redraw`], and when the frame itself
/// asked for one via [`RenderFrame::request_full_redraw`]) OR by a structural
/// change in the instance/text counts since the previous frame, since neither
/// `BoxInstance` nor `TextLine` carries an "added this frame" bit.
fn needs_full_redraw_this_frame(
    sticky: bool,
    prev_instance_count: usize,
    instance_count: usize,
    prev_text_count: usize,
    text_count: usize,
) -> bool {
    sticky || prev_instance_count != instance_count || prev_text_count != text_count
}

/// Compiles the WGSL shader and assembles a `SolidBox`-shaped render
/// pipeline, parameterised by `blend` so the same shader and vertex layout
/// can back both [`EncoderSubsystem::render_pipeline`] (alpha-blended) and
/// [`EncoderSubsystem::clear_pipeline`] (`blend: None`, unconditional
/// replace).
///
/// Separated from [`EncoderSubsystem::init`] to keep that function under the
/// 100-line lint threshold.
///
/// Per RFC §8, the full creation sequence, `create_pipeline_layout`,
/// `create_shader_module`, and `create_render_pipeline`, is wrapped inside a
/// single `push_error_scope` / `pop_error_scope` pair so that any GPU-side
/// validation failure is captured and returned as
/// [`ByardError::PipelineCompilation`].
/// Builds the M21 `DecoratedBox` and `TextureSampler` pipelines plus the texture
/// bind-group layout and shared sampler. Extracted from
/// [`EncoderSubsystem::init`] to keep that function under the line-count lint.
async fn build_m21_pipelines(
    device: &wgpu::Device,
    viewport_layout: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
) -> Result<
    (
        wgpu::RenderPipeline,
        wgpu::RenderPipeline,
        wgpu::BindGroupLayout,
        wgpu::Sampler,
    ),
    ByardError,
> {
    let quad = || wgpu::VertexBufferLayout {
        array_stride: 8,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x2,
        }],
    };

    // RFC-0017: the decorated pass is transparent geometry (shadows, borders,
    // translucent fills), so it *tests* draw-order depth but never *writes* it,
    // otherwise a translucent box or a shadow halo would cull the app text drawn
    // beneath it in the later text pass. Only opaque passes write depth.
    let decorated_pipeline = decorated_box::build_pipeline(
        device,
        viewport_layout,
        quad(),
        surface_format,
        draw_depth_stencil_no_write(),
    )
    .await?;

    let texture_bind_group_layout = texture_sampler::bind_group_layout(device);
    let image_sampler = texture_sampler::sampler(device);
    let texture_pipeline = texture_sampler::build_pipeline(
        device,
        viewport_layout,
        &texture_bind_group_layout,
        quad(),
        surface_format,
    )
    .await?;

    Ok((
        decorated_pipeline,
        texture_pipeline,
        texture_bind_group_layout,
        image_sampler,
    ))
}

/// Depth-buffer format used to resolve draw order across the four UI pipelines.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Depth-stencil state for the *drawing* pipelines (solid/decorated/texture/
/// vector/text): write each primitive's draw-order z and keep the nearest,
/// i.e. the later-emitted, fragment (`LessEqual`, since later = smaller z,
/// buffer cleared to the far plane). `pub(crate)` so `vector_msdf` (a sibling
/// submodule) shares it instead of duplicating the state.
pub(crate) fn draw_depth_stencil() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Depth-stencil state for the **transparent** geometry pass, the whole
/// `DecoratedBox` pipeline (shadows, borders, translucent fills; RFC-0017). It
/// still *tests* draw-order depth (`LessEqual`, so a nearer opaque surface
/// occludes a border or a scrim) but does **not** *write* it. Only opaque
/// geometry (solids/textures/vectors) writes depth; a transparent primitive that
/// wrote its nearer depth would cull every earlier-emitted primitive drawn in a
/// later pass, most visibly all app text beneath a modal scrim or a shadow's
/// halo, which would simply vanish. This is the standard opaque/transparent
/// split. `pub(crate)` so `decorated_box` builds its pipeline from it.
pub(crate) fn draw_depth_stencil_no_write() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Depth-stencil state for the clear pipeline: never tests or writes depth
/// (`Always` + no write), so wiping colour in the incremental scissor region
/// leaves the draw-order depth buffer (already cleared this frame) untouched.
fn clear_depth_stencil() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

async fn build_solid_box_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    quad_layout: wgpu::VertexBufferLayout<'static>,
    surface_format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    depth_stencil: wgpu::DepthStencilState,
    debug_name: &str,
) -> Result<wgpu::RenderPipeline, ByardError> {
    // --- GPU VALIDATION ERROR SCOPE (RFC §8) ---
    // Covers create_pipeline_layout + create_shader_module + create_render_pipeline,
    // the three operations listed in RFC §8 as requiring capture.
    // wgpu 28+: push_error_scope returns an owned scope handle; pop is on the handle.
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ByardCore - SolidBox Pipeline Layout"),
        // wgpu 29: bind_group_layouts is now &[Option<&BindGroupLayout>].
        bind_group_layouts: &[Some(bind_group_layout)],
        // wgpu 28: push_constant_ranges removed; replaced by immediate_size: u32.
        immediate_size: 0,
    });

    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ByardCore - SolidBox WGSL Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
            "solid_box.wgsl"
        ))),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(debug_name),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader_module,
            entry_point: Some("vs_main"),
            buffers: &[quad_layout, BoxInstance::layout(), solid_depth_layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader_module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend,
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
            pipeline: debug_name.to_string(),
            reason: error.to_string(),
        });
    }

    Ok(pipeline)
}

/// Stages the clear quad's instance and depth (RFC-0033 §G4: the "depth"
/// buffers are ordinary per-instance vertex data, not depth attachments, so
/// they join the same arena as another region).
fn stage_clear_quad(
    arena: &mut instance_arena::InstanceArena,
    bounds: Rect,
) -> (instance_arena::Region, instance_arena::Region) {
    let clear_instance = BoxInstance {
        rect: [bounds.x, bounds.y, bounds.width, bounds.height],
        color: [0.0, 0.0, 0.0, 0.0],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    };
    let instance = arena.push_vertex(std::slice::from_ref(&clear_instance));
    // The clear pipeline shares `solid_box.wgsl`, which reads a depth at
    // location 9, so the clear draw must still supply the buffer. The value is
    // irrelevant: the clear pipeline runs with depth-write disabled and an
    // `Always` compare (see `build_solid_box_pipeline`), so it never touches
    // the depth buffer, it only wipes colour in the scissor region.
    let depth = arena.push_vertex(std::slice::from_ref(&crate::frame::DRAW_DEPTH_CLEAR));
    (instance, depth)
}

/// Draws a single fully transparent quad covering the staged bounds using
/// `pipeline`'s no-blend state, so the fragment shader's output
/// unconditionally **replaces** the destination instead of blending with
/// it, see [`EncoderSubsystem::clear_pipeline`]'s doc comment for why this
/// is required before an incremental redraw can erase stale content.
///
/// Must be called while `render_pass`'s active scissor rect already
/// restricts writes to (at most) those bounds, otherwise this would wipe
/// unrelated content outside the dirty region.
fn draw_clear_quad(
    render_pass: &mut wgpu::RenderPass<'_>,
    arena: &instance_arena::InstanceArena,
    regions: (instance_arena::Region, instance_arena::Region),
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    quad_buffer: &wgpu::Buffer,
) {
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, bind_group, &[]);
    render_pass.set_vertex_buffer(0, quad_buffer.slice(..));
    render_pass.set_vertex_buffer(1, arena.slice(regions.0));
    render_pass.set_vertex_buffer(2, arena.slice(regions.1));
    render_pass.draw(0..4, 0..1);
}

/// Stages one segment's solid boxes and their parallel draw-order depths.
///
/// The depths are fed as a second per-instance vertex buffer (shader location
/// 9), which keeps `BoxInstance`'s `Pod` layout untouched, and are padded to
/// the instance count with the far plane so a length mismatch can never index
/// out of range on the GPU.
fn stage_solid_box_instances(
    arena: &mut instance_arena::InstanceArena,
    depth_scratch: &mut Vec<f32>,
    instances: &[BoxInstance],
    depths: &[f32],
) -> (instance_arena::Region, instance_arena::Region) {
    if instances.is_empty() {
        return (
            instance_arena::Region::default(),
            instance_arena::Region::default(),
        );
    }
    let instance_region = arena.push_vertex(instances);
    depth_scratch.clear();
    depth_scratch.extend_from_slice(depths);
    depth_scratch.resize(instances.len(), crate::frame::DRAW_DEPTH_CLEAR);
    let depth_region = arena.push_vertex(depth_scratch);
    (instance_region, depth_region)
}

/// Draws every `BoxInstance` staged for this segment using `pipeline`'s
/// alpha-blended state.
///
/// On an incremental frame, the caller's active GPU scissor rect (not this
/// function) is what actually bounds the pixels touched here, so calling this
/// unconditionally on every `should_draw` frame is still proportional to the
/// dirty region's bandwidth, not the full instance list's.
#[allow(clippy::too_many_arguments)]
fn draw_solid_box_instances(
    render_pass: &mut wgpu::RenderPass<'_>,
    arena: &instance_arena::InstanceArena,
    regions: (instance_arena::Region, instance_arena::Region),
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    quad_buffer: &wgpu::Buffer,
    count: usize,
    clip_slice: &[Option<u16>],
    ctx: ClipCtx<'_>,
) {
    if regions.0.is_empty() {
        return;
    }
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, bind_group, &[]);
    render_pass.set_vertex_buffer(0, quad_buffer.slice(..));
    render_pass.set_vertex_buffer(1, arena.slice(regions.0));
    render_pass.set_vertex_buffer(2, arena.slice(regions.1));
    // Draw in content-clip runs (RFC-0005), each scissored to its viewport.
    for_each_clip_run(render_pass, count, clip_slice, ctx, |p, s, e| {
        p.draw(0..4, s..e);
    });
}

/// The unit quad every instanced pipeline expands, at shader location 0.
///
/// One definition rather than one per call site, because location 0 being the
/// quad's is a fact about the whole encoder: every instance layout in this
/// directory starts counting at 1 for that reason, and the portability check
/// in `tests.rs` measures a pipeline's attribute budget starting here.
fn quad_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![0 => Float32x2];
    wgpu::VertexBufferLayout {
        array_stride: 8, // 2 × f32
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: ATTRS,
    }
}

/// Vertex buffer layout for the `SolidBox` pipeline's parallel draw-order depth
/// buffer (a lone `f32` per instance at shader location 9).
fn solid_depth_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![9 => Float32];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<f32>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: ATTRS,
    }
}

#[cfg(test)]
mod tests;

/// `encode.submit` lives on `submit`, which is `pub(crate)`, reachable from
/// `Engine` but not from an integration test, so its INV-18 assertion has to
/// live in-crate. Everything else about the encode breakdown is covered by
/// `tests/instrumentation.rs`.
#[cfg(all(test, feature = "telemetry"))]
mod submit_scope_tests;
