//! The layer-shell placement solver: where each panel of a screen goes, and
//! what is left for windows.
//!
//! This is the arithmetic a compositor runs when it arranges `zwlr_layer_surface_v1`
//! surfaces, taken from wlroots and sway and not from the protocol's prose, so
//! that a headless shot lays a bar out where the desktop would:
//!
//! - [`arrange`] is sway's `arrange_layers` (`sway/desktop/layer_shell.c`): the
//!   layers are visited from `overlay` down to `background`, surfaces with an
//!   exclusive zone first (all four layers), then the rest, each surface in
//!   creation order within its layer. Each surface goes through
//!   `LayerSurface::place`, which is
//!   `wlr_scene_layer_surface_v1_configure` (`types/scene/layer_shell_v1.c`,
//!   the part named `wlr_scene_layer_surface_v1_get_box`).
//! - [`exclusive_edge`] is `wlr_layer_surface_v1_get_exclusive_edge`
//!   (`types/wlr_layer_shell_v1.c`), and the arithmetic that subtracts a zone
//!   from the usable area is `layer_surface_exclusive_zone`.
//!
//! Where wlroots and the protocol text differ the notes of the branch say which
//! was followed. In short: wlroots, except that a size of 0 on an axis that is
//! not anchored at both ends is reported as an error (the protocol's
//! `invalid_size`) where wlroots only rejects it at commit time, before any of
//! this runs.

use std::ops::BitOr;

use super::{Axis, Rect, Size};

/// Which layer a surface lives in, back to front.
///
/// Within the compositor's scene, `Background` is below everything and
/// `Overlay` is above everything, windows sitting between `Bottom` and `Top`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Layer {
    /// Wallpapers.
    Background,
    /// Below the windows, above the wallpaper: desktop widgets and docks.
    Bottom,
    /// Above the windows: bars and panels. The default for a `Panel`.
    #[default]
    Top,
    /// Above everything: launchers, lock prompts, on-screen displays.
    Overlay,
}

impl Layer {
    /// The order sway arranges the layers in (`arrange_layers`): front to
    /// back, so that an overlay's exclusive zone is subtracted before a top
    /// bar's.
    const ARRANGE_ORDER: [Layer; 4] =
        [Layer::Overlay, Layer::Top, Layer::Bottom, Layer::Background];
}

/// The set of screen edges a surface is anchored to
/// (`zwlr_layer_surface_v1.anchor`, a bitfield with the protocol's values).
///
/// Combine edges with `|`: `Anchors::TOP | Anchors::LEFT` is the top-left
/// corner, and the three edges `TOP | LEFT | RIGHT` are a bar across the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Anchors(u8);

impl Anchors {
    /// Anchored to no edge: the surface is centred on the screen.
    pub const NONE: Anchors = Anchors(0);
    /// Anchored to the top edge.
    pub const TOP: Anchors = Anchors(1);
    /// Anchored to the bottom edge.
    pub const BOTTOM: Anchors = Anchors(2);
    /// Anchored to the left edge.
    pub const LEFT: Anchors = Anchors(4);
    /// Anchored to the right edge.
    pub const RIGHT: Anchors = Anchors(8);

    /// Whether every edge of `other` is in this set.
    #[must_use]
    pub const fn contains(self, other: Anchors) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Anchors {
    type Output = Anchors;

    fn bitor(self, rhs: Anchors) -> Anchors {
        Anchors(self.0 | rhs.0)
    }
}

/// The distance a surface keeps from the edges it is anchored to
/// (`zwlr_layer_surface_v1.set_margin`). May be negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Margin {
    /// From the top edge.
    pub top: i32,
    /// From the right edge.
    pub right: i32,
    /// From the bottom edge.
    pub bottom: i32,
    /// From the left edge.
    pub left: i32,
}

impl Margin {
    /// The same distance on every side.
    #[must_use]
    pub const fn all(value: i32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }
}

/// A screen edge, as the exclusive zone of a surface refers to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    /// The top edge.
    Top,
    /// The bottom edge.
    Bottom,
    /// The left edge.
    Left,
    /// The right edge.
    Right,
}

