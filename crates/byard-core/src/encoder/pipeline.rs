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

    /// Records one native view's batch of this pipeline's instances
    /// (RFC-0039).
    ///
    /// The same pipeline, the same shader, the same vertex layout; the only
    /// difference from [`draw`](Self::draw) is where the instances came from,
    /// and by this point that difference is a byte range in the arena. A
    /// pipeline that cannot be driven this way is one no package could use,
    /// which is why it is a required method rather than a defaulted one.
    fn draw_batch(&self, pass: &mut wgpu::RenderPass<'_>, cx: &BatchDraw<'_>);
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

    /// The instance layout this pipeline declares (from
    /// [`RenderPipeline::vertex_layout`]), which registration reads to answer
    /// whether it can run on any GPU rather than only on the author's.
    fn instance_layout(&self) -> wgpu::VertexBufferLayout<'static>;

    /// Records this pipeline's draws for one segment.
    fn draw_segment(&self, pass: &mut wgpu::RenderPass<'_>, cx: &SegmentDraw<'_>);

    /// This pipeline's key, which is what a native view's batch names it by.
    fn key(&self) -> crate::render::PipelineKey;

    /// Records one native view's batch (RFC-0039).
    fn draw_native_batch(&self, pass: &mut wgpu::RenderPass<'_>, cx: &BatchDraw<'_>);
}

