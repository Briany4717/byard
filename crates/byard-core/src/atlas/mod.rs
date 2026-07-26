//! # Atlas
//!
//! Layout computation and spatial hit-testing.
//!
//! This subsystem owns:
//!
//! - **Taffy integration** — All layout is delegated to
//!   [`taffy`](https://github.com/DioxusLabs/taffy). The engine never
//!   computes box geometry itself. [`LayoutAtlas`] initialises the Taffy
//!   tree, feeds it node constraints, and reads back resolved rectangles.
//!
//! - **Spatial hash grid** *(future sub-issue)* — A partitioned data
//!   structure that indexes a mapping between 2D screen coordinates and
//!   event descriptors.
//!
//! Atlas exposes resolved geometry to the encoder exclusively through
//! [`crate::frame::Rect`] and reacts to dirty-flag notifications produced
//! by the Evaluator subsystem via [`crate::frame::TargetId`] +
//! [`crate::frame::TargetKind::AtlasNode`].
//!
//! # State machine
//!
//! [`LayoutAtlas`] enforces a strict lifecycle with two states:
//!
//! 1. **Building** — nodes can be added (`add_leaf`, `add_container`) and
//!    the root can be set. Querying resolved geometry or marking nodes
//!    dirty panics.
//! 2. **Computed** — `compute(viewport)` transitions here. Resolved
//!    geometry is accessible via `resolved_rect` and `populate_frame`.
//!    Dirty subtrees can be re-laid out incrementally via
//!    `mark_dirty_all` + `recompute_dirty`. Adding or modifying nodes
//!    panics until `clear` is called.
//!
//! ## Transitions
//!
//! - `compute(viewport)` — Building → Computed.
//! - `clear()` — Computed → Building. Preserves internal capacity and
//!   increments the view generation so any
//!   [`TargetId`](crate::frame::TargetId)s from the previous view are
//!   silently rejected by future `mark_dirty_all` calls.
//!
//! Per RFC-0001 §4.1, `compute` is called exactly once per frame at the
//! end of the mutation phase, then `recompute_dirty` is called on
//! subsequent frames whenever the Evaluator reports dirty targets.
//!
//! # Which path is production on? (read this before optimising)
//!
//! **The retained path is not currently reachable from the interpreter.**
//! `Interpreter::render` calls `clear()` and rebuilds the whole tree every
//! frame; `mark_dirty_all` + `recompute_dirty` are exercised by tests and
//! benchmarks only. The generation counter that `clear()` bumps was designed
//! to distinguish one *view* from another, and with a `clear()` per frame it
//! distinguishes one *frame* from another — so it invalidates everything,
//! always. It does exactly what it was built to do, on a condition that is
//! always true.
//!
//! This is measured, not assumed. On a 2-level flex tree the rebuild costs
//! ~30 µs at 50 leaves, ~112 µs at 200 and ~424 µs at 800 (≈5 % of an 8.3 ms
//! budget), against ~8 / ~20 / ~59 µs for `recompute_dirty` with one dirty
//! leaf — so the retained path is worth 3.8–7.2×, and it also avoids ~477 of
//! the 664 heap allocations the rebuild performs per frame at 800 leaves.
//! `cargo bench --bench atlas` reproduces all of it.
//!
//! What blocks it is not the atlas. Taking the retained path requires knowing
//! that nothing layout-affecting changed, and the interpreter cannot know
//! that: element attributes are raw expressions re-evaluated every frame, so
//! an animated `width` or a changed `Text` would silently keep the previous
//! frame's geometry — and a stale rect is still queryable by hit-testing, so
//! the failure mode is an element that looks like it moved but is tappable
//! where it used to be. See `LayoutAtlas::populate_frame`'s documentation for
//! the same constraint on the dirty-target channel.
//!
//! **How to check you are still on the expected path:**
//! `crates/byard-compiler/tests/incremental_paths.rs` reads the
//! `telemetry`-gated counters in [`layout::path_counters`] after a real frame
//! and asserts which path was walked. It also carries the `#[ignore]`d
//! acceptance criteria for the retained path, so the gap is visible in the
//! test output rather than invisible in nobody's head. If you change any of
//! this, that file is where it is decided.
//!
//! # Builder API
//!
//! [`LayoutAtlasBuilder`] sits on top of `add_leaf` /
//! `add_container` / `set_root` to let a multi-level tree be expressed as
//! a single chained expression, instead of one imperative call per node:
//!
//! ```
//! use byard_core::atlas::{ContainerStyle, LayoutAtlas, LayoutAtlasBuilder as B, LeafSize};
//!
//! let mut atlas = LayoutAtlas::new();
//! let root = atlas.build_root(
//!     B::container(ContainerStyle::new(Some(300.0), Some(200.0)), [
//!         B::leaf(LeafSize::new(50.0, 50.0)),
//!         B::container(ContainerStyle::default(), [
//!             B::leaf(LeafSize::new(20.0, 20.0)),
//!         ]),
//!     ]),
//! ).unwrap();
//! # let _ = root;
//! ```
//!
//! `LayoutAtlasBuilder::leaf` / `container` only build an [`AtlasNodeSpec`]
//! description — a plain value, no Taffy or atlas access — which
//! `LayoutAtlas::build` / `build_root` then commits in one depth-first
//! pass via the same low-level methods, in the same order an equivalent
//! imperative call sequence would use. The low-level API from PR #14 is
//! unchanged; the builder is purely additive sugar over it.
//!
//! # Cross-subsystem flow
//!
//! The Atlas is one consumer of the broadcast `TargetId` stream produced
//! by the Logic thread:
//!
//! ```text
//! signals mutate  →  EvaluatorTick::collect_dirty()  →  Vec<TargetId>
//!                                                       │
//!                                                       ▼
//!                                          atlas.mark_dirty_all(...)
//!                                                       │
//!                                                       ▼
//!                                          atlas.recompute_dirty(...)
//!                                                       │
//!                                                       ▼
//!                              per-target `dirty` bit, read off the
//!                              resolved [`TargetId`] and copied onto the
//!                              matching `TextLine`/`BoxInstance` in
//!                              `RenderFrame` — the Atlas is the only
//!                              subsystem that calls `mark_dirty_all`; the
//!                              encoder never broadcasts, it only reads the
//!                              dirty bit already attached to each primitive.
//! ```
//!
//! The Atlas filters the broadcast by [`TargetKind`](crate::frame::TargetKind)
//! and ignores foreign or stale entries. See [`LayoutAtlas::mark_dirty_all`]
//! for the filtering rules.

pub mod layout;
pub mod spatial;

pub use layout::{
    Align, AtlasError, AtlasNodeId, AtlasNodeSpec, ContainerStyle, FlexDir, GridItemPlacement,
    GridTrack, Justify, LayoutAtlas, LayoutAtlasBuilder, LeafSize, Spacing, StackAlign, TextLeaf,
};

pub use spatial::{CELL_SIZE, SpatialGrid};
