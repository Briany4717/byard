//! `TextureSampler` render pipeline (RFC-0001 §3.1).
//!
//! Draws a decoded image into a (optionally rounded) quad with a `fit` policy.
//! Host-side decode uses the `image` crate: the runner owns decode, the
//! interpreter only carries the `Str` path. Decoded textures are cached by path
//! so a static image is uploaded once.

use std::any::Any;
use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedSender;

use crate::ByardError;
use crate::frame::{ImageFit, TextureSampler};

/// Type-erased decode-result channel sender, structurally identical to
/// `relay::DecodeResult`'s sender. Spelled out here (rather than imported
/// from `relay`) so the encoder never gains a dependency on the relay
/// subsystem, the async-decode result flows through the existing type-erased
/// channel, not a new cross-module call (RFC-0001 §9: subsystems meet only at `frame.rs`).
///
/// This is the **render** thread's half of the split channel (RFC-0028 §7): a
/// decoded image is addressed to the thread that owns the `Device`/`Queue`,
/// and never reaches the logic thread's controller-reply drain.
///
/// It carries the render loop's wake with it. A decode finishes with no
/// input and no publish behind it, so an event-driven (`Wait`) loop that is
/// not told would leave the image undrawn until the pointer next moved.
#[derive(Clone)]
pub struct DecodeResultSender {
    tx: UnboundedSender<Box<dyn Any + Send>>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl DecodeResultSender {
    /// A sender that queues on `tx` and then calls `wake`.
    #[must_use]
    pub fn new(
        tx: UnboundedSender<Box<dyn Any + Send>>,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self { tx, wake }
    }

    /// Queues a result for the render thread and wakes its loop. A dropped
    /// receiver (shutdown) is fine: there is nothing left to paint into.
    fn send(&self, result: Box<dyn Any + Send>) {
        if self.tx.send(result).is_ok() {
            (self.wake)();
        }
    }
}

/// Raw decoded RGBA8 pixels produced off the render thread by the I/O pool
/// Carries no `wgpu` handles, `Device`/`Queue` are used only on their
/// owning (render) thread, where [`TextureCache::apply_decoded`] performs the
/// upload.
#[derive(Debug)]
pub struct DecodedRgba {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Tightly packed RGBA8 rows (`4 * width * height` bytes).
    pub bytes: Vec<u8>,
}

/// The result of one async decode, sent back through the I/O channel and
/// drained on the render thread. `result` is `Err(message)` on a missing or
/// corrupt file, the message is logged once when the entry transitions to
/// `TextureState::Failed`.
#[derive(Debug)]
pub struct DecodedImage {
    /// The source path/key this decode was started for.
    pub src: String,
    /// Decoded pixels, or the human-readable decode error.
    pub result: Result<DecodedRgba, String>,
}

/// GPU-side per-instance data for the `TextureSampler` pipeline; matches
/// `texture_sampler.wgsl`'s `InstanceInput`.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TextureInstance {
    /// `[x, y, width, height]` in logical pixels.
    pub rect: [f32; 4],
    /// Per-corner radii `[tl, tr, br, bl]`.
    pub radii: [f32; 4],
    /// `[uv_scale_x, uv_scale_y, uv_offset_x, uv_offset_y]`, the `fit` transform.
    pub uv_xform: [f32; 4],
    /// `[opacity, depth, smooth, 0]`, `misc.z` is the RFC-0031 §S1 corner
    /// smoothing of the image's rounded clip.
    pub misc: [f32; 4],
}

impl TextureInstance {
    /// Vertex buffer layout (locations 1..=4; the static quad occupies 0).
    #[must_use]
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![
            1 => Float32x4, // rect
            2 => Float32x4, // radii
            3 => Float32x4, // uv_xform
            4 => Float32x4, // misc
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TextureInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: ATTRS,
        }
    }
}

/// One decoded, uploaded texture plus its sampling bind group.
pub struct TextureEntry {
    /// Bind group (group 1): the texture view + sampler.
    pub bind_group: wgpu::BindGroup,
    /// Source pixel width.
    pub width: u32,
    /// Source pixel height.
    pub height: u32,
}

/// Lifecycle of a single cached texture.
///
/// Replaces the old `Option<TextureEntry>` (which conflated "decode failed"
/// with "not yet decoded"). The render thread observes `Pending` for a freshly
/// requested image while its decode runs on the I/O pool, then `Ready` once
/// [`TextureCache::apply_decoded`] uploads it, or `Failed` on a bad/corrupt
/// source.
enum TextureState {
    /// Decode spawned on the I/O pool; not yet uploaded. Draws nothing.
    Pending,
    /// Decoded and uploaded; ready to sample.
    Ready(TextureEntry),
    /// Decode failed (logged once). Draws nothing.
    Failed,
    /// The file changed on disk and is being decoded again (dev hot reload).
    /// The previous texture keeps drawing until the new one is uploaded, so
    /// an edited image swaps in place instead of blinking out.
    Reloading(TextureEntry),
}

/// The default ceiling on the VRAM the decoded images may hold, in bytes.
///
/// 128 MiB is 32 full-HD photographs, or about 500 of the 512x512 thumbnails a
/// gallery shows. It is a ceiling and not a reservation: an app with a few
/// images never gets near it and pays one comparison per frame to know so. It
/// is deliberately below what an integrated GPU, which shares system memory
/// with everything else, can afford to give a UI toolkit. An app that really
/// shows more at once is not cut off (see [`TextureCache`]); it can raise the
/// budget with [`EncoderSubsystem::set_texture_budget`](super::EncoderSubsystem::set_texture_budget).
pub const DEFAULT_TEXTURE_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

/// How many frames an image must go undrawn before the cache may evict it.
///
/// Only matters once the budget is exceeded. A grace period, so a picture that
/// is hidden for a moment (a tab switch, a list row scrolling out and back by a
/// pixel of jitter) is not thrown away and decoded again; half a second at 60 Hz.
const EVICT_AFTER_IDLE_FRAMES: u64 = 30;

