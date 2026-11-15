//! The `xdg_positioner` solver: where a popup goes, and how it is moved back
//! inside the screen when it would not fit.
//!
//! Two sources, and they are not the same thing:
//!
//! - The geometry (the anchor point on the anchor rectangle, the gravity, the
//!   offset) follows the protocol text of `xdg_positioner` in `xdg-shell.xml`
//!   and is implemented as `wlr_xdg_positioner_rules_get_geometry`
//!   (`types/xdg_shell/wlr_xdg_positioner.c`).
//! - The constraint adjustments follow wlroots'
//!   `wlr_xdg_positioner_rules_unconstrain_box` in the same file, which the
//!   protocol leaves open ("whether a surface is considered 'constrained' is
//!   left to the compositor"): flip, then slide, then resize, each stopping the
//!   chain as soon as the popup fits.
//!
//! Where the two differ, [`Positioner::solve`] documents which it does. The
//! differences are in the slide (wlroots moves the least distance that helps,
//! where the protocol prose names a direction to try first) and in the resize
//! (the protocol says only that the popup ends up unconstrained).

use std::ops::BitOr;

use super::{Rect, Size};

/// A point on the anchor rectangle (`xdg_positioner.anchor`), or the side of
/// that point a popup extends toward (`xdg_positioner.gravity`). The two enums
/// of the protocol have the same nine values, so this is one type for both.
///
/// As an anchor, a corner is that corner, an edge is the middle of that edge,
/// and `None` is the centre. As a gravity, a corner puts the popup beyond that
/// corner of the anchor point, an edge centres it along the other axis, and
/// `None` centres it on both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Anchor {
    /// The centre of the rectangle (as an anchor), or centred on the point (as
    /// a gravity).
    #[default]
    None,
    /// The middle of the top edge.
    Top,
    /// The middle of the bottom edge.
    Bottom,
    /// The middle of the left edge.
    Left,
    /// The middle of the right edge.
    Right,
    /// The top-left corner.
    TopLeft,
    /// The bottom-left corner.
    BottomLeft,
    /// The top-right corner.
    TopRight,
    /// The bottom-right corner.
    BottomRight,
}

/// The gravity of a popup: the same nine values as an [`Anchor`].
pub type Gravity = Anchor;

impl Anchor {
    /// The horizontal and vertical component: -1 toward the left or top, 1
    /// toward the right or bottom, 0 for neither.
    const fn dirs(self) -> (i8, i8) {
        match self {
            Anchor::None => (0, 0),
            Anchor::Top => (0, -1),
            Anchor::Bottom => (0, 1),
            Anchor::Left => (-1, 0),
            Anchor::Right => (1, 0),
            Anchor::TopLeft => (-1, -1),
            Anchor::BottomLeft => (-1, 1),
            Anchor::TopRight => (1, -1),
            Anchor::BottomRight => (1, 1),
        }
    }

    const fn from_dirs(x: i8, y: i8) -> Anchor {
        match (x, y) {
            (-1, -1) => Anchor::TopLeft,
            (-1, 1) => Anchor::BottomLeft,
            (1, -1) => Anchor::TopRight,
            (1, 1) => Anchor::BottomRight,
            (-1, _) => Anchor::Left,
            (1, _) => Anchor::Right,
            (_, -1) => Anchor::Top,
            (_, 1) => Anchor::Bottom,
            _ => Anchor::None,
        }
    }

    /// Left and right swapped, top and bottom kept
    /// (`xdg_positioner_anchor_invert_x`).
    #[must_use]
    pub const fn invert_x(self) -> Anchor {
        let (x, y) = self.dirs();
        Anchor::from_dirs(-x, y)
    }

    /// Top and bottom swapped, left and right kept
    /// (`xdg_positioner_anchor_invert_y`).
    #[must_use]
    pub const fn invert_y(self) -> Anchor {
        let (x, y) = self.dirs();
        Anchor::from_dirs(x, -y)
    }
}

/// How the compositor may move a popup that would be partly outside the
/// constraint area (`xdg_positioner.constraint_adjustment`, a bitfield with the
/// protocol's values). Combine with `|`.
///
/// Each axis is adjusted by flip, then slide, then resize, whichever of them
/// are set. With no bit set for an axis, the popup is left where it is on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ConstraintAdjustment(u8);

