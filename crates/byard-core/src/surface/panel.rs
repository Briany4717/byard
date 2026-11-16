//! What a `Panel` asks of the compositor, and what a host tells the program
//! about its screens (RFC-0068 §3, §4, §6).
//!
//! A panel is a `zwlr_layer_surface_v1`. The values the protocol takes are held
//! here as plain data, so the interpreter can say what a panel wants without
//! knowing Wayland, a host can send them to whatever it has, and the headless
//! placement ([`placement::arrange`](super::placement::arrange)) can lay panels
//! out by the same numbers the compositor would use.

use super::placement::{Anchors, Edge, Layer, LayerSurface, Margin, exclusive_edge};
use super::{Rect, Size};

/// One connected screen, as a program sees it in `screens` (RFC-0068 §4).
///
/// The geometry is logical: `width` and `height` are in the same pixels a panel's
/// `width` is. A host fills a list of these and hands it to
/// `Interpreter::set_screens`; the list is read in the order given, so a host
/// sorts it by `x`, then `y`.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenInfo {
    /// The connector (`DP-1`), else `screen-N` in connection order.
    pub name: String,
    /// What the compositor says the screen is (`Dell U2723QE`), possibly empty.
    pub description: String,
    /// Left edge in the compositor's logical space.
    pub x: f32,
    /// Top edge in the compositor's logical space.
    pub y: f32,
    /// Logical width.
    pub width: f32,
    /// Logical height.
    pub height: f32,
    /// The scale factor, fractional where the platform says so.
    pub scale: f32,
}

impl ScreenInfo {
    /// A screen at the origin, at scale 1, with no description.
    #[must_use]
    pub fn new(name: impl Into<String>, width: f32, height: f32) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            x: 0.0,
            y: 0.0,
            width,
            height,
            scale: 1.0,
        }
    }

    /// Sorts `screens` by `x`, then `y`, which is the order `screens` is read
    /// in: stable across restarts and the way the screens sit on the desk.
    pub fn sort(screens: &mut [ScreenInfo]) {
        screens.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    }
}

/// Who gets the keyboard (`keyboard`, `set_keyboard_interactivity`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyboardMode {
    /// Never: a bar must not take the keys from the window being typed in.
    #[default]
    None,
    /// When the panel is clicked.
    OnDemand,
    /// Every key, while the panel is shown (a launcher, a lock prompt).
    Exclusive,
}

/// What a panel reserves (`exclusive`, `set_exclusive_zone`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Exclusive {
    /// Nothing: the panel respects the space others reserved (`false`).
    #[default]
    Off,
    /// Its own thickness along its edge (`true`): its height for `top` and
    /// `bottom`, its width for `left` and `right`.
    Thickness,
    /// This many logical pixels.
    Pixels(i32),
    /// Cover what others reserved too (`ignore`, a zone of -1).
    Ignore,
}

/// Which part of a panel takes the pointer (`input`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputMode {
    /// What the program drew that takes it, and nothing else.
    #[default]
    Auto,
    /// The whole panel.
    All,
    /// None of it (an on-screen display).
    None,
}

/// The part of a panel that takes the pointer, as `wl_surface.set_input_region`
/// wants it (RFC-0068 §6). Whole logical pixels in the surface's coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputRegion {
    /// The whole surface: no region is set.
    All,
    /// Nowhere: an empty region.
    None,
    /// The union of these rectangles, sorted and without repeats.
    Rects(Vec<Rect>),
}

/// The layer-shell half of a [`SurfaceSpec`](super::SurfaceSpec): what makes it
/// a layer surface and not a window.
///
/// The size is not here: it is the spec's `size` (0 on an axis the compositor
/// stretches).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerSpec {
    /// The layer it lives in.
    pub layer: Layer,
    /// The edges it is anchored to.
    pub anchors: Anchors,
    /// What it reserves.
    pub exclusive: Exclusive,
    /// Who gets the keyboard.
    pub keyboard: KeyboardMode,
    /// The name the compositor's rules match. Fixed when the surface is made.
    pub namespace: String,
    /// The screen it is on, by name; `None` is the compositor's choice. Fixed
    /// when the surface is made.
    pub screen: Option<String>,
    /// How far it keeps from the edges it is anchored to.
    pub margin: Margin,
    /// Which part of it takes the pointer.
    pub input: InputMode,
}

