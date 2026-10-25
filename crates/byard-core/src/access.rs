//! # The accessibility tree (RFC-0042)
//!
//! What a screen reader, a switch device or a voice-control tool needs to know
//! about a window: each control the user can read or operate is a node with a
//! role, a name, its bounds and its state, and the assistive technology's
//! actions come back as the events a pointer or a keyboard would have sent.
//!
//! Byard draws every pixel itself, so the operating system sees one opaque
//! surface. The interpreter, which alone knows what is on screen and where,
//! fills an [`AccessTree`] on the [`RenderFrame`](crate::frame::RenderFrame)
//! **only while an assistive technology is listening**
//! ([`Relay::access_active`](crate::relay::Relay::access_active)). With none
//! running there is no tree, no allocation and nothing across the frame: the
//! whole cost is one relaxed atomic load per frame.
//!
//! These are Byard's own types. `byard-core` does not depend on AccessKit
//! (RFC-0042 Q1): the platform crate maps them onto it, and a headless run
//! produces the same tree with no assistive technology, no D-Bus and no
//! window, which is what [`AccessTree::to_text`] and the tests build on.
//!
//! ```text
//! interpreter ── AccessTree ──▶ frame ──▶ platform ── accesskit::TreeUpdate ──▶ AT
//!      ▲                                     │
//!      └────── InputEvent ◀── AccessRequest ◀┘   (press, focus, set value, …)
//! ```

use std::cell::Cell;
use std::hash::{Hash, Hasher};

use crate::frame::Rect;
use crate::platform::{EventKind, InputEvent, InputPayload};

/// A node's identity: the element's index in the lowered tree, the same index
/// the event router uses to address an element (RFC-0042 §4).
///
/// Stable while the tree's shape does not change. When a list inserts above a
/// node its id changes and the reader re-reads that node, as a browser does
/// when a DOM node is replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccessId(pub u32);

/// What a node is, in Byard's words; the platform maps each onto the role of
/// the accessibility API it speaks to (RFC-0042 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessRole {
    /// A `Button`, or any element with a tap handler.
    Button,
    /// A `TextField`.
    TextInput,
    /// A `Toggle`.
    Switch,
    /// A `Checkbox`.
    CheckBox,
    /// A `RadioButton`.
    RadioButton,
    /// A `Slider`.
    Slider,
    /// A `Text`.
    Label,
    /// An `Image`, `VectorIcon` or a named `Canvas`.
    Image,
    /// A `ScrollView`.
    ScrollView,
    /// A `NavHost`.
    TabList,
    /// One tab of a `NavHost`.
    Tab,
    /// A modal `Overlay`.
    Dialog,
    /// A grouping with no meaning of its own; what a native view is unless it
    /// declares otherwise.
    Group,
}

impl AccessRole {
    /// The role's name as the headless dump writes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Button => "Button",
            Self::TextInput => "TextInput",
            Self::Switch => "Switch",
            Self::CheckBox => "CheckBox",
            Self::RadioButton => "RadioButton",
            Self::Slider => "Slider",
            Self::Label => "Label",
            Self::Image => "Image",
            Self::ScrollView => "ScrollView",
            Self::TabList => "TabList",
            Self::Tab => "Tab",
            Self::Dialog => "Dialog",
            Self::Group => "Group",
        }
    }

    /// Whether a node of this role is something the user operates or must be
    /// able to name: without a name a screen reader says only the role.
    /// These are the nodes the dump marks `(unnamed)` when they have none.
    #[must_use]
    pub const fn needs_name(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::TextInput
                | Self::Switch
                | Self::CheckBox
                | Self::RadioButton
                | Self::Slider
                | Self::Image
                | Self::Tab
                | Self::Dialog
        )
    }

    /// Whether a node of this role is named by the text inside it, so a
    /// `Text` within it is part of its name and not a node of its own
    /// (RFC-0042 §3: "`Text` inside a named control is not repeated").
    #[must_use]
    pub const fn names_from_content(self) -> bool {
        matches!(self, Self::Button)
    }
}

