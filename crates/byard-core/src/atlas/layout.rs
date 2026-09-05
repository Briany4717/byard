//! Taffy-backed layout tree.
//!
//! See the module-level documentation in [`super`] for the design intent
//! and lifecycle contract.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::frame::{Rect, RenderFrame, TargetId, TargetKind, Viewport};
use taffy::style_helpers::{FromFr, FromLength, TaffyAuto, TaffyGridLine, TaffyGridSpan};
use taffy::{
    AlignItems, AvailableSpace, Dimension, Display, FlexDirection, GridPlacement,
    GridTemplateComponent, JustifyContent, LengthPercentage, LengthPercentageAuto, Line, NodeId,
    Overflow, Point, Position, Rect as TaffyRect, Size, Style, TaffyError, TaffyTree,
    TraversePartialTree,
};

use super::spatial::SpatialGrid;

/// Opaque identifier for a node owned by a [`LayoutAtlas`].
///
/// Wraps [`taffy::NodeId`] so the Atlas does not leak Taffy types into
/// the rest of the engine.
///
/// # Cross-atlas safety
///
/// `AtlasNodeId` is scoped to the [`LayoutAtlas`] instance that created it.
/// Every atlas is assigned a unique `instance_id` at construction time
/// (see [`LayoutAtlas::next_instance_id`]), and that id travels with every
/// `AtlasNodeId` it produces. Passing an ID to a different atlas instance
/// is rejected with [`AtlasError::ForeignNode`] rather than returning
/// incorrect geometry or hitting an opaque backend error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AtlasNodeId {
    node_id: NodeId,
    atlas_id: u32,
}

// `AtlasNodeId` must stay cheap to pass by value, that's the entire point
// of scoping it with a plain `u32` rather than, say, an `Arc<str>` tag.
// `taffy::NodeId` is an 8-byte, 8-byte-aligned newtype over `u64`, so
// `node_id` (8) + `atlas_id` (4) rounds up to 16 bytes of struct alignment
// padding. This assertion guards that budget: if a future field pushes
// `AtlasNodeId` past 16 bytes, the build fails here instead of silently
// regressing pass-by-value performance.
const _: () = assert!(
    std::mem::size_of::<AtlasNodeId>() <= 16,
    "AtlasNodeId exceeded its 16-byte CPU register optimization budget!"
);

/// Which constructor produced a build-order slot.
///
/// The retained build path (RFC-0032 §R4) re-walks the tree in the identical
/// order and reuses the node that already occupies each slot. That reuse is
/// only sound if the slot holds the *same kind* of node, so the kind is
/// recorded on the way in and checked on the way back, a mismatch aborts the
/// retained build rather than restyling a container as if it were a leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    /// [`LayoutAtlas::add_leaf`].
    Leaf,
    /// [`LayoutAtlas::add_flex_leaf`].
    FlexLeaf,
    /// [`LayoutAtlas::add_text_leaf`].
    TextLeaf,
    /// [`LayoutAtlas::add_container`].
    Container,
    /// [`LayoutAtlas::add_grid_container`].
    Grid,
    /// [`LayoutAtlas::add_stack_container`].
    Stack,
}

impl NodeKind {
    /// Whether this kind is created as a childless Taffy leaf.
    const fn is_leaf(self) -> bool {
        matches!(self, Self::Leaf | Self::FlexLeaf | Self::TextLeaf)
    }
}

/// Hashes a node's **resolved layout inputs** into a 64-bit fingerprint
/// (RFC-0032 §R2).
///
/// Two rules make this trustworthy, and both are the difference between a
/// fingerprint that answers "did this change?" and one that answers it wrongly
/// in a way nobody notices:
///
/// - **`f32`s are hashed through [`f32::to_bits`], never directly.** Hashing
///   the float would make `NaN` compare unequal to itself (an element
///   permanently dirty, wasteful but visible) and `-0.0` compare equal to
///   `0.0` (an element permanently *clean*, silent, and wrong).
/// - **The kind discriminant is hashed first**, so a leaf and a container that
///   happen to carry the same numbers never collide into "unchanged".
///
/// `FxHasher` rather than `DefaultHasher` (RFC-0032 §Q7): `endpoint_key` runs
/// once per animation, this runs once per node per frame. Nothing here is
/// adversarial.
struct LayoutFingerprint(rustc_hash::FxHasher);

impl LayoutFingerprint {
    fn new(kind: NodeKind) -> Self {
        let mut h = rustc_hash::FxHasher::default();
        (kind as u8).hash(&mut h);
        Self(h)
    }

    fn f32(&mut self, v: f32) -> &mut Self {
        v.to_bits().hash(&mut self.0);
        self
    }

    fn opt_f32(&mut self, v: Option<f32>) -> &mut Self {
        match v {
            Some(x) => {
                1u8.hash(&mut self.0);
                x.to_bits().hash(&mut self.0);
            }
            None => 0u8.hash(&mut self.0),
        }
        self
    }

    fn u8(&mut self, v: u8) -> &mut Self {
        v.hash(&mut self.0);
        self
    }

    fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }

    fn str(&mut self, v: &str) -> &mut Self {
        v.hash(&mut self.0);
        self
    }

    fn spacing(&mut self, s: Spacing) -> &mut Self {
        self.f32(s.top).f32(s.right).f32(s.bottom).f32(s.left)
    }

    fn tracks(&mut self, tracks: &[GridTrack]) -> &mut Self {
        (tracks.len() as u64).hash(&mut self.0);
        for t in tracks {
            match t {
                GridTrack::Fr(f) => {
                    self.u8(0).f32(*f);
                }
                GridTrack::Px(p) => {
                    self.u8(1).f32(*p);
                }
                GridTrack::Auto => {
                    self.u8(2);
                }
            }
        }
        self
    }

    fn container(&mut self, s: &ContainerStyle) -> &mut Self {
        self.opt_f32(s.width)
            .opt_f32(s.height)
            .u8(s.direction as u8)
            .f32(s.gap)
            .spacing(s.padding)
            .spacing(s.margin)
            .u8(s.align as u8)
            .u8(s.justify as u8)
            .f32(s.grow)
            .bool(s.scroll_x)
            .bool(s.scroll_y)
            .bool(s.absolute)
    }

    /// Folds in the *identity* of this node's children.
    ///
    /// A container whose own style is unchanged but whose child list is not
    /// must still be treated as changed, otherwise a reordered row would keep
    /// last frame's geometry. The retained path already refuses to run when
    /// the structure changed, so this is the second line of defence rather
    /// than the first, and it is cheap.
    fn children(&mut self, children: &[AtlasNodeId]) -> &mut Self {
        (children.len() as u64).hash(&mut self.0);
        for c in children {
            c.node_id.hash(&mut self.0);
        }
        self
    }

    fn finish(&self) -> u64 {
        self.0.finish()
    }
}

/// Explicit size for a leaf node.
#[derive(Debug, Clone, Copy)]
pub struct LeafSize {
    /// Width in logical pixels.
    pub width: f32,
    /// Height in logical pixels.
    pub height: f32,
}

impl LeafSize {
    /// Constructs a new leaf size.
    #[must_use]
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// Granular spacing for padding or margin (top, right, bottom, left).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spacing {
    /// Top spacing.
    pub top: f32,
    /// Right spacing.
    pub right: f32,
    /// Bottom spacing.
    pub bottom: f32,
    /// Left spacing.
    pub left: f32,
}

impl Spacing {
    /// Creates a Spacing with all sides set to the same value.
    #[must_use]
    pub const fn all(val: f32) -> Self {
        Self {
            top: val,
            right: val,
            bottom: val,
            left: val,
        }
    }

    /// Creates a Spacing with specific vertical and horizontal values.
    #[must_use]
    pub const fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }
}

/// One track in a `Grid` template (RFC-0018): a flexible fraction (`1fr`), a
/// fixed length in logical px (`100`), or an auto-sized track (`auto`). Maps to
/// a Taffy `GridTemplateComponent`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridTrack {
    /// A flexible fraction of the leftover space (`Nfr`).
    Fr(f32),
    /// A fixed length in logical pixels.
    Px(f32),
    /// An auto-sized track (fits its content).
    Auto,
}

/// Where a grid child sits in the grid (RFC-0018). `col_start`/`row_start` are
/// 1-based grid lines (CSS convention; negative counts from the end); `None`
/// leaves the axis to Taffy's auto-placement. Spans default to 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridItemPlacement {
    /// 1-based column line to start at, or `None` for auto-placement.
    pub col_start: Option<i16>,
    /// Number of columns to span (≥ 1).
    pub col_span: u16,
    /// 1-based row line to start at, or `None` for auto-placement.
    pub row_start: Option<i16>,
    /// Number of rows to span (≥ 1).
    pub row_span: u16,
}

/// A wrapping `Text` leaf (RFC-0005 default wrap). The atlas sizes it through a
/// [`TextSizer`](crate::text::TextSizer) callback **during** layout, so it
/// reflows to the width its parent offers instead of being pinned to a fixed
/// size measured up front. `width` fixes the wrap width when the `Text` carries
/// an explicit `width`; otherwise the leaf's width is `auto` and it wraps to the
/// available width. `fallback` is the natural single-line size, used only when
/// `compute` is called without a sizer (e.g. layout-only unit tests).
#[derive(Debug, Clone)]
pub struct TextLeaf {
    /// The string to shape.
    pub content: String,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Typographic weight on the CSS axis, `100..=900` (RFC-0034). Part of the
    /// leaf because it changes the shaped width, so it changes layout.
    pub weight: u16,
    /// Resolved font family, or `None` for the system sans-serif (RFC-0034).
    /// Part of the leaf for the same reason `weight` is: two faces set the
    /// same string to different widths, so the family is a layout input.
    pub family: Option<std::sync::Arc<str>>,
    /// Fixed wrap width in logical px, or `None` to wrap to the available width.
    pub width: Option<f32>,
    /// Natural single-line `(width, height)` fallback.
    pub fallback: (f32, f32),
}

/// 2-D alignment of a `ZStack`'s children within the stack rect (RFC-0018
/// `Align2D`). The first word is the block (vertical) edge, the second the
/// inline (horizontal) edge; the single-edge tokens centre on the other axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StackAlign {
    /// Centre on both axes (the default).
    #[default]
    Center,
    /// Top-left.
    TopStart,
    /// Top-right.
    TopEnd,
    /// Bottom-left.
    BottomStart,
    /// Bottom-right.
    BottomEnd,
    /// Top-centre.
    Top,
    /// Bottom-centre.
    Bottom,
    /// Left-centre.
    Start,
    /// Right-centre.
    End,
}