impl ConstraintAdjustment {
    /// Leave a constrained popup where it is.
    pub const NONE: ConstraintAdjustment = ConstraintAdjustment(0);
    /// Slide along the x axis until it is no longer constrained.
    pub const SLIDE_X: ConstraintAdjustment = ConstraintAdjustment(1);
    /// Slide along the y axis until it is no longer constrained.
    pub const SLIDE_Y: ConstraintAdjustment = ConstraintAdjustment(2);
    /// Invert anchor and gravity on the x axis if that makes it fit.
    pub const FLIP_X: ConstraintAdjustment = ConstraintAdjustment(4);
    /// Invert anchor and gravity on the y axis if that makes it fit.
    pub const FLIP_Y: ConstraintAdjustment = ConstraintAdjustment(8);
    /// Shrink horizontally until it fits.
    pub const RESIZE_X: ConstraintAdjustment = ConstraintAdjustment(16);
    /// Shrink vertically until it fits.
    pub const RESIZE_Y: ConstraintAdjustment = ConstraintAdjustment(32);

    /// Whether every flag of `other` is set.
    #[must_use]
    pub const fn contains(self, other: ConstraintAdjustment) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for ConstraintAdjustment {
    type Output = ConstraintAdjustment;

    fn bitor(self, rhs: ConstraintAdjustment) -> ConstraintAdjustment {
        ConstraintAdjustment(self.0 | rhs.0)
    }
}

/// A positioner that cannot be used: the protocol's `invalid_input` and
/// `invalid_positioner` errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionerError {
    /// The popup's size is zero or negative (`set_size`).
    InvalidSize,
    /// The anchor rectangle has a negative width or height (`set_anchor_rect`).
    NegativeAnchorRect,
    /// The anchor rectangle has no width. The protocol calls a positioner
    /// without a non-zero anchor rectangle incomplete, and wlroots
    /// (`wlr_xdg_positioner_is_complete`) tests the width.
    EmptyAnchorRect,
}

impl std::fmt::Display for PositionerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PositionerError::InvalidSize => "a popup's width and height must be positive",
            PositionerError::NegativeAnchorRect => {
                "an anchor rectangle's width and height must not be negative"
            }
            PositionerError::EmptyAnchorRect => "an anchor rectangle needs a width above zero",
        })
    }
}

impl std::error::Error for PositionerError {}

/// The rules of one popup: an `xdg_positioner` together with the area the popup
/// must stay inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Positioner {
    /// The area the popup is kept inside, in the same coordinates as the
    /// anchor rectangle: the parent's. Usually the screen, as the compositor
    /// sees it from the parent's origin (so its `x` and `y` are often negative
    /// for a parent that is not at the screen's corner).
    pub constraint: Rect,
    /// The rectangle of the parent the popup is placed against (`set_anchor_rect`).
    pub anchor_rect: Rect,
    /// The point on the anchor rectangle the popup is placed at.
    pub anchor: Anchor,
    /// The side of that point the popup extends toward.
    pub gravity: Gravity,
    /// Added to the position (`set_offset`), before constraint testing.
    pub offset: (i32, i32),
    /// The popup's size (`set_size`).
    pub size: Size,
    /// What may be done when the popup does not fit.
    pub adjustment: ConstraintAdjustment,
}

/// How far each edge of a box is outside the constraint: positive outside,
/// zero or negative inside (`struct constraint_offsets`).
#[derive(Debug, Clone, Copy)]
struct Offsets {
    top: i32,
    bottom: i32,
    left: i32,
    right: i32,
}

impl Offsets {
    /// `get_constrained_box_offsets`.
    fn of(constraint: Rect, b: Rect) -> Offsets {
        Offsets {
            left: constraint.x - b.x,
            right: b.right() - constraint.right(),
            top: constraint.y - b.y,
            bottom: b.bottom() - constraint.bottom(),
        }
    }

    /// `is_unconstrained`.
    fn fits(self) -> bool {
        self.top <= 0 && self.bottom <= 0 && self.left <= 0 && self.right <= 0
    }
}

impl Positioner {
    fn validate(&self) -> Result<(), PositionerError> {
        if self.size.width <= 0 || self.size.height <= 0 {
            return Err(PositionerError::InvalidSize);
        }
        if self.anchor_rect.width < 0 || self.anchor_rect.height < 0 {
            return Err(PositionerError::NegativeAnchorRect);
        }
        if self.anchor_rect.width == 0 {
            return Err(PositionerError::EmptyAnchorRect);
        }
        Ok(())
    }

    /// Where the popup goes before any constraint is looked at: the anchor
    /// point, the gravity, the offset.
    ///
    /// Mirrors `wlr_xdg_positioner_rules_get_geometry`. The anchor point is a
    /// corner of the anchor rectangle for a corner anchor, the middle of an
    /// edge for an edge, the centre for `None` (halves are integer division).
    /// The popup then extends from the point toward the gravity: for `Top` its
    /// bottom edge is at the point, for `Left` its right edge is, and along an
    /// axis with no gravity it is centred on the point. The offset is added
    /// last, so it is the same for every gravity, as the protocol's example
    /// says.
    ///
    /// # Errors
    ///
    /// A [`PositionerError`] for a size or anchor rectangle the protocol
    /// refuses.
    pub fn geometry(&self) -> Result<Rect, PositionerError> {
        self.validate()?;
        Ok(self.raw_geometry())
    }

