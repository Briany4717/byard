//! # Engine
//!
//! Top-level orchestrator that binds the Relay, Encoder, and `wgpu` surface.
//!
//! [`Engine`] is the public entry point for platform code. The platform
//! creates one `Engine` per window, calls [`Engine::start_logic`] once to
//! spawn the logic thread, notifies it of resize events via
//! [`Engine::on_resize`], and drives it frame-by-frame via
//! [`Engine::render_latest`]. The engine never imports windowing primitives
//! (`winit`, `raw-window-handle`, etc.), surface creation and window
//! lifecycle are entirely the platform's responsibility (RFC-0001 §6).
//!
//! ## Coordinate system
//!
//! All instance coordinates (rect, radii, etc.) and the viewport uniform are
//! in **logical pixels**, the same density-independent unit used by CSS,
//! `SwiftUI` points, and Android dp. On `HiDPI` displays (Retina 2×, etc.) the
//! platform must supply the OS-reported `scale_factor` so that the engine can
//! internally convert physical pixels → logical pixels for the viewport
//! uniform, while keeping the `wgpu` surface in physical pixels as required
//! by the API.
//!
//! ## Concurrency model (RFC-0001 §5)
//!
//! ```text
//!   Platform (winit / etc.)                    byard-core
//!   ──────────────────────                     ──────────────────────────────
//!   on_resume                              ──► Engine::init + Engine::start_logic
//!   resize / DPI change                    ──► Engine::on_resize
//!   RedrawRequested                        ──► Engine::render_latest
//!                                                  └─ Relay::current (lock-free)
//!                                                  └─ EncoderSubsystem::encode_frame_from_relay
//!                                                  └─ queue.submit + surface.present
//!   [logic thread]                             acquire_recycled → tick → Relay::publish
//! ```

use std::sync::Arc;
use std::thread::JoinHandle;

use crate::ByardError;
use crate::InputEvent;
use crate::atlas::{LayoutAtlas, LeafSize};
use crate::encoder::{EncoderSubsystem, TargetKey};
use crate::evaluator::{EvaluatorTick, Signal, ViewArena};
use crate::frame::{BoxInstance, TargetId, TargetKind, TextLine, Viewport};
use crate::relay::Relay;
use crate::surface::SurfaceId;

/// The limits Byard asks a GPU device for.
///
/// The adapter's own limits, with one exception: the vertex-attribute budget is
/// held down to the 16 the WebGPU specification guarantees everywhere, even on
/// an adapter that offers more.
///
/// Asking for everything the machine happens to offer sounds generous and is
/// actually a way of not finding out about a portability bug until someone
/// else's machine finds it. A Metal adapter offers 31 vertex attributes, a
/// Linux Vulkan or GL one exactly 16, so a pipeline that overruns the floor
/// builds on the first machine and does not exist on the second, and the only
/// symptom is a pipeline-compilation error nobody could reproduce. Requesting
/// the floor makes the device validate against it, so the overrun fails the
/// same way on every machine, which is the only way it gets noticed where it
/// was written.
///
/// Every other limit stays the adapter's: nothing else here is a portability
/// cliff Byard has walked off, and lowering limits the engine does honour would
/// cost real capability for no evidence.
///
/// Test devices use this too, so a readback test is running the pipeline the
/// engine runs.
#[must_use]
pub fn device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let mut limits = adapter.limits();
    limits.max_vertex_attributes = limits
        .max_vertex_attributes
        .min(wgpu::Limits::default().max_vertex_attributes);
    limits
}

/// The instance census of one frame (RFC-0030 §P6).
///
/// A snapshot, not a counter: it describes the frame it was read from and
/// nothing else, which is why it can be handed across the boundary as a plain
/// POD without any of the staleness questions a running total would raise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Census {
    /// Box-class draw instances, see [`crate::frame::RenderFrame::instance_count`].
    pub instances: usize,
    /// Text lines (not glyphs).
    pub texts: usize,
    /// MSDF vector-glyph instances.
    pub vectors: usize,
}

/// What every surface of a program shares on the GPU (RFC-0068 §5): the device
/// and queue, every pipeline and shader module, the samplers, the glyph atlas,
/// the image cache and the MSDF atlas.
///
/// A program has one. What belongs to a single surface (its `wgpu::Surface`, its
/// persistent target, what it drew last frame) lives in a [`SurfaceTarget`], and
/// any number of those draw through the one `Gpu`, so a second window or a
/// second bar costs its swapchain and its incremental-redraw target and not
/// another copy of the pipelines or the glyph atlas.
///
/// A `Gpu` always has at least one target, made with it. The first is the one
/// [`Gpu::new`] was given a size for; [`add_target`](Self::add_target) makes
/// others and [`drop_target`](Self::drop_target) removes them.
///
/// Frames are encoded one surface at a time and each is submitted before the
/// next is encoded: the scratch the encode path reuses (the instance arena, the
/// clip table, the viewport uniform) is shared, and the submission is what
/// orders its writes. A *round* ([`begin_round`](Self::begin_round)) is the
/// frames of every surface that draws between two points; the housekeeping that
/// frees what a frame did not use runs once, at the end of it.
pub struct Gpu {
    encoder: EncoderSubsystem,
}

impl Gpu {
    /// Builds the shared GPU state on an already-created device and queue and
    /// the first surface's target, `phys_w` by `phys_h` physical pixels at
    /// `scale`. `target_format` is the format every surface of this program
    /// draws in (what a surface negotiates: the `Unorm` twin of an sRGB one).
    /// `blur_capable` picks the 0.5× tier of the `auto` backdrop blur
    /// (RFC-0023).
    ///
    /// # Errors
    ///
    /// [`ByardError::PipelineCompilation`] if a shader or pipeline fails
    /// validation.
    pub async fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        target_format: wgpu::TextureFormat,
        (phys_w, phys_h, scale): (u32, u32, f64),
        blur_capable: bool,
    ) -> Result<Self, ByardError> {
        #[allow(clippy::cast_possible_truncation)]
        let scale_f32 = scale as f32;
        let mut encoder =
            EncoderSubsystem::init(device, queue, target_format, scale_f32, phys_w, phys_h).await?;
        encoder.set_blur_auto_capable(blur_capable);
        // Viewport uniform uses LOGICAL pixels so all instance coordinates can
        // be authored in density-independent units. cast precision loss is
        // acceptable: logical pixel counts fit well within f32's 24-bit mantissa.
        #[allow(clippy::cast_precision_loss)]
        encoder.update_viewport(
            logical_viewport(phys_w, phys_h, scale),
            phys_w,
            phys_h,
            scale_f32,
        );
        Ok(Self { encoder })
    }

    /// The target made with the `Gpu`.
    #[must_use]
    pub fn first_target(&self) -> TargetKey {
        // The first target is the one the encoder was initialised with, and it
        // is the bound one until another is drawn.
        self.encoder.first_target()
    }

    /// Adds a surface's state, `phys_w` by `phys_h` physical pixels at `scale`.
    pub fn add_target(&mut self, phys_w: u32, phys_h: u32, scale: f64) -> TargetKey {
        #[allow(clippy::cast_possible_truncation)]
        self.encoder.add_target(phys_w, phys_h, scale as f32)
    }

    /// Resizes a target (or changes its scale). Sizes of zero are ignored, as
    /// [`Engine::on_resize`] ignores them: a minimised window reports one.
    /// Returns `false` for a target that is not there.
    pub fn resize_target(&mut self, key: TargetKey, phys_w: u32, phys_h: u32, scale: f64) -> bool {
        if phys_w == 0 || phys_h == 0 {
            return self.encoder.has_target(key);
        }
        if !self.encoder.bind_target(key) {
            return false;
        }
        #[allow(clippy::cast_possible_truncation)]
        self.encoder.update_viewport(
            logical_viewport(phys_w, phys_h, scale),
            phys_w,
            phys_h,
            scale as f32,
        );
        true
    }

    /// Drops a target and the GPU memory it held. The others keep drawing.
    /// `false` if it is unknown or the last one.
    pub fn drop_target(&mut self, key: TargetKey) -> bool {
        self.encoder.remove_target(key)
    }

    /// How many targets are alive.
    #[must_use]
    pub fn target_count(&self) -> usize {
        self.encoder.target_count()
    }

    /// Opens a round (see the type's docs).
    pub fn begin_round(&mut self) {
        self.encoder.begin_round();
    }

    /// Closes the round: the glyph atlas is trimmed once, and the image cache
    /// evicts once, after every surface that drew in it.
    pub fn end_round(&mut self) {
        self.encoder.end_round();
    }

    /// Encodes `frame` for `key` into `texture` (the surface's current texture,
    /// or any texture of the target's size and format) and submits it.
    ///
    /// # Errors
    ///
    /// [`ByardError::RenderSurface`] for a target that is not there, and
    /// whatever [`EncoderSubsystem::encode_frame_from_relay`] returns.
    pub fn render_target(
        &mut self,
        key: TargetKey,
        texture: &wgpu::Texture,
        frame: &crate::frame::RenderFrame,
    ) -> Result<(), ByardError> {
        if !self.encoder.bind_target(key) {
            return Err(ByardError::RenderSurface(
                "this surface was already dropped".to_string(),
            ));
        }
        let cmd = self.encoder.encode_frame_from_relay(texture, frame)?;
        self.encoder.submit(cmd);
        Ok(())
    }

    /// The encoder, for what a test or a tool asks of it.
    #[must_use]
    pub const fn encoder(&self) -> &EncoderSubsystem {
        &self.encoder
    }

    /// The encoder, mutably.
    pub fn encoder_mut(&mut self) -> &mut EncoderSubsystem {
        &mut self.encoder
    }
}