impl StackAlign {
    /// Maps to `(justify_items, align_items)`, the inline (x) and block (y)
    /// grid-item alignment within the single stacking cell.
    fn to_items(self) -> (AlignItems, AlignItems) {
        use taffy::AlignItems::{Center, End, Start};
        match self {
            Self::Center => (Center, Center),
            Self::TopStart => (Start, Start),
            Self::TopEnd => (End, Start),
            Self::BottomStart => (Start, End),
            Self::BottomEnd => (End, End),
            Self::Top => (Center, Start),
            Self::Bottom => (Center, End),
            Self::Start => (Start, Center),
            Self::End => (End, Center),
        }
    }
}

/// Main-axis direction of a flex container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexDir {
    /// Children flow left-to-right.
    #[default]
    Row,
    /// Children flow top-to-bottom.
    Column,
}

/// Cross-axis alignment of a flex container's children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// Pack against the cross-axis start.
    Start,
    /// Center on the cross axis.
    Center,
    /// Pack against the cross-axis end.
    End,
    /// Stretch to fill the cross axis (the default).
    #[default]
    Stretch,
}

/// Main-axis distribution of a flex container's children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    /// Pack against the main-axis start (the default).
    #[default]
    Start,
    /// Center on the main axis.
    Center,
    /// Pack against the main-axis end.
    End,
    /// Even space between children.
    Between,
    /// Even space around children.
    Around,
    /// Even space including the ends.
    Evenly,
}

/// Style for a container node, mapped onto a Taffy flex `Style`.
///
/// Marked `#[non_exhaustive]`; construct with [`ContainerStyle::new`] /
/// [`ContainerStyle::default`] and the `with_*` builders.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct ContainerStyle {
    /// Explicit width in logical pixels. `None` means "grow to fit children".
    pub width: Option<f32>,
    /// Explicit height in logical pixels. `None` means "grow to fit children".
    pub height: Option<f32>,
    /// Main-axis direction.
    pub direction: FlexDir,
    /// Space between children, in logical pixels.
    pub gap: f32,
    /// Granular padding, in logical pixels.
    pub padding: Spacing,
    /// Granular margin, in logical pixels.
    pub margin: Spacing,
    /// Cross-axis alignment of children.
    pub align: Align,
    /// Main-axis distribution of children.
    pub justify: Justify,
    /// Flex-grow factor (how much this node expands to fill its parent's main
    /// axis).
    pub grow: f32,
    /// Whether this is a **horizontal scroll container** (RFC-0005 `ScrollView`
    /// `axis: horizontal|both`): content is measured at natural width and
    /// overflows the fixed viewport on the inline axis (Taffy `overflow.x =
    /// Scroll`), rather than being shrunk to fit. The renderer clips and scrolls
    /// the overflow.
    pub scroll_x: bool,
    /// Whether this is a **vertical scroll container** (RFC-0005 `ScrollView`,
    /// the default `axis: vertical`): content overflows on the block axis
    /// (Taffy `overflow.y = Scroll`). Clipped and scrolled by the renderer.
    pub scroll_y: bool,
    /// Whether this node is **absolutely positioned** and pinned to its
    /// containing block's edges (Taffy `position: Absolute`, `inset: 0`)
    ///, RFC-0017 overlay layer. An absolute node is removed from its parent's
    /// flex flow (it neither displaces siblings nor is displaced by them) and
    /// stretched to fill the containing block, so several can stack over the
    /// same viewport rect independently. The overlay compositor uses this to
    /// float each overlay above the main tree without perturbing its layout.
    pub absolute: bool,
}

impl ContainerStyle {
    /// Constructs a `ContainerStyle` with the given explicit dimensions and
    /// flex defaults (row, stretch, start, no gap/padding/grow).
    #[must_use]
    pub fn new(width: Option<f32>, height: Option<f32>) -> Self {
        Self {
            width,
            height,
            ..Default::default()
        }
    }

    /// Sets the main-axis direction.
    #[must_use]
    pub fn with_direction(mut self, direction: FlexDir) -> Self {
        self.direction = direction;
        self
    }

    /// Sets the inter-child gap (logical px).
    #[must_use]
    pub fn with_gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Sets the padding (logical px).
    #[must_use]
    pub fn with_padding(mut self, padding: Spacing) -> Self {
        self.padding = padding;
        self
    }

    /// Sets the margin (logical px).
    #[must_use]
    pub fn with_margin(mut self, margin: Spacing) -> Self {
        self.margin = margin;
        self
    }

    /// Sets the cross-axis alignment.
    #[must_use]
    pub fn with_align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Sets the main-axis distribution.
    #[must_use]
    pub fn with_justify(mut self, justify: Justify) -> Self {
        self.justify = justify;
        self
    }

    /// Sets the flex-grow factor.
    #[must_use]
    pub fn with_grow(mut self, grow: f32) -> Self {
        self.grow = grow;
        self
    }

    /// Marks this a scroll container on the given axes (RFC-0005 `ScrollView`):
    /// content overflows the viewport where enabled instead of shrinking to fit.
    #[must_use]
    pub fn with_scroll_axes(mut self, scroll_x: bool, scroll_y: bool) -> Self {
        self.scroll_x = scroll_x;
        self.scroll_y = scroll_y;
        self
    }

    /// Marks this node absolutely positioned, pinned to fill its containing
    /// block (RFC-0017 overlay layer). See [`absolute`](Self::absolute).
    #[must_use]
    pub fn with_absolute(mut self, absolute: bool) -> Self {
        self.absolute = absolute;
        self
    }

    /// Builds the Taffy `Style` this container maps to.
    fn to_taffy(self) -> Style {
        Style {
            size: Size {
                width: self.width.map_or(Dimension::auto(), Dimension::from_length),
                height: self
                    .height
                    .map_or(Dimension::auto(), Dimension::from_length),
            },
            flex_direction: match self.direction {
                FlexDir::Row => FlexDirection::Row,
                FlexDir::Column => FlexDirection::Column,
            },
            gap: Size {
                width: LengthPercentage::from_length(self.gap),
                height: LengthPercentage::from_length(self.gap),
            },
            padding: TaffyRect {
                left: LengthPercentage::from_length(self.padding.left),
                right: LengthPercentage::from_length(self.padding.right),
                top: LengthPercentage::from_length(self.padding.top),
                bottom: LengthPercentage::from_length(self.padding.bottom),
            },
            margin: TaffyRect {
                left: LengthPercentageAuto::length(self.margin.left),
                right: LengthPercentageAuto::length(self.margin.right),
                top: LengthPercentageAuto::length(self.margin.top),
                bottom: LengthPercentageAuto::length(self.margin.bottom),
            },
            align_items: Some(match self.align {
                Align::Start => AlignItems::FlexStart,
                Align::Center => AlignItems::Center,
                Align::End => AlignItems::FlexEnd,
                Align::Stretch => AlignItems::Stretch,
            }),
            justify_content: Some(match self.justify {
                Justify::Start => JustifyContent::FlexStart,
                Justify::Center => JustifyContent::Center,
                Justify::End => JustifyContent::FlexEnd,
                Justify::Between => JustifyContent::SpaceBetween,
                Justify::Around => JustifyContent::SpaceAround,
                Justify::Evenly => JustifyContent::SpaceEvenly,
            }),
            flex_grow: self.grow,
            // RFC-0017 overlay layer: an absolute node leaves its parent's flex
            // flow and is pinned to the containing block's edges (inset 0), so it
            // stretches to fill the viewport and stacks over siblings without
            // displacing them. A relative node keeps Taffy's `auto` insets.
            position: if self.absolute {
                Position::Absolute
            } else {
                Position::Relative
            },
            inset: if self.absolute {
                TaffyRect {
                    left: LengthPercentageAuto::length(0.0),
                    right: LengthPercentageAuto::length(0.0),
                    top: LengthPercentageAuto::length(0.0),
                    bottom: LengthPercentageAuto::length(0.0),
                }
            } else {
                TaffyRect::auto()
            },
            // RFC-0005 `ScrollView`: a scroll container measures its content at
            // natural size and lets it overflow the fixed viewport on the
            // scrolling axes, instead of flex-shrinking children to fit. The
            // renderer clips and scrolls the overflow.
            overflow: Point {
                x: if self.scroll_x {
                    Overflow::Scroll
                } else {
                    Overflow::Visible
                },
                y: if self.scroll_y {
                    Overflow::Scroll
                } else {
                    Overflow::Visible
                },
            },
            ..Default::default()
        }
    }
}

/// Declarative description of a layout (sub)tree.
///
/// `AtlasNodeSpec` values are plain data, building one never touches
/// Taffy or any [`LayoutAtlas`], so describing a tree can never fail.
/// They're produced with [`LayoutAtlasBuilder::leaf`] /
/// [`LayoutAtlasBuilder::container`] and committed to a real atlas in one
/// recursive pass via [`LayoutAtlas::build`] or [`LayoutAtlas::build_root`].
///
/// This is the fluent construction API, it sits on top
/// of [`LayoutAtlas::add_leaf`] / [`LayoutAtlas::add_container`] /
/// [`LayoutAtlas::set_root`] and calls them in the exact same depth-first,
/// children-before-parent order a hand-written imperative sequence would,
/// so it produces identical [`AtlasNodeId`]s. The low-level methods are
/// the tested foundation (PR #14) and are unchanged by this type.
#[derive(Debug, Clone)]
pub enum AtlasNodeSpec {
    /// Describes a leaf, mirroring [`LayoutAtlas::add_leaf`].
    Leaf(LeafSize),
    /// Describes a container and its children, mirroring
    /// [`LayoutAtlas::add_container`]. Children are built and attached in
    /// iteration order.
    Container(ContainerStyle, Vec<AtlasNodeSpec>),
}

/// Entry point for building [`AtlasNodeSpec`] trees fluently.
///
/// `LayoutAtlasBuilder` does not wrap a [`LayoutAtlas`], it has no state
/// of its own. It's a pair of associated functions that produce
/// [`AtlasNodeSpec`] values, which `LayoutAtlas::build`/`build_root` then
/// commit. Nesting `container` calls lets a multi-level tree of mixed
/// leaves and containers be expressed as a single chained expression:
///
/// ```
/// use byard_core::atlas::{ContainerStyle, LayoutAtlas, LayoutAtlasBuilder as B, LeafSize};
///
/// let mut atlas = LayoutAtlas::new();
/// let root = atlas.build_root(
///     B::container(ContainerStyle::new(Some(300.0), Some(200.0)), [
///         B::leaf(LeafSize::new(50.0, 50.0)),
///         B::container(ContainerStyle::default(), [
///             B::leaf(LeafSize::new(20.0, 20.0)),
///         ]),
///     ]),
/// ).unwrap();
/// # let _ = root;
/// ```
pub struct LayoutAtlasBuilder;

impl LayoutAtlasBuilder {
    /// Describes a leaf node with the given size.
    #[must_use]
    pub const fn leaf(size: LeafSize) -> AtlasNodeSpec {
        AtlasNodeSpec::Leaf(size)
    }

