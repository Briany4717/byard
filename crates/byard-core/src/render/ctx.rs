//! [`RenderCtx`], the bounded per-frame handle a native view draws through
//! (RFC-0039 §"`RenderCtx`", INV-31).
//!
//! # What it exposes, and why the list is short
//!
//! `pipeline`, `emit`, `upload_texture`, `clip`, `request_repaint`. That is
//! the whole surface. It hands out no `wgpu::Device`, no `Queue`, no buffer,
//! and nothing that outlives the `render` call it was passed to.
//!
//! The reason is not caution about what an extension might do wrong. Byard's
//! memory guarantees, bounded VRAM and release of a view's resources in one
//! linear pass at unmount, are properties of the engine owning every GPU
//! resource. A handle that let a view keep one would make those guarantees
//! conditional on every package author's discipline, which is the same as not
//! having them. Here they are properties of the types: a [`TextureHandle`]
//! borrows the frame, so the borrow checker, not a review, is what stops it
//! being stored in the view.

use std::marker::PhantomData;

use super::batch::{ClipShape, NativeBatches, PipelineKey};
use crate::encoder::pipeline::RenderPipeline;

/// A handle to a registered pipeline, obtained from
/// [`RenderCtx::pipeline`].
///
/// Carries the pipeline's type so [`RenderCtx::emit`] can only be handed
/// instances that pipeline actually draws: emitting a `TextInstance` into a
/// mesh pipeline is a type error rather than a garbled frame.
pub struct PipelineHandle<P: RenderPipeline> {
    key: PipelineKey,
    /// `fn() -> P` rather than `P`, so the handle is `Copy`/`Send` on its own
    /// terms and does not inherit anything from the pipeline type.
    _pipeline: PhantomData<fn() -> P>,
}

impl<P: RenderPipeline> Clone for PipelineHandle<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: RenderPipeline> Copy for PipelineHandle<P> {}

impl<P: RenderPipeline> PipelineHandle<P> {
    /// The key the encoder resolves against its registry.
    #[must_use]
    pub const fn key(&self) -> PipelineKey {
        self.key
    }
}

/// Pixels a view wants on the GPU, handed to
/// [`RenderCtx::upload_texture`].
pub enum TextureSource<'a> {
    /// Tightly packed 8-bit RGBA, `width * height * 4` bytes.
    Rgba8 {
        /// Width in physical pixels.
        width: u32,
        /// Height in physical pixels.
        height: u32,
        /// The pixels, row-major, no padding.
        pixels: &'a [u8],
    },
    /// An image file the engine's decode cache already knows how to read, by
    /// path. The decode is asynchronous and off the logic thread, exactly as
    /// it is for an `Image` element.
    Path(&'a str),
}

/// A texture the engine holds for this frame.
///
/// The lifetime is the point. `'frame` is the borrow of the [`RenderCtx`] the
/// handle came from, so a view can put one in an instance it emits this frame
/// and cannot put one in a field of itself (INV-31). The engine owns the
/// texture; this is a way to name it, not a way to keep it.
///
/// A view that tries to keep one does not compile, which is the whole
/// mechanism:
///
/// ```compile_fail
/// use byard_core::render::{NativeBatches, RenderCtx, TextureHandle, TextureSource};
///
/// fn escapes() -> TextureHandle<'static> {
///     let mut pool = NativeBatches::new();
///     let mut textures = Vec::new();
///     let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.0);
///     cx.upload_texture(&TextureSource::Path("tile.png"))
/// }
/// ```
///
/// Within the frame it belongs to, it is as usable as any other id:
///
/// ```
/// use byard_core::render::{NativeBatches, RenderCtx, TextureSource};
///
/// let mut pool = NativeBatches::new();
/// let mut textures = Vec::new();
/// let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.0);
/// let tile = cx.upload_texture(&TextureSource::Path("tile.png"));
/// assert_eq!(tile.id(), 0);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureHandle<'frame> {
    id: u32,
    _frame: PhantomData<&'frame ()>,
}

impl TextureHandle<'_> {
    /// The engine-side id of the texture, which is what a view writes into an
    /// instance field for its shader to sample.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }
}

/// One texture a view asked the engine to make available this frame.
///
/// Recorded rather than uploaded on the spot, because uploading is a `Queue`
/// operation and the queue is on the other side of the frame swap. The encoder
/// drains these in the same pass it drains the atlas uploads that have always
/// worked this way.
#[derive(Debug)]
pub struct TextureRequest {
    /// The id handed back to the view.
    pub id: u32,
    /// What to upload.
    pub source: OwnedTextureSource,
}

/// The owned form of [`TextureSource`], as it crosses to the render thread.
#[derive(Debug)]
pub enum OwnedTextureSource {
    /// Decoded pixels, ready to upload.
    Rgba8 {
        /// Width in physical pixels.
        width: u32,
        /// Height in physical pixels.
        height: u32,
        /// The pixels, row-major, no padding.
        pixels: Vec<u8>,
    },
    /// A path for the decode cache.
    Path(String),
}

