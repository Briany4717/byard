//! What a native view's `render` leaves behind: instance bytes, the pipeline
//! that draws them, and the clip they draw under (RFC-0039).
//!
//! The pool is shaped like every other pool on the frame, and for the same
//! reason: it is filled on the logic thread, read once on the render thread,
//! and reused rather than reallocated (RFC-0033). A steady-state frame that
//! emits the same batches it emitted last frame allocates nothing here.

use std::any::TypeId;

/// The identity of a registered pipeline, as it crosses the frame boundary.
///
/// A registry index would be smaller and would be wrong: the registry is built
/// on the render thread at startup, and a view emitting on the logic thread
/// has never seen it. A `TypeId` is the pipeline's own identity, is `Send`,
/// and cannot collide, so the encoder resolves it to an index once per frame
/// rather than the view guessing one.
///
/// The name rides along for diagnostics only. Two pipelines with the same name
/// are still two pipelines; the `TypeId` is what decides.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PipelineKey {
    type_id: TypeId,
    name: &'static str,
}

impl PipelineKey {
    /// The key of the pipeline type `P`.
    #[must_use]
    pub fn of<P: crate::encoder::pipeline::RenderPipeline>() -> Self {
        Self {
            type_id: TypeId::of::<P>(),
            name: P::NAME,
        }
    }

    /// The pipeline's declared name, for diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The type identity the encoder resolves against the registry.
    #[must_use]
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }
}

/// The shape a batch is clipped to, if any (RFC-0037 clip masks).
///
/// Both variants are in logical pixels, the space the rest of the frame is in.
/// A rounded rect is its own variant rather than a rect with zero radii,
/// because the two take different paths on the GPU: the rounded case is an
/// analytic test in the fragment shader and the rectangular one is a scissor,
/// and collapsing them would tax the common case with the rarer one's cost.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ClipShape {
    /// `[x, y, width, height]`, a scissor.
    Rect([f32; 4]),
    /// `[x, y, width, height]` with per-corner radii `[tl, tr, br, bl]`.
    RoundedRect {
        /// `[x, y, width, height]` in logical pixels.
        rect: [f32; 4],
        /// Per-corner radii `[tl, tr, br, bl]` in logical pixels.
        radii: [f32; 4],
    },
}

impl ClipShape {
    /// The bounding rectangle of this shape, which is the scissor either
    /// variant is at most allowed to touch.
    #[must_use]
    pub const fn bounds(&self) -> [f32; 4] {
        match self {
            Self::Rect(r) | Self::RoundedRect { rect: r, .. } => *r,
        }
    }

    /// The intersection of two clips, which is what nesting one inside another
    /// means.
    ///
    /// An intersection of two rounded rects is not a rounded rect, so the
    /// result keeps the inner shape's corners and takes the tighter bounds.
    /// That is exact when the outer clip does not cut a corner off the inner
    /// one, which is the shape of every real nesting (a card inside a scroll
    /// viewport), and conservative rather than wrong otherwise: the bounds
    /// still clip, the corner rounding is the inner one's.
    #[must_use]
    pub fn intersect(self, outer: Self) -> Self {
        let a = self.bounds();
        let b = outer.bounds();
        let x = a[0].max(b[0]);
        let y = a[1].max(b[1]);
        let right = (a[0] + a[2]).min(b[0] + b[2]);
        let bottom = (a[1] + a[3]).min(b[1] + b[3]);
        let rect = [x, y, (right - x).max(0.0), (bottom - y).max(0.0)];
        match self {
            Self::Rect(_) => match outer {
                Self::Rect(_) => Self::Rect(rect),
                Self::RoundedRect { radii, .. } => Self::RoundedRect { rect, radii },
            },
            Self::RoundedRect { radii, .. } => Self::RoundedRect { rect, radii },
        }
    }
}

/// One pipeline's worth of instances emitted by one native view.
///
/// The bytes are the view's own `Pod` instance type, already in the layout its
/// shader reads. Nothing between here and the GPU interprets them, which is
/// why a package instance costs exactly what a core one does.
#[derive(Debug)]
pub struct NativeBatch {
    /// Which registered pipeline draws these instances.
    pub pipeline: PipelineKey,
    /// The instances, as the pipeline's `Instance` type laid out in memory.
    pub bytes: Vec<u8>,
    /// `size_of::<P::Instance>()`, so the encoder can turn bytes into a count
    /// without knowing the type.
    pub instance_size: usize,
    /// How many instances `bytes` holds.
    pub count: u32,
    /// The clip in force when this batch was emitted, if any.
    pub clip: Option<ClipShape>,
    /// Draw-order depth (NDC z), stamped by the frame so a native view sorts
    /// against core primitives rather than always over or always under them.
    pub depth: f32,
}

impl NativeBatch {
    /// An empty batch, ready to be refilled for another pipeline.
    fn blank() -> Self {
        Self {
            pipeline: PipelineKey {
                type_id: TypeId::of::<()>(),
                name: "",
            },
            bytes: Vec::new(),
            instance_size: 0,
            count: 0,
            clip: None,
            depth: 0.0,
        }
    }
}

