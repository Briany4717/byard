//! The intrinsic catalog (eleven Phase-2 + `VectorIcon` RFC-0009 + `Overlay`
//! RFC-0017 + `Canvas` RFC-0020) and the RFC-0005 §5 attribute contract, plus
//! the RFC-0020 shape-command contract (`validate_canvas`/`validate_shape`).
//!
//! A closed table maps each reserved intrinsic name to its content arity,
//! accepted property/event vocabulary, focusability, and children policy.
//! [`validate_element`] applies the eight §5 rules in order, each producing a
//! precise span-anchored [`CompileError`], no failure is ever silent (D4,
//! INV-4). Interactive elements register a hit rect inflated to a 44×44 minimum
//! (RFC-0003 E8), computed by [`inflate_hit_rect`].

use std::collections::{HashMap, HashSet};

use crate::diagnostics::CompileError;
use crate::parser::ast::{Attr, AttrKind, ElementNode, Expr, Member};
use crate::util::closest_match;

/// The closed set of intrinsic names (RFC-0005 §4).
pub const INTRINSIC_NAMES: &[&str] = &[
    "Box",
    "Column",
    "Row",
    "Spacer",
    "Text",
    "Button",
    "TextField",
    "Toggle",
    "Slider",
    "Checkbox",
    "RadioButton",
    "Image",
    "ScrollView",
    "VectorIcon",
    "Overlay",
    "Canvas",
    "Grid",
    "Clip",
    "ZStack",
    "NavStack",
    "NavHost",
];

/// Whether `name` is one of the RFC-0026 navigation containers, whose children
/// are `route`/`tab` cases rather than elements.
#[must_use]
pub fn is_nav_container(name: &str) -> bool {
    matches!(name, "NavStack" | "NavHost")
}

/// The scalar type an attribute value must have (RFC-0005 §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropType {
    /// `Int` (logical pixels, counts).
    Int,
    /// `Float` (opacities, slider values).
    Float,
    /// `Bool`.
    Bool,
    /// `Str`.
    Str,
    /// A hex `Color` (`0xRRGGBB` / `0xAARRGGBB`).
    Color,
    /// A `Len`: scalar, pair, or quad.
    Len,
    /// A typography token (or a themed member like `m3.titleLarge`).
    Typo,
    /// An enum token validated against a fixed set.
    Enum(&'static [&'static str]),
    /// The typographic weight axis (RFC-0034): one of the four historical
    /// keywords, or an integer `100..=900`.
    ///
    /// Its own type rather than `Int`, because `Int` accepts a bare identifier
    /// and would let `weight: chunky` through in silence — and a weight that
    /// is quietly ignored is the exact failure this property is being fixed
    /// for. Rather than an `Enum`, because the axis is genuinely numeric and
    /// a variable font's `wght` takes the number.
    WeightAxis,
    /// A font family declared in `[assets.fonts]` (RFC-0034).
    ///
    /// Written as a bare name (`font: display`) or a string, and checked
    /// against the families the project actually declares — which cannot
    /// happen here, because this check knows nothing of the theme. What this
    /// type rules out is a value that could never be a family name at all;
    /// the "is it declared" half is a separate pass with the theme in hand.
    FontFamily,
    /// A scoped style class reference (`.title`).
    Class,
    /// A `Vec2` `(Float, Float)`.
    Vec2,
    /// An angle (`360deg`/`1.5rad`, RFC-0011 T1), canonicalized to radians
    /// by the lexer.
    Angle,
    /// A function-valued callback prop.
    Fn,
    /// A spring curve literal (`anim.spring(...)`), RFC-0021 `snap_spring`. Shape
    /// is validated at lower time via `resolve_curve`.
    Spring,
    /// A list of values, the shape a chart's series takes (RFC-0039).
    ///
    /// Checked as far as syntax can check it: a literal of the wrong kind is
    /// rejected here, and everything else, an identifier, a member access, a
    /// `map` over a collection, is a value the lowering evaluates and converts
    /// when it hands the view its props.
    List,
}

/// Which half of the frame an attribute can change (RFC-0032 §R2).
///
/// This is **not** a lookup table bolted on beside the attribute catalogue:
/// it is a required field of every attribute definition, so an attribute
/// added without a class does not compile. RFC-0032 lists an unclassified
/// attribute as one of its named drawbacks, and this is the mitigation it
/// promised, a maintenance surface that the type system maintains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrClass {
    /// Feeds the layout tree: changing it can move or resize something,
    /// including something that is not this element.
    Layout,
    /// Only changes pixels inside a rect layout has already decided.
    Paint,
}

/// One attribute's definition: the value type it accepts **and** the half of
/// the frame it can change.
#[derive(Debug, Clone, Copy)]
pub struct PropDef {
    /// Accepted value type, used for the lower-time type check.
    pub ty: PropType,
    /// Whether this attribute can move geometry (RFC-0032 §R2).
    pub class: AttrClass,
}

/// A layout-class attribute: it reaches the layout tree.
const fn lay(ty: PropType) -> PropDef {
    PropDef {
        ty,
        class: AttrClass::Layout,
    }
}

/// A paint-class attribute: it changes pixels and nothing else.
const fn pnt(ty: PropType) -> PropDef {
    PropDef {
        ty,
        class: AttrClass::Paint,
    }
}

const ALIGN: &[&str] = &["start", "center", "end", "stretch", "justify"];
const JUSTIFY: &[&str] = &["start", "center", "end", "between", "around", "evenly"];
const WEIGHT: &[&str] = &["thin", "regular", "medium", "bold"];
const FIT: &[&str] = &["fill", "contain", "cover", "none"];
const DIRECTION: &[&str] = &["row", "column"];
const AXIS: &[&str] = &["vertical", "horizontal", "both"];
/// RFC-0021 `ScrollView` `snap`: content snaps to discrete positions after a
/// scroll gesture, `item` to each direct child's boundary, `page` to
/// viewport-sized pages, `none` for free scrolling.
const SNAP: &[&str] = &["none", "item", "page"];
/// RFC-0021 `snap_align`: where a snapped item aligns within the viewport.
const SNAP_ALIGN: &[&str] = &["start", "center", "end"];
/// Overlay child placement within the full-viewport coordinate space
/// (RFC-0017 §"Positioning"). `center` centres; the edge tokens pin the child
/// to that viewport edge, centred on the cross axis. Absolute `(x, y)` and
/// `relative(ref)` anchoring are deferred (RFC-0017 Future possibilities),
/// coordinate-passing covers the gap in the interim.
const ANCHOR: &[&str] = &["center", "top", "bottom", "start", "end"];
/// RFC-0036 `anchor_edge`: which side of the anchor the overlay sits on.
const ANCHOR_EDGE: &[&str] = &["above", "below", "before", "after"];
/// RFC-0036 `anchor_align`: how the overlay lines up along the anchor's other
/// axis.
const ANCHOR_ALIGN: &[&str] = &["start", "center", "end"];
/// RFC-0018 `ZStack` `alignment`: how children smaller than the stack are
/// positioned within it. Two-word tokens are `<block>_<inline>`; single-word
/// tokens centre on the other axis.
/// RFC-0026 `transition`: how a navigation container swaps one screen for the
/// next. `slide` is the iOS/Material push, `slide_up` the modal presentation,
/// `fade` a cross-fade, `none` an instant swap (no second live screen).
const TRANSITION: &[&str] = &["slide", "slide_up", "fade", "none"];
const ALIGN2D: &[&str] = &[
    "center",
    "top_start",
    "top_end",
    "bottom_start",
    "bottom_end",
    "top",
    "bottom",
    "start",
    "end",
];

