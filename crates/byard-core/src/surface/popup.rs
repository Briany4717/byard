//! What a popup asks of the compositor (RFC-0068 §7).
//!
//! In a `Panel`, an `Overlay` whose first child has `anchor_to` is a popup: an
//! `xdg_popup` whose parent is the panel (or another popup, for a submenu). The
//! attributes RFC-0066 gives an anchored overlay (`anchor_edge`, `anchor_align`,
//! `anchor_gap`, `anchor_flip`) become an `xdg_positioner` here, as plain data:
//! the interpreter says what the popup wants without knowing Wayland, a host
//! sends it to whatever it has, and the headless screens solve it with
//! [`Positioner::solve`] against the screen the popup is on.

use super::positioner::{Anchor, ConstraintAdjustment, Gravity, Positioner};
use super::{Rect, Size, SurfaceId};

/// The side of the anchor an overlay sits on (`anchor_edge`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    /// Under the anchor: the default.
    #[default]
    Below,
    /// Over the anchor.
    Above,
    /// To the left of the anchor.
    Before,
    /// To the right of the anchor.
    After,
}

impl Side {
    /// The side `anchor_edge` names, `below` for anything else.
    #[must_use]
    pub fn from_word(word: &str) -> Self {
        match word {
            "above" => Self::Above,
            "before" => Self::Before,
            "after" => Self::After,
            _ => Self::Below,
        }
    }
}

/// How the overlay lines up along the anchor's other axis (`anchor_align`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// Near edges together: the default.
    #[default]
    Start,
    /// Midpoints together.
    Center,
    /// Far edges together.
    End,
}

impl Align {
    /// The alignment `anchor_align` names, `start` for anything else.
    #[must_use]
    pub fn from_word(word: &str) -> Self {
        match word {
            "center" => Self::Center,
            "end" => Self::End,
            _ => Self::Start,
        }
    }
}

/// The positioner data and the grab of a popup (the `popup` of a
/// [`SurfaceSpec`](super::SurfaceSpec)).
///
/// The popup's size is the spec's `size`: what its content measured, laid out
/// unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupSpec {
    /// The surface it hangs from: a panel, or the popup a submenu opens from.
    pub parent: SurfaceId,
    /// The rectangle of the parent it is placed against (`set_anchor_rect`):
    /// the bounds of the `anchor_to` element, in the parent's coordinates.
    pub anchor_rect: Rect,
    /// The point on the anchor rectangle it is placed at (`set_anchor`).
    pub anchor: Anchor,
    /// The side of that point it extends toward (`set_gravity`).
    pub gravity: Gravity,
    /// `anchor_gap`, as the protocol's offset (`set_offset`): already pointing
    /// away from the anchor, since the offset does not follow the gravity.
    pub offset: (i32, i32),
    /// What may be done when it does not fit (`set_constraint_adjustment`).
    pub adjustment: ConstraintAdjustment,
    /// Whether it is created with `xdg_popup.grab`: it takes the keyboard, and
    /// the compositor sends `popup_done` on a click outside it. True for a
    /// popup that is a focus scope (RFC-0066): modal, or holding a `menu` or a
    /// `dialog`.
    pub grab: bool,
}

