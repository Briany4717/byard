//! The pipeline registry: the set of render pipelines a frame draws through,
//! and the API a package registers one into (RFC-0039 §"Pipeline
//! registration").
//!
//! # What changed, and what deliberately did not
//!
//! The encoder used to hold its pipelines as named fields and draw them by
//! writing their names out in order, in one function. That is a perfectly good
//! way to run a fixed set, and it is exactly why a package could not add one:
//! the set was a piece of source code, not data.
//!
//! It is data now. [`PipelineRegistry`] holds an ordered
//! `Vec<Box<dyn ErasedPipeline>>`, and the core pipelines go into it through
//! the same call a package's would. Nothing about *how* a pipeline draws
//! changed, which is the point of the refactor: the frame it produces is
//! byte-identical (INV-22), and the only new cost is one vtable call per
//! pipeline per segment.
//!
//! # Where the dynamic dispatch is, and where it is not (INV-30)
//!
//! Exactly one indirect call per registered pipeline per segment, to decide
//! *which* pipeline runs. Everything downstream of that call is the concrete
//! type: staging N instances into the arena runs through `P::Instance`, the
//! draw call is that pipeline's own compiled shader, and neither knows the
//! registry exists.
//!
//! This is measurable rather than asserted: [`PipelineRegistry::dispatches`]
//! counts the erased calls a frame made, and the test that reads it draws ten
//! thousand boxes and ten and expects the same number.
//!
//! # The two pipelines that are not in here
//!
//! `text_glyph` and `backdrop` do not have the shape this trait describes, and
//! forcing them into it would have been a worse lie than leaving them out:
//!
//! - **Text** is glyphon's renderer. It owns its own shaping cache, its own
//!   atlas and its own draw call, and it has no pool of `Pod` instances the
//!   arena stages. There is no `type Instance` to name.
//! - **Backdrop/blur** is not a pipeline that draws a pool at a point in the
//!   order, it is what *splits* the pass: a copy, an off-screen blur, and a
//!   composite that opens the next segment. It sits above the ordered set
//!   rather than inside it.
//!
//! Both keep their existing call sites in the pass, at the same points in the
//! order they have always been drawn at. A package pipeline has the shape this
//! trait describes, so the registry covers what it exists to cover.

use crate::ByardError;

/// What a pipeline needs from the encoder to build itself.
///
/// Handed to a pipeline's constructor at startup, never to a package's `render`
/// (INV-31): the device is here because building a pipeline is *when* a device
/// is legitimately needed, and this context does not outlive registration.
pub struct PipelineCtx<'a> {
    /// The device pipelines are created on.
    pub device: &'a wgpu::Device,
    /// The surface format the frame is encoded for.
    pub format: wgpu::TextureFormat,
    /// The shared viewport uniform's bind-group layout (group 0). Every
    /// pipeline in the UI pass binds it, so it is built once and lent out.
    pub viewport_layout: &'a wgpu::BindGroupLayout,
}

/// A pipeline that draws a pool of `Pod` instances through the shared instance
/// arena (RFC-0039).
///
/// The associated `Instance` type is the load-bearing part: it is what keeps
/// the per-instance path monomorphized (INV-30). A package's instances and a
/// core intrinsic's instances reach the arena through the same generic code,
/// specialised at compile time for each.
pub trait RenderPipeline: 'static {
    /// The pipeline's name, for diagnostics and the registry's declared order.
    const NAME: &'static str;

    /// The per-instance record this pipeline draws.
    type Instance: bytemuck::Pod;

    /// The vertex-buffer layout of [`Self::Instance`], for the instance step.
    fn vertex_layout() -> wgpu::VertexBufferLayout<'static>;

    /// Records this pipeline's draws for one pass segment.
    ///
    /// Called once per segment through the registry. Everything the draw needs
    /// that is not the pipeline itself arrives in [`SegmentDraw`], which is the
    /// pass's shared frontier for exactly the same reason `frame.rs` is the
    /// subsystem boundary: one place to look for what crosses.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, cx: &SegmentDraw<'_>);
}

