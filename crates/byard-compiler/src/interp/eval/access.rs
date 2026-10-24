//! Building the accessibility tree during the paint walk (RFC-0042).
//!
//! The paint walk already visits every element with its resolved rect, its
//! composed transform and the router state its handlers just registered, which
//! is everything a node needs. So the tree is built *by* the walk, behind one
//! flag, rather than by a second traversal that would have to reproduce the
//! walk's expansion of `when` and `for`, its windowed lists, its navigation
//! screens and its overlays, and keep doing so.
//!
//! **Nothing here runs unless the frame asked.** [`Interpreter::access_begin`]
//! reads the frame's flag once; every other entry point starts with the same
//! plain `bool` and returns. The scratch state is `Vec`s that stay empty (and
//! so unallocated) until a frame with a tree first pushes into them.
//!
//! What the walk does not visit is not in the tree: an element a `ScrollView`
//! has scrolled entirely out of view is culled before it is painted
//! (RFC-0005), and so before it is described.

use byard_core::access::{
    AccessActions, AccessFlags, AccessId, AccessNode, AccessRole, AccessValue,
};
use byard_core::frame::{Rect as FrameRect, RenderFrame, Transform};

use super::{Attr, Interpreter, Pools, RenderNode, SignalId, Value};
use crate::interp::events::EventKind as RouterKind;
use crate::interp::intrinsics::Rect;

/// The per-frame bookkeeping of a tree being built.
#[derive(Default)]
pub(super) struct AccessScratch {
    /// Whether the frame being walked is building a tree.
    pub(super) on: bool,
    /// How many `a11y_hidden` elements enclose the walk right now; while it
    /// is above zero nothing is described.
    hidden: u32,
    /// How many enclosing `ScrollView`s can be scrolled by a write, so a node
    /// inside one can be asked to scroll into view.
    scrolled: u32,
    /// The nodes with a role that enclose the walk, innermost last.
    open: Vec<Open>,
    /// The layout containers that enclose the walk, innermost last.
    containers: Vec<Container>,
    /// For each node of the tree, how many containers enclosed it, so the
    /// "label beside it" rule can tell a `Row`'s own children from those of a
    /// container nested inside it.
    owners: Vec<u32>,
}

/// A node with a role whose subtree is being walked.
struct Open {
    /// Its index in the tree's `nodes`.
    node: usize,
    /// Whether the text inside names it (a button).
    from_content: bool,
    /// The text found inside so far, joined with a space.
    text: String,
    /// Whether it raised [`AccessScratch::scrolled`].
    scroller: bool,
}

/// A layout container whose subtree is being walked.
struct Container {
    /// Whether it is a `Row`, the only place a control is named by the label
    /// beside it.
    row: bool,
    /// How many nodes the tree held when the walk entered it.
    first: usize,
}

/// What [`Interpreter::access_enter`] opened, to close on the way out.
#[derive(Clone, Copy, Default)]
pub(super) struct Scope {
    /// An `a11y_hidden` element was entered.
    hidden: bool,
    /// A layout container was entered.
    container: bool,
    /// A node with a role was entered.
    node: bool,
}

/// Everything about a box the walk knows when it reaches it.
pub(super) struct BoxFacts<'a> {
    /// The intrinsic's name.
    pub name: &'a str,
    /// The attributes as written.
    pub attrs: &'a [Attr],
    /// The attributes with the active state blocks applied.
    pub paint_attrs: &'a [Attr],
    /// The `var` a value widget is bound to.
    pub bound_sig: Option<SignalId>,
    /// Whether the element has a tap handler of its own.
    pub tapped: bool,
    /// The element's index in the lowered tree.
    pub elem: Option<u32>,
    /// Where layout put it.
    pub rect: Rect,
    /// The atlas node, for the `ScrollView`'s content size.
    pub atlas_node: byard_core::atlas::layout::AtlasNodeId,
    /// The transform its subtree is painted under.
    pub transform: Transform,
}