/// One surface of the program: the platform's `wgpu::Surface`, its
/// configuration, its scale factor and the [`TargetKey`] of its persistent
/// target inside the [`Gpu`] (RFC-0068 §5).
///
/// Today's single window is one of these, made by [`Engine::init`].
pub struct SurfaceTarget {
    id: SurfaceId,
    surface: wgpu::Surface<'static>,
    /// Cached surface configuration (physical pixels), updated on every resize
    /// so that surface-loss recovery can reconfigure without external input.
    config: wgpu::SurfaceConfiguration,
    /// Cached scale factor so surface-loss recovery can recompute the logical
    /// viewport without additional platform input.
    scale_factor: f64,
    /// The persistent target and incremental-redraw state in the [`Gpu`].
    key: TargetKey,
}

impl SurfaceTarget {
    /// The surface this is the target of.
    #[must_use]
    pub const fn id(&self) -> SurfaceId {
        self.id
    }

    /// The key of its state in the [`Gpu`].
    #[must_use]
    pub const fn key(&self) -> TargetKey {
        self.key
    }

    /// Its size in physical pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Its scale factor.
    #[must_use]
    pub const fn scale_factor(&self) -> f64 {
        self.scale_factor
    }
}

/// The drawing half of an [`Engine`]: the shared GPU state and the surfaces.
/// Made when the first surface exists, because the adapter is chosen to be
/// compatible with it and its format decides the pipelines'.
struct Render {
    gpu: Gpu,
    /// The adapter, kept to configure the surfaces added later.
    adapter: wgpu::Adapter,
    /// The first surface, inline: a single-surface program has this and
    /// nothing else (RFC-0068 §10). [`SurfaceId::MAIN`] in one; the id of the
    /// first window in a program of several. Another surface takes its place
    /// when it is dropped and others remain.
    main: SurfaceTarget,
    /// Every other surface. Empty, and unallocated, for a single-surface
    /// program.
    others: Vec<SurfaceTarget>,
}

impl Render {
    fn target(&self, id: SurfaceId) -> Option<&SurfaceTarget> {
        if self.main.id == id {
            Some(&self.main)
        } else {
            self.others.iter().find(|t| t.id == id)
        }
    }
}

/// The Byard rendering engine.
///
/// Owns the [`Gpu`] every surface shares, the surfaces themselves
/// ([`SurfaceTarget`]s, one for a single-window program, which is every
/// program today), and the [`Relay`] that connects the logic and render
/// threads. Construction is two-phase:
///
/// 1. [`Engine::init`], selects the GPU adapter, compiles pipelines, and
///    creates the `Relay`. Synchronous startup, no threads yet. The surface it
///    is given becomes [`SurfaceId::MAIN`].
/// 2. [`Engine::start_logic`], spawns the logic thread (Evaluator + Atlas),
///    publishes the first frame synchronously so `render_latest` has content
///    on the very first redraw.
///
/// Thereafter the platform calls [`on_resize`](Engine::on_resize) on resize
/// events and [`render_latest`](Engine::render_latest) on every
/// `RedrawRequested` event. More surfaces are
/// [`add_surface`](Engine::add_surface)d, resized and dropped on the same `Gpu`.
///
/// Dropping `Engine` signals the logic thread to shut down and joins it
/// before returning, no orphan threads after the window closes.
pub struct Engine {
    /// What drawing needs, once there is a first surface to build it against.
    render: Option<Render>,
    /// The vector-upload acknowledgement channel, held until the GPU state
    /// exists to be given it.
    pending_ack: Option<crossbeam_channel::Sender<u64>>,
    /// Lock-free frame swap connecting the logic and render threads.
    relay: Arc<Relay>,
    /// Sender half of the label-text channel. `set_label_text` sends here;
    /// the logic thread drains it each tick via `try_recv`.
    label_tx: crossbeam_channel::Sender<String>,
    /// Receiver half, taken by `start_logic` when the logic thread is spawned.
    /// `None` after `start_logic` is called.
    label_rx: Option<crossbeam_channel::Receiver<String>>,
    /// Image files changed on disk, reported through [`ImageReloader`] from
    /// any thread and drained by the render thread, which owns the texture
    /// cache.
    image_reload_tx: crossbeam_channel::Sender<std::path::PathBuf>,
    image_reload_rx: crossbeam_channel::Receiver<std::path::PathBuf>,
    /// Handle to the logic thread, taken and joined in `Drop`.
    logic_handle: Option<JoinHandle<()>>,
    /// The surfaces whose frame reached the platform in the last round
    /// ([`take_presented`](Engine::take_presented)): replaced each round, so
    /// it never grows past the number of surfaces.
    presented: Vec<SurfaceId>,
}

/// Backing state for [`Engine`]'s one Signal-driven text label.
///
/// Bundles a [`ViewArena`], the [`Signal<String>`](Signal) allocated from
/// it, an [`EvaluatorTick`] tracking that signal, and a trivial single-node
/// [`LayoutAtlas`] that receives the resulting dirty-target broadcast, the
/// same Evaluator → Atlas flow RFC-0001 §2.2/§4.1 describes, now exercised
/// by production code instead of only by `atlas/layout.rs`'s unit tests.
///
/// # Self-referential lifetime
///
/// `Signal<'a, T>` ties its lifetime to the [`ViewArena`] it was allocated
/// from. Storing both as sibling fields is therefore self-referential,
/// which safe Rust cannot express directly. This is resolved the same way
/// any self-referential owner must: heap-allocate the arena (`Box`, so its
/// address is stable even if `ReactiveLabel` itself moves) and erase the
/// signal's lifetime to `'static` via [`Signal::erase_lifetime`]. This is
/// sound because `arena` and `signal` are dropped together, nothing
/// outside this struct ever holds a copy of `signal`, so it is never used
/// after `arena` is gone.
struct ReactiveLabel {
    // Boxed so the heap address backing `signal`'s slot never moves, even
    // if `ReactiveLabel` (or the `Engine` that owns it) is moved. Never
    // read directly, its only job is to keep that heap allocation (and
    // therefore `signal`'s backing slot) alive for as long as `ReactiveLabel`
    // exists; the value is reached exclusively through `signal`'s erased
    // `'static` handle, not through this field.
    #[allow(dead_code, reason = "kept alive only to back `signal`'s slot")]
    arena: Box<ViewArena>,
    signal: Signal<'static, String>,
    tick: EvaluatorTick<'static>,
    atlas: LayoutAtlas,
    target: TargetId,
    x: f32,
    y: f32,
    font_size: f32,
    color: [f32; 4],
}

impl ReactiveLabel {
    fn new(text: impl Into<String>, x: f32, y: f32, font_size: f32, color: [f32; 4]) -> Self {
        let arena = Box::new(ViewArena::new());
        // `arena` is boxed (stable heap address) and dropped together with
        // `signal` when `ReactiveLabel` (and the `Engine` that owns it) is
        // dropped, see the struct doc above. `Signal::new_in_boxed` is the
        // safe wrapper around the `unsafe` lifetime erasure that pattern
        // requires; the `unsafe` block itself stays inside `signal.rs`,
        // the evaluator subsystem file that owns this invariant.
        let signal: Signal<'static, String> = Signal::new_in_boxed(&arena, text.into());

        // A single trivial (zero-sized) leaf is enough to give the label a
        // real AtlasNode TargetId to subscribe to and mark dirty, Phase 1
        // does not yet thread Atlas-resolved geometry into `TextLine` (x/y
        // are authored directly), so this leaf's only job is to participate
        // honestly in the dirty-broadcast flow, not to compute position.
        let mut atlas = LayoutAtlas::new();
        let node = atlas
            .add_leaf(LeafSize {
                width: 0.0,
                height: 0.0,
            })
            .expect("a single freshly created leaf can never fail to add");
        atlas
            .set_root(node)
            .expect("the node just created always belongs to this atlas");
        atlas
            .compute(Viewport::new(0.0, 0.0))
            .expect("computing layout for one zero-sized leaf can never fail");

        let target = TargetId::new(0, atlas.current_generation(), TargetKind::AtlasNode as u16);
        signal.subscribe(target);

        let mut tick = EvaluatorTick::new();
        tick.register(signal);

        Self {
            arena,
            signal,
            tick,
            atlas,
            target,
            x,
            y,
            font_size,
            color,
        }
    }