const LAYOUT: &[(&str, PropDef)] = &[
    ("width", lay(PropType::Int)),
    ("height", lay(PropType::Int)),
    ("gap", lay(PropType::Int)),
    ("p", lay(PropType::Len)),
    ("m", lay(PropType::Len)),
    ("pt", lay(PropType::Len)),
    ("pr", lay(PropType::Len)),
    ("pb", lay(PropType::Len)),
    ("pl", lay(PropType::Len)),
    ("mx", lay(PropType::Len)),
    ("my", lay(PropType::Len)),
    ("mt", lay(PropType::Len)),
    ("mr", lay(PropType::Len)),
    ("mb", lay(PropType::Len)),
    ("ml", lay(PropType::Len)),
    ("align", lay(PropType::Enum(ALIGN))),
    ("justify", lay(PropType::Enum(JUSTIFY))),
    ("grow", lay(PropType::Int)),
    ("basis", lay(PropType::Int)),
];
const DECORATION: &[(&str, PropDef)] = &[
    ("bg", pnt(PropType::Color)),
    ("radius", pnt(PropType::Len)),
    // RFC-0031 §S1: the corner *profile* `radius` is measured with, `0.0`
    // (the default) is the circular arc every box drew before, `1.0` a
    // pronounced squircle. Paint-class: it changes the shape of the pixels
    // inside a rect layout has already decided, and nothing else, so it
    // animates under `with` like any other paint scalar.
    ("smooth", pnt(PropType::Float)),
    ("opacity", pnt(PropType::Float)),
    ("border", pnt(PropType::Color)),
    ("border_width", pnt(PropType::Int)),
    ("shadow", pnt(PropType::Str)),
    // RFC-0001 §3.1: the `DecoratedBox` pipeline's declared remit includes
    // gradients. `gradient` is a named tuple (validated at lower time, like
    // `shadow`); `gradient_offset` is an ordinary number, so it animates through
    // the RFC-0010/RFC-0025 chokepoints for free.
    ("gradient", pnt(PropType::Str)),
    ("gradient_offset", pnt(PropType::Float)),
];
/// Paint-time transform props (RFC-0011). `opacity` is deliberately **not**
/// repeated here, it already lives in [`DECORATION`] and is wired end to
/// end (a non-1.0 `opacity` already promotes a box to the `DecoratedBox`
/// pipeline); this group is only the four props that are new with this RFC.
///
/// Attached everywhere [`DECORATION`] is (every intrinsic sharing the
/// generic container/`Box` render path: `Box`/`Column`/`Row`/`Button`/
/// `TextField`/`Toggle`/`Slider`/`ScrollView`), **not** `Text`/`Image`,
/// whose engine primitives (`TextLine`/`TextureSampler`) have no `Transform`
/// field yet (see the RFC-0011 engine-slice decision log).
const TRANSFORM: &[(&str, PropDef)] = &[
    ("translate", pnt(PropType::Vec2)),
    ("scale", pnt(PropType::Vec2)),
    ("rotate", pnt(PropType::Angle)),
    ("origin", pnt(PropType::Vec2)),
];
/// Paint-time visual effects (RFC-0023). Like [`TRANSFORM`], these are
/// paint-only: they never affect layout. `ripple` is the Material ink reveal,
/// setting it (a colour) enables the effect; `ripple_active` is the boolean
/// trigger (typically flipped by an `on pressed { … }` state block);
/// `ripple_radius` overrides the auto max radius (distance from the tap point
/// to the farthest element corner) and `ripple_duration` the 300 ms default
/// fade-out. `blur` is the iOS frosted-glass backdrop blur (logical px,
/// clamped to 40, `0` disables); `backdrop_tint` blends a colour over the
/// blurred sample (with `blur`, the vibrancy pair; alone, a plain translucent
/// wash); `blur_saturation` is the vibrancy saturation boost (default 1.8)
/// and `blur_quality` the tier override (`auto`/`high`/`low`). Attached
/// everywhere [`DECORATION`] is on the box render path, effects composite
/// against an element's background, so they follow the same prop surface.
/// `blur_quality` tokens (RFC-0023 resolved question "blur quality tiers").
/// The kernel is always the separable Gaussian; the tiers pick the base
/// resolution: `auto` probes the GPU at startup (0.5× on capable adapters,
/// 0.25× on software ones), `high` forces the finest 0.75×, `low` the
/// cheapest 0.25×.
const BLUR_QUALITY: &[&str] = &["auto", "high", "low"];
const EFFECTS: &[(&str, PropDef)] = &[
    ("ripple", pnt(PropType::Color)),
    ("ripple_active", pnt(PropType::Bool)),
    ("ripple_radius", pnt(PropType::Float)),
    ("ripple_duration", pnt(PropType::Int)),
    ("blur", pnt(PropType::Float)),
    ("backdrop_tint", pnt(PropType::Color)),
    ("blur_saturation", pnt(PropType::Float)),
    ("blur_quality", pnt(PropType::Enum(BLUR_QUALITY))),
];
const TEXT_PROPS: &[(&str, PropDef)] = &[
    ("typo", lay(PropType::Typo)),
    ("color", pnt(PropType::Color)),
    ("size", lay(PropType::Int)),
    // RFC-0034: the four keywords stay as aliases for 100/400/500/700, and a
    // numeric value addresses the CSS axis directly, which is what a variable
    // font's `wght` takes and how designers already write it.
    // Layout class, not paint: the weight changes the shaped width, so a
    // heading that gets heavier gets wider and its box has to be recomputed.
    // It was paint-class while it did nothing at all, which was harmless then
    // and would now be a lie any relayout gate built on this would believe.
    ("weight", lay(PropType::WeightAxis)),
    // RFC-0034: selects one of the families declared in `[assets.fonts]`.
    // Layout class for the same reason `weight` is: two faces set the same
    // string to different widths, so a heading that changes face changes the
    // box it needs.
    ("font", lay(PropType::FontFamily)),
    ("align", lay(PropType::Enum(ALIGN))),
    ("lines", lay(PropType::Int)),
    ("wrap", lay(PropType::Bool)),
];

const POINTER_EVENTS: &[&str] = &[
    "tap",
    "click", // alias of "tap" (RFC-0012 §A)
    "pointer_down",
    "pointer_up",
    "pointer_move",
    "pointer_enter",
    "pointer_exit",
    "hover",
    "long_press",
    "double_tap",
    "secondary",
    "wheel",
    // `focus =>`/`blur =>` sugar (RFC-0012 S2) makes *any* interactive
    // element focusable on demand (`register_focusable` creates a fresh
    // internal `focused_sig` when `focused:` wasn't given), so, unlike
    // `key_down`/`key_up` below, these aren't gated behind an intrinsic's
    // *default* focusability.
    "focus",
    "blur",
];
const KEY_EVENTS: &[&str] = &["key_down", "key_up"];

/// The accepted vocabulary of one intrinsic.
pub struct Intrinsic {
    /// Number of positional `(...)` content arguments.
    pub arity: usize,
    /// The content type, when `arity > 0`.
    pub content: Option<PropType>,
    /// Whether the intrinsic accepts a `{ … }` children block.
    pub children: bool,
    /// Whether the intrinsic is focusable by default.
    pub focusable: bool,
    /// Whether attaching a pointer/keyboard listener registers a hit rect.
    pub interactive: bool,
    props: HashMap<&'static str, PropDef>,
    events: HashSet<&'static str>,
}

impl Intrinsic {
    /// Returns the type of property `name`, if recognized.
    #[must_use]
    pub fn property_type(&self, name: &str) -> Option<PropType> {
        self.props.get(name).map(|d| d.ty)
    }

    /// Returns which half of the frame property `name` can change
    /// (RFC-0032 §R2), if recognized.
    #[must_use]
    pub fn property_class(&self, name: &str) -> Option<AttrClass> {
        self.props.get(name).map(|d| d.class)
    }

    /// Returns `true` if `name` is a recognized event.
    #[must_use]
    pub fn has_event(&self, name: &str) -> bool {
        self.events.contains(name)
    }