    /// Describes a container node wrapping `children`, built in order.
    #[must_use]
    pub fn container(
        style: ContainerStyle,
        children: impl IntoIterator<Item = AtlasNodeSpec>,
    ) -> AtlasNodeSpec {
        AtlasNodeSpec::Container(style, children.into_iter().collect())
    }
}

/// Errors produced by the [`LayoutAtlas`].
#[non_exhaustive]
#[derive(Debug)]
pub enum AtlasError {
    /// The layout backend returned an error during tree construction
    /// or layout computation.
    Backend(String),

    /// An [`AtlasNodeId`] was used with a [`LayoutAtlas`] other than the
    /// one that created it.
    ///
    /// This is a misuse error, not a backend failure, without this check,
    /// passing an id from one atlas into a sibling atlas would silently
    /// read or mutate unrelated layout state (or panic deep inside Taffy),
    /// per the caveat this variant closes off.
    ForeignNode {
        /// The `instance_id` of the [`LayoutAtlas`] the id was used with.
        expected: u32,
        /// The `instance_id` of the [`LayoutAtlas`] that actually created
        /// the id.
        actual: u32,
    },

    /// A retained build (RFC-0032 §R4) reached a build-order slot it could not
    /// reuse, the walk produced a different node kind, a different child
    /// count, or more nodes than the retained tree holds.
    ///
    /// This is a *recoverable* signal rather than a failure: the caller
    /// finishes the walk, [`LayoutAtlas::end_retained_build`] returns `false`,
    /// and the frame is rebuilt from scratch. It exists as an error rather
    /// than a silent fallback so a retained build that is going wrong cannot
    /// quietly return a node id belonging to some other element.
    RetainedSlotMismatch {
        /// The build-order slot that could not be reused.
        index: usize,
    },
}

impl AtlasError {
    pub(crate) fn from_taffy(e: &TaffyError) -> Self {
        Self::Backend(e.to_string())
    }
}

impl fmt::Display for AtlasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(message) => write!(f, "layout backend error: {message}"),
            Self::ForeignNode { expected, actual } => write!(
                f,
                "AtlasNodeId belongs to atlas instance {actual}, but was used with atlas instance {expected}"
            ),
            Self::RetainedSlotMismatch { index } => write!(
                f,
                "retained layout build could not reuse build-order slot {index}; \
                 the frame must be rebuilt from scratch"
            ),
        }
    }
}

impl std::error::Error for AtlasError {}

impl From<TaffyError> for AtlasError {
    fn from(e: TaffyError) -> Self {
        Self::Backend(e.to_string())
    }
}

/// Which of the atlas's paths a frame actually took (RFC-0001 §4.1).
///
/// The atlas offers a full path (`clear` → rebuild → [`LayoutAtlas::compute`])
/// and a retained one ([`LayoutAtlas::mark_dirty_all`] →
/// [`LayoutAtlas::recompute_dirty`]), and a dirty-target channel to the frame.
/// All three were validated in isolation, unit tests, benchmarks, and none of
/// them had an assertion that fails when *production* stops taking them. This
/// is that assertion's raw material: integration tests read these counters
/// after a real frame and check which path was walked.
///
/// **Gated on the `telemetry` feature**, so the counters do not exist in a
/// shipped build. They are thread-local: the atlas lives on the logic thread
/// (INV-2) and a process-wide counter would be polluted by the other tests
/// `cargo test` runs concurrently.
#[cfg(feature = "telemetry")]
pub mod path_counters {
    use std::cell::Cell;

    thread_local! {
        static CLEARS: Cell<u64> = const { Cell::new(0) };
        static FULL_COMPUTES: Cell<u64> = const { Cell::new(0) };
        static RETAINED_RECOMPUTES: Cell<u64> = const { Cell::new(0) };
        static RETAINED_ATTEMPTS: Cell<u64> = const { Cell::new(0) };
        static RETAINED_ROLLBACKS: Cell<u64> = const { Cell::new(0) };
        static POPULATE_CALLS: Cell<u64> = const { Cell::new(0) };
        static POPULATE_DIRTY_TARGETS: Cell<u64> = const { Cell::new(0) };
        static POPULATE_DIRTY_MATCHED: Cell<u64> = const { Cell::new(0) };
    }

    /// A snapshot of this thread's atlas path counters.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Counts {
        /// `clear()` calls, each one tears the Taffy tree down and bumps the
        /// view generation, invalidating every previously-issued `TargetId`.
        pub clears: u64,
        /// Full layout passes (`compute` / `compute_with_text`).
        pub full_computes: u64,
        /// Retained layout passes (`recompute_dirty`).
        pub retained_recomputes: u64,
        /// Retained builds **opened** (`begin_retained_build`), i.e. frames the
        /// caller's RFC-0032 §R4 whitelist judged eligible.
        ///
        /// This is what separates "the whitelist rejected this frame" from "the
        /// whitelist let it through and the atlas rolled it back". The two are
        /// indistinguishable in [`clears`](Self::clears) and
        /// [`full_computes`](Self::full_computes), because a rollback ends in
        /// exactly the same `clear` + full pass, so without this counter every
        /// clause of the whitelist can be deleted with the suite still green,
        /// while production quietly pays for a failed attempt before every
        /// rebuild (INV-18).
        pub retained_attempts: u64,
        /// Retained builds opened and then **discarded** (`end_retained_build`
        /// returned `false`).
        ///
        /// Correct, the default-deny verdict is what keeps a half-applied
        /// build off the screen, and pure waste: the walk ran twice. In steady
        /// state this must be `0`; a non-zero count means the whitelist and the
        /// atlas disagree about what is retainable.
        pub retained_rollbacks: u64,
        /// `populate_frame` calls.
        pub populate_calls: u64,
        /// Total `TargetId`s handed to `populate_frame` across those calls,
        /// **including** stale-generation ones, which is the point: a caller
        /// passing last frame's targets is not the same as passing none.
        pub populate_dirty_targets: u64,
        /// How many of those targets actually matched a live node and marked
        /// something dirty in the frame.
        pub populate_dirty_matched: u64,
    }

    /// Reads this thread's counters.
    #[must_use]
    pub fn snapshot() -> Counts {
        Counts {
            clears: CLEARS.with(Cell::get),
            full_computes: FULL_COMPUTES.with(Cell::get),
            retained_recomputes: RETAINED_RECOMPUTES.with(Cell::get),
            retained_attempts: RETAINED_ATTEMPTS.with(Cell::get),
            retained_rollbacks: RETAINED_ROLLBACKS.with(Cell::get),
            populate_calls: POPULATE_CALLS.with(Cell::get),
            populate_dirty_targets: POPULATE_DIRTY_TARGETS.with(Cell::get),
            populate_dirty_matched: POPULATE_DIRTY_MATCHED.with(Cell::get),
        }
    }

    /// Resets this thread's counters, so a test can measure one frame rather
    /// than a session.
    pub fn reset() {
        CLEARS.with(|c| c.set(0));
        FULL_COMPUTES.with(|c| c.set(0));
        RETAINED_RECOMPUTES.with(|c| c.set(0));
        RETAINED_ATTEMPTS.with(|c| c.set(0));
        RETAINED_ROLLBACKS.with(|c| c.set(0));
        POPULATE_CALLS.with(|c| c.set(0));
        POPULATE_DIRTY_TARGETS.with(|c| c.set(0));
        POPULATE_DIRTY_MATCHED.with(|c| c.set(0));
    }

    pub(super) fn record_clear() {
        CLEARS.with(|c| c.set(c.get() + 1));
    }

    pub(super) fn record_full_compute() {
        FULL_COMPUTES.with(|c| c.set(c.get() + 1));
    }

    pub(super) fn record_retained_recompute() {
        RETAINED_RECOMPUTES.with(|c| c.set(c.get() + 1));
    }

    /// Records a retained build the **caller** discarded after the atlas had
    /// accepted it.
    ///
    /// [`LayoutAtlas::end_retained_build`](super::LayoutAtlas::end_retained_build)
    /// counts its own `false` verdicts, but the caller layers its own checks on
    /// top of that verdict (RFC-0032 §R4 keeps the redundant `flat_ids`
    /// comparison deliberately). A discard for one of those reasons costs
    /// exactly as much as the atlas's own and must not read as `0`.
    pub fn note_retained_rollback() {
        record_retained_rollback();
    }

    pub(super) fn record_retained_attempt() {
        RETAINED_ATTEMPTS.with(|c| c.set(c.get() + 1));
    }

    pub(super) fn record_retained_rollback() {
        RETAINED_ROLLBACKS.with(|c| c.set(c.get() + 1));
    }

    pub(super) fn record_populate(targets: usize, matched: usize) {
        POPULATE_CALLS.with(|c| c.set(c.get() + 1));
        POPULATE_DIRTY_TARGETS.with(|c| c.set(c.get() + targets as u64));
        POPULATE_DIRTY_MATCHED.with(|c| c.set(c.get() + matched as u64));
    }
}

/// The `telemetry`-off stand-ins: every recording call compiles to nothing, so
/// a shipped build carries no counters at all.
#[cfg(not(feature = "telemetry"))]
pub mod path_counters {
    /// The `telemetry`-off counterpart of the real
    /// [`Counts`](super::path_counters::Counts): the same shape, always zero,
    /// so consumers (the `byard dev` readout, the frame budget suite) compile
    /// and read identically in both builds instead of being written twice
    /// behind a `cfg`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Counts {
        /// Always `0` without the `telemetry` feature.
        pub clears: u64,
        /// Always `0` without the `telemetry` feature.
        pub full_computes: u64,
        /// Always `0` without the `telemetry` feature.
        pub retained_recomputes: u64,
        /// Always `0` without the `telemetry` feature.
        pub retained_attempts: u64,
        /// Always `0` without the `telemetry` feature.
        pub retained_rollbacks: u64,
        /// Always `0` without the `telemetry` feature.
        pub populate_calls: u64,
        /// Always `0` without the `telemetry` feature.
        pub populate_dirty_targets: u64,
        /// Always `0` without the `telemetry` feature.
        pub populate_dirty_matched: u64,
    }

    /// Always the zero snapshot without the `telemetry` feature.
    #[must_use]
    pub const fn snapshot() -> Counts {
        Counts {
            clears: 0,
            full_computes: 0,
            retained_recomputes: 0,
            retained_attempts: 0,
            retained_rollbacks: 0,
            populate_calls: 0,
            populate_dirty_targets: 0,
            populate_dirty_matched: 0,
        }
    }

    /// No-op without the `telemetry` feature.
    pub const fn reset() {}

    /// No-op without the `telemetry` feature.
    pub const fn note_retained_rollback() {}

    pub(super) const fn record_clear() {}
    pub(super) const fn record_full_compute() {}
    pub(super) const fn record_retained_recompute() {}
    pub(super) const fn record_retained_attempt() {}
    pub(super) const fn record_retained_rollback() {}
    pub(super) const fn record_populate(_targets: usize, _matched: usize) {}
}