/// The role an intrinsic has, or `None` for a layout container, which is
/// flattened away (RFC-0042 §2).
///
/// An element that has a tap handler of its own is a button whatever it is
/// called: it is something the user can press, and a screen reader that could
/// not say so would hide it.
fn role_of(name: &str, tapped: bool) -> Option<AccessRole> {
    match name {
        "Button" => Some(AccessRole::Button),
        "TextField" => Some(AccessRole::TextInput),
        "Toggle" => Some(AccessRole::Switch),
        "Checkbox" => Some(AccessRole::CheckBox),
        "RadioButton" => Some(AccessRole::RadioButton),
        "Slider" => Some(AccessRole::Slider),
        "ScrollView" => Some(AccessRole::ScrollView),
        _ => tapped.then_some(AccessRole::Button),
    }
}

/// A layout rect as it is painted: through the transform, in viewport space.
fn bounds(rect: Rect, t: &Transform) -> FrameRect {
    let tl = t.apply_point([rect.x, rect.y]);
    FrameRect::new(tl[0], tl[1], rect.w * t.scale[0], rect.h * t.scale[1])
}

impl Interpreter {
    /// How much room the interpreter's accessibility scratch holds, in
    /// elements. Zero until a frame has asked for a tree: what "a frame with
    /// the flag off allocates nothing extra" is asserted against.
    #[must_use]
    pub fn access_scratch_capacity(&self) -> usize {
        let scratch = &self.access;
        scratch.open.capacity() + scratch.containers.capacity() + scratch.owners.capacity()
    }

    /// Starts a frame's tree if the frame asked for one (RFC-0042 §1).
    pub(super) fn access_begin(&mut self, frame: &mut RenderFrame) {
        self.access.on = frame.access_wanted();
        if !self.access.on {
            return;
        }
        frame.begin_access();
        let scratch = &mut self.access;
        scratch.hidden = 0;
        scratch.scrolled = 0;
        scratch.open.clear();
        scratch.containers.clear();
        scratch.owners.clear();
    }

    /// Finishes a frame's tree: records which node has focus.
    pub(super) fn access_end(&mut self, frame: &mut RenderFrame) {
        if !std::mem::take(&mut self.access.on) {
            return;
        }
        let focused = self.router.focused().map(AccessId);
        if let Some(tree) = frame.access_mut() {
            tree.focus = focused.filter(|id| tree.node(*id).is_some());
        }
    }

    /// Enters a box: opens the node its role gives it, if it has one, and a
    /// layout container for the "label beside it" rule.
    pub(super) fn access_enter_box(&mut self, frame: &mut RenderFrame, b: &BoxFacts<'_>) -> Scope {
        if !self.access.on || self.access.hidden > 0 {
            return Scope::default();
        }
        if self.eval_bool_prop(b.attrs, "a11y_hidden") == Some(true) {
            self.access.hidden += 1;
            return Scope {
                hidden: true,
                ..Scope::default()
            };
        }
        let node = match (b.elem, role_of(b.name, b.tapped)) {
            (Some(elem), Some(role)) => Some(self.box_node(role, elem, b)),
            _ => None,
        };
        let scroller = node
            .as_ref()
            .is_some_and(|n| n.role == AccessRole::ScrollView)
            && {
                let (x, y) = self.resolve_offset_sigs(b.attrs);
                x.is_some() || y.is_some()
            };
        let from_content = node.as_ref().is_some_and(|n| n.role.names_from_content());
        self.open_scope(frame, node, b.name == "Row", from_content, scroller)
    }

    /// Leaves what [`access_enter_box`](Self::access_enter_box) (or another
    /// `access_enter_*`) opened.
    pub(super) fn access_leave(&mut self, frame: &mut RenderFrame, scope: Scope) {
        if scope.hidden {
            self.access.hidden -= 1;
            return;
        }
        if scope.node {
            if let Some(open) = self.access.open.pop() {
                if open.scroller {
                    self.access.scrolled -= 1;
                }
                if let Some(tree) = frame.access_mut() {
                    let node = &mut tree.nodes[open.node];
                    if open.from_content && node.name.is_none() && !open.text.is_empty() {
                        node.name = Some(open.text);
                    }
                }
            }
        }
        if scope.container {
            if let Some(container) = self.access.containers.pop() {
                if container.row {
                    self.name_from_label_beside(frame, container.first);
                }
            }
        }
    }