    fn raw_geometry(&self) -> Rect {
        let r = self.anchor_rect;
        let (ax, ay) = self.anchor.dirs();
        let (gx, gy) = self.gravity.dirs();
        let mut b = Rect::new(
            self.offset.0,
            self.offset.1,
            self.size.width,
            self.size.height,
        );

        b.y += match ay {
            -1 => r.y,
            1 => r.y + r.height,
            _ => r.y + r.height / 2,
        };
        b.x += match ax {
            -1 => r.x,
            1 => r.x + r.width,
            _ => r.x + r.width / 2,
        };

        match gy {
            -1 => b.y -= b.height,
            0 => b.y -= b.height / 2,
            _ => {}
        }
        match gx {
            -1 => b.x -= b.width,
            0 => b.x -= b.width / 2,
            _ => {}
        }
        b
    }

    /// The popup's rectangle, kept inside [`constraint`](Self::constraint) as
    /// far as the [`adjustment`](Self::adjustment) allows.
    ///
    /// Mirrors `wlr_xdg_positioner_rules_unconstrain_box`: take the geometry,
    /// and if any edge is outside the constraint try, in order, flip, slide and
    /// resize, stopping at the first that leaves it inside. What a step that
    /// does not fit everything leaves behind carries to the next:
    ///
    /// - **Flip.** An axis is flipped (anchor and gravity inverted on it, the
    ///   geometry recomputed from the original anchor rectangle and offset)
    ///   only when its flag is set and exactly one of its two edges is outside:
    ///   if both are, the popup is larger than the area and flipping cannot
    ///   help. The flipped position is kept only if both its edges on that axis
    ///   are inside, otherwise that axis keeps its original position, which is
    ///   the protocol's "the resulting position will be the one before the
    ///   adjustment". The axes are judged separately, so x can flip while y
    ///   does not.
    /// - **Slide.** An axis with an edge outside and its flag set moves by the
    ///   smaller of the two distances that would bring one edge exactly to the
    ///   constraint's: the protocol prose says to try the gravity's direction
    ///   first and the opposite after, which gives the same position except
    ///   when the popup is larger than the area, where both edges are outside.
    ///   There wlroots slides toward the gravity (so that the edge on the
    ///   gravity's side is the one that ends up inside), and a popup with no
    ///   gravity on the axis is slid right or down. This follows wlroots.
    /// - **Resize.** An axis with an edge outside and its flag set is clipped
    ///   to the constraint, on the sides that are outside. If that would leave
    ///   nothing, the popup is left as it was before the resize.
    ///
    /// A popup that cannot be made to fit comes back wherever the last step
    /// left it. It is never an error to be constrained: the compositor
    /// positions the popup anyway.
    ///
    /// # Errors
    ///
    /// A [`PositionerError`] for a size or anchor rectangle the protocol
    /// refuses.
    pub fn solve(&self) -> Result<Rect, PositionerError> {
        self.validate()?;
        let mut b = self.raw_geometry();
        let mut off = Offsets::of(self.constraint, b);
        if off.fits() {
            return Ok(b);
        }
        if self.unconstrain_by_flip(&mut b, &mut off) {
            return Ok(b);
        }
        if self.unconstrain_by_slide(&mut b, &mut off) {
            return Ok(b);
        }
        self.unconstrain_by_resize(&mut b, &mut off);
        Ok(b)
    }

    /// `xdg_positioner_rules_unconstrain_by_flip`.
    fn unconstrain_by_flip(&self, b: &mut Rect, off: &mut Offsets) -> bool {
        let adj = self.adjustment;
        let flip_x =
            ((off.left > 0) ^ (off.right > 0)) && adj.contains(ConstraintAdjustment::FLIP_X);
        let flip_y =
            ((off.top > 0) ^ (off.bottom > 0)) && adj.contains(ConstraintAdjustment::FLIP_Y);
        if !flip_x && !flip_y {
            return false;
        }

        let mut flipped = *self;
        if flip_x {
            flipped.anchor = flipped.anchor.invert_x();
            flipped.gravity = flipped.gravity.invert_x();
        }
        if flip_y {
            flipped.anchor = flipped.anchor.invert_y();
            flipped.gravity = flipped.gravity.invert_y();
        }
        let fb = flipped.raw_geometry();
        let fo = Offsets::of(self.constraint, fb);

        if fo.left <= 0 && fo.right <= 0 {
            b.x = fb.x;
            off.left = fo.left;
            off.right = fo.right;
        }
        if fo.top <= 0 && fo.bottom <= 0 {
            b.y = fb.y;
            off.top = fo.top;
            off.bottom = fo.bottom;
        }
        off.fits()
    }

