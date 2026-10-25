//! The accessibility tree on the way to a screen reader (RFC-0042 §5).
//!
//! `byard-core` describes the window in its own types; this is the one place
//! they meet AccessKit. Everything here is a pure function of a tree and a
//! window (mapping, diffing, translating a request back), so it is tested
//! without a window, an event loop or an assistive technology. The one piece
//! that is not, [`Bridge`], is the thin shell that owns the AccessKit adapter
//! and moves these values between it and the host.

use std::collections::HashMap;

use accesskit::{
    Action, ActionData, ActionRequest, Affine, Node, NodeId, Rect, Role, Toggled, TreeId, TreeInfo,
    TreeUpdate,
};
use byard_core::PlatformHost;
use byard_core::access::{
    AccessAction, AccessActions, AccessFlags, AccessId, AccessNode, AccessRequest, AccessRole,
    AccessTree, AccessValue,
};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::Window;

use crate::UserEvent;

/// The window's own node: everything the tree hangs from.
const ROOT: NodeId = NodeId(0);

/// Whether a window must be created hidden for this to work: AccessKit wants
/// its adapter in place before the window is first shown.
pub(crate) const NEEDS_HIDDEN_START: bool = true;

/// The node id of an element: its index plus one, since the window takes `0`.
fn node_id(id: AccessId) -> NodeId {
    NodeId(u64::from(id.0) + 1)
}

/// The element a node id names, or `None` for the window itself and for ids
/// no element can have.
fn access_id(id: NodeId) -> Option<AccessId> {
    id.0.checked_sub(1)
        .and_then(|n| u32::try_from(n).ok())
        .map(AccessId)
}

/// What the window itself says: its title and size (RFC-0042 §5).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowInfo {
    /// The title, which is the root node's name.
    pub title: String,
    /// The size in logical pixels.
    pub size: (f32, f32),
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

fn role(role: AccessRole) -> Role {
    match role {
        AccessRole::Button => Role::Button,
        AccessRole::TextInput => Role::TextInput,
        AccessRole::Switch => Role::Switch,
        AccessRole::CheckBox => Role::CheckBox,
        AccessRole::RadioButton => Role::RadioButton,
        AccessRole::Slider => Role::Slider,
        AccessRole::Label => Role::Label,
        AccessRole::Image => Role::Image,
        AccessRole::ScrollView => Role::ScrollView,
        AccessRole::TabList => Role::TabList,
        AccessRole::Tab => Role::Tab,
        AccessRole::Dialog => Role::Dialog,
        AccessRole::Group => Role::Group,
    }
}