/// Two-phase lifecycle state of a [`LayoutAtlas`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtlasState {
    /// Nodes can be added and modified. Querying resolved geometry panics.
    Building,
    /// Resolved geometry is accessible. Adding or modifying nodes panics.
    Computed,
}

/// Layout tree backed by Taffy.
///
/// See the module-level docs for the lifecycle contract and the RFC for
/// the architectural rationale.
pub struct LayoutAtlas {
    tree: TaffyTree<u32>,
    root: Option<AtlasNodeId>,
    state: AtlasState,
    children_scratch: Vec<NodeId>,
    grid: SpatialGrid,
    /// Reverse lookup from `TargetId.index` to the underlying node.
    /// Populated as nodes are added; reset on `clear()`.
    nodes_by_index: Vec<AtlasNodeId>,
    /// View generation. Incremented on `clear()` so `TargetId`s from
    /// previous views are silently rejected by `mark_dirty_all`.
    /// Wraps via `wrapping_add` after 65 535 clears, see the doc on
    /// `clear()` for the rationale.
    current_generation: u16,
    /// Unique id for this atlas instance, assigned at construction by
    /// [`Self::next_instance_id`]. Stamped onto every [`AtlasNodeId`] this
    /// atlas produces so a foreign id can be rejected in `O(1)`.
    instance_id: u32,
    parents: rustc_hash::FxHashMap<NodeId, NodeId>,
    /// Wrapping-`Text` leaves (RFC-0005 default wrap), keyed by their Taffy node.
    /// The measure closure in [`compute`](Self::compute) looks a leaf up here to
    /// shape it to the width the parent offers. Rebuilt each view (cleared on
    /// [`clear()`](Self::clear)).
    text_specs: HashMap<NodeId, TextLeaf>,
    /// What kind of node occupies each build-order slot, parallel to
    /// [`Self::nodes_by_index`]. Only read by the retained build.
    node_kinds: Vec<NodeKind>,
    /// Hash of the resolved layout inputs of each build-order slot, parallel
    /// to [`Self::nodes_by_index`] (RFC-0032 §R2).
    layout_fingerprints: Vec<u64>,
    /// Explicit grid placements, so a retained build can tell a re-application
    /// of the same placement (skip) from a real change (restyle + mark).
    grid_items: rustc_hash::FxHashMap<NodeId, GridItemPlacement>,
    /// Nodes whose style was rewritten during the current retained build,
    /// [`Self::set_grid_item`] must re-apply its placement on top of a
    /// rewritten style even when the placement itself did not change.
    restyled: rustc_hash::FxHashSet<NodeId>,
    /// State of the in-progress retained build, or `None` on the full path.
    retained: Option<RetainedBuild>,
    /// The targets whose layout inputs changed this frame, in build order.
    ///
    /// Reused across frames (cleared, never reallocated in steady state) and
    /// handed to [`Self::populate_frame`] by the interpreter, this is the
    /// dirty set RFC-0001 §2.2 described and the runtime did not produce.
    layout_dirty: Vec<TargetId>,
}

/// Bookkeeping for one retained build pass (RFC-0032 §R4).
struct RetainedBuild {
    /// Which build-order slot the next `add_*` call will occupy.
    cursor: usize,
    /// Set when a slot could not be reused. The build still runs to
    /// completion (so the caller's walk is not left half-done) and is then
    /// discarded wholesale in favour of a full rebuild.
    mismatch: bool,
}