/// The per-frame handle a native view's `render` receives (RFC-0039).
///
/// Not `Send`, by construction: it borrows the frame's pools, which the logic
/// thread owns while it assembles a frame (INV-2, INV-12). A view that tries
/// to move one to another thread does not compile, which is the difference
/// between an invariant and a request:
///
/// ```compile_fail
/// use byard_core::render::{NativeBatches, RenderCtx};
///
/// fn only_takes_send<T: Send>(_: T) {}
///
/// let mut pool = NativeBatches::new();
/// let mut textures = Vec::new();
/// let cx = RenderCtx::new(&mut pool, &mut textures, 0.0);
/// only_takes_send(cx);
/// ```
pub struct RenderCtx<'frame> {
    batches: &'frame mut NativeBatches,
    textures: &'frame mut Vec<TextureRequest>,
    /// The clip in force, the intersection of every enclosing
    /// [`clip`](Self::clip).
    clip: Option<ClipShape>,
    /// Draw-order depth for everything this view emits, so it sorts against
    /// core primitives instead of landing on top of them.
    depth: f32,
    repaint: bool,
    /// A raw pointer field is the plain way to say "this type is not `Send`",
    /// and being explicit here is better than depending on which of the
    /// borrowed fields happens to be `!Send` today.
    _not_send: PhantomData<*const ()>,
}

impl<'frame> RenderCtx<'frame> {
    /// Opens a context over a frame's native-view pools.
    ///
    /// Called by the engine once per view per frame, never by a view.
    pub fn new(
        batches: &'frame mut NativeBatches,
        textures: &'frame mut Vec<TextureRequest>,
        depth: f32,
    ) -> Self {
        Self {
            batches,
            textures,
            clip: None,
            depth,
            repaint: false,
            _not_send: PhantomData,
        }
    }

    /// A handle to the registered pipeline `P`.
    ///
    /// Free: the handle is the pipeline's type identity, resolved to a
    /// registry entry once per frame by the encoder rather than once per call
    /// here. A view is expected to ask for it every frame and to keep nothing.
    #[must_use]
    pub fn pipeline<P: RenderPipeline>(&self) -> PipelineHandle<P> {
        PipelineHandle {
            key: PipelineKey::of::<P>(),
            _pipeline: PhantomData,
        }
    }

    /// Appends `instances` to this frame's batch for `handle`'s pipeline.
    ///
    /// The whole per-instance path, and the whole of the zero-cost claim: one
    /// monomorphized copy of a `Pod` slice into a buffer this pool has been
    /// reusing since the first frame. No dynamic dispatch, no allocation in
    /// the steady state, and byte-for-byte what a core intrinsic's staging
    /// does (INV-30).
    ///
    /// An empty slice emits nothing rather than an empty batch, so a view that
    /// has nothing to draw this frame costs the encoder nothing at all.
    pub fn emit<P: RenderPipeline>(
        &mut self,
        handle: PipelineHandle<P>,
        instances: &[P::Instance],
    ) {
        if instances.is_empty() {
            return;
        }
        let count = u32::try_from(instances.len()).unwrap_or(u32::MAX);
        self.batches.push_with(
            handle.key,
            std::mem::size_of::<P::Instance>(),
            count,
            self.clip,
            self.depth,
            |bytes| bytes.extend_from_slice(bytemuck::cast_slice(instances)),
        );
    }