/// One cache entry: the state, the frame it was last asked for, and the VRAM it
/// holds.
struct Slot {
    state: TextureState,
    /// The cache's frame counter the last time a frame referenced this image.
    last_used: u64,
    /// `4 * width * height` while a texture is held (`Ready` / `Reloading`),
    /// zero otherwise.
    bytes: u64,
}

fn texture_bytes(entry: &TextureEntry) -> u64 {
    4 * u64::from(entry.width) * u64::from(entry.height)
}

/// Picks which idle entries to evict, oldest first, until `resident` fits
/// `budget`.
///
/// `idle` holds `(index, last_used, bytes)` for every entry that is *allowed*
/// to go: the caller has already left out anything pending, reloading, failed
/// or used within the grace period, and so anything drawn this frame. If those
/// are not enough to reach the budget it returns all it can and stops: a frame
/// that shows more than the budget is allowed to, because the alternative is
/// to evict what is on screen and decode it again next frame, forever.
fn choose_evictions(idle: &mut [(usize, u64, u64)], mut resident: u64, budget: u64) -> Vec<usize> {
    if resident <= budget {
        return Vec::new();
    }
    idle.sort_unstable_by_key(|&(_, last_used, _)| last_used);
    let mut victims = Vec::new();
    for &(index, _, bytes) in idle.iter() {
        if resident <= budget {
            break;
        }
        resident = resident.saturating_sub(bytes);
        victims.push(index);
    }
    victims
}

/// Path-keyed cache of decoded textures so a static image uploads once, within
/// a VRAM budget.
///
/// Decode is **asynchronous** (RFC-0001 §5.1): [`ensure`](Self::ensure)
/// never touches the filesystem on the calling (render) thread, it inserts a
/// `TextureState::Pending` marker and spawns the blocking `image::open` decode
/// on the relay's I/O runtime. The decoded pixels return through the type-erased
/// I/O channel and are uploaded by [`apply_decoded`](Self::apply_decoded), which
/// runs on the render thread (where the `wgpu` `Device`/`Queue` live).
///
/// # Residency
///
/// A texture stays until the cache holds more than its budget of them, and then
/// only the ones **no frame has asked for lately** go, least recently asked for
/// first. Every frame calls [`begin_frame`](Self::begin_frame), asks for each
/// image it draws with [`ensure`](Self::ensure) (which stamps it with that
/// frame) and then [`trim`](Self::trim)s. An image drawn this frame, or within
/// the last 30, is never evicted, so a screen showing
/// more images than the budget holds goes over it rather than thrash; it comes
/// back under as soon as the view changes. A decode in flight is never
/// evicted either, so its result always has an entry to land in.
pub struct TextureCache {
    entries: HashMap<String, Slot>,
    /// Frames begun, the clock `last_used` is read against.
    frame: u64,
    /// Sum of `bytes` over `entries`.
    resident_bytes: u64,
    budget_bytes: u64,
    /// Entries evicted so far, for tests and telemetry.
    evictions: u64,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::with_budget(DEFAULT_TEXTURE_BUDGET_BYTES)
    }
}