/// A node's value (RFC-0042 §1).
#[derive(Debug, Clone, PartialEq)]
pub enum AccessValue {
    /// A text field's text.
    Text(String),
    /// A number with its range, a slider's position.
    Number {
        /// The current value.
        value: f64,
        /// The smallest value.
        min: f64,
        /// The largest value.
        max: f64,
        /// How far one increment moves it, when the control says.
        step: Option<f64>,
    },
    /// A switch's state.
    Toggled(bool),
    /// A `ScrollView`'s position, in logical pixels, and how far it can go.
    Scroll {
        /// Horizontal offset.
        x: f32,
        /// Vertical offset.
        y: f32,
        /// The largest horizontal offset.
        max_x: f32,
        /// The largest vertical offset.
        max_y: f32,
    },
}

/// A node's state, as a set of flags (RFC-0042 §1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AccessFlags(u16);

impl AccessFlags {
    /// Keyboard focus can be moved here.
    pub const FOCUSABLE: Self = Self(1);
    /// Keyboard focus is here.
    pub const FOCUSED: Self = Self(1 << 1);
    /// The control is `disabled:` and does nothing.
    pub const DISABLED: Self = Self(1 << 2);
    /// A checkbox or radio button is checked.
    pub const CHECKED: Self = Self(1 << 3);
    /// The node is selected (the current tab).
    pub const SELECTED: Self = Self(1 << 4);
    /// The node is expanded.
    pub const EXPANDED: Self = Self(1 << 5);
    /// A checkbox is in its mixed (`indeterminate`) state.
    pub const MIXED: Self = Self(1 << 6);
    /// The node blocks everything beneath it (a modal dialog).
    pub const MODAL: Self = Self(1 << 7);