    /// Reads the text of a subtree the scroll culled, for the button it is in.
    ///
    /// A row scrolled half out of view is still a control the user can press,
    /// and its label may be exactly the part that is out of view. The walk
    /// skips what is entirely clipped, so without this the button would be
    /// described as having no name. Nothing is emitted for the subtree; only
    /// its words, and only when the button around it is named by its content.
    pub(super) fn access_culled(&mut self, node: &RenderNode, pools: Pools<'_>) {
        if !self.access.on || self.access.hidden > 0 {
            return;
        }
        if !self.access.open.last().is_some_and(|o| o.from_content) {
            return;
        }
        match node {
            RenderNode::Text { attrs, content, .. } => {
                if self.eval_bool_prop(attrs, "a11y_hidden") == Some(true) {
                    return;
                }
                let text = match self.binding_value(*content) {
                    Some(Value::Str(s)) => s,
                    other => other.map_or_else(String::new, |v| format!("{v:?}")),
                };
                if let Some(open) = self.access.open.last_mut() {
                    if text.is_empty() {
                        return;
                    }
                    if !open.text.is_empty() {
                        open.text.push(' ');
                    }
                    open.text.push_str(&text);
                }
            }
            RenderNode::Box {
                attrs, children, ..
            } => {
                if self.eval_bool_prop(attrs, "a11y_hidden") == Some(true) {
                    return;
                }
                for child in self.expand_concrete(children, pools) {
                    self.access_culled(child.node, pools);
                }
            }
            _ => {}
        }
    }

    /// Records a `Text`: as a word of its control's name when it is inside a
    /// button, otherwise as a label of its own.
    pub(super) fn access_text(
        &mut self,
        frame: &mut RenderFrame,
        attrs: &[Attr],
        elem: Option<u32>,
        text: &str,
        rect: Rect,
        transform: &Transform,
    ) {
        if !self.access.on || self.access.hidden > 0 || text.is_empty() {
            return;
        }
        if self.eval_bool_prop(attrs, "a11y_hidden") == Some(true) {
            return;
        }
        if let Some(open) = self.access.open.last_mut().filter(|o| o.from_content) {
            if !open.text.is_empty() {
                open.text.push(' ');
            }
            open.text.push_str(text);
            return;
        }
        let Some(elem) = elem else { return };
        let name = self.access_label(attrs).unwrap_or_else(|| text.to_string());
        let node = self.leaf(AccessRole::Label, elem, Some(name), bounds(rect, transform));
        self.push_leaf(frame, node);
    }

    /// Records an `Image`, a `VectorIcon` or a `Canvas`. The first two are
    /// images whatever they carry; a `Canvas` draws shapes and only has a
    /// place in the tree when the author named it.
    pub(super) fn access_image(
        &mut self,
        frame: &mut RenderFrame,
        attrs: &[Attr],
        elem: Option<u32>,
        rect: Rect,
        transform: &Transform,
        is_canvas: bool,
    ) {
        if !self.access.on || self.access.hidden > 0 {
            return;
        }
        let Some(elem) = elem else { return };
        if self.eval_bool_prop(attrs, "a11y_hidden") == Some(true) {
            return;
        }
        let name = self.access_label(attrs);
        if is_canvas && name.is_none() {
            return;
        }
        let node = self.leaf(AccessRole::Image, elem, name, bounds(rect, transform));
        self.push_leaf(frame, node);
    }

    /// Records a native view (RFC-0039) with the role its catalog entry
    /// declares, a group unless it says otherwise.
    pub(super) fn access_native(
        &mut self,
        frame: &mut RenderFrame,
        name: &str,
        attrs: &[Attr],
        elem: Option<u32>,
        rect: Rect,
        transform: &Transform,
    ) {
        if !self.access.on || self.access.hidden > 0 {
            return;
        }
        let Some(elem) = elem else { return };
        if self.eval_bool_prop(attrs, "a11y_hidden") == Some(true) {
            return;
        }
        let role = byard_core::render::registry::access_role(name).unwrap_or(AccessRole::Group);
        let label = self.access_label(attrs);
        let mut node = self.leaf(role, elem, label, bounds(rect, transform));
        self.add_interaction(&mut node, elem);
        self.push_leaf(frame, node);
    }