impl LayerSpec {
    /// A panel in the `top` layer, anchored nowhere, that reserves nothing.
    #[must_use]
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            layer: Layer::Top,
            anchors: Anchors::NONE,
            exclusive: Exclusive::Off,
            keyboard: KeyboardMode::None,
            namespace: namespace.into(),
            screen: None,
            margin: Margin::default(),
            input: InputMode::Auto,
        }
    }

    /// The exclusive zone to send for a panel of `size`: the protocol's
    /// integer (-1 to cover everything, 0 for none, else pixels).
    ///
    /// `Thickness` is the size along the anchored edge, and 0 when the anchors
    /// have no single edge (the protocol ignores a zone there, and the checker
    /// says so: `ExclusiveNeedsEdge`).
    #[must_use]
    pub fn zone(&self, size: Size) -> i32 {
        match self.exclusive {
            Exclusive::Off => 0,
            Exclusive::Ignore => -1,
            Exclusive::Pixels(n) => n.max(0),
            Exclusive::Thickness => match exclusive_edge(self.anchors, 1) {
                Some(Edge::Top | Edge::Bottom) => size.height,
                Some(Edge::Left | Edge::Right) => size.width,
                None => 0,
            },
        }
    }

    /// What this panel asks of the compositor when it asks for `size` (0 on a
    /// stretched axis), as the placement solver takes it.
    #[must_use]
    pub fn layer_surface(&self, size: Size) -> LayerSurface {
        LayerSurface {
            layer: self.layer,
            anchors: self.anchors,
            width: size.width,
            height: size.height,
            margin: self.margin,
            exclusive_zone: self.zone(size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screens_are_ordered_by_x_then_y() {
        let mut list = vec![
            ScreenInfo {
                x: 1920.0,
                ..ScreenInfo::new("right", 1920.0, 1080.0)
            },
            ScreenInfo {
                y: 1080.0,
                ..ScreenInfo::new("below", 1920.0, 1080.0)
            },
            ScreenInfo::new("left", 1920.0, 1080.0),
        ];
        ScreenInfo::sort(&mut list);
        let names: Vec<_> = list.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["left", "below", "right"]);
    }

    #[test]
    fn a_thickness_zone_is_the_size_along_the_edge() {
        let mut spec = LayerSpec::new("bar");
        spec.exclusive = Exclusive::Thickness;
        spec.anchors = Anchors::TOP | Anchors::LEFT | Anchors::RIGHT;
        assert_eq!(spec.zone(Size::new(0, 40)), 40);
        spec.anchors = Anchors::LEFT | Anchors::TOP | Anchors::BOTTOM;
        assert_eq!(spec.zone(Size::new(64, 0)), 64);
        // A corner has no edge: the protocol ignores a zone there.
        spec.anchors = Anchors::TOP | Anchors::LEFT;
        assert_eq!(spec.zone(Size::new(64, 40)), 0);
    }

    #[test]
    fn the_other_zones_are_what_they_say() {
        let mut spec = LayerSpec::new("bar");
        assert_eq!(spec.zone(Size::new(10, 10)), 0);
        spec.exclusive = Exclusive::Ignore;
        assert_eq!(spec.zone(Size::new(10, 10)), -1);
        spec.exclusive = Exclusive::Pixels(24);
        assert_eq!(spec.zone(Size::new(10, 10)), 24);
        spec.exclusive = Exclusive::Pixels(-5);
        assert_eq!(spec.zone(Size::new(10, 10)), 0);
    }
}