    /// Returns an iterator over all recognized property names and their types.
    pub fn properties(&self) -> impl Iterator<Item = (&'static str, PropType)> + '_ {
        self.props.iter().map(|(&k, v)| (k, v.ty))
    }

    /// Returns an iterator over all recognized event names.
    pub fn events(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.events.iter().copied()
    }
}

fn props_from(groups: &[&[(&'static str, PropDef)]]) -> HashMap<&'static str, PropDef> {
    let mut m = HashMap::new();
    for g in groups {
        for &(k, v) in *g {
            m.insert(k, v);
        }
    }
    // Universal props. A style class can carry anything, so it is classified
    // by its most consequential possibility rather than its most common one.
    m.insert("style", lay(PropType::Class));
    // RFC-0024: `selected`/`invalid` are universal opt-in pseudo-state props,
    // any element can drive the `selected`/`invalid` style states (nav items,
    // tabs, chips, form fields).
    m.insert("selected", pnt(PropType::Bool));
    m.insert("invalid", pnt(PropType::Bool));
    m
}

fn events_from(focusable: bool, extra: &[&'static str]) -> HashSet<&'static str> {
    let mut s: HashSet<&'static str> = POINTER_EVENTS.iter().copied().collect();
    if focusable {
        s.extend(KEY_EVENTS.iter().copied());
    }
    s.extend(extra.iter().copied());
    s
}

/// Parses a `Grid` `columns:`/`rows:` template string (RFC-0018) into engine
/// grid tracks. Accepted, whitespace-separated: `Nfr` (flexible fraction), a
/// bare number (fixed logical px), `auto`, and `repeat(N, <track>)` (expanded to
/// `N` copies of a single inner track). Returns `None` on any malformed token or
/// an empty template, the caller turns that into a
/// [`CompileError::InvalidGridTemplate`].
///
/// [`CompileError::InvalidGridTemplate`]: crate::diagnostics::CompileError::InvalidGridTemplate
#[must_use]
pub fn parse_grid_template(s: &str) -> Option<Vec<byard_core::atlas::GridTrack>> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(after) = rest.strip_prefix("repeat(") {
            let close = after.find(')')?;
            let inner = &after[..close];
            rest = &after[close + 1..];
            let (count_s, track_s) = inner.split_once(',')?;
            let count: usize = count_s.trim().parse().ok()?;
            if count == 0 {
                return None;
            }
            let track = parse_grid_track(track_s.trim())?;
            for _ in 0..count {
                out.push(track);
            }
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let (tok, tail) = rest.split_at(end);
            rest = tail;
            out.push(parse_grid_track(tok)?);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Parses one grid track token (`1fr`, `100`, `auto`), the leaf of
/// [`parse_grid_template`].
fn parse_grid_track(t: &str) -> Option<byard_core::atlas::GridTrack> {
    use byard_core::atlas::GridTrack;
    if t == "auto" {
        return Some(GridTrack::Auto);
    }
    if let Some(fr) = t.strip_suffix("fr") {
        return fr.trim().parse::<f32>().ok().map(GridTrack::Fr);
    }
    t.parse::<f32>().ok().map(GridTrack::Px)
}

/// The [`Intrinsic`] a registered native view presents (RFC-0039).
///
/// A native view is looked up here, in the same call an intrinsic is, because
/// the RFC's central claim is that the two are indistinguishable at the call
/// site: same validation rules, same prop typing, same `AttrClass`, same
/// unknown-prop diagnostic with a span. Building a second, parallel checking
/// path for package elements is exactly how that claim would stop being true
/// the first time one of the two paths gained a rule.
///
/// Layout, decoration, transform and effect props come with it, so a native
/// view can be given a `width`, a `bg` or an `opacity` like anything else; the
/// declared props are added on top, with the classes the view declared.
fn native_view(info: &byard_core::render::NativeViewInfo) -> Intrinsic {
    use byard_core::render::NativePropType as N;
    let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
    for prop in info.props {
        let ty = match prop.ty {
            N::Int => PropType::Int,
            N::Float => PropType::Float,
            N::Bool => PropType::Bool,
            N::Str => PropType::Str,
            N::Color => PropType::Color,
            N::Vec2 => PropType::Vec2,
            N::Floats => PropType::List,
        };
        props.insert(
            prop.name,
            PropDef {
                ty,
                class: if prop.layout {
                    AttrClass::Layout
                } else {
                    AttrClass::Paint
                },
            },
        );
    }
    Intrinsic {
        arity: 0,
        content: None,
        children: false,
        focusable: false,
        interactive: true,
        props,
        events: events_from(false, info.events),
    }
}

/// Looks up the intrinsic named `name` (RFC-0005 §4 table), or the native view
/// registered under it (RFC-0039).
#[must_use]
pub fn lookup(name: &str) -> Option<Intrinsic> {
    if let Some(info) = byard_core::render::registry::info(name) {
        return Some(native_view(&info));
    }
    lookup_intrinsic(name)
}

/// The built-in half of [`lookup`].
#[must_use]
fn lookup_intrinsic(name: &str) -> Option<Intrinsic> {
    let container = |dir_default: bool| {
        let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
        if dir_default {
            props.insert("direction", lay(PropType::Enum(DIRECTION)));
        }
        props.insert("focused", pnt(PropType::Bool));
        props.insert("disabled", pnt(PropType::Bool));
        // RFC-0017: a child of an `Overlay` may carry an `anchor` placing it
        // within the viewport. Harmless outside an overlay (no-op in normal
        // flow), so it lives on every container rather than a special case.
        props.insert("anchor", lay(PropType::Enum(ANCHOR)));
        // RFC-0036: element-relative anchoring, the other half of the same
        // placement story. `anchor_to` names an element tagged `as <name>`
        // earlier in the view; the rest say where against it.
        //
        // Spelled `anchor_edge`/`anchor_align`/`anchor_gap` rather than the
        // RFC's bare `edge`/`align`/`gap`, because `align` and `gap` are
        // already layout properties on every container and a second meaning
        // for them would be decided by whether an overlay happened to be the
        // parent — the kind of context-dependence this catalogue exists to
        // prevent.
        props.insert("anchor_to", lay(PropType::Str));
        props.insert("anchor_edge", lay(PropType::Enum(ANCHOR_EDGE)));
        props.insert("anchor_align", lay(PropType::Enum(ANCHOR_ALIGN)));
        props.insert("anchor_gap", lay(PropType::Len));
        props.insert("anchor_flip", lay(PropType::Bool));
        // RFC-0018: grid child-placement props. Valid on any container child of a
        // `Grid`; harmless (no-op) outside a grid, like `anchor`, so they live on
        // every container rather than being special-cased.
        props.insert("col", lay(PropType::Int));
        props.insert("row", lay(PropType::Int));
        props.insert("col_span", lay(PropType::Int));
        props.insert("row_span", lay(PropType::Int));
        Intrinsic {
            arity: 0,
            content: None,
            children: true,
            focusable: false,
            interactive: true,
            props,
            // RFC-0036: an anchored overlay child may carry `dismiss =>`,
            // which fires on a press outside both it and its anchor. Named
            // like RFC-0017's modal dismissal because it is the same intent,
            // and implemented differently because a dropdown must not swallow
            // the events of the page beneath it. Harmless on a container that
            // anchors to nothing, which the checker reports rather than
            // silently ignoring.
            events: events_from(false, &["dismiss"]),
        }
    };
    Some(match name {
        "Box" => container(true),
        "Column" | "Row" => container(false),
        "Spacer" => Intrinsic {
            arity: 0,
            content: None,
            children: false,
            focusable: false,
            interactive: false,
            props: props_from(&[&[("grow", lay(PropType::Int)), ("basis", lay(PropType::Int))]]),
            events: HashSet::new(),
        },
        "Text" => Intrinsic {
            arity: 1,
            content: Some(PropType::Str),
            children: false,
            focusable: false,
            interactive: true,
            props: props_from(&[
                TEXT_PROPS,
                &[("m", lay(PropType::Len)), ("width", lay(PropType::Int))],
            ]),
            events: events_from(false, &[]),
        },
        "Button" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TEXT_PROPS, TRANSFORM]);
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            Intrinsic {
                arity: 1,
                content: Some(PropType::Str),
                children: false,
                focusable: true,
                interactive: true,
                props,
                events: events_from(true, &[]),
            }
        }
        "TextField" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TEXT_PROPS, TRANSFORM]);
            props.insert("placeholder", lay(PropType::Str));
            props.insert("value", lay(PropType::Str));
            props.insert("bind", lay(PropType::Str));
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            Intrinsic {
                arity: 0,
                content: None,
                children: false,
                focusable: true,
                interactive: true,
                props,
                events: events_from(true, &["change", "input", "submit"]),
            }
        }
        "Toggle" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("value", lay(PropType::Bool));
            props.insert("bind", lay(PropType::Bool));
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            Intrinsic {
                arity: 0,
                content: None,
                children: false,
                focusable: true,
                interactive: true,
                props,
                events: events_from(true, &["change"]),
            }
        }
        "Slider" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            for k in ["min", "max", "step"] {
                props.insert(k, pnt(PropType::Float));
            }
            props.insert("value", lay(PropType::Float));
            props.insert("bind", lay(PropType::Float));
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            Intrinsic {
                arity: 0,
                content: None,
                children: false,
                focusable: true,
                interactive: true,
                props,
                events: events_from(true, &["change"]),
            }
        }
        // RFC-0018: `Checkbox`, a boolean toggle with a distinct square
        // identity from `Toggle`. Reflected two-way `value: Bool` (or `bind:`);
        // `true` = checked. `indeterminate: Bool` renders the mixed-state dash.
        // Focusable by default (Space toggles). Fires `change` on flip. Owns its
        // visuals (square container + engine-drawn checkmark), so `bg` is the
        // checked accent, not a full-rect slab, mirrors `Toggle`'s model.
        "Checkbox" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("value", lay(PropType::Bool));
            props.insert("bind", lay(PropType::Bool));
            props.insert("indeterminate", lay(PropType::Bool));
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            Intrinsic {
                arity: 0,
                content: None,
                children: false,
                focusable: true,
                interactive: true,
                props,
                events: events_from(true, &["change"]),
            }
        }
        // RFC-0018: `RadioButton`, single selection within a group. `value: Str`
        // is this button's own identity within the group; `bind: Str` is the
        // shared group `var`. The button is selected when `bind == value`; tapping
        // it writes its `value` to the group var, so the previously selected
        // sibling deselects reactively (automatic mutual exclusion). Focusable by
        // default; arrow keys move selection within the group (wrapping). Owns its
        // visuals (outer ring + inner dot), so `bg` is the selected accent.
        "RadioButton" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("value", lay(PropType::Str));
            props.insert("bind", lay(PropType::Str));
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            Intrinsic {
                arity: 0,
                content: None,
                children: false,
                focusable: true,
                interactive: true,
                props,
                events: events_from(true, &["change"]),
            }
        }
        "Image" => {
            let mut props = props_from(&[LAYOUT]);
            props.insert("radius", pnt(PropType::Len));
            // RFC-0031 §S3: `smooth` goes wherever `radius` goes.
            props.insert("smooth", pnt(PropType::Float));
            props.insert("opacity", pnt(PropType::Float));
            props.insert("fit", lay(PropType::Enum(FIT)));
            Intrinsic {
                arity: 1,
                content: Some(PropType::Str),
                children: false,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &[]),
            }
        }
        "ScrollView" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("axis", lay(PropType::Enum(AXIS)));
            props.insert("offset", lay(PropType::Vec2));
            // RFC-0005 windowed layout: opt-in list virtualization. `windowed`
            // materialises only the visible slice of a uniform-height vertical
            // list; `row_height` is that fixed per-row **stride** the window math
            // indexes by. It MUST equal each row's laid-out outer height, because
            // windowing lays the list out gap-free (a flex `gap` can't survive
            // virtualization), so fold any inter-row spacing into the row itself
            // (its `height` or a `mb` margin), not the container's `gap`. A
            // `row_height` that disagrees with the real stride makes the content
            // jump as rows scroll past the edge.
            props.insert("windowed", lay(PropType::Bool));
            props.insert("row_height", lay(PropType::Int));
            // RFC-0021 advanced scroll behaviours (all default to off, a plain
            // `ScrollView` is unchanged).
            props.insert("snap", lay(PropType::Enum(SNAP)));
            props.insert("snap_align", lay(PropType::Enum(SNAP_ALIGN)));
            props.insert("snap_spring", lay(PropType::Spring));
            props.insert("pull_refresh", lay(PropType::Bool));
            props.insert("refreshing", lay(PropType::Bool));
            props.insert("collapse_header", lay(PropType::Bool));
            props.insert("collapse_min", lay(PropType::Int));
            props.insert("collapse_parallax", lay(PropType::Float));
            props.insert("page", lay(PropType::Int));
            props.insert("page_count", lay(PropType::Int));
            props.insert("end_threshold", lay(PropType::Float));
            Intrinsic {
                arity: 0,
                content: None,
                children: true,
                focusable: false,
                interactive: true,
                props,
                events: events_from(
                    false,
                    &[
                        "scroll",
                        "end_reached",
                        "page_change",
                        "scroll_end",
                        "refresh",
                    ],
                ),
            }
        }
        // The twelfth intrinsic (RFC-0009 §1, RFC-0005 amendment): an MSDF vector
        // glyph. Content arity 1 = an asset handle (a `Str` path resolved against
        // the asset table, like `Image`). Props: `size`, `color`, `m`, `opacity`,
        // universal `style`; pointer events match `Image`. No children. Routes to
        // the `VectorMSDF` pipeline.
        "VectorIcon" => {
            let mut props: HashMap<&'static str, PropDef> = HashMap::new();
            props.insert("size", lay(PropType::Int));
            props.insert("color", pnt(PropType::Color));
            props.insert("m", lay(PropType::Len));
            props.insert("opacity", pnt(PropType::Float));
            props.insert("style", lay(PropType::Class));
            Intrinsic {
                arity: 1,
                content: Some(PropType::Str),
                children: false,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &[]),
            }
        }
        // RFC-0020: a fixed-size programmatic drawing surface. Content: none.
        // Children: shape commands only (`arc`, `circle`, `line`, `rect`,
        // `path`, `bezier`, `text`), validated by [`validate_canvas`], not the
        // generic children rule. Props: `width`/`height` (required, a canvas
        // never sizes to content), `bg` (background fill), `grow`, margins,
        // `opacity`, universal `style`. Events: all pointer events, hit-tested
        // against the canvas rect only (individual shapes are not hit-testable;
        // RFC-0020 resolved question).
        "Canvas" => {
            let mut props: HashMap<&'static str, PropDef> = HashMap::new();
            props.insert("width", lay(PropType::Int));
            props.insert("height", lay(PropType::Int));
            props.insert("bg", pnt(PropType::Color));
            props.insert("grow", lay(PropType::Int));
            props.insert("m", lay(PropType::Len));
            props.insert("opacity", pnt(PropType::Float));
            // RFC-0031 §S10: `morph: <scalar>` reinterprets the canvas's shapes
            // as a *sequence* and indexes it. Paint-class, so it animates
            // through the ordinary chokepoint, a morph that relaid out the
            // tree at the display rate is precisely what INV-8 forbids.
            props.insert("morph", pnt(PropType::Float));
            // RFC-0031 §S7: `fuse: <px>` is the smoothing radius, the distance
            // over which two surfaces bridge into one. Paint-class and
            // animatable for the same reason `morph` is: `k` is an ordinary
            // scalar, and an animating fusion is new per-instance data rather
            // than a re-tessellation.
            props.insert("fuse", pnt(PropType::Float));
            props.insert("style", lay(PropType::Class));
            Intrinsic {
                arity: 0,
                content: None,
                children: true,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &[]),
            }
        }
        // RFC-0017: the overlay escape-hatch. Content: none. Children: the
        // overlay's floating subtree, laid out against the viewport rather than
        // the parent's flow. Props: `modal` (default true, captures all input
        // behind a scrim) and `dismiss_on_outside` (default true when modal). It
        // is layout-only itself (occupies zero space in its parent); its children
        // route to their own pipelines. The `dismiss` event fires when a modal
        // overlay's scrim is tapped or `Escape` is pressed.
        "Overlay" => {
            let mut props: HashMap<&'static str, PropDef> = HashMap::new();
            props.insert("modal", lay(PropType::Bool));
            props.insert("dismiss_on_outside", lay(PropType::Bool));
            props.insert("style", lay(PropType::Class));
            Intrinsic {
                arity: 0,
                content: None,
                children: true,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &["dismiss"]),
            }
        }
        // RFC-0018: `Grid`, a CSS-grid container. Content: none. Children: any
        // (auto-placed into the tracks left-to-right, top-to-bottom, or placed
        // explicitly via child `col`/`row`/`col_span`/`row_span`). Props: Layout +
        // Decoration + Transform, plus `columns`/`rows` (GridTemplate strings) and
        // per-axis `col_gap`/`row_gap` (override the shared `gap`). Pipeline: the
        // generic `DecoratedBox` background, same as `Box`.
        "Grid" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            props.insert("anchor", lay(PropType::Enum(ANCHOR)));
            // A Grid can itself be a grid child, so it carries the placement props.
            props.insert("col", lay(PropType::Int));
            props.insert("row", lay(PropType::Int));
            props.insert("col_span", lay(PropType::Int));
            props.insert("row_span", lay(PropType::Int));
            // Grid-container props.
            props.insert("columns", lay(PropType::Str));
            props.insert("rows", lay(PropType::Str));
            props.insert("col_gap", lay(PropType::Int));
            props.insert("row_gap", lay(PropType::Int));
            Intrinsic {
                arity: 0,
                content: None,
                children: true,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &[]),
            }
        }
        // RFC-0018: `ZStack`, overlapping children within the layout tree.
        // Content: none. Children: any (all occupy the same rect; last on top).
        // Props: Layout + Decoration + Transform + `alignment: Align2D` (how
        // children smaller than the stack are positioned; default `center`).
        // Pipeline: the generic `DecoratedBox` background, same as `Box`.
        // RFC-0037 clip masks: a container whose subtree is clipped to its own
        // box, optionally with rounded corners. Content: none. Children: any.
        //
        // Spelled as an element rather than the RFC's lowercase `clip(...)`
        // form: the RFC itself cites `Overlay` as the precedent for "a
        // container form, not a style prop", and `Overlay` is an element. That
        // keeps the whole attribute, style and state machinery working on it
        // for free, where a new lexical form would need its own grammar and
        // would still have to grow all of it back.
        "Clip" => {
            let mut props = props_from(&[LAYOUT, TRANSFORM]);
            // The corner radius of the mask. Absent (or zero) is a plain
            // rectangular clip, which stays a pure scissor.
            props.insert("rrect", lay(PropType::Len));
            props.insert("col", lay(PropType::Int));
            props.insert("row", lay(PropType::Int));
            props.insert("col_span", lay(PropType::Int));
            props.insert("row_span", lay(PropType::Int));
            Intrinsic {
                arity: 0,
                content: None,
                children: true,
                focusable: false,
                interactive: false,
                props,
                events: events_from(false, &[]),
            }
        }
        "ZStack" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("focused", pnt(PropType::Bool));
            props.insert("disabled", pnt(PropType::Bool));
            props.insert("anchor", lay(PropType::Enum(ANCHOR)));
            // A ZStack can itself be a grid child.
            props.insert("col", lay(PropType::Int));
            props.insert("row", lay(PropType::Int));
            props.insert("col_span", lay(PropType::Int));
            props.insert("row_span", lay(PropType::Int));
            props.insert("alignment", lay(PropType::Enum(ALIGN2D)));
            Intrinsic {
                arity: 0,
                content: None,
                children: true,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &[]),
            }
        }
        // RFC-0026: `NavStack`, a push/pop stack of screens. Content: none.
        // Children: `route` blocks only (enforced by [`validate_nav`], since
        // they are not elements). Props: Layout + Decoration + Transform, plus
        // the reflected `path` (the navigation state, an ordinary `var`, never
        // a controller object), the `transition` family, the Cupertino
        // `swipe_back` edge gesture, `deep_link` (accept OS URL intents) and
        // `max_depth` (the runaway-push guard). Event: `route_change`, fired
        // once a navigation settles. Pipeline: a stack container, during a
        // transition two screens are laid out in the same cell and composited
        // with per-screen transform/opacity.
        "NavStack" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("transition", lay(PropType::Enum(TRANSITION)));
            props.insert("swipe_back", lay(PropType::Bool));
            props.insert("deep_link", lay(PropType::Bool));
            props.insert("max_depth", lay(PropType::Int));
            props.insert("anchor", lay(PropType::Enum(ANCHOR)));
            props.insert("col", lay(PropType::Int));
            props.insert("row", lay(PropType::Int));
            props.insert("col_span", lay(PropType::Int));
            props.insert("row_span", lay(PropType::Int));
            Intrinsic {
                // The navigation state is the container's content, `NavStack(
                // path: navPath)`. It is *required*: a navigation container with
                // nothing driving it is a container with no navigation.
                arity: 1,
                content: Some(PropType::Str),
                children: true,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &["route_change"]),
            }
        }
        // RFC-0026: `NavHost`, the flat tab container. Content: none. Children:
        // `tab` blocks only. Props: as `NavStack` minus the stack-only ones,
        // with the reflected `active` naming the visible tab; `transition`
        // defaults to `fade`, since a tab switch has no push direction.
        "NavHost" => {
            let mut props = props_from(&[LAYOUT, DECORATION, EFFECTS, TRANSFORM]);
            props.insert("transition", lay(PropType::Enum(TRANSITION)));
            props.insert("anchor", lay(PropType::Enum(ANCHOR)));
            props.insert("col", lay(PropType::Int));
            props.insert("row", lay(PropType::Int));
            props.insert("col_span", lay(PropType::Int));
            props.insert("row_span", lay(PropType::Int));
            Intrinsic {
                // `NavHost(active: activeTab)`, the visible tab's name.
                arity: 1,
                content: Some(PropType::Str),
                children: true,
                focusable: false,
                interactive: true,
                props,
                events: events_from(false, &[]),
            }
        }
        _ => return None,
    })
}