    /// `xdg_positioner_rules_unconstrain_by_slide`.
    fn unconstrain_by_slide(&self, b: &mut Rect, off: &mut Offsets) -> bool {
        let adj = self.adjustment;
        let (gx, gy) = self.gravity.dirs();
        let slide_x =
            (off.left > 0 || off.right > 0) && adj.contains(ConstraintAdjustment::SLIDE_X);
        let slide_y =
            (off.top > 0 || off.bottom > 0) && adj.contains(ConstraintAdjustment::SLIDE_Y);
        if !slide_x && !slide_y {
            return false;
        }

        if slide_x {
            if off.left > 0 && off.right > 0 {
                // Larger than the area: toward the gravity, else right.
                if gx < 0 {
                    b.x -= off.right;
                } else {
                    b.x += off.left;
                }
            } else if off.left.abs() < off.right.abs() {
                b.x += off.left;
            } else {
                b.x -= off.right;
            }
        }
        if slide_y {
            if off.top > 0 && off.bottom > 0 {
                if gy < 0 {
                    b.y -= off.bottom;
                } else {
                    b.y += off.top;
                }
            } else if off.top.abs() < off.bottom.abs() {
                b.y += off.top;
            } else {
                b.y -= off.bottom;
            }
        }

        *off = Offsets::of(self.constraint, *b);
        off.fits()
    }

    /// `xdg_positioner_rules_unconstrain_by_resize`.
    fn unconstrain_by_resize(&self, b: &mut Rect, off: &mut Offsets) -> bool {
        let adj = self.adjustment;
        let resize_x =
            (off.left > 0 || off.right > 0) && adj.contains(ConstraintAdjustment::RESIZE_X);
        let resize_y =
            (off.top > 0 || off.bottom > 0) && adj.contains(ConstraintAdjustment::RESIZE_Y);
        if !resize_x && !resize_y {
            return false;
        }

        let left = off.left.max(0);
        let right = off.right.max(0);
        let top = off.top.max(0);
        let bottom = off.bottom.max(0);

        let mut resized = *b;
        if resize_x {
            resized.x += left;
            resized.width -= left + right;
        }
        if resize_y {
            resized.y += top;
            resized.height -= top + bottom;
        }
        if resized.is_empty() {
            return false;
        }

        *b = resized;
        *off = Offsets::of(self.constraint, *b);
        off.fits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0, 0, 800, 600);

    /// A menu of `w` x `h` hanging below `anchor_rect`.
    fn below(anchor_rect: Rect, w: i32, h: i32, adjustment: ConstraintAdjustment) -> Positioner {
        Positioner {
            constraint: SCREEN,
            anchor_rect,
            anchor: Anchor::Bottom,
            gravity: Anchor::Bottom,
            offset: (0, 0),
            size: Size::new(w, h),
            adjustment,
        }
    }

    fn plain(anchor: Anchor, gravity: Gravity) -> Positioner {
        Positioner {
            constraint: Rect::new(-1000, -1000, 5000, 5000),
            anchor_rect: Rect::new(100, 200, 40, 20),
            anchor,
            gravity,
            offset: (0, 0),
            size: Size::new(30, 10),
            adjustment: ConstraintAdjustment::NONE,
        }
    }

    #[test]
    fn every_anchor_names_the_point_the_popup_hangs_from() {
        // The anchor rectangle is (100, 200) 40 x 20; a popup with no gravity
        // is centred on the point, so its centre is the point.
        let points = [
            (Anchor::None, (120, 210)),
            (Anchor::Top, (120, 200)),
            (Anchor::Bottom, (120, 220)),
            (Anchor::Left, (100, 210)),
            (Anchor::Right, (140, 210)),
            (Anchor::TopLeft, (100, 200)),
            (Anchor::BottomLeft, (100, 220)),
            (Anchor::TopRight, (140, 200)),
            (Anchor::BottomRight, (140, 220)),
        ];
        for (anchor, (px, py)) in points {
            let r = plain(anchor, Anchor::None).geometry().unwrap();
            assert_eq!((r.x + 15, r.y + 5), (px, py), "{anchor:?}");
        }
    }

    #[test]
    fn every_gravity_puts_the_popup_on_its_side_of_the_anchor_point() {
        // Anchored at the centre (120, 210); the popup is 30 x 10.
        let corners = [
            (Anchor::None, (105, 205)),
            (Anchor::Top, (105, 200)),
            (Anchor::Bottom, (105, 210)),
            (Anchor::Left, (90, 205)),
            (Anchor::Right, (120, 205)),
            (Anchor::TopLeft, (90, 200)),
            (Anchor::BottomLeft, (90, 210)),
            (Anchor::TopRight, (120, 200)),
            (Anchor::BottomRight, (120, 210)),
        ];
        for (gravity, (x, y)) in corners {
            let r = plain(Anchor::None, gravity).geometry().unwrap();
            assert_eq!((r.x, r.y), (x, y), "{gravity:?}");
            assert_eq!(r.size(), Size::new(30, 10));
        }
    }

