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
//! **The retained path is production, since RFC-0032.** A frame with no
//! structural change, no resize, no hot reload, no theme flip and no
//! overlay/route movement calls [`LayoutAtlas::begin_retained_build`] instead
//! of [`LayoutAtlas::clear`]: the Taffy tree, its cached geometry, the parent
//! map, the spatial grid and — critically — the **view generation** all
//! survive. Nodes are restyled in place, and only the ones whose resolved
//! layout inputs actually changed are marked dirty.
//!
//! That last one is why the dirty channel works at all now. `clear()` bumps
//! the generation, which invalidates every outstanding [`layout::TargetId`],
//! and while every frame cleared, the generation counter distinguished one
//! *frame* from another rather than one *view* from another — so a caller
//! could not have passed a valid dirty set even if it had one.
//!
//! **The rule that keeps this sound** (RFC-0032 §R3, INV-23):
//!
//! > Fingerprints decide what to **mark**. Taffy decides what to
//! > **recompute**. The spatial grid is rebuilt from **resolved rects**,
//! > never from fingerprints.
//!
//! A node that moved only because a sibling resized is recomputed by Taffy's
//! own dirty propagation and re-indexed by the full `rebuild_grid` walk. No
//! code here ever concludes that a rect is still valid — which is the only
//! reason a stale-but-tappable element is impossible rather than unlikely.
//!
//! **Two traps, both of which have a test:**
//!
//! - [`LayoutAtlas::recompute_dirty`] runs the measure protocol with **no
//!   sizer**, so every wrapping `Text` leaf it touches collapses to its
//!   natural single-line size. Production must use
//!   [`LayoutAtlas::recompute_dirty_with_text`]. The sizer-less form is for
//!   benchmarks and layout-only unit tests.
//! - The retained path **must reuse the stored [`layout::AtlasNodeId`]s**.
//!   `next_target_index()` is `nodes_by_index.len()`, so anything that
//!   re-derives ids reassigns every element's identity silently.
//!
//! **How to check you are still on the expected path:**
//! `crates/byard-compiler/tests/incremental_paths.rs` reads the
//! `telemetry`-gated counters in [`layout::path_counters`] after a real frame
//! and asserts which path was walked — including one test per eligibility
//! clause, because a fast path whose *deny* conditions are untested is not a
//! fast path, it is a hazard. `byard dev` prints the same answer once a
//! second as an `atlas retained · N node(s) marked` line.
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