impl TextureCache {
    /// A cache that evicts idle textures once they hold more than
    /// `budget_bytes` of VRAM.
    #[must_use]
    pub fn with_budget(budget_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            frame: 0,
            resident_bytes: 0,
            budget_bytes,
            evictions: 0,
        }
    }

    /// Changes the budget. Takes effect at the next [`trim`](Self::trim).
    pub fn set_budget(&mut self, budget_bytes: u64) {
        self.budget_bytes = budget_bytes;
    }

    /// VRAM held by decoded textures, in bytes.
    #[must_use]
    pub const fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// The budget [`trim`](Self::trim) holds idle textures to.
    #[must_use]
    pub const fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// How many entries (in any state) the cache holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many textures have been evicted so far.
    #[must_use]
    pub const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Starts a frame. Every image the frame draws must be
    /// [`ensure`](Self::ensure)d after this, and [`trim`](Self::trim) called
    /// once they all have been.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Requests `src` without blocking: if unseen, inserts a `Pending` marker
    /// and spawns the decode on `io_handle`, returning immediately. A second
    /// call for a still-`Pending` (or already-resolved) `src` is a no-op apart
    /// from recording that this frame uses it, the `contains_key` guard
    /// ensures exactly one decode task per source.
    ///
    /// Decode happens entirely off the calling thread (the render and logic
    /// threads never block on I/O); only the cheap GPU upload, later, runs on
    /// the render thread.
    pub fn ensure(
        &mut self,
        io_handle: &tokio::runtime::Handle,
        io_tx: &DecodeResultSender,
        src: &str,
    ) {
        if self.mark_used(src) {
            return;
        }
        self.entries.insert(
            src.to_string(),
            Slot {
                state: TextureState::Pending,
                last_used: self.frame,
                bytes: 0,
            },
        );
        spawn_decode(io_handle, io_tx, src);
    }

    /// Records that the current frame uses `src`, returning whether it is known.
    pub fn mark_used(&mut self, src: &str) -> bool {
        match self.entries.get_mut(src) {
            Some(slot) => {
                slot.last_used = self.frame;
                true
            }
            None => false,
        }
    }

    /// Evicts textures to bring the cache back under its budget, if it is over
    /// it: only `Ready` ones that no frame has used for
    /// 30 frames, least recently used first. If everything
    /// held is in use the cache stays over budget. A cache under budget does
    /// nothing, and costs one comparison.
    pub fn trim(&mut self) {
        if self.resident_bytes <= self.budget_bytes {
            return;
        }
        let frame = self.frame;
        let keys: Vec<&String> = self.entries.keys().collect();
        let mut idle: Vec<(usize, u64, u64)> = keys
            .iter()
            .enumerate()
            .filter_map(|(i, key)| {
                let slot = &self.entries[*key];
                let evictable = matches!(slot.state, TextureState::Ready(_))
                    && frame.saturating_sub(slot.last_used) >= EVICT_AFTER_IDLE_FRAMES;
                evictable.then_some((i, slot.last_used, slot.bytes))
            })
            .collect();
        let victims: Vec<String> =
            choose_evictions(&mut idle, self.resident_bytes, self.budget_bytes)
                .into_iter()
                .map(|i| keys[i].clone())
                .collect();
        for key in victims {
            if let Some(slot) = self.entries.remove(&key) {
                self.resident_bytes = self.resident_bytes.saturating_sub(slot.bytes);
                self.evictions += 1;
            }
        }
    }

    /// Decodes again every entry that is the file at `changed` (dev hot
    /// reload), returning how many. An entry drawn so far keeps drawing until
    /// the new pixels are uploaded; one that had failed (the file was missing
    /// or broken) is simply retried. An entry still `Pending` is left alone:
    /// its decode reads the file when it runs, which is after the change. An
    /// entry the cache already evicted needs nothing: the next frame that draws
    /// it decodes the file as it now is.
    ///
    /// Matched by file rather than by string, since a view names the image
    /// relative to where the app runs and a watcher reports it absolute.
    pub fn reload(
        &mut self,
        io_handle: &tokio::runtime::Handle,
        io_tx: &DecodeResultSender,
        changed: &std::path::Path,
    ) -> usize {
        let changed = std::fs::canonicalize(changed).unwrap_or_else(|_| changed.to_path_buf());
        let same = |src: &str| {
            std::fs::canonicalize(src)
                .map_or_else(|_| std::path::Path::new(src) == changed, |p| p == changed)
        };
        let mut reloaded = 0;
        for (src, slot) in &mut self.entries {
            if !same(src) {
                continue;
            }
            slot.state = match std::mem::replace(&mut slot.state, TextureState::Pending) {
                TextureState::Ready(entry) | TextureState::Reloading(entry) => {
                    TextureState::Reloading(entry)
                }
                TextureState::Failed => TextureState::Pending,
                TextureState::Pending => continue,
            };
            spawn_decode(io_handle, io_tx, src);
            reloaded += 1;
        }
        reloaded
    }

    /// Uploads an async decode result on the render thread, transitioning the
    /// entry to `Ready` (on success) or `Failed` (on a decode error, logged
    /// once). Idempotent if the entry was already resolved.
    pub fn apply_decoded(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        decoded: DecodedImage,
    ) {
        let previous = self.entries.remove(&decoded.src);
        let (last_used, held) = previous
            .as_ref()
            .map_or((self.frame, 0), |p| (p.last_used, p.bytes));
        self.resident_bytes = self.resident_bytes.saturating_sub(held);
        let state = match decoded.result {
            Ok(rgba) => TextureState::Ready(upload_rgba(device, queue, layout, sampler, &rgba)),
            // A reload that could not be read, most often a file caught half
            // written by the editor saving it: keep drawing what was there.
            // The save that completes it is another change, and another try.
            Err(err)
                if matches!(
                    previous,
                    Some(Slot {
                        state: TextureState::Reloading(_),
                        ..
                    })
                ) =>
            {
                eprintln!(
                    "byard: warning: image changed but could not be decoded, keeping the previous one: '{}': {err}",
                    decoded.src
                );
                match previous.map(|p| p.state) {
                    Some(TextureState::Reloading(entry)) => TextureState::Ready(entry),
                    _ => TextureState::Failed,
                }
            }
            Err(err) => {
                // A missing/corrupt image simply does not draw. The
                // warning fires once per path (the entry is now `Failed`, so
                // `ensure` never re-spawns it).
                eprintln!(
                    "byard: warning: image not found or could not be decoded: '{}': {err}",
                    decoded.src
                );
                TextureState::Failed
            }
        };
        let bytes = match &state {
            TextureState::Ready(entry) | TextureState::Reloading(entry) => texture_bytes(entry),
            TextureState::Pending | TextureState::Failed => 0,
        };
        self.resident_bytes += bytes;
        self.entries.insert(
            decoded.src,
            Slot {
                state,
                last_used,
                bytes,
            },
        );
    }

    /// Looks up a `Ready` entry. Returns `None` for both `Pending` and
    /// `Failed`, a not-yet-loaded image draws nothing, exactly like a missing
    /// one, so callers need no new pending-state handling.
    #[must_use]
    pub fn get(&self, src: &str) -> Option<&TextureEntry> {
        match self.entries.get(src).map(|slot| &slot.state) {
            Some(TextureState::Ready(entry) | TextureState::Reloading(entry)) => Some(entry),
            _ => None,
        }
    }
}

/// Spawns the decode of `src` on the I/O pool; the result comes back through
/// `tx`, which also wakes the render loop.
fn spawn_decode(io_handle: &tokio::runtime::Handle, tx: &DecodeResultSender, src: &str) {
    let src_owned = src.to_string();
    let tx = tx.clone();
    io_handle.spawn(async move {
        let result = decode_rgba(&src_owned);
        // The receiver (the render thread) may already be gone on shutdown;
        // a dropped send is fine, nothing left to paint into.
        tx.send(Box::new(DecodedImage {
            src: src_owned,
            result,
        }));
    });
}

/// Decodes an image file to RGBA8 **off the render thread** (no `wgpu` calls).
///
/// Blocking file read + CPU decode; this is what the I/O pool runs. The result
/// is uploaded later by [`upload_rgba`]. `pub(crate)` so the encoder's
/// no-relay fallback path can decode synchronously.
pub(crate) fn decode_rgba(src: &str) -> Result<DecodedRgba, String> {
    let img = image::open(src).map_err(|err| {
        let cwd =
            std::env::current_dir().map_or_else(|_| "?".to_string(), |p| p.display().to_string());
        format!("searched relative to {cwd}: {err}")
    })?;
    let img = img.to_rgba8();
    let (width, height) = img.dimensions();
    Ok(DecodedRgba {
        width,
        height,
        bytes: img.into_raw(),
    })
}

