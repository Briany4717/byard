//! The input region of a panel, as `wl_surface.set_input_region` takes it
//! (RFC-0068 §6).
//!
//! The interpreter works out which part of a panel takes the pointer and
//! says it with an [`InputRegion`]. [`plan`] turns that into what the backend
//! sends: no region (the whole surface), an empty one, or the rectangles to
//! add to a `wl_region`, clipped to the surface so the compositor is never
//! told about pixels the panel does not have.

use byard_core::surface::{InputRegion, Rect};

/// What to send for an input region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// `set_input_region(null)`: the whole surface takes the pointer.
    Whole,
    /// A `wl_region` holding these rectangles, as `(x, y, width, height)`.
    /// Empty means nothing takes the pointer.
    Rects(Vec<(i32, i32, i32, i32)>),
}

/// The plan for `region` on a surface of `size` logical pixels.
///
/// Rectangles are clipped to `0..size`, rectangles with no area after that are
/// dropped, and repeats are removed, keeping the interpreter's order.
pub(crate) fn plan(region: &InputRegion, size: (i32, i32)) -> Plan {
    match region {
        InputRegion::All => Plan::Whole,
        InputRegion::None => Plan::Rects(Vec::new()),
        InputRegion::Rects(rects) => {
            let mut out: Vec<(i32, i32, i32, i32)> = Vec::with_capacity(rects.len());
            for r in rects {
                if let Some(clipped) = clip(*r, size) {
                    if !out.contains(&clipped) {
                        out.push(clipped);
                    }
                }
            }
            Plan::Rects(out)
        }
    }
}

fn clip(r: Rect, size: (i32, i32)) -> Option<(i32, i32, i32, i32)> {
    let x0 = r.x.max(0);
    let y0 = r.y.max(0);
    let x1 = r.right().min(size.0);
    let y1 = r.bottom().min(size.1);
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_no_region_and_none_is_an_empty_one() {
        assert_eq!(plan(&InputRegion::All, (100, 40)), Plan::Whole);
        assert_eq!(plan(&InputRegion::None, (100, 40)), Plan::Rects(vec![]));
    }

    #[test]
    fn rectangles_become_a_region_in_the_order_given() {
        let region = InputRegion::Rects(vec![Rect::new(0, 0, 40, 40), Rect::new(60, 4, 30, 32)]);
        assert_eq!(
            plan(&region, (100, 40)),
            Plan::Rects(vec![(0, 0, 40, 40), (60, 4, 30, 32)])
        );
    }

    #[test]
    fn rectangles_are_clipped_to_the_surface_and_empty_ones_dropped() {
        let region = InputRegion::Rects(vec![
            Rect::new(-10, -10, 30, 30),
            Rect::new(90, 30, 50, 50),
            Rect::new(200, 0, 10, 10),
            Rect::new(5, 5, 0, 10),
        ]);
        assert_eq!(
            plan(&region, (100, 40)),
            Plan::Rects(vec![(0, 0, 20, 20), (90, 30, 10, 10)])
        );
    }

    #[test]
    fn rectangles_that_clip_to_the_same_one_are_sent_once() {
        let region =
            InputRegion::Rects(vec![Rect::new(0, 0, 100, 40), Rect::new(-5, -5, 200, 100)]);
        assert_eq!(plan(&region, (100, 40)), Plan::Rects(vec![(0, 0, 100, 40)]));
    }
}