/// Validates a navigation container's children (RFC-0026): a `NavStack` holds
/// `route` blocks, a `NavHost` holds `tab` blocks, and nothing else, an
/// element, a declaration, or the wrong keyword is a precise diagnostic rather
/// than a silently ignored child. Each accepted case's pattern is compiled here
/// too, so a malformed pattern is caught at check time, not first navigation.
///
/// Call alongside [`validate_element`], which covers the container's own
/// attributes.
#[must_use]
pub fn validate_nav(el: &ElementNode) -> Vec<CompileError> {
    use crate::parser::ast::RouteKind;

    let want = if el.name.as_str() == "NavStack" {
        RouteKind::Route
    } else {
        RouteKind::Tab
    };
    let mut errs = Vec::new();
    for child in &el.children {
        match child {
            Member::Route {
                kind,
                pattern,
                pattern_span,
                ..
            } if *kind == want => {
                if let Err(err) = crate::interp::nav::RoutePattern::compile(pattern, *pattern_span)
                {
                    errs.push(err);
                }
            }
            // The right shape, the wrong container, say so directly.
            Member::Route { kind, span, .. } => errs.push(CompileError::MisplacedNavCase {
                span: *span,
                keyword: kind.as_str().to_string(),
                container: kind.container().to_string(),
            }),
            other => errs.push(CompileError::NavCaseRequired {
                span: member_span(other),
                container: el.name.as_str().to_string(),
                keyword: want.as_str().to_string(),
                found: describe_member(other),
            }),
        }
    }
    errs
}

/// A short description of a non-case member, for [`validate_nav`]'s message.
fn describe_member(m: &Member) -> String {
    match m {
        Member::Element(e) => format!("the element `{}`", e.name.as_str()),
        Member::Var { .. } => "a `var` declaration".to_string(),
        Member::Let { .. } => "a `let` declaration".to_string(),
        Member::Fn { .. } => "a `fn` declaration".to_string(),
        Member::Inject { .. } => "an `inject` declaration".to_string(),
        Member::For { .. } => "a `for` loop".to_string(),
        Member::When { .. } => "a `when` block".to_string(),
        Member::Style { .. } => "a `style` block".to_string(),
        Member::Route { kind, .. } => format!("a `{}` block", kind.as_str()),
        Member::Timer { every, .. } => {
            format!("an `{}` timer", if *every { "every" } else { "after" })
        }
        Member::Lifecycle { on_mount, .. } => {
            format!(
                "an `on {}` effect",
                if *on_mount { "mount" } else { "unmount" }
            )
        }
        Member::Measure { .. } => "an `on measure` event".to_string(),
        Member::Expr(_) => "an expression".to_string(),
    }
}