/// The object-safe half of [`RenderPipeline`], which is what the registry
/// stores.
///
/// Blanket-implemented, so nothing implements this directly and the two can
/// never describe different pipelines.
pub trait ErasedPipeline: 'static {
    /// The pipeline's name (from [`RenderPipeline::NAME`]).
    fn name(&self) -> &'static str;

    /// The size of one instance record, in bytes. Read by the arena-budget
    /// diagnostics, and by the test that a package instance is staged by the
    /// same code as a core one.
    fn instance_size(&self) -> usize;

    /// Records this pipeline's draws for one segment.
    fn draw_segment(&self, pass: &mut wgpu::RenderPass<'_>, cx: &SegmentDraw<'_>);
}

impl<P: RenderPipeline> ErasedPipeline for P {
    fn name(&self) -> &'static str {
        P::NAME
    }

    fn instance_size(&self) -> usize {
        std::mem::size_of::<P::Instance>()
    }

    fn draw_segment(&self, pass: &mut wgpu::RenderPass<'_>, cx: &SegmentDraw<'_>) {
        self.draw(pass, cx);
    }
}

/// Which half of the declared draw order a pipeline belongs to (INV-32).
///
/// Order is a property of the registration, not of when a `HashMap` happens to
/// yield an entry or of the order a linker resolved static initializers in. Two
/// runs of the same program draw in the same order, and so do two machines.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PipelineOrder {
    /// A pipeline the engine itself registers, in its historical draw order.
    Core,
    /// A pipeline a package registers. Always after every core pipeline, and
    /// among themselves in registration order.
    Package,
}

/// One registered pipeline and the two numbers that place it in the order.
struct Registration {
    order: PipelineOrder,
    /// Registration sequence, the tie-breaker within an order class.
    seq: usize,
    pipeline: Box<dyn ErasedPipeline>,
}

/// The ordered set of pipelines a frame draws through (RFC-0039, INV-32).
#[derive(Default)]
pub struct PipelineRegistry {
    entries: Vec<Registration>,
    /// Erased calls made since the last [`begin_frame`](Self::begin_frame),
    /// the INV-30 measurement.
    dispatches: std::cell::Cell<u32>,
}

impl PipelineRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a pipeline, returning its index in the draw order.
    ///
    /// The entries are kept sorted by `(order, seq)` on insertion, so iteration
    /// never has to sort and the order cannot depend on when a caller happened
    /// to register (INV-32). A stable sort on a two-part key is the whole
    /// mechanism; there is deliberately no priority number for an author to
    /// tune, because a tunable order is one nobody can reason about.
    pub fn register(&mut self, order: PipelineOrder, pipeline: Box<dyn ErasedPipeline>) -> usize {
        let seq = self.entries.len();
        let entry = Registration {
            order,
            seq,
            pipeline,
        };
        let at = self
            .entries
            .partition_point(|e| (e.order, e.seq) <= (entry.order, entry.seq));
        self.entries.insert(at, entry);
        at
    }

    /// Registers a core pipeline (convenience over [`register`](Self::register)).
    pub fn register_core<P: RenderPipeline>(&mut self, pipeline: P) -> usize {
        self.register(PipelineOrder::Core, Box::new(pipeline))
    }

    /// The registered pipelines, in declared draw order.
    pub fn iter(&self) -> impl Iterator<Item = &dyn ErasedPipeline> {
        self.entries.iter().map(|e| e.pipeline.as_ref())
    }

    /// The names in draw order, for diagnostics and the order test.
    #[must_use]
    pub fn order(&self) -> Vec<&'static str> {
        self.entries.iter().map(|e| e.pipeline.name()).collect()
    }

    /// How many pipelines are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no pipeline is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resets the dispatch counter at the start of a frame.
    pub fn begin_frame(&self) {
        self.dispatches.set(0);
    }

    /// How many erased calls this frame has made (INV-30).
    ///
    /// The number that must stay proportional to the pipeline count and not to
    /// the instance count. A regression to per-instance dispatch shows up here
    /// as a number in the thousands.
    #[must_use]
    pub fn dispatches(&self) -> u32 {
        self.dispatches.get()
    }

    /// Draws one segment through every registered pipeline, in order.
    pub fn draw_segment(&self, pass: &mut wgpu::RenderPass<'_>, cx: &SegmentDraw<'_>) {
        for entry in &self.entries {
            self.dispatches.set(self.dispatches.get() + 1);
            entry.pipeline.draw_segment(pass, cx);
        }
    }
}