impl<P: RenderPipeline> ErasedPipeline for P {
    fn name(&self) -> &'static str {
        P::NAME
    }

    fn instance_size(&self) -> usize {
        std::mem::size_of::<P::Instance>()
    }

    fn instance_layout(&self) -> wgpu::VertexBufferLayout<'static> {
        P::vertex_layout()
    }

    fn draw_segment(&self, pass: &mut wgpu::RenderPass<'_>, cx: &SegmentDraw<'_>) {
        self.draw(pass, cx);
    }

    fn key(&self) -> crate::render::PipelineKey {
        crate::render::PipelineKey::of::<P>()
    }

    fn draw_native_batch(&self, pass: &mut wgpu::RenderPass<'_>, cx: &BatchDraw<'_>) {
        self.draw_batch(pass, cx);
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
    ///
    /// # Errors
    ///
    /// [`ByardError::PipelineCompilation`] naming the pipeline if its instance
    /// layout asks for more vertex attributes than a GPU is guaranteed to have
    /// (see [`check_portable_layout`]). Registration is the last moment this
    /// can be answered on the CPU, with the pipeline's name in hand; after it,
    /// the same mistake is a driver validation error on one machine and a
    /// working build on another.
    pub fn register(
        &mut self,
        order: PipelineOrder,
        pipeline: Box<dyn ErasedPipeline>,
    ) -> Result<usize, ByardError> {
        check_portable_layout(pipeline.name(), &pipeline.instance_layout())?;
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
        Ok(at)
    }

    /// Registers a core pipeline (convenience over [`register`](Self::register)).
    ///
    /// # Errors
    ///
    /// As [`register`](Self::register): a core pipeline is held to the same
    /// portability rule a package's is, which is the point of core going
    /// through this call at all.
    pub fn register_core<P: RenderPipeline>(&mut self, pipeline: P) -> Result<usize, ByardError> {
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

    /// Draws one native view's batch through the pipeline it names
    /// (RFC-0039), reporting whether that pipeline is registered.
    ///
    /// A `false` is a view drawing through a pipeline nobody registered, which
    /// is an app-assembly mistake rather than a frame-time condition: the
    /// caller names it once and the batch is skipped, because the alternative
    /// is either a panic in the render thread or a silently missing widget
    /// (INV-4).
    pub fn draw_batch(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        key: crate::render::PipelineKey,
        cx: &BatchDraw<'_>,
    ) -> bool {
        let Some(entry) = self
            .entries
            .iter()
            .find(|e| e.pipeline.key().type_id() == key.type_id())
        else {
            return false;
        };
        self.dispatches.set(self.dispatches.get() + 1);
        entry.pipeline.draw_native_batch(pass, cx);
        true
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

/// Rejects an instance layout that could not run on some conformant GPU.
///
/// A vertex attribute is scarcer than it looks. The WebGPU specification
/// guarantees sixteen of them, at locations `0..16`, and that is exactly what a
/// Linux Vulkan or GL adapter offers, while a Metal one offers thirty-one. A
/// layout that spends more than the guarantee therefore builds on its author's
/// machine and does not exist on its user's, and the only symptom is a
/// pipeline that never draws.
///
/// The rule is enforced here, at registration, because this is where a package
/// pipeline first meets the engine and where the pipeline still has a name.
/// One attribute of the budget is the shared unit quad's (location 0), which
/// every instanced pipeline binds and none of them declares, so an instance
/// layout may spend the fifteen that are left.
///
/// # Errors
///
/// [`ByardError::PipelineCompilation`] naming the pipeline and the lane, if a
/// location falls outside the guaranteed range or the layout declares more
/// attributes than remain after the quad.
pub fn check_portable_layout(
    name: &str,
    layout: &wgpu::VertexBufferLayout<'static>,
) -> Result<(), ByardError> {
    /// The quad at location 0, which every instanced pipeline binds.
    const QUAD_ATTRIBUTES: u32 = 1;
    let guaranteed = wgpu::Limits::default().max_vertex_attributes;

    for attr in layout.attributes {
        if attr.shader_location >= guaranteed {
            return Err(build_error(
                name,
                &format!(
                    "vertex attribute at shader location {} is outside the 0..{guaranteed} every \
                     GPU guarantees; pack lanes into wider attributes rather than raising the \
                     location",
                    attr.shader_location
                ),
            ));
        }
    }

    let declared = u32::try_from(layout.attributes.len()).unwrap_or(u32::MAX);
    if declared.saturating_add(QUAD_ATTRIBUTES) > guaranteed {
        return Err(build_error(
            name,
            &format!(
                "declares {declared} vertex attributes, and with the shared quad that is past the \
                 {guaranteed} every GPU guarantees"
            ),
        ));
    }
    Ok(())
}

/// Everything one native view's batch draw reads (RFC-0039).
///
/// Deliberately narrower than [`SegmentDraw`]: a batch is one contiguous run
/// of one pipeline's instances, so there are no pool ranges to slice and no
/// per-pool clip table to walk. What is left is where the instances are, how
/// many, and the shared bindings every instanced pipeline in the UI pass uses.
pub struct BatchDraw<'a> {
    /// The frame's instance arena.
    pub arena: &'a super::instance_arena::InstanceArena,
    /// Where this batch's instances were staged.
    pub instances: super::instance_arena::Region,
    /// Where this batch's parallel draw-order depths were staged, one `f32`
    /// per instance. Staged for every batch, because whether a pipeline reads
    /// it is the pipeline's business and staging it is cheaper than asking.
    pub depths: super::instance_arena::Region,
    /// How many instances the batch holds.
    pub count: u32,
    /// The shared viewport uniform bind group (group 0).
    pub viewport_bind_group: &'a wgpu::BindGroup,
    /// The unit quad every instanced pipeline expands.
    pub quad_buffer: &'a wgpu::Buffer,
    /// The `CanvasShape` pipeline's per-frame shape-record binding (group 1).
    pub records_bind_group: &'a wgpu::BindGroup,
    /// The MSDF atlas the vector pipeline samples (group 1).
    pub vector_atlas: &'a super::VectorAtlas,
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
        fn draw_batch(&self, _pass: &mut wgpu::RenderPass<'_>, _cx: &BatchDraw<'_>) {}
    }

    #[test]
    fn core_pipelines_draw_before_package_pipelines() {
        // INV-32: the order is declared. A package that registers first still
        // draws after core, because "first" is not what decides.
        let mut registry = PipelineRegistry::new();
        registry
            .register(PipelineOrder::Package, Box::new(Probe::<1>))
            .unwrap();
        registry.register_core(Probe::<2>).unwrap();
        registry
            .register(PipelineOrder::Package, Box::new(Probe::<3>))
            .unwrap();
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
            r.register_core(Probe::<1>).unwrap();
            r.register_core(Probe::<2>).unwrap();
            r.register(PipelineOrder::Package, Box::new(Probe::<3>))
                .unwrap();
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

    /// A pipeline whose instance layout spends a lane no GPU is guaranteed to
    /// have, which is precisely what `DecoratedBox` did when its gradient kind
    /// took location 16 and the pipeline stopped existing on Linux.
    struct Overrun;

    impl RenderPipeline for Overrun {
        const NAME: &'static str = "overrun";
        type Instance = ProbeInstance;
        fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
            const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![16 => Uint32];
            wgpu::VertexBufferLayout {
                array_stride: 16,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: ATTRS,
            }
        }
        fn draw(&self, _pass: &mut wgpu::RenderPass<'_>, _cx: &SegmentDraw<'_>) {}
        fn draw_batch(&self, _pass: &mut wgpu::RenderPass<'_>, _cx: &BatchDraw<'_>) {}
    }

    /// A pipeline that stays inside every location but declares too many of
    /// them: fifteen plus the shared quad is the whole budget, sixteen is one
    /// past it.
    struct TooMany;

    impl RenderPipeline for TooMany {
        const NAME: &'static str = "too_many";
        type Instance = ProbeInstance;
        fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
            const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![
                0 => Float32, 1 => Float32, 2 => Float32, 3 => Float32,
                4 => Float32, 5 => Float32, 6 => Float32, 7 => Float32,
                8 => Float32, 9 => Float32, 10 => Float32, 11 => Float32,
                12 => Float32, 13 => Float32, 14 => Float32, 15 => Float32,
            ];
            wgpu::VertexBufferLayout {
                array_stride: 64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: ATTRS,
            }
        }
        fn draw(&self, _pass: &mut wgpu::RenderPass<'_>, _cx: &SegmentDraw<'_>) {}
        fn draw_batch(&self, _pass: &mut wgpu::RenderPass<'_>, _cx: &BatchDraw<'_>) {}
    }

    #[test]
    fn a_pipeline_that_could_not_run_everywhere_is_refused_at_registration() {
        // The registry is where a package pipeline first meets the engine, and
        // the last point at which "this asks for more than a GPU has" can be
        // said with the pipeline's name attached. After it, the same mistake
        // is a driver error on someone else's machine (INV-4).
        let mut registry = PipelineRegistry::new();
        let err = registry
            .register(PipelineOrder::Package, Box::new(Overrun))
            .expect_err("a lane past the guaranteed range must be refused");
        let text = err.to_string();
        assert!(
            text.contains("overrun"),
            "the error names the pipeline: {text}"
        );
        assert!(text.contains("16"), "the error names the lane: {text}");
        assert!(
            registry.is_empty(),
            "a refused pipeline is not in the order"
        );
    }

    #[test]
    fn the_shared_quad_is_counted_against_the_budget() {
        // Fifteen attributes plus the quad at location 0 is exactly sixteen,
        // which fits. A pipeline declaring sixteen of its own does not, even
        // though every one of its locations is in range.
        let mut registry = PipelineRegistry::new();
        let err = registry
            .register(PipelineOrder::Package, Box::new(TooMany))
            .expect_err("sixteen declared attributes plus the quad is seventeen");
        assert!(err.to_string().contains("too_many"));
    }

    #[test]
    fn every_core_pipeline_layout_passes_the_portability_rule() {
        // The rule is only worth enforcing if core itself lives by it, which
        // is the same claim the encoder's own layout test makes and the reason
        // core registers through this call instead of around it.
        for (name, layout) in [
            ("solid_box", super::super::BoxInstance::layout()),
            (
                "decorated_box",
                super::super::decorated_box::DecoratedInstance::layout(),
            ),
            ("ripple", crate::frame::RippleInstance::layout()),
            (
                "canvas_shape",
                super::super::canvas_shape::CanvasShapeInstance::layout(),
            ),
            (
                "texture_sampler",
                super::super::texture_sampler::TextureInstance::layout(),
            ),
            ("vector_msdf", super::super::vector_msdf::instance_layout()),
        ] {
            check_portable_layout(name, &layout)
                .unwrap_or_else(|e| panic!("{name} is not portable: {e}"));
        }
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