    #[test]
    fn all_eighty_one_anchor_and_gravity_pairs_are_the_point_plus_the_side() {
        let anchors = [
            (Anchor::None, (120, 210)),
            (Anchor::Top, (120, 200)),
            (Anchor::Bottom, (120, 220)),
            (Anchor::Left, (100, 210)),
            (Anchor::Right, (140, 210)),
            (Anchor::TopLeft, (100, 200)),
            (Anchor::BottomLeft, (100, 220)),
            (Anchor::TopRight, (140, 200)),
            (Anchor::BottomRight, (140, 220)),
        ];
        let gravities = [
            (Anchor::None, (-15, -5)),
            (Anchor::Top, (-15, -10)),
            (Anchor::Bottom, (-15, 0)),
            (Anchor::Left, (-30, -5)),
            (Anchor::Right, (0, -5)),
            (Anchor::TopLeft, (-30, -10)),
            (Anchor::BottomLeft, (-30, 0)),
            (Anchor::TopRight, (0, -10)),
            (Anchor::BottomRight, (0, 0)),
        ];
        for (anchor, (px, py)) in anchors {
            for (gravity, (dx, dy)) in gravities {
                let r = plain(anchor, gravity).geometry().unwrap();
                assert_eq!(
                    (r.x, r.y),
                    (px + dx, py + dy),
                    "anchor {anchor:?}, gravity {gravity:?}"
                );
            }
        }
    }

    #[test]
    fn the_protocols_example_is_the_anchor_point_plus_the_offset() {
        // "if the anchor of the anchor rectangle is at (x, y), the surface has
        // the gravity bottom|right, and the offset is (ox, oy), the calculated
        // surface position will be (x + ox, y + oy)".
        let p = Positioner {
            offset: (7, -3),
            ..plain(Anchor::TopLeft, Anchor::BottomRight)
        };
        let r = p.geometry().unwrap();
        assert_eq!((r.x, r.y), (100 + 7, 200 - 3));
    }

    #[test]
    fn the_offset_does_not_depend_on_the_gravity() {
        for gravity in [Anchor::Top, Anchor::Left, Anchor::BottomRight, Anchor::None] {
            let base = plain(Anchor::Bottom, gravity).geometry().unwrap();
            let moved = Positioner {
                offset: (4, 9),
                ..plain(Anchor::Bottom, gravity)
            }
            .geometry()
            .unwrap();
            assert_eq!((moved.x - base.x, moved.y - base.y), (4, 9), "{gravity:?}");
        }
    }

    #[test]
    fn halves_of_odd_sizes_round_toward_zero() {
        let p = Positioner {
            anchor_rect: Rect::new(0, 0, 41, 21),
            size: Size::new(31, 11),
            ..plain(Anchor::None, Anchor::None)
        };
        let r = p.geometry().unwrap();
        // 41 / 2 = 20 and 31 / 2 = 15; 21 / 2 = 10 and 11 / 2 = 5.
        assert_eq!((r.x, r.y), (5, 5));
    }

    #[test]
    fn inverting_an_anchor_swaps_only_the_named_axis() {
        assert_eq!(Anchor::TopLeft.invert_x(), Anchor::TopRight);
        assert_eq!(Anchor::TopLeft.invert_y(), Anchor::BottomLeft);
        assert_eq!(Anchor::Left.invert_x(), Anchor::Right);
        assert_eq!(Anchor::Left.invert_y(), Anchor::Left);
        assert_eq!(Anchor::Bottom.invert_y(), Anchor::Top);
        assert_eq!(Anchor::Bottom.invert_x(), Anchor::Bottom);
        assert_eq!(Anchor::None.invert_x(), Anchor::None);
        assert_eq!(Anchor::None.invert_y(), Anchor::None);
        for a in [
            Anchor::Top,
            Anchor::BottomRight,
            Anchor::Right,
            Anchor::BottomLeft,
        ] {
            assert_eq!(a.invert_x().invert_x(), a);
            assert_eq!(a.invert_y().invert_y(), a);
        }
    }

    #[test]
    fn a_popup_that_fits_is_left_exactly_where_the_geometry_puts_it() {
        let all = ConstraintAdjustment::FLIP_X
            | ConstraintAdjustment::FLIP_Y
            | ConstraintAdjustment::SLIDE_X
            | ConstraintAdjustment::SLIDE_Y
            | ConstraintAdjustment::RESIZE_X
            | ConstraintAdjustment::RESIZE_Y;
        let p = below(Rect::new(100, 20, 80, 40), 200, 100, all);
        assert_eq!(p.solve(), p.geometry());
        assert_eq!(p.solve().unwrap(), Rect::new(40, 60, 200, 100));
    }

