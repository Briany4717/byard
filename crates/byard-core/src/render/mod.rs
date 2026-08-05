//! The native render extension ABI: what a package draws through (RFC-0039).
//!
//! This module is the front door. A package that ships a custom-drawn widget
//! reaches the engine here and nowhere else, which is what makes the boundary
//! reviewable: everything an extension can do to the GPU is a method in this
//! directory.
//!
//! # What crosses, and on which thread
//!
//! A native view runs on the **logic thread**, where the rest of the frame is
//! assembled (INV-2). It never sees a `wgpu` type, so it cannot hold one, so
//! the `!Send` graphics state cannot follow it anywhere (INV-12). What it
//! produces is instance bytes and the identity of the pipeline that draws
//! them, both plain data, and the encoder stages those into the persistent
//! arena (RFC-0033) in the same single linear pass it stages every core pool
//! in.
//!
//! That is a delta against RFC-0039's wording, which says `cx.emit` writes
//! "directly into the persistent instance arena". The arena lives on the
//! render thread behind the frame swap, so writing into it from a view would
//! be exactly the cross-thread graphics access the same RFC forbids two
//! paragraphs later. The claim the wording is defending is that a package
//! instance and a core instance reach the arena **by the same code**, and they
//! do: [`RenderCtx::emit`] is one monomorphized memcpy into a reused buffer,
//! per pipeline per frame, and the arena sees both pools identically.
//!
//! # Where the dynamic dispatch is not
//!
//! Nowhere on the per-instance path. [`RenderCtx::emit`] is generic over the
//! pipeline's own `Instance` type, so a batch of ten thousand instances is one
//! `extend_from_slice` of a `Pod` slice, and the only indirection in the whole
//! frame is the encoder choosing which registered pipeline draws (INV-30).

pub mod batch;
pub mod ctx;
pub mod registry;
pub mod view;

pub use batch::{ClipShape, NativeBatch, NativeBatches, PipelineKey};
pub use ctx::{PipelineHandle, RenderCtx, TextureHandle, TextureRequest, TextureSource};
pub use registry::{NativeProp, NativePropType, NativeViewInfo, NativeViewMeta};
pub use view::{Event, Handled, Layout, Measure, NativeProps, NativeView, RequestKey};

// ── What a package needs to draw with, in one import ──────────────────────
//
// A native view lives in a package, and a package depends on the `byard`
// façade, not on `byard-core` directly (INV-1). Everything a view legitimately
// touches is therefore re-exported here rather than left for an author to
// reach across the crate graph for: the core pipelines it may emit into, the
// instance records those pipelines draw, the colour conversion the engine
// itself uses, and the event vocabulary its `on_event` receives.

/// Turns a written colour (`0xRRGGBB`, `0xAARRGGBB`) into the linear RGBA the
/// engine paints in.
pub use crate::color::rgba;
/// The `SolidBox` pipeline, the cheapest way for a view to put a rectangle on
/// screen.
pub use crate::encoder::SolidBoxPipeline;
/// The `CanvasShape` pipeline (RFC-0020), for analytic strokes.
pub use crate::encoder::canvas_shape::CanvasShapePipeline;
/// The `DecoratedBox` pipeline, for a rectangle with a border, a shadow or a
/// gradient.
pub use crate::encoder::decorated_box::DecoratedBoxPipeline;
/// The trait a package implements to register a pipeline of its own, and the
/// contexts its methods receive.
pub use crate::encoder::pipeline::{BatchDraw, PipelineCtx, RenderPipeline, SegmentDraw};
/// The instance records the core pipelines draw.
pub use crate::frame::{BoxInstance, CanvasShape, DecoratedBox, Transform};
/// The event vocabulary [`Event`] carries.
pub use crate::platform::{EventKind, InputPayload};