/// One element as an AccessKit node, with its children as the tree gave them.
fn node(n: &AccessNode, children: Vec<NodeId>) -> Node {
    let mut out = Node::new(role(n.role));
    let b = n.bounds;
    out.set_bounds(Rect {
        x0: f64::from(b.x),
        y0: f64::from(b.y),
        x1: f64::from(b.x + b.width),
        y1: f64::from(b.y + b.height),
    });
    if let Some(name) = &n.name {
        // A label's text is its value to AccessKit; everything else is named.
        if n.role == AccessRole::Label {
            out.set_value(name.as_str());
        } else {
            out.set_label(name.as_str());
        }
    }
    if let Some(description) = &n.description {
        out.set_description(description.as_str());
    }
    match &n.value {
        Some(AccessValue::Text(text)) => out.set_value(text.as_str()),
        Some(AccessValue::Number {
            value,
            min,
            max,
            step,
        }) => {
            out.set_numeric_value(*value);
            out.set_min_numeric_value(*min);
            out.set_max_numeric_value(*max);
            if let Some(step) = step {
                out.set_numeric_value_step(*step);
            }
        }
        Some(AccessValue::Toggled(on)) => out.set_toggled(Toggled::from(*on)),
        Some(AccessValue::Scroll { x, y, max_x, max_y }) => {
            out.set_scroll_x(f64::from(*x));
            out.set_scroll_x_min(0.0);
            out.set_scroll_x_max(f64::from(*max_x));
            out.set_scroll_y(f64::from(*y));
            out.set_scroll_y_min(0.0);
            out.set_scroll_y_max(f64::from(*max_y));
        }
        None => {}
    }
    let flags = n.flags;
    if flags.contains(AccessFlags::DISABLED) {
        out.set_disabled();
    }
    if flags.contains(AccessFlags::MIXED) {
        out.set_toggled(Toggled::Mixed);
    } else if flags.contains(AccessFlags::CHECKED) {
        out.set_toggled(Toggled::True);
    } else if matches!(n.role, AccessRole::CheckBox | AccessRole::RadioButton) {
        out.set_toggled(Toggled::False);
    }
    if flags.contains(AccessFlags::SELECTED) {
        out.set_selected(true);
    }
    if flags.contains(AccessFlags::EXPANDED) {
        out.set_expanded(true);
    }
    if flags.contains(AccessFlags::MODAL) {
        out.set_modal();
    }
    for (ours, theirs) in [
        (AccessActions::PRESS, Action::Click),
        (AccessActions::FOCUS, Action::Focus),
        (AccessActions::SET_VALUE, Action::SetValue),
        (AccessActions::INCREMENT, Action::Increment),
        (AccessActions::DECREMENT, Action::Decrement),
        (AccessActions::SCROLL_INTO_VIEW, Action::ScrollIntoView),
    ] {
        if n.actions.contains(ours) {
            out.add_action(theirs);
        }
    }
    if !children.is_empty() {
        out.set_children(children);
    }
    out
}

/// The window node, parent of everything at the top of the tree.
fn root(window: &WindowInfo, children: Vec<NodeId>) -> Node {
    let mut out = Node::new(Role::Window);
    out.set_bounds(Rect {
        x0: 0.0,
        y0: 0.0,
        x1: f64::from(window.size.0),
        y1: f64::from(window.size.1),
    });
    // Bounds are logical pixels, the platform's are physical.
    out.set_transform(Affine::scale(window.scale));
    out.set_label(window.title.as_str());
    out.set_children(children);
    out
}

/// What has been sent to the assistive technology, so the next tree can be
/// sent as the difference (RFC-0042 §5: "an unchanged tree sends nothing").
#[derive(Default)]
pub(crate) struct TreeSender {
    /// The nodes of the last update, as AccessKit holds them. `None` until a
    /// first, full tree has gone out.
    sent: Option<HashMap<NodeId, Node>>,
    /// The tree and window of the last update, for the cheap "same again"
    /// answer.
    last: Option<(u64, WindowInfo)>,
    /// The focus of the last update.
    focus: Option<NodeId>,
}

impl TreeSender {
    /// Forgets everything sent, so the next update is a whole tree: what a
    /// newly connected assistive technology needs.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The update that takes the assistive technology from the last tree
    /// sent to `tree`, or `None` when they say the same thing.
    ///
    /// The first update after a [`reset`](Self::reset) carries the whole tree
    /// and the window's [`TreeInfo`]; later ones only the nodes that are new or
    /// changed (a node whose children changed with them, so a parent is
    /// always sent with the child it gained or lost).
    pub(crate) fn update(&mut self, tree: &AccessTree, window: &WindowInfo) -> Option<TreeUpdate> {
        let hash = tree.content_hash();
        if self
            .last
            .as_ref()
            .is_some_and(|(h, w)| *h == hash && w == window)
        {
            return None;
        }

        // Children, in the order the tree lists them.
        let mut children: HashMap<Option<AccessId>, Vec<NodeId>> = HashMap::new();
        for n in &tree.nodes {
            children.entry(n.parent).or_default().push(node_id(n.id));
        }
        let top = children.remove(&None).unwrap_or_default();

        let mut nodes = vec![(ROOT, root(window, top))];
        for n in &tree.nodes {
            let id = node_id(n.id);
            nodes.push((
                id,
                node(n, children.remove(&Some(n.id)).unwrap_or_default()),
            ));
        }

        let first = self.sent.is_none();
        let previous = self.sent.take().unwrap_or_default();
        let now: HashMap<NodeId, Node> = nodes.iter().cloned().collect();
        if !first {
            nodes.retain(|(id, n)| previous.get(id) != Some(n));
        }
        let focus = tree.focus.map_or(ROOT, node_id);
        let unchanged = !first && nodes.is_empty() && self.focus == Some(focus);
        self.sent = Some(now);
        self.last = Some((hash, window.clone()));
        self.focus = Some(focus);
        if unchanged {
            return None;
        }
        Some(TreeUpdate {
            nodes,
            tree: first.then(|| TreeInfo::new(ROOT)),
            tree_id: TreeId::ROOT,
            focus,
        })
    }
}