/// What a client asks of the compositor for one layer surface: the state a
/// `zwlr_layer_surface_v1` holds after its commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerSurface {
    /// The layer it was created in.
    pub layer: Layer,
    /// The edges it is anchored to.
    pub anchors: Anchors,
    /// The width it asked for (`set_size`). 0 means "stretch", which is only
    /// valid when anchored to both the left and the right edge.
    pub width: i32,
    /// The height it asked for. 0 means "stretch", which is only valid when
    /// anchored to both the top and the bottom edge.
    pub height: i32,
    /// Its margins.
    pub margin: Margin,
    /// The exclusive zone (`set_exclusive_zone`). Positive: reserve this many
    /// pixels along the surface's edge (see [`exclusive_edge`]). 0: place the
    /// surface in the area the others left. -1: ignore the others' zones and
    /// use the whole screen.
    pub exclusive_zone: i32,
}

impl LayerSurface {
    /// A surface in `layer` with no anchors, no size, no margins and no zone.
    /// A starting point to fill in with struct update syntax.
    #[must_use]
    pub const fn new(layer: Layer) -> Self {
        Self {
            layer,
            anchors: Anchors::NONE,
            width: 0,
            height: 0,
            margin: Margin::all(0),
            exclusive_zone: 0,
        }
    }

    /// Places this one surface and, if it reserves space, shrinks `usable`.
    ///
    /// Mirrors `wlr_scene_layer_surface_v1_get_box` and, at its end,
    /// `layer_surface_exclusive_zone`.
    ///
    /// The surface is placed in the usable area, or in the whole screen
    /// (`full`) when its exclusive zone is -1. On each axis, in this order:
    ///
    /// 1. a requested size of 0 stretches: the surface starts at the margin and
    ///    takes what is left between the two margins;
    /// 2. anchored to both ends of the axis (with a size): centred, margins not
    ///    counted;
    /// 3. anchored to the start (left, top): at the start edge plus its margin;
    /// 4. anchored to the end (right, bottom): at the end edge minus its size
    ///    and margin;
    /// 5. anchored to neither: centred.
    ///
    /// Centring uses integer halves, so an odd leftover lands the extra pixel
    /// after the surface, as wlroots does.
    fn place(&self, full: Rect, usable: &mut Rect) -> Rect {
        let bounds = if self.exclusive_zone == -1 {
            full
        } else {
            *usable
        };
        let a = self.anchors;
        let m = self.margin;
        let mut rect = Rect {
            width: self.width,
            height: self.height,
            ..Rect::default()
        };

        if rect.width == 0 {
            rect.x = bounds.x + m.left;
            rect.width = bounds.width - (m.left + m.right);
        } else if a.contains(Anchors::LEFT) && a.contains(Anchors::RIGHT) {
            rect.x = bounds.x + bounds.width / 2 - rect.width / 2;
        } else if a.contains(Anchors::LEFT) {
            rect.x = bounds.x + m.left;
        } else if a.contains(Anchors::RIGHT) {
            rect.x = bounds.x + bounds.width - rect.width - m.right;
        } else {
            rect.x = bounds.x + bounds.width / 2 - rect.width / 2;
        }

        if rect.height == 0 {
            rect.y = bounds.y + m.top;
            rect.height = bounds.height - (m.top + m.bottom);
        } else if a.contains(Anchors::TOP) && a.contains(Anchors::BOTTOM) {
            rect.y = bounds.y + bounds.height / 2 - rect.height / 2;
        } else if a.contains(Anchors::TOP) {
            rect.y = bounds.y + m.top;
        } else if a.contains(Anchors::BOTTOM) {
            rect.y = bounds.y + bounds.height - rect.height - m.bottom;
        } else {
            rect.y = bounds.y + bounds.height / 2 - rect.height / 2;
        }

        if self.exclusive_zone > 0 {
            if let Some(edge) = exclusive_edge(self.anchors, self.exclusive_zone) {
                reserve(edge, self.exclusive_zone, m, usable);
            }
        }
        rect
    }
}