/// Uploads decoded RGBA8 pixels to a fresh GPU texture + bind group. Runs on
/// the render thread (the only place `Device`/`Queue` may be used).
fn upload_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    rgba: &DecodedRgba,
) -> TextureEntry {
    let DecodedRgba {
        width,
        height,
        bytes,
    } = rgba;
    let (width, height) = (*width, *height);
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ByardCore - Sampled Image Texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),
            rows_per_image: Some(height),
        },
        size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ByardCore - Texture Bind Group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    TextureEntry {
        bind_group,
        width,
        height,
    }
}

/// Builds the texture+sampler bind group layout (group 1).
#[must_use]
pub fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ByardCore - Texture Bind Group Layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
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

/// Computes the `[scale_x, scale_y, offset_x, offset_y]` UV transform that
/// realizes `fit` for a `(img_w × img_h)` image inside a `(rect_w × rect_h)`
/// rectangle. UVs outside `[0,1]` are discarded by the shader (letterbox bars).
#[must_use]
#[allow(clippy::cast_precision_loss)] // image dimensions are small; f32 is exact in practice.
pub fn uv_transform(fit: ImageFit, img_w: u32, img_h: u32, rect_w: f32, rect_h: f32) -> [f32; 4] {
    if img_w == 0 || img_h == 0 || rect_w <= 0.0 || rect_h <= 0.0 {
        return [1.0, 1.0, 0.0, 0.0];
    }
    let img_aspect = img_w as f32 / img_h as f32;
    let rect_aspect = rect_w / rect_h;
    match fit {
        ImageFit::Fill => [1.0, 1.0, 0.0, 0.0],
        ImageFit::Cover => {
            // Sample a centered sub-rectangle so the image covers the rect.
            if img_aspect > rect_aspect {
                let s = rect_aspect / img_aspect;
                [s, 1.0, (1.0 - s) / 2.0, 0.0]
            } else {
                let s = img_aspect / rect_aspect;
                [1.0, s, 0.0, (1.0 - s) / 2.0]
            }
        }
        ImageFit::Contain => {
            // Fit the whole image inside; the quad over-extends on the
            // letterboxed axis so the bars fall outside [0,1] and discard.
            if img_aspect > rect_aspect {
                let s = img_aspect / rect_aspect;
                [1.0, s, 0.0, (1.0 - s) / 2.0]
            } else {
                let s = rect_aspect / img_aspect;
                [s, 1.0, (1.0 - s) / 2.0, 0.0]
            }
        }
        ImageFit::None => {
            // Natural pixel size, top-left aligned.
            [rect_w / img_w as f32, rect_h / img_h as f32, 0.0, 0.0]
        }
    }
}

/// Builds the linear-filtering sampler shared by all sampled images.
#[must_use]
pub fn sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ByardCore - Image Sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    })
}

/// Compiles the WGSL shader and assembles the `TextureSampler` pipeline inside a
/// single validation error scope (RFC-0001 §8).
///
/// # Errors
///
/// [`ByardError::PipelineCompilation`] on GPU-side validation failure.
pub async fn build_pipeline(
    device: &wgpu::Device,
    viewport_layout: &wgpu::BindGroupLayout,
    texture_layout: &wgpu::BindGroupLayout,
    quad_layout: wgpu::VertexBufferLayout<'static>,
    surface_format: wgpu::TextureFormat,
) -> Result<wgpu::RenderPipeline, ByardError> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ByardCore - TextureSampler Pipeline Layout"),
        bind_group_layouts: &[Some(viewport_layout), Some(texture_layout)],
        immediate_size: 0,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ByardCore - TextureSampler WGSL Shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(format!(
            "{}
{}",
            include_str!("clip.wgsl"),
            include_str!("texture_sampler.wgsl")
        ))),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ByardCore - TextureSampler Render Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[quad_layout, TextureInstance::layout()],
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
        depth_stencil: Some(super::draw_depth_stencil()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    if let Some(error) = scope.pop().await {
        return Err(ByardError::PipelineCompilation {
            pipeline: "TextureSampler".to_string(),
            reason: error.to_string(),
        });
    }

    Ok(pipeline)
}

/// One staged image: which `TextureSampler` it is, the scissor it draws
/// under, and where its instance landed in the arena.
#[derive(Debug, Clone, Copy)]
pub struct StagedImage {
    /// Index into the caller's `textures` slice.
    pub index: usize,
    /// Content-clip scissor (RFC-0005), resolved at staging time.
    pub scissor: super::Scissor,
    /// This image's single-instance region in the arena.
    pub region: super::instance_arena::Region,
}

/// Stages every drawable image's instance into the frame's arena
/// (RFC-0033 §G1), skipping sources that have not decoded and images
/// scrolled fully out of their content clip.
///
/// One region per image rather than one for the batch: each image needs its
/// own texture bind group, so each is its own draw call anyway.
#[allow(clippy::too_many_arguments)]
pub fn stage(
    arena: &mut super::instance_arena::InstanceArena,
    out: &mut Vec<StagedImage>,
    cache: &TextureCache,
    textures: &[TextureSampler],
    depths: &[f32],
    clip_slice: &[Option<u16>],
    ctx: super::ClipCtx<'_>,
) {
    for (i, t) in textures.iter().enumerate() {
        let Some(entry) = cache.get(&t.src) else {
            continue;
        };
        // Content clip (RFC-0005): scissor this image to its ScrollView
        // viewport; skip it entirely if scrolled fully out of view.
        let Some(scissor) = super::clip_scissor(ctx, clip_slice.get(i).copied().flatten()) else {
            continue;
        };
        let uv_xform = uv_transform(t.fit, entry.width, entry.height, t.rect[2], t.rect[3]);
        // misc.y carries this image's draw-order depth (NDC-z); the shader reads
        // it as position.z. Missing depth falls back to the far plane.
        let depth = depths
            .get(i)
            .copied()
            .unwrap_or(crate::frame::DRAW_DEPTH_CLEAR);
        let instance = TextureInstance {
            rect: t.rect,
            radii: t.radii,
            uv_xform,
            misc: [t.opacity, depth, t.smooth, 0.0],
        };
        let region = arena.push_vertex(std::slice::from_ref(&instance));
        out.push(StagedImage {
            index: i,
            scissor,
            region,
        });
    }
}