    /// Every flag, in the order the dump writes them.
    const ALL: [(Self, &'static str); 8] = [
        (Self::FOCUSABLE, "focusable"),
        (Self::FOCUSED, "focused"),
        (Self::DISABLED, "disabled"),
        (Self::CHECKED, "checked"),
        (Self::MIXED, "mixed"),
        (Self::SELECTED, "selected"),
        (Self::EXPANDED, "expanded"),
        (Self::MODAL, "modal"),
    ];

    /// No flags.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Sets every flag in `other`.
    pub const fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Whether no flag is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// What an assistive technology may ask a node to do (RFC-0042 §1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AccessActions(u8);

impl AccessActions {
    /// Press it, as a tap would.
    pub const PRESS: Self = Self(1);
    /// Move keyboard focus to it.
    pub const FOCUS: Self = Self(1 << 1);
    /// Replace its value.
    pub const SET_VALUE: Self = Self(1 << 2);
    /// Move its value one step up.
    pub const INCREMENT: Self = Self(1 << 3);
    /// Move its value one step down.
    pub const DECREMENT: Self = Self(1 << 4);
    /// Scroll it into view.
    pub const SCROLL_INTO_VIEW: Self = Self(1 << 5);

    /// No actions.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every action in `other` is allowed.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Allows every action in `other`.
    pub const fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Whether no action is allowed.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One control, or one piece of content, in the accessibility tree.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessNode {
    /// The node's identity (§4).
    pub id: AccessId,
    /// The nearest ancestor with a role; `None` for a node at the top. Layout
    /// containers are flattened away, so this is not the element's parent.
    pub parent: Option<AccessId>,
    /// What the node is.
    pub role: AccessRole,
    /// What a screen reader calls it (§3), or `None` when nothing names it.
    pub name: Option<String>,
    /// More to say about it: a text field's placeholder, when the name is
    /// something else.
    pub description: Option<String>,
    /// Its value, for the controls that have one.
    pub value: Option<AccessValue>,
    /// Where it is, in viewport space, after transforms and scroll.
    pub bounds: Rect,
    /// Its state.
    pub flags: AccessFlags,
    /// What an assistive technology may ask of it.
    pub actions: AccessActions,
}

/// The accessibility tree of one frame (RFC-0042 §1): the controls and the
/// content of the window, parents before children.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccessTree {
    /// The nodes, parents before children, in reading order.
    pub nodes: Vec<AccessNode>,
    /// The node that has keyboard focus, when it is in the tree.
    pub focus: Option<AccessId>,
}

thread_local! {
    /// How many [`AccessTree`] builds this thread has started.
    ///
    /// The counter the "flag off builds no tree" assertion reads: it is bumped
    /// by [`AccessTree::begin_build`], the one door the interpreter goes
    /// through, so a frame that never asked for a tree leaves it where it
    /// was. Per thread, because the tree is built on the logic thread and
    /// tests run side by side: a process-wide count would also count their
    /// neighbours.
    static BUILDS: Cell<u64> = const { Cell::new(0) };
}

/// The number of accessibility-tree builds the calling thread has started
/// (RFC-0042 "Cost"). Stays put for as long as no assistive technology is
/// listening.
#[must_use]
pub fn builds() -> u64 {
    BUILDS.with(Cell::get)
}

impl AccessTree {
    /// Marks the start of a build, counting it for [`builds`], and empties the
    /// tree (keeping its capacity) for the nodes about to be pushed.
    pub fn begin_build(&mut self) {
        BUILDS.with(|b| b.set(b.get() + 1));
        self.nodes.clear();
        self.focus = None;
    }

    /// The node with this id.
    #[must_use]
    pub fn node(&self, id: AccessId) -> Option<&AccessNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// A hash of everything the tree says, to tell whether it differs from the
    /// one last sent (RFC-0042 §5: "an unchanged tree sends nothing").
    #[must_use]
    pub fn content_hash(&self) -> u64 {
        let mut h = rustc_hash::FxHasher::default();
        self.nodes.len().hash(&mut h);
        self.focus.hash(&mut h);
        for n in &self.nodes {
            n.id.hash(&mut h);
            n.parent.hash(&mut h);
            n.role.hash(&mut h);
            n.name.hash(&mut h);
            n.description.hash(&mut h);
            hash_value(n.value.as_ref(), &mut h);
            for v in [n.bounds.x, n.bounds.y, n.bounds.width, n.bounds.height] {
                v.to_bits().hash(&mut h);
            }
            n.flags.hash(&mut h);
            n.actions.hash(&mut h);
        }
        h.finish()
    }

    /// The tree as indented text: one node a line, children under their
    /// parent, with its role, name, value, state and bounds. A node a screen
    /// reader could not name is marked `(unnamed)`.
    ///
    /// ```text
    /// Button "Save" [focusable focused] @ 24,120 88x36
    /// Switch "Wi-Fi" = on [focusable] @ 24,170 52x32
    /// Slider (unnamed) = 0.5 in 0..1 [focusable] @ 24,220 200x24
    /// ```
    ///
    /// This is what `byard shot --a11y` writes, and what a test or a person
    /// checks without a screen reader.
    #[must_use]
    pub fn to_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        // Parents precede children, so a node's depth is its parent's plus one.
        let mut depths: Vec<(AccessId, usize)> = Vec::with_capacity(self.nodes.len());
        for n in &self.nodes {
            let depth = n
                .parent
                .and_then(|p| depths.iter().find(|(id, _)| *id == p))
                .map_or(0, |(_, d)| d + 1);
            depths.push((n.id, depth));
            let _ = write!(out, "{:indent$}{}", "", n.role.name(), indent = depth * 2);
            match &n.name {
                Some(name) => {
                    let _ = write!(out, " {name:?}");
                }
                None if n.role.needs_name() => out.push_str(" (unnamed)"),
                None => {}
            }
            if let Some(description) = &n.description {
                let _ = write!(out, " description {description:?}");
            }
            if let Some(value) = &n.value {
                out.push_str(" = ");
                write_value(&mut out, value);
            }
            if !n.flags.is_empty() {
                out.push_str(" [");
                let mut first = true;
                for (flag, label) in AccessFlags::ALL {
                    if n.flags.contains(flag) {
                        if !first {
                            out.push(' ');
                        }
                        out.push_str(label);
                        first = false;
                    }
                }
                out.push(']');
            }
            let b = n.bounds;
            let _ = writeln!(
                out,
                " @ {},{} {}x{}",
                num(b.x),
                num(b.y),
                num(b.width),
                num(b.height)
            );
        }
        out
    }
}

/// Feeds a value into a hasher; floats by their bits, since they are not
/// [`Hash`].
fn hash_value(value: Option<&AccessValue>, h: &mut impl Hasher) {
    match value {
        None => 0_u8.hash(h),
        Some(AccessValue::Text(t)) => {
            1_u8.hash(h);
            t.hash(h);
        }
        Some(AccessValue::Number {
            value,
            min,
            max,
            step,
        }) => {
            2_u8.hash(h);
            for v in [*value, *min, *max] {
                v.to_bits().hash(h);
            }
            step.map(f64::to_bits).hash(h);
        }
        Some(AccessValue::Toggled(on)) => {
            3_u8.hash(h);
            on.hash(h);
        }
        Some(AccessValue::Scroll { x, y, max_x, max_y }) => {
            4_u8.hash(h);
            for v in [*x, *y, *max_x, *max_y] {
                v.to_bits().hash(h);
            }
        }
    }
}

/// A number as the dump writes it: a tenth of a pixel at most, no trailing
/// zero, so the same layout always reads the same.
fn num(v: f32) -> String {
    let s = format!("{v:.1}");
    let s = s.strip_suffix(".0").unwrap_or(&s);
    if s == "-0" { "0".into() } else { s.into() }
}

/// A float as the dump writes it: as short as it can be written.
fn float(v: f64) -> String {
    if (v - v.trunc()).abs() < f64::EPSILON && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

fn write_value(out: &mut String, value: &AccessValue) {
    use std::fmt::Write as _;
    match value {
        AccessValue::Text(t) => {
            let _ = write!(out, "{t:?}");
        }
        AccessValue::Number {
            value,
            min,
            max,
            step,
        } => {
            let _ = write!(out, "{} in {}..{}", float(*value), float(*min), float(*max));
            if let Some(step) = step {
                let _ = write!(out, " step {}", float(*step));
            }
        }
        AccessValue::Toggled(on) => out.push_str(if *on { "on" } else { "off" }),
        AccessValue::Scroll { x, y, max_x, max_y } => {
            let _ = write!(
                out,
                "scroll {},{} of {},{}",
                num(*x),
                num(*y),
                num(*max_x),
                num(*max_y)
            );
        }
    }
}

/// What an assistive technology asked a node to do (RFC-0042 §5).
#[derive(Debug, Clone, PartialEq)]
pub enum AccessAction {
    /// Press it: a [`Tap`](EventKind::Tap) at the node's centre, so every
    /// existing handler runs unchanged (Q3).
    Press,
    /// Move keyboard focus to it, through the router.
    Focus,
    /// Replace a text field's text: a [`Change`](EventKind::Change) at the
    /// node's centre, which is where the router looks for a value handler.
    SetText(String),
    /// Set a slider's value, the same way.
    SetNumber(f64),
    /// Move a slider one step up.
    Increment,
    /// Move a slider one step down.
    Decrement,
    /// Scroll its `ScrollView` so it shows.
    ScrollIntoView,
}

/// An action and the node it is aimed at.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessRequest {
    /// The node the assistive technology addressed.
    pub target: AccessId,
    /// What it asked.
    pub action: AccessAction,
}

impl AccessRequest {
    /// The input events this request stands for, read against the `tree` the
    /// assistive technology was looking at (RFC-0042 §5).
    ///
    /// Empty when the node is gone from the tree or does not allow the action,
    /// which is the answer to a request for something that changed under the
    /// reader: nothing happens, rather than something unintended.
    #[must_use]
    pub fn to_events(&self, tree: &AccessTree, time_ms: u64) -> Vec<InputEvent> {
        let Some(node) = tree.node(self.target) else {
            return Vec::new();
        };
        let event = |kind, pos, payload| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload,
            time_ms,
            // An assistive technology's request comes with no keyboard.
            mods: crate::platform::KeyModifiers::default(),
        };
        let allowed = |a: AccessActions| node.actions.contains(a);
        let b = node.bounds;
        // Where the router looks for a handler: a press, and a value change,
        // both go to whatever is under the node's centre.
        let centre = (b.x + b.width / 2.0, b.y + b.height / 2.0);
        let change = |payload| vec![event(EventKind::Change, centre, Some(payload))];
        let number = |v: Option<f64>| v.map_or_else(Vec::new, |v| change(InputPayload::Number(v)));
        match &self.action {
            AccessAction::Press if allowed(AccessActions::PRESS) => {
                vec![event(EventKind::Tap, centre, None)]
            }
            AccessAction::Focus if allowed(AccessActions::FOCUS) => vec![event(
                EventKind::AccessFocus,
                (0.0, 0.0),
                Some(InputPayload::Element(self.target.0)),
            )],
            AccessAction::SetText(text) if allowed(AccessActions::SET_VALUE) => {
                change(InputPayload::Str(text.clone()))
            }
            AccessAction::SetNumber(v) if allowed(AccessActions::SET_VALUE) => {
                number(clamped(node, *v))
            }
            AccessAction::Increment if allowed(AccessActions::INCREMENT) => {
                number(stepped(node, 1.0))
            }
            AccessAction::Decrement if allowed(AccessActions::DECREMENT) => {
                number(stepped(node, -1.0))
            }
            AccessAction::ScrollIntoView if allowed(AccessActions::SCROLL_INTO_VIEW) => {
                vec![InputEvent {
                    delta: (b.width, b.height),
                    ..event(
                        EventKind::AccessScrollIntoView,
                        (b.x, b.y),
                        Some(InputPayload::Element(self.target.0)),
                    )
                }]
            }
            _ => Vec::new(),
        }
    }
}