/// What an assistive technology asked, as an engine request (RFC-0042 §5), or
/// `None` for an action Byard has no use for or a node that is not an element.
pub(crate) fn request(r: &ActionRequest) -> Option<AccessRequest> {
    if r.target_tree != TreeId::ROOT {
        return None;
    }
    let target = access_id(r.target_node)?;
    let action = match (r.action, r.data.as_ref()) {
        (Action::Click, _) => AccessAction::Press,
        (Action::Focus, _) => AccessAction::Focus,
        (Action::Increment, _) => AccessAction::Increment,
        (Action::Decrement, _) => AccessAction::Decrement,
        (Action::ScrollIntoView, _) => AccessAction::ScrollIntoView,
        (Action::SetValue, Some(ActionData::Value(text))) => {
            AccessAction::SetText(text.to_string())
        }
        (Action::SetValue, Some(ActionData::NumericValue(v))) => AccessAction::SetNumber(*v),
        _ => return None,
    };
    Some(AccessRequest { target, action })
}

/// The AccessKit adapter of one window, and what it has been told.
pub(crate) struct Bridge {
    adapter: accesskit_winit::Adapter,
    sender: TreeSender,
    /// Whether an assistive technology is connected.
    active: bool,
}

impl Bridge {
    /// Creates the adapter. The window must not have been shown yet.
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        Self {
            adapter: accesskit_winit::Adapter::with_event_loop_proxy(event_loop, window, proxy),
            sender: TreeSender::default(),
            active: false,
        }
    }

    /// Lets the adapter see a window event before the host does, as AccessKit
    /// requires.
    pub(crate) fn process_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        self.adapter.process_event(window, event);
    }

    /// Handles what the adapter reported: a screen reader connecting or
    /// leaving, or asking a node to act.
    pub(crate) fn handle<H: PlatformHost>(
        &mut self,
        host: &mut H,
        event: accesskit_winit::WindowEvent,
    ) {
        match event {
            accesskit_winit::WindowEvent::InitialTreeRequested => {
                self.sender.reset();
                self.active = true;
                host.on_access_active(true);
            }
            accesskit_winit::WindowEvent::ActionRequested(r) => {
                if let Some(request) = request(&r) {
                    host.on_access_request(request);
                }
            }
            accesskit_winit::WindowEvent::AccessibilityDeactivated => {
                self.active = false;
                self.sender.reset();
                host.on_access_active(false);
            }
        }
    }

    /// Sends the host's latest tree if it differs from the last one sent.
    /// Costs nothing while no assistive technology is connected.
    pub(crate) fn sync<H: PlatformHost>(&mut self, host: &H, window: &Window) {
        if !self.active {
            return;
        }
        let Some(frame) = host.access_frame() else {
            return;
        };
        let Some(tree) = frame.access() else {
            return;
        };
        let scale = window.scale_factor();
        let size = window.inner_size().to_logical::<f32>(scale);
        let info = WindowInfo {
            title: window.title(),
            size: (size.width, size.height),
            scale,
        };
        if let Some(update) = self.sender.update(tree, &info) {
            self.adapter.update_if_active(|| update);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byard_core::access::AccessActions;
    use byard_core::frame::Rect as FrameRect;

    fn window() -> WindowInfo {
        WindowInfo {
            title: "Demo".into(),
            size: (400.0, 300.0),
            scale: 2.0,
        }
    }

    fn element(id: u32, parent: Option<u32>, role: AccessRole, name: &str) -> AccessNode {
        AccessNode {
            id: AccessId(id),
            parent: parent.map(AccessId),
            role,
            name: Some(name.into()),
            description: None,
            value: None,
            bounds: FrameRect::new(10.0, 20.0, 30.0, 40.0),
            flags: AccessFlags::empty(),
            actions: AccessActions::empty(),
        }
    }

    fn tree(nodes: Vec<AccessNode>) -> AccessTree {
        AccessTree { nodes, focus: None }
    }

    fn find(update: &TreeUpdate, id: u32) -> &Node {
        &update
            .nodes
            .iter()
            .find(|(n, _)| *n == node_id(AccessId(id)))
            .unwrap_or_else(|| panic!("node {id} was not sent"))
            .1
    }

    #[test]
    fn element_ids_leave_room_for_the_window_and_round_trip() {
        assert_eq!(node_id(AccessId(0)), NodeId(1));
        assert_eq!(access_id(NodeId(1)), Some(AccessId(0)));
        assert_eq!(access_id(ROOT), None, "the window is not an element");
        assert_eq!(access_id(NodeId(u64::MAX)), None);
    }

    #[test]
    fn every_role_maps_to_an_accesskit_role() {
        use AccessRole as R;
        let pairs = [
            (R::Button, Role::Button),
            (R::TextInput, Role::TextInput),
            (R::Switch, Role::Switch),
            (R::CheckBox, Role::CheckBox),
            (R::RadioButton, Role::RadioButton),
            (R::Slider, Role::Slider),
            (R::Label, Role::Label),
            (R::Image, Role::Image),
            (R::ScrollView, Role::ScrollView),
            (R::TabList, Role::TabList),
            (R::Tab, Role::Tab),
            (R::Dialog, Role::Dialog),
            (R::Group, Role::Group),
        ];
        for (ours, theirs) in pairs {
            assert_eq!(role(ours), theirs);
        }
    }

    #[test]
    fn a_first_update_is_the_whole_tree_under_the_window() {
        let mut button = element(0, None, AccessRole::Button, "Save");
        button.actions.insert(AccessActions::PRESS);
        button.flags.insert(AccessFlags::DISABLED);
        let label = element(1, Some(0), AccessRole::Label, "inside");
        let mut sender = TreeSender::default();
        let update = sender
            .update(&tree(vec![button, label]), &window())
            .expect("a first tree is always sent");

        assert_eq!(update.tree, Some(TreeInfo::new(ROOT)));
        assert_eq!(update.focus, ROOT, "no element is focused");
        assert_eq!(update.nodes.len(), 3);
        let window_node = &update.nodes.iter().find(|(n, _)| *n == ROOT).unwrap().1;
        assert_eq!(window_node.role(), Role::Window);
        assert_eq!(window_node.label(), Some("Demo"));
        assert_eq!(window_node.children(), [node_id(AccessId(0))]);
        assert_eq!(window_node.transform(), Some(&Affine::scale(2.0)));

        let button = find(&update, 0);
        assert_eq!(button.role(), Role::Button);
        assert_eq!(button.label(), Some("Save"));
        assert!(button.supports_action(Action::Click));
        assert!(!button.supports_action(Action::Focus));
        assert!(button.is_disabled());
        assert_eq!(button.children(), [node_id(AccessId(1))]);
        assert_eq!(
            button.bounds(),
            Some(Rect {
                x0: 10.0,
                y0: 20.0,
                x1: 40.0,
                y1: 60.0
            })
        );
        // A label's text is its value, not its name.
        assert_eq!(find(&update, 1).value(), Some("inside"));
    }

    #[test]
    fn values_and_state_reach_accesskit() {
        let mut slider = element(0, None, AccessRole::Slider, "Volume");
        slider.value = Some(AccessValue::Number {
            value: 0.5,
            min: 0.0,
            max: 1.0,
            step: Some(0.25),
        });
        slider.actions.insert(AccessActions::INCREMENT);
        slider.actions.insert(AccessActions::DECREMENT);
        slider.actions.insert(AccessActions::SET_VALUE);
        let mut field = element(1, None, AccessRole::TextInput, "Search");
        field.value = Some(AccessValue::Text("shoes".into()));
        field.description = Some("Type to filter".into());
        let mut switch = element(2, None, AccessRole::Switch, "Wi-Fi");
        switch.value = Some(AccessValue::Toggled(true));
        let mut mixed = element(3, None, AccessRole::CheckBox, "All");
        mixed.flags.insert(AccessFlags::MIXED);
        let mut checked = element(4, None, AccessRole::CheckBox, "One");
        checked.flags.insert(AccessFlags::CHECKED);
        let unchecked = element(5, None, AccessRole::RadioButton, "Two");
        let mut tab = element(6, None, AccessRole::Tab, "home");
        tab.flags.insert(AccessFlags::SELECTED);
        let mut dialog = element(7, None, AccessRole::Dialog, "Confirm");
        dialog.flags.insert(AccessFlags::MODAL);
        let mut scroll = element(8, None, AccessRole::ScrollView, "");
        scroll.name = None;
        scroll.value = Some(AccessValue::Scroll {
            x: 0.0,
            y: 30.0,
            max_x: 0.0,
            max_y: 300.0,
        });

        let update = TreeSender::default()
            .update(
                &tree(vec![
                    slider, field, switch, mixed, checked, unchecked, tab, dialog, scroll,
                ]),
                &window(),
            )
            .unwrap();

        let slider = find(&update, 0);
        assert_eq!(slider.numeric_value(), Some(0.5));
        assert_eq!(slider.min_numeric_value(), Some(0.0));
        assert_eq!(slider.max_numeric_value(), Some(1.0));
        assert_eq!(slider.numeric_value_step(), Some(0.25));
        for action in [Action::Increment, Action::Decrement, Action::SetValue] {
            assert!(slider.supports_action(action));
        }
        let field = find(&update, 1);
        assert_eq!(field.value(), Some("shoes"));
        assert_eq!(field.description(), Some("Type to filter"));
        assert_eq!(find(&update, 2).toggled(), Some(Toggled::True));
        assert_eq!(find(&update, 3).toggled(), Some(Toggled::Mixed));
        assert_eq!(find(&update, 4).toggled(), Some(Toggled::True));
        assert_eq!(find(&update, 5).toggled(), Some(Toggled::False));
        assert_eq!(find(&update, 6).is_selected(), Some(true));
        assert!(find(&update, 7).is_modal());
        let scroll = find(&update, 8);
        assert_eq!(scroll.scroll_y(), Some(30.0));
        assert_eq!(scroll.scroll_y_max(), Some(300.0));
    }

    #[test]
    fn an_unchanged_tree_sends_nothing() {
        let t = tree(vec![element(0, None, AccessRole::Button, "Save")]);
        let mut sender = TreeSender::default();
        assert!(sender.update(&t, &window()).is_some());
        assert!(sender.update(&t, &window()).is_none());
        assert!(sender.update(&t.clone(), &window()).is_none());
    }

    #[test]
    fn a_change_sends_only_what_changed_and_the_parent_of_what_came_and_went() {
        let before = tree(vec![
            element(0, None, AccessRole::Button, "Save"),
            element(1, None, AccessRole::Label, "Stays"),
        ]);
        let mut sender = TreeSender::default();
        sender.update(&before, &window()).unwrap();

        // One node's name changes: that node alone is sent.
        let renamed = tree(vec![
            element(0, None, AccessRole::Button, "Saved"),
            element(1, None, AccessRole::Label, "Stays"),
        ]);
        let update = sender.update(&renamed, &window()).unwrap();
        assert_eq!(update.tree, None);
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(find(&update, 0).label(), Some("Saved"));

        // A node arrives: it is sent with the window it now hangs from.
        let grown = tree(vec![
            element(0, None, AccessRole::Button, "Saved"),
            element(1, None, AccessRole::Label, "Stays"),
            element(2, None, AccessRole::Label, "New"),
        ]);
        let update = sender.update(&grown, &window()).unwrap();
        let ids: Vec<NodeId> = update.nodes.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [ROOT, node_id(AccessId(2))]);

        // And one goes: the window is sent without it.
        let update = sender.update(&renamed, &window()).unwrap();
        let (id, window_node) = &update.nodes[0];
        assert_eq!(*id, ROOT);
        assert_eq!(window_node.children().len(), 2);
        assert_eq!(update.nodes.len(), 1);
    }

    #[test]
    fn a_new_focus_alone_is_an_update_with_no_nodes() {
        let mut field = element(0, None, AccessRole::TextInput, "Search");
        field.flags.insert(AccessFlags::FOCUSABLE);
        let mut t = tree(vec![field]);
        let mut sender = TreeSender::default();
        sender.update(&t, &window()).unwrap();
        t.focus = Some(AccessId(0));
        let update = sender.update(&t, &window()).unwrap();
        assert_eq!(update.nodes.len(), 0);
        assert_eq!(update.focus, node_id(AccessId(0)));
    }

    #[test]
    fn a_resized_window_resends_the_window_node_and_a_reset_resends_everything() {
        let t = tree(vec![element(0, None, AccessRole::Button, "Save")]);
        let mut sender = TreeSender::default();
        sender.update(&t, &window()).unwrap();
        let bigger = WindowInfo {
            size: (800.0, 600.0),
            ..window()
        };
        let update = sender.update(&t, &bigger).unwrap();
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(update.nodes[0].0, ROOT);

        // A reader that connects afterwards knows nothing yet.
        sender.reset();
        let update = sender.update(&t, &bigger).unwrap();
        assert_eq!(update.nodes.len(), 2);
        assert!(update.tree.is_some());
    }

    #[test]
    fn requests_become_engine_requests_and_foreign_ones_are_dropped() {
        let ask = |action, data: Option<ActionData>, node: NodeId| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: node,
            data,
        };
        let target = node_id(AccessId(4));
        let to = |action, data| request(&ask(action, data, target));
        let expect = |action| {
            Some(AccessRequest {
                target: AccessId(4),
                action,
            })
        };
        assert_eq!(to(Action::Click, None), expect(AccessAction::Press));
        assert_eq!(to(Action::Focus, None), expect(AccessAction::Focus));
        assert_eq!(to(Action::Increment, None), expect(AccessAction::Increment));
        assert_eq!(to(Action::Decrement, None), expect(AccessAction::Decrement));
        assert_eq!(
            to(Action::ScrollIntoView, None),
            expect(AccessAction::ScrollIntoView)
        );
        assert_eq!(
            to(Action::SetValue, Some(ActionData::Value("hi".into()))),
            expect(AccessAction::SetText("hi".into()))
        );
        assert_eq!(
            to(Action::SetValue, Some(ActionData::NumericValue(0.25))),
            expect(AccessAction::SetNumber(0.25))
        );
        // No value to set, an action Byard has no use for, and the window
        // itself.
        assert_eq!(to(Action::SetValue, None), None);
        assert_eq!(to(Action::Blur, None), None);
        assert_eq!(request(&ask(Action::Click, None, ROOT)), None);
    }
}