/// The edge a positive exclusive zone counts from, or `None` when the protocol
/// ignores the zone.
///
/// Mirrors `wlr_layer_surface_v1_get_exclusive_edge`: the zone applies only to
/// a surface anchored to exactly one edge, or to one edge and both
/// perpendicular ones. Anything else (a corner, two opposite edges, all four,
/// none) reserves nothing, whatever zone it asks for. A zone of 0 or less has
/// no edge.
///
/// The `exclusive_edge` request of layer-shell version 5, which names the edge
/// of a corner-anchored surface, is not modelled: a `Panel` writes `edge`, and
/// that is always one of the two shapes above.
#[must_use]
pub fn exclusive_edge(anchors: Anchors, exclusive_zone: i32) -> Option<Edge> {
    const TLR: Anchors = Anchors(Anchors::TOP.0 | Anchors::LEFT.0 | Anchors::RIGHT.0);
    const BLR: Anchors = Anchors(Anchors::BOTTOM.0 | Anchors::LEFT.0 | Anchors::RIGHT.0);
    const TBL: Anchors = Anchors(Anchors::TOP.0 | Anchors::BOTTOM.0 | Anchors::LEFT.0);
    const TBR: Anchors = Anchors(Anchors::TOP.0 | Anchors::BOTTOM.0 | Anchors::RIGHT.0);
    if exclusive_zone <= 0 {
        return None;
    }
    match anchors {
        a if a == Anchors::TOP || a == TLR => Some(Edge::Top),
        a if a == Anchors::BOTTOM || a == BLR => Some(Edge::Bottom),
        a if a == Anchors::LEFT || a == TBL => Some(Edge::Left),
        a if a == Anchors::RIGHT || a == TBR => Some(Edge::Right),
        _ => None,
    }
}

/// Takes a surface's zone, plus its margin on that edge, off the usable area
/// (`layer_surface_exclusive_zone`). The area never goes negative.
fn reserve(edge: Edge, zone: i32, margin: Margin, usable: &mut Rect) {
    match edge {
        Edge::Top => {
            usable.y += zone + margin.top;
            usable.height -= zone + margin.top;
        }
        Edge::Bottom => usable.height -= zone + margin.bottom,
        Edge::Left => {
            usable.x += zone + margin.left;
            usable.width -= zone + margin.left;
        }
        Edge::Right => usable.width -= zone + margin.right,
    }
    usable.width = usable.width.max(0);
    usable.height = usable.height.max(0);
}

/// Where one surface ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedSurface {
    /// Its rectangle, in the screen's coordinates (the origin is the screen's
    /// top-left corner).
    pub rect: Rect,
    /// The size the compositor sends in `zwlr_layer_surface_v1.configure`
    /// (`wlr_layer_surface_v1_configure(surface, box.width, box.height)`). The
    /// size of [`rect`](Self::rect): the requested size, or, along a stretched
    /// axis, what the margins and the usable area leave.
    pub configure: Size,
}

/// The result of arranging a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    /// One entry per input surface, in the order they were given.
    pub surfaces: Vec<PlacedSurface>,
    /// The area left for windows once every exclusive zone is taken off
    /// (`output->usable_area` in sway).
    pub usable: Rect,
}

/// A surface that cannot be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementError {
    /// The protocol's `invalid_size`: a size that is negative, or 0 on an axis
    /// that is not anchored at both of its ends (so there is nothing to
    /// stretch between).
    InvalidSize {
        /// The index of the surface in the input.
        index: usize,
        /// The axis whose size is wrong.
        axis: Axis,
    },
}

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementError::InvalidSize { index, axis } => {
                let (name, ends) = match axis {
                    Axis::Horizontal => ("width", "left and right"),
                    Axis::Vertical => ("height", "top and bottom"),
                };
                write!(
                    f,
                    "surface {index}: a {name} of 0 stretches the surface, and needs it anchored to \
                     both {ends}"
                )
            }
        }
    }
}

