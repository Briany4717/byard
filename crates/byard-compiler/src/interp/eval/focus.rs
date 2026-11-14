//! Focus scopes, and focus that is asked for (RFC-0066 §1).
//!
//! An overlay that is modal, or whose content is a dialog or a menu, is a
//! **focus scope**: when it mounts, focus goes into it (to the element marked
//! `autofocus`, else to the first element Tab would reach), and when it goes,
//! focus goes back to what had it. The router owns where Tab can reach and what
//! Escape dismisses; this is the part that remembers where focus came from, and
//! it is only ever run for a frame that has a scope, an `autofocus` or a
//! `focused:` write to look at.

use super::{Interpreter, RenderNode};
use crate::interp::events::Scope;
use byard_core::access::AccessRole;

/// A scope that is mounted, and where focus goes back to when it is not.
pub(super) struct LiveScope {
    /// The overlay's identity.
    key: u32,
    /// The element that had focus when it mounted and the focusable elements
    /// around it, nearest first, by identity: what it returns to, in order of
    /// preference, if the element itself is gone.
    returns: Vec<u32>,
}

impl Interpreter {
    /// The layout index `uid` has in this frame's layout.
    fn index_of_uid(&self, uid: u32) -> Option<u32> {
        self.prev_elem_uids
            .iter()
            .find(|(_, u)| *u == uid)
            .map(|(i, _)| *i)
    }

    /// The identity of the element at layout index `index`.
    fn uid_of_index(&self, index: u32) -> Option<u32> {
        self.prev_elem_uids
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, u)| *u)
    }

    /// The element with focus and every focusable one around it, by identity,
    /// nearest first.
    fn focus_returns(&self, focused: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut node = self.atlas.node_at(focused);
        while let Some(n) = node {
            if let Some(index) = self.atlas.node_index(n) {
                if self.router.is_focusable_now(index) {
                    out.extend(self.uid_of_index(index));
                }
            }
            node = self.atlas.parent_node(n);
        }
        out
    }

    /// Whether an overlay is a focus scope: it is modal, or the first element
    /// of its content that has a role is a dialog or a menu (RFC-0066 §1).
    /// Returns `Some(is_menu)` for a scope.
    pub(super) fn overlay_scope(
        &mut self,
        modal: bool,
        children: &[RenderNode],
        pools: super::layout::Pools<'_>,
    ) -> Option<bool> {
        let first = self.first_role(children, pools);
        let menu = first == Some(AccessRole::Menu);
        (modal || menu || first == Some(AccessRole::Dialog)).then_some(menu)
    }

    /// The role of the first element in `nodes`, in tree order, that writes an
    /// `a11y_role`.
    pub(super) fn first_role(
        &mut self,
        nodes: &[RenderNode],
        pools: super::layout::Pools<'_>,
    ) -> Option<AccessRole> {
        for c in self.expand_concrete(nodes, pools) {
            let RenderNode::Box {
                attrs,
                children,
                env_snapshot,
                ..
            } = c.node
            else {
                continue;
            };
            if attrs.iter().any(|a| a.name.as_str() == "a11y_role") {
                let base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let role = self.declared_role(attrs);
                self.env.truncate(base);
                return role;
            }
            if let Some(role) = self.first_role(children, pools) {
                return Some(role);
            }
        }
        None
    }

    /// After the frame is drawn: what its focus scopes, its `autofocus` and its
    /// `focused:` writes ask of focus (RFC-0066 §1).
    ///
    /// A scope that has gone gives focus back, innermost first, to the element
    /// that had it, or the nearest focusable one around where it was, and
    /// failing that nowhere. It does so only if focus is not somewhere else the
    /// user put it: a press on the page that dismissed a dropdown has already
    /// moved it, and moving it back would undo that. A scope that has mounted
    /// takes focus in, and remembers where it came from.
    pub(super) fn settle_focus(&mut self) {
        let scopes: Vec<Scope> = self.router.scopes().to_vec();
        if scopes.is_empty()
            && self.live_scopes.is_empty()
            && self.router.autofocus_is_empty()
            && self.focus_requests.is_empty()
            && self.autofocused.is_empty()
        {
            return;
        }
        let visible = self.router.key_modality();
        let mut moved = false;

        // Scopes that have gone, innermost first.
        let mut gone = Vec::new();
        self.live_scopes.retain_mut(|l| {
            if scopes.iter().any(|s| s.key == l.key) {
                true
            } else {
                gone.push(std::mem::take(&mut l.returns));
                false
            }
        });
        for returns in gone.into_iter().rev() {
            let stranded = self
                .router
                .focused()
                .is_none_or(|f| !self.router.is_focusable_now(f));
            if !stranded {
                continue;
            }
            let target = returns
                .iter()
                .filter_map(|u| self.index_of_uid(*u))
                .find(|i| self.router.is_focusable_now(*i));
            self.router
                .move_focus(&mut self.ctx, target, visible && target.is_some());
            moved = true;
        }

        // Scopes that have mounted, outermost first.
        for scope in &scopes {
            if self.live_scopes.iter().any(|l| l.key == scope.key) {
                continue;
            }
            let returns = self
                .router
                .focused()
                .map(|f| self.focus_returns(f))
                .unwrap_or_default();
            self.live_scopes.push(LiveScope {
                key: scope.key,
                returns,
            });
            // With nothing in it to go to, focus is cleared, so the keys go to
            // the scope and not to the page behind it.
            let entry = self.router.scope_entry(scope);
            self.router
                .move_focus(&mut self.ctx, entry, visible && entry.is_some());
            moved = true;
        }

        // An `autofocus` outside any scope is given focus the first time it is
        // drawn, when nothing else has it.
        let mut page: Vec<u32> = Vec::new();
        for index in self.router.autofocus_outside(&scopes) {
            let Some(uid) = self.uid_of_index(index) else {
                continue;
            };
            if self.autofocused.insert(uid) && self.router.focused().is_none() {
                page.push(index);
            }
        }
        if let Some(&first) = page.first() {
            self.router.move_focus(&mut self.ctx, Some(first), visible);
            moved = true;
        }
        if !self.autofocused.is_empty() {
            let now: Vec<u32> = self
                .router
                .autofocus_outside(&scopes)
                .into_iter()
                .filter_map(|i| self.uid_of_index(i))
                .collect();
            self.autofocused.retain(|u| now.contains(u));
        }

        // A write to a bound `focused:`.
        for (index, wanted) in std::mem::take(&mut self.focus_requests) {
            if wanted {
                if self.router.focus_element(&mut self.ctx, index) {
                    moved = true;
                }
            } else if self.router.focused() == Some(index) {
                self.router.move_focus(&mut self.ctx, None, false);
                moved = true;
            }
        }

        // The frame that has just been drawn did not show the move; the next
        // one has to.
        if moved {
            self.any_active = true;
        }
    }
}