    /// Enters a `NavHost`: a tab list whose tabs are the screens alive.
    pub(super) fn access_enter_tabs(
        &mut self,
        frame: &mut RenderFrame,
        attrs: &[Attr],
        elem: Option<u32>,
        rect: Rect,
        transform: &Transform,
    ) -> Scope {
        if !self.access.on || self.access.hidden > 0 {
            return Scope::default();
        }
        if self.eval_bool_prop(attrs, "a11y_hidden") == Some(true) {
            self.access.hidden += 1;
            return Scope {
                hidden: true,
                ..Scope::default()
            };
        }
        let node = elem.map(|elem| {
            let label = self.access_label(attrs);
            self.leaf(AccessRole::TabList, elem, label, bounds(rect, transform))
        });
        self.open_scope(frame, node, false, false, false)
    }

    /// Enters one screen of a `NavHost` as a tab: named for the tab it is,
    /// selected when it is the one the navigation state names.
    pub(super) fn access_enter_tab(
        &mut self,
        frame: &mut RenderFrame,
        name: &str,
        selected: bool,
        elem: Option<u32>,
        rect: Rect,
        transform: &Transform,
    ) -> Scope {
        if !self.access.on || self.access.hidden > 0 {
            return Scope::default();
        }
        let node = elem.map(|elem| {
            let mut node = self.leaf(
                AccessRole::Tab,
                elem,
                Some(name.to_string()),
                bounds(rect, transform),
            );
            if selected {
                node.flags.insert(AccessFlags::SELECTED);
            }
            node
        });
        self.open_scope(frame, node, false, false, false)
    }

    /// Enters an `Overlay`: a dialog when it is modal, and otherwise no node
    /// at all, its content simply joining the tree where it floats.
    pub(super) fn access_enter_overlay(
        &mut self,
        frame: &mut RenderFrame,
        attrs: &[Attr],
        modal: bool,
        elem: Option<u32>,
        viewport: Rect,
    ) -> Scope {
        if !self.access.on || self.access.hidden > 0 {
            return Scope::default();
        }
        if self.eval_bool_prop(attrs, "a11y_hidden") == Some(true) {
            self.access.hidden += 1;
            return Scope {
                hidden: true,
                ..Scope::default()
            };
        }
        let node = elem.filter(|_| modal).map(|elem| {
            let label = self.access_label(attrs);
            let mut node = self.leaf(
                AccessRole::Dialog,
                elem,
                label,
                bounds(viewport, &Transform::IDENTITY),
            );
            node.flags.insert(AccessFlags::MODAL);
            node
        });
        self.open_scope(frame, node, false, false, false)
    }

    /// Scrolls the innermost scroll view under a node into the position that
    /// shows the node, as far as it can (RFC-0042 `ScrollIntoView`).
    ///
    /// `at` and `size` are where the node is, in viewport space, as its last
    /// tree said.
    pub(super) fn scroll_into_view(&mut self, at: (f32, f32), size: (f32, f32)) {
        let target = Rect::new(at.0, at.1, size.0, size.1);
        let Some(view) = self
            .scroll_targets
            .iter()
            .rev()
            .find(|t| {
                at.0 < t.rect.x + t.rect.w
                    && at.0 + size.0 > t.rect.x
                    && at.1 < t.rect.y + t.rect.h
                    && at.1 + size.1 > t.rect.y
            })
            .copied()
        else {
            return;
        };
        // How far the node pokes out past each edge: a positive nudge shows
        // earlier content, so a node below the view is a negative one.
        let shift = |lo: f32, hi: f32, view_lo: f32, view_hi: f32| {
            if lo < view_lo || hi - lo > view_hi - view_lo {
                view_lo - lo
            } else if hi > view_hi {
                view_hi - hi
            } else {
                0.0
            }
        };
        if let Some(axis) = view.x {
            let by = shift(
                target.x,
                target.x + target.w,
                view.rect.x,
                view.rect.x + view.rect.w,
            );
            if by != 0.0 {
                self.nudge_scroll(axis, by);
            }
        }
        if let Some(axis) = view.y {
            let by = shift(
                target.y,
                target.y + target.h,
                view.rect.y,
                view.rect.y + view.rect.h,
            );
            if by != 0.0 {
                self.nudge_scroll(axis, by);
            }
        }
    }