    /// Makes a texture available to this frame's draws, returning the handle a
    /// view writes into an instance.
    ///
    /// The returned handle borrows `self`, which is what stops a view from
    /// keeping it: it cannot outlive the `render` call, so a texture can never
    /// be retained past the frame that asked for it (INV-31).
    pub fn upload_texture(&mut self, source: &TextureSource<'_>) -> TextureHandle<'_> {
        let id = u32::try_from(self.textures.len()).unwrap_or(u32::MAX);
        let source = match *source {
            TextureSource::Rgba8 {
                width,
                height,
                pixels,
            } => OwnedTextureSource::Rgba8 {
                width,
                height,
                pixels: pixels.to_vec(),
            },
            TextureSource::Path(p) => OwnedTextureSource::Path(p.to_string()),
        };
        self.textures.push(TextureRequest { id, source });
        TextureHandle {
            id,
            _frame: PhantomData,
        }
    }

    /// Runs `draw` with everything it emits clipped to `shape`.
    ///
    /// A closure rather than a push/pop pair, because an unbalanced clip stack
    /// is a bug that shows up as a clip leaking across half the scene, and the
    /// shape of this call makes it unrepresentable. Nesting intersects, so an
    /// inner clip can only ever narrow an outer one.
    pub fn clip<R>(&mut self, shape: ClipShape, draw: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.clip;
        self.clip = Some(match previous {
            Some(outer) => shape.intersect(outer),
            None => shape,
        });
        let result = draw(self);
        self.clip = previous;
        result
    }

    /// The clip in force right now, if any.
    #[must_use]
    pub const fn current_clip(&self) -> Option<ClipShape> {
        self.clip
    }

    /// Asks for one more frame after this one (RFC-0032).
    ///
    /// For a view that is animating from its own state rather than from a
    /// signal the engine can see. It marks exactly the next frame, not a
    /// standing subscription: a view that wants to keep animating asks again
    /// each frame, so an animation that ends stops costing frames without
    /// anybody remembering to cancel it.
    pub const fn request_repaint(&mut self) {
        self.repaint = true;
    }

    /// Whether this view asked for another frame.
    #[must_use]
    pub const fn wants_repaint(&self) -> bool {
        self.repaint
    }

    /// The draw-order depth this view's emissions carry.
    #[must_use]
    pub const fn depth(&self) -> f32 {
        self.depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::SolidBoxPipeline;
    use crate::frame::{BoxInstance, Transform};

    fn boxes(n: usize) -> Vec<BoxInstance> {
        (0..n)
            .map(|i| BoxInstance {
                #[allow(clippy::cast_precision_loss)]
                rect: [i as f32, 0.0, 10.0, 10.0],
                color: [1.0, 0.0, 0.0, 1.0],
                radii: [0.0; 4],
                transform: Transform::IDENTITY,
                smooth: 0.0,
            })
            .collect()
    }

    #[test]
    fn emitted_instances_are_the_bytes_a_core_pool_would_have_staged() {
        // The claim the whole ABI rests on: a package's instances reach the
        // arena as the same bytes a core intrinsic's do. Not "equivalent",
        // the same, which is checkable (INV-30).
        let mut pool = NativeBatches::new();
        let mut textures = Vec::new();
        let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.5);
        let instances = boxes(3);
        let handle = cx.pipeline::<SolidBoxPipeline>();
        cx.emit(handle, &instances);

        let batch = &pool.batches()[0];
        assert_eq!(batch.count, 3);
        assert_eq!(batch.instance_size, std::mem::size_of::<BoxInstance>());
        assert_eq!(
            batch.bytes,
            bytemuck::cast_slice::<BoxInstance, u8>(&instances),
            "a native view stages the same bytes the core path stages"
        );
        assert_eq!(batch.pipeline, PipelineKey::of::<SolidBoxPipeline>());
        assert!((batch.depth - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn emitting_nothing_costs_the_encoder_nothing() {
        let mut pool = NativeBatches::new();
        let mut textures = Vec::new();
        let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.0);
        let handle = cx.pipeline::<SolidBoxPipeline>();
        cx.emit(handle, &[]);
        assert!(pool.is_empty(), "an empty emit is not an empty batch");
    }

    #[test]
    fn a_batch_carries_the_clip_that_was_in_force() {
        let mut pool = NativeBatches::new();
        let mut textures = Vec::new();
        let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.0);
        let handle = cx.pipeline::<SolidBoxPipeline>();
        let outer = ClipShape::Rect([0.0, 0.0, 100.0, 100.0]);
        let inner = ClipShape::Rect([50.0, 50.0, 100.0, 100.0]);
        cx.emit(handle, &boxes(1));
        cx.clip(outer, |cx| {
            cx.emit(handle, &boxes(1));
            cx.clip(inner, |cx| cx.emit(handle, &boxes(1)));
            cx.emit(handle, &boxes(1));
        });
        cx.emit(handle, &boxes(1));

        let clips: Vec<Option<ClipShape>> = pool.batches().iter().map(|b| b.clip).collect();
        assert_eq!(
            clips,
            vec![
                None,
                Some(outer),
                Some(ClipShape::Rect([50.0, 50.0, 50.0, 50.0])),
                Some(outer),
                None,
            ],
            "a clip narrows inside its closure and is restored on the way out"
        );
    }

    #[test]
    fn a_texture_request_is_recorded_for_the_encoder_to_drain() {
        let mut pool = NativeBatches::new();
        let mut textures = Vec::new();
        let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.0);
        let first = cx.upload_texture(&TextureSource::Rgba8 {
            width: 1,
            height: 1,
            pixels: &[255, 0, 0, 255],
        });
        assert_eq!(first.id(), 0);
        let second = cx.upload_texture(&TextureSource::Path("tile.png"));
        assert_eq!(second.id(), 1);
        assert_eq!(textures.len(), 2);
    }

    #[test]
    fn a_repaint_request_is_for_the_next_frame_and_not_a_subscription() {
        let mut pool = NativeBatches::new();
        let mut textures = Vec::new();
        let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.0);
        assert!(!cx.wants_repaint());
        cx.request_repaint();
        assert!(cx.wants_repaint());

        // The next frame's context is a new one, and starts unasked.
        let mut next = RenderCtx::new(&mut pool, &mut textures, 0.0);
        assert!(
            !next.wants_repaint(),
            "asking once must not keep the app awake forever"
        );
        next.request_repaint();
    }
}