    /// Overwrites the label's text content. The signal's version counter
    /// advances; the next [`ReactiveLabel::text_line`] call observes it.
    fn set_text(&self, text: impl Into<String>) {
        self.signal.write(|v| *v = text.into());
    }

    /// Runs one Evaluator → Atlas tick and returns this frame's `TextLine`,
    /// with `dirty` reflecting the real dirty-flag pipeline output, never
    /// a hardcoded value.
    fn text_line(&mut self) -> Result<TextLine, ByardError> {
        let dirty_targets = self.tick.collect_dirty();
        let dirty = if dirty_targets.is_empty() {
            false
        } else {
            self.atlas.mark_dirty_all(&dirty_targets);
            self.atlas.recompute_dirty(Viewport::new(0.0, 0.0))?;
            dirty_targets.contains(&self.target)
        };

        Ok(TextLine {
            align: crate::frame::TextAlign::Start,
            max_lines: 0,
            ellipsis: false,
            x: self.x,
            y: self.y,
            text: self.signal.read(String::clone),
            font_size: self.font_size,
            weight: 400,
            family: None,
            variations: None,
            color: self.color,
            dirty,
        })
    }
}

/// The surface's format and the format the engine draws in (RFC-0060).
///
/// The engine draws into `Unorm` targets and its shaders write sRGB-encoded
/// bytes, so the blend mixes encoded values like a design tool. A `Unorm`
/// surface is taken when one is offered. When the surface only offers sRGB, it
/// is taken with its `Unorm` twin allowed as a view format; the engine's own
/// target is the twin, and the final copy onto the surface is byte for byte.
fn negotiate_formats(
    offered: &[wgpu::TextureFormat],
) -> (wgpu::TextureFormat, wgpu::TextureFormat) {
    let surface = offered
        .iter()
        .find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .or_else(|| offered.iter().find(|f| f.is_srgb()))
        .copied()
        .unwrap_or(offered[0]);
    (surface, surface.remove_srgb_suffix())
}

/// Negotiates a surface's format and composite alpha, and configures it at
/// `width` by `height` physical pixels. Returns the configuration; its
/// `view_formats` holds the format the engine draws in when that is not the
/// surface's own (RFC-0060), and is empty when they are the same.
/// The composite alpha mode of a surface: opaque unless the surface is meant
/// to be see-through (`transparent: true`), then a mode that blends it with
/// what is behind it, premultiplied first because that is what the output
/// shader writes (RFC-0060). A surface that offers neither is drawn opaque,
/// and says so, once.
fn alpha_mode_for(
    offered: &[wgpu::CompositeAlphaMode],
    transparent: bool,
) -> wgpu::CompositeAlphaMode {
    use wgpu::CompositeAlphaMode as Mode;
    static TOLD: std::sync::Once = std::sync::Once::new();
    if transparent {
        for mode in [Mode::PreMultiplied, Mode::PostMultiplied] {
            if offered.contains(&mode) {
                return mode;
            }
        }
        TOLD.call_once(|| {
            eprintln!(
                "  a window asked for `transparent: true`, but this surface offers only {offered:?}; \
                 it is drawn opaque"
            );
        });
    }
    if offered.contains(&Mode::Opaque) {
        Mode::Opaque
    } else {
        offered.first().copied().unwrap_or(Mode::Auto)
    }
}

fn configure_surface(
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    width: u32,
    height: u32,
    transparent: bool,
) -> wgpu::SurfaceConfiguration {
    // --- Surface format negotiation ---
    let caps = surface.get_capabilities(adapter);
    let (surface_format, target_format) = negotiate_formats(&caps.formats);
    let view_formats = if target_format == surface_format {
        vec![]
    } else {
        vec![target_format]
    };

    // --- Composite alpha negotiation ---
    // The UI clears its target to `Color::TRANSPARENT` (an incremental
    // frame needs a genuinely empty background to blend onto), so the
    // presented image carries a zero alpha in its cleared regions. Under a
    // *non-opaque* composite mode the window manager then blends the whole
    // window against the desktop and the app looks fully transparent, a
    // frozen, see-through window on X11, and on Wayland a toplevel the
    // compositor may not visibly map at all. macOS/Metal and Windows/DX12
    // happen to report `Opaque` first so `alpha_modes[0]` was invisibly
    // "correct" there; most Linux drivers report a transparent mode first,
    // which is the real cause of the "window never appears" report. Pick
    // `Opaque` explicitly whenever the surface offers it, so the alpha
    // channel is ignored at composite time and the cleared background reads
    // as solid on every platform.
    let alpha_mode = alpha_mode_for(&caps.alpha_modes, transparent);

    // --- Surface configuration (physical pixels, wgpu requirement) ---
    //
    // COPY_DST is required in addition to RENDER_ATTACHMENT: per RFC §3.3's
    // scissor-clipping implementation, `EncoderSubsystem::encode_frame`
    // never draws directly onto the swapchain image, it draws onto a
    // persistent off-screen target and `copy_texture_to_texture`s the
    // result onto this surface's current texture every frame (the
    // swapchain image's own previous contents are never assumed valid
    // under multi-buffering). See `encoder::EncoderSubsystem::persistent_color`.
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
        format: surface_format,
        width,
        height,
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode,
        view_formats,
        desired_maximum_frame_latency: 2,
    };
    surface.configure(device, &config);
    config
}

impl Engine {
    /// Initialises the engine, selects a GPU adapter, and compiles all pipelines.
    ///
    /// Performs adapter selection, device creation, surface format negotiation,
    /// surface configuration, and pipeline compilation. All GPU errors are
    /// captured and returned as [`ByardError`], this method never panics.
    ///
    /// ## Parameters
    ///
    /// - `width`, `height`, initial surface dimensions in **physical pixels**
    ///   (`window.inner_size()` in winit). Used for the `wgpu` surface only.
    /// - `scale_factor`, OS DPI scale factor (`window.scale_factor()` in
    ///   winit; typically `1.0` on non-HiDPI, `2.0` on Retina). Used to
    ///   convert physical pixels → logical pixels for the viewport uniform so
    ///   that all instance coordinates (rect, radii, etc.) can be authored in
    ///   logical pixels regardless of display density.
    ///
    /// # Errors
    ///
    /// - [`ByardError::UnsupportedBackend`], no compatible GPU adapter found,
    ///   or logical device creation failed.
    /// - [`ByardError::PipelineCompilation`], WGSL shader or pipeline
    ///   descriptor failed GPU-side validation.
    pub async fn init(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        scale_factor: f64,
    ) -> Result<Self, ByardError> {
        let mut engine = Self::without_surface()?;
        engine
            .attach_first_surface(
                instance,
                SurfaceId::MAIN,
                surface,
                (width, height, scale_factor),
                false,
            )
            .await?;
        Ok(engine)
    }

    /// An engine with no surface yet: the relay, the channels, everything the
    /// logic thread needs, so it can be started (and say which windows the
    /// program opens) before any window exists. Nothing can be drawn until
    /// [`attach_first_surface`](Self::attach_first_surface).
    ///
    /// # Errors
    ///
    /// [`ByardError::ThreadSpawn`] if the relay's I/O pool cannot start.
    pub fn without_surface() -> Result<Self, ByardError> {
        let relay = Arc::new(Relay::new()?);
        let (label_tx, label_rx) = crossbeam_channel::unbounded::<String>();
        let (image_reload_tx, image_reload_rx) = crossbeam_channel::unbounded();
        Ok(Self {
            image_reload_tx,
            image_reload_rx,
            render: None,
            pending_ack: None,
            relay,
            label_tx,
            label_rx: Some(label_rx),
            logic_handle: None,
            presented: Vec::new(),
        })
    }

    /// The surfaces whose latest frame was presented (committed to the
    /// platform) by the last round. A round can draw nothing
    /// for a surface that has no frame yet or whose swapchain was lost and
    /// reconfigured; a host that paces its frames on the compositor's own
    /// callbacks (RFC-0068 §5) has to know which ones did commit.
    pub fn take_presented(&mut self) -> Vec<SurfaceId> {
        std::mem::take(&mut self.presented)
    }

    /// Whether the engine has a surface to draw into yet.
    #[must_use]
    pub const fn has_surface(&self) -> bool {
        self.render.is_some()
    }

    /// Selects the adapter, creates the device and compiles the pipelines for
    /// `surface`, the first surface of the program, which is `id`
    /// ([`SurfaceId::MAIN`] in a program of one window, the first window's
    /// id otherwise). `size` is `(width, height, scale_factor)`, physical
    /// pixels. `transparent` asks for a composite alpha mode that blends the
    /// surface with what is behind it.
    ///
    /// # Errors
    ///
    /// As [`init`](Self::init); [`ByardError::RenderSurface`] if the engine
    /// already has a surface.
    #[allow(clippy::too_many_lines)]
    pub async fn attach_first_surface(
        &mut self,
        instance: &wgpu::Instance,
        id: SurfaceId,
        surface: wgpu::Surface<'static>,
        (width, height, scale_factor): (u32, u32, f64),
        transparent: bool,
    ) -> Result<(), ByardError> {
        if self.render.is_some() {
            return Err(ByardError::RenderSurface(
                "the engine already has its first surface".to_string(),
            ));
        }
        // --- Adapter selection ---
        // wgpu 29: request_adapter returns Result<Adapter, RequestAdapterError>
        // instead of Option<Adapter>; use map_err instead of ok_or.
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| ByardError::UnsupportedBackend)?;