/// `v` kept inside the range of a node's numeric value, or `None` for a node
/// that has none.
fn clamped(node: &AccessNode, v: f64) -> Option<f64> {
    let Some(AccessValue::Number { min, max, .. }) = node.value else {
        return None;
    };
    Some(if min <= max { v.clamp(min, max) } else { v })
}

/// A node's numeric value moved one step the way `direction` says: the step
/// it declares, else a tenth of its range.
fn stepped(node: &AccessNode, direction: f64) -> Option<f64> {
    let Some(AccessValue::Number {
        value,
        min,
        max,
        step,
    }) = node.value
    else {
        return None;
    };
    let by = step.filter(|s| *s > 0.0).unwrap_or((max - min) / 10.0);
    clamped(node, value + direction * by)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, parent: Option<u32>, role: AccessRole) -> AccessNode {
        AccessNode {
            id: AccessId(id),
            parent: parent.map(AccessId),
            role,
            name: None,
            description: None,
            value: None,
            bounds: Rect::new(10.0, 20.0, 30.0, 40.0),
            flags: AccessFlags::empty(),
            actions: AccessActions::empty(),
        }
    }

    fn slider(value: f64) -> AccessNode {
        AccessNode {
            value: Some(AccessValue::Number {
                value,
                min: 0.0,
                max: 1.0,
                step: Some(0.25),
            }),
            actions: {
                let mut a = AccessActions::FOCUS;
                a.insert(AccessActions::SET_VALUE);
                a.insert(AccessActions::INCREMENT);
                a.insert(AccessActions::DECREMENT);
                a
            },
            ..node(3, None, AccessRole::Slider)
        }
    }

    #[test]
    fn the_dump_indents_children_and_marks_what_cannot_be_named() {
        let mut button = node(1, None, AccessRole::Button);
        button.name = Some("Save".into());
        button.flags.insert(AccessFlags::FOCUSABLE);
        button.flags.insert(AccessFlags::FOCUSED);
        let mut tabs = node(2, None, AccessRole::TabList);
        tabs.bounds = Rect::new(0.0, 0.0, 100.5, 50.0);
        let tab = node(4, Some(2), AccessRole::Tab);
        let tree = AccessTree {
            nodes: vec![button, tabs, tab, slider(0.5)],
            focus: Some(AccessId(1)),
        };
        assert_eq!(
            tree.to_text(),
            "Button \"Save\" [focusable focused] @ 10,20 30x40\n\
             TabList @ 0,0 100.5x50\n  \
             Tab (unnamed) @ 10,20 30x40\n\
             Slider (unnamed) = 0.5 in 0..1 step 0.25 @ 10,20 30x40\n"
        );
    }

    #[test]
    fn a_tree_that_says_the_same_hashes_the_same_and_any_change_does_not() {
        let tree = AccessTree {
            nodes: vec![slider(0.5)],
            focus: None,
        };
        assert_eq!(tree.content_hash(), tree.clone().content_hash());
        let mut moved = tree.clone();
        moved.nodes[0].bounds.x += 1.0;
        let mut valued = tree.clone();
        valued.nodes[0].value = slider(0.75).value;
        let mut focused = tree.clone();
        focused.focus = Some(AccessId(3));
        for changed in [moved, valued, focused] {
            assert_ne!(tree.content_hash(), changed.content_hash());
        }
    }

    #[test]
    fn begin_build_counts_and_empties() {
        let mut tree = AccessTree {
            nodes: vec![node(1, None, AccessRole::Label)],
            focus: Some(AccessId(1)),
        };
        let before = builds();
        tree.begin_build();
        assert!(tree.nodes.is_empty() && tree.focus.is_none());
        assert!(builds() > before);
    }

    #[test]
    fn a_press_is_a_tap_at_the_centre_and_only_where_pressing_is_allowed() {
        let mut button = node(1, None, AccessRole::Button);
        button.actions.insert(AccessActions::PRESS);
        let label = node(2, None, AccessRole::Label);
        let tree = AccessTree {
            nodes: vec![button, label],
            focus: None,
        };
        let press = |id| {
            AccessRequest {
                target: AccessId(id),
                action: AccessAction::Press,
            }
            .to_events(&tree, 7)
        };
        let events = press(1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Tap);
        assert_eq!(events[0].pos, (25.0, 40.0));
        assert_eq!(events[0].time_ms, 7);
        assert!(press(2).is_empty(), "a label does not allow a press");
        assert!(press(9).is_empty(), "a node that is gone does nothing");
    }

    #[test]
    fn setting_a_number_changes_the_slider_at_its_centre_inside_its_range() {
        let tree = AccessTree {
            nodes: vec![slider(0.5)],
            focus: None,
        };
        let set = |v| {
            AccessRequest {
                target: AccessId(3),
                action: AccessAction::SetNumber(v),
            }
            .to_events(&tree, 0)
        };
        let events = set(0.8);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Change);
        assert_eq!(events[0].pos, (25.0, 40.0), "where the router looks");
        assert_eq!(events[0].payload, Some(InputPayload::Number(0.8)));
        assert_eq!(set(7.0)[0].payload, Some(InputPayload::Number(1.0)));
        assert_eq!(set(-1.0)[0].payload, Some(InputPayload::Number(0.0)));
    }

    #[test]
    fn increment_and_decrement_move_one_declared_step_and_stop_at_the_ends() {
        let tree = AccessTree {
            nodes: vec![slider(0.9)],
            focus: None,
        };
        let go = |action| {
            AccessRequest {
                target: AccessId(3),
                action,
            }
            .to_events(&tree, 0)
        };
        assert_eq!(
            go(AccessAction::Increment)[0].payload,
            Some(InputPayload::Number(1.0))
        );
        assert_eq!(
            go(AccessAction::Decrement)[0].payload,
            Some(InputPayload::Number(0.65))
        );
    }

    #[test]
    fn a_focus_request_names_its_element_and_needs_a_focusable_node() {
        let field = node(5, None, AccessRole::TextInput);
        let tree = |actions: AccessActions| {
            let mut n = field.clone();
            n.actions = actions;
            AccessTree {
                nodes: vec![n],
                focus: None,
            }
        };
        let request = AccessRequest {
            target: AccessId(5),
            action: AccessAction::Focus,
        };
        let events = request.to_events(&tree(AccessActions::FOCUS), 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::AccessFocus);
        assert_eq!(events[0].payload, Some(InputPayload::Element(5)));
        assert_eq!(
            request.to_events(&tree(AccessActions::empty()), 0),
            Vec::new()
        );
    }

    #[test]
    fn setting_text_changes_the_field_at_its_centre() {
        let mut field = node(5, None, AccessRole::TextInput);
        field.actions.insert(AccessActions::FOCUS);
        field.actions.insert(AccessActions::SET_VALUE);
        let tree = AccessTree {
            nodes: vec![field],
            focus: None,
        };
        let events = AccessRequest {
            target: AccessId(5),
            action: AccessAction::SetText("hi".into()),
        }
        .to_events(&tree, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Change);
        assert_eq!(events[0].payload, Some(InputPayload::Str("hi".into())));
    }

    #[test]
    fn scrolling_into_view_carries_the_node_and_where_it_is() {
        let mut item = node(6, None, AccessRole::Label);
        item.actions.insert(AccessActions::SCROLL_INTO_VIEW);
        let tree = AccessTree {
            nodes: vec![item],
            focus: None,
        };
        let events = AccessRequest {
            target: AccessId(6),
            action: AccessAction::ScrollIntoView,
        }
        .to_events(&tree, 0);
        assert_eq!(events[0].kind, EventKind::AccessScrollIntoView);
        assert_eq!(events[0].pos, (10.0, 20.0));
        assert_eq!(events[0].delta, (30.0, 40.0));
        assert_eq!(events[0].payload, Some(InputPayload::Element(6)));
    }

    /// A frame that never asked for a tree holds none and has allocated none;
    /// one that did reuses the storage on the frame after it.
    #[test]
    fn a_frame_holds_a_tree_only_when_one_was_built_and_keeps_its_storage() {
        let mut frame = crate::frame::RenderFrame::new();
        assert!(!frame.access_wanted());
        assert!(frame.access().is_none());
        assert_eq!(frame.access_capacity(), 0);

        frame.set_access_wanted(true);
        frame
            .begin_access()
            .nodes
            .push(node(1, None, AccessRole::Label));
        assert_eq!(frame.access().map(|t| t.nodes.len()), Some(1));
        let capacity = frame.access_capacity();
        assert!(capacity >= 1);

        frame.clear();
        assert!(!frame.access_wanted() && frame.access().is_none());
        assert_eq!(frame.access_capacity(), capacity, "kept for the next build");
        assert_eq!(frame.begin_access().nodes.len(), 0);
    }

    #[test]
    fn flags_and_actions_are_sets() {
        let mut flags = AccessFlags::empty();
        assert!(flags.is_empty());
        flags.insert(AccessFlags::CHECKED);
        flags.insert(AccessFlags::DISABLED);
        assert!(flags.contains(AccessFlags::CHECKED));
        assert!(!flags.contains(AccessFlags::FOCUSED));
        let mut both = AccessActions::empty();
        both.insert(AccessActions::PRESS);
        both.insert(AccessActions::FOCUS);
        assert!(both.contains(AccessActions::PRESS));
        assert!(!both.contains(AccessActions::SET_VALUE));
    }
}