impl PopupSpec {
    /// The positioner data of an overlay anchored to `anchor_rect` of `parent`
    /// by `anchor_edge`, `anchor_align` and `anchor_gap` (RFC-0068 §7).
    ///
    /// `below` is anchor `bottom` and gravity `bottom`, and `anchor_align` picks
    /// the corner on the other axis: `start` lines the near edges up (anchor
    /// bottom-left, gravity bottom-right), `end` the far ones, `center` the
    /// midpoints. The compositor, which knows the screen, does the flipping
    /// RFC-0066 asks for: `flip_y` for `below` and `above`, `flip_x` for
    /// `before` and `after`, with `slide` on both axes to keep it inside when
    /// the other side has no room either. `flip: false` (`anchor_flip`) asks for
    /// the slide alone.
    #[must_use]
    pub fn anchored(
        parent: SurfaceId,
        anchor_rect: Rect,
        side: Side,
        align: Align,
        gap: i32,
        flip: bool,
        grab: bool,
    ) -> Self {
        // The popup extends away from the anchor on the main axis; `a` and `g`
        // are the horizontal and vertical halves of the anchor point and of the
        // gravity (-1 toward the left or top, 1 toward the right or bottom).
        let (anchor, gravity) = {
            // Along the other axis: where on the anchor's edge, and which way
            // the popup extends from there.
            let (point, grow) = match align {
                Align::Start => (-1, 1),
                Align::Center => (0, 0),
                Align::End => (1, -1),
            };
            match side {
                Side::Below => (dir(point, 1), dir(grow, 1)),
                Side::Above => (dir(point, -1), dir(grow, -1)),
                Side::Before => (dir(-1, point), dir(-1, grow)),
                Side::After => (dir(1, point), dir(1, grow)),
            }
        };
        let offset = match side {
            Side::Below => (0, gap),
            Side::Above => (0, -gap),
            Side::Before => (-gap, 0),
            Side::After => (gap, 0),
        };
        let mut adjustment = ConstraintAdjustment::SLIDE_X | ConstraintAdjustment::SLIDE_Y;
        if flip {
            adjustment = adjustment
                | match side {
                    Side::Below | Side::Above => ConstraintAdjustment::FLIP_Y,
                    Side::Before | Side::After => ConstraintAdjustment::FLIP_X,
                };
        }
        Self {
            parent,
            anchor_rect,
            anchor,
            gravity,
            offset,
            adjustment,
            grab,
        }
    }

    /// The `xdg_positioner` of a popup of `size`, kept inside `constraint`: the
    /// screen as seen from the parent's origin, which is what a compositor
    /// constrains a popup to.
    #[must_use]
    pub fn positioner(&self, size: Size, constraint: Rect) -> Positioner {
        Positioner {
            constraint,
            anchor_rect: self.anchor_rect,
            anchor: self.anchor,
            gravity: self.gravity,
            offset: self.offset,
            size,
            adjustment: self.adjustment,
        }
    }
}