/// Everything a pipeline's segment draw reads, in one place.
///
/// Wide on purpose. The alternative is each pipeline reaching into the encoder
/// for what it needs, which is how a "registered" pipeline quietly becomes a
/// hard-wired one again: a package cannot reach into the encoder, so anything a
/// core pipeline reads that way is a hole in the ABI.
pub struct SegmentDraw<'a> {
    /// The frame's instance arena; every staged region indexes into it.
    pub arena: &'a super::instance_arena::InstanceArena,
    /// This segment's staged regions, per pipeline.
    pub staged: &'a super::SegmentStaging,
    /// This segment's per-pool index ranges (instance counts).
    pub ranges: &'a super::SegmentRanges,
    /// The frame's clip table and per-pool clip slices.
    pub clips: super::FrameClips<'a>,
    /// The scissor context clip runs are resolved against.
    pub clip_ctx: super::ClipCtx<'a>,
    /// The shared viewport uniform bind group (group 0).
    pub viewport_bind_group: &'a wgpu::BindGroup,
    /// The unit quad every instanced pipeline expands.
    pub quad_buffer: &'a wgpu::Buffer,
    /// The `CanvasShape` pipeline's per-frame shape-record binding.
    pub records_bind_group: &'a wgpu::BindGroup,
    /// The MSDF atlas the vector pipeline samples.
    pub vector_atlas: &'a super::VectorAtlas,
    /// The decoded-image cache the texture pipeline samples.
    pub texture_cache: &'a super::texture_sampler::TextureCache,
    /// This segment's texture instances (the texture pipeline binds per image).
    pub textures: &'a [crate::frame::TextureSampler],
}

/// A pipeline whose shader failed to compile names itself (INV-4).
///
/// Kept here rather than at each call site so every pipeline's build failure
/// reads the same, whether it is core's or a package's.
#[must_use]
pub fn build_error(name: &str, detail: &str) -> ByardError {
    ByardError::PipelineCompilation {
        pipeline: name.to_string(),
        reason: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipeline that draws nothing, for testing the registry itself.
    struct Probe<const N: usize>;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct ProbeInstance {
        _x: [f32; 4],
    }

    impl<const N: usize> RenderPipeline for Probe<N> {
        const NAME: &'static str = "probe";
        type Instance = ProbeInstance;
        fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
            wgpu::VertexBufferLayout {
                array_stride: 16,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &[],
            }
        }
        fn draw(&self, _pass: &mut wgpu::RenderPass<'_>, _cx: &SegmentDraw<'_>) {}
    }

    #[test]
    fn core_pipelines_draw_before_package_pipelines() {
        // INV-32: the order is declared. A package that registers first still
        // draws after core, because "first" is not what decides.
        let mut registry = PipelineRegistry::new();
        registry.register(PipelineOrder::Package, Box::new(Probe::<1>));
        registry.register_core(Probe::<2>);
        registry.register(PipelineOrder::Package, Box::new(Probe::<3>));
        let order: Vec<usize> = registry
            .entries
            .iter()
            .map(|e| match e.order {
                PipelineOrder::Core => 0,
                PipelineOrder::Package => 1,
            })
            .collect();
        assert_eq!(order, vec![0, 1, 1], "core first, then packages");
    }

    #[test]
    fn registration_order_breaks_ties_and_is_reproducible() {
        // Two registries built by the same sequence of calls iterate
        // identically. This is the property a `HashMap` of pipelines would not
        // have, and the reason the entries are a `Vec` sorted on insertion.
        let build = || {
            let mut r = PipelineRegistry::new();
            r.register_core(Probe::<1>);
            r.register_core(Probe::<2>);
            r.register(PipelineOrder::Package, Box::new(Probe::<3>));
            r.entries
                .iter()
                .map(|e| (e.order, e.seq))
                .collect::<Vec<_>>()
        };
        assert_eq!(build(), build());
        assert_eq!(
            build(),
            vec![
                (PipelineOrder::Core, 0),
                (PipelineOrder::Core, 1),
                (PipelineOrder::Package, 2),
            ]
        );
    }

    #[test]
    fn an_erased_pipeline_still_knows_its_instance_size() {
        // The erased half has to keep the concrete instance type's facts, or
        // the arena budget diagnostics would be reading a vtable that forgot
        // what it was pointing at.
        let p: Box<dyn ErasedPipeline> = Box::new(Probe::<1>);
        assert_eq!(p.instance_size(), std::mem::size_of::<ProbeInstance>());
        assert_eq!(p.name(), "probe");
    }
}