/// Draws every staged image. Group 0 (viewport) must already be bound by the
/// caller.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    render_pass: &mut wgpu::RenderPass<'_>,
    arena: &super::instance_arena::InstanceArena,
    staged: &[StagedImage],
    pipeline: &wgpu::RenderPipeline,
    viewport_bind_group: &wgpu::BindGroup,
    quad_buffer: &wgpu::Buffer,
    cache: &TextureCache,
    textures: &[TextureSampler],
) {
    if staged.is_empty() {
        return;
    }
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, viewport_bind_group, &[super::clip_offset(None)]);
    render_pass.set_vertex_buffer(0, quad_buffer.slice(..));

    for image in staged {
        // `cache.get` succeeded during staging and the cache is not mutated
        // between the two passes, so this lookup cannot fail.
        let Some(entry) = textures.get(image.index).and_then(|t| cache.get(&t.src)) else {
            continue;
        };
        let (sx, sy, sw, sh) = image.scissor;
        render_pass.set_scissor_rect(sx, sy, sw, sh);
        render_pass.set_bind_group(1, &entry.bind_group, &[]);
        render_pass.set_vertex_buffer(1, arena.slice(image.region));
        render_pass.draw(0..4, 0..1);
    }
}

/// The registered `TextureSampler` pipeline (RFC-0039).
///
/// Unlike its neighbours it rebinds per image, so its "instances" are staged
/// one image at a time. That is a property of sampling N different textures in
/// one segment, not of the registration: the per-instance record is still one
/// `Pod` type going into the same arena.
pub struct TextureSamplerPipeline {
    pipeline: wgpu::RenderPipeline,
}

impl TextureSamplerPipeline {
    /// Wraps a built pipeline for registration.
    #[must_use]
    pub const fn new(pipeline: wgpu::RenderPipeline) -> Self {
        Self { pipeline }
    }
}

impl super::pipeline::RenderPipeline for TextureSamplerPipeline {
    const NAME: &'static str = "texture_sampler";
    type Instance = TextureInstance;

    fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
        TextureInstance::layout()
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, cx: &super::pipeline::SegmentDraw<'_>) {
        draw(
            pass,
            cx.arena,
            &cx.staged.textures,
            &self.pipeline,
            cx.viewport_bind_group,
            cx.quad_buffer,
            cx.texture_cache,
            cx.textures,
        );
    }

    fn draw_batch(&self, _pass: &mut wgpu::RenderPass<'_>, cx: &super::pipeline::BatchDraw<'_>) {
        // This is the one core pipeline a native view cannot emit into, and
        // the reason is in the pipeline rather than in the ABI: sampling an
        // image means binding *that* image's bind group before the draw, so
        // the unit of work here is one image, not a run of instances. A batch
        // says nothing about which image it wants.
        //
        // A view that draws images registers its own pipeline with its own
        // binding, which is what the extension ABI is for, and it is a better
        // answer than a texture id smuggled through an instance field would
        // be. Said out loud rather than silently drawn as nothing.
        if cx.count > 0 {
            log_unbatchable();
        }
    }
}