    #[test]
    fn a_menu_below_the_bottom_edge_flips_above_the_anchor_when_there_is_room() {
        let p = below(
            Rect::new(10, 570, 100, 30),
            200,
            100,
            ConstraintAdjustment::FLIP_Y,
        );
        // Unflipped it would start at y = 600 and run 100 past the screen.
        assert_eq!(p.geometry().unwrap().y, 600);
        let r = p.solve().unwrap();
        assert_eq!((r.y, r.height), (470, 100));
        assert_eq!(r.x, 10 + 50 - 100);
    }

    #[test]
    fn without_the_flip_flag_a_constrained_popup_stays_constrained() {
        let p = below(
            Rect::new(10, 570, 100, 30),
            200,
            100,
            ConstraintAdjustment::NONE,
        );
        assert_eq!(p.solve().unwrap().y, 600);
        let sliding_x_only = below(
            Rect::new(10, 570, 100, 30),
            200,
            100,
            ConstraintAdjustment::SLIDE_X | ConstraintAdjustment::FLIP_X,
        );
        assert_eq!(sliding_x_only.solve().unwrap().y, 600);
    }

    #[test]
    fn a_flip_that_does_not_fit_either_leaves_the_original_position() {
        // 300 tall, anchor rectangle at 140..170, popup 200 tall: below it
        // overflows the bottom by 70, above it overflows the top by 60.
        let p = Positioner {
            constraint: Rect::new(0, 0, 800, 300),
            ..below(
                Rect::new(10, 140, 100, 30),
                100,
                200,
                ConstraintAdjustment::FLIP_Y,
            )
        };
        assert_eq!(p.solve().unwrap().y, 170);
    }

    #[test]
    fn when_the_flip_does_not_fit_the_slide_takes_over() {
        let p = Positioner {
            constraint: Rect::new(0, 0, 800, 300),
            ..below(
                Rect::new(10, 140, 100, 30),
                100,
                200,
                ConstraintAdjustment::FLIP_Y | ConstraintAdjustment::SLIDE_Y,
            )
        };
        // Slid up by the 70 pixels it was over the bottom edge, from y = 170.
        assert_eq!(p.solve().unwrap().y, 100);
    }

    #[test]
    fn a_flip_that_works_is_taken_in_preference_to_a_slide() {
        let p = below(
            Rect::new(10, 570, 100, 30),
            200,
            100,
            ConstraintAdjustment::FLIP_Y | ConstraintAdjustment::SLIDE_Y,
        );
        assert_eq!(p.solve().unwrap().y, 470);
    }

