//! Keeping what belongs to an element with the element (RFC-0041 §1).
//!
//! A layout index is a place in the build order, so a `when` or `for` that
//! grows before an element moves its index by the number of nodes it added.
//! The element is the same one, and what the engine holds for it goes with it:
//! focus, a field's caret, selection and undo, an IME composition, a
//! `ScrollView`'s momentum, snap and pull state, the pointer's press, drag and
//! hover. All of it is keyed by index, and this is the one place that re-keys it.

use std::collections::HashMap;

use super::Interpreter;

/// Moves every entry of `map` to the index `to` gives its element, and drops
/// the ones whose element is gone.
fn rekey<V>(map: &mut HashMap<u32, V>, to: &impl Fn(u32) -> Option<u32>) {
    if map.is_empty() {
        return;
    }
    let old = std::mem::take(map);
    *map = old
        .into_iter()
        .filter_map(|(elem, v)| Some((to(elem)?, v)))
        .collect();
}

impl Interpreter {
    /// Whether anything is keyed by an element index right now. A frame with
    /// nothing focused, scrolled, pressed, hovered or edited has nothing to
    /// follow, and does not even compare the two layouts.
    fn holds_elements(&self) -> bool {
        self.router.holds_elements()
            || !self.edit.borrow().is_empty()
            || self.composition.is_some()
            || self.blink.is_some()
            || self.scroll_drag.is_some()
            || !self.ripples.is_empty()
            || !(self.scroll_page_last.is_empty()
                && self.scroll_quiet.is_empty()
                && self.snap_anims.is_empty()
                && self.pull_distance.is_empty()
                && self.pull_anims.is_empty()
                && self.refreshing_seen.is_empty()
                && self.scroll_vel.is_empty()
                && self.scroll_vel_last.is_empty()
                && self.end_reached_fired.is_empty())
    }

    /// After a layout, finds again the elements that engine state belongs to.
    ///
    /// Run on every layout, the retained one too, since that walk records the
    /// same table. When nothing holds state, or the layout is the one last
    /// frame had, it costs a check and at most one comparison.
    pub(super) fn follow_elements(&mut self) {
        if self.holds_elements() && self.elem_uids != self.prev_elem_uids {
            let prev = std::mem::take(&mut self.prev_elem_uids);
            let now = std::mem::take(&mut self.elem_uids);
            // An index the last frame did not record is not an element that
            // can have moved: it stays where it is.
            let to = |old: u32| -> Option<u32> {
                let Some(&(_, uid)) = prev.iter().find(|p| p.0 == old) else {
                    return Some(old);
                };
                now.iter().find(|n| n.1 == uid).map(|n| n.0)
            };
            if let Some(old) = self.router.focused() {
                self.router.follow_focus(&mut self.ctx, to(old));
            }
            self.router.follow_pointer(&to);
            self.edit.borrow_mut().rekey(&to);
            if let Some(c) = &mut self.composition {
                match to(c.owner()) {
                    Some(new) => c.set_owner(new),
                    None => self.composition = None,
                }
            }
            if let Some(b) = &mut self.blink {
                match to(b.owner()) {
                    Some(new) => b.set_owner(new),
                    None => self.stop_blink(),
                }
            }
            if let Some(d) = &mut self.scroll_drag {
                d.elem = d.elem.and_then(to);
            }
            self.ripples.retain_mut(|r| match to(r.elem()) {
                Some(new) => {
                    r.set_elem(new);
                    true
                }
                None => false,
            });
            if let Some((e, t)) = self.ripple_spawned {
                self.ripple_spawned = to(e).map(|n| (n, t));
            }
            rekey(&mut self.scroll_page_last, &to);
            rekey(&mut self.scroll_quiet, &to);
            rekey(&mut self.snap_anims, &to);
            rekey(&mut self.pull_distance, &to);
            rekey(&mut self.pull_anims, &to);
            rekey(&mut self.refreshing_seen, &to);
            rekey(&mut self.scroll_vel, &to);
            rekey(&mut self.scroll_vel_last, &to);
            self.end_reached_fired = std::mem::take(&mut self.end_reached_fired)
                .into_iter()
                .filter_map(to)
                .collect();
            self.elem_uids = now;
            self.prev_elem_uids = prev;
        }
        std::mem::swap(&mut self.prev_elem_uids, &mut self.elem_uids);
    }
}