impl LayoutAtlas {
    /// Creates a new, empty atlas in the `Building` state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tree: TaffyTree::new(),
            root: None,
            state: AtlasState::Building,
            children_scratch: Vec::new(),
            grid: SpatialGrid::new(),
            nodes_by_index: Vec::new(),
            current_generation: 0,
            instance_id: Self::next_instance_id(),
            parents: rustc_hash::FxHashMap::default(),
            text_specs: HashMap::new(),
            node_kinds: Vec::new(),
            layout_fingerprints: Vec::new(),
            grid_items: rustc_hash::FxHashMap::default(),
            restyled: rustc_hash::FxHashSet::default(),
            retained: None,
            layout_dirty: Vec::new(),
        }
    }

    /// Returns this atlas's unique instance id.
    ///
    /// Every [`AtlasNodeId`] this atlas produces carries this id, so it can
    /// be rejected with [`AtlasError::ForeignNode`] if later used against a
    /// different `LayoutAtlas`.
    #[must_use]
    pub const fn instance_id(&self) -> u32 {
        self.instance_id
    }

    /// Allocates the next globally unique atlas instance id.
    ///
    /// Backed by a function-local `AtomicU32` rather than a module-level
    /// static, keeps the counter's existence scoped to the one place that
    /// uses it. `Relaxed` ordering is sufficient: callers only need a
    /// distinct value per atlas, not synchronization with any other memory
    /// access.
    fn next_instance_id() -> u32 {
        static NEXT_INSTANCE_ID: AtomicU32 = AtomicU32::new(0);
        NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns `Ok(())` if `node` was created by this atlas instance, or
    /// [`AtlasError::ForeignNode`] otherwise.
    fn validate_node(&self, node: AtlasNodeId) -> Result<(), AtlasError> {
        if node.atlas_id == self.instance_id {
            Ok(())
        } else {
            Err(AtlasError::ForeignNode {
                expected: self.instance_id,
                actual: node.atlas_id,
            })
        }
    }

    /// Clears the tree but retains internal capacity.
    ///
    /// Increments the internal view generation, which causes any
    /// [`TargetId`]s produced before this call to be silently rejected
    /// by [`Self::mark_dirty_all`]. The generation wraps after `u16::MAX`
    /// clears, the collision probability with a stale `TargetId`
    /// surviving that long is statistically negligible (see project notes
    /// on `TargetId` packing).
    pub fn clear(&mut self) {
        path_counters::record_clear();
        self.tree.clear();
        self.root = None;
        self.state = AtlasState::Building;
        self.children_scratch.clear();
        self.grid.clear();
        self.nodes_by_index.clear();
        self.parents.clear();
        self.text_specs.clear();
        self.node_kinds.clear();
        self.layout_fingerprints.clear();
        self.grid_items.clear();
        self.restyled.clear();
        self.retained = None;
        self.layout_dirty.clear();
        self.current_generation = self.current_generation.wrapping_add(1);
    }

    /// Opens a **retained build** (RFC-0032 §R3/§R4): the caller re-walks the
    /// same tree in the same order, and each `add_*` call reuses the node that
    /// already occupies that build-order slot instead of creating a new one.
    ///
    /// Nothing is torn down, the Taffy tree, its cached geometry, the parent
    /// map, the spatial grid and, critically, the **view generation** all
    /// survive. That last one is the point: [`Self::clear`] bumps the
    /// generation, which invalidates every outstanding [`TargetId`], which is
    /// why the dirty channel could never be used while every frame cleared.
    ///
    /// A slot whose style hash differs from last frame's is restyled and
    /// marked dirty in Taffy, and its target lands in
    /// [`Self::layout_dirty_targets`]. **Taffy then decides what to
    /// recompute**, this method never decides which *rects* changed, only
    /// which *inputs* did (RFC-0032 §R3, INV-23).
    ///
    /// The caller must finish with [`Self::end_retained_build`] and honour its
    /// verdict.
    ///
    /// # Panics
    ///
    /// Panics if the atlas has no nodes, there is nothing to retain, and the
    /// caller should have taken the full path.
    pub fn begin_retained_build(&mut self) {
        assert!(
            !self.nodes_by_index.is_empty(),
            "LayoutAtlas::begin_retained_build called on an empty atlas, \
             the full build path is the only correct one for the first frame"
        );
        path_counters::record_retained_attempt();
        self.state = AtlasState::Building;
        self.root = None;
        self.children_scratch.clear();
        self.restyled.clear();
        self.layout_dirty.clear();
        self.retained = Some(RetainedBuild {
            cursor: 0,
            mismatch: false,
        });
    }

    /// Closes a retained build and reports whether it may be used.
    ///
    /// Returns `false` when any slot could not be reused or when the caller's
    /// walk produced a different number of nodes than the retained tree holds.
    /// A `false` verdict is **not** an error the caller may ignore: the atlas
    /// is now in an inconsistent state and the caller must [`clear`](Self::clear)
    /// and rebuild from scratch before computing.
    ///
    /// The default is the full rebuild, and every way of leaving this method
    /// unsure returns `false` (RFC-0032 §R4, default-deny).
    pub fn end_retained_build(&mut self) -> bool {
        let Some(build) = self.retained.take() else {
            return false;
        };
        let complete = build.cursor == self.nodes_by_index.len();
        if !build.mismatch && complete {
            self.state = AtlasState::Computed;
            return true;
        }
        // The verdict is correct and the walk was still wasted: the caller now
        // has to clear and build the same tree a second time. Counted so a
        // whitelist that has stopped rejecting what it should is visible as a
        // number rather than as an unexplained frame cost.
        path_counters::record_retained_rollback();
        false
    }

    /// The targets whose layout inputs changed during the last retained build
    /// (RFC-0032 §R3 step 5), what [`Self::populate_frame`] wants.
    #[must_use]
    pub fn layout_dirty_targets(&self) -> &[TargetId] {
        &self.layout_dirty
    }

    /// Places a node into the next build-order slot: creates it on the full
    /// path, or reuses (and restyles when its fingerprint moved) the node that
    /// already occupies the slot on the retained path.
    ///
    /// Every `add_*` constructor funnels through here, which is what keeps the
    /// two paths from drifting: there is exactly one place that decides what a
    /// build-order slot means.
    fn place(
        &mut self,
        kind: NodeKind,
        fingerprint: u64,
        style: Style,
        children: &[AtlasNodeId],
        text: Option<TextLeaf>,
    ) -> Result<AtlasNodeId, AtlasError> {
        let Some(cursor) = self.retained.as_ref().map(|r| r.cursor) else {
            return self.create_node(kind, fingerprint, style, children, text);
        };

        // Default-deny: anything about this slot that is not exactly what the
        // last build left here aborts the retained pass.
        let reusable = self.nodes_by_index.get(cursor).copied().filter(|_| {
            self.node_kinds.get(cursor) == Some(&kind)
                && self.layout_fingerprints.len() > cursor
                && self.tree.child_count(self.nodes_by_index[cursor].node_id) == children.len()
        });
        let Some(id) = reusable else {
            if let Some(r) = &mut self.retained {
                r.mismatch = true;
                r.cursor += 1;
            }
            return Err(AtlasError::RetainedSlotMismatch { index: cursor });
        };
        debug_assert!(
            children
                .iter()
                .zip(self.tree.children(id.node_id).unwrap_or_default())
                .all(|(c, t)| c.node_id == t),
            "retained slot {cursor} holds a different child list than the walk produced"
        );

        if self.layout_fingerprints[cursor] != fingerprint {
            self.tree
                .set_style(id.node_id, style)
                .map_err(|e| AtlasError::from_taffy(&e))?;
            self.layout_fingerprints[cursor] = fingerprint;
            self.restyled.insert(id.node_id);
            if let Some(spec) = text {
                self.text_specs.insert(id.node_id, spec);
            }
            self.mark_layout_dirty(cursor, id);
        }

        if let Some(r) = &mut self.retained {
            r.cursor += 1;
        }
        Ok(id)
    }

    /// Marks the node at build-order slot `index` dirty in Taffy and records
    /// its [`TargetId`] for [`Self::populate_frame`].
    fn mark_layout_dirty(&mut self, index: usize, id: AtlasNodeId) {
        // Taffy propagates the mark up to the root itself, so a sibling that
        // reflows because *this* node resized is recomputed without anyone
        // here working out which siblings those are (RFC-0032 §R3 step 3).
        let _ = self.tree.mark_dirty(id.node_id);
        #[allow(clippy::cast_possible_truncation)]
        let raw_index = index as u32;
        self.layout_dirty.push(TargetId::new(
            raw_index,
            self.current_generation,
            TargetKind::AtlasNode as u16,
        ));
    }

    /// The full path: create a fresh Taffy node and append it to the
    /// build-order tables.
    fn create_node(
        &mut self,
        kind: NodeKind,
        fingerprint: u64,
        style: Style,
        children: &[AtlasNodeId],
        text: Option<TextLeaf>,
    ) -> Result<AtlasNodeId, AtlasError> {
        let next_index = self.next_target_index();
        let node = if children.is_empty() && kind.is_leaf() {
            self.tree
                .new_leaf_with_context(style, next_index)
                .map_err(|e| AtlasError::from_taffy(&e))?
        } else {
            self.children_scratch.clear();
            self.children_scratch
                .extend(children.iter().map(|c| c.node_id));
            let node = self
                .tree
                .new_with_children(style, &self.children_scratch)
                .map_err(|e| AtlasError::from_taffy(&e))?;
            self.tree
                .set_node_context(node, Some(next_index))
                .map_err(|e| AtlasError::from_taffy(&e))?;
            node
        };
        for &child in children {
            self.parents.insert(child.node_id, node);
        }
        if let Some(spec) = text {
            self.text_specs.insert(node, spec);
        }
        let id = AtlasNodeId {
            node_id: node,
            atlas_id: self.instance_id,
        };
        self.nodes_by_index.push(id);
        self.node_kinds.push(kind);
        self.layout_fingerprints.push(fingerprint);
        Ok(id)
    }

    /// Adds a leaf node with an explicit size.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state. Call [`Self::clear`]
    /// before adding new nodes.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if the underlying engine refuses the
    /// node (extremely rare; indicates resource exhaustion).
    pub fn add_leaf(&mut self, size: LeafSize) -> Result<AtlasNodeId, AtlasError> {
        self.assert_building("add_leaf");

        let style = Style {
            size: Size {
                width: Dimension::from_length(size.width),
                height: Dimension::from_length(size.height),
            },
            ..Default::default()
        };

        let mut fp = LayoutFingerprint::new(NodeKind::Leaf);
        fp.f32(size.width).f32(size.height);
        self.place(NodeKind::Leaf, fp.finish(), style, &[], None)
    }

    /// Adds a **flexible leaf**, a node with no intrinsic size that absorbs the
    /// free space of its parent's main axis (RFC-0005 `Spacer`: "flexible gap",
    /// `grow` / `basis`).
    ///
    /// `basis` is the leaf's main-axis size *before* growing (flex-basis, so the
    /// parent's direction decides which axis it means) and `grow` is its share of
    /// whatever is left over. It never shrinks below `basis`. A `grow` of `0`
    /// degenerates to a fixed `basis`-sized gap, which is what makes `Spacer
    /// #[grow: 0, basis: 12]` an ordinary spacer.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state. Call [`Self::clear`]
    /// before adding new nodes.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if the backend refuses the node.
    pub fn add_flex_leaf(&mut self, grow: f32, basis: f32) -> Result<AtlasNodeId, AtlasError> {
        self.assert_building("add_flex_leaf");

        let style = Style {
            flex_basis: Dimension::from_length(basis.max(0.0)),
            flex_grow: grow.max(0.0),
            flex_shrink: 0.0,
            ..Default::default()
        };

        let mut fp = LayoutFingerprint::new(NodeKind::FlexLeaf);
        fp.f32(basis.max(0.0)).f32(grow.max(0.0));
        self.place(NodeKind::FlexLeaf, fp.finish(), style, &[], None)
    }

    /// Adds a **wrapping `Text` leaf** (RFC-0005 default wrap): a leaf whose size
    /// is resolved by the [`TextSizer`](crate::text::TextSizer) callback during
    /// [`compute`](Self::compute), so it reflows to the width its parent offers.
    /// A `spec.width` of `Some` fixes the wrap width (an explicit `Text` width);
    /// `None` leaves the width `auto` and wraps to the available space.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if the backend refuses the node.
    pub fn add_text_leaf(&mut self, spec: TextLeaf) -> Result<AtlasNodeId, AtlasError> {
        self.assert_building("add_text_leaf");

        let style = Style {
            size: Size {
                width: spec.width.map_or(Dimension::auto(), Dimension::from_length),
                height: Dimension::auto(),
            },
            ..Default::default()
        };

        // **Text content is layout-class** (RFC-0032 §R2), the single most
        // important row of that table. It is what lets a clean text leaf skip
        // glyph shaping entirely, which the encode breakdown measured at
        // 84–98 % of the frame's encode cost, and it is what would silently
        // freeze last frame's line breaks if it were left out.
        let mut fp = LayoutFingerprint::new(NodeKind::TextLeaf);
        fp.str(&spec.content)
            .f32(spec.font_size)
            // RFC-0034: the weight changes the shaped width, so a leaf whose
            // weight moved is a leaf that must re-measure. Left out of the
            // fingerprint it would keep last frame's line breaks at the new
            // weight — the silent staleness this fingerprint exists to stop.
            .f32(f32::from(spec.weight))
            // RFC-0034: and the family, for exactly the same reason. A leaf
            // that changed face and kept its fingerprint keeps last frame's
            // measurement, which is the whole class of staleness INV-26 names.
            .str(spec.family.as_deref().unwrap_or(""))
            .opt_f32(spec.width)
            .f32(spec.fallback.0)
            .f32(spec.fallback.1);
        self.place(NodeKind::TextLeaf, fp.finish(), style, &[], Some(spec))
    }

    /// Adds a container node that wraps the given children.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if the underlying engine refuses the
    /// node, or if any child has already been attached to another parent.
    /// Returns [`AtlasError::ForeignNode`] if any child was created by a
    /// different `LayoutAtlas` instance.
    pub fn add_container(
        &mut self,
        style: ContainerStyle,
        children: &[AtlasNodeId],
    ) -> Result<AtlasNodeId, AtlasError> {
        self.assert_building("add_container");

        for &child in children {
            self.validate_node(child)?;
        }

        let taffy_style = style.to_taffy();
        let mut fp = LayoutFingerprint::new(NodeKind::Container);
        fp.container(&style).children(children);
        self.place(
            NodeKind::Container,
            fp.finish(),
            taffy_style,
            children,
            None,
        )
    }

    /// Adds a **CSS-grid** container (RFC-0018 `Grid`) wrapping `children`.
    ///
    /// The `style` supplies the shared box properties (size, padding, margin,
    /// grow, alignment); this method overrides the display mode to grid and sets
    /// the column/row track templates and the per-axis gaps. Children are
    /// auto-placed left-to-right, top-to-bottom by default; call
    /// [`set_grid_item`](Self::set_grid_item) on a child to place it explicitly.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if the backend refuses the node, or
    /// [`AtlasError::ForeignNode`] if any child came from another atlas.
    pub fn add_grid_container(
        &mut self,
        style: ContainerStyle,
        columns: &[GridTrack],
        rows: &[GridTrack],
        col_gap: f32,
        row_gap: f32,
        children: &[AtlasNodeId],
    ) -> Result<AtlasNodeId, AtlasError> {
        self.assert_building("add_grid_container");

        for &child in children {
            self.validate_node(child)?;
        }

        let mut taffy_style = style.to_taffy();
        taffy_style.display = Display::Grid;
        // Inline the track mapping at each `collect` so the destination field's
        // type constrains `GridTemplateComponent`'s string generic (a shared
        // closure couldn't infer it).
        taffy_style.grid_template_columns = columns
            .iter()
            .map(|t| match t {
                GridTrack::Fr(f) => GridTemplateComponent::from_fr(*f),
                GridTrack::Px(p) => GridTemplateComponent::from_length(*p),
                GridTrack::Auto => GridTemplateComponent::AUTO,
            })
            .collect();
        taffy_style.grid_template_rows = rows
            .iter()
            .map(|t| match t {
                GridTrack::Fr(f) => GridTemplateComponent::from_fr(*f),
                GridTrack::Px(p) => GridTemplateComponent::from_length(*p),
                GridTrack::Auto => GridTemplateComponent::AUTO,
            })
            .collect();
        // Grid gaps are per-axis: `gap.width` is the column gap, `gap.height` the
        // row gap (Taffy/CSS convention).
        taffy_style.gap = Size {
            width: LengthPercentage::from_length(col_gap),
            height: LengthPercentage::from_length(row_gap),
        };

        let mut fp = LayoutFingerprint::new(NodeKind::Grid);
        fp.container(&style)
            .tracks(columns)
            .tracks(rows)
            .f32(col_gap)
            .f32(row_gap)
            .children(children);
        self.place(NodeKind::Grid, fp.finish(), taffy_style, children, None)
    }

    /// Places an already-created grid child explicitly (RFC-0018): sets its
    /// `grid_column`/`grid_row` from `placement`. A `None` start leaves that
    /// axis to auto-placement; the span (≥ 1) always applies. A no-op-equivalent
    /// placement (auto start, span 1) still resolves to the same auto flow.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::ForeignNode`] if `node` came from another atlas, or
    /// [`AtlasError::Backend`] if the backend rejects the restyle.
    pub fn set_grid_item(
        &mut self,
        node: AtlasNodeId,
        placement: GridItemPlacement,
    ) -> Result<(), AtlasError> {
        self.assert_building("set_grid_item");
        self.validate_node(node)?;

        // On a retained build the placement is re-applied every frame from the
        // same source data, and `set_style` marks the node dirty in Taffy
        // unconditionally, so re-applying an unchanged placement would
        // recompute every grid child on every frame and quietly turn the
        // retained path back into a full one. Skip when nothing moved *and*
        // this node's style was not rewritten out from under the placement
        // earlier in the same build.
        if self.retained.is_some()
            && self.grid_items.get(&node.node_id) == Some(&placement)
            && !self.restyled.contains(&node.node_id)
        {
            return Ok(());
        }

        let mut style = self
            .tree
            .style(node.node_id)
            .map_err(|e| AtlasError::from_taffy(&e))?
            .clone();
        // Assigned directly onto the `Line<GridPlacement<S>>` fields so the field
        // type constrains the string generic `S` (a shared helper couldn't infer
        // it). A `None` start stays auto; the span (≥ 1) always applies.
        let col_span = placement.col_span.max(1);
        style.grid_column = match placement.col_start {
            Some(s) => Line {
                start: GridPlacement::from_line_index(s),
                end: GridPlacement::from_span(col_span),
            },
            None => Line {
                start: GridPlacement::Auto,
                end: GridPlacement::from_span(col_span),
            },
        };
        let row_span = placement.row_span.max(1);
        style.grid_row = match placement.row_start {
            Some(s) => Line {
                start: GridPlacement::from_line_index(s),
                end: GridPlacement::from_span(row_span),
            },
            None => Line {
                start: GridPlacement::Auto,
                end: GridPlacement::from_span(row_span),
            },
        };
        self.tree
            .set_style(node.node_id, style)
            .map_err(|e| AtlasError::from_taffy(&e))?;
        let changed = self.grid_items.insert(node.node_id, placement) != Some(placement);
        if self.retained.is_some() && changed {
            if let Some(index) = self.nodes_by_index.iter().position(|n| *n == node) {
                self.mark_layout_dirty(index, node);
            }
        }
        Ok(())
    }

    /// Fixes `node`'s width in logical pixels **after** layout has run, and
    /// reports whether that changed anything (RFC-0036 `width: match(ref)`).
    ///
    /// The one post-`compute` style change this atlas allows, and it exists
    /// for one shape of problem: an element whose width is another element's
    /// resolved width. A dropdown as wide as the field it hangs from cannot be
    /// expressed as a layout relationship, because the two are in different
    /// trees, and it cannot be applied by moving the finished rect either,
    /// because the dropdown's own children were laid out against the width it
    /// had.
    ///
    /// **This is not a cycle**, which is the thing worth checking before
    /// reaching for it. The dependency runs one way: the main tree resolves,
    /// the anchor's rect is a fact, and only then is a subtree that is not
    /// part of the main tree's layout given a width. The overlay cannot
    /// influence the anchor, so no amount of iterating would change either
    /// answer. Feeding a *main-tree* rect back into the main tree's own layout
    /// remains forbidden and is a different thing entirely.
    ///
    /// Returns `false` when the width is already exactly this, which is the
    /// common case on a steady frame: `set_style` marks the node dirty in
    /// Taffy unconditionally, so re-applying an unchanged width every frame
    /// would recompute the subtree every frame and turn the retained path back
    /// into a full one for anything with a dropdown on screen.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::ForeignNode`] if `node` came from another atlas,
    /// or [`AtlasError::Backend`] if the backend refuses the style.
    pub fn set_fixed_width(&mut self, node: AtlasNodeId, width: f32) -> Result<bool, AtlasError> {
        self.validate_node(node)?;
        let mut style = self
            .tree
            .style(node.node_id)
            .map_err(|e| AtlasError::from_taffy(&e))?
            .clone();
        let wanted = Dimension::from_length(width);
        if style.size.width == wanted {
            return Ok(false);
        }
        style.size.width = wanted;
        self.tree
            .set_style(node.node_id, style)
            .map_err(|e| AtlasError::from_taffy(&e))?;
        Ok(true)
    }

    /// Adds a **stacking** container (RFC-0018 `ZStack`): a single-cell CSS grid
    /// in which every child is placed in the same cell, so they overlap. The
    /// lone `auto` track sizes the stack to its largest child, and `align`/`justify` position smaller children within it
    /// (default centred). Children paint in declaration order (last on top) via
    /// the ordinary container paint path.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if the backend refuses the node, or
    /// [`AtlasError::ForeignNode`] if any child came from another atlas.
    pub fn add_stack_container(
        &mut self,
        style: ContainerStyle,
        align: StackAlign,
        children: &[AtlasNodeId],
    ) -> Result<AtlasNodeId, AtlasError> {
        self.assert_building("add_stack_container");
        for &child in children {
            self.validate_node(child)?;
        }

        let (justify, align_items) = align.to_items();
        let mut taffy_style = style.to_taffy();
        taffy_style.display = Display::Grid;
        // A single auto column and row: the cell sizes to the largest child.
        taffy_style.grid_template_columns = core::iter::once(GridTemplateComponent::AUTO).collect();
        taffy_style.grid_template_rows = core::iter::once(GridTemplateComponent::AUTO).collect();
        // Position children within the cell rather than stretching them to fill
        // it (the ZStack keeps each child at its natural size).
        taffy_style.justify_items = Some(justify);
        taffy_style.align_items = Some(align_items);

        let mut fp = LayoutFingerprint::new(NodeKind::Stack);
        fp.container(&style).u8(align as u8).children(children);
        let id = self.place(NodeKind::Stack, fp.finish(), taffy_style, children, None)?;

        // Pin every child to the same cell (line 1, span 1 on both axes) so they
        // overlap, otherwise grid auto-placement would flow them into implicit
        // rows and stack them vertically instead.
        let cell = GridItemPlacement {
            col_start: Some(1),
            col_span: 1,
            row_start: Some(1),
            row_span: 1,
        };
        for &child in children {
            self.set_grid_item(child, cell)?;
        }
        Ok(id)
    }

    /// Sets the root node for layout computation.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::ForeignNode`] if `root` was created by a
    /// different `LayoutAtlas` instance.
    pub fn set_root(&mut self, root: AtlasNodeId) -> Result<(), AtlasError> {
        self.assert_building("set_root");
        self.validate_node(root)?;
        self.root = Some(root);
        Ok(())
    }

    /// Commits an [`AtlasNodeSpec`] tree built via [`LayoutAtlasBuilder`].
    ///
    /// Walks `spec` depth-first, building every child before its parent,
    /// the exact same order and resulting [`AtlasNodeId`]s a hand-written
    /// call sequence of [`Self::add_leaf`] / [`Self::add_container`] would
    /// produce. Does not set the result as the root; use
    /// [`Self::build_root`] for that, or call [`Self::set_root`] yourself
    /// on the returned id.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state (same contract as
    /// [`Self::add_leaf`] / [`Self::add_container`]).
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if the underlying engine refuses a
    /// node. [`AtlasError::ForeignNode`] cannot occur here, every
    /// `AtlasNodeId` `build` consumes is one it just created itself while
    /// walking `spec`, never one supplied by the caller.
    pub fn build(&mut self, spec: AtlasNodeSpec) -> Result<AtlasNodeId, AtlasError> {
        match spec {
            AtlasNodeSpec::Leaf(size) => self.add_leaf(size),
            AtlasNodeSpec::Container(style, children) => {
                let mut built = Vec::with_capacity(children.len());
                for child in children {
                    built.push(self.build(child)?);
                }
                self.add_container(style, &built)
            }
        }
    }

    /// Like [`Self::build`], but also installs the result as the root via
    /// [`Self::set_root`], the common case when `spec` describes a whole
    /// view rather than a fragment to be attached elsewhere.
    ///
    /// # Panics
    ///
    /// See [`Self::build`].
    ///
    /// # Errors
    ///
    /// See [`Self::build`].
    pub fn build_root(&mut self, spec: AtlasNodeSpec) -> Result<AtlasNodeId, AtlasError> {
        let root = self.build(spec)?;
        self.set_root(root)?;
        Ok(root)
    }

    /// Computes layout against the given viewport size.
    ///
    /// Transitions the atlas from `Building` to `Computed`. After this
    /// call, [`Self::resolved_rect`] returns geometry; modifying nodes
    /// panics until [`Self::clear`] is called.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Computed` state, or if no root has
    /// been set via [`Self::set_root`].
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if layout computation fails.
    pub fn compute(&mut self, viewport: Viewport) -> Result<(), AtlasError> {
        self.compute_inner(viewport, None)
    }

    /// Like [`compute`](Self::compute), but drives wrapping-`Text` leaves through
    /// `sizer` so they reflow to the width their parent offers (RFC-0005 default
    /// wrap). The interpreter passes its shared [`TextMeasurer`] here; the cache
    /// on the measurer means an unchanged string re-shapes nothing across ticks.
    ///
    /// [`TextMeasurer`]: crate::text::TextMeasurer
    ///
    /// # Panics
    ///
    /// Panics if the atlas is not in the `Building` state, or has no root.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if layout computation fails.
    pub fn compute_with_text(
        &mut self,
        viewport: Viewport,
        sizer: &mut dyn crate::text::TextSizer,
    ) -> Result<(), AtlasError> {
        self.compute_inner(viewport, Some(sizer))
    }

    fn compute_inner(
        &mut self,
        viewport: Viewport,
        sizer: Option<&mut dyn crate::text::TextSizer>,
    ) -> Result<(), AtlasError> {
        self.assert_building("compute");

        let root = self
            .root
            .expect("LayoutAtlas::compute called without a root node, call set_root first");

        let available = Size {
            width: AvailableSpace::Definite(viewport.width),
            height: AvailableSpace::Definite(viewport.height),
        };

        self.run_layout(root.node_id, available, sizer)?;
        path_counters::record_full_compute();
        self.state = AtlasState::Computed;
        self.rebuild_grid();
        Ok(())
    }

    /// Runs Taffy layout, using the measure protocol only when there are
    /// wrapping-`Text` leaves (the common no-text tree keeps the cheaper
    /// `compute_layout` fast path). Shared by the initial and incremental passes.
    fn run_layout(
        &mut self,
        root: NodeId,
        available: Size<AvailableSpace>,
        sizer: Option<&mut dyn crate::text::TextSizer>,
    ) -> Result<(), AtlasError> {
        // RFC-0030 §I1: the one scope that isolates Taffy's cost from the
        // interpreter work around it. Declared here rather than at the caller
        // so both the full (`compute`/`compute_with_text`) and the
        // incremental (`recompute_dirty`) paths are measured by construction
        //, a future call site cannot forget to wrap it, and the two paths'
        // costs are directly comparable because they carry the same label.
        crate::profile_scope!("layout.taffy");
        if self.text_specs.is_empty() {
            return self
                .tree
                .compute_layout(root, available)
                .map_err(|e| AtlasError::from_taffy(&e));
        }
        // Split the borrow so the measure closure can read `text_specs` while
        // Taffy holds `tree` mutably. The sizer is captured as a plain `&mut dyn`
        // (reborrowed per call), not an `Option<&mut>`, whose invariance would
        // make the reborrow escape the `FnMut` body.
        let tree = &mut self.tree;
        let specs = &self.text_specs;
        match sizer {
            Some(sizer) => tree
                .compute_layout_with_measure(
                    root,
                    available,
                    |known, avail, node_id, _ctx, _style| {
                        measure_text_node(specs, Some(&mut *sizer), node_id, known, avail)
                    },
                )
                .map_err(|e| AtlasError::from_taffy(&e)),
            None => tree
                .compute_layout_with_measure(
                    root,
                    available,
                    |known, avail, node_id, _ctx, _style| {
                        measure_text_node(specs, None, node_id, known, avail)
                    },
                )
                .map_err(|e| AtlasError::from_taffy(&e)),
        }
    }

    /// Returns the resolved rectangle for `node`.
    ///
    /// # Caveat: orphan nodes
    ///
    /// If `node` was added to the tree but never attached (directly or
    /// transitively) to the root configured via [`Self::set_root`], Taffy
    /// still resolves a default `Rect` of all zeros for it rather than
    /// failing, this returns `Ok(Some(zero_rect))`, not `Ok(None)`. See
    /// [`Self::resolved_rect_internal`] for the raw Taffy behaviour this
    /// wraps. Callers should only query rects for nodes known to be
    /// reachable from the root.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Building` state. Call
    /// [`Self::compute`] first.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::ForeignNode`] if `node` was created by a
    /// different `LayoutAtlas` instance.
    pub fn resolved_rect(&self, node: AtlasNodeId) -> Result<Option<Rect>, AtlasError> {
        assert_eq!(
            self.state,
            AtlasState::Computed,
            "LayoutAtlas::resolved_rect called before compute, geometry is not available yet"
        );
        self.validate_node(node)?;

        Ok(self.resolved_rect_internal(node))
    }

    /// The `(width, height)` of a node's **content**, the extent of its
    /// children, which for a `ScrollView` (Taffy `overflow: Scroll`) exceeds the
    /// node's own box. Subtracting the viewport size gives the maximum scroll
    /// distance (RFC-0005). `None` if the node is unknown or not yet computed.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::ForeignNode`] if `node` belongs to another atlas.
    pub fn content_size(&self, node: AtlasNodeId) -> Result<Option<(f32, f32)>, AtlasError> {
        self.validate_node(node)?;
        if self.state != AtlasState::Computed {
            return Ok(None);
        }
        Ok(self
            .tree
            .layout(node.node_id)
            .ok()
            .map(|l| (l.content_size.width, l.content_size.height)))
    }

    /// Writes the resolved geometry of every node into `frame`, marking each
    /// entry dirty if its `TargetId` appears in `dirty_targets`.
    ///
    /// Walks the tree from the root in pre-order and appends each node's
    /// resolved [`Rect`] to the frame. This is how the Atlas hands geometry
    /// to the Encoder without either subsystem importing from the other,
    /// the frame is the shared boundary defined in RFC-0001 §9.
    ///
    /// `dirty_targets` is intended to be the output of
    /// [`EvaluatorTick::collect_dirty`](crate::evaluator::EvaluatorTick::collect_dirty)
    /// for this tick. Each node's own `TargetId` (kind, generation, index)
    /// is reconstructed using the same scheme as [`Self::rebuild_grid`] and
    /// [`Self::mark_dirty_all`], so stale-generation targets are excluded
    /// for free, they simply will not match any live node's id.
    ///
    /// # What the interpreter passes
    ///
    /// The set of nodes whose **layout inputs** changed this frame, via
    /// [`Self::populate_frame_dirty`] (RFC-0032 §R3 step 5). On a full rebuild
    /// that is every node, because every node is new; on a retained frame it
    /// is typically a handful, and on a paint-only frame, a recolour, an
    /// opacity fade, it is correctly **empty**: a colour is not a layout
    /// input, and saying otherwise here would recompute a tree that did not
    /// move.
    ///
    /// This parameter was inert for several phases, called with `&[]` from the
    /// one production call site, because the interpreter had no per-element
    /// change signal to pass. It has one now; if this ever starts receiving
    /// `&[]` again on a frame that changed something, the assertions in
    /// `byard-compiler`'s `tests/incremental_paths.rs` are what will say so.
    ///
    /// The frame is **not** cleared before pushing; callers that want a
    /// fresh frame must call [`RenderFrame::clear`] first. This lets the
    /// orchestrator batch contributions from multiple subsystems into the
    /// same frame.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Building` state. Call [`Self::compute`]
    /// first.
    #[track_caller]
    pub fn populate_frame(&self, frame: &mut RenderFrame, dirty_targets: &[TargetId]) {
        assert_eq!(
            self.state,
            AtlasState::Computed,
            "LayoutAtlas::populate_frame called before compute, geometry is not available yet"
        );

        let root = self.root.expect(
            "LayoutAtlas::populate_frame reached Computed state without a root node, \
         this indicates an internal state-machine inconsistency",
        );

        let dirty: HashSet<u64> = dirty_targets.iter().map(|t| t.as_raw()).collect();

        let matched = self.walk_and_push(root, frame, &dirty);
        path_counters::record_populate(dirty_targets.len(), matched);
    }

    /// [`populate_frame`](Self::populate_frame) driven by this atlas's own
    /// layout-dirty set (RFC-0032 §R3 step 5).
    ///
    /// `retained` says which set that is. After a **retained** build it is the
    /// nodes whose layout inputs moved, typically a handful. After a **full
    /// rebuild** every node is new, so every rect is reported dirty; that is
    /// not a fallback, it is the truth about a rebuilt frame, and reporting
    /// anything narrower would be the stale-rect hazard wearing a performance
    /// costume.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Building` state.
    pub fn populate_frame_dirty(&mut self, frame: &mut RenderFrame, retained: bool) {
        if !retained {
            self.layout_dirty.clear();
            self.layout_dirty.reserve(self.nodes_by_index.len());
            for index in 0..self.nodes_by_index.len() {
                #[allow(clippy::cast_possible_truncation)]
                let raw = index as u32;
                self.layout_dirty.push(TargetId::new(
                    raw,
                    self.current_generation,
                    TargetKind::AtlasNode as u16,
                ));
            }
        }
        // Split the borrow: `populate_frame` only reads, but the compiler
        // cannot see that through `&mut self`, so the set moves out and back.
        let dirty = std::mem::take(&mut self.layout_dirty);
        self.populate_frame(frame, &dirty);
        self.layout_dirty = dirty;
    }

    /// Performs spatial hit-testing to find the topmost node at the given screen coordinates.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Building` state.
    #[must_use]
    pub fn hit_test(&self, x: f32, y: f32) -> Option<AtlasNodeId> {
        assert_eq!(
            self.state,
            AtlasState::Computed,
            "LayoutAtlas::hit_test called while in Building state, layout must be computed first"
        );

        if let Some(target) = self.grid.query(x, y) {
            if target.generation() == self.current_generation
                && (target.index() as usize) < self.nodes_by_index.len()
            {
                return Some(self.nodes_by_index[target.index() as usize]);
            }
        }
        None
    }

    /// Returns the context index of the given node, if it belongs to this atlas.
    #[must_use]
    pub fn node_index(&self, node: AtlasNodeId) -> Option<u32> {
        if node.atlas_id == self.instance_id {
            self.tree.get_node_context(node.node_id).copied()
        } else {
            None
        }
    }

    /// Returns the parent node of the given node, if it belongs to this atlas and has a parent.
    #[must_use]
    pub fn parent_node(&self, node: AtlasNodeId) -> Option<AtlasNodeId> {
        if node.atlas_id == self.instance_id {
            let parent_id = self.parents.get(&node.node_id)?;
            Some(AtlasNodeId {
                node_id: *parent_id,
                atlas_id: self.instance_id,
            })
        } else {
            None
        }
    }

    /// The direct child nodes of `node`, in layout order, if it belongs to this
    /// atlas. Empty for a leaf or a node from another atlas. RFC-0021 `snap: item`
    /// reads each item's boundary by pairing this with
    /// [`resolved_rect`](Self::resolved_rect).
    #[must_use]
    pub fn children(&self, node: AtlasNodeId) -> Vec<AtlasNodeId> {
        if node.atlas_id != self.instance_id {
            return Vec::new();
        }
        self.tree.children(node.node_id).map_or_else(
            |_| Vec::new(),
            |ids| {
                ids.into_iter()
                    .map(|node_id| AtlasNodeId {
                        node_id,
                        atlas_id: self.instance_id,
                    })
                    .collect()
            },
        )
    }

    /// Recursively walks the tree from `node` in pre-order, pushing each
    /// resolved rectangle, and its dirty state, into `frame`.
    ///
    /// `dirty` holds the raw (`TargetId::as_raw`) bits of every dirty
    /// target for this tick. Each node's own `TargetId` is reconstructed
    /// from its Taffy node-context index, the atlas's current generation,
    /// and `TargetKind::AtlasNode`, the same triple `rebuild_grid` uses,
    /// so the lookup is exact and stale generations never match.
    ///
    /// Returns how many pushed nodes matched a target in `dirty`, the
    /// quantity that distinguishes "the caller passed no dirty set" from "the
    /// caller passed a dirty set from a generation this atlas has already
    /// invalidated". Both produce an all-`false` frame; only one is a bug in
    /// the caller.
    fn walk_and_push(
        &self,
        root: AtlasNodeId,
        frame: &mut RenderFrame,
        dirty: &HashSet<u64>,
    ) -> usize {
        let mut matched = 0;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if let Some(rect) = self.resolved_rect_internal(node) {
                let index = *self.tree.get_node_context(node.node_id).unwrap();
                let target =
                    TargetId::new(index, self.current_generation, TargetKind::AtlasNode as u16);
                let is_dirty = dirty.contains(&target.as_raw());
                matched += usize::from(is_dirty);
                frame.push_rect(rect, is_dirty);
            }
            if let Ok(children) = self.tree.children(node.node_id) {
                // Push in reverse so the leftmost child is popped first,
                // preserving pre-order traversal semantics.
                for child in children.iter().rev() {
                    stack.push(AtlasNodeId {
                        node_id: *child,
                        atlas_id: self.instance_id,
                    });
                }
            }
        }
        matched
    }

    /// Rebuilds the hit-testing spatial grid from the current layout.
    ///
    /// This is a full `clear()` + root-to-leaf walk on every
    /// `compute`/`recompute_dirty`, regardless of how many nodes were marked
    /// dirty. That was measured (M28) on a 200-leaf tree (the high end
    /// of `EvaluatorTick`'s expected per-tick target count): the whole
    /// `recompute_dirty`, layout + this grid rebuild, costs ~24 µs with one
    /// dirty leaf and ~111 µs with every node dirty, i.e. ≲0.7% of a 60 Hz
    /// frame even in the worst case. A partial grid update would have to track
    /// nodes whose rect shifted only as a *side effect* of a sibling's flex
    /// reflow, risking dangling (stale-but-queryable) hit rects, a correctness
    /// hazard strictly worse than a redundant walk this cheap. The full walk is
    /// therefore kept deliberately; see the `atlas` bench for the numbers.
    fn rebuild_grid(&mut self) {
        self.grid.clear();
        let Some(root) = self.root else {
            return;
        };

        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let index = *self.tree.get_node_context(node.node_id).unwrap();
            let target =
                TargetId::new(index, self.current_generation, TargetKind::AtlasNode as u16);
            if let Some(rect) = self.resolved_rect_internal(node) {
                self.grid.insert(rect, target);
            }
            if let Ok(children) = self.tree.children(node.node_id) {
                // Push in reverse so the leftmost child is popped first,
                // preserving pre-order traversal semantics.
                for child in children.iter().rev() {
                    stack.push(AtlasNodeId {
                        node_id: *child,
                        atlas_id: self.instance_id,
                    });
                }
            }
        }
    }

    /// Same as `resolved_rect` but without the state assertion or the
    /// cross-atlas check, used internally during traversal, where every
    /// `node` is already known to belong to this atlas (it was fetched
    /// from `self.tree` or `self.root`, never supplied by an external
    /// caller).
    fn resolved_rect_internal(&self, node: AtlasNodeId) -> Option<Rect> {
        let layout = self.tree.layout(node.node_id).ok()?;
        let mut x = layout.location.x;
        let mut y = layout.location.y;

        let mut current = node.node_id;
        while let Some(&parent) = self.parents.get(&current) {
            if let Ok(p_layout) = self.tree.layout(parent) {
                x += p_layout.location.x;
                y += p_layout.location.y;
                current = parent;
            } else {
                break;
            }
        }

        Some(Rect {
            x,
            y,
            width: layout.size.width,
            height: layout.size.height,
        })
    }

    /// Returns the current root node, if any.
    #[must_use]
    pub fn root(&self) -> Option<AtlasNodeId> {
        self.root
    }

    /// Returns the number of nodes currently in the tree.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.tree.total_node_count()
    }

    #[track_caller]
    fn assert_building(&self, method: &str) {
        assert_eq!(
            self.state,
            AtlasState::Building,
            "LayoutAtlas::{method} called while in Computed state, call clear() first"
        );
    }

    /// Returns the index that the next added node will receive.
    ///
    /// Useful when constructing a [`TargetId`] before the node is created
    /// (e.g. when registering a `Signal` that points to a yet-to-be-built
    /// layout target).
    ///
    /// # Truncation
    ///
    /// The returned value is `u32` to match the [`TargetId`] bit layout.
    /// In the theoretical case of an atlas containing more than `u32::MAX`
    /// (≈ 4.3 billion) nodes, the cast truncates and subsequent `TargetId`s
    /// will alias earlier ones. A `debug_assert!` catches this in debug
    /// builds; in release the bug would surface as ghost dirty marks on
    /// the wrong nodes.
    ///
    /// In practice this limit is unreachable, 4 billion nodes at ~100
    /// bytes each would require ~400 GB of RAM for the tree alone.
    #[must_use]
    pub fn next_target_index(&self) -> u32 {
        let len = self.nodes_by_index.len();
        debug_assert!(
            u32::try_from(len).is_ok(),
            "LayoutAtlas exceeded u32::MAX nodes, TargetId indexing will alias",
        );
        #[allow(clippy::cast_possible_truncation)]
        {
            len as u32
        }
    }

    /// Returns the current view generation.
    ///
    /// Embed this into [`TargetId::new`] when registering a `Signal`
    /// against an Atlas node; future `mark_dirty_all` calls will then
    /// validate it against the Atlas's current generation.
    #[must_use]
    pub fn current_generation(&self) -> u16 {
        self.current_generation
    }

    /// Marks every target in `targets` that belongs to this atlas as dirty.
    ///
    /// Targets are filtered by [`TargetKind::AtlasNode`] and by matching
    /// generation, so callers can safely pass the full batch produced by
    /// [`EvaluatorTick::collect_dirty`](crate::evaluator::EvaluatorTick::collect_dirty).
    /// Foreign or stale targets are silently ignored, this is the
    /// broadcast/event-bus pattern documented in RFC-0001 §4.1.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Building` state. Call [`Self::compute`]
    /// first.
    pub fn mark_dirty_all(&mut self, targets: &[TargetId]) {
        assert_eq!(
            self.state,
            AtlasState::Computed,
            "LayoutAtlas::mark_dirty_all called before compute, \
         no geometry exists to mark dirty yet"
        );

        for target in targets {
            if target.kind() != TargetKind::AtlasNode as u16 {
                continue;
            }
            if target.generation() != self.current_generation {
                continue;
            }
            let index = target.index() as usize;
            if let Some(&node) = self.nodes_by_index.get(index) {
                // If Taffy refuses (very rare, would indicate the node
                // was somehow removed from the tree), skip silently. The
                // next recompute will produce a layout that reflects the
                // tree as it actually is.
                let _ = self.tree.mark_dirty(node.node_id);
            }
        }
    }

    /// Recomputes layout for the subtrees marked dirty since the last
    /// `compute` or `recompute_dirty`, **without a text sizer**.
    ///
    /// # No production path may call this
    ///
    /// It runs the measure protocol with no sizer, so every wrapping `Text`
    /// leaf Taffy happens to recompute falls back to its natural *single-line*
    /// size, silently un-wrapping paragraphs on the frame after any retained
    /// one. Reach for [`Self::recompute_dirty_with_text`] instead; this
    /// variant exists for the benchmarks and for layout-only unit tests, where
    /// there is no text to un-wrap and no sizer to pass.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Building` state, or if no root has
    /// been set.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if layout computation fails.
    pub fn recompute_dirty(&mut self, viewport: Viewport) -> Result<(), AtlasError> {
        self.recompute_dirty_inner(viewport, None)
    }

    /// Like [`recompute_dirty`](Self::recompute_dirty), but drives wrapping
    /// `Text` leaves through `sizer`, the incremental counterpart of
    /// [`compute_with_text`](Self::compute_with_text), and the **only** form a
    /// production path may use (RFC-0032 §R5).
    ///
    /// The measure callback is invoked by Taffy only for the nodes it is
    /// actually recomputing, so a text leaf whose content, size and offered
    /// width are unchanged is never re-shaped. That is the principal win of
    /// the retained path, the encode breakdown put glyph work at 84–98 % of
    /// the frame's encode cost, and it falls out of Taffy's own dirty
    /// propagation rather than from anything here having to be careful.
    ///
    /// # Panics
    ///
    /// Panics if the atlas is in the `Building` state, or if no root has been
    /// set.
    ///
    /// # Errors
    ///
    /// Returns [`AtlasError::Backend`] if layout computation fails.
    pub fn recompute_dirty_with_text(
        &mut self,
        viewport: Viewport,
        sizer: &mut dyn crate::text::TextSizer,
    ) -> Result<(), AtlasError> {
        self.recompute_dirty_inner(viewport, Some(sizer))
    }

    fn recompute_dirty_inner(
        &mut self,
        viewport: Viewport,
        sizer: Option<&mut dyn crate::text::TextSizer>,
    ) -> Result<(), AtlasError> {
        assert_eq!(
            self.state,
            AtlasState::Computed,
            "LayoutAtlas::recompute_dirty called before compute, \
         the initial layout pass must run via compute() first"
        );

        let root = self.root.expect(
            "LayoutAtlas::recompute_dirty reached Computed state without a root node, \
         this indicates an internal state-machine inconsistency",
        );

        let available = Size {
            width: AvailableSpace::Definite(viewport.width),
            height: AvailableSpace::Definite(viewport.height),
        };

        self.run_layout(root.node_id, available, sizer)?;
        path_counters::record_retained_recompute();
        // Unconditionally rebuilt from the **resolved rects**, never from the
        // fingerprints (RFC-0032 §R3 step 4, INV-23). A node that moved only
        // because a sibling resized was never marked by anyone here, and the
        // full walk is what guarantees its grid entry cannot be stale, which
        // is the difference between a wrong pixel and an element that is
        // tappable where it used to be.
        self.rebuild_grid();
        Ok(())
    }
}

