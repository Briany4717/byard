//! # Surfaces (RFC-0068)
//!
//! What a program draws into, and where a compositor would put it.
//!
//! - [`id`]: [`SurfaceId`], [`SurfaceSpec`], [`SurfaceChange`] and [`PerSurface`],
//!   the small types the engine and the relay use to tell surfaces apart
//!   (§5, §6).
//!
//! The rest of this module is the two pure solvers that headless screens rely on (§9). A compositor decides
//! where a layer surface and a popup go; `byard shot` has no compositor, so it
//! asks these instead. Both are plain functions over plain data: no GPU, no
//! Wayland connection, no `unsafe` and nothing that is platform specific, which
//! is why they are the one part of the shell backend that is tested on every
//! platform in CI.
//!
//! - [`placement`] arranges the layer surfaces of one screen (anchors, margins,
//!   exclusive zones, in the order wlroots and sway do) and says what area is
//!   left for windows.
//! - [`positioner`] places one popup against an anchor rectangle and keeps it
//!   inside a constraint area, by the flip, slide and resize rules of
//!   `xdg_positioner`.
//!
//! Everything here is in whole logical pixels, because both protocols are
//! (`int32` on the wire). Nothing uses these yet: step 5 of RFC-0068 wires them
//! into `byard shot`.

pub mod id;
pub mod panel;
pub mod placement;
pub mod positioner;

pub use id::{PerSurface, SurfaceChange, SurfaceId, SurfaceKind, SurfaceSpec};
pub use panel::{Exclusive, InputMode, InputRegion, KeyboardMode, LayerSpec, ScreenInfo};

/// A rectangle in whole logical pixels, with `y` growing downwards.
///
/// The same shape as wlroots' `struct wlr_box`, and the same arithmetic: all
/// division is integer division that truncates toward zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width. Negative only in a malformed input.
    pub width: i32,
    /// Height. Negative only in a malformed input.
    pub height: i32,
}

impl Rect {
    /// A rectangle from its position and size.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The x coordinate just past the right edge.
    #[must_use]
    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    /// The y coordinate just past the bottom edge.
    #[must_use]
    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// Whether it covers no pixel (`wlr_box_empty`: a width or height of zero
    /// or less).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// The size, without the position.
    #[must_use]
    pub const fn size(&self) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }
}

/// A width and a height in whole logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

impl Size {
    /// A size from its width and height.
    #[must_use]
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }
}

/// One axis of a surface, named in an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// The horizontal axis (width).
    Horizontal,
    /// The vertical axis (height).
    Vertical,
}