/// Every native view's emissions for one frame.
///
/// Reused across frames like the rest of the frame's pools: `begin_frame`
/// drops the live count to zero and leaves the buffers alone, so the steady
/// state is a memcpy into memory that is already there (RFC-0033 §G1).
#[derive(Debug, Default)]
pub struct NativeBatches {
    entries: Vec<NativeBatch>,
    /// How many of `entries` this frame filled. Everything from here on is a
    /// buffer waiting to be refilled, not data.
    live: usize,
}

impl NativeBatches {
    /// An empty pool.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a frame: the pool is empty and its buffers are kept.
    pub fn begin_frame(&mut self) {
        self.live = 0;
    }

    /// This frame's batches, in emission order.
    #[must_use]
    pub fn batches(&self) -> &[NativeBatch] {
        &self.entries[..self.live]
    }

    /// How many batches this frame emitted.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.live
    }

    /// Whether this frame emitted no batch at all, the case for every app that
    /// uses no native view.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many batch buffers are being kept for reuse.
    ///
    /// The number that proves the pool is not reallocating: it rises to the
    /// high-water mark of a frame's batch count and then stops.
    #[must_use]
    pub fn retained(&self) -> usize {
        self.entries.len()
    }

    /// Appends a batch, reusing the next retained buffer if there is one.
    ///
    /// `write` fills the byte buffer, which arrives cleared and with whatever
    /// capacity the last frame left it.
    pub(crate) fn push_with(
        &mut self,
        pipeline: PipelineKey,
        instance_size: usize,
        count: u32,
        clip: Option<ClipShape>,
        depth: f32,
        write: impl FnOnce(&mut Vec<u8>),
    ) {
        if self.live == self.entries.len() {
            self.entries.push(NativeBatch::blank());
        }
        let entry = &mut self.entries[self.live];
        entry.pipeline = pipeline;
        entry.instance_size = instance_size;
        entry.count = count;
        entry.clip = clip;
        entry.depth = depth;
        entry.bytes.clear();
        write(&mut entry.bytes);
        self.live += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frames_batches_reuse_the_last_frames_buffers() {
        // The steady state of a native view is the same batches every frame.
        // If that allocated, every frame would pay for a widget that did not
        // change (RFC-0033 §G1).
        let mut pool = NativeBatches::new();
        for _ in 0..3 {
            pool.begin_frame();
            for _ in 0..4 {
                pool.push_with(
                    PipelineKey::of::<crate::encoder::SolidBoxPipeline>(),
                    4,
                    1,
                    None,
                    0.5,
                    |b| {
                        b.extend_from_slice(&[1, 2, 3, 4]);
                    },
                );
            }
        }
        assert_eq!(pool.len(), 4);
        assert_eq!(
            pool.retained(),
            4,
            "three frames of four batches keep four buffers, not twelve"
        );
    }

    #[test]
    fn a_shorter_frame_does_not_leave_the_longer_one_visible() {
        // `live`, not `truncate`: the buffers stay for reuse, but a batch the
        // frame did not emit is not in `batches()`.
        let mut pool = NativeBatches::new();
        pool.begin_frame();
        for i in 0..3u8 {
            pool.push_with(
                PipelineKey::of::<crate::encoder::SolidBoxPipeline>(),
                1,
                1,
                None,
                0.0,
                |b| {
                    b.push(i);
                },
            );
        }
        pool.begin_frame();
        pool.push_with(
            PipelineKey::of::<crate::encoder::SolidBoxPipeline>(),
            1,
            1,
            None,
            0.0,
            |b| {
                b.push(9);
            },
        );
        assert_eq!(pool.batches().len(), 1);
        assert_eq!(pool.batches()[0].bytes, vec![9]);
        assert_eq!(pool.retained(), 3);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "exact geometry: these are the numbers that were written, not a computation"
    )]
    fn a_nested_clip_is_the_intersection_and_keeps_the_inner_corners() {
        let outer = ClipShape::Rect([0.0, 0.0, 100.0, 100.0]);
        let inner = ClipShape::RoundedRect {
            rect: [50.0, 50.0, 100.0, 100.0],
            radii: [8.0; 4],
        };
        let ClipShape::RoundedRect { rect, radii } = inner.intersect(outer) else {
            panic!("a rounded clip inside a rect clip is still rounded");
        };
        assert_eq!(rect, [50.0, 50.0, 50.0, 50.0]);
        assert_eq!(radii, [8.0; 4]);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "exact geometry: these are the numbers that were written, not a computation"
    )]
    fn disjoint_clips_intersect_to_nothing_rather_than_to_a_negative_rect() {
        let a = ClipShape::Rect([0.0, 0.0, 10.0, 10.0]);
        let b = ClipShape::Rect([100.0, 100.0, 10.0, 10.0]);
        let bounds = a.intersect(b).bounds();
        assert_eq!(bounds[2], 0.0);
        assert_eq!(bounds[3], 0.0);
    }

    #[test]
    fn a_pipeline_key_is_the_pipelines_own_identity() {
        assert_eq!(
            PipelineKey::of::<crate::encoder::SolidBoxPipeline>(),
            PipelineKey::of::<crate::encoder::SolidBoxPipeline>()
        );
        assert_ne!(
            PipelineKey::of::<crate::encoder::SolidBoxPipeline>(),
            PipelineKey::of::<crate::encoder::decorated_box::DecoratedBoxPipeline>()
        );
        assert_eq!(
            PipelineKey::of::<crate::encoder::SolidBoxPipeline>().name(),
            "solid_box"
        );
    }
}