    #[test]
    fn a_popup_hanging_to_the_right_flips_to_the_left_of_its_anchor() {
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(700, 100, 80, 30),
            anchor: Anchor::Right,
            gravity: Anchor::Right,
            offset: (0, 0),
            size: Size::new(150, 200),
            adjustment: ConstraintAdjustment::FLIP_X,
        };
        // From x = 780 it would end at 930; flipped it ends where the anchor rectangle starts.
        assert_eq!(p.geometry().unwrap().x, 780);
        let r = p.solve().unwrap();
        assert_eq!(r.x, 700 - 150);
        assert_eq!(r.y, 115 - 100);
    }

    #[test]
    fn a_flip_uses_the_original_anchor_rectangle_and_offset() {
        let p = Positioner {
            offset: (0, 12),
            ..below(
                Rect::new(10, 570, 100, 30),
                200,
                100,
                ConstraintAdjustment::FLIP_Y,
            )
        };
        // Flipped: anchor top (570), gravity top (-100), offset still +12.
        assert_eq!(p.solve().unwrap().y, 570 - 100 + 12);
    }

    #[test]
    fn the_axes_flip_independently_and_a_good_flip_survives_a_bad_one() {
        // A corner popup: anchor and gravity bottom-right of a rectangle in the
        // bottom-right of the screen. x flips to the left and fits; y would
        // flip above and fit too.
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(700, 500, 80, 80),
            anchor: Anchor::BottomRight,
            gravity: Anchor::BottomRight,
            offset: (0, 0),
            size: Size::new(100, 100),
            adjustment: ConstraintAdjustment::FLIP_X | ConstraintAdjustment::FLIP_Y,
        };
        let r = p.solve().unwrap();
        assert_eq!((r.x, r.y), (700 - 100, 500 - 100));

        // A shorter screen: below overflows the bottom, above overflows the
        // top. y keeps its original position, while x still flips.
        let short = Positioner {
            size: Size::new(100, 160),
            anchor_rect: Rect::new(700, 150, 80, 40),
            constraint: Rect::new(0, 0, 800, 300),
            ..p
        };
        let r = short.solve().unwrap();
        // x: 780..880 overflows the right by 80, flipped 600..700 fits.
        assert_eq!(r.x, 600);
        // y: 190..350 overflows the bottom by 50; above, 150 - 160 = -10 does not fit.
        assert_eq!(r.y, 190);
    }

    #[test]
    fn a_popup_larger_than_the_area_is_not_flipped() {
        // Both edges are outside, so flipping cannot help.
        let p = Positioner {
            constraint: Rect::new(0, 0, 800, 100),
            ..below(
                Rect::new(10, 40, 100, 20),
                100,
                150,
                ConstraintAdjustment::FLIP_Y,
            )
        };
        assert_eq!(p.solve().unwrap().y, 60);
    }

    #[test]
    fn a_popup_past_the_right_edge_slides_left_until_its_edge_touches() {
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(750, 100, 40, 20),
            anchor: Anchor::BottomLeft,
            gravity: Anchor::BottomRight,
            offset: (0, 0),
            size: Size::new(200, 100),
            adjustment: ConstraintAdjustment::SLIDE_X,
        };
        assert_eq!(p.geometry().unwrap().x, 750);
        assert_eq!(p.solve().unwrap().x, 600);
    }

    #[test]
    fn a_popup_past_the_left_edge_slides_right_until_its_edge_touches() {
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(5, 100, 40, 20),
            anchor: Anchor::BottomRight,
            gravity: Anchor::BottomLeft,
            offset: (0, 0),
            size: Size::new(200, 100),
            adjustment: ConstraintAdjustment::SLIDE_X,
        };
        assert_eq!(p.geometry().unwrap().x, 45 - 200);
        assert_eq!(p.solve().unwrap().x, 0);
    }

    #[test]
    fn sliding_vertically_keeps_a_menu_inside_the_bottom_of_the_screen() {
        let p = below(
            Rect::new(10, 570, 100, 30),
            200,
            100,
            ConstraintAdjustment::SLIDE_Y,
        );
        assert_eq!(p.solve().unwrap().y, 500);
    }

    #[test]
    fn a_slide_moves_only_the_axis_that_is_constrained_and_allowed() {
        // Constrained on both axes, only x may slide.
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(750, 570, 40, 30),
            anchor: Anchor::BottomLeft,
            gravity: Anchor::BottomRight,
            offset: (0, 0),
            size: Size::new(100, 100),
            adjustment: ConstraintAdjustment::SLIDE_X,
        };
        let r = p.solve().unwrap();
        assert_eq!((r.x, r.y), (700, 600));
    }

    #[test]
    fn a_popup_bigger_than_the_area_slides_toward_its_gravity() {
        // 1000 wide in an 800 wide area: both edges are outside. Gravity right
        // slides it right, so the left edge touches the left of the area...
        let right = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(100, 100, 40, 20),
            anchor: Anchor::BottomLeft,
            gravity: Anchor::BottomRight,
            offset: (-150, 0),
            size: Size::new(1000, 50),
            adjustment: ConstraintAdjustment::SLIDE_X,
        };
        assert_eq!(right.solve().unwrap().x, 0);
        // ... and gravity left slides it left, so the right edge touches.
        let left = Positioner {
            gravity: Anchor::BottomLeft,
            offset: (150, 0),
            anchor: Anchor::BottomRight,
            anchor_rect: Rect::new(60, 100, 40, 20),
            ..right
        };
        assert_eq!(left.solve().unwrap().x, 800 - 1000);
    }

    #[test]
    fn with_no_gravity_a_too_large_popup_slides_right_or_down() {
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(400, 300, 10, 10),
            anchor: Anchor::None,
            gravity: Anchor::None,
            offset: (0, 0),
            size: Size::new(1000, 700),
            adjustment: ConstraintAdjustment::SLIDE_X | ConstraintAdjustment::SLIDE_Y,
        };
        let r = p.solve().unwrap();
        assert_eq!((r.x, r.y), (0, 0));
    }

    #[test]
    fn a_resize_clips_the_popup_to_the_area() {
        let p = below(
            Rect::new(10, 500, 100, 30),
            200,
            300,
            ConstraintAdjustment::RESIZE_Y,
        );
        let r = p.solve().unwrap();
        assert_eq!((r.y, r.height), (530, 70));
        assert_eq!((r.x, r.width), (-40, 200));
    }

    #[test]
    fn a_resize_on_the_near_side_moves_the_origin_in_and_shrinks_the_size() {
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(20, 100, 40, 20),
            anchor: Anchor::BottomRight,
            gravity: Anchor::BottomLeft,
            offset: (0, 0),
            size: Size::new(200, 50),
            adjustment: ConstraintAdjustment::RESIZE_X,
        };
        // Starts at 60 - 200 = -140: clipped to the left edge of the area.
        let r = p.solve().unwrap();
        assert_eq!((r.x, r.width), (0, 60));
    }

    #[test]
    fn a_resize_that_would_leave_nothing_changes_nothing() {
        // Entirely below the screen: clipping would leave a negative height.
        let p = Positioner {
            constraint: Rect::new(0, 0, 800, 100),
            ..below(
                Rect::new(10, 300, 100, 30),
                200,
                50,
                ConstraintAdjustment::RESIZE_Y,
            )
        };
        assert_eq!(p.solve().unwrap(), p.geometry().unwrap());
    }

    #[test]
    fn the_order_is_flip_then_slide_then_resize() {
        let all = ConstraintAdjustment::FLIP_Y
            | ConstraintAdjustment::SLIDE_Y
            | ConstraintAdjustment::RESIZE_Y;
        // Flip fits.
        let flips = below(Rect::new(10, 570, 100, 30), 200, 100, all);
        assert_eq!(flips.solve().unwrap().y, 470);
        // Flip does not fit, slide does: the popup keeps its size.
        let slides = Positioner {
            constraint: Rect::new(0, 0, 800, 300),
            ..below(Rect::new(10, 140, 100, 30), 100, 200, all)
        };
        let r = slides.solve().unwrap();
        assert_eq!((r.y, r.height), (100, 200));
        // Nothing fits, so what is left is clipped: 400 tall in 300.
        let resizes = Positioner {
            constraint: Rect::new(0, 0, 800, 300),
            ..below(Rect::new(10, 140, 100, 30), 100, 400, all)
        };
        let r = resizes.solve().unwrap();
        assert_eq!((r.y, r.height), (0, 300));
    }

    #[test]
    fn a_slide_that_does_not_finish_still_hands_what_it_did_to_the_resize() {
        // 1000 wide in 800 after sliding: the resize clips what is left.
        let p = Positioner {
            constraint: SCREEN,
            anchor_rect: Rect::new(100, 100, 40, 20),
            anchor: Anchor::BottomLeft,
            gravity: Anchor::BottomRight,
            offset: (-150, 0),
            size: Size::new(1000, 50),
            adjustment: ConstraintAdjustment::SLIDE_X | ConstraintAdjustment::RESIZE_X,
        };
        let r = p.solve().unwrap();
        assert_eq!((r.x, r.width), (0, 800));
    }

    #[test]
    fn a_popup_may_sit_in_a_constraint_area_that_does_not_start_at_the_origin() {
        // A bar at the bottom of a second screen: the screen as the bar sees it.
        let p = Positioner {
            constraint: Rect::new(-1920, -1040, 1920, 1080),
            anchor_rect: Rect::new(10, 0, 100, 40),
            anchor: Anchor::Bottom,
            gravity: Anchor::Bottom,
            offset: (0, 0),
            size: Size::new(100, 100),
            adjustment: ConstraintAdjustment::FLIP_Y,
        };
        // Below the bar (y 40..140) is outside the screen (which ends at 40).
        let r = p.solve().unwrap();
        assert_eq!(r.y, -100);
    }

    #[test]
    fn a_size_that_is_not_positive_is_an_error() {
        let mut p = plain(Anchor::None, Anchor::None);
        p.size = Size::new(0, 10);
        assert_eq!(p.solve(), Err(PositionerError::InvalidSize));
        p.size = Size::new(10, -1);
        assert_eq!(p.geometry(), Err(PositionerError::InvalidSize));
    }

    #[test]
    fn an_anchor_rectangle_with_a_negative_size_or_no_width_is_an_error() {
        let mut p = plain(Anchor::None, Anchor::None);
        p.anchor_rect = Rect::new(0, 0, -1, 5);
        assert_eq!(p.solve(), Err(PositionerError::NegativeAnchorRect));
        p.anchor_rect = Rect::new(0, 0, 0, 5);
        assert_eq!(p.solve(), Err(PositionerError::EmptyAnchorRect));
        assert!(
            PositionerError::EmptyAnchorRect
                .to_string()
                .contains("width")
        );
    }

    #[test]
    fn an_anchor_rectangle_with_a_height_of_zero_is_a_point_along_that_axis() {
        // A context menu opens at a click: a rectangle as wide as a pixel.
        let p = Positioner {
            anchor_rect: Rect::new(50, 60, 1, 0),
            ..plain(Anchor::TopLeft, Anchor::BottomRight)
        };
        let r = p.solve().unwrap();
        assert_eq!((r.x, r.y), (50, 60));
    }
}