/// The source span of any member (mirrors the private helper in `eval`, kept
/// local so this module stays self-contained).
fn member_span(m: &Member) -> crate::diagnostics::Span {
    match m {
        Member::Var { span, .. }
        | Member::Let { span, .. }
        | Member::Fn { span, .. }
        | Member::Inject { span, .. }
        | Member::For { span, .. }
        | Member::When { span, .. }
        | Member::Route { span, .. }
        | Member::Lifecycle { span, .. }
        | Member::Timer { span, .. }
        | Member::Measure { span, .. }
        | Member::Style { span, .. } => *span,
        Member::Element(e) => e.span,
        Member::Expr(e) => e.span(),
    }
}

/// Validates `el` against the §5 contract, returning every diagnostic it
/// produces (possibly several). `known_views` are the user `ViewDecl` names in
/// scope, so a non-intrinsic element that resolves to a view is not an error.
#[must_use]
pub fn validate_element(
    el: &ElementNode,
    attrs: &[Attr],
    known_views: &[&str],
) -> Vec<CompileError> {
    let mut errs = Vec::new();
    let name = el.name.as_str();

    // Rule 1, name resolution.
    let Some(info) = lookup(name) else {
        if !known_views.contains(&name) {
            // A shape command reached the ordinary element path, it was
            // written outside a `Canvas` body (RFC-0020 §3). More precise
            // than the generic unknown-view diagnostic.
            if is_shape_command(name) {
                errs.push(CompileError::ShapeOutsideCanvas {
                    span: el.span,
                    name: name.to_string(),
                });
                return errs;
            }
            let hint = closest_match(
                name,
                INTRINSIC_NAMES
                    .iter()
                    .copied()
                    .chain(known_views.iter().copied()),
            )
            .map(str::to_string);
            errs.push(CompileError::UnknownView {
                span: el.span,
                name: name.to_string(),
                hint,
            });
        }
        // A user view: its own body is validated when that view is checked.
        return errs;
    };

    // Rule 2, content arity.
    if el.content.len() != info.arity {
        errs.push(CompileError::ArityMismatch {
            span: el.span,
            name: name.to_string(),
            expected: info.arity,
            found: el.content.len(),
        });
    } else if let Some(ty) = info.content {
        // Rule 3, content type.
        for arg in &el.content {
            if let Some(err) = check_value_type(ty, &arg.value) {
                errs.push(err);
            }
        }
    }

    // Rule 8, children on a childless intrinsic.
    if !info.children && !el.children.is_empty() {
        errs.push(CompileError::UnexpectedChildren {
            span: el.span,
            name: name.to_string(),
        });
    }

    // Rules 4–6, per attribute.
    for attr in attrs {
        let an = attr.name.as_str();
        let is_prop = matches!(attr.kind, AttrKind::Prop { .. });
        let prop_def = info.props.get(an).copied();
        let prop_ty = prop_def.map(|d| d.ty);
        let is_event = info.events.contains(an);

        if prop_ty.is_none() && !is_event {
            // Rule 4, unknown attribute.
            let hint = closest_match(
                an,
                info.props
                    .keys()
                    .copied()
                    .chain(info.events.iter().copied()),
            )
            .map(str::to_string);
            errs.push(CompileError::UnknownAttribute {
                span: attr.span,
                name: an.to_string(),
                hint,
            });
            continue;
        }

        // Rule 5, separator/kind.
        if is_prop && prop_ty.is_none() && is_event {
            errs.push(CompileError::WrongAttributeSeparator {
                span: attr.span,
                name: an.to_string(),
                expected_property: false,
            });
            continue;
        }
        if !is_prop && prop_ty.is_some() && !is_event {
            errs.push(CompileError::WrongAttributeSeparator {
                span: attr.span,
                name: an.to_string(),
                expected_property: true,
            });
            continue;
        }

        // Rule 6, attribute value type.
        if let (AttrKind::Prop { value }, Some(def)) = (&attr.kind, prop_def) {
            let ty = def.ty;
            // RFC-0032 §Q8 / RFC-0010 INV-8: the class comes from *this*
            // intrinsic's own definition, not from a global name list, so
            // `align` on a `Column` and `align` on a `Text` are answered
            // separately and an attribute cannot be added without an answer.
            let is_layout = def.class == AttrClass::Layout;
            // RFC-0010: `value with anim.*(…)`, reject an animation on a layout
            // property (it can't animate on the GPU), otherwise validate every
            // curve in the (possibly nested) chain and type-check the innermost
            // target value. The chain walk matters: `(x with a) with b` must not
            // let its inner curve or value slip past unchecked.
            // The layout half of that rule is asked of the whole expression, not
            // just its outermost node. `width: (x with anim.linear(200ms)) + 0`
            // animates a layout property exactly as `width: x with …` does, the
            // `with` is simply written one level in, and matching only the top
            // node let it through to be sampled during layout, where it relaid
            // out every frame *and* resolved to a float the integer-valued
            // dimension readers drop, silently collapsing the element to its
            // default size. The rule holds however the animation is written.
            if is_layout && animated_anywhere(value) {
                errs.push(CompileError::LayoutPropNotAnimatable {
                    span: value.span(),
                    prop: an.to_string(),
                });
            } else if let Expr::Animated { .. } = value {
                {
                    let mut target = value;
                    while let Expr::Animated {
                        value: inner, anim, ..
                    } = target
                    {
                        // RFC-0025: the whole motion spec is validated, not just
                        // the curve, a misspelt `revrse:` or an out-of-range
                        // `repeat:` is a diagnostic, never a silently ignored
                        // modifier.
                        if let Err(err) = crate::interp::anim::resolve_motion(anim) {
                            errs.push(err);
                        }
                        target = inner;
                    }
                    if let Some(err) = check_value_type(ty, target) {
                        errs.push(err);
                    }
                }
            } else if let Some(track) = crate::interp::anim::resolve_keyframes(value) {
                // Same rule for the keyframe form (RFC-0025 §3).
                // RFC-0025 §3: `anim.keyframes(…)` stands in value position. It
                // is rejected on a layout property for the same reason `with`
                // is (a relayout every frame, INV-8), handled above, for the
                // nested form too, and each step's value is type-checked
                // against the property like any other value.
                {
                    match track {
                        Ok(track) => errs.extend(
                            track
                                .steps
                                .iter()
                                .filter_map(|step| check_value_type(ty, step.value)),
                        ),
                        Err(err) => errs.push(err),
                    }
                }
            } else if let Some(err) = check_value_type(ty, value) {
                errs.push(err);
            }
        }
    }

    errs
}

/// Whether `name` is a layout-affecting attribute, one whose value feeds Taffy
/// and so cannot be GPU-animated (RFC-0010 §"Layout properties"). Covers the
/// [`LAYOUT`] group plus the container `direction`.
/// Whether an animation appears **anywhere** inside `value`, a `with` clause or
/// an `anim.keyframes(…)` call, at any depth (RFC-0010 §"Layout properties").
///
/// Only layout properties ask this. Everywhere else an animation is a value like
/// any other and its position in the expression is the author's business; on a
/// layout property it is prohibited outright, and a prohibition that only
/// inspects the outermost node is one that anyone can step around by accident,
/// `(x with anim.linear(200ms)) * 2` is as animated as `x with …`.
fn animated_anywhere(value: &Expr) -> bool {
    fn args(args: &[crate::parser::ast::Arg]) -> bool {
        args.iter().any(|a| animated_anywhere(&a.value))
    }
    if crate::interp::anim::is_keyframes_call(value) {
        return true;
    }
    match value {
        Expr::Animated { .. } => true,
        Expr::IntLit(..)
        | Expr::FloatLit(..)
        | Expr::AngleLit(..)
        | Expr::StrLit(..)
        | Expr::Ident(..)
        | Expr::ClassRef(..)
        | Expr::StyleValue { .. }
        // A controller call is a statement, never a property value, so it can
        // never carry a layout animation.
        | Expr::ControllerCall { .. }
        | Expr::Error(..) => false,
        Expr::Array(items, _) | Expr::Block(items, _) => items.iter().any(animated_anywhere),
        Expr::Tuple(items, _) => args(items),
        Expr::Call {
            callee, args: a, ..
        } => animated_anywhere(callee) || args(a),
        Expr::Member { base, .. } | Expr::Postfix { target: base, .. } => animated_anywhere(base),
        Expr::Lambda { body, .. } | Expr::Unary { rhs: body, .. } => animated_anywhere(body),
        Expr::Assign { target, value, .. } => animated_anywhere(target) || animated_anywhere(value),
        Expr::Index { base, index, .. } => animated_anywhere(base) || animated_anywhere(index),
        Expr::Record { fields, spread, .. } => {
            fields.iter().any(|(_, v)| animated_anywhere(v))
                || spread.as_deref().is_some_and(animated_anywhere)
        }
        Expr::Binary { lhs, rhs, .. }
        | Expr::Merge {
            left: lhs,
            right: rhs,
            ..
        } => animated_anywhere(lhs) || animated_anywhere(rhs),
        Expr::Ternary {
            cond, then, els, ..
        } => animated_anywhere(cond) || animated_anywhere(then) || animated_anywhere(els),
        Expr::KeyframeStep { value, .. } => animated_anywhere(value),

    }
}