/// The Taffy measure callback for one leaf (RFC-0005 default wrap): a wrapping
/// `Text` leaf shapes itself to the offered width, everything else reports its
/// already-known size. `known.width` (a fixed/stretched width) wins; otherwise a
/// `Definite` available width is the wrap target, `MaxContent` is the natural
/// single line, and `MinContent` wraps as narrow as possible.
fn measure_text_node(
    specs: &HashMap<NodeId, TextLeaf>,
    sizer: Option<&mut dyn crate::text::TextSizer>,
    node_id: NodeId,
    known: Size<Option<f32>>,
    avail: Size<AvailableSpace>,
) -> Size<f32> {
    let Some(spec) = specs.get(&node_id) else {
        return Size {
            width: known.width.unwrap_or(0.0),
            height: known.height.unwrap_or(0.0),
        };
    };
    let wrap_w = known.width.or(match avail.width {
        AvailableSpace::Definite(w) => Some(w),
        AvailableSpace::MaxContent => None,
        AvailableSpace::MinContent => Some(0.0),
    });
    let (width, height) = match sizer {
        Some(s) => s.measure(
            &spec.content,
            spec.font_size,
            wrap_w,
            spec.weight,
            spec.family.as_deref(),
        ),
        None => spec.fallback,
    };
    // Reserve a **whole pixel** of width for the glyphs. Taffy rounds resolved
    // layout to integer coordinates (rounding on by default), so a node measured
    // at e.g. 100.4px would resolve to width 100, and the render pass, which
    // re-shapes the run to that resolved width, would then wrap a line that
    // actually fits. Ceiling here makes the reserved width an integer ≥ the
    // glyph extent; an integer width survives taffy's position rounding exactly
    // (`round(x + n) == round(x) + n`), so the painted run never re-wraps from a
    // sub-pixel deficit. Height is left untouched (its own line-height rounding
    // is handled by the vertical layout).
    Size {
        width: width.ceil(),
        height,
    }
}

impl Default for LayoutAtlas {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;

/// RFC-0032 §R2 / §R4: the layout fingerprints and the retained build path.
///
/// These are the mechanism's own tests, with nothing consuming them,
/// `byard-compiler`'s `tests/incremental_paths.rs` covers what production does
/// with the result.
#[cfg(test)]
mod retained_build_tests;