    // ── internals ───────────────────────────────────────────────────────

    /// The `a11y_label:` an element carries, if it says anything.
    fn access_label(&mut self, attrs: &[Attr]) -> Option<String> {
        self.eval_str_prop(attrs, "a11y_label")
            .filter(|s| !s.is_empty())
    }

    /// A node with no value, state or action yet.
    fn leaf(
        &self,
        role: AccessRole,
        elem: u32,
        name: Option<String>,
        bounds: FrameRect,
    ) -> AccessNode {
        AccessNode {
            id: AccessId(elem),
            parent: None,
            role,
            name,
            description: None,
            value: None,
            bounds,
            flags: AccessFlags::empty(),
            actions: if self.access.scrolled > 0 {
                AccessActions::SCROLL_INTO_VIEW
            } else {
                AccessActions::empty()
            },
        }
    }

    /// Pushes a node that encloses nothing the walk will visit.
    fn push_leaf(&mut self, frame: &mut RenderFrame, node: AccessNode) {
        let _ = self.push_node(frame, node);
    }

    /// Appends `node` to the tree under the innermost open node.
    fn push_node(&mut self, frame: &mut RenderFrame, mut node: AccessNode) -> Option<usize> {
        let tree = frame.access_mut()?;
        node.parent = self.access.open.last().map(|o| tree.nodes[o.node].id);
        let index = tree.nodes.len();
        tree.nodes.push(node);
        self.access
            .owners
            .push(u32::try_from(self.access.containers.len()).unwrap_or(u32::MAX));
        Some(index)
    }

    /// Opens a scope: the node (if any) and a container around the walk of
    /// its children.
    fn open_scope(
        &mut self,
        frame: &mut RenderFrame,
        node: Option<AccessNode>,
        row: bool,
        from_content: bool,
        scroller: bool,
    ) -> Scope {
        let first = frame.access().map_or(0, |t| t.nodes.len());
        // The node belongs to the container outside it, so it is pushed
        // before this scope's own container is.
        let mut scope = Scope::default();
        if let Some(node) = node {
            if let Some(index) = self.push_node(frame, node) {
                if scroller {
                    self.access.scrolled += 1;
                }
                self.access.open.push(Open {
                    node: index,
                    from_content,
                    text: String::new(),
                    scroller,
                });
                scope.node = true;
            }
        }
        self.access.containers.push(Container { row, first });
        scope.container = true;
        scope
    }

    /// What a control the router has handlers for lets an assistive technology
    /// do, and the state the router holds for it.
    fn add_interaction(&self, node: &mut AccessNode, elem: u32) {
        let disabled = self.router.is_disabled(elem);
        if disabled {
            node.flags.insert(AccessFlags::DISABLED);
        }
        if self.router.is_focused(elem) {
            node.flags.insert(AccessFlags::FOCUSED);
        }
        if self.router.is_focus_target(elem) {
            node.flags.insert(AccessFlags::FOCUSABLE);
            if !disabled {
                node.actions.insert(AccessActions::FOCUS);
            }
        }
        if disabled {
            return;
        }
        if self.router.has_handler(elem, RouterKind::Tap) {
            node.actions.insert(AccessActions::PRESS);
        }
        if self.router.has_handler(elem, RouterKind::Change) {
            if matches!(node.role, AccessRole::TextInput | AccessRole::Slider) {
                node.actions.insert(AccessActions::SET_VALUE);
            }
            if node.role == AccessRole::Slider {
                node.actions.insert(AccessActions::INCREMENT);
                node.actions.insert(AccessActions::DECREMENT);
            }
        }
    }