/// Light, false-positive-averse type check: only clear scalar-literal
/// mismatches and unknown enum tokens are flagged; identifiers/members (which
/// may resolve to a reactive `var`) are accepted.
fn check_value_type(ty: PropType, value: &Expr) -> Option<CompileError> {
    let span = value.span();
    let mismatch = |what: &str| {
        Some(CompileError::AttributeTypeMismatch {
            span,
            expected: what.to_string(),
        })
    };
    match (ty, value) {
        (PropType::Color, Expr::StrLit(..) | Expr::FloatLit(..)) => mismatch("a color (0xRRGGBB)"),
        (PropType::Int | PropType::Len, Expr::StrLit(..)) => mismatch("an integer length"),
        (PropType::Angle, Expr::StrLit(..) | Expr::IntLit(..) | Expr::FloatLit(..)) => {
            mismatch("an angle (e.g. 90deg, 1.5rad)")
        }
        (PropType::Angle, Expr::Tuple(args, _)) => {
            // Verbose `rotate: (angle: <expr>)`, recurse into the field so a
            // bare number can't slip past the terse-form rejection by hiding in
            // the tuple wrapper (which is otherwise not type-checked).
            match args.as_slice() {
                [arg] if arg.name.as_ref().is_some_and(|n| n.as_str() == "angle") => {
                    check_value_type(PropType::Angle, &arg.value)
                }
                _ => mismatch("an angle (e.g. 90deg) or the verbose form `(angle: 90deg)`"),
            }
        }
        (PropType::Str, Expr::IntLit(..) | Expr::FloatLit(..)) => mismatch("a string"),
        (
            PropType::List,
            Expr::IntLit(..) | Expr::FloatLit(..) | Expr::StrLit(..) | Expr::ClassRef(..),
        ) => mismatch("a list"),
        (PropType::Bool, Expr::IntLit(..) | Expr::StrLit(..) | Expr::FloatLit(..)) => {
            mismatch("a boolean")
        }
        (PropType::Class, e) if !matches!(e, Expr::ClassRef(..)) => {
            mismatch("a style class (.name)")
        }
        (PropType::WeightAxis, Expr::IntLit(n, _)) => {
            if (100..=900).contains(n) {
                None
            } else {
                Some(CompileError::AttributeTypeMismatch {
                    span,
                    expected: "a weight on the 100..=900 axis".to_string(),
                })
            }
        }
        (PropType::WeightAxis, Expr::Ident(sym, _)) => {
            let tok = sym.as_str();
            if WEIGHT.contains(&tok) {
                None
            } else {
                let hint = closest_match(tok, WEIGHT.iter().copied()).map(str::to_string);
                Some(CompileError::AttributeTypeMismatch {
                    span,
                    expected: hint.map_or_else(
                        || format!("one of {WEIGHT:?}, or a number 100..=900"),
                        |h| {
                            format!(
                                "one of {WEIGHT:?} (did you mean `{h}`?), or a number 100..=900"
                            )
                        },
                    ),
                })
            }
        }
        (PropType::WeightAxis, Expr::StrLit(..) | Expr::FloatLit(..)) => {
            mismatch("one of the weight keywords, or a whole number 100..=900")
        }
        (PropType::FontFamily, Expr::IntLit(..) | Expr::FloatLit(..) | Expr::ClassRef(..)) => {
            mismatch("a font family declared in [assets.fonts]")
        }
        (PropType::Enum(set), Expr::Ident(sym, _)) => {
            let tok = sym.as_str();
            if tok == "true" || tok == "false" || set.contains(&tok) {
                None
            } else {
                let hint = closest_match(tok, set.iter().copied()).map(str::to_string);
                Some(CompileError::AttributeTypeMismatch {
                    span,
                    expected: hint.map_or_else(
                        || format!("one of {set:?}"),
                        |h| format!("one of {set:?} (did you mean `{h}`?)"),
                    ),
                })
            }
        }
        _ => None,
    }
}

// ── Canvas shape commands (RFC-0020) ─────────────────────────────────────────

/// The closed set of shape-command names valid inside a `Canvas` body
/// (RFC-0020 §"Shape commands").
pub const SHAPE_COMMAND_NAMES: &[&str] = &[
    "arc", "circle", "line", "rect", "ngon", "path", "bezier", "text",
];

/// The commands a `path { … }` body may contain (RFC-0037 Tier-2).
///
/// A closed set, like the shape commands: a path is built from moves, lines
/// and curves, and a name outside this list is a typo rather than an
/// extension point.
pub const PATH_COMMAND_NAMES: &[&str] = &["move", "line", "cubic", "quad", "close"];

/// Whether `name` is one of the RFC-0037 path commands.
#[must_use]
pub fn is_path_command(name: &str) -> bool {
    PATH_COMMAND_NAMES.contains(&name)
}

/// The parameters one path command takes, in order (RFC-0037).
///
/// Coordinate pairs rather than the RFC sketch's `Vec2` arguments, because
/// every other canvas command spells a point as two numbers and one command
/// spelling it differently is a rule nobody can remember.
#[must_use]
pub fn path_command_params(name: &str) -> ShapeParams {
    match name {
        "move" | "line" => &[("x", PropType::Float), ("y", PropType::Float)],
        "quad" => &[
            ("cx", PropType::Float),
            ("cy", PropType::Float),
            ("x", PropType::Float),
            ("y", PropType::Float),
        ],
        "cubic" => &[
            ("c1x", PropType::Float),
            ("c1y", PropType::Float),
            ("c2x", PropType::Float),
            ("c2y", PropType::Float),
            ("x", PropType::Float),
            ("y", PropType::Float),
        ],
        _ => &[],
    }
}

/// Whether `name` is one of the RFC-0020 shape commands.
#[must_use]
pub fn is_shape_command(name: &str) -> bool {
    SHAPE_COMMAND_NAMES.contains(&name)
}

/// Fill-rule tokens (RFC-0037): which points a self-intersecting path
/// encloses. `nonzero` is the default everywhere that has one.
const WINDING: &[&str] = &["nonzero", "even_odd"];

/// Line-cap tokens (RFC-0020 §"Stroke and fill").
const CAP: &[&str] = &["butt", "round", "square"];
/// Line-join tokens. Accepted for forward-compatibility; v1's shape set has
/// no polyline joints (`rect` corners are exact SDF, `bezier` flattens to
/// round-capped segments), so the value does not yet change rendering.
const JOIN: &[&str] = &["miter", "round", "bevel"];

/// The stroke/fill/paint parameters every geometric shape accepts
/// (RFC-0020 §"Stroke and fill").
const SHAPE_PAINT_PARAMS: &[(&str, PropType)] = &[
    ("stroke", PropType::Color),
    // RFC-0035 §"Canvas arc strokes": a gradient along the *stroke*, spelled
    // apart from `path`'s `gradient:` because they paint different things. One
    // name meaning the fill's ramp on one command and the stroke's on another
    // would be a rule to remember rather than a name to read. Typed loosely
    // here for the same reason `path`'s is: the value is a named tuple that
    // the lowering parses and reports on.
    ("stroke_gradient", PropType::Str),
    ("stroke_width", PropType::Float),
    ("cap", PropType::Enum(CAP)),
    ("join", PropType::Enum(JOIN)),
    ("fill", PropType::Color),
    ("dash", PropType::Vec2),
    ("dash_offset", PropType::Float),
    ("opacity", PropType::Float),
];

/// A static table of shape-parameter `(name, type)` pairs.
pub type ShapeParams = &'static [(&'static str, PropType)];

/// Geometry parameters per shape command: `(required, optional)` name/type
/// pairs, not counting the shared [`SHAPE_PAINT_PARAMS`].
fn shape_geometry(name: &str) -> (ShapeParams, ShapeParams) {
    match name {
        "arc" => (
            &[
                ("cx", PropType::Float),
                ("cy", PropType::Float),
                ("r", PropType::Float),
            ],
            // `start`/`sweep` default to 0°/360°, an unswept arc is a circle.
            &[("start", PropType::Float), ("sweep", PropType::Float)],
        ),
        "circle" => (
            &[
                ("cx", PropType::Float),
                ("cy", PropType::Float),
                ("r", PropType::Float),
            ],
            &[],
        ),
        "line" => (
            &[
                ("x1", PropType::Float),
                ("y1", PropType::Float),
                ("x2", PropType::Float),
                ("y2", PropType::Float),
            ],
            &[],
        ),
        "rect" => (
            &[
                ("x", PropType::Float),
                ("y", PropType::Float),
                ("w", PropType::Float),
                ("h", PropType::Float),
            ],
            // `smooth` (RFC-0031 §S3): the `rect` kind's corner profile, the
            // same 0..1 scalar the box intrinsics take.
            &[("radius", PropType::Float), ("smooth", PropType::Float)],
        ),
        // RFC-0031 §"`ngon`": one parametric kind covering the great majority
        // of the Material 3 Expressive vocabulary. `n` is an integer literal
        // and is *not* animatable (§Q10), `morph` is what changes shape over
        // time.
        "ngon" => (
            &[
                ("cx", PropType::Float),
                ("cy", PropType::Float),
                ("r", PropType::Float),
                ("n", PropType::Int),
            ],
            &[
                ("corner", PropType::Float),
                ("inner", PropType::Float),
                ("rotate", PropType::Angle),
            ],
        ),
        // Tier-1 (`d:`, rasterised through the MSDF atlas) and Tier-2 (a
        // command body, tessellated) are the same command with two spellings,
        // because they are the same shape drawn by whichever pipeline suits
        // it: static art amortises a bake, dynamic geometry does not
        // (RFC-0037's dividing line).
        "path" => (
            &[],
            &[
                ("d", PropType::Str),
                ("gradient", PropType::Str),
                ("winding", PropType::Enum(WINDING)),
            ],
        ),
        "bezier" => (
            &[
                ("x1", PropType::Float),
                ("y1", PropType::Float),
                ("cx1", PropType::Float),
                ("cy1", PropType::Float),
                ("cx2", PropType::Float),
                ("cy2", PropType::Float),
                ("x2", PropType::Float),
                ("y2", PropType::Float),
            ],
            &[],
        ),
        // Canvas `text`: positional content (the string) is handled by the
        // caller; these are its named parameters.
        "text" => (
            &[("x", PropType::Float), ("y", PropType::Float)],
            &[
                ("color", PropType::Color),
                ("size", PropType::Float),
                ("align", PropType::Enum(&["start", "center", "end"])),
            ],
        ),
        _ => (&[], &[]),
    }
}