/// The anchor or gravity of a horizontal and a vertical direction.
const fn dir(x: i8, y: i8) -> Anchor {
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

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT: SurfaceId = SurfaceId::new(1);

    fn spec(side: Side, align: Align) -> PopupSpec {
        PopupSpec::anchored(
            PARENT,
            Rect::new(100, 300, 60, 20),
            side,
            align,
            8,
            true,
            false,
        )
    }

    /// Where a popup of 80x120 lands on a screen big enough to hold it.
    fn at(side: Side, align: Align) -> Rect {
        let screen = Rect::new(0, 0, 1000, 1000);
        spec(side, align)
            .positioner(Size::new(80, 120), screen)
            .solve()
            .unwrap()
    }

    #[test]
    fn below_is_the_anchor_bottom_with_gravity_bottom() {
        let p = spec(Side::Below, Align::Center);
        assert_eq!((p.anchor, p.gravity), (Anchor::Bottom, Anchor::Bottom));
        assert_eq!(p.offset, (0, 8));
        let p = spec(Side::Below, Align::Start);
        assert_eq!(
            (p.anchor, p.gravity),
            (Anchor::BottomLeft, Anchor::BottomRight)
        );
        let p = spec(Side::Below, Align::End);
        assert_eq!(
            (p.anchor, p.gravity),
            (Anchor::BottomRight, Anchor::BottomLeft)
        );
    }

    #[test]
    fn every_side_and_align_lands_where_the_overlay_pass_puts_it() {
        // The anchor is x 100..160, y 300..320; the popup is 80 wide and 120
        // tall, eight pixels away.
        assert_eq!(at(Side::Below, Align::Start), Rect::new(100, 328, 80, 120));
        assert_eq!(at(Side::Below, Align::Center), Rect::new(90, 328, 80, 120));
        assert_eq!(at(Side::Below, Align::End), Rect::new(80, 328, 80, 120));
        assert_eq!(at(Side::Above, Align::Start), Rect::new(100, 172, 80, 120));
        assert_eq!(at(Side::Above, Align::Center), Rect::new(90, 172, 80, 120));
        assert_eq!(at(Side::Above, Align::End), Rect::new(80, 172, 80, 120));
        assert_eq!(at(Side::After, Align::Start), Rect::new(168, 300, 80, 120));
        assert_eq!(at(Side::After, Align::Center), Rect::new(168, 250, 80, 120));
        assert_eq!(at(Side::After, Align::End), Rect::new(168, 200, 80, 120));
        assert_eq!(at(Side::Before, Align::Start), Rect::new(12, 300, 80, 120));
        assert_eq!(at(Side::Before, Align::Center), Rect::new(12, 250, 80, 120));
        assert_eq!(at(Side::Before, Align::End), Rect::new(12, 200, 80, 120));
    }

    #[test]
    fn the_gap_is_away_from_the_anchor_on_every_side() {
        assert_eq!(spec(Side::Below, Align::Start).offset, (0, 8));
        assert_eq!(spec(Side::Above, Align::Start).offset, (0, -8));
        assert_eq!(spec(Side::Before, Align::Start).offset, (-8, 0));
        assert_eq!(spec(Side::After, Align::Start).offset, (8, 0));
    }

    #[test]
    fn a_popup_flips_on_the_axis_it_leaves_the_anchor_by_and_slides_on_both() {
        let flags = |side| spec(side, Align::Start).adjustment;
        for side in [Side::Below, Side::Above] {
            assert!(flags(side).contains(ConstraintAdjustment::FLIP_Y));
            assert!(!flags(side).contains(ConstraintAdjustment::FLIP_X));
        }
        for side in [Side::Before, Side::After] {
            assert!(flags(side).contains(ConstraintAdjustment::FLIP_X));
            assert!(!flags(side).contains(ConstraintAdjustment::FLIP_Y));
        }
        for side in [Side::Below, Side::Above, Side::Before, Side::After] {
            assert!(flags(side).contains(ConstraintAdjustment::SLIDE_X));
            assert!(flags(side).contains(ConstraintAdjustment::SLIDE_Y));
        }
    }

    #[test]
    fn anchor_flip_false_leaves_the_slide_alone() {
        let p = PopupSpec::anchored(
            PARENT,
            Rect::new(0, 0, 10, 10),
            Side::Below,
            Align::Start,
            0,
            false,
            false,
        );
        assert!(!p.adjustment.contains(ConstraintAdjustment::FLIP_Y));
        assert!(p.adjustment.contains(ConstraintAdjustment::SLIDE_Y));
    }

    #[test]
    fn words_that_are_not_a_side_or_an_alignment_are_the_defaults() {
        assert_eq!(Side::from_word("above"), Side::Above);
        assert_eq!(Side::from_word("after"), Side::After);
        assert_eq!(Side::from_word("sideways"), Side::Below);
        assert_eq!(Align::from_word("end"), Align::End);
        assert_eq!(Align::from_word("middle"), Align::Start);
    }

    #[test]
    fn a_menu_below_a_bar_at_the_bottom_of_the_screen_is_flipped_above_it() {
        // A 40 px bar at the bottom of a 800 px screen, its button at y 765..795:
        // a 200 px menu does not fit below, and does above.
        let p = PopupSpec::anchored(
            PARENT,
            Rect::new(10, 5, 80, 30),
            Side::Below,
            Align::Start,
            4,
            true,
            true,
        );
        // The screen as seen from the bar, whose top is at y 760.
        let screen = Rect::new(0, -760, 1280, 800);
        let r = p.positioner(Size::new(120, 200), screen).solve().unwrap();
        // A flip inverts the anchor and the gravity and keeps the offset (as wlroots
        // does), so the gap is a 4 px overlap above the button.
        assert_eq!(r, Rect::new(10, -191, 120, 200));
    }
}