        // Surfaced unconditionally (not gated behind a verbose/debug flag):
        // which adapter and backend actually got picked is exactly the
        // information needed to diagnose "nothing renders, no error" reports,
        // a software/CPU adapter (`DeviceType::Cpu`) or an unexpected backend
        // (e.g. GL instead of Vulkan) explains a great deal that would
        // otherwise look like a silent failure.
        let info = adapter.get_info();
        eprintln!(
            "  GPU adapter: {} ({:?}, {:?} backend, driver: {})",
            info.name, info.device_type, info.backend, info.driver
        );

        // --- Logical device ---
        // wgpu 29: request_device takes only &DeviceDescriptor (trace path moved
        // into DeviceDescriptor::trace); use ..Default::default() to zero-fill
        // the new `experimental_features` and `trace` fields.
        //
        // `TIMESTAMP_QUERY` is requested opportunistically (intersected with
        // what the adapter actually supports, never a hard requirement) so
        // `EncoderSubsystem`'s `GpuTimer` (RFC-0013 "GPU timing") can activate
        // when the backend allows it; `GpuTimer::new` checks the resulting
        // `device.features()` and degrades to unavailable otherwise (P5).
        let optional_features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("ByardCore - Engine Device"),
                required_features: optional_features,
                required_limits: device_limits(&adapter),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            })
            .await
            .map_err(|_| ByardError::UnsupportedBackend)?;

        // --- Surface format negotiation and configuration ---
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let config = configure_surface(&surface, &adapter, &device, width, height, transparent);
        let target_format = config
            .view_formats
            .first()
            .copied()
            .unwrap_or(config.format);

        // --- The shared GPU state, and the first surface's target ---
        // RFC-0023 resolved question "blur quality tiers": the `auto` tier
        // runs the separable Gaussian at 0.5× base resolution on every real
        // GPU, `IntegratedGpu` includes Apple Silicon and modern mobile
        // parts, which handle it easily, and drops to the cheap 0.25× base
        // only on software/virtual adapters, where every fragment costs CPU.
        // A per-element `blur_quality: high | low` always overrides this.
        let blur_capable = !matches!(
            info.device_type,
            wgpu::DeviceType::Cpu | wgpu::DeviceType::VirtualGpu
        );
        let gpu = Gpu::new(
            device,
            queue,
            target_format,
            (width, height, scale_factor),
            blur_capable,
        )
        .await?;
        let key = gpu.first_target();

        // Wire async image decode: the encoder spawns decodes on the relay's
        // I/O pool and reports results back through the **render** thread's
        // half of the split result channel (RFC-0028 §7), which
        // `render_latest` drains. Controller replies travel the other half and
        // are never seen here.
        let mut gpu = gpu;
        gpu.encoder.set_io_context(
            self.relay.io_handle(),
            crate::encoder::texture_sampler::DecodeResultSender::new(
                self.relay.decode_result_sender(),
                self.relay.renderer_wake(),
            ),
        );
        if let Some(tx) = self.pending_ack.take() {
            gpu.encoder.set_vector_ack_sender(tx);
        }
        self.render = Some(Render {
            gpu,
            adapter,
            main: SurfaceTarget {
                id,
                surface,
                config,
                scale_factor,
                key,
            },
            others: Vec::new(),
        });
        Ok(())
    }

    /// The GPU state every surface shares, once there is a first surface.
    #[must_use]
    pub fn gpu(&self) -> Option<&Gpu> {
        self.render.as_ref().map(|r| &r.gpu)
    }

    /// Adds a surface to the program: a second window, a panel (RFC-0068).
    /// `width` and `height` are physical pixels. It draws through the same
    /// [`Gpu`] as every other, so it shares the pipelines and the glyph atlas.
    /// `transparent` asks for a composite alpha mode that blends it with what
    /// is behind it.
    ///
    /// # Errors
    ///
    /// - [`ByardError::RenderSurface`] if `id` already has a surface, if the
    ///   engine has no first surface yet, or if `surface` negotiates a
    ///   different format from the one the pipelines were built for (every
    ///   surface of a program shares them).
    pub fn add_surface(
        &mut self,
        id: SurfaceId,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        scale_factor: f64,
        transparent: bool,
    ) -> Result<(), ByardError> {
        let Some(r) = self.render.as_mut() else {
            return Err(ByardError::RenderSurface(
                "the engine has no first surface yet".to_string(),
            ));
        };
        if r.target(id).is_some() {
            return Err(ByardError::RenderSurface(format!(
                "surface {} already exists",
                id.index()
            )));
        }
        let config = configure_surface(
            &surface,
            &r.adapter,
            r.gpu.encoder.device(),
            width,
            height,
            transparent,
        );
        let format = config
            .view_formats
            .first()
            .copied()
            .unwrap_or(config.format);
        if format != r.gpu.encoder.target_format() {
            // Undo nothing: the surface was only configured, and drops here.
            return Err(ByardError::RenderSurface(format!(
                "surface {} draws in {format:?} but the program's pipelines are built for {:?}",
                id.index(),
                r.gpu.encoder.target_format()
            )));
        }
        let key = r.gpu.add_target(width, height, scale_factor);
        r.others.push(SurfaceTarget {
            id,
            surface,
            config,
            scale_factor,
            key,
        });
        Ok(())
    }

    /// Drops a surface, and its frame slot in the relay. The others keep
    /// drawing. The first surface can be dropped too while another remains:
    /// that one takes its place. `false` if it is not there, or it is the only
    /// surface left (the program ends then).
    pub fn drop_surface(&mut self, id: SurfaceId) -> bool {
        let Some(r) = self.render.as_mut() else {
            return false;
        };
        if r.main.id == id {
            if r.others.is_empty() {
                return false;
            }
            let next = r.others.swap_remove(0);
            let old = std::mem::replace(&mut r.main, next);
            r.gpu.drop_target(old.key);
        } else {
            let Some(index) = r.others.iter().position(|t| t.id == id) else {
                return false;
            };
            let target = r.others.swap_remove(index);
            r.gpu.drop_target(target.key);
        }
        self.relay.remove_surface(id);
        true
    }

    /// The surfaces the engine draws, the first one first.
    pub fn surface_ids(&self) -> impl Iterator<Item = SurfaceId> + '_ {
        self.render
            .iter()
            .flat_map(|r| std::iter::once(r.main.id).chain(r.others.iter().map(|t| t.id)))
    }

    /// The frame to read the logic thread's telemetry and census from: the
    /// first surface's latest.
    fn primary_frame(&self) -> Option<Arc<crate::frame::RenderFrame>> {
        match &self.render {
            Some(r) => self.relay.current_for(r.main.id),
            None => self.relay.current(),
        }
    }

    /// Replaces the engine's reactive label text.
    ///
    /// Sends the new text to the logic thread via a lock-free channel; the
    /// logic thread picks it up on its next tick, writes it to the `Signal`,
    /// and the resulting `TextLine::dirty` bit propagates through the
    /// Evaluator → Atlas pipeline, the full RFC-0001 §2.2/§4.1 path,
    /// without any cross-thread `Signal` write.
    pub fn set_label_text(&self, text: impl Into<String>) {
        // Unbounded channel: send is always non-blocking. Ignore the error,
        // it only fires when the logic thread has already exited, at which
        // point delivering the text is moot.
        let _ = self.label_tx.send(text.into());
    }

    /// Pushes an input event into the engine's logic queue.
    pub fn push_input(&self, event: InputEvent) {
        self.relay.push_input(event);
    }

    /// A handle any thread can use to report that an image file changed on
    /// disk (dev hot reload). Every `Image` drawn from it is decoded again,
    /// and the render loop is woken to show it.
    #[must_use]
    pub fn image_reloader(&self) -> ImageReloader {
        ImageReloader {
            tx: self.image_reload_tx.clone(),
            wake: self.relay.renderer_wake(),
        }
    }

    /// Installs a callback the logic thread fires after it publishes a frame
    /// that changed in response to input.
    ///
    /// An event-driven (`Wait`-mode) host should point this at its event loop's
    /// wake primitive (e.g. a winit `EventLoopProxy`) and request a redraw when
    /// it fires, so input results appear immediately instead of waiting for the
    /// next unrelated OS event. A continuously-redrawing (`Poll`) host does not
    /// need it. See [`Relay::set_frame_waker`](crate::relay::Relay::set_frame_waker).
    pub fn set_frame_waker(&self, waker: crate::relay::FrameWaker) {
        self.relay.set_frame_waker(waker);
    }

    /// Installs the channel this engine reports applied vector-atlas-upload
    /// ids through (RFC-0009 §2-C), see
    /// [`EncoderSubsystem::set_vector_ack_sender`](crate::encoder::EncoderSubsystem::set_vector_ack_sender).
    pub fn set_vector_ack_sender(&mut self, tx: crossbeam_channel::Sender<u64>) {
        match self.render.as_mut() {
            Some(r) => r.gpu.encoder.set_vector_ack_sender(tx),
            // Given to the GPU state when the first surface makes it.
            None => self.pending_ack = Some(tx),
        }
    }

    /// Notifies the engine that the window surface has been resized.
    ///
    /// `width` and `height` are the new dimensions in **physical pixels**
    /// (`window.inner_size()` in winit). `scale_factor` is the OS DPI scale
    /// factor, pass `window.scale_factor()` and also call this method from
    /// `WindowEvent::ScaleFactorChanged` so the viewport uniform stays correct
    /// when the window moves between displays of different densities.
    ///
    /// Calls with `width == 0` or `height == 0` are silently ignored, zero-size
    /// surfaces are invalid in wgpu (occurs on window minimise on some platforms).
    pub fn on_resize(&mut self, width: u32, height: u32, scale_factor: f64) {
        self.resize_surface(SurfaceId::MAIN, width, height, scale_factor);
    }

    /// [`on_resize`](Self::on_resize) for the surface `id`. `false` if there is
    /// no such surface.
    pub fn resize_surface(
        &mut self,
        id: SurfaceId,
        width: u32,
        height: u32,
        scale_factor: f64,
    ) -> bool {
        let Some(Render {
            gpu, main, others, ..
        }) = self.render.as_mut()
        else {
            return false;
        };
        let target = if main.id == id {
            Some(main)
        } else {
            others.iter_mut().find(|t| t.id == id)
        };
        let Some(target) = target else {
            return false;
        };
        if width == 0 || height == 0 {
            return true;
        }
        target.scale_factor = scale_factor;
        target.config.width = width;
        target.config.height = height;
        target
            .surface
            .configure(gpu.encoder.device(), &target.config);
        gpu.resize_target(target.key, width, height, scale_factor)
    }

    /// Bundles `registry` with this engine's I/O runtime and its logic-thread
    /// reply channel into the [`Dispatcher`](crate::bridge::Dispatcher) the
    /// interpreter places controller calls through (RFC-0028 §5).
    ///
    /// Built here because the engine is the one place that owns both halves:
    /// the app supplies the controllers, the relay supplies the pool and the
    /// channel. The result is `Send`, so it can be moved into the logic-thread
    /// factory closure.
    #[must_use]
    pub fn dispatcher(
        &self,
        registry: crate::bridge::ControllerRegistry,
    ) -> crate::bridge::Dispatcher {
        crate::bridge::Dispatcher::new(
            registry,
            self.relay.io_handle(),
            self.relay.io_result_sender(),
        )
    }

    /// Spawns the logic thread and publishes the first frame synchronously.
    ///
    /// Must be called exactly once, after [`Engine::init`] and before the first
    /// [`Engine::render_latest`] call. `instances` and `texts` are the static
    /// scene content authored by the platform; the engine appends its own
    /// reactive label to `texts` each tick.
    ///
    /// The first frame is published synchronously (before this method returns)
    /// so that [`render_latest`](Engine::render_latest) always finds a frame
    /// ready on the very first `RedrawRequested` event.
    ///
    /// # Panics
    ///
    /// Panics if called more than once on the same `Engine`.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::ThreadSpawn`] if the OS refuses to create the
    /// logic thread.
    pub fn start_logic(
        &mut self,
        instances: Vec<BoxInstance>,
        texts: Vec<TextLine>,
    ) -> Result<(), ByardError> {
        let relay = Arc::clone(&self.relay);
        let label_rx = self
            .label_rx
            .take()
            .expect("start_logic must be called exactly once");

        // Publish the first frame synchronously so render_latest has content
        // on the very first RedrawRequested, before the thread has ticked.
        {
            let mut label =
                ReactiveLabel::new("Byard, Phase 1", 110.0, 110.0, 20.0, [1.0, 1.0, 1.0, 1.0]);
            let label_line = label.text_line()?;
            let mut frame = relay.acquire_recycled();
            for &inst in &instances {
                frame.push_instance(inst);
            }
            for text in &texts {
                frame.push_text(text.clone());
            }
            frame.push_text(label_line);
            relay.publish(frame);
        }

        // Spawn the logic thread. `ReactiveLabel` is created inside the thread
        // so it never needs to cross a thread boundary, `Signal<T>` is !Send
        // by design (RFC-0001 §5.1). The closure only captures Send types:
        // `relay: Arc<Relay>`, `label_rx: Receiver<String>`, and the
        // plain-data `instances`/`texts` vecs.
        let handle = std::thread::Builder::new()
            .name("byard-logic-thread".to_string())
            .spawn(move || {
                let mut label =
                    ReactiveLabel::new("Byard, Phase 1", 110.0, 110.0, 20.0, [1.0, 1.0, 1.0, 1.0]);
                while !relay.is_shutdown() {
                    while let Ok(new_text) = label_rx.try_recv() {
                        label.set_text(new_text);
                    }
                    let label_line = label
                        .text_line()
                        .expect("text_line never fails in logic loop");
                    // The version is the relay's publish sequence number and
                    // is stamped by `Relay::publish` itself (RFC-0001 §5.2),
                    // a host cannot forget to maintain it, which is the point.
                    let mut frame = relay.acquire_recycled();
                    for &inst in &instances {
                        frame.push_instance(inst);
                    }
                    for text in &texts {
                        frame.push_text(text.clone());
                    }
                    frame.push_text(label_line);
                    relay.publish(frame);
                    std::thread::yield_now();
                }
            })
            .map_err(|e| ByardError::ThreadSpawn(e.to_string()))?;

        self.logic_handle = Some(handle);
        Ok(())
    }

    /// Spawns the logic thread from a `build` factory that constructs a
    /// [`LogicRuntime`](crate::LogicRuntime) (e.g. the `byld` Dev interpreter) **inside** the thread
    /// (RFC-0002 §"Integration with Engine", RFC-0003 §8).
    ///
    /// This is the entry point the `byard-compiler` crate targets: it hands in
    /// a `Send + 'static` factory closing over a plain-data compiled view, and
    /// the factory builds the `!Send` running interpreter (holding `Signal`s
    /// and a logic-thread-local reactive scope) on the logic thread, where it
    /// is then driven once per tick. The `Send + 'static` bound is on the
    /// factory only, never on the [`LogicRuntime`](crate::LogicRuntime) it produces.
    ///
    /// Use this **instead of** [`start_logic`](Engine::start_logic); call it at
    /// most once.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::ThreadSpawn`] if the OS refuses to create the
    /// logic thread.
    pub fn start_logic_from_view<F>(&mut self, build: F) -> Result<(), ByardError>
    where
        F: for<'a> FnOnce(&'a ViewArena) -> Box<dyn crate::LogicRuntime + 'a> + Send + 'static,
    {
        let handle = Relay::spawn_logic_from_view(&self.relay, build)?;
        self.logic_handle = Some(handle);
        Ok(())
    }

    /// Tells the logic thread whether an assistive technology is listening
    /// (RFC-0042). While one is, every published frame carries an
    /// accessibility tree; while none is, no tree is built and nothing extra
    /// is allocated. A host calls this from
    /// [`PlatformHost::on_access_active`](crate::platform::PlatformHost::on_access_active).
    pub fn set_access_active(&self, active: bool) {
        self.relay.set_access_active(active);
    }

    /// The latest frame the logic thread published, whose
    /// [`access`](crate::frame::RenderFrame::access) is the tree to send to
    /// the assistive technology (RFC-0042). A host answers
    /// [`PlatformHost::access_frame`](crate::platform::PlatformHost::access_frame)
    /// with it.
    #[must_use]
    pub fn latest_frame(&self) -> Option<Arc<crate::frame::RenderFrame>> {
        self.relay.current()
    }

    /// [`latest_frame`](Self::latest_frame) for the surface `id`: each surface
    /// has its own accessibility tree (RFC-0068 §5).
    #[must_use]
    pub fn latest_frame_for(&self, id: SurfaceId) -> Option<Arc<crate::frame::RenderFrame>> {
        self.relay.current_for(id)
    }

    /// Takes the surface changes the logic thread queued, in order. The host
    /// calls this when the frame waker woke it, and creates, reconfigures or
    /// destroys platform surfaces to match; for each one it then calls
    /// [`add_surface`](Self::add_surface), [`resize_surface`](Self::resize_surface)
    /// or [`drop_surface`](Self::drop_surface). Empty for a single-surface
    /// program.
    pub fn drain_surface_changes(
        &self,
    ) -> impl Iterator<Item = crate::surface::SurfaceChange> + '_ {
        self.relay.drain_surface_changes()
    }

    /// Turns what an assistive technology asked into the input events a
    /// pointer or a keyboard would have sent, read against the tree of the
    /// latest frame, and queues them for the logic thread (RFC-0042 §5).
    ///
    /// A request for a node the latest tree no longer has queues nothing.
    pub fn push_access_request(&self, request: &crate::access::AccessRequest, time_ms: u64) {
        self.push_access_request_for(SurfaceId::MAIN, request, time_ms);
    }

    /// [`push_access_request`](Self::push_access_request) for a request the
    /// assistive technology made of the tree of surface `id`: it is read
    /// against that surface's latest tree, and the events it becomes belong to
    /// that surface.
    pub fn push_access_request_for(
        &self,
        id: SurfaceId,
        request: &crate::access::AccessRequest,
        time_ms: u64,
    ) {
        let Some(frame) = self.relay.current_for(id) else {
            return;
        };
        let Some(tree) = frame.access() else {
            return;
        };
        for event in request.to_events_on(id, tree, time_ms) {
            self.relay.push_input(event);
        }
    }

    /// The caret of the focused text field in the latest published frame,
    /// in viewport space (RFC-0040 §4). The platform reads it after each
    /// redraw to place the IME's candidate window.
    #[must_use]
    pub fn text_input(&self) -> Option<crate::frame::TextInputState> {
        self.primary_frame().and_then(|f| f.text_input())
    }

    /// Renders the latest [`RenderFrame`](crate::frame::RenderFrame) published
    /// by the logic thread to the window surface.
    ///
    /// Reads the latest frame from the [`Relay`] without blocking the logic
    /// thread, then encodes and presents it. If no frame has been published yet
    /// (i.e. [`start_logic`](Engine::start_logic) has not been called), this
    /// returns `Ok(())` immediately and skips the frame.
    ///
    /// # Surface loss
    ///
    /// If the surface is lost or outdated (window minimise/restore, driver
    /// reset), the engine silently reconfigures it and returns `Ok(())`.
    /// The next call to `render_latest` will produce output normally.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::RenderSurface`] only on unrecoverable surface
    /// errors such as out-of-memory or GPU timeout.
    pub fn render_latest(&mut self) -> Result<(), ByardError> {
        self.render_surfaces(&[SurfaceId::MAIN])
    }

    /// [`render_latest`](Self::render_latest) for the surface `id`: draws the
    /// latest frame published for it and presents it. A round of one.
    ///
    /// # Errors
    ///
    /// [`ByardError::RenderSurface`] if there is no such surface, and as
    /// [`render_latest`](Self::render_latest).
    pub fn render_surface(&mut self, id: SurfaceId) -> Result<(), ByardError> {
        self.render_surfaces(&[id])
    }

    /// Draws the surfaces `ids`, in order, as one round (RFC-0068 §5): each is
    /// encoded and submitted before the next, and the glyph atlas is trimmed,
    /// and the image cache evicts, once, after the last. A surface with no
    /// frame published yet is skipped, as [`render_latest`](Self::render_latest)
    /// skips.
    ///
    /// Every surface of the round is attempted even if one fails; the first
    /// error is the one returned.
    ///
    /// # Errors
    ///
    /// [`ByardError::RenderSurface`] if an id has no surface, and as
    /// [`render_latest`](Self::render_latest).
    pub fn render_surfaces(&mut self, ids: &[SurfaceId]) -> Result<(), ByardError> {
        let Some(r) = self.render.as_mut() else {
            return Ok(());
        };
        r.gpu.begin_round();
        self.presented.clear();
        let mut first_error = None;
        for &id in ids {
            let result = self.render_one(id);
            if let Err(e) = result {
                first_error.get_or_insert(e);
            }
        }
        if let Some(r) = self.render.as_mut() {
            r.gpu.end_round();
        }
        first_error.map_or(Ok(()), Err)
    }

    fn render_one(&mut self, id: SurfaceId) -> Result<(), ByardError> {
        let Self {
            render,
            relay,
            image_reload_rx,
            presented,
            ..
        } = self;
        let Some(Render {
            gpu, main, others, ..
        }) = render.as_mut()
        else {
            return Ok(());
        };
        let target = if main.id == id {
            Some(main)
        } else {
            others.iter_mut().find(|t| t.id == id)
        };
        let Some(target) = target else {
            return Err(ByardError::RenderSurface(format!(
                "surface {} does not exist",
                id.index()
            )));
        };
        // Nothing published yet (the logic thread hasn't produced its first
        // frame): skip *before* touching the surface. Acquiring a surface
        // texture and dropping it without `present()` is an anti-pattern, on
        // Wayland/FIFO it churns the swapchain acquire without ever committing a
        // buffer, so the window can sit unmapped ("loading", no error) until the
        // first real frame instead of drawing as soon as one lands.
        let Some(relay_frame) = relay.current_for(id) else {
            return Ok(());
        };

        // RFC-0030 §I1: swapchain acquire, timed on its own.
        //
        // Under FIFO this is where vsync backpressure lands, the driver
        // parks the caller here until a swapchain image is free. Left
        // unscoped (as it was) that wait is invisible, and a frame that is
        // simply *well paced* reads identically to one that is slow, which
        // makes every other number on the readout unfalsifiable. It is
        // deliberately a top-level scope and deliberately not inside
        // `encode.frame`: it is wall-clock the engine spends waiting, not
        // work it performs.
        let frame = {
            crate::profile_scope!("present.acquire");
            match target.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f)
                | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                    target
                        .surface
                        .configure(gpu.encoder.device(), &target.config);
                    return Ok(());
                }
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    return Ok(());
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    return Err(ByardError::RenderSurface(
                        "GPU validation error during surface texture acquire".to_string(),
                    ));
                }
            }
        };

        // Image files changed on disk (dev hot reload): decode them again.
        // The previous picture keeps drawing meanwhile, and the new decode
        // wakes the loop again when it lands.
        while let Ok(path) = image_reload_rx.try_recv() {
            gpu.encoder.reload_image(&path);
        }

        // Drain any completed async image decodes and upload them on this
        // (render) thread before encoding, so a freshly-decoded texture is
        // `Ready` for this frame. The decode itself already ran on the relay's
        // I/O pool, only the cheap GPU upload happens here.
        while let Some(result) = relay.try_recv_decode_result() {
            match result.downcast::<crate::encoder::DecodedImage>() {
                Ok(decoded) => gpu.encoder.apply_decoded(*decoded),
                // A result of some other type is not ours to handle here; drop
                // it. Since RFC-0028 §7 split the channel by destination this
                // is unreachable in practice, the logic thread's replies go to
                // the other half, and it stays as the honest `downcast` arm.
                Err(_other) => {}
            }
        }

        gpu.render_target(target.key, &frame.texture, &relay_frame)?;
        // *This* frame reached the GPU: its dirty bits have been acted on, so
        // the publish that replaces it need not carry them forward (RFC-0032
        // §R3 step 6). Named by version, because the logic thread has very
        // likely published a newer frame during the encode above, and this must
        // not be read as a claim about that one.
        relay.mark_rendered_for(id, relay_frame.version());
        {
            // RFC-0030 §I1: the swapchain commit. Cheap on a healthy frame and
            // the second place a compositor can charge the engine for pacing,
            // so it is measured rather than assumed, the pair
            // (`present.acquire`, `present.submit`) is what distinguishes "the
            // frame was slow" from "the frame was on time and waited".
            crate::profile_scope!("present.submit");
            frame.present();
        }
        presented.push(id);
        Ok(())
    }

    /// Returns the CPU-side telemetry ([`crate::telemetry::SampleBlock`])
    /// captured on the **logic** thread for the most recently published
    /// frame, or `None` if nothing has been published yet.
    ///
    /// Call after [`render_latest`](Self::render_latest) to build a combined
    /// CPU+GPU overlay (RFC-0013 "Overlay format"): GPU-tagged samples for
    /// the frame just encoded land on the **calling** thread's own telemetry
    /// ring instead (see [`crate::encoder::EncoderSubsystem`]'s module
    /// docs), so a caller on the render thread drains
    /// [`crate::telemetry::drain_samples`] itself for those.
    #[must_use]
    pub fn latest_cpu_telemetry(&self) -> Option<crate::telemetry::SampleBlock> {
        self.primary_frame().map(|frame| frame.telemetry().clone())
    }

    /// Which layout paths the atlas took producing the most recently published
    /// frame (RFC-0032 §R7), or the zero snapshot if nothing has been
    /// published yet.
    #[must_use]
    pub fn latest_atlas_paths(&self) -> crate::atlas::layout::path_counters::Counts {
        self.relay
            .current()
            .map(|frame| frame.atlas_paths())
            .unwrap_or_default()
    }

    /// The instance census of the most recently published frame (RFC-0030
    /// §P6): box-class instances, text lines, vector glyphs.
    ///
    /// Three `Vec::len` reads on data that already crossed the frame boundary
    ///, no new field, no new traffic, and nothing that survives the frame it
    /// describes.
    #[must_use]
    pub fn latest_census(&self) -> Census {
        self.primary_frame()
            .map_or_else(Census::default, |frame| Census {
                instances: frame.instance_count(),
                texts: frame.text_count(),
                vectors: frame.vector_count(),
            })
    }

    /// How many text lines the last encoded frame re-shaped, see
    /// [`crate::encoder::EncoderSubsystem::last_text_reshapes`].
    #[must_use]
    pub fn last_text_reshapes(&self) -> usize {
        match &self.render {
            Some(r) => r.gpu.encoder.last_text_reshapes(),
            None => 0,
        }
    }

    /// Whether GPU pass timing is active for this engine (RFC-0013 **P5**),
    /// see [`crate::encoder::EncoderSubsystem::gpu_timing_available`].
    #[must_use]
    pub fn gpu_timing_available(&self) -> bool {
        self.render
            .as_ref()
            .is_some_and(|r| r.gpu.encoder.gpu_timing_available())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.relay.request_shutdown();
        if let Some(handle) = self.logic_handle.take() {
            match handle.join() {
                Ok(()) => {}
                // Resume the logic thread's panic unless we're already
                // unwinding, resuming inside a panic would abort the process
                // (double-panic), so skip re-raising in that case.
                Err(payload) => {
                    if !std::thread::panicking() {
                        std::panic::resume_unwind(payload);
                    }
                }
            }
        }
    }
}