/// Validates a `path { … }` body (RFC-0037): path commands only, each with the
/// parameters it takes, and a first command that establishes where the path
/// starts.
fn validate_path_body(el: &ElementNode) -> Vec<CompileError> {
    let mut errs = Vec::new();
    let mut first = true;
    for member in &el.children {
        let Member::Element(cmd) = member else {
            // A `for` or a `when` inside a path body is a shape the language
            // cannot check the arity of yet; the shape commands have the same
            // restriction, and lifting it is a change to both.
            continue;
        };
        let name = cmd.name.as_str();
        if !is_path_command(name) {
            errs.push(CompileError::UnknownShapeCommand {
                span: cmd.span,
                name: name.to_string(),
                hint: closest_match(name, PATH_COMMAND_NAMES.iter().copied()).map(str::to_string),
            });
            continue;
        }
        if first && name != "move" {
            // A path that starts with a `line` has no start point to draw
            // from, and picking one silently (the origin, the last path's end)
            // is how a chart ends up with a stray triangle nobody can explain.
            errs.push(CompileError::PathMustStartWithMove { span: cmd.span });
        }
        first = false;

        let params = path_command_params(name);
        let positional = cmd.content.iter().filter(|a| a.name.is_none()).count();
        let named = cmd.content.len() - positional;
        if positional > 0 && positional != params.len() {
            errs.push(CompileError::ArityMismatch {
                span: cmd.span,
                name: name.to_string(),
                expected: params.len(),
                found: positional,
            });
        }
        for arg in &cmd.content {
            let Some(argname) = &arg.name else { continue };
            let known = params.iter().find(|(k, _)| *k == argname.as_str());
            match known {
                Some((_, ty)) => {
                    if let Some(err) = check_value_type(*ty, &arg.value) {
                        errs.push(err);
                    }
                }
                None => errs.push(CompileError::UnknownShapeParam {
                    span: arg.value.span(),
                    shape: name.to_string(),
                    name: argname.as_str().to_string(),
                    hint: closest_match(argname.as_str(), params.iter().map(|(k, _)| *k))
                        .map(str::to_string),
                }),
            }
        }
        if positional == 0 && named < params.len() {
            errs.push(CompileError::ArityMismatch {
                span: cmd.span,
                name: name.to_string(),
                expected: params.len(),
                found: named,
            });
        }
    }
    errs
}

/// Validates a `Canvas` element (RFC-0020 §1): required `width`/`height`
/// props, and a body of shape commands only, each checked against its
/// geometry/paint parameter contract with the same precision as RFC-0005 §5's
/// attribute rules. Call alongside [`validate_element`] (which covers the
/// canvas's own attrs/events through the ordinary intrinsic contract).
#[must_use]
pub fn validate_canvas(el: &ElementNode, attrs: &[Attr]) -> Vec<CompileError> {
    let mut errs = Vec::new();

    // A canvas is a fixed-size surface: it never sizes to content, so both
    // dimensions are required up front.
    let has_prop = |name: &str| {
        attrs
            .iter()
            .any(|a| a.name.as_str() == name && matches!(a.kind, AttrKind::Prop { .. }))
    };
    if !has_prop("width") || !has_prop("height") {
        errs.push(CompileError::CanvasMissingSize { span: el.span });
    }

    validate_canvas_body(&el.children, &mut errs);
    validate_group_cap(el, attrs, &mut errs);
    validate_group_mode(el, attrs, &mut errs);
    errs
}

/// RFC-0031 §Q4/§Q5/§Q6: the rules a *combine mode* imposes on a `Canvas`.
///
/// - `fuse` and `morph` are mutually exclusive ([`ConflictingGroupMode`]). Not a
///   preference: morphing between fused sub-groups needs a member to itself be
///   a group head, which turns a flat contiguous range into a tree and the
///   unrolled fragment loop into recursion.
/// - A fused group has **one** outline, the fused boundary, and it is drawn
///   with the first shape's stroke properties, since that is the only place
///   they can come from. A stroke on any *later* member is therefore inert
///   ([`StrokeInFusionGroup`], a *warning*, since the shape still renders
///   correctly and only the property is ignored).
/// - A fused stroke cannot be dashed ([`DashOnFusedStroke`], an error, since
///   there is no arc length to dash along and an approximation would crawl).
///
/// [`ConflictingGroupMode`]: CompileError::ConflictingGroupMode
/// [`StrokeInFusionGroup`]: CompileError::StrokeInFusionGroup
/// [`DashOnFusedStroke`]: CompileError::DashOnFusedStroke
/// Walks a fused `Canvas` body reporting the stroke properties a member
/// cannot own (RFC-0031 §Q5/§Q6). `seen` counts the shapes walked so far: the
/// first shape's paint *is* the group's, so only a later shape's stroke is
/// genuinely inert.
fn walk_fused_members(members: &[Member], seen: &mut usize, errs: &mut Vec<CompileError>) {
    for member in members {
        match member {
            Member::Element(child) if is_shape_command(child.name.as_str()) => {
                let first = *seen == 0;
                *seen += 1;
                for arg in &child.content {
                    let Some(name) = arg.name.as_ref().map(crate::symbol::Symbol::as_str) else {
                        continue;
                    };
                    // A dash is refused on any member, first included:
                    // there is no arc length along a fused boundary to dash
                    // along, whichever shape asked for it.
                    if name == "dash" || name == "dash_offset" {
                        errs.push(CompileError::DashOnFusedStroke {
                            span: arg.value.span(),
                        });
                    } else if !first && matches!(name, "stroke" | "stroke_width" | "cap" | "join") {
                        errs.push(CompileError::StrokeInFusionGroup {
                            span: arg.value.span(),
                            param: name.to_string(),
                        });
                    }
                }
            }
            Member::For { body, .. } => walk_fused_members(body, seen, errs),
            Member::When { then, els, .. } => {
                walk_fused_members(then, seen, errs);
                if let Some(els) = els {
                    walk_fused_members(els, seen, errs);
                }
            }
            _ => {}
        }
    }
}

fn validate_group_mode(el: &ElementNode, attrs: &[Attr], errs: &mut Vec<CompileError>) {
    let mode_attr = |name: &str| {
        attrs
            .iter()
            .find(|a| a.name.as_str() == name && matches!(a.kind, AttrKind::Prop { .. }))
    };
    let fuse = mode_attr("fuse");
    if let (Some(_), Some(morph)) = (fuse, mode_attr("morph")) {
        errs.push(CompileError::ConflictingGroupMode { span: morph.span });
    }
    let Some(_) = fuse else { return };

    // Per-member stroke properties inside a fusion group. `seen` counts the
    // shapes walked so far: the first one's paint *is* the group's, so only a
    // later shape's stroke is genuinely inert.
    walk_fused_members(&el.children, &mut 0, errs);
}

/// RFC-0031 §S5/§Q3: a `Canvas` that declares a combine mode turns its shapes
/// into one group's members, and a group holds at most
/// [`MAX_GROUP_MEMBERS`](byard_core::frame::MAX_GROUP_MEMBERS) of them.
///
/// Counted over the *written* shape commands. A `for` inside the body can
/// generate members from data, and how many is not knowable here, that case is
/// caught where it becomes knowable, at lowering, against the same cap. This
/// check is the one that names a source position, which is what makes it the
/// useful half.
fn validate_group_cap(el: &ElementNode, attrs: &[Attr], errs: &mut Vec<CompileError>) {
    let grouped = attrs.iter().any(|a| {
        GROUP_MODE_PROPS.contains(&a.name.as_str()) && matches!(a.kind, AttrKind::Prop { .. })
    });
    if !grouped {
        return;
    }
    let mut shapes = Vec::new();
    collect_group_members(&el.children, &mut shapes);
    if shapes.len() > byard_core::frame::MAX_GROUP_MEMBERS {
        errs.push(CompileError::TooManyGroupMembers {
            // The shape that broke the cap, not the canvas: the author needs to
            // know *which* one to move.
            span: shapes[byard_core::frame::MAX_GROUP_MEMBERS],
            max: byard_core::frame::MAX_GROUP_MEMBERS,
            found: shapes.len(),
        });
    }
}

/// The `Canvas` attributes that declare a combine mode (RFC-0031 §S4).
const GROUP_MODE_PROPS: &[&str] = &["fuse", "morph"];

/// Collects the spans of the shape commands a grouped `Canvas` body writes
/// literally, in order. `when` branches are both walked, either can be the one
/// that is taken, and `for` bodies are skipped, since their count is data.
fn collect_group_members(members: &[Member], out: &mut Vec<crate::diagnostics::Span>) {
    for member in members {
        match member {
            Member::Element(child) if is_shape_command(child.name.as_str()) => {
                out.push(child.span);
            }
            Member::When { then, els, .. } => {
                collect_group_members(then, out);
                if let Some(els) = els {
                    collect_group_members(els, out);
                }
            }
            _ => {}
        }
    }
}

/// Validates a `Canvas` body: shape commands, and the `for`/`when` that
/// generate them (RFC-0020 §1).
///
/// `for` and `when` are admitted because a drawing surface whose shape count
/// cannot come from data cannot draw a chart, which is the thing a drawing
/// surface is for. Everything else stays rejected: a `var` or a `style` block
/// inside a canvas has no meaning to give it, and silently ignoring one is how
/// a developer spends an afternoon on a shape that was never going to appear.
fn validate_canvas_body(members: &[Member], errs: &mut Vec<CompileError>) {
    for member in members {
        match member {
            Member::Element(child) if is_shape_command(child.name.as_str()) => {
                errs.extend(validate_shape(child));
            }
            Member::Element(child) => {
                let name = child.name.as_str();
                errs.push(CompileError::UnknownShapeCommand {
                    span: child.span,
                    name: name.to_string(),
                    hint: closest_match(name, SHAPE_COMMAND_NAMES.iter().copied())
                        .map(str::to_string),
                });
            }
            Member::For { body, .. } => validate_canvas_body(body, errs),
            Member::When { then, els, .. } => {
                validate_canvas_body(then, errs);
                if let Some(els) = els {
                    validate_canvas_body(els, errs);
                }
            }
            // Declarations and style blocks are not shape commands. Reported
            // with the member keyword so the message reads naturally.
            Member::Var { span, .. } => push_non_shape(errs, *span, "var"),
            Member::Let { span, .. } => push_non_shape(errs, *span, "let"),
            Member::Fn { span, .. } => push_non_shape(errs, *span, "fn"),
            Member::Inject { span, .. } => push_non_shape(errs, *span, "inject"),
            Member::Style { span, .. } => push_non_shape(errs, *span, "style"),
            Member::Route { kind, span, .. } => push_non_shape(errs, *span, kind.as_str()),
            Member::Lifecycle { span, .. } => push_non_shape(errs, *span, "on mount"),
            Member::Timer { span, .. } => push_non_shape(errs, *span, "a timer"),
            Member::Measure { span, .. } => push_non_shape(errs, *span, "on measure"),
            Member::Expr(e) => push_non_shape(errs, e.span(), "an expression"),
        }
    }
}