    /// The node a box with a role becomes: its name, value, state and what
    /// can be done to it (RFC-0042 §2, §3).
    fn box_node(&mut self, role: AccessRole, elem: u32, b: &BoxFacts<'_>) -> AccessNode {
        let label = self.access_label(b.attrs);
        let mut node = self.leaf(role, elem, label, bounds(b.rect, &b.transform));
        let peek = |this: &Self| b.bound_sig.map(|s| this.ctx.peek_signal(s));
        match role {
            AccessRole::TextInput => {
                let placeholder = self
                    .eval_str_prop(b.paint_attrs, "placeholder")
                    .filter(|p| !p.is_empty());
                let text = match peek(self) {
                    Some(Value::Str(t)) => t,
                    _ => String::new(),
                };
                node.value = Some(AccessValue::Text(text));
                // The placeholder names the field when nothing else does, and
                // is a description when something does.
                if node.name.is_none() {
                    node.name = placeholder;
                } else if placeholder != node.name {
                    node.description = placeholder;
                }
            }
            AccessRole::Switch => {
                let on = matches!(peek(self), Some(Value::Bool(true)));
                node.value = Some(AccessValue::Toggled(on));
            }
            AccessRole::CheckBox => {
                if matches!(peek(self), Some(Value::Bool(true))) {
                    node.flags.insert(AccessFlags::CHECKED);
                }
                if self.eval_bool_prop(b.paint_attrs, "indeterminate") == Some(true) {
                    node.flags.insert(AccessFlags::MIXED);
                }
            }
            AccessRole::RadioButton => {
                let mine = self
                    .eval_str_prop(b.paint_attrs, "value")
                    .unwrap_or_default();
                if matches!(peek(self), Some(Value::Str(v)) if v == mine) {
                    node.flags.insert(AccessFlags::CHECKED);
                }
            }
            AccessRole::Slider => {
                let min = self.eval_float_prop(b.paint_attrs, "min").unwrap_or(0.0);
                let max = self.eval_float_prop(b.paint_attrs, "max").unwrap_or(1.0);
                let step = self.eval_float_prop(b.paint_attrs, "step");
                let value = match peek(self) {
                    Some(Value::Float(f)) => f,
                    #[allow(clippy::cast_precision_loss)]
                    Some(Value::Int(n)) => n as f64,
                    _ => min,
                };
                node.value = Some(AccessValue::Number {
                    value,
                    min,
                    max,
                    step,
                });
            }
            AccessRole::ScrollView => {
                let (x, y) = self.resolve_axis_pair(b.attrs, "offset", (0.0, 0.0));
                let (cw, ch) = self
                    .atlas
                    .content_size(b.atlas_node)
                    .ok()
                    .flatten()
                    .unwrap_or((b.rect.w, b.rect.h));
                node.value = Some(AccessValue::Scroll {
                    x,
                    y,
                    max_x: (cw - b.rect.w).max(0.0),
                    max_y: (ch - b.rect.h).max(0.0),
                });
            }
            _ => {}
        }
        if role != AccessRole::ScrollView {
            self.add_interaction(&mut node, elem);
        }
        node
    }

    /// The "label beside it" rule (RFC-0042 §3): a switch, checkbox or radio
    /// button nothing else names is named by the next `Text` in the same `Row`.
    ///
    /// Runs when the `Row` is left, over the nodes it held: those it owns
    /// itself, not the ones in containers nested inside it.
    fn name_from_label_beside(&self, frame: &mut RenderFrame, first: usize) {
        let Some(tree) = frame.access_mut() else {
            return;
        };
        // This `Row`'s container has just been popped.
        let depth = u32::try_from(self.access.containers.len() + 1).unwrap_or(u32::MAX);
        let owners = &self.access.owners;
        for i in first..tree.nodes.len() {
            let wants = matches!(
                tree.nodes[i].role,
                AccessRole::Switch | AccessRole::CheckBox | AccessRole::RadioButton
            ) && tree.nodes[i].name.is_none()
                && owners.get(i) == Some(&depth);
            if !wants {
                continue;
            }
            let label = (i + 1..tree.nodes.len())
                .find(|&j| owners.get(j) == Some(&depth) && tree.nodes[j].role == AccessRole::Label)
                .and_then(|j| tree.nodes[j].name.clone());
            tree.nodes[i].name = label;
        }
    }
}