/// Converts physical pixel dimensions + DPI scale factor to a [`Viewport`] in
/// logical pixels.
///
/// The viewport uniform is always in logical pixels so that instance
/// coordinates (rect, radii, etc.) can be authored once and render correctly
/// on every display density.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn logical_viewport(phys_w: u32, phys_h: u32, scale: f64) -> Viewport {
    Viewport::new(phys_w as f32 / scale as f32, phys_h as f32 / scale as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window that is not transparent is opaque when the surface offers it,
    /// whatever else it offers; a transparent one takes a blending mode,
    /// premultiplied first (what the output shader writes), and falls back to
    /// opaque only when none is offered.
    #[test]
    fn a_transparent_surface_gets_a_blending_alpha_mode_when_there_is_one() {
        use wgpu::CompositeAlphaMode::{Auto, Opaque, PostMultiplied, PreMultiplied};
        let offered = [Auto, Opaque, PostMultiplied, PreMultiplied];
        assert_eq!(alpha_mode_for(&offered, false), Opaque);
        assert_eq!(alpha_mode_for(&offered, true), PreMultiplied);
        assert_eq!(
            alpha_mode_for(&[Opaque, PostMultiplied], true),
            PostMultiplied
        );
        assert_eq!(
            alpha_mode_for(&[Opaque], true),
            Opaque,
            "said once on stderr"
        );
        assert_eq!(alpha_mode_for(&[PreMultiplied], false), PreMultiplied);
    }

    /// The engine of a program of several windows exists before its first
    /// window: the logic thread can start and say which windows to open.
    #[test]
    fn an_engine_can_start_before_it_has_a_surface() {
        let mut engine = Engine::without_surface().unwrap();
        assert!(!engine.has_surface());
        assert_eq!(engine.surface_ids().count(), 0);
        assert!(!engine.drop_surface(SurfaceId::new(1)));
        assert!(engine.render_surfaces(&[SurfaceId::new(1)]).is_ok());
        let (tx, _rx) = crossbeam_channel::unbounded();
        engine.set_vector_ack_sender(tx);
        assert!(engine.pending_ack.is_some(), "kept for the GPU state");
        assert!(engine.latest_cpu_telemetry().is_none());
    }

    #[test]
    fn the_engine_draws_in_the_unorm_twin_of_the_surface() {
        use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb};
        // A surface that offers Unorm is taken as it is.
        assert_eq!(
            negotiate_formats(&[Bgra8UnormSrgb, Bgra8Unorm]),
            (Bgra8Unorm, Bgra8Unorm)
        );
        // One that only offers sRGB is taken, and the engine draws in its twin
        // (which `configure` then lists as a view format).
        assert_eq!(
            negotiate_formats(&[Rgba8UnormSrgb]),
            (Rgba8UnormSrgb, Rgba8Unorm)
        );
    }

    /// Asserts two `f32` values are equal within a small tolerance.
    ///
    /// `assert_eq!` on `f32` triggers `clippy::float_cmp`; every value
    /// checked here is either passed straight through `ReactiveLabel`
    /// untouched or produced by a simple division, so an exact bit-pattern
    /// match would normally hold, but this tolerance keeps the intent
    /// (approximate equality) honest rather than relying on that
    /// incidental exactness. Mirrors `atlas::layout::tests::assert_f32_eq`.
    #[track_caller]
    fn assert_f32_eq(actual: f32, expected: f32) {
        let diff = (actual - expected).abs();
        assert!(
            diff < 0.001,
            "expected {expected}, got {actual} (diff = {diff})",
        );
    }

    /// Asserts two `[f32; 4]` colors are equal within [`assert_f32_eq`]'s
    /// tolerance, component by component.
    ///
    /// `assert_eq!` on a float array triggers `clippy::float_cmp` just like
    /// it does on a bare `f32` (the lint looks through array equality), so
    /// every color comparison in this module goes through here instead.
    #[track_caller]
    fn assert_color_eq(actual: [f32; 4], expected: [f32; 4]) {
        for i in 0..4 {
            assert_f32_eq(actual[i], expected[i]);
        }
    }

    /// Builds a `ReactiveLabel` with a fixed, distinctive position/size/color
    /// so tests that only care about text/dirty behaviour don't repeat the
    /// same four literals everywhere.
    fn label_with_text(text: &str) -> ReactiveLabel {
        ReactiveLabel::new(text, 10.0, 20.0, 16.0, [1.0, 0.5, 0.25, 0.75])
    }

    // ── construction: text content ─────────────────────────────────────

    #[test]
    fn new_stores_initial_text() {
        let mut label = label_with_text("hello");
        let line = label.text_line().expect("first tick never fails");
        assert_eq!(line.text, "hello");
    }

    #[test]
    fn new_with_empty_string_text() {
        let mut label = label_with_text("");
        let line = label.text_line().expect("first tick never fails");
        assert_eq!(line.text, "");
    }

    #[test]
    fn new_with_unicode_text() {
        let mut label = label_with_text("Byard, 🦀 ñ");
        let line = label.text_line().expect("first tick never fails");
        assert_eq!(line.text, "Byard, 🦀 ñ");
    }

    #[test]
    fn new_with_long_text() {
        let long = "x".repeat(10_000);
        let mut label = label_with_text(&long);
        let line = label.text_line().expect("first tick never fails");
        assert_eq!(line.text.len(), 10_000);
    }

    #[test]
    fn new_accepts_owned_string() {
        let owned: String = String::from("owned");
        let mut label = ReactiveLabel::new(owned, 0.0, 0.0, 12.0, [0.0, 0.0, 0.0, 1.0]);
        let line = label.text_line().expect("first tick never fails");
        assert_eq!(line.text, "owned");
    }

    #[test]
    fn new_accepts_str_slice() {
        let mut label = ReactiveLabel::new("slice", 0.0, 0.0, 12.0, [0.0, 0.0, 0.0, 1.0]);
        let line = label.text_line().expect("first tick never fails");
        assert_eq!(line.text, "slice");
    }

    // ── construction: position, size, color pass-through ────────────────

    #[test]
    fn new_stores_x_position() {
        let mut label = label_with_text("t");
        let line = label.text_line().expect("first tick never fails");
        assert_f32_eq(line.x, 10.0);
    }

    #[test]
    fn new_stores_y_position() {
        let mut label = label_with_text("t");
        let line = label.text_line().expect("first tick never fails");
        assert_f32_eq(line.y, 20.0);
    }

    #[test]
    fn new_stores_font_size() {
        let mut label = label_with_text("t");
        let line = label.text_line().expect("first tick never fails");
        assert_f32_eq(line.font_size, 16.0);
    }

    #[test]
    fn new_stores_color() {
        let mut label = label_with_text("t");
        let line = label.text_line().expect("first tick never fails");
        assert_color_eq(line.color, [1.0, 0.5, 0.25, 0.75]);
    }

    #[test]
    fn new_with_zero_font_size() {
        let mut label = ReactiveLabel::new("t", 0.0, 0.0, 0.0, [0.0, 0.0, 0.0, 1.0]);
        let line = label.text_line().expect("first tick never fails");
        assert_f32_eq(line.font_size, 0.0);
    }

    #[test]
    fn new_with_negative_position() {
        let mut label = ReactiveLabel::new("t", -50.0, -75.0, 12.0, [0.0, 0.0, 0.0, 1.0]);
        let line = label.text_line().expect("first tick never fails");
        assert_f32_eq(line.x, -50.0);
        assert_f32_eq(line.y, -75.0);
    }

    #[test]
    fn new_with_alpha_zero_color() {
        let mut label = ReactiveLabel::new("t", 0.0, 0.0, 12.0, [1.0, 1.0, 1.0, 0.0]);
        let line = label.text_line().expect("first tick never fails");
        assert_color_eq(line.color, [1.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn new_with_all_color_components_at_max() {
        let mut label = ReactiveLabel::new("t", 0.0, 0.0, 12.0, [1.0, 1.0, 1.0, 1.0]);
        let line = label.text_line().expect("first tick never fails");
        assert_color_eq(line.color, [1.0, 1.0, 1.0, 1.0]);
    }

    // ── dirty-flag lifecycle ──────────────────────────────────────────

    #[test]
    fn first_text_line_is_not_dirty() {
        let mut label = label_with_text("t");
        let line = label.text_line().expect("first tick never fails");
        assert!(
            !line.dirty,
            "a label that was never written to has nothing to mark dirty"
        );
    }

    #[test]
    fn first_text_line_returns_ok() {
        let mut label = label_with_text("t");
        assert!(label.text_line().is_ok());
    }

    #[test]
    fn set_text_marks_next_tick_dirty() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let line = label.text_line().expect("tick after a write never fails");
        assert!(line.dirty);
    }

    #[test]
    fn set_text_updates_text_content() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let line = label.text_line().expect("tick after a write never fails");
        assert_eq!(line.text, "after");
    }

    #[test]
    fn set_text_does_not_change_x() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let line = label.text_line().expect("tick after a write never fails");
        assert_f32_eq(line.x, 10.0);
    }

    #[test]
    fn set_text_does_not_change_y() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let line = label.text_line().expect("tick after a write never fails");
        assert_f32_eq(line.y, 20.0);
    }

    #[test]
    fn set_text_does_not_change_font_size() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let line = label.text_line().expect("tick after a write never fails");
        assert_f32_eq(line.font_size, 16.0);
    }

    #[test]
    fn set_text_does_not_change_color() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let line = label.text_line().expect("tick after a write never fails");
        assert_color_eq(line.color, [1.0, 0.5, 0.25, 0.75]);
    }

    #[test]
    fn second_tick_after_dirty_tick_is_clean() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let first = label.text_line().expect("tick after a write never fails");
        assert!(first.dirty);

        let second = label.text_line().expect("tick with no writes never fails");
        assert!(
            !second.dirty,
            "no write happened between the two ticks, so the second must be clean"
        );
    }

    #[test]
    fn second_tick_after_dirty_tick_keeps_the_same_text() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let _ = label.text_line().expect("tick after a write never fails");
        let second = label.text_line().expect("tick with no writes never fails");
        assert_eq!(second.text, "after");
    }

    #[test]
    fn multiple_writes_between_ticks_yield_a_single_dirty_tick() {
        let mut label = label_with_text("v0");
        label.set_text("v1");
        label.set_text("v2");
        label.set_text("v3");

        let line = label
            .text_line()
            .expect("tick after several writes never fails");
        assert!(line.dirty);

        let next = label.text_line().expect("tick with no writes never fails");
        assert!(
            !next.dirty,
            "the three writes must collapse into exactly one dirty tick"
        );
    }

    #[test]
    fn multiple_writes_between_ticks_yield_the_latest_text() {
        let mut label = label_with_text("v0");
        label.set_text("v1");
        label.set_text("v2");
        label.set_text("v3");

        let line = label
            .text_line()
            .expect("tick after several writes never fails");
        assert_eq!(line.text, "v3");
    }

    #[test]
    fn set_text_with_same_value_still_marks_dirty() {
        // A Signal::write always advances the version counter, even when the
        // new value is identical to the old one, there is no value-equality
        // short-circuit anywhere in the Evaluator. This is intentional (see
        // RFC-0001 §2.2): cheaply comparing arbitrary `T` would not be
        // possible in general, and skipping it keeps the pipeline simple.
        let mut label = label_with_text("same");
        label.set_text("same");
        let line = label.text_line().expect("tick after a write never fails");
        assert!(line.dirty);
        assert_eq!(line.text, "same");
    }

    #[test]
    fn set_text_accepts_owned_string() {
        let mut label = label_with_text("before");
        let owned: String = String::from("after");
        label.set_text(owned);
        let line = label.text_line().expect("tick after a write never fails");
        assert_eq!(line.text, "after");
    }

    #[test]
    fn set_text_accepts_str_slice() {
        let mut label = label_with_text("before");
        label.set_text("after");
        let line = label.text_line().expect("tick after a write never fails");
        assert_eq!(line.text, "after");
    }

    #[test]
    fn set_text_accepts_formatted_string() {
        let mut label = label_with_text("before");
        label.set_text(format!("clicked {} time(s)", 3));
        let line = label.text_line().expect("tick after a write never fails");
        assert_eq!(line.text, "clicked 3 time(s)");
    }

    #[test]
    fn sequential_set_text_calls_apply_in_order() {
        let mut label = label_with_text("start");
        for n in 1..=5 {
            label.set_text(format!("step {n}"));
            let line = label.text_line().expect("each tick never fails");
            assert_eq!(line.text, format!("step {n}"));
            assert!(line.dirty, "each step wrote, so each tick must be dirty");
        }
    }

    #[test]
    fn alternating_write_and_tick_toggles_dirty_each_time() {
        let mut label = label_with_text("v0");
        for n in 1..=4 {
            label.set_text(format!("v{n}"));
            let dirty_line = label.text_line().expect("tick after a write never fails");
            assert!(
                dirty_line.dirty,
                "iteration {n}: expected dirty after write"
            );

            let clean_line = label.text_line().expect("tick with no writes never fails");
            assert!(
                !clean_line.dirty,
                "iteration {n}: expected clean with no intervening write"
            );
        }
    }

    #[test]
    fn repeated_ticks_with_no_writes_stay_clean() {
        let mut label = label_with_text("static");
        // Consume the initial (clean) state, then tick several more times
        // with no writes in between, every one must stay clean.
        for _ in 0..10 {
            let line = label.text_line().expect("tick with no writes never fails");
            assert!(!line.dirty);
            assert_eq!(line.text, "static");
        }
    }

    #[test]
    fn text_line_can_be_called_many_times_consecutively() {
        let mut label = label_with_text("t");
        for _ in 0..100 {
            assert!(label.text_line().is_ok());
        }
    }

    // ── independence between separate labels ─────────────────────────

    #[test]
    fn two_labels_have_independent_text() {
        let mut a = label_with_text("a-text");
        let mut b = label_with_text("b-text");
        assert_eq!(a.text_line().unwrap().text, "a-text");
        assert_eq!(b.text_line().unwrap().text, "b-text");
    }

    #[test]
    fn two_labels_have_independent_dirty_state() {
        let mut a = label_with_text("a");
        let mut b = label_with_text("b");

        a.set_text("a2");
        let a_line = a.text_line().expect("tick after a write never fails");
        let b_line = b.text_line().expect("tick with no writes never fails");

        assert!(a_line.dirty, "a was written to");
        assert!(!b_line.dirty, "b was never touched");
    }

    #[test]
    fn set_text_on_one_label_does_not_affect_another() {
        let mut a = label_with_text("a");
        let mut b = label_with_text("b");

        a.set_text("changed");
        let _ = a.text_line().expect("tick after a write never fails");
        let b_line = b.text_line().expect("tick with no writes never fails");

        assert_eq!(b_line.text, "b", "writing to `a` must not leak into `b`");
    }

    // ── logical_viewport: pure conversion helper ──────────────────────

    #[test]
    fn logical_viewport_identity_at_scale_one() {
        let viewport = logical_viewport(800, 600, 1.0);
        assert_f32_eq(viewport.width, 800.0);
        assert_f32_eq(viewport.height, 600.0);
    }

    #[test]
    fn logical_viewport_halves_dimensions_at_scale_two() {
        let viewport = logical_viewport(800, 600, 2.0);
        assert_f32_eq(viewport.width, 400.0);
        assert_f32_eq(viewport.height, 300.0);
    }

    #[test]
    fn logical_viewport_scales_fractionally() {
        // A 1.5x DPI scale (common on some Windows/Linux HiDPI setups).
        let viewport = logical_viewport(1200, 900, 1.5);
        assert_f32_eq(viewport.width, 800.0);
        assert_f32_eq(viewport.height, 600.0);
    }

    #[test]
    fn logical_viewport_zero_dimensions_stay_zero() {
        let viewport = logical_viewport(0, 0, 2.0);
        assert_f32_eq(viewport.width, 0.0);
        assert_f32_eq(viewport.height, 0.0);
    }

    #[test]
    fn logical_viewport_large_dimensions_do_not_overflow() {
        // 8K physical resolution at 1x scale, comfortably within f32's
        // 24-bit mantissa (RFC-0001 already accepts this precision
        // trade-off for viewport math; see `Engine::init`'s `cast_precision_loss`
        // allow).
        let viewport = logical_viewport(7680, 4320, 1.0);
        assert_f32_eq(viewport.width, 7680.0);
        assert_f32_eq(viewport.height, 4320.0);
    }
}

/// Reports changed image files to the render thread (see
/// [`Engine::image_reloader`]). Cheap to clone and `Send`.
#[derive(Clone)]
pub struct ImageReloader {
    tx: crossbeam_channel::Sender<std::path::PathBuf>,
    wake: crate::relay::FrameWaker,
}

impl ImageReloader {
    /// The file at `path` changed: decode every image drawn from it again.
    pub fn reload(&self, path: std::path::PathBuf) {
        if self.tx.send(path).is_ok() {
            (self.wake)();
        }
    }
}