/// Helper for [`validate_canvas`]: a non-element member inside a `Canvas`.
fn push_non_shape(errs: &mut Vec<CompileError>, span: crate::diagnostics::Span, what: &str) {
    errs.push(CompileError::UnknownShapeCommand {
        span,
        name: what.to_string(),
        hint: None,
    });
}

/// Validates one shape command against its parameter contract (RFC-0020):
/// unknown parameters (with a Levenshtein hint), missing required geometry,
/// scalar-literal type mismatches, no attribute block, no children, and the
/// `path`-is-fill-only rule ([`CompileError::PathStrokeUnsupported`]).
#[must_use]
pub fn validate_shape(el: &ElementNode) -> Vec<CompileError> {
    let mut errs = Vec::new();
    let shape = el.name.as_str();
    let (required, optional) = shape_geometry(shape);

    // Shape commands carry everything in their `(...)` argument list: an
    // attribute block or a children block has no meaning on one.
    for attr in &el.attrs {
        errs.push(CompileError::UnknownShapeParam {
            span: attr.span,
            shape: shape.to_string(),
            name: attr.name.as_str().to_string(),
            hint: None,
        });
    }
    if !el.children.is_empty() {
        // RFC-0037: except a `path`, whose body *is* its geometry.
        if shape == "path" {
            errs.extend(validate_path_body(el));
        } else {
            errs.push(CompileError::UnexpectedChildren {
                span: el.span,
                name: shape.to_string(),
            });
        }
    }

    let param_type = |name: &str| -> Option<PropType> {
        required
            .iter()
            .chain(optional)
            .chain(SHAPE_PAINT_PARAMS)
            .find(|(k, _)| *k == name)
            .map(|(_, t)| *t)
    };
    // `text` is fill-rendered glyphs, not a stroked path: it takes only its
    // own geometry/typography params, never the paint set.
    let paint_allowed = shape != "text";

    // `bezier` accepts the terse positional form (8 numbers, RFC-0020 table)
    // as well as the named form; `text` takes its content string positionally.
    let positional_budget = match shape {
        "bezier" => required.len(),
        "text" => 1,
        _ => 0,
    };
    let mut positional_seen = 0usize;

    for arg in &el.content {
        // A positional arg only spends the shape's positional budget
        // (`bezier`'s 8 coordinates, canvas `text`'s content string).
        let Some(name) = &arg.name else {
            positional_seen += 1;
            if positional_seen > positional_budget {
                errs.push(CompileError::UnknownShapeParam {
                    span: arg.value.span(),
                    shape: shape.to_string(),
                    name: "<positional>".to_string(),
                    hint: None,
                });
            }
            continue;
        };
        let pname = name.as_str();
        let known = param_type(pname)
            .is_some_and(|_| paint_allowed || !SHAPE_PAINT_PARAMS.iter().any(|(k, _)| *k == pname));
        if !known {
            let candidates =
                required
                    .iter()
                    .chain(optional)
                    .map(|(k, _)| *k)
                    .chain(if paint_allowed {
                        SHAPE_PAINT_PARAMS
                            .iter()
                            .map(|(k, _)| *k)
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    });
            errs.push(CompileError::UnknownShapeParam {
                span: arg.value.span(),
                shape: shape.to_string(),
                name: pname.to_string(),
                hint: closest_match(pname, candidates).map(str::to_string),
            });
        } else if let Some(ty) = param_type(pname) {
            // RFC-0031 §Q10: `ngon`'s `n` is paint-class, it moves no
            // geometry, and still cannot animate, because there is no shape
            // between a pentagon and a hexagon. A fractional `n` leaves a
            // partial sector whose seam sweeps the shape *while animating*,
            // which is the only time the feature would be used. The diagnostic
            // names `morph`, so it teaches the right construct rather than
            // only refusing; it is deliberately *not* `LayoutPropNotAnimatable`,
            // whose reason (and whose remedy) are different ones.
            if shape == "ngon" && pname == "n" {
                if let Expr::Animated { span, .. } = &arg.value {
                    errs.push(CompileError::NotAnimatable {
                        span: *span,
                        prop: "n".to_string(),
                        use_instead: "morph".to_string(),
                    });
                }
            }
            // Same literal-level check as attribute values, including the
            // RFC-0010 `with` animation chain walk.
            let mut target = &arg.value;
            while let Expr::Animated { value, anim, .. } = target {
                if let Err(err) = crate::interp::anim::resolve_curve(anim) {
                    errs.push(err);
                }
                target = value;
            }
            if let Some(err) = check_value_type(ty, target) {
                errs.push(err);
            }
        }
    }

    // Required geometry. A fully-positional `bezier` (all 8 coordinates in
    // order) satisfies its required set; canvas `text`'s positional content
    // string is checked separately below.
    let named_has = |name: &str| {
        el.content
            .iter()
            .any(|a| a.name.as_ref().is_some_and(|n| n.as_str() == name))
    };
    let bezier_positional = shape == "bezier" && positional_seen == required.len();
    if !bezier_positional {
        for (name, _) in required {
            if !named_has(name) {
                errs.push(CompileError::MissingShapeParam {
                    span: el.span,
                    shape: shape.to_string(),
                    name: (*name).to_string(),
                });
            }
        }
    }
    if shape == "text" && positional_seen == 0 {
        errs.push(CompileError::MissingShapeParam {
            span: el.span,
            shape: shape.to_string(),
            name: "content".to_string(),
        });
    }

    // RFC-0020 §2 Tier 2 is fill-only in v1: stroking a `path` is rejected
    // rather than silently ignored (never-silent, RFC-0002 D4 spirit).
    if shape == "path" {
        let strokes = [
            "stroke",
            "stroke_width",
            "cap",
            "join",
            "dash",
            "dash_offset",
        ];
        if el.content.iter().any(|a| {
            a.name
                .as_ref()
                .is_some_and(|n| strokes.contains(&n.as_str()))
        }) {
            errs.push(CompileError::PathStrokeUnsupported { span: el.span });
        }
    }

    errs
}

/// An axis-aligned rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// Creates a rectangle.
    #[must_use]
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Minimum hit-target size in logical pixels (RFC-0003 E8).
pub const HIT_MIN: f32 = 44.0;

/// Default MSDF distance range in atlas texels for a `VectorIcon` (RFC-0009
/// §2-E), used by the placeholder lowering until the generator bakes a
/// per-glyph value. Ties to the generation grid (a 32² grid with a 4-texel
/// range gives a clean edge under heavy magnification).
pub const VECTOR_DEFAULT_PX_RANGE: f32 = 4.0;

/// Inflates an interactive element's collision rect to at least 44×44, centered
/// on the original rect and clamped to `parent` (RFC-0003 E8).
#[must_use]
pub fn inflate_hit_rect(rect: Rect, parent: Rect) -> Rect {
    let w = rect.w.max(HIT_MIN);
    let h = rect.h.max(HIT_MIN);
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let x = (cx - w / 2.0).clamp(parent.x, (parent.x + parent.w - w).max(parent.x));
    let y = (cy - h / 2.0).clamp(parent.y, (parent.y + parent.h - h).max(parent.y));
    Rect { x, y, w, h }
}

/// Whether a colour value carries an alpha byte: the lexer's >6-digit-hex
/// tag ([`crate::lexer::COLOR_HAS_ALPHA_TAG`], which is what distinguishes
/// `0x00FFFFFF` from `0xFFFFFF`), or, for computed/theme values that never
/// went through a literal, the RFC-0011 magnitude heuristic (above
/// `0xFFFFFF` is alpha-first `0xAARRGGBB`).
#[must_use]
pub fn color_has_alpha(hex: i64) -> bool {
    #[allow(clippy::cast_sign_loss)]
    let magnitude = (hex as u64) & 0xFFFF_FFFF;
    hex & crate::lexer::COLOR_HAS_ALPHA_TAG != 0 || magnitude > 0x00FF_FFFF
}

/// [`color_to_rgba`] with the alpha byte auto-detected via
/// [`color_has_alpha`], the one resolver every alpha-aware colour consumer
/// (ripple ink, `backdrop_tint`, shape colours) funnels through.
#[must_use]
pub fn color_rgba_auto(hex: i64) -> [f32; 4] {
    color_to_rgba(hex, color_has_alpha(hex))
}

/// Parses a `Color` integer into **linear-space** RGBA `[f32; 4]` (6-digit ⇒
/// opaque, 8-digit ⇒ alpha-first `0xAARRGGBB`), RFC-0005 §1.
///
/// # Why the transfer function is here
///
/// A colour is *written* the way a designer reads it, `0x5B8DEF` is the number
/// out of the design tool, which is an sRGB-encoded triple. Everything
/// downstream of this function is linear: `RenderFrame`'s colours are
/// documented as linear, the shaders blend in linear, and the surface is an
/// sRGB format precisely so the GPU encodes once on write.
///
/// Handing the encoded bytes straight through skipped the decode, so every
/// colour in the engine was encoded twice and displayed lighter and flatter
/// than it was written: `0x808080` reached the screen as `0xBC`. Blends were
/// wrong in the same direction, because a mix of two encoded values is not the
/// encoding of their mix, which is why gradients and shadows washed out worst.
///
/// Alpha is **not** transferred: it is a coverage fraction, not a colour, and
/// has never been gamma-encoded.
///
/// The transfer itself lives in [`byard_core::color`], because a package's
/// native view writes colours too and one engine has one colour space
/// (RFC-0039).
#[must_use]
pub fn color_to_rgba(hex: i64, alpha_byte: bool) -> [f32; 4] {
    byard_core::color::to_rgba(hex, alpha_byte)
}

pub use byard_core::color::srgb_to_linear;

#[cfg(test)]
mod tests;