/// Names the `texture_sampler` batch attempt once per process.
///
/// Once, because it is a mistake in how an app was assembled: it is either
/// true on every frame or on none, and a per-frame line would bury the frame
/// it first appeared on.
fn log_unbatchable() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "  a native view emitted into `texture_sampler`, which draws one \
             image at a time and cannot take a batch. Register a pipeline that \
             binds the texture the view wants (RFC-0039)."
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Builds a multi-threaded Tokio runtime mirroring `Relay`'s (no time/io
    /// drivers, decode is pure compute, results travel a plain channel).
    fn io_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .build()
            .expect("io runtime")
    }

    /// Writes a `w×h` solid-colour PNG fixture to a unique temp path and
    /// returns it. The caller removes it when done.
    fn write_png_fixture(tag: &str, w: u32, h: u32) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("byard_decode_{tag}_{pid}_{nanos}.png"));
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        img.save(&path).expect("save fixture png");
        path
    }

    /// The render thread never blocks on I/O: `ensure` must return without
    /// doing the decode itself, the blocking `image::open` runs on the I/O
    /// pool, not the calling thread.
    #[test]
    fn ensure_does_not_block_when_decoding_a_slow_fixture() {
        let rt = io_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Any + Send>>();
        let tx = DecodeResultSender::new(tx, std::sync::Arc::new(|| {}));
        // A 1024×1024 PNG: its decode (inflate + unfilter) takes many
        // milliseconds, far longer than the microseconds `ensure` needs just
        // to spawn the task and return.
        let path = write_png_fixture("inv12", 1024, 1024);
        let src = path.to_str().unwrap();

        // Warm the pool so its worker threads are already running, we are
        // timing `ensure`'s enqueue, not Tokio's one-time lazy thread spin-up.
        rt.block_on(async {
            tokio::runtime::Handle::current()
                .spawn(async {})
                .await
                .unwrap();
        });

        let mut cache = TextureCache::default();
        let start = Instant::now();
        cache.ensure(rt.handle(), &tx, src);
        let elapsed = start.elapsed();

        // The load-bearing never-block check is *structural*, not a tight timing
        // bound (CI runners are too jittery for sub-millisecond wall-clock
        // assertions): immediately after `ensure` returns, the decode result
        // has **not** arrived on the channel. A blocking `ensure` (one that ran
        // `image::open` inline before returning) would have already sent its
        // result, so this is empty only because the decode is still in flight
        // on the I/O pool.
        assert!(
            rx.try_recv().is_err(),
            "ensure must not have completed the decode synchronously"
        );
        // The decode is deferred, so nothing is `Ready` yet either.
        assert!(
            cache.get(src).is_none(),
            "image must be Pending (not Ready) immediately after ensure"
        );
        // Secondary sanity bound, deliberately generous: `ensure` only spawns,
        // so it returns far below the multi-ms decode even on a loaded runner.
        assert!(
            elapsed < Duration::from_millis(25),
            "ensure should return promptly (only spawns); took {elapsed:?}"
        );

        // It does complete off-thread, proving the decode actually ran there.
        let received = rt.block_on(rx.recv()).expect("decode result");
        let decoded = received
            .downcast::<DecodedImage>()
            .expect("result is a DecodedImage");
        assert!(decoded.result.is_ok(), "fixture should decode cleanly");

        let _ = std::fs::remove_file(&path);
    }

    /// `ensure` called twice for the same still-`Pending` path must spawn only
    /// one decode task (the `contains_key` guard), so exactly one result lands
    /// on the channel.
    #[test]
    fn ensure_called_twice_for_the_same_pending_path_spawns_one_decode_task() {
        let rt = io_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Any + Send>>();
        let tx = DecodeResultSender::new(tx, std::sync::Arc::new(|| {}));
        let path = write_png_fixture("dedup", 8, 8);
        let src = path.to_str().unwrap();

        let mut cache = TextureCache::default();
        cache.ensure(rt.handle(), &tx, src);
        cache.ensure(rt.handle(), &tx, src);

        // Block until the (single) task completes, then assert the channel is
        // empty, the second `ensure` provably never spawned, since the first
        // inserted `Pending` synchronously before either task could run.
        let _first = rt.block_on(rx.recv()).expect("one decode result");
        assert!(
            rx.try_recv().is_err(),
            "a second decode task must not have been spawned"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Returns the shared `(device, queue)` and the turn to use them, for a real adapter, or `None` headless.
    fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
        byard_test_gpu::device(crate::engine::device_limits)
    }

    /// A texture is `Pending` (drawing nothing) until its decode result is
    /// drained and applied, after which it is `Ready`. Since every primitive is
    /// re-emitted dirty each tick, that `Ready` texture then paints on the next
    /// frame with no extra dirty signal needed.
    #[test]
    fn texture_becomes_ready_after_io_result_drain() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping texture-ready drain test");
            return;
        };
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);

        let rt = io_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Any + Send>>();
        let tx = DecodeResultSender::new(tx, std::sync::Arc::new(|| {}));
        let path = write_png_fixture("ready", 16, 16);
        let src = path.to_str().unwrap();

        let mut cache = TextureCache::default();
        cache.ensure(rt.handle(), &tx, src);
        assert!(cache.get(src).is_none(), "Pending before drain → no draw");

        let received = rt.block_on(rx.recv()).expect("decode result");
        let decoded = *received.downcast::<DecodedImage>().unwrap();
        cache.apply_decoded(&device, &queue, &layout, &smp, decoded);

        assert!(
            cache.get(src).is_some(),
            "Ready after drain → entry available to draw"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Dev hot reload: a changed file is decoded again, the old picture keeps
    /// drawing until the new one lands, and a reload that cannot be decoded
    /// (a file caught half written) keeps the old one rather than blanking.
    /// Matched by file: the watcher reports an absolute path, the view names
    /// the image however it likes.
    #[test]
    fn a_changed_image_is_decoded_again_and_swaps_in_place() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping image reload test");
            return;
        };
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);
        let rt = io_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Any + Send>>();
        let tx = DecodeResultSender::new(tx, std::sync::Arc::new(|| {}));
        let mut drain = |cache: &mut TextureCache| {
            let received = rt.block_on(rx.recv()).expect("decode result");
            cache.apply_decoded(
                &device,
                &queue,
                &layout,
                &smp,
                *received.downcast().unwrap(),
            );
        };

        let path = write_png_fixture("reload", 16, 16);
        // Named the way a view might: through a `..` the watcher never uses.
        let dir = path.parent().unwrap();
        let src = format!(
            "{}/../{}/{}",
            dir.display(),
            dir.file_name().unwrap().to_str().unwrap(),
            path.file_name().unwrap().to_str().unwrap()
        );
        let mut cache = TextureCache::default();
        cache.ensure(rt.handle(), &tx, &src);
        drain(&mut cache);
        assert_eq!(cache.get(&src).map(|e| e.width), Some(16));

        // The file changes: one entry is decoded again, and until the new
        // pixels land the old ones still draw.
        image::RgbaImage::from_pixel(32, 8, image::Rgba([200, 20, 30, 255]))
            .save(&path)
            .unwrap();
        assert_eq!(cache.reload(rt.handle(), &tx, &path), 1);
        assert_eq!(
            cache.get(&src).map(|e| e.width),
            Some(16),
            "old picture meanwhile"
        );
        drain(&mut cache);
        assert_eq!(cache.get(&src).map(|e| (e.width, e.height)), Some((32, 8)));

        // Caught half written: the previous picture stays.
        std::fs::write(&path, b"\x89PNG half").unwrap();
        assert_eq!(cache.reload(rt.handle(), &tx, &path), 1);
        drain(&mut cache);
        assert_eq!(
            cache.get(&src).map(|e| e.width),
            Some(32),
            "kept, not blanked"
        );

        // Another file changing touches nothing.
        let other = write_png_fixture("other", 4, 4);
        assert_eq!(cache.reload(rt.handle(), &tx, &other), 0);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&other);
    }

    /// A bad path decodes to `Failed` (drawing nothing), never a panic, and
    /// `get` keeps returning `None`.
    #[test]
    fn missing_image_resolves_to_failed_not_panic() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping failed-decode test");
            return;
        };
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);
        let mut cache = TextureCache::default();

        let decoded = DecodedImage {
            src: "/no/such/byard/image.png".to_string(),
            result: decode_rgba("/no/such/byard/image.png"),
        };
        assert!(decoded.result.is_err(), "missing file must fail to decode");
        cache.apply_decoded(&device, &queue, &layout, &smp, decoded);
        assert!(cache.get("/no/such/byard/image.png").is_none());
    }

    /// A decode finishes with no input and no publish behind it, so the
    /// sender wakes the render loop itself. Without that an event-driven
    /// window showed the image only after the pointer next moved.
    #[test]
    fn a_finished_decode_wakes_the_render_loop() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Any + Send>>();
        let (woke_tx, woke_rx) = std::sync::mpsc::channel::<()>();
        let woke_tx = std::sync::Mutex::new(woke_tx);
        let tx = DecodeResultSender::new(
            tx,
            std::sync::Arc::new(move || {
                let _ = woke_tx.lock().unwrap().send(());
            }),
        );
        let path = write_png_fixture("wake", 8, 8);
        let mut cache = TextureCache::default();
        cache.ensure(rt.handle(), &tx, path.to_str().unwrap());
        // Waited for, not read: the wake runs on the pool, after the send.
        woke_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the decode woke the render loop");
        assert!(rx.try_recv().is_ok(), "and its result is waiting");
        assert!(woke_rx.try_recv().is_err(), "exactly once");
    }

    // ── Residency: the budget, and not thrashing under it ───────────────────

    /// Sorted-input helper: `(index, last_used, bytes)` for `n` idle entries
    /// of `bytes` each, entry `i` last used at frame `i`.
    fn idle(n: usize, bytes: u64) -> Vec<(usize, u64, u64)> {
        (0..n).map(|i| (i, i as u64, bytes)).collect()
    }

    #[test]
    fn a_cache_under_its_budget_evicts_nothing() {
        assert_eq!(choose_evictions(&mut idle(10, 100), 1_000, 1_000).len(), 0);
        assert_eq!(choose_evictions(&mut idle(10, 100), 900, 1_000).len(), 0);
    }

    #[test]
    fn the_least_recently_used_go_first_and_only_as_many_as_needed() {
        // Ten entries of 100 bytes held, budget 650: four must go, the oldest.
        let mut entries = idle(10, 100);
        entries.reverse(); // order in, must not matter
        let victims = choose_evictions(&mut entries, 1_000, 650);
        assert_eq!(victims, vec![0, 1, 2, 3]);
    }

    #[test]
    fn nothing_in_use_means_nothing_is_evicted_however_far_over_budget() {
        // The caller offers only entries that are allowed to go. A thousand
        // textures on screen at once are none of them offered.
        assert_eq!(
            choose_evictions(&mut [], 1_000 * 1_024, 128 * 1_024).len(),
            0
        );
        // Offered fewer bytes than the overshoot: take them all, stop there.
        let victims = choose_evictions(&mut idle(2, 100), 10_000, 500);
        assert_eq!(victims, vec![0, 1]);
    }

    fn decoded(src: &str, side: u32) -> DecodedImage {
        DecodedImage {
            src: src.to_string(),
            result: Ok(DecodedRgba {
                width: side,
                height: side,
                bytes: vec![200; (4 * side * side) as usize],
            }),
        }
    }

    /// One frame of a view showing `visible`, with decoding instantaneous:
    /// anything the cache does not hold is "decoded" and uploaded now. Returns
    /// how many were (re)decoded, which is the thrash metric.
    fn show(
        cache: &mut TextureCache,
        gpu: &(Arc<wgpu::Device>, Arc<wgpu::Queue>),
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        visible: impl Iterator<Item = usize>,
    ) -> usize {
        cache.begin_frame();
        let mut decodes = 0;
        for i in visible {
            let src = format!("img{i}");
            if !cache.mark_used(&src) {
                cache.apply_decoded(&gpu.0, &gpu.1, layout, sampler, decoded(&src, 16));
                decodes += 1;
            }
        }
        cache.trim();
        decodes
    }

    const TEX: u64 = 4 * 16 * 16; // bytes of one test image

    /// The failure the first design had: more images on screen than the budget
    /// holds. Evicting the older half re-decodes what is being drawn, every
    /// frame, forever. Here a thousand are drawn for a hundred frames and each
    /// is decoded exactly once.
    #[test]
    fn a_view_larger_than_the_budget_does_not_thrash() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping texture residency test");
            return;
        };
        let gpu = (device.clone(), queue);
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);
        let mut cache = TextureCache::with_budget(128 * TEX);

        let mut decodes = 0;
        for _ in 0..100 {
            decodes += show(&mut cache, &gpu, &layout, &smp, 0..1_000);
        }
        assert_eq!(decodes, 1_000, "every image decodes once, then stays");
        assert_eq!(cache.evictions(), 0);
        assert_eq!(
            cache.resident_bytes(),
            1_000 * TEX,
            "over budget, on purpose"
        );
    }

    /// Images the view moved on from go, once they have been idle long enough,
    /// and what is on screen stays.
    #[test]
    fn images_nobody_draws_any_more_are_evicted_and_the_visible_ones_stay() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping texture residency test");
            return;
        };
        let gpu = (device.clone(), queue);
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);
        let mut cache = TextureCache::with_budget(200 * TEX);

        for _ in 0..5 {
            show(&mut cache, &gpu, &layout, &smp, 0..200);
        }
        // Navigate to a different set of 200. The first set is idle but within
        // the grace period, and the cache is over budget: nothing is dropped
        // yet, because it could still come back.
        for _ in 0..5 {
            show(&mut cache, &gpu, &layout, &smp, 1_000..1_200);
        }
        assert_eq!(cache.evictions(), 0, "the first set is within its grace");
        for _ in 0..=EVICT_AFTER_IDLE_FRAMES {
            show(&mut cache, &gpu, &layout, &smp, 1_000..1_200);
        }
        assert_eq!(cache.evictions(), 200, "the first set aged out");
        assert!(cache.get("img0").is_none() && cache.get("img199").is_none());
        assert!((1_000..1_200).all(|i| cache.get(&format!("img{i}")).is_some()));
        assert_eq!(cache.resident_bytes(), 200 * TEX);
    }

    /// The test to run to see the budget hold: a window of 50 images slides
    /// over 3,000, one step a frame, and the VRAM the cache holds never goes
    /// above the budget once it has filled.
    ///
    /// `cargo test -p byard-core --lib images_cycled -- --nocapture` prints the
    /// peak.
    #[test]
    fn images_cycled_through_a_window_stay_under_the_budget() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping texture residency test");
            return;
        };
        let gpu = (device.clone(), queue);
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);
        let budget = 100 * TEX;
        let mut cache = TextureCache::with_budget(budget);

        let mut peak = 0;
        let mut decodes = 0;
        for step in 0..3_000 {
            decodes += show(&mut cache, &gpu, &layout, &smp, step..step + 50);
            peak = peak.max(cache.resident_bytes());
            assert!(
                cache.resident_bytes() <= budget,
                "step {step}: {} bytes held, budget {budget}",
                cache.resident_bytes()
            );
        }
        // Each image enters once as the window reaches it, and none that was
        // still in the window was ever decoded twice.
        assert_eq!(decodes, 3_000 + 49);
        eprintln!(
            "texture cache: peak {peak} bytes of a {budget} byte budget, \
             {} images seen, {} evicted, {} resident",
            decodes,
            cache.evictions(),
            cache.len()
        );
    }

    /// A decode in flight is never evicted, so its result always has an entry
    /// to land in, and a result for an image the cache no longer knows is kept
    /// (and can age out) rather than leaked or lost.
    #[test]
    fn a_pending_decode_survives_trimming_and_its_result_is_not_leaked() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping texture residency test");
            return;
        };
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);
        let rt = io_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Any + Send>>();
        let tx = DecodeResultSender::new(tx, std::sync::Arc::new(|| {}));

        let mut cache = TextureCache::with_budget(0);
        cache.begin_frame();
        cache.ensure(rt.handle(), &tx, "/nonexistent/byard-pending.png");
        for _ in 0..200 {
            cache.begin_frame();
            cache.trim();
        }
        assert_eq!(cache.len(), 1, "a pending entry is not evictable");
        assert_eq!(cache.evictions(), 0);

        let result = rt.block_on(rx.recv()).expect("decode result");
        let result = *result.downcast::<DecodedImage>().expect("a DecodedImage");
        assert!(result.result.is_err());
        cache.apply_decoded(&device, &queue, &layout, &smp, result);
        assert_eq!(cache.len(), 1, "the failure landed in the entry that asked");
        assert_eq!(cache.resident_bytes(), 0);

        // A result for something nobody asked for (or that was dropped) is
        // stored with the current frame, and then ages out like any other.
        cache.apply_decoded(&device, &queue, &layout, &smp, decoded("stray", 16));
        assert_eq!(cache.resident_bytes(), TEX);
        for _ in 0..=EVICT_AFTER_IDLE_FRAMES {
            cache.begin_frame();
        }
        cache.trim();
        assert!(cache.get("stray").is_none());
        assert_eq!(cache.resident_bytes(), 0);
    }

    /// A changed file still reloads with eviction in play: a resident image
    /// swaps in place, and one that was evicted is simply decoded from the new
    /// file the next time a frame draws it.
    #[test]
    fn hot_reload_works_for_resident_and_evicted_images() {
        let Some((device, queue, _turn)) = try_device() else {
            eprintln!("no GPU adapter, skipping texture residency test");
            return;
        };
        let layout = bind_group_layout(&device);
        let smp = sampler(&device);
        let rt = io_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Any + Send>>();
        let tx = DecodeResultSender::new(tx, std::sync::Arc::new(|| {}));
        let path = write_png_fixture("evict_reload", 8, 8);
        let src = path.to_str().unwrap();
        let mut pump = |cache: &mut TextureCache| {
            let r = rt.block_on(rx.recv()).expect("decode result");
            cache.apply_decoded(
                &device,
                &queue,
                &layout,
                &smp,
                *r.downcast::<DecodedImage>().unwrap(),
            );
        };

        let mut cache = TextureCache::with_budget(0);
        cache.begin_frame();
        cache.ensure(rt.handle(), &tx, src);
        pump(&mut cache);
        assert_eq!(cache.get(src).map(|e| e.width), Some(8));

        // Resident: the reload swaps in place and keeps drawing the old pixels
        // meanwhile, and the byte count follows the new texture.
        image::RgbaImage::from_pixel(16, 4, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        assert_eq!(cache.reload(rt.handle(), &tx, &path), 1);
        assert_eq!(
            cache.get(src).map(|e| e.width),
            Some(8),
            "old picture until the new lands"
        );
        cache.begin_frame();
        cache.trim();
        assert_eq!(cache.len(), 1, "a reloading entry is not evictable");
        pump(&mut cache);
        assert_eq!(cache.get(src).map(|e| (e.width, e.height)), Some((16, 4)));
        assert_eq!(cache.resident_bytes(), 4 * 16 * 4);

        // Evicted (zero budget, idle past the grace): nothing to reload, and
        // the next draw decodes the file as it is now.
        for _ in 0..=EVICT_AFTER_IDLE_FRAMES {
            cache.begin_frame();
        }
        cache.trim();
        assert!(cache.get(src).is_none());
        assert_eq!(cache.resident_bytes(), 0);
        image::RgbaImage::from_pixel(5, 5, image::Rgba([9, 9, 9, 255]))
            .save(&path)
            .unwrap();
        assert_eq!(cache.reload(rt.handle(), &tx, &path), 0);
        cache.begin_frame();
        cache.ensure(rt.handle(), &tx, src);
        pump(&mut cache);
        assert_eq!(cache.get(src).map(|e| (e.width, e.height)), Some((5, 5)));

        let _ = std::fs::remove_file(&path);
    }
}