impl std::error::Error for PlacementError {}

/// Arranges the layer surfaces of one screen.
///
/// `surfaces` are in creation order. The result has one rectangle per surface,
/// in the same order, plus the usable area.
///
/// Mirrors sway's `arrange_layers`, which starts with the usable area equal to
/// the screen (`full_area`) and makes two passes over the layers from
/// `overlay` to `background`:
///
/// 1. every surface with an exclusive zone above 0, each placed in what the
///    surfaces before it left and then reserving its own zone;
/// 2. every other surface, placed in what the reservations left (or on the
///    whole screen, when its zone is -1).
///
/// Two consequences follow, which are the point of the order. Within a layer,
/// zones stack in creation order: a second top bar sits below the first. And a
/// panel with no zone that was created before a bar still avoids the bar,
/// because the first pass finishes before the second begins.
///
/// # Errors
///
/// [`PlacementError::InvalidSize`] for the first surface (in input order) with
/// a size the protocol rejects.
pub fn arrange(screen: Size, surfaces: &[LayerSurface]) -> Result<Arrangement, PlacementError> {
    for (index, s) in surfaces.iter().enumerate() {
        let a = s.anchors;
        if s.width < 0
            || (s.width == 0 && !(a.contains(Anchors::LEFT) && a.contains(Anchors::RIGHT)))
        {
            return Err(PlacementError::InvalidSize {
                index,
                axis: Axis::Horizontal,
            });
        }
        if s.height < 0
            || (s.height == 0 && !(a.contains(Anchors::TOP) && a.contains(Anchors::BOTTOM)))
        {
            return Err(PlacementError::InvalidSize {
                index,
                axis: Axis::Vertical,
            });
        }
    }

    let full = Rect::new(0, 0, screen.width, screen.height);
    let mut usable = full;
    let mut placed = vec![None; surfaces.len()];

    for exclusive in [true, false] {
        for layer in Layer::ARRANGE_ORDER {
            for (i, s) in surfaces.iter().enumerate() {
                if s.layer == layer && (s.exclusive_zone > 0) == exclusive {
                    placed[i] = Some(s.place(full, &mut usable));
                }
            }
        }
    }

    let surfaces = placed
        .into_iter()
        .map(|rect| {
            let rect = rect.unwrap_or_default();
            PlacedSurface {
                rect,
                configure: rect.size(),
            }
        })
        .collect();
    Ok(Arrangement { surfaces, usable })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Size = Size::new(1920, 1080);
    const ACROSS: Anchors = Anchors(Anchors::LEFT.0 | Anchors::RIGHT.0);
    const DOWN: Anchors = Anchors(Anchors::TOP.0 | Anchors::BOTTOM.0);
    const ALL: Anchors = Anchors(ACROSS.0 | DOWN.0);

    fn top_bar(height: i32) -> LayerSurface {
        LayerSurface {
            anchors: Anchors::TOP | ACROSS,
            height,
            exclusive_zone: height,
            ..LayerSurface::new(Layer::Top)
        }
    }

    fn arranged(surfaces: &[LayerSurface]) -> Arrangement {
        arrange(SCREEN, surfaces).expect("the surfaces are valid")
    }

    #[test]
    fn a_bar_on_the_top_edge_spans_the_screen_and_takes_the_top_off_the_usable_area() {
        let a = arranged(&[top_bar(40)]);
        assert_eq!(a.surfaces[0].rect, Rect::new(0, 0, 1920, 40));
        assert_eq!(a.usable, Rect::new(0, 40, 1920, 1040));
    }

    #[test]
    fn a_surface_is_configured_to_the_size_of_its_rectangle() {
        let a = arranged(&[top_bar(40)]);
        assert_eq!(a.surfaces[0].configure, Size::new(1920, 40));
    }

    #[test]
    fn each_edge_reserves_its_zone_from_its_own_side() {
        let bottom = LayerSurface {
            anchors: Anchors::BOTTOM | ACROSS,
            height: 30,
            exclusive_zone: 30,
            ..LayerSurface::new(Layer::Top)
        };
        let left = LayerSurface {
            anchors: Anchors::LEFT | DOWN,
            width: 50,
            exclusive_zone: 50,
            ..LayerSurface::new(Layer::Top)
        };
        let right = LayerSurface {
            anchors: Anchors::RIGHT | DOWN,
            width: 20,
            exclusive_zone: 20,
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[bottom, left, right]);
        // The bottom bar is first, so the side docks only span what is above it.
        assert_eq!(a.surfaces[0].rect, Rect::new(0, 1050, 1920, 30));
        assert_eq!(a.surfaces[1].rect, Rect::new(0, 0, 50, 1050));
        assert_eq!(a.surfaces[2].rect, Rect::new(1900, 0, 20, 1050));
        assert_eq!(a.usable, Rect::new(50, 0, 1850, 1050));
    }

    #[test]
    fn two_top_bars_stack_in_creation_order_and_then_a_left_dock_starts_below_both() {
        let first = top_bar(30);
        let second = top_bar(40);
        let dock = LayerSurface {
            anchors: Anchors::LEFT | DOWN,
            width: 60,
            exclusive_zone: 60,
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[first, second, dock]);
        assert_eq!(a.surfaces[0].rect, Rect::new(0, 0, 1920, 30));
        assert_eq!(a.surfaces[1].rect, Rect::new(0, 30, 1920, 40));
        assert_eq!(a.surfaces[2].rect, Rect::new(0, 70, 60, 1010));
        assert_eq!(a.usable, Rect::new(60, 70, 1860, 1010));
    }

    #[test]
    fn swapping_the_creation_order_swaps_the_stack() {
        let a = arranged(&[top_bar(40), top_bar(30)]);
        assert_eq!(a.surfaces[0].rect.y, 0);
        assert_eq!(a.surfaces[1].rect.y, 40);
    }

    #[test]
    fn an_exclusive_zone_counts_the_margin_on_its_edge() {
        let bar = LayerSurface {
            margin: Margin {
                top: 8,
                ..Margin::default()
            },
            ..top_bar(40)
        };
        let a = arranged(&[bar]);
        assert_eq!(a.surfaces[0].rect, Rect::new(0, 8, 1920, 40));
        assert_eq!(a.usable, Rect::new(0, 48, 1920, 1032));
    }

    #[test]
    fn a_zone_can_be_larger_than_the_surface_it_belongs_to() {
        let bar = LayerSurface {
            exclusive_zone: 56,
            ..top_bar(40)
        };
        let a = arranged(&[bar]);
        assert_eq!(a.surfaces[0].rect.height, 40);
        assert_eq!(a.usable.y, 56);
    }

    #[test]
    fn a_panel_with_no_zone_stays_in_the_area_the_bars_left() {
        let bar = top_bar(40);
        let fill = LayerSurface {
            anchors: ALL,
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[bar, fill]);
        assert_eq!(a.surfaces[1].rect, Rect::new(0, 40, 1920, 1040));
        // It reserved nothing, so the usable area is the bar's alone.
        assert_eq!(a.usable, Rect::new(0, 40, 1920, 1040));
    }

    #[test]
    fn a_panel_with_no_zone_avoids_a_bar_that_was_created_after_it() {
        let fill = LayerSurface {
            anchors: ALL,
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[fill, top_bar(40)]);
        assert_eq!(a.surfaces[0].rect, Rect::new(0, 40, 1920, 1040));
    }

    #[test]
    fn a_zone_of_minus_one_covers_the_screen_whatever_was_reserved() {
        let wallpaper = LayerSurface {
            anchors: ALL,
            exclusive_zone: -1,
            ..LayerSurface::new(Layer::Background)
        };
        let a = arranged(&[top_bar(40), wallpaper]);
        assert_eq!(a.surfaces[1].rect, Rect::new(0, 0, 1920, 1080));
        assert_eq!(a.usable, Rect::new(0, 40, 1920, 1040));
    }

    #[test]
    fn a_zone_of_minus_one_is_laid_out_on_the_screen_not_in_the_usable_area() {
        let osd = LayerSurface {
            anchors: Anchors::BOTTOM,
            width: 200,
            height: 100,
            margin: Margin {
                bottom: 10,
                ..Margin::default()
            },
            exclusive_zone: -1,
            ..LayerSurface::new(Layer::Overlay)
        };
        let bottom_bar = LayerSurface {
            anchors: Anchors::BOTTOM | ACROSS,
            height: 60,
            exclusive_zone: 60,
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[bottom_bar, osd]);
        assert_eq!(a.surfaces[1].rect, Rect::new(860, 970, 200, 100));
        let respecting = LayerSurface {
            exclusive_zone: 0,
            ..osd
        };
        let b = arranged(&[bottom_bar, respecting]);
        assert_eq!(b.surfaces[1].rect, Rect::new(860, 910, 200, 100));
    }

    #[test]
    fn an_overlay_reserves_before_a_top_layer_bar_whatever_the_creation_order() {
        // The exclusive pass visits the overlay layer first, so its zone is
        // taken off before the top bar is placed, though the bar came first.
        let bar = top_bar(40);
        let overlay = LayerSurface {
            layer: Layer::Overlay,
            ..top_bar(25)
        };
        let a = arranged(&[bar, overlay]);
        assert_eq!(a.surfaces[1].rect, Rect::new(0, 0, 1920, 25));
        assert_eq!(a.surfaces[0].rect, Rect::new(0, 25, 1920, 40));
        assert_eq!(a.usable.y, 65);
    }

    #[test]
    fn a_zone_only_counts_on_one_edge_or_one_edge_and_both_perpendicular_ones() {
        let shapes = [
            (Anchors::TOP, Some(Edge::Top)),
            (Anchors::TOP | ACROSS, Some(Edge::Top)),
            (Anchors::BOTTOM, Some(Edge::Bottom)),
            (Anchors::BOTTOM | ACROSS, Some(Edge::Bottom)),
            (Anchors::LEFT, Some(Edge::Left)),
            (Anchors::LEFT | DOWN, Some(Edge::Left)),
            (Anchors::RIGHT, Some(Edge::Right)),
            (Anchors::RIGHT | DOWN, Some(Edge::Right)),
            (Anchors::TOP | Anchors::LEFT, None),
            (Anchors::BOTTOM | Anchors::RIGHT, None),
            (ACROSS, None),
            (DOWN, None),
            (ALL, None),
            (Anchors::NONE, None),
            (
                Anchors::TOP | Anchors::LEFT | Anchors::BOTTOM,
                Some(Edge::Left),
            ),
        ];
        for (anchors, edge) in shapes {
            assert_eq!(exclusive_edge(anchors, 10), edge, "{anchors:?}");
        }
    }

    #[test]
    fn a_zone_of_zero_or_less_has_no_edge() {
        assert_eq!(exclusive_edge(Anchors::TOP | ACROSS, 0), None);
        assert_eq!(exclusive_edge(Anchors::TOP | ACROSS, -1), None);
    }

    #[test]
    fn a_corner_anchored_panel_reserves_nothing_even_with_a_zone() {
        let corner = LayerSurface {
            anchors: Anchors::TOP | Anchors::LEFT,
            width: 300,
            height: 40,
            exclusive_zone: 40,
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[corner]);
        assert_eq!(a.usable, Rect::new(0, 0, 1920, 1080));
    }

    #[test]
    fn a_zone_larger_than_the_screen_leaves_an_empty_usable_area_not_a_negative_one() {
        let huge = LayerSurface {
            exclusive_zone: 5000,
            ..top_bar(40)
        };
        let a = arranged(&[huge]);
        assert_eq!(a.usable.height, 0);
        assert_eq!(a.usable.width, 1920);
    }

    #[test]
    fn a_top_right_panel_sits_in_its_corner_inside_its_margins() {
        let panel = LayerSurface {
            anchors: Anchors::TOP | Anchors::RIGHT,
            width: 300,
            height: 120,
            margin: Margin {
                top: 10,
                right: 20,
                ..Margin::default()
            },
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[panel]);
        assert_eq!(a.surfaces[0].rect, Rect::new(1600, 10, 300, 120));
    }

    #[test]
    fn a_bottom_left_panel_counts_its_bottom_and_left_margins() {
        let panel = LayerSurface {
            anchors: Anchors::BOTTOM | Anchors::LEFT,
            width: 300,
            height: 120,
            margin: Margin {
                bottom: 12,
                left: 16,
                top: 99,
                right: 99,
            },
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[panel]);
        assert_eq!(a.surfaces[0].rect, Rect::new(16, 948, 300, 120));
    }

    #[test]
    fn the_other_two_corners_are_placed_from_their_own_margins() {
        let top_left = LayerSurface {
            anchors: Anchors::TOP | Anchors::LEFT,
            width: 100,
            height: 50,
            margin: Margin {
                top: 5,
                left: 7,
                ..Margin::default()
            },
            ..LayerSurface::new(Layer::Top)
        };
        let bottom_right = LayerSurface {
            anchors: Anchors::BOTTOM | Anchors::RIGHT,
            width: 100,
            height: 50,
            margin: Margin {
                bottom: 3,
                right: 9,
                ..Margin::default()
            },
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[top_left, bottom_right]);
        assert_eq!(a.surfaces[0].rect, Rect::new(7, 5, 100, 50));
        assert_eq!(a.surfaces[1].rect, Rect::new(1811, 1027, 100, 50));
    }

    #[test]
    fn a_corner_panel_stays_inside_the_area_the_bars_left() {
        let corner = LayerSurface {
            anchors: Anchors::TOP | Anchors::RIGHT,
            width: 200,
            height: 100,
            margin: Margin::all(10),
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[top_bar(40), corner]);
        assert_eq!(a.surfaces[1].rect, Rect::new(1710, 50, 200, 100));
    }

    #[test]
    fn a_stretched_axis_takes_what_the_margins_leave() {
        let bar = LayerSurface {
            anchors: Anchors::BOTTOM | ACROSS,
            height: 50,
            margin: Margin {
                left: 8,
                right: 12,
                bottom: 6,
                top: 0,
            },
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[bar]);
        assert_eq!(a.surfaces[0].rect, Rect::new(8, 1024, 1900, 50));
        assert_eq!(a.surfaces[0].configure, Size::new(1900, 50));
    }

    #[test]
    fn both_axes_can_stretch_at_once() {
        let fill = LayerSurface {
            anchors: ALL,
            margin: Margin {
                top: 1,
                right: 2,
                bottom: 3,
                left: 4,
            },
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[fill]);
        assert_eq!(a.surfaces[0].rect, Rect::new(4, 1, 1914, 1076));
    }

    #[test]
    fn a_panel_anchored_to_one_edge_is_centred_along_it() {
        let bottom = LayerSurface {
            anchors: Anchors::BOTTOM,
            width: 400,
            height: 64,
            margin: Margin {
                bottom: 8,
                ..Margin::default()
            },
            ..LayerSurface::new(Layer::Top)
        };
        let left = LayerSurface {
            anchors: Anchors::LEFT,
            width: 64,
            height: 300,
            margin: Margin {
                left: 8,
                ..Margin::default()
            },
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[bottom, left]);
        assert_eq!(a.surfaces[0].rect, Rect::new(760, 1008, 400, 64));
        assert_eq!(a.surfaces[1].rect, Rect::new(8, 390, 64, 300));
    }

    #[test]
    fn a_panel_with_no_anchors_is_centred_and_ignores_its_margins() {
        let dialog = LayerSurface {
            width: 600,
            height: 300,
            margin: Margin::all(50),
            ..LayerSurface::new(Layer::Overlay)
        };
        let a = arranged(&[dialog]);
        assert_eq!(a.surfaces[0].rect, Rect::new(660, 390, 600, 300));
    }

    #[test]
    fn a_panel_anchored_to_both_ends_with_a_size_is_centred_between_them() {
        let narrow = LayerSurface {
            anchors: ACROSS,
            width: 500,
            height: 80,
            margin: Margin::all(30),
            ..LayerSurface::new(Layer::Top)
        };
        let a = arranged(&[narrow]);
        assert_eq!(a.surfaces[0].rect.x, 710);
        // Not anchored vertically, so centred on that axis too.
        assert_eq!(a.surfaces[0].rect.y, 500);
    }

    #[test]
    fn an_odd_leftover_puts_the_extra_pixel_after_the_surface() {
        let panel = LayerSurface {
            width: 101,
            height: 51,
            ..LayerSurface::new(Layer::Top)
        };
        let a = arrange(Size::new(1000, 500), &[panel]).unwrap();
        // 1000 / 2 - 101 / 2 = 500 - 50, 500 / 2 - 51 / 2 = 250 - 25.
        assert_eq!(a.surfaces[0].rect, Rect::new(450, 225, 101, 51));
    }

    #[test]
    fn a_surface_with_no_zone_goes_to_the_same_place_in_every_layer() {
        let make = |layer| LayerSurface {
            layer,
            anchors: Anchors::TOP | Anchors::RIGHT,
            width: 10,
            height: 10,
            ..LayerSurface::new(Layer::Top)
        };
        let layers = [Layer::Background, Layer::Bottom, Layer::Top, Layer::Overlay];
        let a = arranged(&layers.map(make));
        for placed in &a.surfaces {
            assert_eq!(placed.rect, Rect::new(1910, 0, 10, 10));
        }
    }

    #[test]
    fn the_results_come_back_in_the_order_the_surfaces_were_given() {
        let a = arranged(&[
            LayerSurface {
                anchors: ALL,
                ..LayerSurface::new(Layer::Background)
            },
            top_bar(40),
            LayerSurface {
                width: 10,
                height: 10,
                ..LayerSurface::new(Layer::Overlay)
            },
        ]);
        assert_eq!(a.surfaces.len(), 3);
        assert_eq!(a.surfaces[1].rect, Rect::new(0, 0, 1920, 40));
        assert_eq!(a.surfaces[2].rect, Rect::new(955, 555, 10, 10));
        // Arranged after the bar, though first in the list.
        assert_eq!(a.surfaces[0].rect, Rect::new(0, 40, 1920, 1040));
    }

    #[test]
    fn a_screen_with_no_surfaces_is_all_usable() {
        let a = arranged(&[]);
        assert_eq!(a.surfaces.len(), 0);
        assert_eq!(a.usable, Rect::new(0, 0, 1920, 1080));
    }

    #[test]
    fn a_width_of_zero_without_both_side_anchors_is_an_error_naming_the_surface() {
        let ok = top_bar(40);
        let bad = LayerSurface {
            anchors: Anchors::TOP | Anchors::LEFT,
            width: 0,
            height: 40,
            ..LayerSurface::new(Layer::Top)
        };
        let err = arrange(SCREEN, &[ok, bad]).unwrap_err();
        assert_eq!(
            err,
            PlacementError::InvalidSize {
                index: 1,
                axis: Axis::Horizontal
            }
        );
        assert!(err.to_string().contains("surface 1"));
        assert!(err.to_string().contains("left and right"));
    }

    #[test]
    fn a_height_of_zero_without_both_vertical_anchors_is_an_error() {
        let bad = LayerSurface {
            anchors: Anchors::TOP,
            width: 100,
            height: 0,
            ..LayerSurface::new(Layer::Top)
        };
        assert_eq!(
            arrange(SCREEN, &[bad]).unwrap_err(),
            PlacementError::InvalidSize {
                index: 0,
                axis: Axis::Vertical
            }
        );
    }

    #[test]
    fn a_negative_size_is_an_error_even_on_a_stretched_axis() {
        let bad = LayerSurface {
            width: -1,
            ..top_bar(40)
        };
        assert!(arrange(SCREEN, &[bad]).is_err());
    }
}
