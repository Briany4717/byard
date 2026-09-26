//! The eval driver: walk the AST, wiring declarations to the reactive core
//! (RFC-0002 §"Dev-mode interpreter"; RFC-0004 §3/§11).
//!
//! Each `byld` expression is **lowered** to a reactive computation, a
//! `FnMut(&mut ReactiveCtx) -> Value` closure that resolves identifiers against
//! the [`Env`] *at lowering time* (capturing their `SignalId`/`ScopeId`
//! handles) and performs its `Signal`/memo reads through the context at run
//! time, so read-tracking stays dynamic (RFC-0004 §3). This is the concrete
//! form of RFC-0004's `walk_expr(scope.expr)`:
//!
//! - `var x = init` ⇒ a reactive source: `init` is evaluated once and a signal
//!   is created from it; `x` binds to `Value::Signal`.
//! - `let y` / `fn f` ⇒ a [`ReactiveCtx::open_memo`]; `y`/`f` binds to
//!   `Value::Memo`. Whether it is actually reactive is observed by the tracker
//!   (D3), not declared.
//! - Reading a `var`/memo identifier routes through `read_signal`/`read_memo`.
//! - `untrack(expr)` is a reserved-name call dispatched to [`untrack`].
//! - A mutation (`=`, `+=`, `++`, `--`) on a `var` marks it; on anything else
//!   it is [`CompileError::NotAssignable`].

mod controller;
mod measure;

use controller::UNBOUND_CONTROLLER;

use super::env::{Env, SignalId, Value};
use super::events::Action;
use super::intrinsics::validate_element;
use super::reactive::{FrameTarget, ReactiveCtx, ScopeId, untrack};
use crate::diagnostics::{CompileError, Span};
use crate::parser::ast::{
    Arg, AssignOp, Attr, AttrKind, BinOp, ElementNode, Expr, Member, Param, PostfixOp, RouteKind,
    StateBlock, StrPart, StyleStateKind, Type, UnOp, ViewDecl,
};
use crate::symbol::Symbol;
use crate::util::closest_match;

/// Decimal places a `Slider` without an explicit `step` quantises its value to.
///
/// A continuous slider otherwise emits the full `f64` precision of a
/// pixel-derived ratio (e.g. `0.6035294…`); rounding keeps the bound value
/// readable. Authors who need a specific granularity set `step:` instead.
const SLIDER_DEFAULT_DECIMALS: i32 = 3;

/// Maximum user-`View` instantiation depth before lowering truncates with a
/// diagnostic rather than risking a native stack overflow (RFC-0007 §4, D-C).
/// Far beyond any hand-written nesting, shallow enough to never
/// approach the stack limit. The static cycle check (`load_views`) catches
/// *unguarded* cycles at load; this bound is the backstop for a guarded
/// recursion whose runtime guard never terminates at lower time.
const MAX_INSTANCE_DEPTH: u32 = 64;

/// Decimal places implied by `step`, via its shortest round-trip form
/// (`0.1 → 1`, `0.25 → 2`, `1.0 → 0`). Used so a stepped slider never emits a
/// value with more decimal places than the step itself, e.g. `step: 0.1`
/// landing on `6 × 0.1 = 0.6000000000000001` is rounded back to `0.6`. Capped
/// at 10 places (any real step is far coarser).
fn step_decimals(step: f64) -> i32 {
    match format!("{}", step.abs()).split_once('.') {
        Some((_, frac)) => i32::try_from(frac.len().min(10)).unwrap_or(0),
        None => 0,
    }
}

/// Settling thresholds for the CPU-sampled animation path (RFC-0010).
///
/// `eval_pure` animates opacity, scale, rotate, and translate through one
/// generic path that doesn't carry the property's unit, so the epsilons must be
/// tight enough to be correct for the *tightest* unit (ratios, ~0..1), which is
/// simply conservative (settles a hair later) for pixels and radians. Position
/// is the final-value accuracy gate; a tight velocity gate keeps a spring's
/// overshoot alive rather than freezing it at the first crossing of the target.
const ANIM_SETTLE_EPS_POS: f32 = 0.002;
const ANIM_SETTLE_EPS_VEL: f32 = 0.02;

/// The private timeline of one repeating, delayed or keyframed animation
/// (RFC-0025), kept in [`Interpreter::anim_clocks`].
///
/// A one-shot `with` animation needs no clock of its own, its `Motion` carries
/// `start_ms` and the value is a function of `now − start_ms`. A repeating one
/// does: it has to know where its *sequence* began, independently of the
/// endpoints, and it has to survive frames on which it is not drawn at all.
#[derive(Clone, Copy)]
struct LoopClock {
    /// Engine time (ms) the current timeline began, delay included.
    start_ms: u32,
    /// Engine time (ms) this animation was last sampled.
    last_seen_ms: u32,
    /// The render this animation was last sampled on, the offscreen probe. A
    /// *render* count, not a clock reading, so "was this drawn last frame?" is
    /// exact however fast or slow the host is pacing frames.
    last_seen_seq: u64,
    /// Fingerprint of the endpoints this timeline was started for
    /// ([`endpoint_key`]), so a retarget can restart it (RFC-0025 §5) without
    /// the endpoints themselves having to be persisted.
    endpoints: u64,
    /// Whether this timeline still waits out its `delay` (RFC-0025 §5): true on
    /// mount, and cleared by a retarget that cancels a `delay:`, never by one
    /// that restarts a stagger.
    honor_delay: bool,
}

/// Where a keyframe sequence sits right now: the two step *values* surrounding
/// the current time and the eased factor between them (RFC-0025 §3).
///
/// Values stay as expressions until the caller needs them, which is what lets
/// one blend serve both the numeric and the colour path, a colour blends in
/// OKLab, a scalar numerically, and neither wants the other's interpretation.
struct KeyframeBlend<'a> {
    /// The step being interpolated from.
    lo: &'a Expr,
    /// The step being interpolated to (`lo` when parked on a step).
    hi: &'a Expr,
    /// Eased `0..=1` factor from `lo` to `hi`.
    t: f32,
}

/// The source range a list of members covers, as one span, the union of their
/// own spans. Used to name a `when` branch's extent so its animation state can
/// be dropped with it.
fn members_span(members: &[Member]) -> Span {
    let mut span = Span::new(u32::MAX, 0);
    for member in members {
        let member_span = match member {
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
            Member::Element(el) => el.span,
            Member::Expr(expr) => expr.span(),
        };
        span.start = span.start.min(member_span.start);
        span.end = span.end.max(member_span.end);
    }
    if span.end <= span.start {
        Span::new(0, 0)
    } else {
        span
    }
}

/// One item in a `Canvas` body (RFC-0020 §1): a shape command, or the control
/// flow that generates them.
///
/// # Why control flow belongs here at all
///
/// A `Canvas` body was shape commands and nothing else, which made the one
/// thing a drawing surface is *for*, a chart, a sparkline, anything whose
/// shape count comes from data, inexpressible. You could draw twenty-four
/// bars by writing twenty-four `rect(…)` lines against twenty-four separately
/// named fields, and that is not a language feature, it is a workaround for
/// the absence of one.
///
/// # Why it expands at emit time and not at lowering
///
/// Everything else in the language lowers `for` and `when` into reactive pools
/// (`ForPool` / `WhenPool`), because their bodies are *elements* with layout,
/// identity and mountable state, and re-deriving them per frame would throw all
/// three away.
///
/// A canvas body has none of that. Shape commands carry no layout, no identity
/// and no state; the render walk already re-evaluates every one of their
/// parameter expressions every tick, which is exactly what makes them reactive
/// without any pooling. Expanding the loop in the same walk is therefore the
/// *consistent* choice rather than the cheap one: it makes the shape count as
/// reactive as the coordinates already were, and it adds no node, no pool and
/// no per-frame allocation beyond the bindings it pushes and pops.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasItem {
    /// A validated shape command (`rect`, `arc`, `circle`, `line`, `text`,
    /// `path`).
    Shape(ElementNode),
    /// `for item in items { … }`, expanded against the environment each tick.
    For {
        /// Loop variable.
        var: Symbol,
        /// The optional index variable of the `for i, item in items` form.
        index: Option<Symbol>,
        /// Iterable expression, evaluated per tick.
        iter: Expr,
        /// Body items.
        body: Vec<CanvasItem>,
    },
    /// `when cond { … } else { … }`.
    When {
        /// Condition, evaluated per tick.
        cond: Expr,
        /// Then-branch items.
        then: Vec<CanvasItem>,
        /// Else-branch items.
        els: Vec<CanvasItem>,
    },
}

/// Where a canvas body's Tier-1 shapes go (RFC-0031 §S4).
///
/// A plain `Canvas` pushes each shape to the frame as its own instance, exactly
/// as it always has. A `Canvas` that declares a combine mode collects them into
/// one group instead, same walk, same per-tick re-evaluation of every
/// parameter expression, different destination, and the head is pushed once at
/// the end.
///
/// Routing this at the emission site rather than post-processing the frame's
/// shape pool is what keeps `for`/`when` working inside a group for free: the
/// members are whatever the walk produced, however it produced them.
#[derive(Debug, Default)]
pub(crate) struct ShapeGroupSink {
    /// The members collected so far, in declaration order.
    members: Vec<byard_core::frame::ShapeRecord>,
    /// The union of their bounds, the head's quad (§S4).
    bounds: Option<byard_core::frame::Rect>,
    /// The paint parameters the head carries on the members' behalf: a group
    /// has one stroke, one cap, one dash pattern and one opacity, taken from
    /// the first member that contributed (§S8 makes the head's stroke govern;
    /// there is no group-level stroke syntax, so the first shape's is it).
    paint: Option<byard_core::frame::CanvasShape>,
    /// Whether more shapes were offered than [`MAX_GROUP_MEMBERS`] allows.
    ///
    /// [`MAX_GROUP_MEMBERS`]: byard_core::frame::MAX_GROUP_MEMBERS
    overflowed: usize,
}

impl ShapeGroupSink {
    /// Adds one shape as a member, unioning its bounds into the head's quad.
    fn push(&mut self, shape: &byard_core::frame::CanvasShape) {
        let b = shape.bounds();
        self.bounds = Some(match self.bounds {
            Some(u) => union_rect(u, b),
            None => b,
        });
        if self.paint.is_none() {
            self.paint = Some(shape.clone());
        }
        if self.members.len() < byard_core::frame::MAX_GROUP_MEMBERS {
            self.members
                .push(byard_core::frame::ShapeRecord::from_shape(shape));
        }
        self.overflowed += 1;
    }
}

/// A representative source position for a set of attributes, the first one's,
/// or the file start when there are none. Used where a diagnostic is raised
/// during the render walk, which has the element's attrs but not its node.
fn attr_span(attrs: &[Attr]) -> Span {
    attrs.first().map_or(Span::new(0, 0), |a| a.span)
}

/// The smallest rect containing both.
fn union_rect(
    first: byard_core::frame::Rect,
    second: byard_core::frame::Rect,
) -> byard_core::frame::Rect {
    let left = first.x.min(second.x);
    let top = first.y.min(second.y);
    let right = (first.x + first.width).max(second.x + second.width);
    let bottom = (first.y + first.height).max(second.y + second.height);
    byard_core::frame::Rect::new(left, top, right - left, bottom - top)
}

/// Collects a `Canvas` body into [`CanvasItem`]s, keeping declaration order.
///
/// Anything that is neither a shape command nor `for`/`when` has already been
/// reported by `validate_canvas`, so it is dropped here rather than diagnosed
/// twice.
fn lower_canvas_items(members: &[Member]) -> Vec<CanvasItem> {
    members
        .iter()
        .filter_map(|m| match m {
            Member::Element(c) if super::intrinsics::is_shape_command(c.name.as_str()) => {
                Some(CanvasItem::Shape(c.clone()))
            }
            Member::For {
                var,
                index,
                iter,
                body,
                ..
            } => Some(CanvasItem::For {
                var: var.clone(),
                index: index.clone(),
                iter: iter.clone(),
                body: lower_canvas_items(body),
            }),
            Member::When {
                cond, then, els, ..
            } => Some(CanvasItem::When {
                cond: cond.clone(),
                then: lower_canvas_items(then),
                els: els.as_deref().map(lower_canvas_items).unwrap_or_default(),
            }),
            _ => None,
        })
        .collect()
}

/// What a persisted animation belongs to: the `with` node that wrote it, **and
/// the element instance it is running on**.
///
/// The span alone is not an identity. A `for` body is lowered once per pooled
/// slot but written once, so ten rows animating a property share one source
/// span, and, before this key existed, one `Motion` and one `LoopClock`. Two
/// rows heading for *different* targets therefore fought over the same state:
/// each retarget reseeded the other's `from` and restarted its clock, and both
/// stalled near `t ≈ 0`. Anyone with a list of animated components was affected.
///
/// `slot` is the instance. It is the per-slot element signal a `for` pool
/// creates for each of its indices, which is unique across the whole program by
/// construction: a nested `for` is lowered once per *outer* slot and gets its
/// own pool with its own fresh signals, so the innermost binding already
/// distinguishes every instance without a path to fold. `0` is "no enclosing
/// loop", the top level, where the span was always enough.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct AnimKey {
    /// Source range of the `with`/keyframes node.
    span: Span,
    /// The element instance, or `0` outside any `for` body.
    slot: u32,
}

impl AnimKey {
    /// The key for `span` on the instance currently being rendered.
    const fn new(span: Span, slot: u32) -> Self {
        Self { span, slot }
    }
}

/// Every `for` pool reachable from `nodes` without leaving them, in any order.
fn collect_pools(nodes: &[RenderNode], out: &mut Vec<usize>) {
    for n in nodes {
        match n {
            RenderNode::For { pool, .. } => out.push(*pool),
            RenderNode::Box { children, .. } | RenderNode::Overlay { children, .. } => {
                collect_pools(children, out);
            }
            _ => {}
        }
    }
}

/// One concrete node produced by expanding `when`/`for`, and the animation slot
/// it belongs to (RFC-0025).
///
/// The slot travels *with the node* rather than being recomputed by each
/// walker, because the expansion is the only place that knows it: after
/// flattening, a row of a `for` is indistinguishable from a node written
/// literally at that position.
#[derive(Clone, Copy)]
pub(crate) struct Concrete<'a> {
    node: &'a RenderNode,
    slot: u32,
}

/// A fingerprint of a repeating animation's endpoints, used to notice a
/// retarget (RFC-0025 §5).
///
/// Repeating animations recompute their endpoints every frame, so nothing needs
/// to be stored to *sample* them, only enough to answer "are these the same
/// endpoints as last frame?". A hash of the raw bits answers exactly that, for
/// one scalar or four colour channels alike.
fn endpoint_key(motions: &[byard_core::frame::Motion]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for motion in motions {
        motion.from.to_bits().hash(&mut hasher);
        motion.to.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// Interpolates between two evaluated keyframe values (RFC-0025 §3).
///
/// Scalars blend numerically; a tuple blends component-wise (keeping the left
/// operand's field names), which is what makes `translate: anim.keyframes(0%:
/// (-100, 0), …)` mean what it reads like. Anything else, mismatched shapes
/// included, snaps at the segment's midpoint rather than inventing a value.
fn lerp_value(a: &Value, b: &Value, t: f32) -> Value {
    let scalar = |v: &Value| match v {
        Value::Float(f) => Some(*f),
        #[allow(clippy::cast_precision_loss)]
        Value::Int(n) => Some(*n as f64),
        _ => None,
    };
    if let (Some(x), Some(y)) = (scalar(a), scalar(b)) {
        return Value::Float(x + (y - x) * f64::from(t));
    }
    if let (Value::Tuple(xs), Value::Tuple(ys)) = (a, b) {
        if xs.len() == ys.len() {
            return Value::Tuple(
                xs.iter()
                    .zip(ys)
                    .map(|((name, x), (_, y))| (name.clone(), lerp_value(x, y, t)))
                    .collect(),
            );
        }
    }
    if t < 0.5 { a.clone() } else { b.clone() }
}

/// RFC-0021 pull-to-refresh geometry, in logical px. The pull region resists with
/// a diminishing-returns curve that asymptotes to [`PULL_MAX`]; releasing past
/// [`PULL_THRESHOLD`] triggers a refresh and rests the indicator at [`PULL_REST`].
const PULL_MAX: f32 = 120.0;
const PULL_THRESHOLD: f32 = 56.0;
const PULL_REST: f32 = 44.0;

/// Rounds `val` to `decimals` decimal places (half-away-from-zero).
fn round_to_decimals(val: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    (val * factor).round() / factor
}

/// A resolved first-class style value (RFC-0016): a flat base attribute set
/// plus its `on <state> { … }` interaction-state blocks. Produced by
/// [`Interpreter::resolve_style_expr`] from a `style { … }` value, a `let`-bound
/// style name, or a `merge` of two styles. Static and view-scoped, no cascade.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleDef {
    /// The base attributes, last-write-wins in written order.
    pub base: Vec<Attr>,
    /// The state blocks, in written order (a later block of the same state
    /// wins, which is how `merge` layers the right operand over the left).
    pub states: Vec<StateBlock>,
}

/// A lowered render-tree node: the interpreter's plan for one element. Reactive
/// fields are reactive-scope ids the engine reads each tick (M14).
#[derive(Debug, Clone, PartialEq)]
pub enum RenderNode {
    /// A box-like container.
    Box {
        /// The element intrinsic name.
        name: Symbol,
        /// Styling attributes.
        attrs: Vec<Attr>,
        /// `on <state> { … }` blocks (RFC-0016), overlaid onto `attrs` at render
        /// time when their engine state is active. Empty for the common case.
        state_blocks: Vec<StateBlock>,
        /// Child render nodes.
        children: Vec<RenderNode>,
        /// Event shorthand action.
        action: Option<Expr>,
        /// The `var` signal bound via `bind:` or `value:` (M16: value widgets).
        bound_sig: Option<super::env::SignalId>,
        /// The instance environment captured at lower time (RFC-0019 §2), or
        /// empty at the top level. Event attrs and the `action` are re-lowered
        /// each frame during the render walk; for a box lowered inside a
        /// user-view instance this snapshot restores the callee's `Fn` params
        /// and argument bindings, so a forwarded callback (`tap => on_tap()`)
        /// resolves against the scope it was instantiated in.
        env_snapshot: Vec<(Symbol, super::env::Value)>,
        /// `as <name>`, this element's anchor tag (RFC-0036), carried through
        /// lowering so the overlay pass can find its laid-out rect.
        anchor_name: Option<Symbol>,
        /// The RFC-0038 `on measure` slot this element declared, if any: an
        /// index into the interpreter's measure slots.
        ///
        /// On the node rather than in a side table keyed by position, because
        /// the element and the rect it wants are only in the same hand during
        /// the layout build; `None` is the whole cost of not using the feature.
        measure: Option<u32>,
    },
    /// A text run.
    Text {
        /// Styling attributes.
        attrs: Vec<Attr>,
        /// `on <state> { … }` blocks (RFC-0016) overlaid at render time.
        state_blocks: Vec<StateBlock>,
        /// The reactive scope projecting the text content.
        content: ScopeId,
    },
    /// A flexible gap (layout-only, RFC-0005): absorbs its parent's free space
    /// along the main axis. `attrs` carries `grow`/`basis`, which is why this is
    /// not a unit variant, a `Spacer` that ignored them could not flex.
    Spacer {
        /// `grow` (default 1) and `basis` (default 0).
        attrs: Vec<Attr>,
    },
    /// A texture-sampled image (M21).
    Image {
        /// Styling attributes (width, height, fit, radii, opacity, …).
        attrs: Vec<Attr>,
        /// `on <state> { … }` blocks (RFC-0016) overlaid at render time.
        state_blocks: Vec<StateBlock>,
        /// The reactive scope that evaluates to the image source path/URL.
        src: ScopeId,
    },
    /// A package-authored native view (RFC-0039): an element whose drawing,
    /// measurement and event handling are compiled-in Rust rather than an
    /// intrinsic's.
    ///
    /// It is a leaf here for the same reason an `Image` is: what it contains
    /// is its own business, and the tree only needs to know how much room it
    /// takes and when to hand it a frame.
    Native {
        /// The registered name, which is also what `byld` calls it.
        name: Symbol,
        /// Styling attributes, including the view's own declared props.
        attrs: Vec<Attr>,
        /// `on <state> { … }` blocks (RFC-0016) overlaid at render time.
        state_blocks: Vec<StateBlock>,
        /// Event shorthand action, as on any element.
        action: Option<Expr>,
        /// The instance environment captured at lower time (RFC-0019 §2), so a
        /// prop expression resolves against the scope the element was written
        /// in even though it is re-evaluated every tick.
        env_snapshot: Vec<(Symbol, super::env::Value)>,
        /// This element's view instance, as an index into the interpreter's
        /// live views.
        ///
        /// The instance is made when this node is lowered and dropped when the
        /// tree is lowered again, which is exactly the element's lifetime: a
        /// view's state lives and dies with the element that declared it, with
        /// no separate lifetime to manage (INV-31).
        slot: usize,
    },
    /// An MSDF vector glyph, the `VectorIcon` intrinsic (RFC-0009 §1)
    /// routed to the `VectorMSDF` pipeline.
    Vector {
        /// Styling attributes (`size`, `color`, `m`, `opacity`, `style`).
        attrs: Vec<Attr>,
        /// The reactive scope evaluating to the asset handle (a `Str` path).
        src: ScopeId,
    },
    /// A `Canvas`, the RFC-0020 programmatic drawing surface. A fixed-size
    /// leaf whose children are *shape commands*, not views: each render tick
    /// re-evaluates every shape's parameter expressions (so a reactive
    /// `sweep: percent * 3.6` animates for free) and lowers Tier-1 shapes to
    /// the `CanvasShape` pipeline, `path` commands to `VectorMSDF` (Tier 2),
    /// and `text` commands to `TextLine`s.
    Canvas {
        /// Styling attributes (`width`/`height`, `bg`, `opacity`, `style`).
        attrs: Vec<Attr>,
        /// `on <state> { … }` blocks (RFC-0016) overlaid at render time.
        state_blocks: Vec<StateBlock>,
        /// The validated canvas body, in declaration order. Kept as AST,
        /// named args are ordinary `Expr`s evaluated per tick through
        /// `eval_pure`, which is what makes every parameter reactive and
        /// `with`-animatable (RFC-0010) without extra plumbing.
        shapes: Vec<CanvasItem>,
        /// The `=> action` tap shorthand.
        action: Option<Expr>,
        /// The instance environment captured at lower time (RFC-0019 §2), so
        /// shape parameters and event actions referencing instance vars
        /// resolve against the scope the canvas was instantiated in.
        env_snapshot: Vec<(Symbol, super::env::Value)>,
    },
    /// An `Overlay`, the RFC-0017 escape-hatch. Its children leave the normal
    /// layout flow and render in the overlay layer, above all main content and
    /// laid out against the viewport. In the parent tree the node occupies zero
    /// space (a 0×0 layout leaf); the render walk collects it into the overlay
    /// stack and emits it in a deferred second phase.
    Overlay {
        /// Styling/behaviour attributes: `modal`, `dismiss_on_outside`, and the
        /// `dismiss =>` event action.
        attrs: Vec<Attr>,
        /// The overlay's floating content subtree.
        children: Vec<RenderNode>,
        /// The instance environment captured at lower time (RFC-0019 §2), so a
        /// `dismiss` action or a child's forwarded callback resolves against the
        /// scope the overlay was instantiated in. Empty at the top level.
        env_snapshot: Vec<(Symbol, super::env::Value)>,
    },
    /// A reactive `when cond { … } else { … }` (RFC-0018 structural reactivity).
    /// The driver re-reads `cond` every frame and expands the taken branch, so a
    /// `var` flip mounts/unmounts the subtree with no re-lowering. Each branch is
    /// lowered **lazily** on first selection and cached (see [`WhenPool`]), so a
    /// guarded recursion (`when done { … } else { Recurse() }`) only lowers the
    /// recursive branch when the guard actually reaches it, terminating finitely.
    When {
        /// The reactive predicate, re-read each frame.
        cond: ScopeId,
        /// Index into the interpreter's `when_pools`.
        pool: usize,
    },
    /// A reactive `for item in list { … }` (RFC-0018 structural reactivity).
    /// Coarse, positional reconciliation (RFC-0002 D7): the driver reads `list`
    /// each frame and renders one pooled body per element. Bodies are lowered
    /// lazily into a reusable pool (grown to the high-water length, never
    /// re-lowered per frame), each reading its element from a per-slot signal the
    /// driver updates, so list growth/shrink/value changes are reactive without
    /// re-lowering or churning scopes.
    For {
        /// Index into the interpreter's `for_pools`.
        pool: usize,
        /// The reactive list projection, re-read each frame.
        list: ScopeId,
    },
    /// A structural effect (RFC-0028 §4b): `on mount => …` / `on unmount =>
    /// …`.
    ///
    /// A node with no pixels. It is a node anyway, and not a side table, so
    /// that its **position in the tree** is what decides when it fires: the
    /// structural walk reaches exactly the mounted subtree, so being visited
    /// is being mounted, and every container that can mount or unmount a
    /// subtree, `when`, `for`, a route, a user-view instance, gets a correct
    /// edge without knowing effects exist.
    Effect {
        /// Index into the interpreter's effect slots.
        index: usize,
    },
    /// A `NavStack`/`NavHost` (RFC-0026): a stack container whose live children
    /// are the screens its navigation state selects. Unlike `When`/`For` this is
    /// a *concrete* node, it lays out as one container so the two screens alive
    /// during a transition overlap instead of stacking, and its children come
    /// from the pool, which the reconcile pass keeps in step with the driving
    /// `path`/`active` projection.
    Nav {
        /// Index into the interpreter's `nav_pools`.
        pool: usize,
        /// The reactive `path:`/`active:` projection, re-read each frame.
        path: ScopeId,
    },
}

/// A borrowed view of the structural-reactivity caches (RFC-0018) and the
/// navigation stacks (RFC-0026), passed through the read-only build/paint phase
/// so `when`/`for`/`NavStack` expand consistently.
#[derive(Clone, Copy)]
struct Pools<'a> {
    fors: &'a [ForPool],
    whens: &'a [WhenPool],
    navs: &'a [NavPool],
}

/// Which navigation container a [`NavPool`] backs (RFC-0026).
#[derive(Clone, Copy, PartialEq, Eq)]
enum NavKind {
    /// `NavStack`, a push/pop history stack driven by a `path` var.
    Stack,
    /// `NavHost`, a flat set of preserved tabs driven by an `active` var.
    Host,
}

/// One compiled case of a navigation container: its pattern, its optional
/// params binding, and the body AST that is lowered *once per live entry* and
/// then preserved (RFC-0026 §4).
struct RouteDef {
    /// The compiled pattern (a literal tab name for a `NavHost`).
    pattern: crate::interp::nav::RoutePattern,
    /// The `{|params| … }` binding name, if written.
    params_binding: Option<Symbol>,
    /// The case body, re-lowered per entry (each entry gets its own state).
    body: Vec<Member>,
}

/// One live screen of a navigation container: a route instance whose lowered
/// subtree, and therefore every `var`, scroll offset and animation inside it,
/// stays alive while it is on the stack (RFC-0026 §4 state preservation).
struct NavEntry {
    /// The concrete path this entry was mounted for (`/detail/42`), or the tab
    /// name for a `NavHost`.
    path: String,
    /// The lowered subtree. Never re-lowered while the entry lives.
    nodes: Vec<RenderNode>,
    /// The source range the entry's body spans, so a discarded entry takes its
    /// animation state with it (RFC-0025: an animation lives and dies with its
    /// element).
    body_span: Span,
}

/// An in-flight route transition (RFC-0026 §5): two entries are alive and
/// composited at once, placed from a single progress scalar.
#[derive(Clone, Copy)]
struct NavAnim {
    /// The entry being covered/left.
    outgoing: usize,
    /// The entry being revealed.
    incoming: usize,
    /// Whether this is a pop (reverses the direction).
    pop: bool,
    /// The `0 → 1` progress spring. Sampled once per frame; both screens'
    /// geometry is a closed form of it.
    motion: byard_core::frame::Motion,
    /// Whether progress is currently driven by an edge-swipe gesture rather
    /// than the clock (RFC-0026 §"Swipe-back gesture"). A gesture-driven
    /// transition never settles on its own, the release decides.
    gesture: bool,
    /// Gesture progress, while `gesture` is set.
    gesture_p: f32,
}

/// One screen a navigation container paints this frame, with the offset and
/// opacity its transition places it at.
#[derive(Clone, Copy)]
struct LiveScreen {
    /// Index into [`NavPool::entries`].
    entry: usize,
    /// Whether this is the screen being revealed (as opposed to the one being
    /// covered/left), the half of the transition geometry that is per-screen.
    incoming: bool,
    /// Whether this screen is the one in the container's normal flow. Exactly
    /// one is: the screen the navigation state currently names, which is what
    /// gives the container its size. Its transitioning partner is laid out
    /// absolutely over the same rect, so it overlaps without displacing
    /// anything and without perturbing the container's measured size.
    in_flow: bool,
}

/// The slice of a navigation container's state that lowered *action closures*
/// need at fire time (RFC-0026 §"navigation actions").
///
/// An action lowers to a `FnMut(&mut ReactiveCtx)`, it never sees the
/// interpreter, but `back(navPath)` has to know what is underneath the top of
/// the stack, and `replace(…)` has to tell the next reconcile not to stack. Both
/// are answered by this one shared cell, updated whenever the stack moves. `Rc`/
/// `RefCell` are sound here for the same reason the radio groups' are: the
/// interpreter and its event closures are single-threaded logic-thread state
/// (`!Send`, INV-2).
type NavSharedCell = std::rc::Rc<std::cell::RefCell<NavShared>>;

#[derive(Default)]
struct NavShared {
    /// The live entries' paths, mirroring [`NavPool::entries`].
    paths: Vec<String>,
    /// The visible entry's index.
    current: usize,
    /// Set by `replace(…)`: the next navigation takes the current entry's slot
    /// instead of stacking on top of it.
    replace_next: bool,
}

/// A navigation container's state (RFC-0026), indexed by
/// [`RenderNode::Nav::pool`].
///
/// The whole model is: *navigation state is a reactive `var`*. This pool holds
/// no navigation intent of its own, it observes the driving `path`/`active`
/// projection each frame and reconciles its history stack to match, which is
/// what makes `navPath = "/detail/42"` a push and `navPath = "/"` a pop with no
/// controller object anywhere (RFC-0003: no widget references).
struct NavPool {
    /// Stack or tabs.
    kind: NavKind,
    /// The container element's own attributes (background, padding, `grow`, …).
    attrs: Vec<Attr>,
    /// `on <state> { … }` blocks on the container.
    state_blocks: Vec<StateBlock>,
    /// The transition family (`slide` for a stack, `fade` for tabs by default).
    transition: crate::interp::nav::NavTransition,
    /// RFC-0026 `swipe_back`: the Cupertino interactive edge-pop gesture.
    swipe_back: bool,
    /// RFC-0026 `deep_link`: this stack accepts OS URL intents.
    deep_link: bool,
    /// RFC-0026 `max_depth` (default 10, `0` disables): the runaway-push guard.
    max_depth: usize,
    /// The signal behind `path:`/`active:` when it is a writable `var`, the
    /// engine writes it back for `back(…)`, a completed swipe-back, a rejected
    /// over-deep push, and a delivered deep link. Reflected state, never a
    /// second source of truth.
    path_sig: Option<SignalId>,
    /// The compiled route table, in declaration order (first match wins).
    routes: Vec<RouteDef>,
    /// User-view names in scope at lower time.
    known_views: Vec<String>,
    /// The instance env captured at lower time (RFC-0019), restored when a new
    /// entry's body is lowered.
    env_snapshot: Vec<(Symbol, Value)>,
    /// The live history stack (a `NavHost`'s instantiated, preserved tabs).
    entries: Vec<NavEntry>,
    /// Index into `entries` of the screen the navigation state names.
    current: usize,
    /// The in-flight transition, if any.
    anim: Option<NavAnim>,
    /// The screens to lay out and paint this frame, one at rest, two during a
    /// transition, in painter's order (the moving edge last).
    live: Vec<LiveScreen>,
    /// This frame's transition progress (`1.0` at rest).
    progress: f32,
    /// Whether this frame's transition is a pop (reverses the direction).
    popping: bool,
    /// The last navigation value observed, so a change is edge-triggered.
    last_path: Option<String>,
    /// The stack state the lowered `back`/`replace` closures read and write.
    shared: NavSharedCell,
    /// A settled navigation whose `route_change` has not fired yet.
    pending_change: Option<String>,
    /// Paths already reported as unmatched, so a steady-state mismatch
    /// diagnoses once instead of every frame.
    warned_paths: Vec<String>,
    /// The container's source span, for diagnostics.
    span: Span,
}

/// A `when`'s lazily-lowered branch cache (RFC-0018). Each branch's AST is kept
/// and lowered only the first time the condition selects it, then reused, so an
/// untaken (possibly recursive) branch costs nothing until it is actually shown.
struct WhenPool {
    /// The `then` branch AST.
    then_ast: Vec<Member>,
    /// The `else` branch AST (empty when there is no `else`).
    els_ast: Vec<Member>,
    /// Source range each branch spans, so a branch that goes away can take its
    /// animation state with it (RFC-0025: an animation lives and dies with its
    /// element). `(then, else)`.
    branch_spans: (Span, Span),
    /// Which branch was selected on the previous reconcile, or `None` before the
    /// first one, the edge detector for that unmount.
    last_take: Option<bool>,
    /// User-view names in scope at lower time.
    known_views: Vec<String>,
    /// Instance env captured at lower time (RFC-0019), restored when lowering.
    env_snapshot: Vec<(Symbol, super::env::Value)>,
    /// The lowered `then` branch, once first taken.
    then: Option<Vec<RenderNode>>,
    /// The lowered `else` branch, once first taken.
    els: Option<Vec<RenderNode>>,
}

/// A `for`'s reusable body pool (RFC-0018). Bodies are lowered once per slot and
/// reused across frames; each slot's element value lives in `item_slots[i]`,
/// which the driver rewrites from the current list before painting.
struct ForPool {
    /// The loop variable name, bound to each slot's signal when lowering a body.
    item_var: Symbol,
    /// The optional index variable (`for i, item in …`, RFC-0025). Bound to the
    /// slot's own index as a constant while its body is lowered, a pooled slot
    /// *is* its position, so the index never needs to be reactive.
    index_var: Option<Symbol>,
    /// The loop body AST, re-lowered only when the pool grows to a new index.
    body: Vec<Member>,
    /// User-view names in scope at lower time (for lowering new bodies).
    known_views: Vec<String>,
    /// The instance env captured at lower time (RFC-0019), restored when lowering
    /// a new body so it resolves against the scope the `for` was written in.
    env_snapshot: Vec<(Symbol, super::env::Value)>,
    /// One signal per pooled index, holding that slot's current element value.
    item_slots: Vec<super::env::SignalId>,
    /// One lowered body per pooled index (parallel to `item_slots`).
    bodies: Vec<Vec<RenderNode>>,
    /// How many bodies are live (painted) this frame, the current list length.
    len: usize,
}

/// A lowered reactive computation (see the module docs).
type Lowered = Box<dyn FnMut(&mut ReactiveCtx) -> Value>;

/// One scrollable axis of a [`ScrollTarget`]: the `var` behind `offset.x` or
/// `offset.y` and how far it may travel (content extent − viewport, ≥ 0).
#[derive(Clone, Copy)]
struct ScrollAxis {
    /// The signal backing this axis's offset component; the wheel/drag writes it.
    sig: SignalId,
    /// Maximum scroll distance on this axis (content − viewport), clamped ≥ 0.
    max: f32,
}

/// RFC-0021 `snap` mode: where a scroll settles after a fling/scroll goes quiet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SnapMode {
    /// Free scrolling (`snap: none`, the default), never snaps.
    None,
    /// `snap: page`, settle to the nearest viewport-sized page.
    Page,
    /// `snap: item`, settle to the nearest direct-child boundary (the child
    /// offsets are precomputed into [`Interpreter::scroll_item_bounds`]).
    Item,
}

/// RFC-0021 `snap_align`: where the snapped item aligns within the viewport.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SnapAlign {
    /// Item's leading edge at the viewport's leading edge (default).
    Start,
    /// Item centred in the viewport.
    Center,
    /// Item's trailing edge at the viewport's trailing edge.
    End,
}

/// Stands in for a view whose registration went missing between checking and
/// lowering (RFC-0039).
///
/// It draws nothing, and it exists so the slot table stays index-aligned with
/// the lowered tree: the diagnostic beside it is what tells the author, and a
/// missing entry here would silently shift every later view's slot onto the
/// wrong instance.
struct MissingNativeView;

impl byard_core::render::NativeProps for MissingNativeView {}

impl byard_core::render::NativeView for MissingNativeView {
    fn render(
        &mut self,
        _layout: byard_core::render::Layout,
        _cx: &mut byard_core::render::RenderCtx<'_>,
    ) {
    }
}

/// One tessellated path, and when it was last drawn (RFC-0037).
struct CachedMesh {
    mesh: std::sync::Arc<byard_core::frame::FillMesh>,
    /// The frame this mesh was last used on, so a path that has gone away
    /// stops being paid for.
    last_used: u64,
}

/// How many frames an unused mesh is kept before it is dropped.
///
/// Long enough that a path alternating with a sibling (a `when` toggling two
/// charts) does not re-tessellate on every switch, short enough that a screen
/// nobody is on stops holding geometry. Frames rather than seconds because the
/// cost being bounded is memory per drawn frame, not wall-clock.
const MESH_CACHE_FRAMES: u64 = 120;

/// One evaluated path command, in absolute logical pixels (RFC-0037).
#[derive(Clone, Copy, Debug, PartialEq)]
enum PathCommand {
    /// Start a new subpath at this point.
    Move([f32; 2]),
    /// A straight segment to this point.
    Line([f32; 2]),
    /// A quadratic Bézier: control point, end point.
    Quad([f32; 2], [f32; 2]),
    /// A cubic Bézier: two control points, end point.
    Cubic([f32; 2], [f32; 2], [f32; 2]),
    /// Close the current subpath back to its start.
    Close,
}

/// The fingerprint a mesh is cached under (RFC-0037, RFC-0032's rule).
///
/// `to_bits` rather than the float itself, because hashing an `f32` directly
/// makes `NaN` permanently dirty and `-0.0`/`0.0` permanently clean: two
/// coordinates that are the same number would fingerprint differently, and a
/// coordinate that is no number at all would fingerprint differently every
/// frame. The tolerance and the fill rule are in the key too, since both
/// change the triangles the same commands produce.
fn path_fingerprint(commands: &[PathCommand], tolerance: f32, even_odd: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tolerance.to_bits().hash(&mut hasher);
    even_odd.hash(&mut hasher);
    for command in commands {
        match command {
            PathCommand::Move(p) => (0u8, p[0].to_bits(), p[1].to_bits()).hash(&mut hasher),
            PathCommand::Line(p) => (1u8, p[0].to_bits(), p[1].to_bits()).hash(&mut hasher),
            PathCommand::Quad(c, p) => {
                (
                    2u8,
                    c[0].to_bits(),
                    c[1].to_bits(),
                    p[0].to_bits(),
                    p[1].to_bits(),
                )
                    .hash(&mut hasher);
            }
            PathCommand::Cubic(a, b, p) => {
                (
                    3u8,
                    a[0].to_bits(),
                    a[1].to_bits(),
                    b[0].to_bits(),
                    b[1].to_bits(),
                    p[0].to_bits(),
                    p[1].to_bits(),
                )
                    .hash(&mut hasher);
            }
            PathCommand::Close => 4u8.hash(&mut hasher),
        }
    }
    hasher.finish()
}

/// Turns a tessellated position into the vertex the pipeline reads: the
/// position it already had, plus where it sits inside the path's box.
///
/// The `uv` is what a gradient is measured in, so it is computed here, once
/// per vertex, rather than in the shader from bounds it would have to be told
/// separately (RFC-0037).
struct WithUv {
    bounds: [f32; 4],
}

impl lyon_tessellation::FillVertexConstructor<byard_core::encoder::canvas_fill::FillVertex>
    for WithUv
{
    fn new_vertex(
        &mut self,
        vertex: lyon_tessellation::FillVertex<'_>,
    ) -> byard_core::encoder::canvas_fill::FillVertex {
        let p = vertex.position();
        byard_core::encoder::canvas_fill::FillVertex {
            pos: [p.x, p.y],
            uv: [
                (p.x - self.bounds[0]) / self.bounds[2],
                (p.y - self.bounds[1]) / self.bounds[3],
            ],
        }
    }
}

/// Tessellates a path into the triangles the `CanvasFill` pipeline draws
/// (RFC-0037).
///
/// An unclosed fill is closed back to its subpath's start rather than
/// refused: an open filled path is almost always an oversight, and closing it
/// produces the obviously-intended shape (the RFC's resolved question). `lyon`
/// closes it for us, which is why there is no special case here to get wrong.
fn tessellate_path(
    commands: &[PathCommand],
    tolerance: f32,
    even_odd: bool,
) -> byard_core::frame::FillMesh {
    use lyon_tessellation::geom::point;
    use lyon_tessellation::path::Path;
    use lyon_tessellation::{
        BuffersBuilder, FillOptions, FillRule, FillTessellator, VertexBuffers,
    };

    let mut builder = Path::builder();
    let mut open = false;
    for command in commands {
        match command {
            PathCommand::Move(p) => {
                if open {
                    builder.end(true);
                }
                builder.begin(point(p[0], p[1]));
                open = true;
            }
            PathCommand::Line(p) if open => {
                builder.line_to(point(p[0], p[1]));
            }
            PathCommand::Quad(c, p) if open => {
                builder.quadratic_bezier_to(point(c[0], c[1]), point(p[0], p[1]));
            }
            PathCommand::Cubic(a, b, p) if open => {
                builder.cubic_bezier_to(point(a[0], a[1]), point(b[0], b[1]), point(p[0], p[1]));
            }
            PathCommand::Close if open => {
                builder.end(true);
                open = false;
            }
            // A segment before any `move` is refused at check time
            // (`PathMustStartWithMove`); reaching here means the body was
            // built some other way, and dropping the segment is the only
            // answer that cannot invent geometry.
            _ => {}
        }
    }
    if open {
        builder.end(true);
    }
    let path = builder.build();

    // The bounds `uv` is normalised against: the path's own box, so a
    // gradient across a fill runs from its top to its bottom rather than from
    // the canvas' (RFC-0035 reused verbatim).
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for command in commands {
        let points: &[[f32; 2]] = match command {
            PathCommand::Move(p) | PathCommand::Line(p) => std::slice::from_ref(p),
            PathCommand::Quad(c, p) => &[*c, *p],
            PathCommand::Cubic(a, b, p) => &[*a, *b, *p],
            PathCommand::Close => &[],
        };
        for p in points {
            min_x = min_x.min(p[0]);
            min_y = min_y.min(p[1]);
            max_x = max_x.max(p[0]);
            max_y = max_y.max(p[1]);
        }
    }
    let bounds = [
        min_x,
        min_y,
        (max_x - min_x).max(1e-3),
        (max_y - min_y).max(1e-3),
    ];

    let mut buffers: VertexBuffers<byard_core::encoder::canvas_fill::FillVertex, u32> =
        VertexBuffers::new();
    let options = FillOptions::tolerance(tolerance).with_fill_rule(if even_odd {
        FillRule::EvenOdd
    } else {
        FillRule::NonZero
    });
    let ok = FillTessellator::new()
        .tessellate_path(
            &path,
            &options,
            &mut BuffersBuilder::new(&mut buffers, WithUv { bounds }),
        )
        .is_ok();
    if !ok {
        // A tessellation failure is a path the tessellator could not make
        // sense of. An empty mesh draws nothing, which is what a shape nobody
        // could triangulate looks like; the alternative is garbage triangles.
        return byard_core::frame::FillMesh::default();
    }
    byard_core::frame::FillMesh {
        vertices: buffers.vertices,
        indices: buffers.indices,
        bounds,
    }
}

/// One outstanding controller request a native view issued (RFC-0039).
#[derive(Clone, Copy)]
struct NativeRequest {
    /// Which view asked.
    slot: usize,
    /// The view table's generation when it asked.
    generation: u64,
    /// The key the view chose, handed back with the answer.
    key: byard_core::render::RequestKey,
}

/// A native view laid out in the last render (RFC-0039), and where.
///
/// The event half of the ABI: an input event is offered to the views under the
/// pointer, innermost first, and a view that says [`Handled::Yes`] stops it, so
/// a package widget takes part in routing under the rules an intrinsic follows
/// rather than beside them.
///
/// [`Handled::Yes`]: byard_core::render::Handled::Yes
#[derive(Clone, Copy)]
struct NativeTarget {
    /// Which live view, by slot.
    slot: usize,
    /// Its on-screen rect in logical pixels.
    rect: crate::interp::intrinsics::Rect,
}

/// A wheel/drag-scrollable region recorded during render (RFC-0005
/// `ScrollView`). `dispatch_events` turns a wheel or a drag over `rect` into a
/// clamped write to whichever of `offset.x`/`offset.y` is a writable `var`.
#[derive(Clone, Copy)]
struct ScrollTarget {
    /// Viewport rect in logical screen px (the wheel/drag hit region).
    rect: crate::interp::intrinsics::Rect,
    /// Horizontal axis, present when `offset.x` is a writable `var`.
    x: Option<ScrollAxis>,
    /// Vertical axis, present when `offset.y` is a writable `var`.
    y: Option<ScrollAxis>,
    /// The `ScrollView` element index, for firing engine scroll events
    /// (`end_reached`/`page_change`/`scroll_end`), RFC-0021.
    elem: Option<u32>,
    /// RFC-0021 `snap` mode (`none`/`page`/`item`). For `item`, the
    /// `snap_align`-adjusted child boundaries live in
    /// [`Interpreter::scroll_item_bounds`], keyed by [`elem`](Self::elem).
    snap: SnapMode,
    /// RFC-0021 `snap_spring` override, the packed curve driving the snap glide,
    /// or `None` for the engine default spring.
    snap_spring: Option<byard_core::frame::MotionCurve>,
    /// RFC-0021 reflected `page:` var, written the current page index on a
    /// page-snap settle; a `page_change` fires when it changes.
    page_sig: Option<SignalId>,
    /// RFC-0021 `end_threshold` (0..1): the fraction of the scrollable extent at
    /// which `end_reached` fires. `None` when no `end_reached` handler exists.
    end_threshold: Option<f32>,
    /// RFC-0021 `pull_refresh`: overscrolling past the top drags an elastic pull
    /// region (see [`Interpreter::pull_distance`]); releasing past the threshold
    /// fires `refresh`. A pull view needs no `offset` var, the pull region is
    /// engine state, not the scroll offset.
    pull_refresh: bool,
    /// RFC-0021 reflected `refreshing:` var, set `true` by the engine when a pull
    /// triggers a refresh; the app sets it back to `false` when its load finishes,
    /// which springs the indicator away.
    refreshing_sig: Option<SignalId>,
}

/// An in-flight smooth snap (RFC-0021 §2): a spring driving one `ScrollView`
/// axis's offset signal to an exact page boundary. Seeded when scrolling settles
/// (drag release or scroll-quiet), advanced each `render` from the shared engine
/// clock, and dropped once [`Motion::is_settled_with_eps`] reports it has
/// arrived, at which point the offset is pinned to `target` and `scroll_end`
/// fires. Cancelled outright by any fresh scroll/drag on the same elem.
#[derive(Clone, Copy)]
struct SnapAnim {
    /// The offset-axis signal the spring writes each frame.
    sig: SignalId,
    /// The spring itself (`from` = offset at settle, `to` = `target`).
    motion: byard_core::frame::Motion,
    /// The exact page boundary to pin the offset to on settle.
    target: f32,
}

/// The visible slice of a windowed `ScrollView` list (RFC-0005 windowed layout):
/// only rows `start..end` of a uniform-height list are built, laid out, and
/// emitted, with a leading/trailing [`Spacer`] standing in for the elided rows so
/// the content extent (and thus the scroll clamp) and every visible row's
/// position stay exact. Computed identically in the build and render passes from
/// the same offset, so the parallel flat-id cursor stays aligned.
#[derive(Clone, Copy)]
struct WindowSpec {
    /// Index of the first materialised row.
    start: usize,
    /// One past the last materialised row (`start..end` is the live slice).
    end: usize,
    /// Fixed per-row extent in logical px (spacing folded in).
    row_height: f32,
    /// Total row count, so the trailing spacer covers `n − end` rows.
    n: usize,
}

/// One resolved drop shadow (RFC-0011 custom shadows): offset, blur, spread, and
/// resolved RGBA colour. A box may carry several, CSS-style layered shadows,
/// each emitted as its own shadow-only `DecoratedBox` beneath the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowSpec {
    dx: f32,
    dy: f32,
    blur: f32,
    spread: f32,
    color: [f32; 4],
}

/// Default drop-shadow colour (`0xAARRGGBB`, ~33% black) when a shadow omits its
/// own `color`.
const DEFAULT_SHADOW_COLOR: i64 = 0x5500_0000;

/// Default `NavStack` `max_depth` (RFC-0026 resolved question "memory
/// pressure"): a stack deeper than this is almost certainly a push loop, not a
/// screen hierarchy. `max_depth: 0` disables the guard.
const DEFAULT_NAV_MAX_DEPTH: i64 = 10;

/// Fraction of the viewport an edge-swipe must cross to complete a pop on
/// release (RFC-0026 §"Swipe-back gesture"); short of it, the screen snaps back.
const NAV_SWIPE_COMMIT: f32 = 0.5;

/// Width of the left-edge strip an interactive swipe-back may start in, in
/// logical px, the iOS convention, and narrow enough that a horizontally
/// scrolling child in the middle of the screen is never stolen from.
const NAV_SWIPE_EDGE: f32 = 24.0;

/// The progress curve for the *remaining* `fraction` of a transition, what a
/// released swipe-back hands over to (RFC-0026): the same ramp, shortened to the
/// distance still to travel, with a floor so a hand-off at the very end is still
/// a frame or two of motion rather than a snap.
fn nav_transition_tail(
    transition: crate::interp::nav::NavTransition,
    fraction: f32,
) -> byard_core::frame::MotionCurve {
    /// Shortest tail worth animating: one 60 Hz frame.
    const MIN_TAIL_MS: f32 = 16.0;
    let mut curve = nav_progress_curve(transition);
    curve.params[0] = (curve.params[0] * fraction.clamp(0.0, 1.0)).max(MIN_TAIL_MS);
    curve
}

/// The `0 → 1` progress curve driving a route transition (RFC-0026
/// §"Transitions"): a **fixed-duration, monotone** ramp, decelerating into
/// place for the positional transitions, symmetric for the cross-fade.
///
/// Deliberately *not* a spring, even though a spring drives every other
/// animation in the engine. A spring is the right primitive for a value that
/// may be retargeted mid-flight; it is the wrong one for normalized progress.
/// RFC-0010's default spring is underdamped (`ζ ≈ 0.69`), and a screen's
/// arrival must not overshoot: past `p = 1` the incoming screen would slide off
/// its own leading edge, and the undershoot that follows would walk it visibly
/// back out again, a wobble at the exact moment the transition is meant to be
/// over. Clamping `p` hides the overshoot but not the undershoot, and it makes
/// the settle test (which reads the *unclamped* curve) disagree with what is on
/// screen, so the engine keeps asking for frames long after the motion looks
/// finished and can park with the screen a few pixels out of place.
///
/// A duration ramp has none of those problems: bounded to `0..=1` by
/// construction, monotone, and landing on exactly `1.0` at exactly
/// [`duration_ms`](crate::interp::nav::NavTransition::duration_ms). The
/// decelerating cubic is the shape a *critically damped* spring traces, the
/// feel RFC-0026 asks for, without the asymptotic tail that never ends.
fn nav_progress_curve(
    transition: crate::interp::nav::NavTransition,
) -> byard_core::frame::MotionCurve {
    use crate::interp::nav::NavTransition;
    let kind = match transition {
        // Decelerate into place: fast off the mark, easing to a stop.
        NavTransition::Slide | NavTransition::SlideUp => byard_core::frame::MotionCurve::EASE_OUT,
        // A cross-fade has no direction to decelerate along.
        NavTransition::Fade | NavTransition::None => byard_core::frame::MotionCurve::EASE_IN_OUT,
    };
    byard_core::frame::MotionCurve {
        kind,
        #[allow(clippy::cast_precision_loss)]
        params: [transition.duration_ms() as f32, 0.0, 0.0],
    }
}

/// The navigable path inside a deep-link URL (RFC-0026 §"Deep linking"): the
/// scheme and authority belong to the platform, the path to the route table.
/// `byard://item/42` → `/item/42`; a bare `/item/42` passes through unchanged.
/// A query string or fragment is dropped, v1 routes match on the path alone.
fn deep_link_path(url: &str) -> String {
    let path = match url.split_once("://") {
        Some((scheme, rest)) => {
            // A web link's first segment is a host (`app.example/item/42`); a
            // custom app scheme has no authority, so `byard://item/42` is all
            // path. Told apart by the scheme and by the shape of that first
            // segment, a host has a dot or a port, a route segment does not.
            let authority = rest.split('/').next().unwrap_or("");
            let has_host = matches!(scheme, "http" | "https")
                || authority.contains('.')
                || authority.contains(':');
            if has_host {
                rest.find('/').map_or("/", |i| &rest[i..])
            } else {
                rest
            }
        }
        // `byard:/item/42` (scheme-relative) or a bare `/item/42`.
        None => url.split_once(':').map_or(url, |(_, p)| p),
    };
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_end_matches('/');
    if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Pushes a single rounded stroke quad from `a` to `b` of thickness `t`,
/// rotated to the segment angle about its midpoint and composed under
/// `transform` (so an element/group transform carries the mark with it). Backs
/// the RFC-0018 `Checkbox` checkmark. Emitted on the **decorated** pipeline so it
/// paints above the checkbox's decorated (bordered) container, which is pushed
/// first.
fn push_stroke_quad(
    frame: &mut byard_core::frame::RenderFrame,
    a: [f32; 2],
    b: [f32; 2],
    t: f32,
    color: [f32; 4],
    transform: byard_core::frame::Transform,
) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();
    let cx = f32::midpoint(a[0], b[0]);
    let cy = f32::midpoint(a[1], b[1]);
    let seg = byard_core::frame::Transform {
        rotate: dy.atan2(dx),
        origin: [cx, cy],
        ..byard_core::frame::Transform::IDENTITY
    };
    frame.push_decorated(byard_core::frame::DecoratedBox {
        base: byard_core::BoxInstance {
            rect: [cx - len / 2.0, cy - t / 2.0, len, t],
            color,
            radii: [t / 2.0; 4],
            transform: transform.compose(&seg),
            smooth: 0.0,
        },
        dirty: true,
        ..Default::default()
    });
}

/// Shifts a hit rect by the accumulated scroll displacement and clips it to
/// the enclosing scroll viewport (RFC-0005): interaction follows the content
/// to its *on-screen* position, and content scrolled out of the viewport
/// stops being hittable, it is not visible, so it must not be tappable.
fn scrolled_hit_rect(
    r: crate::interp::intrinsics::Rect,
    shift: (f32, f32),
    viewport: Option<byard_core::frame::Rect>,
) -> crate::interp::intrinsics::Rect {
    let shifted = crate::interp::intrinsics::Rect::new(r.x + shift.0, r.y + shift.1, r.w, r.h);
    let Some(v) = viewport else {
        return shifted;
    };
    let x0 = shifted.x.max(v.x);
    let y0 = shifted.y.max(v.y);
    let x1 = (shifted.x + shifted.w).min(v.x + v.width);
    let y1 = (shifted.y + shifted.h).min(v.y + v.height);
    crate::interp::intrinsics::Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// A shadow-only [`DecoratedBox`](byard_core::frame::DecoratedBox): the box's
/// geometry (rect/radii/transform) with a transparent fill and no border, so it
/// casts `sh` beneath the surface. Emitted per shadow (RFC-0011 layered shadows).
fn shadow_decorated(
    base: byard_core::BoxInstance,
    opacity: f32,
    sh: &ShadowSpec,
) -> byard_core::frame::DecoratedBox {
    byard_core::frame::DecoratedBox {
        base: byard_core::BoxInstance {
            color: [0.0; 4],
            ..base
        },
        border_width: 0.0,
        border_color: [0.0; 4],
        shadow_dx: sh.dx,
        shadow_dy: sh.dy,
        shadow_blur: sh.blur,
        shadow_spread: sh.spread,
        shadow_color: sh.color,
        opacity,
        // A shadow cast is geometry, not fill: the element's own ramp belongs to
        // its surface, never to the blurred silhouette beneath it.
        gradient: None,
        dirty: true,
    }
}

/// The per-instance parameter bindings produced by binding a user-view call's
/// arguments to the callee's declared parameters (RFC-0007 §3). Each entry
/// is a reactive memo projecting the argument expression over the *parent*
/// scope, so a parameter fed a parent `var` stays live (RFC-0004); a literal
/// argument lowers to a constant memo with no dirty edges.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct InstanceBindings {
    /// Successfully bound `(param name, projecting memo)` pairs, in parameter
    /// declaration order.
    pub bindings: Vec<(Symbol, ScopeId)>,
}

/// The default-value expression of a parameter, if it declares one (RFC-0007
/// D-B). A parameter with a default is not required at the call site;
/// the default is evaluated in the callee scope when the argument is omitted.
fn param_default(param: &Param) -> Option<&Expr> {
    param.default.as_ref()
}

/// Whether a parameter is a callback prop, declared with a function type
/// `Fn(...)` (RFC-0019). Callback params bind a caller-supplied action block
/// rather than a projected value, so they take a separate binding path and are
/// skipped by the ordinary value-argument machinery in [`Interpreter::bind_args`].
fn is_callback_param(param: &Param) -> bool {
    matches!(param.ty, Some(Type::Function { .. }))
}

/// The reserved parameter/element name for a user view's child-block slot
/// (RFC-0007 D-A). A `View` declaring a `content` parameter accepts a
/// `{ ... }` block at its call sites; referencing `content` as an element inside
/// the body splices the caller-supplied block.
const RESERVED_CONTENT: &str = "content";

thread_local! {
    /// Thread-local storage holding the active payload of the event currently being processed.
    pub static CURRENT_PAYLOAD: std::cell::RefCell<Option<Value>> = const { std::cell::RefCell::new(None) };
}

/// The Dev-mode interpreter for one `View` instance: a reactive context plus
/// the View's binding environment.
#[derive(Default)]
pub struct Interpreter {
    ctx: ReactiveCtx,
    env: Env<'static>,
    next_target: u32,
    errors: Vec<CompileError>,
    /// `var` name → its `Signal`, so a hot-reload can preserve state by
    /// rebinding instead of re-initializing (RFC-0004 §11).
    var_sigs: std::collections::HashMap<Symbol, SignalId>,
    /// Incremental LayoutAtlas.
    pub atlas: byard_core::atlas::layout::LayoutAtlas,
    /// Interactive events router.
    pub router: crate::interp::events::EventRouter,
    /// Glyph-accurate text measurer, created lazily on first layout so the
    /// non-rendering paths (parsing, reactivity tests) never load fonts.
    text_measurer: Option<byard_core::text::TextMeasurer>,
    /// Active design-token theme (RFC-0022; the theme-default layer).
    pub theme: super::theme::Theme,
    /// The registered font families, in the shape the render thread reads
    /// them (RFC-0034), rebuilt whenever the theme changes and handed to
    /// every frame.
    ///
    /// One table rather than a per-frame pool: see
    /// [`FontTable`](byard_core::frame::FontTable). Kept beside the theme it
    /// was built from so a hot-reload that swaps the theme swaps this in the
    /// same breath, rather than leaving the render thread holding faces the
    /// measurer no longer knows about.
    fonts: std::sync::Arc<byard_core::frame::FontTable>,
    /// The reactive `Bool` signal backing the theme's active scheme (`true` ⇒
    /// dark), created by [`set_theme`](Self::set_theme). `theme.primary` reads it
    /// (tracked) and `theme.dark = …` / `bind: theme.dark` writes it, so a scheme
    /// flip drives Mark-and-Pull across every token reference (RFC-0022 §1).
    theme_scheme: Option<SignalId>,
    /// Parameterized `fn` definitions (`fn f(params) => body`, M25) *and*
    /// callback-prop bindings (RFC-0019): stored as `(param names, body expr,
    /// is_callback)` and indexed by `AstId`. Both share the invocation path in
    /// [`Self::lower_call`], a callback is a caller-supplied action block
    /// inlined at the callee's invocation site; the `is_callback` flag turns on
    /// the RFC-0019 §4 arity/invocability diagnostics that plain `fn`s don't
    /// want.
    fn_table: Vec<(Vec<Symbol>, Expr, bool)>,
    /// The resolved user-`View` registry for this program (RFC-0007 §1).
    /// Built once from `ParsedFile::views` via [`Interpreter::load_views`]; a
    /// call whose name resolves here is a user-view instantiation, not a
    /// container.
    view_table: super::views::ViewTable,
    /// Current user-view instantiation depth, bounded by [`MAX_INSTANCE_DEPTH`]
    /// to guard against runaway guarded recursion (RFC-0007 §4).
    instance_depth: u32,
    /// Current `for`-body lowering depth. Like `instance_depth`, a non-zero
    /// value means an event action lowered *now* must capture the live
    /// environment (the loop variable `t`) so a render-time re-lowering of the
    /// action still resolves `t`, the loop binding is truncated out of the
    /// persistent View env after the body is lowered (RFC-0018 × RFC-0027).
    for_depth: u32,
    /// Current `route`/`tab` body lowering depth (RFC-0026). Like `for_depth`, a
    /// non-zero value means the body being lowered has extra bindings in scope
    /// (`route`, and the `{|params| … }` name) that are truncated out of the
    /// persistent View env afterwards, so anything re-lowered at render time
    /// must capture them in its env snapshot.
    nav_depth: u32,
    /// Current depth of a *restored* environment: a pooled body being lowered
    /// at render time with a captured snapshot pushed back onto `self.env`
    /// (a lazily-lowered `when` branch). The bindings it restores, a loop
    /// element, an instance param, are truncated out again the moment the
    /// body is lowered, so anything inside it that will be re-lowered or
    /// re-evaluated later must capture them now, exactly as it would inside the
    /// `for` body or route body the snapshot came from.
    restored_depth: u32,
    /// Stack of caller-supplied child-block slots, one frame per active
    /// user-view instance (RFC-0007 D-A). The block is
    /// pre-lowered in the *caller* scope so a `content` element reference inside
    /// the callee body splices nodes that capture the caller's environment.
    slot_stack: Vec<Vec<RenderNode>>,
    /// Reactive `for` body pools (RFC-0018), indexed by [`RenderNode::For::pool`].
    /// Grows as `for` loops are lowered; each pool holds that loop's reusable
    /// per-slot bodies and element signals. Reconciled once per frame before the
    /// layout/paint walk.
    for_pools: Vec<ForPool>,
    /// Reactive `when` branch caches (RFC-0018), indexed by
    /// [`RenderNode::When::pool`]. Each branch is lowered lazily on first
    /// selection so an untaken (recursive) branch never lowers until shown.
    when_pools: Vec<WhenPool>,
    /// Navigation stacks (RFC-0026), indexed by [`RenderNode::Nav::pool`]. Each
    /// holds one container's route table, its live (preserved) screens and its
    /// in-flight transition. Reconciled once per frame, before layout, against
    /// the driving `path`/`active` projection.
    nav_pools: Vec<NavPool>,
    /// The element index each navigation container laid out at in the last
    /// render, parallel to [`nav_pools`](Self::nav_pools). Kept beside the pools
    /// rather than inside them because the pools are lent out (immutably) for
    /// the whole build/paint phase, and this is written during it.
    nav_elems: Vec<Option<u32>>,
    /// RFC-0026: the shared navigation state each container publishes for the
    /// lowered `back`/`replace` closures, keyed by the navigation `var` that
    /// drives it. Keyed rather than indexed by pool because an action can be
    /// lowered *before* the container it drives (a back button written above its
    /// `NavStack`) and re-lowered during a render, when the pools are lent out,
    /// the `var` is the one identity both sides always agree on.
    nav_shared: std::collections::HashMap<SignalId, NavSharedCell>,
    /// RFC-0026 `swipe_back`: the viewport rect of each swipe-enabled `NavStack`
    /// recorded during the last render, paired with its pool, the same
    /// render-then-dispatch handshake the scroll targets use, so the edge
    /// gesture hit-tests against what was actually painted.
    nav_targets: Vec<(crate::interp::intrinsics::Rect, usize)>,
    /// The in-flight edge-swipe back gesture, if any: the pool being dragged and
    /// the pointer x at the press.
    nav_swipe: Option<(usize, f32)>,
    /// Current engine time (ms since the runner's epoch), set once per frame by
    /// the runner via [`set_now_ms`](Self::set_now_ms). Drives `with`
    /// animations (RFC-0010).
    now_ms: u32,
    /// Whether a host has ever advanced the clock. Distinguishes a real
    /// `set_now_ms(0)` start from "the clock was never set", without it, a host
    /// that never ticks the clock would pin an animation at `t = 0` (never
    /// settling, `has_active_animations` latched true, an infinite redraw loop
    /// on a wait-based runner). Unset ⇒ animations resolve to their target
    /// instantly.
    clock_set: bool,
    /// Persisted per-property animation state (RFC-0010), keyed by the `with`
    /// node's source span **and the element instance running it** ([`AnimKey`])
    /// so it survives the whole-tree re-render each frame without ten `for`
    /// rows sharing one `Motion`.
    /// A mid-flight target change reseeds `from` to the current sampled value
    /// (interruptible springs).
    animations: std::collections::HashMap<AnimKey, byard_core::frame::Motion>,
    /// Persisted colour-animation state (RFC-0010 A3): one `Motion` per OKLab
    /// channel (`L`, `a`, `b`) plus one for the alpha byte, so a
    /// `bg`/`color`/`border`/`backdrop_tint` transition interpolates in a
    /// perceptually-uniform space, no muddy mid-points, with translucency
    /// animating alongside (RFC-0023: a tint fading in is an alpha ramp), and
    /// is interruptible like the scalar props. Keyed by the `with` node's span.
    color_animations: std::collections::HashMap<AnimKey, [byard_core::frame::Motion; 4]>,
    /// Loop clocks for repeating, delayed and keyframed animations (RFC-0025),
    /// keyed by the animation node's span like the two maps above. A repeating
    /// animation cannot sample against `now − start_ms` the way a one-shot does:
    /// it needs its own timeline, which is what a [`LoopClock`] carries, plus
    /// the last-sampled stamp that implements §2's offscreen pause.
    anim_clocks: std::collections::HashMap<AnimKey, LoopClock>,
    /// Live ripple ink reveals (RFC-0023), spawned by a press gesture over an
    /// element whose resolved `ripple_active` is true. Gesture-like state: it
    /// persists across renders (a ripple keeps fading after release) and is
    /// retired by time at the top of each [`render`](Self::render). An entry
    /// whose element no longer renders simply stops being emitted and ages
    /// out the same way, hot-reload safe with no extra bookkeeping.
    ripples: Vec<ActiveRipple>,
    /// The last press gesture `(elem, press time)` that already spawned its
    /// ripple, the rising-edge latch. A single global slot suffices because
    /// there is one pointer, hence at most one in-flight press (E4): a hold
    /// never respawns (same identity, even after its ripple retires), while
    /// each new tap is a fresh identity and spawns again.
    ripple_spawned: Option<(u32, u64)>,
    /// Runtime performance diagnostics recomputed each render (RFC-0023):
    /// currently the overlapping-blurs stack check, run over the frame's
    /// emitted backdrops at the end of [`render`](Self::render).
    perf_warnings: Vec<PerfWarning>,
    /// RFC-0038 `on measure`: one slot per element that declared the event,
    /// allocated at lower time and indexed by
    /// [`RenderNode::Box::measure`](RenderNode::Box). An element that does not
    /// declare it allocates nothing, which is the whole cost model: the event
    /// is a slot, not a per-element field.
    measures: Vec<MeasureSlot>,
    /// The elements to measure this frame, `(laid-out node, slot)`, refilled by
    /// the layout build walk.
    ///
    /// Recorded by the build rather than rediscovered afterwards because the
    /// build is where a `RenderNode` and its atlas node are in the same hand;
    /// a second walk would have to re-derive that pairing and could disagree
    /// with it.
    measure_targets: Vec<(byard_core::atlas::layout::AtlasNodeId, u32)>,
    /// The element instance the render walk is currently inside, the `slot`
    /// half of an [`AnimKey`].
    ///
    /// Set from the expansion, which is the only place that knows it: after
    /// `when`/`for` are flattened, a row of a `for` is indistinguishable from a
    /// node written literally at that position. `0` outside any `for` body.
    anim_slot: u32,
    /// Set true during a render whenever an animation sampled this frame has not
    /// yet settled, the runner reads it (via [`has_active_animations`]) to keep
    /// requesting frames until motion stops (idle → 0 frames).
    ///
    /// [`has_active_animations`]: Self::has_active_animations
    any_active: bool,
    /// First-class style values (RFC-0016): `let name = style { … }` registers
    /// its base attributes and `on <state>` blocks here, and a `..name` spread
    /// on an element splices them in at lower time. Static and view-scoped, no
    /// cascade.
    styles: std::collections::HashMap<Symbol, StyleDef>,
    /// Dev-mode MSDF generation cache/dispatcher for `VectorIcon` (RFC-0009
    /// §2). Drained once per [`render`](Self::render) call, before the tree
    /// walk, so a freshly-resident glyph is visible the same tick it lands.
    vector_jit: crate::vector::VectorJit,
    /// Wheel-scroll targets recorded during the last render (RFC-0005): one per
    /// `ScrollView` whose `offset.y` is a writable signal. `dispatch_events`
    /// reads this to convert a wheel into a clamped scroll, the same
    /// render-then-dispatch handshake the router's hit rects use.
    scroll_targets: Vec<ScrollTarget>,
    /// Live native-view instances (RFC-0039), indexed by a lowered element's
    /// `slot`.
    ///
    /// Dropped and rebuilt whenever the tree is lowered, which is what makes
    /// mount and unmount the element's own lifetime rather than a second thing
    /// for a package author to get right.
    native_views: Vec<Box<dyn byard_core::render::NativeView>>,
    /// Tessellated path meshes, keyed by a fingerprint of the commands that
    /// produced them (RFC-0037).
    ///
    /// The cache that makes a live chart affordable: the commands are
    /// re-evaluated every tick, and the mesh is rebuilt only when the numbers
    /// they produced actually changed.
    path_meshes: std::collections::HashMap<u64, CachedMesh>,
    /// How many meshes have been tessellated this session, the measurement
    /// behind the caching claim (INV-19).
    tessellations: u64,
    /// Which view issued which of this frame's native controller requests
    /// (RFC-0039), as `(slot, range into the frame's call pool)`.
    ///
    /// Collected during the walk and consumed at the end of it, because the
    /// answer has to come back to the view that asked and a `RenderCtx` is
    /// deliberately not told which view it belongs to.
    native_call_owners: Vec<(usize, std::ops::Range<usize>)>,
    /// A native view's outstanding requests, by continuation id: which view
    /// asked, under which key, and which generation of the view table it
    /// belonged to (RFC-0039).
    native_waiting: std::collections::HashMap<u64, NativeRequest>,
    /// How many times the view table has been rebuilt.
    ///
    /// A reply that comes back after a re-lower belongs to a view that no
    /// longer exists, even though some *other* view may now hold its slot.
    /// The generation is what tells those two cases apart, and it is why a
    /// late tile does not land in whatever widget inherited the index.
    native_generation: u64,
    /// Native views laid out in the last render, with the rect each occupies
    /// (RFC-0039 × RFC-0003).
    ///
    /// The same render-then-dispatch handshake `scroll_targets` uses: the walk
    /// records where each view ended up, and `dispatch_events` routes the
    /// tick's input against it.
    native_targets: Vec<NativeTarget>,
    /// The drag-to-scroll gesture in flight, if any (RFC-0005). Set when a
    /// pointer press lands on inert `ScrollView` content; each move writes the
    /// offset so the content tracks the pointer; cleared on release.
    scroll_drag: Option<ScrollDrag>,
    /// RFC-0021 `on_end_reached` debounce: `ScrollView` element indices currently
    /// past their `end_threshold` and already fired. An elem re-fires only after
    /// its offset falls back below the threshold (removed here), so appending
    /// items, which lowers the fraction, re-arms it. Persists across ticks
    /// (gesture-like state), keyed by the stable element index.
    end_reached_fired: std::collections::HashSet<u32>,
    /// RFC-0021 reflected `page:`, the last page value synced to the offset per
    /// `ScrollView` elem. Edge-triggered: when the `page` var differs from this
    /// (the app set it), the offset scrolls to `page × viewport`; a drag never
    /// changes `page` mid-gesture, so this never fights scrolling.
    scroll_page_last: std::collections::HashMap<u32, i64>,
    /// RFC-0021 snap settle: the [`frame_seq`](Self::frame_seq) of the last
    /// wheel/trackpad scroll input per `snap`-enabled `ScrollView` elem. Snapping
    /// waits until an elem has been *quiet* (no scroll input) for a few frames, so
    /// trackpad momentum, a stream of ever-smaller deltas that leaves the offset
    /// looking briefly "still", cannot trigger a snap mid-fling that then fights
    /// the next momentum event. Clock-independent (frame-counted), so it settles
    /// identically whether or not the host advances `now_ms`.
    scroll_quiet: std::collections::HashMap<u32, u64>,
    /// RFC-0021 smooth snap: the in-flight spring driving a `snap: page` view's
    /// offset to its target page, per elem. Seeded on drag release / scroll-quiet
    /// settle and advanced each `render` until it settles (then removed and the
    /// offset pinned exactly on the page). A fresh scroll/drag on the elem cancels
    /// it so the user always takes over cleanly (interruptible).
    snap_anims: std::collections::HashMap<u32, SnapAnim>,
    /// RFC-0021 `snap: item`: the candidate rest offsets (one per direct child,
    /// pre-adjusted for `snap_align` and clamped to the scroll extent) per
    /// `ScrollView` elem. Rebuilt each render from the laid-out child rects; the
    /// item settle picks the candidate nearest the current offset. Empty for
    /// `snap: none`/`page`.
    scroll_item_bounds: std::collections::HashMap<u32, Vec<f32>>,
    /// RFC-0021 pull-to-refresh: the current elastic pull-region height per
    /// `pull_refresh` `ScrollView` elem (0 when idle). Grown by a downward
    /// over-drag at the top, sprung back by [`pull_anims`](Self::pull_anims). The
    /// content is painted shifted down by this much, with the indicator in the gap.
    pull_distance: std::collections::HashMap<u32, f32>,
    /// `as <name>` anchors and the rect each resolved to this frame (RFC-0036).
    ///
    /// Filled by the main render walk and read by the overlay pass, which runs
    /// after it — which is the whole reason an overlay may only anchor to a
    /// name declared before it: by the time the overlay is placed, the anchor's
    /// rect is a fact rather than a prediction.
    anchor_rects: std::collections::HashMap<Symbol, crate::interp::intrinsics::Rect>,
    /// RFC-0021 pull-to-refresh: the in-flight spring retracting or resting the
    /// pull region per elem (see [`PullAnim`]).
    pull_anims: std::collections::HashMap<u32, PullAnim>,
    /// RFC-0021 pull-to-refresh: the last-observed `refreshing:` value per elem, so
    /// the app clearing it (`true → false`) edge-triggers the retract spring
    /// without fighting the engine's own set on trigger.
    refreshing_seen: std::collections::HashMap<u32, bool>,
    /// RFC-0021 fling projection: the latest signed scroll velocity (px/s, positive
    /// = offset increasing) per `ScrollView` elem, estimated from the offset change
    /// between scroll inputs over their `time_ms`. A settle above the fling
    /// threshold projects one boundary in this direction instead of snapping to the
    /// nearest.
    scroll_vel: std::collections::HashMap<u32, f32>,
    /// Fling-velocity bookkeeping: the `(offset, time_ms)` of the last scroll input
    /// per elem, differenced against the next to estimate [`scroll_vel`](Self::scroll_vel).
    scroll_vel_last: std::collections::HashMap<u32, (f32, u64)>,
    /// Monotonic render counter (RFC-0021 snap timing): bumped once at the top of
    /// every [`render`](Self::render). Drives the frame-counted "scroll has gone
    /// quiet" test in [`scroll_quiet`](Self::scroll_quiet) without depending on an
    /// advancing wall clock.
    frame_seq: u64,
    /// RFC-0018 `RadioButton` groups: the ordered `value`s of every radio sharing
    /// a `bind:` group var, keyed by that var's [`SignalId`]. Rebuilt each render
    /// (cleared at the top of [`render`](Self::render), appended as each radio is
    /// painted, in declaration order). Each group's ordering is shared into that
    /// group's radios' arrow-key handlers via a cheap `Rc` clone, so the handlers
    ///, which fire *after* the full render has populated the vector, can move
    /// selection to the next/previous value with wrap-around (WAI-ARIA radio
    /// group pattern). `Rc`/`RefCell` are sound here: the interpreter and its
    /// event closures are single-threaded logic-thread state (`!Send`).
    radio_groups: std::collections::HashMap<SignalId, std::rc::Rc<std::cell::RefCell<Vec<String>>>>,
    /// RFC-0032 §R4 retained-path state.
    retained: RetainedLayout,
    /// RFC-0032 §R3 step 6: the hash of every primitive this frame emitted, in
    /// pool order, kept so the next frame can answer "did this primitive
    /// change?" by comparing resolved values rather than by asserting `true`.
    paint: byard_core::frame::PaintDigest,
    /// The controller boundary (RFC-0028): the dispatcher, the call queue, the
    /// continuation table and the structural effects. One field rather than
    /// six, see [`controller::Bridge`].
    bridge: controller::Bridge,
}

/// What [`Interpreter::render`] remembers between frames so it can decide
/// whether the layout tree may be retained (RFC-0032 §R4).
///
/// Every field here exists to answer *one* eligibility question, and the
/// default answer is always "rebuild": a `None`/`false`/mismatched value takes
/// the full path. New structural mutations that nobody classified therefore
/// fall on the safe side by construction rather than by review.
#[derive(Default)]
struct RetainedLayout {
    /// The atlas node ids the last successful build produced, in build order.
    /// Retained rather than local (it was `let flat_ids = Vec::new()` inside
    /// `render`) because the retained path **must reuse the stored ids**:
    /// `next_target_index()` is `nodes_by_index.len()`, so re-deriving them
    /// would silently reassign every id.
    flat_ids: Vec<byard_core::atlas::layout::AtlasNodeId>,
    /// Viewport of the last successful build, a resize forces a rebuild.
    viewport: Option<(f32, f32)>,
    /// How many overlays and how deep each navigation container was, so an
    /// overlay or route mount/unmount forces a rebuild.
    ///
    /// RFC-0032 §R4 justified this clause by saying those pools do not travel
    /// through `reconcile_structure`. They do: `reconcile_structure` descends
    /// into `RenderNode::Nav` via `reconcile_nav`, and an overlay mounts behind
    /// a `when`. Measured on every case the suite can construct, `shape` and
    /// `structure_changed` reject the same frames, so this is defence in
    /// depth, not the sole guard the RFC described, and it is kept as such: it
    /// is a *deny* clause, and the direction that costs a rebuild is the safe
    /// one. Deleting `structure_changed` leaves the overlay and route tests
    /// green on this clause alone, and vice versa.
    shape: Option<(usize, Vec<usize>)>,
    /// Active colour scheme at the last successful build. A flip changes
    /// nearly every resolved value at once, so it forces a rebuild (RFC-0032
    /// §Q6), compared here rather than hooked onto the setter, because
    /// `theme.dark = …`, `bind: theme.dark` and the programmatic setter are
    /// three different write paths into the same signal and a hook would have
    /// to catch all three.
    dark: Option<bool>,
    /// Set by [`Interpreter::reload`] and [`Interpreter::set_theme`]; cleared
    /// by the next render, which is forced to be a full one.
    invalidated: bool,
}

/// One axis of a live [`ScrollDrag`]: the signal to write and its value at the
/// press, so the live offset is a pure function of the pointer travel.
#[derive(Clone, Copy)]
struct ScrollDragAxis {
    /// The signal backing this axis's offset component, written as it moves.
    sig: SignalId,
    /// The offset at the press; the live offset is this minus the pointer travel.
    start_offset: f32,
    /// Maximum scroll distance on this axis (content − viewport), clamped ≥ 0.
    max: f32,
    /// Whether `sig` holds an `Int` (write back rounded) or a `Float`.
    is_int: bool,
}

/// A live drag-to-scroll gesture (RFC-0005 `ScrollView`): the content follows
/// the pointer between press and release. Captured at press so the offset is a
/// pure function of the pointer travel, no accumulated drift (IMPL-10).
#[derive(Clone, Copy)]
struct ScrollDrag {
    /// Pointer position at the press, in logical screen px.
    start_pos: (f32, f32),
    /// Horizontal axis, present when `offset.x` is a writable `var`.
    x: Option<ScrollDragAxis>,
    /// Vertical axis, present when `offset.y` is a writable `var`.
    y: Option<ScrollDragAxis>,
    /// The dragged `ScrollView` element (for RFC-0021 snap-settle activity).
    elem: Option<u32>,
    /// RFC-0021 `pull_refresh`: whether the dragged view enables pull-to-refresh,
    /// so a downward over-drag at the top grows the pull region.
    pull_refresh: bool,
    /// RFC-0021 reflected `refreshing:` var of the dragged view, set `true` when a
    /// release past the threshold triggers a refresh.
    refreshing_sig: Option<SignalId>,
}

/// One live ripple ink reveal (RFC-0023): spawned on a press gesture over an
/// element whose resolved `ripple_active` is true, expanded and faded over
/// `duration_ms`, and retired once fully faded. The snapshot model mirrors the
/// RFC's `RippleInstance` reference: colour/duration/radius are resolved once
/// at the spawn (the ink is a launched entity, a later style change doesn't
/// recolour ink already in flight), while the element rect and clip radii are
/// re-read at emission each frame so the ink tracks a moving/resizing element.
#[derive(Clone, Copy)]
struct ActiveRipple {
    /// The stable element index this ripple belongs to (the router's key).
    /// The press identity that spawned it is not kept here, the rising-edge
    /// latch ([`Interpreter::ripple_spawned`]) is the only reader of gesture
    /// identity, and a launched ripple's lifecycle is purely time-based.
    elem: u32,
    /// Tap point relative to the element rect's origin (RFC-0023 §1),
    /// relative, so the ink stays anchored if layout shifts the element.
    center_rel: [f32; 2],
    /// Ink colour, resolved at spawn.
    color: [f32; 4],
    /// Engine time at activation.
    start_ms: u32,
    /// Total expand/fade duration in ms (`ripple_duration`, default 300).
    duration_ms: f32,
    /// `ripple_radius` override; `None` auto-expands to the farthest corner.
    max_radius: Option<f32>,
}

/// Default ripple fade-out duration in ms (RFC-0023 §"Ripple properties").
const RIPPLE_DEFAULT_DURATION_MS: f32 = 300.0;

/// Default `blur_saturation` boost (RFC-0023 §"Blur properties"): the iOS
/// vibrancy look.
const BLUR_DEFAULT_SATURATION: f32 = 1.8;

/// One element's `on measure` declaration (RFC-0038): the action, and the last
/// size it was told about.
struct MeasureSlot {
    /// The lowered action, with `it` bound to the measured size. Shared the way
    /// a lifecycle effect's action is, so firing it needs no exclusive borrow of
    /// the interpreter it writes through.
    action: std::rc::Rc<std::cell::RefCell<super::events::Action>>,
    /// The last size delivered, as raw bits.
    ///
    /// **Bits, not `f32`.** Comparing the floats would make a `NaN` extent fire
    /// forever (visible, merely wasteful) and, far worse, make `-0.0` and `0.0`
    /// compare equal, so a collapsed element would silently never fire again.
    /// This is the RFC-0032 fingerprint rule, and it applies here for the same
    /// reason it applies there.
    last: Option<(u32, u32)>,
    /// The size before [`last`](Self::last), for the oscillation check below.
    prev: Option<(u32, u32)>,
    /// How many times in a row the rect has flipped back to the size it held
    /// two fires ago: the signature of a same-element feedback loop that the
    /// compiler could not see statically (RFC-0038 "no feedback loop").
    flips: u32,
    /// The last frame this slot fired on, the one-resolve-per-frame clamp.
    fired_frame: u64,
    /// Source span of the `on measure` declaration, for the runtime warning.
    span: Span,
}

/// How many alternations it takes before an `on measure` slot is reported as a
/// feedback loop (RFC-0038).
///
/// An honest resize never alternates: dragging a window edge walks the size in
/// one direction and stops. A feedback loop flips between exactly two sizes
/// forever, so a handful of flips is already conclusive, and waiting longer
/// only delays the one message that explains what the developer is seeing.
const MEASURE_FEEDBACK_FLIPS: u32 = 8;

/// A runtime performance diagnostic surfaced by the evaluator (RFC-0023
/// resolved question "multiple blurred elements overlap"). Recomputed every
/// [`render`](Interpreter::render); hosts (the dev runner) report new ones to
/// the developer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PerfWarning {
    /// Three or more backdrop-blur panes overlap in the same frame: each
    /// upper pane re-blurs the already-blurred output of the ones below
    /// (visually correct stacked frosted glass, but each pane costs a
    /// pass-split + copy + blur). `count` is the largest overlap cluster
    /// around a single pane, the pane itself plus every pane sharing area
    /// with it, a deliberately conservative stack estimate.
    OverlappingBlurs {
        /// The size of the largest overlap cluster.
        count: usize,
    },
    /// A native view called a controller the app never provided (RFC-0039).
    ///
    /// A widget that fetches something and is answered by silence looks
    /// exactly like a widget with nothing to show, which is why this is
    /// surfaced rather than dropped: the mistake is in how the app was
    /// assembled (`App::provide`), and it is the same mistake on every frame
    /// until somebody is told (INV-4).
    UnprovidedNativeCall {
        /// The controller name the view asked for.
        controller: String,
        /// The method it wanted.
        method: String,
    },
    /// A `NavStack` grew past its `max_depth` (default 10), RFC-0026 resolved
    /// question "memory pressure". Every entry below the top keeps its View
    /// subtree, signals and scroll offsets alive, so a stack this deep is
    /// almost always a navigation bug (a push loop, or a `back` that never
    /// fires) rather than a real screen hierarchy. The push is rejected, not
    /// crashed: the app keeps running on the screen it is already showing.
    DeepNavStack {
        /// The depth the stack was already at when the push was rejected.
        depth: usize,
        /// The path that could not be pushed.
        path: String,
    },
    /// An `on measure` element's rect has been alternating between two sizes
    /// for [`MEASURE_FEEDBACK_FLIPS`] frames (RFC-0038 "no feedback loop"): the
    /// handler is feeding its own size back into its own layout through a path
    /// the compiler could not see (a `var` read by an ancestor, a `let` derived
    /// from it).
    ///
    /// The loop is already bounded, one fire per element per frame, so this
    /// costs a frame's work and not a hang; the warning exists because a
    /// silently twitching layout is far harder to diagnose than a named one.
    MeasureFeedback {
        /// Source offset of the `on measure` declaration.
        span: Span,
    },
}

/// The per-frame delta between two atlas path-counter snapshots (RFC-0032 §R7).
///
/// The counters are cumulative for the life of the thread; what a reader wants
/// is "what did *this* frame do", and subtracting is cheaper and less invasive
/// than resetting a global that tests also read.
fn path_delta(
    before: byard_core::atlas::layout::path_counters::Counts,
    after: byard_core::atlas::layout::path_counters::Counts,
) -> byard_core::atlas::layout::path_counters::Counts {
    byard_core::atlas::layout::path_counters::Counts {
        clears: after.clears.saturating_sub(before.clears),
        full_computes: after.full_computes.saturating_sub(before.full_computes),
        retained_recomputes: after
            .retained_recomputes
            .saturating_sub(before.retained_recomputes),
        retained_attempts: after
            .retained_attempts
            .saturating_sub(before.retained_attempts),
        retained_rollbacks: after
            .retained_rollbacks
            .saturating_sub(before.retained_rollbacks),
        populate_calls: after.populate_calls.saturating_sub(before.populate_calls),
        populate_dirty_targets: after
            .populate_dirty_targets
            .saturating_sub(before.populate_dirty_targets),
        populate_dirty_matched: after
            .populate_dirty_matched
            .saturating_sub(before.populate_dirty_matched),
    }
}

/// The largest overlap cluster among `rects` (`[x, y, w, h]` each): the
/// maximum, over every rect, of how many rects intersect it (itself
/// included). A conservative estimate of blur-stack depth, a chain
/// `a∩b, b∩c` reports 3 around `b` even though no single point is 3 deep,
/// which errs on the side of surfacing the diagnostic. Quadratic, fine for
/// the handful of glass panes a real frame carries.
fn deepest_rect_overlap(rects: &[[f32; 4]]) -> usize {
    let intersects = |a: &[f32; 4], b: &[f32; 4]| {
        a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
    };
    rects
        .iter()
        .map(|a| rects.iter().filter(|b| intersects(a, b)).count())
        .max()
        .unwrap_or(0)
}

/// An in-flight pull-region spring (RFC-0021 pull-to-refresh): drives a
/// `ScrollView`'s [`pull_distance`](Interpreter::pull_distance) to `target`,
/// the indicator's rest height while refreshing, or `0` to retract it, advanced
/// each `render` from the engine clock like a [`SnapAnim`].
#[derive(Clone, Copy)]
struct PullAnim {
    /// The spring (`from` = pull at release, `to` = `target`).
    motion: byard_core::frame::Motion,
    /// The exact pull distance to pin on settle (`0` retracts the indicator).
    target: f32,
}

impl Interpreter {
    /// Creates an empty interpreter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The reactive context (for tests and the engine bridge).
    #[must_use]
    pub fn ctx(&self) -> &ReactiveCtx {
        &self.ctx
    }

    /// Diagnostics accumulated while evaluating.
    #[must_use]
    pub fn errors(&self) -> &[CompileError] {
        &self.errors
    }

    /// Builds the user-`View` registry (RFC-0007 §1) from a whole file's
    /// views and stores it on the interpreter, so subsequent `lower_view`/
    /// `lower_element` calls can recognize and expand user-view calls.
    /// Returns the load-time diagnostics, `IntrinsicShadowed` and any
    /// unguarded-cycle `RecursiveView` (RFC-0007 §4), which are also
    /// recorded in [`Interpreter::errors`].
    pub fn load_views(&mut self, views: &[ViewDecl]) -> Vec<CompileError> {
        let (table, mut diags) = super::views::ViewTable::build(views);
        // Static cycle detection over the call graph.
        let graph = super::views::CallGraph::build(&table);
        if let Some((view, path)) = graph.unguarded_cycle(&table) {
            diags.push(CompileError::RecursiveView {
                span: table.decl(view).span,
                path,
            });
        }
        self.view_table = table;
        self.errors.extend(diags.iter().cloned());
        diags
    }

    /// The resolved user-`View` registry (for the reload pass and tests).
    #[must_use]
    pub fn view_table(&self) -> &super::views::ViewTable {
        &self.view_table
    }

    /// Runs one tick: begins an epoch and pulls all dirty scopes.
    pub fn tick(&mut self) {
        // Anything an action raised since the last tick reaches the pool
        // before the pull, so a call placed by a tap is in flight during the
        // very frame that tap produced (RFC-0028 §5 step 2).
        self.drain_calls();
        self.drain_closure_diagnostics();
        let epoch = self.ctx.begin_tick();
        self.ctx.pull(epoch);
    }

    /// Sets the current engine time (ms since the runner's epoch) that `with`
    /// animations sample against (RFC-0010). The runner calls this once per
    /// frame, before [`render`](Self::render).
    pub fn set_now_ms(&mut self, ms: u32) {
        self.now_ms = ms;
        self.clock_set = true;
    }

    /// Wires in the channel the render thread reports applied vector-atlas
    /// upload ids through (RFC-0009 §2-C), so the dev JIT cache stops
    /// re-sending an upload once it knows the GPU actually received it. Call
    /// once, right after construction, before the first [`render`](Self::render).
    pub fn set_vector_ack_receiver(&mut self, rx: crossbeam_channel::Receiver<u64>) {
        self.vector_jit.set_ack_receiver(rx);
    }

    /// Invalidates any cached MSDF field generated from the asset at `path`, so
    /// a saved `.svg` regenerates live (RFC-0009 §3, M47). The dev runner calls
    /// this on the logic thread when the file watcher reports an SVG change; the
    /// regenerated field reuses the same atlas cell, so the consuming `View`
    /// never remounts. Returns `true` if a cached asset matched `path`.
    pub fn invalidate_vector_asset(&mut self, path: &std::path::Path) -> bool {
        self.vector_jit.invalidate_path(path)
    }

    /// Points the vector JIT at a persistent on-disk field cache (RFC-0009 §5,
    /// M52), so cold `byard dev` starts load previously generated fields instead
    /// of regenerating them. The dev runner passes `.byard/cache/vectors/`.
    pub fn set_vector_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.vector_jit.set_cache_dir(dir);
    }

    /// Whether any `with` animation was still in flight as of the last
    /// [`render`](Self::render). The runner keeps requesting frames while this
    /// is true and lets the app idle (0 frames) once every animation settles.
    #[must_use]
    pub fn has_active_animations(&self) -> bool {
        self.any_active
    }

    /// The most recently projected value of a value binding (for tests).
    #[must_use]
    pub fn binding_value(&self, s: ScopeId) -> Option<Value> {
        self.ctx.binding_value(s)
    }

    /// Allocates the next frame-target id for a value binding.
    fn next_target(&mut self) -> FrameTarget {
        let t = FrameTarget(self.next_target);
        self.next_target += 1;
        t
    }

    /// Glyph-accurate `(width, height)` of `text` at `font_size`, lazily
    /// initializing the font system on first use.
    fn measure_text(&mut self, text: &str, font_size: f32) -> (f32, f32) {
        self.measure_text_wrapped(text, font_size, None, 400, None)
    }

    /// Measures `text`, wrapping to `max_width` logical pixels when `Some`
    /// (RFC-0018 text wrap). Returns the wrapped `(width, height)`.
    fn measure_text_wrapped(
        &mut self,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
        weight: u16,
        family: Option<&str>,
    ) -> (f32, f32) {
        self.text_measurer
            .get_or_insert_with(byard_core::text::TextMeasurer::new)
            .measure_wrapped(text, font_size, max_width, weight, family)
    }

    /// The `weight:` of a text-bearing element on the CSS axis (RFC-0034).
    ///
    /// Accepts the four historical keywords and any integer `100..=900`; the
    /// keywords are aliases for `100/400/500/700`, so both spellings land on
    /// the same axis rather than on two parallel notions of weight. Absent is
    /// `400`.
    fn resolve_weight(&mut self, attrs: &[Attr]) -> u16 {
        // An explicit `weight:` always wins. Failing that, a `typo:` token
        // brings its own — a theme that calls its headline semibold means it,
        // and the token's weight was being dropped on the way through while
        // only its size survived.
        if !attrs.iter().any(|a| a.name.as_str() == "weight") {
            if let Some(w) = self.typo_weight(attrs) {
                return w;
            }
            return 400;
        }
        if let Some(kw) = Self::enum_prop(attrs, "weight") {
            return match kw {
                "thin" => 100,
                "medium" => 500,
                "bold" => 700,
                _ => 400,
            };
        }
        match self.eval_px_prop(attrs, "weight") {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some(n) => (n.round().clamp(100.0, 900.0)) as u16,
            None => 400,
        }
    }

    // ── declarations ────────────────────────────────────────────────────

    /// Processes the declaration-level members of a `View` body (`var`/`let`/
    /// `fn`/`inject`/bare expression). Elements are lowered by the intrinsics
    /// layer (M10).
    pub fn eval_view_decls(&mut self, view: &ViewDecl) {
        for member in &view.body {
            self.eval_member(member);
        }
    }

    fn eval_member(&mut self, member: &Member) {
        match member {
            Member::Var { name, init, .. } => {
                self.define_var(name.clone(), init);
            }
            Member::Let { name, init, .. } => {
                // `let x = style { … }` / `let x = a merge b` (RFC-0016) register
                // a style value in the view-scoped table rather than a reactive
                // memo; a `..x` spread splices its attributes at lower time.
                if matches!(init, Expr::StyleValue { .. } | Expr::Merge { .. }) {
                    match self.resolve_style_expr(init) {
                        Some(def) => {
                            self.styles.insert(name.clone(), def);
                        }
                        None => self
                            .errors
                            .push(CompileError::NotAStyle { span: init.span() }),
                    }
                } else {
                    self.define_let(name.clone(), init);
                }
            }
            Member::Fn {
                name, params, body, ..
            } => {
                if params.is_empty() {
                    // No-param fn: lower body to a memo (existing behavior).
                    self.define_let(name.clone(), body);
                } else {
                    // Parameterized fn (M25): store params+body in fn_table,
                    // bind Value::Fn(AstId) in env.
                    let id = crate::interp::env::AstId(
                        u32::try_from(self.fn_table.len()).unwrap_or(u32::MAX),
                    );
                    let param_names: Vec<Symbol> = params.iter().map(|p| p.name.clone()).collect();
                    self.fn_table.push((param_names, body.clone(), false));
                    self.env.push(name.clone(), Value::Fn(id));
                }
            }
            Member::Expr(e) => {
                // A bare statement member is an action, so a call written
                // there is in effect position like any other (RFC-0028 §4).
                let result = self.in_action_position(|s| s.eval_action(e));
                if let Err(err) = result {
                    self.errors.push(err);
                }
            }
            Member::Inject { ty, name, span } => {
                // Resolve `inject T as name` from the ambient environment chain (M23).
                let ty_name = match ty {
                    crate::parser::ast::Type::Named { name: n, .. } => n.clone(),
                    crate::parser::ast::Type::Function { .. } => Symbol::intern("?"),
                };
                match self.env.resolve_inject(&ty_name).cloned() {
                    Some(val) => self.env.push(name.clone(), val),
                    // Nothing provides it. Which of the two things that means
                    // depends on whether anyone *could* have: a host with a
                    // registry knows its whole controller set, so an
                    // unresolved name there is wrong. A headless check has no
                    // registry and no way to read the app's Rust half, so the
                    // same name there is merely unknown, and the binding
                    // becomes an unbound handle so the calls on it still
                    // parse, lower and get checked.
                    None if self.has_dispatcher() => {
                        self.errors.push(CompileError::UnresolvedInject {
                            span: *span,
                            name: ty_name.as_str().to_string(),
                        });
                    }
                    None => {
                        self.errors.push(CompileError::UncheckableInject {
                            span: *span,
                            name: ty_name.as_str().to_string(),
                        });
                        self.env
                            .push(name.clone(), Value::Controller(UNBOUND_CONTROLLER));
                    }
                }
            }
            // elements / control flow / style handled in lower_members.
            _ => {}
        }
    }

    /// `var x = init`, evaluate `init` once, create a reactive source from it.
    pub fn define_var(&mut self, name: Symbol, init: &Expr) -> SignalId {
        let initial = self.eval_pure(init);
        let sig = self.ctx.create_signal(initial);
        self.env.push(name.clone(), Value::Signal(sig));
        self.var_sigs.insert(name, sig);
        sig
    }

    /// The `Signal` backing the `var` named `name`, if any.
    #[must_use]
    pub fn var_signal(&self, name: &Symbol) -> Option<SignalId> {
        self.var_sigs.get(name).copied()
    }

    /// Writes a value to a `Signal` (a controller result or test driver), running
    /// the mark cascade.
    pub fn write_var(&mut self, sig: SignalId, value: Value) {
        self.ctx.write_signal(sig, value);
    }

    /// Reads a `Signal`'s current value without tracking.
    #[must_use]
    pub fn peek(&self, sig: SignalId) -> Value {
        self.ctx.peek_signal(sig)
    }

    /// The number of nodes in the last-computed layout atlas, the direct
    /// witness that a windowed `ScrollView` lays out O(visible), not O(list)
    /// (RFC-0005 windowed layout).
    #[cfg(test)]
    #[must_use]
    fn atlas_node_count(&self) -> usize {
        self.atlas.node_count()
    }

    // ── M23: Controller boundary ─────────────────────────────────────────

    /// Provides an ambient value keyed by `ty` to this view and its
    /// descendants (`inject T as name` resolution, RFC-0002 §inject).
    /// Call before [`lower_view`](Self::lower_view) so the environment is
    /// ready when the view body is evaluated.
    pub fn inject_provider(&mut self, ty: &str, value: Value) {
        self.env.provide(Symbol::intern(ty), value);
    }

    /// Installs `theme` as the active design-token theme and provides it as the
    /// ambient `Theme` so `inject Theme as t` resolves in every view (RFC-0022).
    ///
    /// Creates the reactive scheme signal (a `Bool`, `true` ⇒ dark) seeded from
    /// the theme's active scheme, then provides a [`Value::Theme`] carrying it.
    /// Call once, before [`lower_view`](Self::lower_view). Idempotent: re-calling
    /// reuses the existing scheme signal (so a hot-reload keeps the toggle state).
    pub fn set_theme(&mut self, theme: super::theme::Theme) {
        let dark = theme.active_dark;
        self.theme = theme;
        self.register_theme_fonts();
        // A whole new token set: every resolved colour, size and typo scale
        // may differ, so the next frame rebuilds (RFC-0032 §Q6).
        self.invalidate_retained_layout();
        let sig = if let Some(sig) = self.theme_scheme {
            sig
        } else {
            let sig = self.ctx.create_signal(Value::Bool(dark));
            self.theme_scheme = Some(sig);
            sig
        };
        self.env.provide(Symbol::intern("Theme"), Value::Theme(sig));
    }

    /// Loads every family the theme declares into the measurement
    /// `FontSystem` and rebuilds the table the render thread reads
    /// (RFC-0034 §Reference "Asset side", INV-27).
    ///
    /// This is the single source of truth the invariant asks for. The bytes
    /// come from one place, the theme; they go to the measurer directly
    /// because it lives on this thread, and to the paint `FontSystem` on the
    /// frame, because it does not. Neither side ever reads a font file.
    fn register_theme_fonts(&mut self) {
        if self.theme.fonts().is_empty() {
            // Nothing declared: leave the table empty and, crucially, do not
            // force the lazy measurer into existence. A project with no fonts
            // must not start paying for a `FontSystem` because this ran.
            self.fonts = std::sync::Arc::default();
            return;
        }
        let measurer = self
            .text_measurer
            .get_or_insert_with(byard_core::text::TextMeasurer::new);
        let mut table = byard_core::frame::FontTable::default();
        for (declared, font) in self.theme.fonts() {
            let here = measurer.register_family(&font.bytes);
            // The manifest resolved this name from the same bytes when it read
            // the file. If the measurer disagrees, one side of INV-27 is
            // shaping against a family name the other never produced.
            debug_assert_eq!(
                here.as_deref(),
                Some(font.resolved.as_ref()),
                "family `{declared}` resolves differently on the measurement side (INV-27)"
            );
            table.push(byard_core::frame::FontFace {
                declared: declared.clone(),
                resolved: font.resolved.clone(),
                bytes: font.bytes.clone(),
            });
        }
        self.fonts = std::sync::Arc::new(table);
    }

    /// Flips the active color scheme (RFC-0022 §1): writes the reactive scheme
    /// signal, marking every binding that reads a theme token dirty, and
    /// mirrors the flag into the theme's non-reactive default accessors. The
    /// next [`tick`](Self::tick) recomputes; the next [`render`](Self::render)
    /// paints the new scheme. A no-op if no theme has been installed.
    ///
    /// This is the programmatic entry point (a controller, or a future OS
    /// dark-mode observer) equivalent to `theme.dark = <dark>` in `byld`.
    pub fn set_theme_dark(&mut self, dark: bool) {
        self.theme.active_dark = dark;
        // RFC-0032 §Q6: a scheme flip changes nearly every resolved value, so
        // marking would visit the whole tree and then recompute the whole tree.
        // It is also rare and user-initiated, so one rebuilt frame is
        // imperceptible, and it is the honest default for a change this wide.
        self.invalidate_retained_layout();
        if let Some(sig) = self.theme_scheme {
            self.ctx.write_signal(sig, Value::Bool(dark));
        }
    }

    /// Whether the active theme scheme is currently dark (RFC-0022 §1).
    #[must_use]
    pub fn theme_is_dark(&self) -> bool {
        self.theme_scheme.map_or(self.theme.active_dark, |sig| {
            self.ctx.peek_signal(sig).as_bool().unwrap_or(false)
        })
    }

    /// Applies a batch of ready-made logic-thread callbacks, the test and
    /// host driver for the reply path (RFC-0001 §5.1). Each callback receives
    /// a mutable reference to `self` and writes whatever `var` signals it
    /// needs via [`write_var`](Self::write_var).
    ///
    /// The real controller path is
    /// [`apply_io_results`](Self::apply_io_results), which downcasts typed
    /// replies off the relay channel; this one exists for callers that already
    /// hold the write they want performed.
    pub fn apply_io_callbacks(
        &mut self,
        results: impl IntoIterator<Item = Box<dyn FnOnce(&mut Self) + Send>>,
    ) {
        for f in results {
            f(self);
        }
    }

    /// Applies a hot-reload patch (RFC-0002 §"Hot-reload boundary", RFC-0004
    /// §11). On a [`reactive-compatible`](super::reload::ReloadKind) patch the
    /// existing `Signal`s are **kept** (matched by name) so state survives; on a
    /// structure-incompatible patch every `var` is re-initialized from the new
    /// AST (state resets). The reactive scopes are rebuilt from the new AST
    /// either way, read-tracking re-derives the dependency graph (§11).
    pub fn reload(&mut self, new_view: &ViewDecl, kind: super::reload::ReloadKind) {
        use super::reload::ReloadKind;
        // The tree is about to be re-lowered from a different AST, so nothing
        // about last frame's build order can be assumed (RFC-0032 §R4).
        self.invalidate_retained_layout();
        let old = std::mem::take(&mut self.var_sigs);
        self.env = Env::new();
        // A reload replaces the program, so every continuation now names a
        // call site that no longer exists and every effect's mount state
        // describes a tree that is gone (RFC-0028 §5, INV-14). The ambient
        // controller handles are re-provided because the environment they
        // lived in was just discarded.
        self.reset_bridge_state();
        self.provide_controllers();
        for member in &new_view.body {
            match member {
                Member::Var { name, init, .. } => {
                    if matches!(kind, ReloadKind::ReactiveCompatible) {
                        if let Some(&sig) = old.get(name) {
                            // Keep the live Signal (and its value).
                            self.env.push(name.clone(), Value::Signal(sig));
                            self.var_sigs.insert(name.clone(), sig);
                            continue;
                        }
                    }
                    self.define_var(name.clone(), init);
                }
                Member::Let { name, init, .. }
                | Member::Fn {
                    name, body: init, ..
                } => {
                    self.define_let(name.clone(), init);
                }
                _ => {}
            }
        }
    }

    /// `let y = init` (and `fn`), open a computed memo.
    pub fn define_let(&mut self, name: Symbol, init: &Expr) -> ScopeId {
        let compute = self.lower_expr(init, None);
        let scope = self.ctx.open_memo(compute);
        self.env.push(name, Value::Memo(scope));
        scope
    }

    /// Opens a value binding projecting `expr` into a fresh frame target
    /// (used by intrinsics, M10, and by tests).
    pub fn bind_value(&mut self, expr: &Expr) -> ScopeId {
        let target = self.next_target();
        let compute = self.lower_expr(expr, None);
        self.ctx.open_value_binding(target, compute)
    }

    /// Reads a memo's current value (for the engine bridge and tests). Pulls it
    /// on demand if dirty.
    pub fn read_memo(&mut self, scope: ScopeId) -> Value {
        self.ctx.read_memo(scope)
    }

    // ── argument → parameter binding (RFC-0007 §3) ──────────────────

    /// Projects one call argument into a reactive memo over the **parent** scope
    /// (the env active at the call site), so a parameter fed a parent `var` stays
    /// live (RFC-0004); a literal lowers to a constant memo with no dirty edges.
    fn project_arg(&mut self, expr: &Expr) -> ScopeId {
        let compute = self.lower_expr(expr, None);
        self.ctx.open_memo(compute)
    }

    /// Binds a single named argument (`name: value`, from `(...)` or `#[...]`) to
    /// the callee parameter of the same `Symbol`, filling `slots[i]` and emitting
    /// `UnknownParam`/`DuplicateParam` as needed (RFC-0007 §3/§6).
    fn bind_named_arg(
        &mut self,
        params: &[Param],
        callee: &str,
        name: &Symbol,
        value: &Expr,
        slots: &mut [Option<ScopeId>],
    ) {
        // The reserved `content` slot is filled by the child block, never a
        // named value.
        if name.as_str() == RESERVED_CONTENT {
            return;
        }
        match params.iter().position(|p| &p.name == name) {
            // A callback prop is bound separately (RFC-0019): it captures the
            // caller's action block, not a projected value, so leave its slot
            // empty here and let `bind_callbacks` handle it.
            Some(i) if is_callback_param(&params[i]) => {}
            Some(i) if slots[i].is_none() => {
                slots[i] = Some(self.project_arg(value));
            }
            Some(_) => self.errors.push(CompileError::DuplicateParam {
                span: value.span(),
                name: name.as_str().to_string(),
                callee: callee.to_string(),
            }),
            None => {
                let hint = closest_match(name.as_str(), params.iter().map(|p| p.name.as_str()))
                    .map(str::to_string);
                self.errors.push(CompileError::UnknownParam {
                    span: value.span(),
                    name: name.as_str().to_string(),
                    callee: callee.to_string(),
                    hint,
                });
            }
        }
    }

    /// Binds a user-view call's positional `content` and named `content`/`attrs`
    /// arguments to the callee's declared parameters, producing one reactive memo
    /// per bound parameter (RFC-0007 §3) and the §6 diagnostics
    /// (`ViewArityMismatch`/`UnknownParam`/`MissingParam`/`DuplicateParam`).
    ///
    /// Positional arguments (unnamed `(...)` entries) match by declaration order;
    /// named arguments (`name:` in `(...)` or `#[name: value]`) match by symbol.
    pub fn bind_args(&mut self, callee: &ViewDecl, call: &ElementNode) -> InstanceBindings {
        let params = &callee.params;
        let callee_name = callee.name.as_str().to_string();
        let mut slots: Vec<Option<ScopeId>> = vec![None; params.len()];
        // Positional arguments map only to *value* parameters; the reserved
        // `content` slot is filled by the child block, not a value.
        let value_param_idx: Vec<usize> = params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.name.as_str() != RESERVED_CONTENT && !is_callback_param(p))
            .map(|(i, _)| i)
            .collect();
        let mut positional_count = 0usize;
        let mut next_positional = 0usize;

        // 1) `(...)` content: unnamed → positional by order; named → by symbol.
        for arg in &call.content {
            if let Some(name) = &arg.name {
                self.bind_named_arg(params, &callee_name, name, &arg.value, &mut slots);
            } else {
                positional_count += 1;
                if let Some(&i) = value_param_idx.get(next_positional) {
                    next_positional += 1;
                    let scope = self.project_arg(&arg.value);
                    if slots[i].is_some() {
                        self.errors.push(CompileError::DuplicateParam {
                            span: arg.value.span(),
                            name: params[i].name.as_str().to_string(),
                            callee: callee_name.clone(),
                        });
                    } else {
                        slots[i] = Some(scope);
                    }
                }
                // Excess positional args are reported once via the arity check
                // below.
            }
        }

        // 2) `#[name: value]` attrs: named arguments (events are not parameters).
        for attr in &call.attrs {
            if let AttrKind::Prop { value } = &attr.kind {
                self.bind_named_arg(params, &callee_name, &attr.name, value, &mut slots);
            }
        }

        // 3) Arity: more positional args than the callee declares value
        //    parameters (RFC-0007 §6).
        if positional_count > value_param_idx.len() {
            self.errors.push(CompileError::ViewArityMismatch {
                span: call.span,
                name: callee_name.clone(),
                expected: value_param_idx.len(),
                found: positional_count,
            });
        }

        // 4) Missing required parameters: an unbound parameter with no default
        //   . The reserved `content` slot is never required, it
        //    defaults to an empty block.
        for (i, slot) in slots.iter().enumerate() {
            if slot.is_none()
                && param_default(&params[i]).is_none()
                && params[i].name.as_str() != RESERVED_CONTENT
                // Callback params are checked for presence in `bind_callbacks`.
                && !is_callback_param(&params[i])
            {
                self.errors.push(CompileError::MissingParam {
                    span: call.span,
                    name: params[i].name.as_str().to_string(),
                    callee: callee_name.clone(),
                });
            }
        }

        InstanceBindings {
            bindings: slots
                .into_iter()
                .enumerate()
                .filter_map(|(i, s)| s.map(|sc| (params[i].name.clone(), sc)))
                .collect(),
        }
    }

    // ── callback props (RFC-0019) ───────────────────────────────────────

    /// Registers a caller-supplied callback body in the shared `fn_table`,
    /// returning its [`AstId`]. The body is the caller's action block; it is
    /// lowered later, at the callee's invocation site, against the shared flat
    /// env, which still holds the caller's `var` bindings below the callee
    /// frame, so writes route to the caller's signals (RFC-0019 §2/§3).
    fn register_callback(&mut self, params: &[Symbol], body: &Expr) -> super::env::AstId {
        let id = super::env::AstId(u32::try_from(self.fn_table.len()).unwrap_or(u32::MAX));
        self.fn_table.push((params.to_vec(), body.clone(), true));
        id
    }

    /// Registers an arity-matched no-op callback (an empty action block with
    /// `arity` ignored parameters), used when a bare-identifier forward cannot
    /// be resolved to a live callback in the current lowering context. Matching
    /// the declared arity keeps the invocation-site arity check (§4) quiet.
    fn noop_callback(&mut self, arity: usize, span: Span) -> super::env::AstId {
        let params: Vec<Symbol> = (0..arity)
            .map(|i| Symbol::intern(&format!("__cb_arg{i}")))
            .collect();
        self.register_callback(&params, &Expr::Block(Vec::new(), span))
    }

    /// The caller-supplied argument expression for a named parameter, a `name:`
    /// entry in the `(...)` content or a `#[name: value]` attribute. Callback
    /// props are always passed by name.
    fn find_named_arg<'a>(&self, call: &'a ElementNode, name: &Symbol) -> Option<&'a Expr> {
        call.content
            .iter()
            .find(|a| a.name.as_ref() == Some(name))
            .map(|a| &a.value)
            .or_else(|| {
                call.attrs.iter().find_map(|attr| match &attr.kind {
                    AttrKind::Prop { value } if &attr.name == name => Some(value),
                    _ => None,
                })
            })
    }

    /// Binds a callback-prop parameter (RFC-0019): pushes a `Value::Fn` naming
    /// the caller's action block (or the `= { … }` default, or a forwarded
    /// callback already in scope). Emits the §4 diagnostics, arity mismatch
    /// between the `Fn(...)` type and the block's `|params|`, a non-callback
    /// argument, or a missing required callback.
    fn bind_callback_param(&mut self, param: &Param, call: &ElementNode) {
        let arg_ty_count = match &param.ty {
            Some(Type::Function { params, .. }) => params.len(),
            _ => 0,
        };
        if let Some(arg) = self.find_named_arg(call, &param.name) {
            match arg {
                Expr::Lambda {
                    params, body, span, ..
                } => {
                    if params.len() != arg_ty_count {
                        self.errors.push(CompileError::CallbackArityMismatch {
                            span: *span,
                            name: param.name.as_str().to_string(),
                            expected: arg_ty_count,
                            found: params.len(),
                        });
                    }
                    let id = self.register_callback(params, body);
                    self.env.push(param.name.clone(), Value::Fn(id));
                }
                // Forwarding: `on_tap: outer_on_tap` re-binds a callback already
                // in scope (a wrapper forwarding its own callback prop inward).
                // A bare identifier that does *not* currently resolve to a
                // callback is bound to an arity-matched no-op rather than a hard
                // type error, a wrapper checked in isolation has its own
                // callback params unbound, and that must not false-positive.
                Expr::Ident(other, span) => {
                    if let Some(&Value::Fn(id)) = self.env.lookup(other) {
                        self.env.push(param.name.clone(), Value::Fn(id));
                    } else {
                        let id = self.noop_callback(arg_ty_count, *span);
                        self.env.push(param.name.clone(), Value::Fn(id));
                    }
                }
                other => self.errors.push(CompileError::CallbackTypeMismatch {
                    span: other.span(),
                    callee: call.name.as_str().to_string(),
                    name: param.name.as_str().to_string(),
                }),
            }
        } else if let Some(Expr::Lambda { params, body, .. }) = param_default(param) {
            // The default is an action block (`= {}` / `= {|_|}`); register it.
            let id = self.register_callback(params, body);
            self.env.push(param.name.clone(), Value::Fn(id));
        } else {
            // A required callback with no default and no argument.
            self.errors.push(CompileError::MissingParam {
                span: call.span,
                name: param.name.as_str().to_string(),
                callee: call.name.as_str().to_string(),
            });
        }
    }

    // ── element lowering (RFC-0005) ─────────────────────────────────────

    /// Resolves the `bind:` or `value:` attribute of a value widget to a
    /// `SignalId`. Returns `None` if no such attribute exists or it doesn't
    /// name a `var` (M16).
    fn resolve_bind_sig(&self, attrs: &[Attr]) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if matches!(attr.name.as_str(), "bind" | "value") {
                match &attr.kind {
                    AttrKind::Prop {
                        value: Expr::Ident(name, _),
                    } => {
                        if let Some(super::env::Value::Signal(sig)) = self.env.lookup(name) {
                            return Some(*sig);
                        }
                    }
                    // `bind: theme.dark` binds a toggle straight to the reactive
                    // scheme flag (RFC-0022 §1), tapping it recolors the tree.
                    AttrKind::Prop { value } => {
                        if let Some(sig) = self.resolve_theme_scheme_target(value) {
                            return Some(sig);
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// Resolves *only* the `bind:` attribute of a `RadioButton` (RFC-0018) to the
    /// group `var`'s `SignalId`. Unlike [`resolve_bind_sig`], it never inspects
    /// `value:`, for a radio, `value` is the button's literal identity string,
    /// not a signal binding. Returns `None` if `bind:` is absent or doesn't name
    /// a `var`.
    fn resolve_group_bind_sig(&self, attrs: &[Attr]) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if attr.name.as_str() == "bind" {
                if let AttrKind::Prop {
                    value: Expr::Ident(name, _),
                } = &attr.kind
                {
                    if let Some(super::env::Value::Signal(sig)) = self.env.lookup(name) {
                        return Some(*sig);
                    }
                }
            }
        }
        None
    }

    /// Resolves the attribute `name`'s value to a writable `var`'s `SignalId`
    /// when it is a bare identifier bound to a `var` (else `None`). Backs
    /// RFC-0021's reflected `page:` prop; a small generalization of
    /// [`resolve_group_bind_sig`](Self::resolve_group_bind_sig).
    fn resolve_named_var_sig(&self, attrs: &[Attr], name: &str) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if attr.name.as_str() == name {
                if let AttrKind::Prop {
                    value: Expr::Ident(n, _),
                } = &attr.kind
                {
                    if let Some(super::env::Value::Signal(sig)) = self.env.lookup(n) {
                        return Some(*sig);
                    }
                }
            }
        }
        None
    }

    /// The prop/value-driven style states an element contributes (RFC-0024), on
    /// top of the router's pointer/focus/drag states: `checked` (a value-widget's
    /// bound value is true), `selected` (the `selected:` prop, or a `RadioButton`
    /// whose `bind == value`), `invalid` (the `invalid:` prop), and
    /// `indeterminate` (a `Checkbox`'s mixed prop). `checked` and `indeterminate`
    /// are mutually exclusive, `indeterminate` clears `checked` (RFC-0024).
    fn prop_style_state(
        &mut self,
        attrs: &[Attr],
        bound_sig: Option<super::env::SignalId>,
        name: &str,
    ) -> crate::interp::events::StyleState {
        use crate::interp::events::StyleState;
        let mut s = StyleState::empty();
        if self.eval_bool_prop(attrs, "selected") == Some(true) {
            s = s.union(StyleState::SELECTED);
        }
        if self.eval_bool_prop(attrs, "invalid") == Some(true) {
            s = s.union(StyleState::INVALID);
        }
        let indeterminate =
            name == "Checkbox" && self.eval_bool_prop(attrs, "indeterminate") == Some(true);
        if indeterminate {
            s = s.union(StyleState::INDETERMINATE);
        }
        // `checked` from a value-widget's bound bool, suppressed while mixed.
        if matches!(name, "Checkbox" | "Toggle") && !indeterminate {
            let checked =
                bound_sig.is_some_and(|sig| self.ctx.peek_signal(sig).as_bool().unwrap_or(false));
            if checked {
                s = s.union(StyleState::CHECKED);
            }
        }
        // `selected` from a `RadioButton` whose group var equals its value.
        if name == "RadioButton" {
            let value = self.eval_str_prop(attrs, "value").unwrap_or_default();
            let selected = bound_sig.is_some_and(
                |sig| matches!(self.ctx.peek_signal(sig), Value::Str(v) if v == value),
            );
            if selected {
                s = s.union(StyleState::SELECTED);
            }
        }
        s
    }

    /// The signals backing a `ScrollView`'s `offset.x` and `offset.y` (RFC-0005),
    /// each present when that tuple component is a writable `var`, e.g.
    /// `offset: (panX, scrollY)` yields both, `offset: (0, scrollY)` only the y.
    /// A component that is a literal or computed value yields `None` (that axis
    /// is inert to wheel/drag; the app drives it). Returned as `(x, y)`.
    fn resolve_offset_sigs(
        &self,
        attrs: &[Attr],
    ) -> (Option<super::env::SignalId>, Option<super::env::SignalId>) {
        use crate::parser::ast::Expr;
        let Some(value) = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "offset" => Some(value),
            _ => None,
        }) else {
            return (None, None);
        };
        // `offset: (x, y)`, a component is scrollable iff it names a `var`.
        let sig_at = |i: usize| -> Option<super::env::SignalId> {
            let Expr::Tuple(args, _) = value else {
                return None;
            };
            let Some(Expr::Ident(name, _)) = args.get(i).map(|a| &a.value) else {
                return None;
            };
            match self.env.lookup(name) {
                Some(super::env::Value::Signal(sig)) => Some(*sig),
                _ => None,
            }
        };
        (sig_at(0), sig_at(1))
    }

    /// The visible row window of a windowed `ScrollView` (RFC-0005), or `None`
    /// when it is not `windowed`, its `row_height` is unset/≤ 0, or it has no
    /// uniform list child. The window brackets the viewport with a couple of
    /// overscan rows so a partially-scrolled row is always materialised. Computed
    /// from the *current* `offset.y`, and, because both passes read the same
    /// offset within one render, identically in build and render.
    fn scroll_window(&mut self, sv_attrs: &[Attr], child_count: usize) -> Option<WindowSpec> {
        // Overscan rows on each side keep a row that is only partly scrolled into
        // view fully materialised, and hide the one-frame lag between an input
        // and the re-render that follows it.
        const OVERSCAN: usize = 2;
        if self.eval_bool_prop(sv_attrs, "windowed") != Some(true) {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let row_height = self.eval_px_prop(sv_attrs, "row_height").unwrap_or(0.0);
        if row_height <= 0.0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let viewport_h = self.eval_px_prop(sv_attrs, "height").unwrap_or(0.0);
        let (_, offset_y) = self.resolve_axis_pair(sv_attrs, "offset", (0.0, 0.0));

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first = (offset_y / row_height).floor().max(0.0) as usize;
        let start = first.saturating_sub(OVERSCAN);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let span = (viewport_h / row_height).ceil() as usize + 2 * OVERSCAN + 1;
        let end = start.saturating_add(span).min(child_count);
        Some(WindowSpec {
            start,
            end,
            row_height,
            n: child_count,
        })
    }

    /// Whether a `ScrollView` child's laid-out rectangle, mapped through the
    /// scroll-shifted `transform`, falls entirely outside `clip`, the emission-
    /// culling test (RFC-0005 §3.3). All four corners are transformed so a scaled
    /// ancestor is handled; an unknown rect is conservatively kept (never culled).
    fn child_fully_clipped(
        &self,
        child_id: byard_core::atlas::layout::AtlasNodeId,
        transform: byard_core::frame::Transform,
        clip: byard_core::frame::Rect,
    ) -> bool {
        let Ok(Some(r)) = self.atlas.resolved_rect(child_id) else {
            return false;
        };
        let corners = [
            transform.apply_point([r.x, r.y]),
            transform.apply_point([r.x + r.width, r.y]),
            transform.apply_point([r.x, r.y + r.height]),
            transform.apply_point([r.x + r.width, r.y + r.height]),
        ];
        let min_x = corners.iter().map(|c| c[0]).fold(f32::INFINITY, f32::min);
        let max_x = corners
            .iter()
            .map(|c| c[0])
            .fold(f32::NEG_INFINITY, f32::max);
        let min_y = corners.iter().map(|c| c[1]).fold(f32::INFINITY, f32::min);
        let max_y = corners
            .iter()
            .map(|c| c[1])
            .fold(f32::NEG_INFINITY, f32::max);
        max_x <= clip.x
            || min_x >= clip.x + clip.width
            || max_y <= clip.y
            || min_y >= clip.y + clip.height
    }

    /// Lowers an element to a [`RenderNode`], validating it against the §5
    /// attribute contract first (diagnostics accumulate in [`Interpreter::errors`]).
    /// `known_views` are user `ViewDecl` names in scope.
    pub fn lower_element(&mut self, el: &ElementNode, known_views: &[&str]) -> RenderNode {
        // RFC-0016: expand `..style` spreads into a flat attribute set *before*
        // validating or lowering, so everything downstream sees ordinary
        // resolved attributes (and a spread can never leak into the checker).
        let (attrs, state_blocks) = self.expand_style_spreads(&el.attrs);
        // Validate the base *and* every state block's attributes against the
        // intrinsic's contract (an `on hover { bg: … }` must obey the same §5
        // rules as an inline `bg:`); the state attrs are validation-only and do
        // not affect the emitted base set.
        let to_validate = attrs_with_states(&attrs, &state_blocks);
        self.errors
            .extend(validate_element(el, &to_validate, known_views));
        // RFC-0038: an `on measure` member is *about* this element, so it is
        // consumed here and never reaches the child list. `el_children` is the
        // element's members minus those declarations, borrowed unchanged in the
        // overwhelming case where there are none.
        let (measure, el_children) = self.lower_measures(el, &attrs);
        let el_children: &[Member] = &el_children;
        match el.name.as_str() {
            "Text" | "Button" if !el.content.is_empty() => {
                let content = self.bind_value(&el.content[0].value);
                if el.name.as_str() == "Button" {
                    // A Button is a decorated box wrapping its label.
                    RenderNode::Box {
                        name: Symbol::intern("Button"),
                        attrs,
                        state_blocks,
                        anchor_name: el.anchor_name.clone(),
                        children: vec![RenderNode::Text {
                            attrs: Vec::new(),
                            state_blocks: Vec::new(),
                            content,
                        }],
                        action: el.action.clone(),
                        bound_sig: None,
                        env_snapshot: self.capture_env_snapshot(),
                        measure,
                    }
                } else {
                    RenderNode::Text {
                        attrs,
                        state_blocks,
                        content,
                    }
                }
            }
            // RFC-0026: a navigation container. Its children are `route`/`tab`
            // cases, not elements, so it never takes the generic container path
            //, it compiles its route table into a pool here and lowers each
            // screen lazily, the first time navigation reaches it.
            "NavStack" | "NavHost" => self.lower_nav(el, &attrs, state_blocks, known_views),
            "Spacer" => RenderNode::Spacer { attrs },
            // Image intrinsic → TextureSampler pipeline (M21).
            // Syntax: Image("path.jpg") #[fit: .cover, width: 200, height: 150]
            "Image" => {
                let src_expr = el.content.first().map_or_else(
                    || Expr::StrLit(vec![], crate::diagnostics::Span::new(0, 0)),
                    |c| c.value.clone(),
                );
                let src = self.bind_value(&src_expr);
                RenderNode::Image {
                    attrs,
                    state_blocks,
                    src,
                }
            }
            // VectorIcon intrinsic → VectorMSDF pipeline. Content is an
            // asset handle (a `Str` path), like Image's source.
            "VectorIcon" => {
                let src_expr = el.content.first().map_or_else(
                    || Expr::StrLit(vec![], crate::diagnostics::Span::new(0, 0)),
                    |c| c.value.clone(),
                );
                let src = self.bind_value(&src_expr);
                RenderNode::Vector {
                    attrs: el.attrs.clone(),
                    src,
                }
            }
            // RFC-0020: the `Canvas` drawing surface. Its children are shape
            // commands, validated here (never silently ignored) and carried as
            // AST elements, the render walk re-evaluates their parameter
            // expressions every tick, which is what makes them reactive.
            "Canvas" => {
                self.errors
                    .extend(super::intrinsics::validate_canvas(el, &to_validate));
                let shapes = lower_canvas_items(el_children);
                RenderNode::Canvas {
                    attrs,
                    state_blocks,
                    shapes,
                    action: el.action.clone(),
                    env_snapshot: self.capture_env_snapshot(),
                }
            }
            // RFC-0017: the `Overlay` escape-hatch. Its children are lowered
            // normally, but the node itself carries them out of the parent flow
            //, the render walk defers them to the overlay layer.
            "Overlay" => {
                let children = self.lower_members(el_children, known_views);
                RenderNode::Overlay {
                    attrs,
                    children,
                    env_snapshot: self.capture_env_snapshot(),
                }
            }
            // Value widgets: resolve bound signal and keep as leaf nodes (M16/M19).
            // `Checkbox` (RFC-0018) joins them: a `bind: Bool` leaf that owns its
            // square-plus-checkmark visual and flips on tap/Space.
            "Toggle" | "Slider" | "TextField" | "Checkbox" => {
                let bound_sig = self.resolve_bind_sig(&attrs);
                RenderNode::Box {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    anchor_name: el.anchor_name.clone(),
                    children: Vec::new(),
                    action: el.action.clone(),
                    bound_sig,
                    env_snapshot: self.capture_env_snapshot(),
                    measure,
                }
            }
            // RFC-0018 `RadioButton`: like the value widgets, but its `value:` is a
            // literal identity (a `Str`), not a bound signal, only `bind:` names
            // the shared group `var`. Resolve *just* `bind` to the group signal;
            // `value` is read from the attrs at render time.
            "RadioButton" => {
                let bound_sig = self.resolve_group_bind_sig(&attrs);
                RenderNode::Box {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    anchor_name: el.anchor_name.clone(),
                    children: Vec::new(),
                    action: el.action.clone(),
                    bound_sig,
                    env_snapshot: self.capture_env_snapshot(),
                    measure,
                }
            }
            // RFC-0039: a package's native view. Reached here because the
            // catalog resolved the name (`validate_element` has already checked
            // its props and events against the entry the package declared), so
            // by this point it is an element like any other, with a Rust
            // implementation behind it instead of an intrinsic.
            name if byard_core::render::registry::info(name).is_some() => {
                let slot = self.mount_native_view(name, el.span);
                RenderNode::Native {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    action: el.action.clone(),
                    env_snapshot: self.capture_env_snapshot(),
                    slot,
                }
            }
            _ => {
                // Box / Column / Row / ScrollView and any other container.
                // RFC-0021 collapsing header: a `collapse_header` ScrollView mints
                // a `scroll_fraction` signal (0 = expanded, 1 = collapsed) scoped to
                // its *first* child (the header), the header's descendants read it
                // to interpolate their own size/opacity. The signal is carried on
                // `bound_sig` (unused by a ScrollView) so `render` can drive it.
                let collapse = el.name.as_str() == "ScrollView"
                    && Self::enum_prop(&attrs, "collapse_header") == Some("true");
                let (children, bound_sig) = if collapse && !el_children.is_empty() {
                    let frac = self.ctx.create_signal(Value::Float(0.0));
                    let snap = self.env.len();
                    self.env
                        .push(Symbol::intern("scroll_fraction"), Value::Signal(frac));
                    let mut kids = self.lower_members(&el_children[..1], known_views);
                    self.env.truncate(snap);
                    kids.extend(self.lower_members(&el_children[1..], known_views));
                    (kids, Some(frac))
                } else {
                    (self.lower_members(el_children, known_views), None)
                };
                RenderNode::Box {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    anchor_name: el.anchor_name.clone(),
                    children,
                    action: el.action.clone(),
                    bound_sig,
                    env_snapshot: self.capture_env_snapshot(),
                    measure,
                }
            }
        }
    }

    /// Captures the instance environment for a box being lowered (RFC-0019 §2),
    /// or an empty snapshot at the top level. Only boxes lowered *inside* a
    /// user-view instance need one, a top-level box's event actions re-lower
    /// against the persistent root env, exactly as before, so its snapshot stays
    /// empty and render behaviour is unchanged.
    fn capture_env_snapshot(&self) -> Vec<(Symbol, Value)> {
        if self.instance_depth == 0
            && self.for_depth == 0
            && self.nav_depth == 0
            && self.restored_depth == 0
        {
            Vec::new()
        } else {
            self.env.snapshot()
        }
    }

    /// Whether `el` is a user-`View` call: a name that resolves in the view
    /// table and is not an RFC-0005 intrinsic, which always wins.
    fn is_user_view_call(&self, el: &ElementNode) -> bool {
        super::intrinsics::lookup(el.name.as_str()).is_none() && self.view_table.contains(&el.name)
    }

    /// Expands a user-`View` call site into its instantiated subtree, spliced as
    /// siblings at `out` (RFC-0007 §2). Opens a fresh instance scope holding the
    /// argument bindings plus the callee's own local `var`/`let`/`fn`
    /// (isolated per instance), lowers the callee body, then truncates the scope
    /// so the parent environment is untouched.
    fn lower_user_view_call(
        &mut self,
        el: &ElementNode,
        known_views: &[&str],
        out: &mut Vec<RenderNode>,
    ) {
        // A user view is not validated by the intrinsic contract; its argument
        // diagnostics come from `bind_args` (RFC-0007 §3/§6).
        let Some(id) = self.view_table.resolve(&el.name) else {
            // Unreachable in practice (caller checked), but degrade gracefully.
            out.push(self.lower_element(el, known_views));
            return;
        };
        // Own the callee so the `&self.view_table` borrow does not conflict with
        // the `&mut self` lowering below (the table is `Send`/owned, INV-3).
        let callee = self.view_table.decl(id).clone();

        // Runtime depth bound (RFC-0007 §4): a guarded recursion whose
        // guard never terminates at lower time is truncated with a diagnostic
        // rather than overflowing the native stack.
        if self.instance_depth >= MAX_INSTANCE_DEPTH {
            self.errors.push(CompileError::RecursiveView {
                span: el.span,
                path: format!(
                    "{} (instantiation depth bound {MAX_INSTANCE_DEPTH} exceeded)",
                    el.name.as_str()
                ),
            });
            return; // truncate the subtree; never recurse past the bound
        }
        self.instance_depth += 1;

        // Slot (RFC-0007 D-A): a `{ ... }` block is allowed only when
        // the callee declares a `content` parameter; the block is pre-lowered in
        // the *caller* scope (capturing caller `var`s) and pushed as this
        // instance's slot. A block passed to a slot-less callee is
        // `UnexpectedChildren`.
        let has_content_param = callee
            .params
            .iter()
            .any(|p| p.name.as_str() == RESERVED_CONTENT);
        let slot_nodes = if el.children.is_empty() {
            Vec::new()
        } else if has_content_param {
            self.lower_members(&el.children, known_views)
        } else {
            self.errors.push(CompileError::UnexpectedChildren {
                span: el.span,
                name: el.name.as_str().to_string(),
            });
            Vec::new()
        };

        // 1) Bind arguments → parameters in the *parent* scope (RFC-0007 §3).
        let bindings = self.bind_args(&callee, el);

        // 2) Open the per-instance lexical frame (D-D): push each
        //    parameter in declaration order, a bound argument's memo, else a
        //    default evaluated in the callee scope, then the callee's
        //    own declarations.
        let snapshot = self.env.len();
        for param in &callee.params {
            // A callback prop (RFC-0019) binds the caller's action block as a
            // `Value::Fn`, resolved at invocation in `lower_call`, rather than a
            // projected value memo.
            if is_callback_param(param) {
                self.bind_callback_param(param, el);
                continue;
            }
            if let Some((_, scope)) = bindings.bindings.iter().find(|(n, _)| n == &param.name) {
                self.env.push(param.name.clone(), Value::Memo(*scope));
            } else if let Some(default) = param_default(param) {
                // Lowered in the current (callee) frame, so a default may
                // reference earlier parameters.
                let scope = self.project_arg(default);
                self.env.push(param.name.clone(), Value::Memo(scope));
            }
            // An unbound, defaultless parameter already produced a `MissingParam`
            // diagnostic in `bind_args`; leave it unbound.
        }
        // Local `var`/`let`/`fn`/`inject` open in the instance frame, so two
        // instances of the same view keep independent state.
        self.eval_view_decls(&callee);

        // 3) Lower the callee body and splice the roots as siblings (RFC-0007
        //    §2 step 5; reuses the multi-node `when`/`for` splice shape). The
        //    slot is live for `content` references within the body.
        self.slot_stack.push(slot_nodes);
        let nodes = self.lower_members(&callee.body, known_views);
        self.slot_stack.pop();
        out.extend(nodes);

        // 4) Close the instance scope (RFC-0007 §2 step 6).
        self.env.truncate(snapshot);
        self.instance_depth -= 1;
    }

    /// Expands `..style` spreads (RFC-0016) in an attribute list into a flat
    /// set: each spread splices the referenced style's attributes in written
    /// order (a later spread overrides an earlier one), then inline attributes
    /// override every spread. The common, spread-free case returns a plain
    /// clone with no work. A spread that doesn't resolve to a known style is a
    /// [`CompileError::NotAStyle`].
    fn expand_style_spreads(&mut self, attrs: &[Attr]) -> (Vec<Attr>, Vec<StateBlock>) {
        if !attrs
            .iter()
            .any(|a| matches!(a.kind, AttrKind::Spread { .. }))
        {
            return (attrs.to_vec(), Vec::new());
        }
        let mut resolved: Vec<Attr> = Vec::new();
        let mut states: Vec<StateBlock> = Vec::new();
        // 1) Spreads first, in written order. Each spread contributes its base
        //    attributes (last-write-wins) and appends its `on <state>` blocks
        //    (a later spread's block of the same state wins at resolve time).
        for a in attrs {
            if let AttrKind::Spread { value } = &a.kind {
                match self.resolve_style_expr(value) {
                    Some(def) => {
                        for sa in def.base {
                            override_attr(&mut resolved, sa);
                        }
                        states.extend(def.states);
                    }
                    None => self.errors.push(CompileError::NotAStyle { span: a.span }),
                }
            }
        }
        // 2) Inline attributes win over the spreads.
        for a in attrs {
            if !matches!(a.kind, AttrKind::Spread { .. }) {
                override_attr(&mut resolved, a.clone());
            }
        }
        (resolved, states)
    }

    /// Resolves a style expression to a [`StyleDef`] (base attributes + state
    /// blocks): a `let`-bound style name, an inline `style { … }` value, or a
    /// `merge` of two styles.
    fn resolve_style_expr(&self, value: &Expr) -> Option<StyleDef> {
        match value {
            Expr::Ident(name, _) => self.styles.get(name).cloned(),
            Expr::StyleValue { attrs, states, .. } => Some(StyleDef {
                base: attrs.clone(),
                states: states.clone(),
            }),
            // `a merge b` (RFC-0016): the right style overrides the left, its
            // base attributes overlay last-write-wins, and its state blocks are
            // appended so a later block of the same state wins at resolve time.
            Expr::Merge { left, right, .. } => {
                let mut def = self.resolve_style_expr(left)?;
                let over = self.resolve_style_expr(right)?;
                for a in over.base {
                    override_attr(&mut def.base, a);
                }
                def.states.extend(over.states);
                Some(def)
            }
            _ => None,
        }
    }

    /// Lowers a slice of `Member`s into child `RenderNode`s, handling
    /// `Element`, `When`, and `For` (M20).
    fn lower_members(&mut self, members: &[Member], known_views: &[&str]) -> Vec<RenderNode> {
        let mut nodes = Vec::new();
        for m in members {
            self.lower_member_into(m, known_views, &mut nodes);
        }
        nodes
    }

    fn lower_member_into(
        &mut self,
        member: &Member,
        known_views: &[&str],
        out: &mut Vec<RenderNode>,
    ) {
        match member {
            // A `content` reference inside a user-view body splices the slot the
            // current instance was called with (RFC-0007 D-A). The slot
            // nodes were pre-lowered in the caller scope.
            Member::Element(e)
                if e.name.as_str() == RESERVED_CONTENT && !self.slot_stack.is_empty() =>
            {
                if let Some(slot) = self.slot_stack.last() {
                    out.extend(slot.clone());
                }
            }
            Member::Element(e) if self.is_user_view_call(e) => {
                // A user-view call expands into its instantiated subtree, spliced
                // as siblings here (RFC-0007 §2).
                self.lower_user_view_call(e, known_views, out);
            }
            Member::Element(e) => {
                out.push(self.lower_element(e, known_views));
            }
            // RFC-0028 §4b: a lifecycle effect. Lowered here, in the scope it
            // was written in, and emitted as a node so its position in the
            // tree is what decides when it mounts.
            Member::Lifecycle {
                on_mount, action, ..
            } => {
                if let Ok(lowered) = self.lower_action(action, None) {
                    let kind = if *on_mount {
                        controller::EffectKind::Mount
                    } else {
                        controller::EffectKind::Unmount
                    };
                    let index = self.register_effect(kind, lowered);
                    out.push(RenderNode::Effect { index });
                }
            }
            // RFC-0029 §5: a timer effect. Same node, same mount rule and same
            // reply path as a lifecycle effect, which is the point: a timer
            // *is* a repeating mount-scoped effect.
            Member::Timer {
                every,
                dur_ms,
                action,
                ..
            } => {
                if let Ok(lowered) = self.lower_action(action, None) {
                    let index = self.register_effect(
                        controller::EffectKind::Timer {
                            every: *every,
                            dur_ms: *dur_ms,
                        },
                        lowered,
                    );
                    out.push(RenderNode::Effect { index });
                }
            }
            // RFC-0026: a `route`/`tab` case only means something as a direct
            // child of its container, the nav lowering consumes those without
            // ever coming through here, so anything reaching this arm is
            // misplaced. Diagnosed rather than dropped (INV-4).
            Member::Route { kind, span, .. } => {
                self.errors.push(CompileError::MisplacedNavCase {
                    span: *span,
                    keyword: kind.as_str().to_string(),
                    container: kind.container().to_string(),
                });
            }
            // RFC-0018 reactive `when`: bind the condition and register a branch
            // cache. Branches are lowered lazily on first selection (see
            // [`WhenPool`]) so an untaken recursive branch never lowers; the
            // driver re-reads the condition each frame and expands the taken one.
            Member::When {
                cond, then, els, ..
            } => {
                let cond_scope = self.bind_value(cond);
                let pool = self.when_pools.len();
                let env_snapshot = self.capture_env_snapshot();
                self.when_pools.push(WhenPool {
                    branch_spans: (
                        members_span(then),
                        els.as_deref().map_or(Span::new(0, 0), members_span),
                    ),
                    then_ast: then.clone(),
                    els_ast: els.clone().unwrap_or_default(),
                    known_views: known_views.iter().map(|s| (*s).to_string()).collect(),
                    env_snapshot,
                    then: None,
                    els: None,
                    last_take: None,
                });
                out.push(RenderNode::When {
                    cond: cond_scope,
                    pool,
                });
            }
            // RFC-0018 reactive `for`: bind the list as a reactive projection and
            // register a body pool. Bodies are lowered lazily per slot during
            // reconciliation (never per frame); the driver renders one pooled body
            // per current element.
            Member::For {
                var,
                index,
                iter,
                body,
                ..
            } => {
                let list_scope = self.bind_value(iter);
                let pool = self.for_pools.len();
                let env_snapshot = self.capture_env_snapshot();
                self.for_pools.push(ForPool {
                    item_var: var.clone(),
                    index_var: index.clone(),
                    body: body.clone(),
                    known_views: known_views.iter().map(|s| (*s).to_string()).collect(),
                    env_snapshot,
                    item_slots: Vec::new(),
                    bodies: Vec::new(),
                    len: 0,
                });
                out.push(RenderNode::For {
                    pool,
                    list: list_scope,
                });
            }
            _ => {}
        }
    }

    // ── Navigation (RFC-0026) ───────────────────────────────────────────────

    /// Lowers a `NavStack`/`NavHost` (RFC-0026): validates and compiles its
    /// route table into a [`NavPool`] and returns the single [`RenderNode::Nav`]
    /// that stands for the container.
    ///
    /// No screen is lowered here. A route's View subtree is instantiated the
    /// first time navigation actually reaches it and preserved from then on, so
    /// mounting a ten-route table costs ten compiled patterns, not ten View
    /// trees (RFC-0026's "lazy route loading", which falls out of the
    /// entry model rather than needing a separate mechanism).
    fn lower_nav(
        &mut self,
        el: &ElementNode,
        attrs: &[Attr],
        state_blocks: Vec<StateBlock>,
        known_views: &[&str],
    ) -> RenderNode {
        use crate::interp::nav::{NavTransition, RoutePattern};

        self.errors.extend(super::intrinsics::validate_nav(el));
        let kind = if el.name.as_str() == "NavStack" {
            NavKind::Stack
        } else {
            NavKind::Host
        };
        let want = if kind == NavKind::Stack {
            RouteKind::Route
        } else {
            RouteKind::Tab
        };

        let mut routes = Vec::new();
        for child in &el.children {
            let Member::Route {
                kind: written,
                pattern,
                params,
                body,
                pattern_span,
                ..
            } = child
            else {
                continue; // diagnosed by `validate_nav`
            };
            if *written != want {
                continue; // likewise
            }
            let Ok(compiled) = RoutePattern::compile(pattern, *pattern_span) else {
                continue;
            };
            routes.push(RouteDef {
                pattern: compiled,
                params_binding: params.clone(),
                body: body.clone(),
            });
        }

        // The navigation state: `NavStack(path: navPath)` / `NavHost(active:
        // tab)`, read as an ordinary reactive projection. A container written
        // without it (already an arity diagnostic) falls back to its first case,
        // so a malformed source still renders instead of blanking.
        let path_expr = el.content.first().map_or_else(
            || {
                let first = routes
                    .first()
                    .map_or_else(String::new, |r| r.pattern.raw.clone());
                Expr::StrLit(vec![StrPart::Text(first)], el.span)
            },
            |arg| arg.value.clone(),
        );
        // The writable side of the same value, when it is a `var` (the usual
        // case): reflected state the engine writes back for `back(…)`, a
        // completed swipe, a rejected over-deep push and a delivered deep link.
        let path_sig = match &path_expr {
            Expr::Ident(name, _) => match self.env.lookup(name) {
                Some(Value::Signal(sig)) => Some(*sig),
                _ => None,
            },
            _ => None,
        };
        let path = self.bind_value(&path_expr);

        let transition = Self::enum_prop(attrs, "transition")
            .and_then(NavTransition::from_token)
            // A stack push has a direction; a tab switch does not.
            .unwrap_or(if kind == NavKind::Stack {
                NavTransition::Slide
            } else {
                NavTransition::Fade
            });
        let swipe_back =
            kind == NavKind::Stack && self.eval_bool_prop(attrs, "swipe_back").unwrap_or(false);
        let deep_link = self.eval_bool_prop(attrs, "deep_link").unwrap_or(false);
        #[allow(clippy::cast_sign_loss)]
        let max_depth = self
            .eval_int_prop(attrs, "max_depth")
            .unwrap_or(DEFAULT_NAV_MAX_DEPTH)
            .max(0) as usize;

        let shared = match path_sig {
            Some(sig) => self.nav_shared_cell(sig),
            None => NavSharedCell::default(),
        };
        let pool = self.nav_pools.len();
        self.nav_pools.push(NavPool {
            kind,
            attrs: attrs.to_vec(),
            state_blocks,
            transition,
            swipe_back,
            deep_link,
            max_depth,
            path_sig,
            routes,
            known_views: known_views.iter().map(|s| (*s).to_string()).collect(),
            env_snapshot: self.capture_env_snapshot(),
            entries: Vec::new(),
            current: 0,
            anim: None,
            live: Vec::new(),
            progress: 1.0,
            popping: false,
            last_path: None,
            shared,
            pending_change: None,
            warned_paths: Vec::new(),
            span: el.span,
        });
        self.nav_elems.push(None);
        RenderNode::Nav { pool, path }
    }

    /// Reconciles one navigation container against its driving projection
    /// (RFC-0026), then advances its transition and descends into the live
    /// screens so nested `when`/`for`/`NavStack` reconcile too. Returns `true`
    /// when something structural changed, so the caller re-pulls.
    fn reconcile_nav(&mut self, pool: usize, path: ScopeId, depth: u32) -> bool {
        let mut dirtied = false;
        // The navigation value. Anything that is not a `Str` names no route, so
        // fall back to the first case, a `NavStack` always shows *something*.
        let target = match self.binding_value(path) {
            Some(Value::Str(s)) => s,
            _ => self.nav_pools[pool]
                .routes
                .first()
                .map_or_else(String::new, |r| r.pattern.raw.clone()),
        };
        if self.nav_pools[pool].last_path.as_deref() != Some(target.as_str()) {
            self.nav_pools[pool].last_path = Some(target.clone());
            dirtied |= self.navigate_to(pool, &target);
        }
        dirtied |= self.advance_nav(pool);

        let live: Vec<usize> = self.nav_pools[pool].live.iter().map(|s| s.entry).collect();
        for entry in live {
            // Take the subtree out so a nested container growing `self.nav_pools`
            // (or the `for`/`when` pools) can never alias this entry's vector.
            let nodes = std::mem::take(&mut self.nav_pools[pool].entries[entry].nodes);
            dirtied |= self.reconcile_structure(&nodes, depth + 1);
            self.nav_pools[pool].entries[entry].nodes = nodes;
        }
        dirtied
    }

    /// Moves a navigation container to `target` (RFC-0026 §3): pops to a
    /// preserved entry if the path is already on the stack, switches to an
    /// instantiated tab, or mounts a new screen. Returns `true` if the live set
    /// changed.
    fn navigate_to(&mut self, pool: usize, target: &str) -> bool {
        // A navigation that lands mid-transition finishes the previous one
        // outright: entry indices stay stable, and the user's latest intent,
        // not a half-finished animation, decides what is on screen.
        self.finish_nav(pool);
        let kind = self.nav_pools[pool].kind;
        let from = self.nav_pools[pool].current;
        // `replace(…)` armed this navigation to take the current slot.
        let replacing = kind == NavKind::Stack
            && std::mem::take(&mut self.nav_pools[pool].shared.borrow_mut().replace_next);
        if !replacing
            && self.nav_pools[pool]
                .entries
                .get(from)
                .is_some_and(|e| e.path == target)
        {
            return false;
        }

        // A path already on the stack is a *pop* back to it; a tab already
        // instantiated is simply re-shown. Either way the preserved subtree,
        // its `var`s, scroll offsets and controllers, is reused as it stands.
        // A replace deliberately skips this: it must mint a fresh screen in the
        // slot it is taking over, not resurrect an older one.
        if !replacing {
            if let Some(to) = self.nav_pools[pool]
                .entries
                .iter()
                .position(|e| e.path == target)
            {
                let pop = kind == NavKind::Stack && to < from;
                self.begin_nav_anim(pool, from, to, pop);
                self.sync_nav_shared(pool);
                return true;
            }
        }

        let Some((route, params)) = self.match_route(pool, target) else {
            // RFC-0026: an unmatched path is a runtime warning, the container
            // keeps showing the last matched route rather than blanking. Warned
            // once per path so a steady-state mismatch cannot spam the log.
            if !self.nav_pools[pool]
                .warned_paths
                .iter()
                .any(|p| p == target)
            {
                self.nav_pools[pool].warned_paths.push(target.to_string());
                let span = self.nav_pools[pool].span;
                self.errors.push(CompileError::UnmatchedRoute {
                    span,
                    path: target.to_string(),
                });
            }
            return false;
        };

        // RFC-0026 resolved question "memory pressure": refuse to grow a stack
        // past `max_depth` and reflect the refusal back into the navigation
        // `var`, so app state and screen agree instead of silently diverging.
        let depth = self.nav_pools[pool].entries.len();
        let max = self.nav_pools[pool].max_depth;
        if kind == NavKind::Stack && !replacing && max > 0 && depth >= max {
            // Reported on the frame the push is refused. It cannot repeat on its
            // own, reflecting the path back means the next reconcile sees no
            // change, so the host hears about each distinct runaway once.
            self.perf_warnings.push(PerfWarning::DeepNavStack {
                depth,
                path: target.to_string(),
            });
            self.reflect_nav_path(pool);
            return false;
        }

        // A stack only ever pushes onto its top: anything above `current` was
        // already discarded by the pop that got us here. A replace goes one
        // further and drops the screen it is standing on, so the new one lands
        // in that slot rather than on top of it.
        let from = if replacing {
            self.drop_nav_entries(pool, from);
            let below = from.saturating_sub(1);
            self.nav_pools[pool].current = below;
            below
        } else {
            if kind == NavKind::Stack {
                self.drop_nav_entries(pool, from + 1);
            }
            from
        };
        let to = self.mount_nav_entry(pool, route, target, &params);
        self.begin_nav_anim(pool, from, to, false);
        self.sync_nav_shared(pool);
        true
    }

    /// Mirrors the stack's shape into the cell the lowered `back`/`replace`
    /// closures read (see [`NavShared`]). Called whenever entries or `current`
    /// move, which is the only time it can go stale.
    fn sync_nav_shared(&mut self, pool: usize) {
        let paths: Vec<String> = self.nav_pools[pool]
            .entries
            .iter()
            .map(|e| e.path.clone())
            .collect();
        let current = self.nav_pools[pool].current;
        let mut shared = self.nav_pools[pool].shared.borrow_mut();
        shared.paths = paths;
        shared.current = current;
    }

    /// The first route whose pattern matches `path`, with its extracted
    /// parameters, declaration order, first match wins (RFC-0026).
    fn match_route(&self, pool: usize, path: &str) -> Option<(usize, Vec<(String, String)>)> {
        self.nav_pools[pool]
            .routes
            .iter()
            .enumerate()
            .find_map(|(i, r)| r.pattern.match_path(path).map(|params| (i, params)))
    }

    /// Instantiates a route's View subtree for `path` and appends it as a new
    /// entry, returning its index. The body is lowered with `route` (and the
    /// `{|params| … }` binding, if written) in scope, bound to the extracted
    /// parameters as records, `Str`-valued in v1 (RFC-0026 resolved question).
    fn mount_nav_entry(
        &mut self,
        pool: usize,
        route: usize,
        path: &str,
        params: &[(String, String)],
    ) -> usize {
        let body = self.nav_pools[pool].routes[route].body.clone();
        let binding = self.nav_pools[pool].routes[route].params_binding.clone();
        let known: Vec<String> = self.nav_pools[pool].known_views.clone();
        let env_snap = self.nav_pools[pool].env_snapshot.clone();

        let env_base = self.env.len();
        for (k, v) in &env_snap {
            self.env.push(k.clone(), v.clone());
        }
        let params_record = Value::Record(
            params
                .iter()
                .map(|(k, v)| (Symbol::intern(k), Value::Str(v.clone())))
                .collect(),
        );
        self.env.push(
            Symbol::intern("route"),
            Value::Record(vec![
                (Symbol::intern("path"), Value::Str(path.to_string())),
                (Symbol::intern("params"), params_record.clone()),
            ]),
        );
        if let Some(name) = binding {
            self.env.push(name, params_record);
        }
        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
        // Mark that we are inside a route body, so anything re-lowered at render
        // time (an event action, a reactive prop) captures `route`/`params` in
        // its env snapshot, the bindings are truncated out just below.
        self.nav_depth += 1;
        let nodes = self.lower_members(&body, &known_refs);
        self.nav_depth -= 1;
        self.env.truncate(env_base);

        let body_span = members_span(&self.nav_pools[pool].routes[route].body);
        self.nav_pools[pool].entries.push(NavEntry {
            path: path.to_string(),
            nodes,
            body_span,
        });
        self.nav_pools[pool].entries.len() - 1
    }

    /// Starts the transition from entry `from` to entry `to`, or completes the
    /// move instantly when there is nothing to animate (`transition: none`, a
    /// first mount, or a host that never advanced the clock).
    fn begin_nav_anim(&mut self, pool: usize, from: usize, to: usize, pop: bool) {
        use crate::interp::nav::NavTransition;

        self.nav_pools[pool].current = to;
        let transition = self.nav_pools[pool].transition;
        let first_mount = self.nav_pools[pool].live.is_empty();
        if transition == NavTransition::None || !self.clock_set || first_mount || from == to {
            self.nav_pools[pool].anim = None;
            self.complete_nav(pool, pop, !first_mount);
            return;
        }
        self.nav_pools[pool].anim = Some(NavAnim {
            outgoing: from,
            incoming: to,
            pop,
            motion: byard_core::frame::Motion {
                from: 0.0,
                to: 1.0,
                start_ms: self.now_ms,
                curve: nav_progress_curve(transition),
            },
            gesture: false,
            gesture_p: 0.0,
        });
    }

    /// Completes any in-flight transition: a pop discards the screens it left
    /// behind (with their animation state, RFC-0025), and the settled path is
    /// queued for `route_change`.
    fn finish_nav(&mut self, pool: usize) {
        let Some(anim) = self.nav_pools[pool].anim.take() else {
            return;
        };
        self.complete_nav(pool, anim.pop, true);
    }

    /// The settled state of a navigation, whether it animated or not: a pop
    /// discards what it left behind, and the arrival is queued for
    /// `route_change` unless this was the container's first mount.
    fn complete_nav(&mut self, pool: usize, pop: bool, announce: bool) {
        if pop {
            let keep = self.nav_pools[pool].current + 1;
            self.drop_nav_entries(pool, keep);
        }
        if announce {
            self.nav_pools[pool].pending_change = self.nav_pools[pool]
                .entries
                .get(self.nav_pools[pool].current)
                .map(|e| e.path.clone());
        }
        self.sync_nav_shared(pool);
    }

    /// Discards entries from `keep` upward (RFC-0026 §4: only the back target is
    /// preserved, the routes a multi-pop skipped over are gone), dropping each
    /// discarded screen's animation state along with it.
    fn drop_nav_entries(&mut self, pool: usize, keep: usize) {
        while self.nav_pools[pool].entries.len() > keep {
            let Some(entry) = self.nav_pools[pool].entries.pop() else {
                break;
            };
            self.drop_animation_state(entry.body_span);
        }
    }

    /// Samples this frame's transition progress, settles it if it has arrived,
    /// and rebuilds the live screen set. Returns `true` when the live set
    /// changed (a screen mounted or unmounted), which is what makes a
    /// transition's start and end structural events.
    fn advance_nav(&mut self, pool: usize) -> bool {
        let before: Vec<usize> = self.nav_pools[pool].live.iter().map(|s| s.entry).collect();
        let (p, pop) = match self.nav_pools[pool].anim {
            None => (1.0, false),
            Some(anim) if anim.gesture => (anim.gesture_p, anim.pop),
            Some(anim) => {
                // A duration ramp is *done* when its duration has elapsed,
                // exactly, on the frame it reaches `p = 1`. Settling on the
                // clock rather than on an epsilon around the value keeps "the
                // motion has stopped" and "the pixels have stopped" the same
                // instant: an epsilon test keeps frames coming through a tail
                // the eye cannot see, and can call it settled at a `p` that is
                // not quite `1`, leaving the screen fractionally out of place
                // until something else forces a redraw.
                let elapsed = self.now_ms.saturating_sub(anim.motion.start_ms);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let duration = anim.motion.curve.params[0].max(0.0) as u32;
                if elapsed >= duration {
                    self.finish_nav(pool);
                    (1.0, false)
                } else {
                    // Keep the frames coming while a screen is still moving.
                    self.any_active = true;
                    (anim.motion.sample(self.now_ms), anim.pop)
                }
            }
        };
        self.rebuild_nav_live(pool, p, pop);
        before
            != self.nav_pools[pool]
                .live
                .iter()
                .map(|s| s.entry)
                .collect::<Vec<_>>()
    }

    /// Recomputes which screens are alive this frame, in painter's order: at
    /// rest exactly one (the current entry); mid-transition the covered screen
    /// first and the one on the moving edge above it, so a pushed screen slides
    /// *over* its predecessor and a popped one slides *off* the screen beneath.
    fn rebuild_nav_live(&mut self, pool: usize, p: f32, pop: bool) {
        let current = self.nav_pools[pool].current;
        let live = match self.nav_pools[pool].anim {
            Some(anim) => {
                // On a push the arriving screen is on top; on a pop the leaving
                // one is, since it is the screen sliding away over the other.
                let (back, front) = if pop {
                    (anim.incoming, anim.outgoing)
                } else {
                    (anim.outgoing, anim.incoming)
                };
                [back, front]
                    .into_iter()
                    .map(|entry| LiveScreen {
                        entry,
                        incoming: entry == anim.incoming,
                        in_flow: entry == current,
                    })
                    .collect()
            }
            None if current < self.nav_pools[pool].entries.len() => vec![LiveScreen {
                entry: current,
                incoming: true,
                in_flow: true,
            }],
            None => Vec::new(),
        };
        self.nav_pools[pool].progress = p;
        self.nav_pools[pool].popping = pop;
        self.nav_pools[pool].live = live;
    }

    /// Writes the current entry's path back into the navigation `var`, when it
    /// is one (RFC-0026: `path` is *reflected*, the engine and the app share
    /// one source of truth, so an engine-side move updates it).
    fn reflect_nav_path(&mut self, pool: usize) {
        let Some(sig) = self.nav_pools[pool].path_sig else {
            return;
        };
        let current = self.nav_pools[pool].current;
        let Some(path) = self.nav_pools[pool]
            .entries
            .get(current)
            .map(|e| e.path.clone())
        else {
            return;
        };
        // Keep `last_path` in step so the write is not read back as a fresh
        // navigation on the next reconcile.
        self.nav_pools[pool].last_path = Some(path.clone());
        self.ctx.write_signal(sig, Value::Str(path));
    }

    /// Fires `route_change` for every container whose navigation has settled
    /// since the last tick (RFC-0026 §1). Runs after the render that registered
    /// the handlers, like the other engine-fired events.
    fn fire_route_changes(&mut self) {
        for pool in 0..self.nav_pools.len() {
            let (Some(path), Some(&Some(elem))) = (
                self.nav_pools[pool].pending_change.take(),
                self.nav_elems.get(pool),
            ) else {
                continue;
            };
            self.router.fire_event(
                &mut self.ctx,
                elem,
                super::events::EventKind::RouteChange,
                Some(&Value::Str(path)),
            );
        }
    }

    /// Lowers one of the RFC-0026 navigation actions to a reactive computation.
    /// Returns `None` for anything else, so ordinary calls keep their handling.
    ///
    /// All three end in the same place, a write to the navigation `var`, which
    /// is the whole model: navigation state *is* that `var`, so an action and an
    /// assignment are the same event downstream. What the two non-trivial
    /// actions add is the bit an assignment cannot know: `back` reads the
    /// history to find the path underneath, and `replace` marks the write so the
    /// next reconcile takes the current screen's slot instead of stacking.
    fn lower_nav_action(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        payload_name: Option<&Symbol>,
    ) -> Option<Lowered> {
        let Expr::Ident(name, _) = callee else {
            return None;
        };
        let action = name.as_str();
        if !matches!(action, "navigate" | "back" | "replace") {
            return None;
        }
        // The first argument names the navigation `var`; without a matching
        // container there is nothing to drive, so leave the call alone rather
        // than swallow an app's own `navigate`/`back`/`replace` binding.
        let target = args.first()?;
        let Expr::Ident(var, _) = &target.value else {
            return None;
        };
        let sig = match self.env.lookup(var) {
            Some(Value::Signal(sig)) => *sig,
            _ => return None,
        };
        // Keyed by the `var`, so lowering order does not matter: whichever of
        // the action and the container is lowered first creates the cell.
        let shared = self.nav_shared_cell(sig);

        if action == "back" {
            return Some(Box::new(move |ctx| {
                let previous = {
                    let state = shared.borrow();
                    (state.current > 0)
                        .then(|| state.paths.get(state.current - 1).cloned())
                        .flatten()
                };
                // At the root there is nothing to pop, a no-op, not an error
                // (RFC-0026 §6). The returned `Bool` says which happened.
                match previous {
                    Some(path) => {
                        ctx.write_signal(sig, Value::Str(path));
                        Value::Bool(true)
                    }
                    None => Value::Bool(false),
                }
            }));
        }

        let mut path = self.lower_expr(&args.get(1)?.value, payload_name);
        let is_replace = action == "replace";
        Some(Box::new(move |ctx| {
            let value = path(ctx);
            let Value::Str(target) = value else {
                return Value::Unit;
            };
            if is_replace {
                shared.borrow_mut().replace_next = true;
            }
            ctx.write_signal(sig, Value::Str(target));
            Value::Unit
        }))
    }

    /// Starts an interactive edge-swipe pop on the stack under `pos`, if one is
    /// enabled there, the press landed in the leading edge strip, and there is
    /// somewhere to pop to (RFC-0026 §"Swipe-back gesture").
    ///
    /// The gesture *is* the transition: it moves `current` to the entry below
    /// immediately and drives the same pop geometry by hand, so what the finger
    /// drags is the real previous screen, already preserved on the stack, not
    /// a snapshot of it. Returns `true` if a swipe began, so the caller knows
    /// the press is spoken for.
    fn begin_nav_swipe(&mut self, pos: (f32, f32)) -> bool {
        let (px, py) = pos;
        let Some(&(_, pool)) = self.nav_targets.iter().rev().find(|(r, _)| {
            px >= r.x && px < r.x + NAV_SWIPE_EDGE.min(r.w) && py >= r.y && py < r.y + r.h
        }) else {
            return false;
        };
        let from = self.nav_pools[pool].current;
        if from == 0 || self.nav_pools[pool].anim.is_some() {
            return false;
        }
        self.nav_pools[pool].current = from - 1;
        self.nav_pools[pool].anim = Some(NavAnim {
            outgoing: from,
            incoming: from - 1,
            pop: true,
            motion: byard_core::frame::Motion::resting(0.0),
            gesture: true,
            gesture_p: 0.0,
        });
        self.nav_swipe = Some((pool, px));
        self.sync_nav_shared(pool);
        true
    }

    /// Tracks a live edge swipe: the pop's progress is the fraction of the
    /// container's width the finger has travelled.
    fn drive_nav_swipe(&mut self, pos: (f32, f32)) {
        let Some((pool, start_x)) = self.nav_swipe else {
            return;
        };
        let width = self
            .nav_targets
            .iter()
            .find(|(_, p)| *p == pool)
            .map_or(0.0, |(r, _)| r.w);
        if width <= 0.0 {
            return;
        }
        let p = ((pos.0 - start_x) / width).clamp(0.0, 1.0);
        if let Some(anim) = self.nav_pools[pool].anim.as_mut() {
            anim.gesture_p = p;
        }
    }

    /// Releases an edge swipe (RFC-0026): past [`NAV_SWIPE_COMMIT`] the pop
    /// completes, otherwise it springs back. Either way the hand-off is the
    /// same, the gesture's progress becomes a spring's starting point, so the
    /// screen never jumps at the moment the finger lifts.
    fn release_nav_swipe(&mut self) {
        let Some((pool, _)) = self.nav_swipe.take() else {
            return;
        };
        let Some(anim) = self.nav_pools[pool].anim else {
            return;
        };
        let p = anim.gesture_p;
        // The gesture already covered `p` of the distance, so the ramp that
        // finishes (or undoes) it runs only for the fraction still to travel: a
        // finger released a hair from the end must not sit through a whole
        // transition's worth of frames to cross the last few pixels.
        let remaining = if p >= NAV_SWIPE_COMMIT { 1.0 - p } else { p };
        let curve = nav_transition_tail(self.nav_pools[pool].transition, remaining);
        if p >= NAV_SWIPE_COMMIT {
            // Commit: finish the pop the finger started, and reflect the new
            // path so app state and screen agree (`current` is already there).
            self.nav_pools[pool].anim = Some(NavAnim {
                motion: byard_core::frame::Motion {
                    from: p,
                    to: 1.0,
                    start_ms: self.now_ms,
                    curve,
                },
                gesture: false,
                ..anim
            });
            self.reflect_nav_path(pool);
        } else {
            // Cancel: the same two screens, run the other way. A cancelled pop
            // is a push back to where we were, resumed at the mirrored
            // progress, which is why nothing on screen moves at the hand-off.
            self.nav_pools[pool].current = anim.outgoing;
            self.sync_nav_shared(pool);
            self.nav_pools[pool].anim = Some(NavAnim {
                outgoing: anim.incoming,
                incoming: anim.outgoing,
                pop: false,
                motion: byard_core::frame::Motion {
                    from: 1.0 - p,
                    to: 1.0,
                    start_ms: self.now_ms,
                    curve,
                },
                gesture: false,
                gesture_p: 0.0,
            });
        }
    }

    /// The shared navigation cell for the `var` `sig`, created on first use.
    fn nav_shared_cell(&mut self, sig: SignalId) -> NavSharedCell {
        std::rc::Rc::clone(self.nav_shared.entry(sig).or_default())
    }

    /// Delivers an OS URL intent to every `deep_link: true` navigation stack
    /// whose route table matches it (RFC-0026 §"Deep linking").
    ///
    /// The URL's *path* is what navigates, scheme and authority are the
    /// platform's business, so `byard://item/42`, `https://app.example/item/42`
    /// and `/item/42` all reach the same route. Returns `true` if any stack
    /// accepted the link; a URL no stack has a route for is rejected here rather
    /// than navigating something to a blank screen.
    ///
    /// This is the whole of the deep-link contract the compiler owns: the host
    /// registers the scheme with the OS and hands the URL here, and from this
    /// point on it is an ordinary navigation, the same push, the same
    /// transition, the same `route_change`.
    ///
    /// Only *mounted* containers can receive a link: a stack nested in a tab the
    /// app has never shown does not exist yet, so call this after the first
    /// render, and put the stack that should answer deep links in the tab the
    /// app starts on.
    pub fn apply_deep_link(&mut self, url: &str) -> bool {
        let path = deep_link_path(url);
        let mut accepted = false;
        for pool in 0..self.nav_pools.len() {
            if !self.nav_pools[pool].deep_link || self.match_route(pool, &path).is_none() {
                continue;
            }
            let Some(sig) = self.nav_pools[pool].path_sig else {
                continue;
            };
            self.ctx.write_signal(sig, Value::Str(path.clone()));
            accepted = true;
        }
        accepted
    }

    /// Whether any navigation container declares `deep_link: true`, what a host
    /// checks before registering a URL scheme with the OS.
    #[must_use]
    pub fn accepts_deep_links(&self) -> bool {
        self.nav_pools.iter().any(|p| p.deep_link)
    }

    /// The path currently shown by each navigation container, in declaration
    /// order, the observable navigation state, for tests and host tooling.
    #[must_use]
    pub fn nav_paths(&self) -> Vec<String> {
        self.nav_pools
            .iter()
            .map(|p| {
                p.entries
                    .get(p.current)
                    .map_or_else(String::new, |e| e.path.clone())
            })
            .collect()
    }

    /// The history depth of each navigation container, in declaration order.
    #[must_use]
    pub fn nav_depths(&self) -> Vec<usize> {
        self.nav_pools.iter().map(|p| p.entries.len()).collect()
    }

    /// Walks a render tree, projecting it into a `byard-core` [`RenderFrame`]
    /// using Taffy layout via `byard-core`'s [`LayoutAtlas`].
    #[allow(clippy::similar_names)]
    pub fn render(
        &mut self,
        tree: &[RenderNode],
        frame: &mut byard_core::frame::RenderFrame,
        width: f32,
        height: f32,
    ) {
        use byard_core::frame::Viewport;

        // RFC-0034: the registered families ride every frame, not just the one
        // after registration. The relay keeps only the latest frame, so a pool
        // handed over once is a pool a dropped frame loses for good; the table
        // is `Arc`-shared, so carrying all of it is a pointer clone and the
        // render thread skips what it already holds.
        frame.set_fonts(self.fonts.clone());

        // RFC-0030 §I1. `layout.taffy` (Native) nests strictly inside this
        // scope, the interpreter owns the `LayoutAtlas` and drives it from
        // here, which is exactly why the interpreter tax is self-time and
        // not inclusive time (RFC-0030 §I2b): an AOT build still pays for
        // Taffy in full, so billing layout to the interpreter would make the
        // AOT projection optimistic by the entire cost of layout.
        byard_core::profile_scope!(
            "interp.render",
            byard_core::telemetry::ScopeKind::Interpreter
        );

        // RFC-0032 §R7: which layout paths this frame takes, as a delta rather
        // than a running total, so the `byard dev` readout can answer "am I on
        // the fast path?" for *this* frame.
        let paths_before = byard_core::atlas::layout::path_counters::snapshot();

        // Recomputed every frame: an animation re-marks itself active below if it
        // sampled without having settled this tick (RFC-0010).
        self.any_active = false;
        // RFC-0023: retire fully-faded ripples by time. Gesture-like state kept
        // across renders, so this runs before the walk, ink whose element no
        // longer renders (hot reload, `when` unmount) ages out here too.
        let now = self.now_ms;
        self.ripples
            .retain(|r| (now.saturating_sub(r.start_ms) as f32) < r.duration_ms);
        // One monotonic tick per render, the clock-independent basis for the
        // RFC-0021 "scroll has gone quiet" snap settle.
        self.frame_seq = self.frame_seq.wrapping_add(1);
        // Runtime diagnostics are recomputed per frame, so clear them *before*
        // the passes that record them (reconcile can raise one too, RFC-0026's
        // stack-depth guard fires while navigation is being reconciled).
        self.perf_warnings.clear();
        // RFC-0036: anchors are recorded by this frame's walk, so last frame's
        // rects have to go first. An anchor whose element unmounted must stop
        // existing rather than keep pointing at where it used to be.
        self.anchor_rects.clear();
        // The atlas is **not** torn down here any more (RFC-0032 §R3). Whether
        // it can be retained is not knowable until `reconcile_structure` has
        // run, so the decision, and the `clear()` that follows from it, moves
        // below, next to the build it governs. Nothing between here and there
        // touches the atlas.
        // Rebuild the handler set from the fresh layout, but keep the in-flight
        // gesture state (a pending `down`, the focused element) so a tap that
        // spans this re-render is still recognized (RFC-0003 E4).
        self.router.clear_handlers();
        // RFC-0021, over the previous frame's scroll targets (before they are
        // dropped, so the offset writes below are what *this* frame paints):
        //   • reverse `page:`, honour an app-driven `page` change (edge-triggered,
        //     never fights a drag);
        //   • snap settle, snap a `snap: page` view once its scroll has gone quiet
        //     (works for wheel/trackpad, which has no release event).
        self.sync_page_offsets();
        self.advance_snap_anims();
        self.settle_snaps();
        // RFC-0021 pull-to-refresh: honour an app-driven `refreshing` change, then
        // advance the pull-region spring (retract / rest) over the same targets.
        self.sync_refreshing();
        self.advance_pull_anims();
        // Wheel-scroll targets are re-recorded each render (RFC-0005), like the
        // router's hit rects. RFC-0021 `snap: item` boundaries follow the same
        // lifecycle (settle above read the previous frame's; emit rebuilds them).
        self.scroll_targets.clear();
        self.scroll_item_bounds.clear();
        // RFC-0039: where each native view ended up is re-recorded every
        // render, for the same reason the scroll targets are, a rect from two
        // frames ago is a hit test against a widget that has moved.
        self.native_targets.clear();
        // RFC-0037: a mesh nobody has drawn for a while stops being paid for.
        // Swept here rather than on insertion, because the question is "is
        // this path still on screen", and only a frame knows that.
        if !self.path_meshes.is_empty() {
            let horizon = self.frame_seq.saturating_sub(MESH_CACHE_FRAMES);
            self.path_meshes.retain(|_, m| m.last_used >= horizon);
        }
        // RFC-0026 swipe-back regions follow the same render-then-dispatch
        // lifecycle as the scroll targets.
        self.nav_targets.clear();
        // RFC-0018 radio groups are rebuilt from the fresh layout each render.
        self.radio_groups.clear();
        // The walk starts outside every `for`. Reset rather than assume,
        // so a frame that ends deep inside a list cannot leak that row's
        // identity into the next frame's top-level animations.
        self.anim_slot = 0;

        // Drain any MSDF generations that finished since the last tick,
        // before the tree walk below, so a freshly-resident glyph is visible
        // the same tick it lands (RFC-0009 §2, INV-2: logic-thread only).
        for upload in self.vector_jit.drain_ready() {
            frame.push_atlas_upload(upload);
        }
        // RFC-0018 structural reactivity, phase A: reconcile the `for` pools
        // (grow to the current list lengths, rewrite element slots) so the tree's
        // reactive structure reflects this frame's state. If anything changed,
        // re-pull so the freshly-mounted/updated bindings project before paint.
        // Iterate to a fixpoint: a branch/body lowered *this* pass creates fresh
        // bindings (its own `when` condition, its `for` list) that are not
        // projected until the next pull, so a newly-mounted nested `when`/`for`
        // would otherwise read stale (false/empty) for a frame. Re-pull and
        // re-reconcile until nothing new mounts. Bounded by the reconcile depth
        // guard, so a runaway recursion still terminates (with a diagnostic).
        let mut passes = 0;
        let mut structure_changed = false;
        loop {
            // The structural walk marks every effect it reaches; settling
            // reads those marks and fires the mount/unmount edges they imply
            // (RFC-0028 §4b). It runs *inside* the fixpoint because a mount
            // action writes `var`s, and a screen whose `on mount` sets
            // `state = "loading"` should show the spinner on the frame it
            // mounted, not the one after.
            let mut changed = self.reconcile_structure(tree, 0);
            changed |= self.settle_effects();
            if !changed || passes > MAX_INSTANCE_DEPTH {
                break;
            }
            structure_changed = true;
            let epoch = self.ctx.begin_tick();
            self.ctx.pull(epoch);
            passes += 1;
        }
        // Phase B is read-only over the pools: take them out so `&mut self`
        // (atlas, router, …) stays free while build/paint borrow them.
        let for_pools = std::mem::take(&mut self.for_pools);
        let when_pools = std::mem::take(&mut self.when_pools);
        let nav_pools = std::mem::take(&mut self.nav_pools);
        let pools = Pools {
            fors: &for_pools,
            whens: &when_pools,
            navs: &nav_pools,
        };

        // RFC-0017: collect every mounted `Overlay` (pre-order = declaration =
        // mount order) and build each into the *same* atlas as an absolutely
        // positioned wrapper floating over the main tree. Nothing is built when
        // no overlay is mounted, so the overlay path is truly zero-cost, the
        // render root stays the plain main container it always was.
        let mut overlays: Vec<&RenderNode> = Vec::new();
        self.collect_overlays(tree, pools, &mut overlays);

        // ── RFC-0032 §R4: may this frame retain the layout tree? ──────────────
        //
        // A default-deny whitelist. Every clause below is a reason the build
        // order could differ from last frame's, and anything not on the list
        // takes the full rebuild, so a future structural mutation that nobody
        // classified is safe by omission rather than by review.
        let shape = Self::layout_shape(overlays.len(), pools);
        let eligible = !structure_changed
            && !self.retained.invalidated
            && self.retained.viewport == Some((width, height))
            && self.retained.shape.as_ref() == Some(&shape)
            && self.retained.dark == Some(self.theme_is_dark())
            && !self.retained.flat_ids.is_empty();
        self.retained.invalidated = false;

        let mut flat_ids = Vec::new();
        let mut overlay_layouts: Vec<OverlayLayout<'_>> = Vec::new();
        let mut root_ids = None;
        let mut retained_used = false;

        if eligible {
            self.atlas.begin_retained_build();
            root_ids = self.build_layout_pass(
                tree,
                pools,
                (width, height),
                &mut flat_ids,
                &overlays,
                &mut overlay_layouts,
            );
            // `end_retained_build` is the load-bearing check: it fails unless
            // every build-order slot was reused *and* the walk produced exactly
            // as many nodes as the retained tree holds. The `flat_ids`
            // comparison is redundant given that, and kept anyway, this is the
            // path where being wrong is invisible on screen and answers taps
            // from the wrong element.
            let retained_ok = self.atlas.end_retained_build();
            retained_used = retained_ok && root_ids.is_some() && flat_ids == self.retained.flat_ids;
            if !retained_used {
                // Discard wholesale. A half-applied retained build is not a
                // thing this code will ever try to repair in place.
                //
                // `end_retained_build` counts its own `false` verdicts; the two
                // extra checks above are the caller's, so their discards are
                // noted here. Otherwise a whitelist that let an ineligible
                // frame through would look free: the rebuild that follows is
                // indistinguishable from a frame the whitelist rejected outright.
                if retained_ok {
                    byard_core::atlas::layout::path_counters::note_retained_rollback();
                }
                self.atlas.clear();
                flat_ids.clear();
                overlay_layouts.clear();
                root_ids = None;
            }
        }

        if !retained_used {
            if !eligible {
                self.atlas.clear();
            }
            root_ids = self.build_layout_pass(
                tree,
                pools,
                (width, height),
                &mut flat_ids,
                &overlays,
                &mut overlay_layouts,
            );
        }

        let Some((main_id, _root_id)) = root_ids else {
            // Nothing to lay out (an empty tree). Still restore the pools taken
            // out above, or the next frame would see them empty (RFC-0018).
            self.for_pools = for_pools;
            self.when_pools = when_pools;
            self.nav_pools = nav_pools;
            self.retained.flat_ids.clear();
            self.retained.viewport = None;
            self.retained.shape = None;
            frame.set_atlas_paths(path_delta(
                paths_before,
                byard_core::atlas::layout::path_counters::snapshot(),
            ));
            return;
        };
        // Drive layout with the shared text measurer so wrapping `Text` leaves
        // reflow to their parent's width (RFC-0005 default wrap). Disjoint field
        // borrows: `self.atlas` and `self.text_measurer`.
        let measurer = self
            .text_measurer
            .get_or_insert_with(byard_core::text::TextMeasurer::new);
        if retained_used {
            // **`_with_text`, always.** The sizer-less `recompute_dirty` runs
            // the measure protocol with no sizer, so every wrapping `Text` leaf
            // Taffy touches would fall back to its natural single-line size and
            // silently un-wrap (RFC-0032 §R5). Taffy invokes the callback only
            // for nodes it is actually recomputing, so a clean paragraph is
            // never re-shaped, which is where this whole path's win comes from.
            self.atlas
                .recompute_dirty_with_text(Viewport::new(width, height), measurer)
                .unwrap();
        } else {
            self.atlas
                .compute_with_text(Viewport::new(width, height), measurer)
                .unwrap();
        }
        // RFC-0032 §R3 step 5. On the retained path this is the set of nodes
        // whose layout *inputs* moved; on a full rebuild every node is new, so
        // the atlas reports the whole tree and the frame is dirty everywhere,
        // which is the truth about a rebuilt frame.
        self.atlas.populate_frame_dirty(frame, retained_used);

        // RFC-0038: rects are final for this frame, so an element that asked
        // for its own is told now, before anything paints and while nothing can
        // still move it (INV-29). Elements whose rect did not change are not
        // told anything, which is what keeps a static screen at zero writes.
        self.fire_measures();

        self.retained.flat_ids.clear();
        self.retained.flat_ids.extend_from_slice(&flat_ids);
        self.retained.viewport = Some((width, height));
        self.retained.shape = Some(shape);
        self.retained.dark = Some(self.theme_is_dark());

        let parent_rect = crate::interp::intrinsics::Rect::new(0.0, 0.0, width, height);

        // Emit the main tree (below every overlay in painter's order). Iterate
        // the same expanded concrete node sequence `build_children` laid out, so
        // the flat-id cursor stays in lockstep (RFC-0018).
        if main_id.is_some() {
            let mut flat_idx = 0;
            for concrete in self.expand_concrete(tree, pools) {
                let node_id = flat_ids[flat_idx];
                self.anim_slot = concrete.slot;
                self.render_node_with_atlas(
                    concrete.node,
                    node_id,
                    frame,
                    &flat_ids,
                    &mut flat_idx,
                    parent_rect,
                    1.0,
                    byard_core::frame::Transform::IDENTITY,
                    None,
                    (0.0, 0.0),
                    None,
                    pools,
                );
            }
        }

        // RFC-0017 overlay phase: emit each overlay's children *after* the main
        // tree, so their emission-order depth is nearer and they composite on
        // top (the shared depth buffer resolves cross-layer order, no separate
        // GPU pass needed). Emitted in mount order, so a later overlay stacks
        // over an earlier one. A modal overlay installs a scrim first.
        //
        // `begin_layer` marks the z-layer boundary: the Encoder draws each
        // layer's pools, including its *text*, as one interleaved batch
        // inside the single render pass, so this overlay's transparent
        // geometry (scrim, shadow) alpha-blends over the text and images of
        // everything beneath it instead of being drawn before a frame-final
        // text batch. With no overlay, no mark is recorded and the frame
        // renders through the exact single-layer draw stream.
        for ol in &overlay_layouts {
            frame.begin_layer();
            // An overlay leaves the layout flow, and its animation
            // identity leaves the walk with it, it is emitted here rather than
            // where it was written, so whatever row the main pass finished
            // inside must not carry over into it.
            self.anim_slot = 0;
            self.emit_overlay(ol, frame, width, height, pools);
        }

        // RFC-0032 §R3 step 6, and the last thing this frame does before its
        // primitives leave the interpreter: replace the blanket `dirty: true`
        // every emission site writes with what actually changed, by comparing
        // each primitive's resolved values against the same position last
        // frame. Runs after the overlay phase because overlays push into the
        // same pools, and one comparison over the finished frame is both
        // cheaper and harder to get wrong than a per-site bookkeeping scheme.
        self.paint.apply(frame);

        // RFC-0018/RFC-0026: return the (possibly grown) pools taken out for the
        // read-only build/paint phase.
        self.for_pools = for_pools;
        self.when_pools = when_pools;
        self.nav_pools = nav_pools;

        // RFC-0039: place the controller requests this frame's views issued.
        // After the walk, because a request is placed once per frame and the
        // walk is what decides which views ran; before the frame ships, so a
        // widget that asked for a tile on the frame it mounted has its request
        // in flight rather than waiting a frame for nothing.
        self.dispatch_native_calls(frame);

        frame.set_atlas_paths(path_delta(
            paths_before,
            byard_core::atlas::layout::path_counters::snapshot(),
        ));

        // RFC-0023 performance diagnostic: ≥ 3 stacked frosted-glass panes in
        // one frame means each upper pane re-blurs the output of the lower
        // ones, visually correct, but each pane costs a pass-split + copy +
        // blur. Recomputed from this frame's emitted pool; the host decides
        // how to surface it.
        let pane_rects: Vec<[f32; 4]> = frame.backdrops().iter().map(|b| b.rect).collect();
        let deepest = deepest_rect_overlap(&pane_rects);
        if deepest >= 3 {
            self.perf_warnings
                .push(PerfWarning::OverlappingBlurs { count: deepest });
        }
    }

    /// The structural shape a retained build must match (RFC-0032 §R4): how
    /// many overlays are mounted, and how deep / how many screens live each
    /// navigation container holds.
    ///
    /// Overlay mount/unmount and route push/pop change the node sequence
    /// without ever travelling through `reconcile_structure`, they are pools
    /// of their own, so they need their own clause rather than being covered
    /// by the structural signal that already exists.
    fn layout_shape(overlays: usize, pools: Pools<'_>) -> (usize, Vec<usize>) {
        let navs = pools
            .navs
            .iter()
            .flat_map(|p| [p.entries.len(), p.live.len()])
            .collect();
        (overlays, navs)
    }

    /// Builds this frame's layout tree into the atlas and returns
    /// `(main container, render root)`, or `None` when there is nothing to lay
    /// out.
    ///
    /// Called with the atlas either freshly [`clear`](byard_core::atlas::LayoutAtlas::clear)ed
    /// (full path) or opened for a retained build, the walk is **identical**
    /// either way, which is the property that makes the retained path safe to
    /// reason about: there is no second implementation to drift.
    fn build_layout_pass<'a>(
        &mut self,
        tree: &'a [RenderNode],
        pools: Pools<'a>,
        viewport: (f32, f32),
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
        overlays: &[&'a RenderNode],
        overlay_layouts: &mut Vec<OverlayLayout<'a>>,
    ) -> Option<(
        Option<byard_core::atlas::layout::AtlasNodeId>,
        byard_core::atlas::layout::AtlasNodeId,
    )> {
        let (width, height) = viewport;
        // Refilled by this walk (RFC-0038). Cleared rather than appended to,
        // because a retained build that rolls back runs the walk twice and the
        // second run is the one that describes the frame.
        self.measure_targets.clear();
        // Expand reactive `when`/`for` at the root, then build each concrete node.
        let root_children = self.build_children(tree, pools, flat_ids);

        for ov in overlays {
            if let Some(layout) = self.build_overlay_layout(ov, pools) {
                overlay_layouts.push(layout);
            }
        }

        // The main content container (viewport-sized, column). `None` when the
        // whole view is nothing but overlays.
        let main_id = if root_children.is_empty() {
            None
        } else {
            let root_style =
                byard_core::atlas::layout::ContainerStyle::new(Some(width), Some(height))
                    .with_direction(byard_core::atlas::layout::FlexDir::Column);
            self.atlas.add_container(root_style, &root_children).ok()
        };

        // The render root: with no overlay it is the main container itself (the
        // pre-RFC-0017 shape, unchanged). With overlays it is a super-root
        // holding the main content plus each overlay wrapper as an absolute
        // sibling that neither displaces nor is displaced by the main tree.
        let root_id = if overlay_layouts.is_empty() {
            main_id
        } else {
            let mut super_children = Vec::new();
            if let Some(m) = main_id {
                super_children.push(m);
            }
            for ol in overlay_layouts.iter() {
                super_children.push(ol.wrapper_id);
            }
            let super_style =
                byard_core::atlas::layout::ContainerStyle::new(Some(width), Some(height))
                    .with_direction(byard_core::atlas::layout::FlexDir::Column);
            self.atlas.add_container(super_style, &super_children).ok()
        };

        // Set while the atlas is still `Building`, on the retained path
        // `end_retained_build` flips it to `Computed` immediately afterwards,
        // and `set_root` refuses to run in that state.
        let root = root_id?;
        self.atlas.set_root(root).ok()?;
        Some((main_id, root))
    }

    /// Forces the next [`render`](Self::render) onto the full rebuild path
    /// (RFC-0032 §R4).
    ///
    /// Called by every mutation that can change the *shape* of the tree
    /// without going through `reconcile_structure`: a hot reload re-lowers the
    /// view, and a theme flip changes nearly every resolved value at once, so
    /// marking would visit everything and then recompute everything, strictly
    /// more expensive than the rebuild it replaced (RFC-0032 §Q6).
    pub fn invalidate_retained_layout(&mut self) {
        self.retained.invalidated = true;
        // A pool *position* is about to stop meaning what it meant, so the
        // positional paint comparison has to forget too, otherwise two
        // unrelated primitives that happen to hash alike would be equated
        // across the discontinuity (RFC-0032 §R3 step 6).
        self.paint.reset();
    }

    /// How many elements in the lowered program declared `on measure`
    /// (RFC-0038).
    ///
    /// Exposed because "an element that does not use the event pays nothing"
    /// is a claim about a number, and a claim about a number should be read
    /// from the number rather than inferred from behaviour.
    #[must_use]
    pub fn measure_slots(&self) -> usize {
        self.measures.len()
    }

    /// Runtime performance diagnostics recomputed by the last
    /// [`render`](Self::render) (RFC-0023): empty when the frame is healthy.
    #[must_use]
    pub fn perf_warnings(&self) -> &[PerfWarning] {
        &self.perf_warnings
    }

    /// Flattens a node slice into the concrete nodes to lay out/paint this frame
    /// (RFC-0018 structural reactivity): a `When` expands to its taken branch
    /// (condition re-read live), a `For` to its live pooled bodies, recursively.
    /// A concrete node (`Box`/`Text`/…) passes through unchanged, its *own*
    /// children are expanded when it is built/walked, not here. Build, paint,
    /// `flat_len`, and overlay collection all funnel through this one function, so
    /// they agree on the exact node sequence and the flat-id cursor stays aligned.
    fn expand_concrete<'a>(&self, nodes: &'a [RenderNode], pools: Pools<'a>) -> Vec<Concrete<'a>> {
        self.expand_concrete_in(nodes, pools, self.anim_slot)
    }

    /// [`expand_concrete`](Self::expand_concrete) with the enclosing animation
    /// slot passed explicitly, so a `for` can stamp its rows with theirs.
    ///
    /// A node passes through carrying whatever slot it inherited; a `for` body
    /// replaces it with that slot's own element signal. `when` is transparent,
    /// a branch is not an instance, it is the same one under a condition.
    fn expand_concrete_in<'a>(
        &self,
        nodes: &'a [RenderNode],
        pools: Pools<'a>,
        slot: u32,
    ) -> Vec<Concrete<'a>> {
        let mut out = Vec::new();
        for n in nodes {
            match n {
                RenderNode::When { cond, pool } => {
                    let take = self
                        .binding_value(*cond)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // The taken branch was lowered by `reconcile_structure`; an
                    // as-yet-unselected branch is `None` and expands to nothing.
                    if let Some(p) = pools.whens.get(*pool) {
                        let branch = if take {
                            p.then.as_ref()
                        } else {
                            p.els.as_ref()
                        };
                        if let Some(branch) = branch {
                            out.extend(self.expand_concrete_in(branch, pools, slot));
                        }
                    }
                }
                RenderNode::For { pool, .. } => {
                    if let Some(p) = pools.fors.get(*pool) {
                        for (i, body) in p.bodies.iter().take(p.len).enumerate() {
                            // The slot's own element signal, unique across the
                            // program, because a nested `for` is lowered once
                            // per outer slot and gets its own fresh signals.
                            let row = p.item_slots.get(i).map_or(slot, |s| s.0);
                            out.extend(self.expand_concrete_in(body, pools, row));
                        }
                    }
                }
                // An effect has no pixels and no layout box, so it is dropped
                // *here*, before anything downstream can count it. Giving it a
                // 0x0 leaf instead (the `Overlay` shape) would put it in the
                // flat-id sequence, which is the RFC-0032 retained path's
                // identity, for a node that can never draw.
                RenderNode::Effect { .. } => {}
                other => out.push(Concrete { node: other, slot }),
            }
        }
        out
    }

    /// Reconciles the reactive `for` pools before the paint walk (RFC-0018,
    /// coarse D7): reads each live `for`'s list, grows its pool to the list length
    /// (lowering a body the first time an index is needed), rewrites each slot's
    /// element signal, and records the live count. Returns `true` if any slot or
    /// pool changed, so the caller re-pulls to project the new values. Descends
    /// through `when` (taken branch) and `for` (live bodies) so nested loops
    /// reconcile too.
    fn reconcile_structure(&mut self, nodes: &[RenderNode], depth: u32) -> bool {
        // Bound the reconcile recursion: a guarded recursion whose guard never
        // terminates (`when go { Recurse() }` with `go` always true) lowers a new
        // level each descent, so cap it here, the same role `instance_depth`
        // plays at lower time (RFC-0007 §4), but for the reconcile-time expansion.
        // Truncate with a diagnostic rather than overflow the stack (D4: never a
        // silent failure); dedup so a re-render doesn't spam the error list.
        if depth >= MAX_INSTANCE_DEPTH {
            let already = self
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::RecursiveView { .. }));
            if !already {
                self.errors.push(CompileError::RecursiveView {
                    span: crate::diagnostics::Span::new(0, 0),
                    path: format!(
                        "(reactive `when`/`for` recursion exceeded {MAX_INSTANCE_DEPTH})"
                    ),
                });
            }
            return false;
        }
        let mut dirtied = false;
        for n in nodes {
            match n {
                RenderNode::When { cond, pool } => {
                    let take = self
                        .binding_value(*cond)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // RFC-0025: an animation lives and dies with its element, so a
                    // branch that has just been unmounted drops its animation
                    // state, a spinner that comes back starts its turn again
                    // rather than resuming a stale phase. (This is the opposite of
                    // §2's *offscreen* rule, where the element is still mounted and
                    // must resume exactly where it paused.)
                    let previous = self.when_pools[*pool].last_take;
                    if previous.is_some_and(|was| was != take) {
                        let (then_span, els_span) = self.when_pools[*pool].branch_spans;
                        let gone = if take { els_span } else { then_span };
                        self.drop_animation_state(gone);
                        dirtied = true;
                    }
                    self.when_pools[*pool].last_take = Some(take);
                    // Lazily lower the taken branch on first selection (so an
                    // untaken recursive branch never lowers), then descend into it
                    // to reconcile any nested `when`/`for`.
                    let already = if take {
                        self.when_pools[*pool].then.is_some()
                    } else {
                        self.when_pools[*pool].els.is_some()
                    };
                    if !already {
                        let ast = if take {
                            self.when_pools[*pool].then_ast.clone()
                        } else {
                            self.when_pools[*pool].els_ast.clone()
                        };
                        let known: Vec<String> = self.when_pools[*pool].known_views.clone();
                        let env_snap = self.when_pools[*pool].env_snapshot.clone();
                        let env_base = self.env.len();
                        for (k, v) in &env_snap {
                            self.env.push(k.clone(), v.clone());
                        }
                        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
                        // A branch lowered here is lowered *away from where it
                        // was written*: the scope it belongs to exists only as
                        // the snapshot just pushed, and it is truncated again
                        // below. Anything inside the branch that resolves later
                        //, an event action, an animated attribute reading the
                        // row it belongs to, a nested `for`/`when` pool, has to
                        // capture that scope now, or it will look for the row in
                        // an environment that no longer has one.
                        let restored = !env_snap.is_empty();
                        self.restored_depth += u32::from(restored);
                        let nodes = self.lower_members(&ast, &known_refs);
                        self.restored_depth -= u32::from(restored);
                        self.env.truncate(env_base);
                        if take {
                            self.when_pools[*pool].then = Some(nodes);
                        } else {
                            self.when_pools[*pool].els = Some(nodes);
                        }
                        dirtied = true;
                    }
                    // Descend into the (now-lowered) taken branch. Take it out to
                    // avoid aliasing `self.when_pools` during nested reconcile.
                    let branch = if take {
                        self.when_pools[*pool].then.take()
                    } else {
                        self.when_pools[*pool].els.take()
                    };
                    if let Some(branch) = branch {
                        dirtied |= self.reconcile_structure(&branch, depth + 1);
                        if take {
                            self.when_pools[*pool].then = Some(branch);
                        } else {
                            self.when_pools[*pool].els = Some(branch);
                        }
                    }
                }
                RenderNode::For { pool, list } => {
                    let items = match self.binding_value(*list) {
                        Some(Value::List(items)) => items,
                        _ => Vec::new(),
                    };
                    let new_len = items.len();
                    // Grow the pool: lower one body per newly-needed index, each
                    // reading its element from a fresh per-slot signal.
                    while self.for_pools[*pool].bodies.len() < new_len {
                        let slot = self.ctx.create_signal(Value::Unit);
                        // Clone the lowering inputs out first so the borrow on
                        // `self.for_pools` is released before `lower_members`
                        // (which may append *nested* pools to `self.for_pools`).
                        let item_var = self.for_pools[*pool].item_var.clone();
                        let index_var = self.for_pools[*pool].index_var.clone();
                        #[allow(clippy::cast_possible_wrap)]
                        let slot_index = self.for_pools[*pool].bodies.len() as i64;
                        let body_ast = self.for_pools[*pool].body.clone();
                        let known: Vec<String> = self.for_pools[*pool].known_views.clone();
                        let env_snap = self.for_pools[*pool].env_snapshot.clone();
                        let env_base = self.env.len();
                        for (k, v) in &env_snap {
                            self.env.push(k.clone(), v.clone());
                        }
                        self.env.push(item_var, Value::Signal(slot));
                        if let Some(index_var) = index_var {
                            self.env.push(index_var, Value::Int(slot_index));
                        }
                        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
                        // Mark that we are lowering inside a `for` body so any
                        // event action captures `t` in its env snapshot (the
                        // binding is truncated below before render, RFC-0027).
                        self.for_depth += 1;
                        let body_nodes = self.lower_members(&body_ast, &known_refs);
                        self.for_depth -= 1;
                        self.env.truncate(env_base);
                        // Index `*pool` is still valid (nested lowering only
                        // appended higher indices).
                        self.for_pools[*pool].item_slots.push(slot);
                        self.for_pools[*pool].bodies.push(body_nodes);
                        dirtied = true;
                    }
                    // Update each live slot's element value (value-deduped).
                    for (i, item) in items.iter().enumerate() {
                        let slot = self.for_pools[*pool].item_slots[i];
                        if self.ctx.peek_signal(slot) != *item {
                            self.ctx.write_signal(slot, item.clone());
                            dirtied = true;
                        }
                    }
                    // RFC-0025: an animation lives and dies with its element,
                    // so a row that just left the list drops its state, a
                    // re-grown row starts fresh rather than resuming a stale
                    // timeline. The pool's *bodies* are grow-only (index `i`
                    // reuses its lowered nodes and its element signal), which is
                    // exactly why the state has to be dropped explicitly: it
                    // would otherwise be waiting there for the next occupant.
                    let previous_len = self.for_pools[*pool].len;
                    if new_len < previous_len {
                        self.drop_slot_state(*pool, new_len, previous_len);
                        dirtied = true;
                    }
                    self.for_pools[*pool].len = new_len;
                    // Reconcile nested loops inside the live bodies. Take the
                    // bodies out so nested growth (which mutates `self.for_pools`)
                    // can't alias this pool's own vector.
                    let bodies = std::mem::take(&mut self.for_pools[*pool].bodies);
                    for body in bodies.iter().take(new_len) {
                        dirtied |= self.reconcile_structure(body, depth + 1);
                    }
                    self.for_pools[*pool].bodies = bodies;
                }
                // RFC-0026: reconcile the navigation state, advance the
                // transition, and descend into whichever screens are live.
                RenderNode::Nav { pool, path } => {
                    dirtied |= self.reconcile_nav(*pool, *path, depth);
                }
                RenderNode::Box { children, .. } | RenderNode::Overlay { children, .. } => {
                    dirtied |= self.reconcile_structure(children, depth + 1);
                }
                // Being reached by this walk *is* being mounted (RFC-0028
                // §4b); `settle_effects` reads the marks once the walk has
                // reached its fixpoint.
                RenderNode::Effect { index } => self.mark_effect_seen(*index),
                _ => {}
            }
        }
        dirtied
    }

    /// Builds the atlas layout for a node slice, expanding reactive `when`/`for`
    /// (RFC-0018) into their concrete children first. Returns the child atlas ids
    /// in paint order, appending each subtree's flat-id list to `flat_ids`.
    fn build_children(
        &mut self,
        nodes: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Vec<byard_core::atlas::layout::AtlasNodeId> {
        // The expansion refs borrow `nodes`/`pools`, never `self`, so `&mut self`
        // stays free for `build_layout_tree` below.
        let concrete = self.expand_concrete(nodes, pools);
        let mut ids = Vec::with_capacity(concrete.len());
        // The instance each child belongs to, exactly as the paint walk does it:
        // a `with` on a *layout* prop is sampled here, one pass before paint, and
        // must key to the same row there and here or the two passes drive two
        // different animations off one written clause.
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            if let Ok(id) = self.build_layout_tree(c.node, pools, flat_ids) {
                ids.push(id);
            }
        }
        // The caller's own style (`gap`, `p`, …) is evaluated *after* this
        // returns, so the walk owes it the slot it came in with rather than
        // whichever row happened to be last.
        self.anim_slot = enclosing;
        ids
    }

    /// The number of flattened layout nodes a concrete [`RenderNode`] subtree
    /// contributes, mirroring [`build_children`](Self::build_children)/
    /// [`build_layout_tree`](Self::build_layout_tree) exactly (one entry plus its
    /// expanded children). Used to advance the flat-id cursor past a culled
    /// `ScrollView` child without walking it (RFC-0005). `when`/`for` are
    /// expanded, so their live subtree is counted (RFC-0018).
    fn flat_len(&self, node: &RenderNode, pools: Pools<'_>) -> usize {
        match node {
            RenderNode::Box { children, .. } => {
                1 + self
                    .expand_concrete(children, pools)
                    .iter()
                    .map(|c| self.flat_len(c.node, pools))
                    .sum::<usize>()
            }
            // RFC-0026: the container itself, plus, per live screen, that
            // screen's synthesized wrapper and its subtree.
            RenderNode::Nav { pool, .. } => {
                let Some(p) = pools.navs.get(*pool) else {
                    return 1;
                };
                1 + p
                    .live
                    .iter()
                    .map(|screen| {
                        1 + self
                            .expand_concrete(&p.entries[screen.entry].nodes, pools)
                            .iter()
                            .map(|c| self.flat_len(c.node, pools))
                            .sum::<usize>()
                    })
                    .sum::<usize>()
            }
            _ => 1,
        }
    }

    /// Collects every mounted `Overlay` in `nodes` in pre-order (RFC-0017 mount =
    /// declaration order), expanding reactive `when`/`for` (RFC-0018) so an
    /// overlay inside a live branch/body is found. Recurses through `Box` and an
    /// overlay's own children, so a nested overlay is collected as its own later
    ///, hence higher, stack entry.
    fn collect_overlays<'a>(
        &self,
        nodes: &'a [RenderNode],
        pools: Pools<'a>,
        out: &mut Vec<&'a RenderNode>,
    ) {
        for concrete in self.expand_concrete(nodes, pools) {
            match concrete.node {
                RenderNode::Overlay { children, .. } => {
                    out.push(concrete.node);
                    self.collect_overlays(children, pools, out);
                }
                RenderNode::Box { children, .. } => {
                    self.collect_overlays(children, pools, out);
                }
                // RFC-0026 × RFC-0017: a modal opened by a live screen floats
                // over the whole app, exactly as one opened anywhere else.
                RenderNode::Nav { pool, .. } => {
                    if let Some(p) = pools.navs.get(*pool) {
                        for screen in &p.live {
                            self.collect_overlays(&p.entries[screen.entry].nodes, pools, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Builds one `Overlay`'s layout into the atlas (RFC-0017): each child is
    /// laid out at its natural size, then wrapped in an absolute, inset-0
    /// container whose `justify`/`align` realise the child's `anchor` within the
    /// viewport. All the anchor wrappers hang off one absolute overlay wrapper.
    /// Returns the wrapper id and per-child emission slots. `None` if `ov` is not
    /// an `Overlay` or the atlas rejects the nodes.
    fn build_overlay_layout<'a>(
        &mut self,
        ov: &'a RenderNode,
        pools: Pools<'a>,
    ) -> Option<OverlayLayout<'a>> {
        let RenderNode::Overlay { children, .. } = ov else {
            return None;
        };
        // RFC-0018: an overlay's direct children may be reactive `when`/`for`;
        // expand them to concrete anchor targets before laying each out.
        let concrete = self.expand_concrete(children, pools);
        let mut anchor_ids = Vec::with_capacity(concrete.len());
        let mut slots = Vec::with_capacity(concrete.len());
        // An overlay is emitted away from where it was written, and the render
        // pass resets the slot to 0 for that reason; its *own* rows still have
        // one, so lay each out as itself.
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            let child = c.node;
            let mut cflat = Vec::new();
            let Ok(cid) = self.build_layout_tree(child, pools, &mut cflat) else {
                continue;
            };
            // RFC-0036: an anchored child is positioned by the overlay pass,
            // so its wrapper must not stretch it. It has to arrive at its own
            // content size, because that size is exactly what the placement
            // measures against — a panel stretched to the viewport would be
            // "placed" correctly and still cover the screen.
            let style = if self.anchor_ref(child).is_some() {
                anchor_wrapper_style(Some("__anchored"))
            } else {
                anchor_wrapper_style(self.anchor_token(child).as_deref())
            };
            let Ok(anchor_id) = self.atlas.add_container(style, &[cid]) else {
                continue;
            };
            anchor_ids.push(anchor_id);
            slots.push(OverlayChildSlot {
                node: child,
                id: cid,
                flat_ids: cflat,
            });
        }
        self.anim_slot = enclosing;
        let wrapper_style =
            byard_core::atlas::layout::ContainerStyle::default().with_absolute(true);
        let wrapper_id = self.atlas.add_container(wrapper_style, &anchor_ids).ok()?;
        Some(OverlayLayout {
            node: ov,
            wrapper_id,
            children: slots,
        })
    }

    /// The `anchor_to:` name of an overlay child (RFC-0036), if it anchors to
    /// an element rather than to the viewport.
    fn anchor_ref(&mut self, child: &RenderNode) -> Option<String> {
        match child {
            RenderNode::Box { attrs, .. } => {
                let attrs = attrs.clone();
                self.eval_str_prop(&attrs, "anchor_to")
            }
            _ => None,
        }
    }

    /// The `anchor:` token of an overlay child (RFC-0017), or `None` for an
    /// unanchored child (a scrim, which fills the viewport via `grow`).
    fn anchor_token(&mut self, child: &RenderNode) -> Option<String> {
        match child {
            RenderNode::Box { attrs, .. } => Self::enum_prop(attrs, "anchor").map(str::to_string),
            _ => None,
        }
    }

    /// Emits one overlay's children on top of the main scene (RFC-0017 overlay
    /// phase). Clips them to the viewport, installs a modal scrim first when
    /// `modal` (the input barrier + `dismiss` target), then walks each child
    /// through the ordinary render path so it uses every existing pipeline.
    fn emit_overlay(
        &mut self,
        ol: &OverlayLayout<'_>,
        frame: &mut byard_core::frame::RenderFrame,
        width: f32,
        height: f32,
        pools: Pools<'_>,
    ) {
        let RenderNode::Overlay {
            attrs,
            env_snapshot,
            ..
        } = ol.node
        else {
            return;
        };
        let viewport = crate::interp::intrinsics::Rect::new(0.0, 0.0, width, height);
        // `modal` defaults true (RFC-0017 §Modality); `dismiss_on_outside`
        // defaults to whatever `modal` is.
        let modal = self.eval_bool_prop(attrs, "modal").unwrap_or(true);
        let dismiss_on_outside = self
            .eval_bool_prop(attrs, "dismiss_on_outside")
            .unwrap_or(modal);

        // Clamp everything the overlay paints to the viewport (RFC-0017
        // resolved-question: scissor interaction).
        frame.begin_clip(byard_core::frame::Rect::new(0.0, 0.0, width, height));

        // A modal overlay installs its scrim *before* its content so the content
        // wins hit-testing where it overlaps, while the scrim blocks (and
        // optionally dismisses) everything beneath the overlay.
        if modal {
            // Restore the overlay's instance environment so a `dismiss` action
            // referencing an instance `var`/param resolves correctly (RFC-0019).
            let env_base = self.env.len();
            for (k, v) in env_snapshot {
                self.env.push(k.clone(), v.clone());
            }
            let dismiss = if dismiss_on_outside {
                self.lower_overlay_dismiss(attrs)
            } else {
                None
            };
            self.env.truncate(env_base);
            let elem = self.atlas.node_index(ol.wrapper_id).unwrap_or(u32::MAX);
            self.router.push_modal_scrim(elem, viewport, dismiss);
        }

        for slot in &ol.children {
            // RFC-0036: an overlay child naming an anchor is moved from where
            // the viewport wrapper put it onto the anchor's rect. The shift
            // rides both channels — the paint transform and the hit-rect shift
            // — because a dropdown you can see but not click is not placed.
            let shift = self.anchor_shift(slot, width, height);
            let mut transform = byard_core::frame::Transform::IDENTITY;
            transform.translate[0] += shift.0;
            transform.translate[1] += shift.1;

            let mut flat_idx = 0;
            self.render_node_with_atlas(
                slot.node,
                slot.id,
                frame,
                &slot.flat_ids,
                &mut flat_idx,
                viewport,
                1.0,
                transform,
                None,
                shift,
                None,
                pools,
            );
        }

        frame.end_clip();
    }

    /// How far to move an overlay child so it sits against its anchor
    /// (RFC-0036), or `(0, 0)` for a child that names none.
    ///
    /// Returns zero rather than an error when the name is unknown at render
    /// time: the *compile* check is what reports a misspelt or forward
    /// reference, and a frame is the wrong place to raise it again.
    fn anchor_shift(&mut self, slot: &OverlayChildSlot<'_>, width: f32, height: f32) -> (f32, f32) {
        let RenderNode::Box { attrs, .. } = slot.node else {
            return (0.0, 0.0);
        };
        let Some(name) = self.eval_str_prop(attrs, "anchor_to") else {
            return (0.0, 0.0);
        };
        // Compared by text rather than interned. `anchor_to` may be computed,
        // and the interner is process-global and append-only for the life of
        // the process, so interning a per-frame string would grow memory
        // without bound and take the write lock on the logic thread every time
        // a new one appeared. The table holds a handful of entries — one per
        // `as` tag in the view — so the scan is cheaper than the hash would be.
        let Some(anchor) = self
            .anchor_rects
            .iter()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, r)| *r)
        else {
            return (0.0, 0.0);
        };
        let Ok(Some(own)) = self.atlas.resolved_rect(slot.id) else {
            return (0.0, 0.0);
        };
        let edge = Self::enum_prop(attrs, "anchor_edge")
            .unwrap_or("below")
            .to_string();
        let align = Self::enum_prop(attrs, "anchor_align")
            .unwrap_or("start")
            .to_string();
        let gap = self.eval_px_prop(attrs, "anchor_gap").unwrap_or(0.0);
        let flip = self.eval_bool_prop(attrs, "anchor_flip") != Some(false);

        let (tx, ty) = anchor_placement_flipped(
            anchor,
            (own.width, own.height),
            &edge,
            &align,
            gap,
            flip,
            (width, height),
        );
        (tx - own.x, ty - own.y)
    }

    /// Lowers an `Overlay`'s `dismiss =>` action to a router [`Action`], if
    /// present (RFC-0017 §Dismissal). The action runs on scrim tap and on
    /// `Escape`.
    ///
    /// [`Action`]: super::events::Action
    fn lower_overlay_dismiss(&mut self, attrs: &[Attr]) -> Option<super::events::Action> {
        for attr in attrs {
            if attr.name.as_str() == "dismiss" {
                if let AttrKind::Event { payload, action } = &attr.kind {
                    return self.lower_action(action, payload.clone()).ok();
                }
            }
        }
        None
    }

    // ── Canvas shape lowering (RFC-0020) ────────────────────────────────────

    /// The named argument `name` of a shape command, if present.
    fn shape_arg<'e>(el: &'e ElementNode, name: &str) -> Option<&'e Expr> {
        el.content
            .iter()
            .find(|a| a.name.as_ref().is_some_and(|n| n.as_str() == name))
            .map(|a| &a.value)
    }

    /// Evaluates a numeric shape parameter (reactive + `with`-animatable via
    /// `eval_pure`'s animation chokepoint, RFC-0010).
    fn shape_num(&mut self, el: &ElementNode, name: &str) -> Option<f32> {
        Self::shape_arg(el, name).map(|e| {
            let e = e.clone();
            self.eval_num(&e)
        })
    }

    /// Evaluates a shape color parameter. The alpha byte is auto-detected
    /// (the lexer's >6-digit tag, or magnitude for computed values),
    /// matching every alpha-aware colour consumer (RFC-0011).
    fn shape_color(&mut self, el: &ElementNode, name: &str) -> Option<[f32; 4]> {
        let e = Self::shape_arg(el, name)?.clone();
        let packed = self.eval_pure(&e).as_int()?;
        Some(super::intrinsics::color_rgba_auto(packed))
    }

    /// Evaluates a `(a, b)` shape parameter (the `dash` pattern).
    fn shape_vec2(&mut self, el: &ElementNode, name: &str) -> Option<[f32; 2]> {
        let e = Self::shape_arg(el, name)?.clone();
        match self.eval_pure(&e) {
            Value::Tuple(items) if items.len() == 2 => {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let f = |v: &Value| match v {
                    Value::Int(n) => *n as f32,
                    Value::Float(x) => *x as f32,
                    _ => 0.0,
                };
                Some([f(&items[0].1), f(&items[1].1)])
            }
            _ => None,
        }
    }

    /// Reads a bare-token shape parameter (`cap: round`), an enum token is a
    /// syntactic identifier, never an env lookup, mirroring how `align:` and
    /// `fit:` tokens read elsewhere.
    fn shape_token(el: &ElementNode, name: &str) -> Option<String> {
        match Self::shape_arg(el, name)? {
            Expr::Ident(sym, _) | Expr::ClassRef(sym, _) => Some(sym.as_str().to_string()),
            _ => None,
        }
    }

    /// Evaluates a string shape parameter (`path`'s `d`).
    fn shape_string(&mut self, el: &ElementNode, name: &str) -> Option<String> {
        let e = Self::shape_arg(el, name)?.clone();
        match self.eval_pure(&e) {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Emits one shape command into the frame (RFC-0020). `canvas` is the
    /// canvas's resolved rect: shape coordinates are canvas-local and offset
    /// by its origin here. Tier 1 (`arc`/`circle`/`line`/`rect`, plus
    /// `bezier` flattened to line segments) goes to the `CanvasShape`
    /// pipeline; `path` rasterizes through `VectorMSDF` (Tier 2); `text`
    /// lowers to a `TextLine`.
    /// Emits a canvas body, expanding `for`/`when` against the current
    /// environment.
    ///
    /// Bindings are pushed and truncated per iteration rather than snapshotted,
    /// because the whole body runs inside the canvas's already-restored
    /// instance environment, the loop variable is the only thing that changes.
    #[allow(clippy::too_many_arguments)]
    fn emit_canvas_items(
        &mut self,
        items: &[CanvasItem],
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        // `Some` while a grouped `Canvas` is collecting its members
        // (RFC-0031 §S4); `None` for the ordinary one-instance-per-shape path.
        group: Option<&mut ShapeGroupSink>,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        // Reborrowed per item rather than moved, so the same sink threads
        // through nested `for`/`when` bodies.
        let mut group = group;
        for item in items {
            match item {
                CanvasItem::Shape(el) => {
                    self.emit_canvas_shape(
                        el,
                        canvas,
                        opacity,
                        transform,
                        group.as_deref_mut(),
                        frame,
                    );
                }
                CanvasItem::For {
                    var,
                    index,
                    iter,
                    body,
                } => {
                    // Cloned because the body's expressions are evaluated with
                    // `&mut self`, and the list lives in the environment the
                    // evaluation may touch. A canvas's data is a handful of
                    // records, not an app-state list.
                    let Some(items) = self.eval_pure(iter).as_list().map(<[Value]>::to_vec) else {
                        continue;
                    };
                    let base = self.env.len();
                    for (i, value) in items.into_iter().enumerate() {
                        self.env.truncate(base);
                        if let Some(index) = index {
                            self.env.push(
                                index.clone(),
                                Value::Int(i64::try_from(i).unwrap_or(i64::MAX)),
                            );
                        }
                        self.env.push(var.clone(), value);
                        self.emit_canvas_items(
                            body,
                            canvas,
                            opacity,
                            transform,
                            group.as_deref_mut(),
                            frame,
                        );
                    }
                    self.env.truncate(base);
                }
                CanvasItem::When { cond, then, els } => {
                    let taken = if self.eval_pure(cond).as_bool().unwrap_or(false) {
                        then
                    } else {
                        els
                    };
                    self.emit_canvas_items(
                        taken,
                        canvas,
                        opacity,
                        transform,
                        group.as_deref_mut(),
                        frame,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn emit_canvas_shape(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        mut group: Option<&mut ShapeGroupSink>,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{
            CANVAS_CAP_BUTT, CANVAS_CAP_ROUND, CANVAS_CAP_SQUARE, CANVAS_SHAPE_ARC,
            CANVAS_SHAPE_CIRCLE, CANVAS_SHAPE_LINE, CANVAS_SHAPE_NGON, CANVAS_SHAPE_RECT,
            CanvasShape,
        };

        /// One resolved Tier-1 shape: into the group being collected, or
        /// straight onto the frame as its own instance.
        fn emit(
            group: &mut Option<&mut ShapeGroupSink>,
            frame: &mut byard_core::frame::RenderFrame,
            shape: CanvasShape,
        ) {
            match group {
                Some(sink) => sink.push(&shape),
                None => frame.push_canvas_shape(shape),
            }
        }

        let name = el.name.as_str();
        if name == "text" {
            self.emit_canvas_text(el, canvas, opacity, transform, frame);
            return;
        }
        if name == "path" {
            self.emit_canvas_path(el, canvas, opacity, transform, frame);
            return;
        }

        // Shared paint parameters (RFC-0020 §"Stroke and fill"). A shape with
        // neither stroke nor fill paints nothing, skip it entirely.
        let stroke_color = self.shape_color(el, "stroke").unwrap_or([0.0; 4]);
        let fill_color = self.shape_color(el, "fill").unwrap_or([0.0; 4]);
        if stroke_color[3] <= 0.0 && fill_color[3] <= 0.0 {
            return;
        }
        let stroke_width = self.shape_num(el, "stroke_width").unwrap_or(1.0);
        let cap = match Self::shape_token(el, "cap").as_deref() {
            Some("round") => CANVAS_CAP_ROUND,
            Some("square") => CANVAS_CAP_SQUARE,
            _ => CANVAS_CAP_BUTT,
        };
        let dash = self.shape_vec2(el, "dash").unwrap_or([0.0, 0.0]);
        let dash_offset = self.shape_num(el, "dash_offset").unwrap_or(0.0);
        let shape_opacity = opacity * self.shape_num(el, "opacity").unwrap_or(1.0);
        let (ox, oy) = (canvas.x, canvas.y);

        let base = CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [0.0; 8],
            stroke_color,
            fill_color,
            stroke_width,
            cap,
            dash,
            dash_offset,
            opacity: shape_opacity,
            transform,
            dirty: true,
            ..CanvasShape::default()
        };

        match name {
            "arc" | "circle" => {
                let cx = ox + self.shape_num(el, "cx").unwrap_or(0.0);
                let cy = oy + self.shape_num(el, "cy").unwrap_or(0.0);
                let r = self.shape_num(el, "r").unwrap_or(0.0);
                // Angles are authored in degrees (RFC-0020 examples:
                // `start: -90, sweep: 270`); the GPU wants radians. An
                // unswept `arc` defaults to a full circle, `circle` is the
                // explicit sugar for exactly that (RFC-0020 §"Shape commands").
                let start = self.shape_num(el, "start").unwrap_or(0.0);
                let sweep = if name == "circle" {
                    360.0
                } else {
                    self.shape_num(el, "sweep").unwrap_or(360.0)
                };
                let full = sweep.abs() >= 360.0;
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: if full {
                            CANVAS_SHAPE_CIRCLE
                        } else {
                            CANVAS_SHAPE_ARC
                        },
                        params: [
                            cx,
                            cy,
                            r,
                            start.to_radians(),
                            sweep.to_radians(),
                            0.0,
                            0.0,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            "line" => {
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: CANVAS_SHAPE_LINE,
                        params: [
                            ox + self.shape_num(el, "x1").unwrap_or(0.0),
                            oy + self.shape_num(el, "y1").unwrap_or(0.0),
                            ox + self.shape_num(el, "x2").unwrap_or(0.0),
                            oy + self.shape_num(el, "y2").unwrap_or(0.0),
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            "rect" => {
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: CANVAS_SHAPE_RECT,
                        params: [
                            ox + self.shape_num(el, "x").unwrap_or(0.0),
                            oy + self.shape_num(el, "y").unwrap_or(0.0),
                            self.shape_num(el, "w").unwrap_or(0.0),
                            self.shape_num(el, "h").unwrap_or(0.0),
                            self.shape_num(el, "radius").unwrap_or(0.0),
                            // RFC-0031 §S3: the `rect` kind's corner smoothing,
                            // clamped like every other consumer of the property.
                            self.shape_num(el, "smooth").unwrap_or(0.0).clamp(0.0, 1.0),
                            0.0,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            // RFC-0031 §"`ngon`": one parametric kind for the n-fold symmetric
            // half of the expressive vocabulary.
            "ngon" => {
                #[allow(clippy::cast_precision_loss)]
                let n = self.shape_num(el, "n").unwrap_or(3.0).round().max(3.0);
                let r = self.shape_num(el, "r").unwrap_or(0.0).max(0.0);
                emit(
                    &mut group,
                    frame,
                    CanvasShape {
                        kind: CANVAS_SHAPE_NGON,
                        params: [
                            ox + self.shape_num(el, "cx").unwrap_or(0.0),
                            oy + self.shape_num(el, "cy").unwrap_or(0.0),
                            r,
                            // A corner larger than the circumradius has no
                            // meaning; clamped at the one place it is read,
                            // like `radius` on a box.
                            self.shape_num(el, "corner").unwrap_or(0.0).clamp(0.0, r),
                            // `1.0` is the convex regular polygon; below it the
                            // notches pull in towards the centre.
                            self.shape_num(el, "inner").unwrap_or(1.0).clamp(0.0, 1.0),
                            // Already canonicalized to radians by the lexer.
                            self.shape_num(el, "rotate").unwrap_or(0.0),
                            n,
                            0.0,
                        ],
                        ..base
                    },
                );
            }
            "bezier" => {
                // Flattened CPU-side into round-capped line segments on the
                // same Tier-1 pipeline, cheaper and *fully animatable*,
                // unlike an MSDF re-rasterization (see the RFC-0020 notes in
                // the design record). Round caps hide the joints; the curve
                // has no fill.
                if let Some(c) = self.bezier_coords(el) {
                    let p = |t: f32| -> [f32; 2] {
                        let u = 1.0 - t;
                        let b0 = u * u * u;
                        let b1 = 3.0 * u * u * t;
                        let b2 = 3.0 * u * t * t;
                        let b3 = t * t * t;
                        [
                            ox + b0 * c[0] + b1 * c[2] + b2 * c[4] + b3 * c[6],
                            oy + b0 * c[1] + b1 * c[3] + b2 * c[5] + b3 * c[7],
                        ]
                    };
                    // Segment count scales with the control polygon's length:
                    // ~one segment per 6 logical px, clamped to [8, 48].
                    let poly_len = ((c[2] - c[0]).hypot(c[3] - c[1])
                        + (c[4] - c[2]).hypot(c[5] - c[3])
                        + (c[6] - c[4]).hypot(c[7] - c[5]))
                    .max(1.0);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let segments = ((poly_len / 6.0) as usize).clamp(8, 48);
                    let mut prev = p(0.0);
                    #[allow(clippy::cast_precision_loss)]
                    for i in 1..=segments {
                        let next = p(i as f32 / segments as f32);
                        emit(
                            &mut group,
                            frame,
                            CanvasShape {
                                kind: CANVAS_SHAPE_LINE,
                                params: [prev[0], prev[1], next[0], next[1], 0.0, 0.0, 0.0, 0.0],
                                cap: CANVAS_CAP_ROUND,
                                fill_color: [0.0; 4],
                                ..base.clone()
                            },
                        );
                        prev = next;
                    }
                }
            }
            _ => {}
        }
    }

    /// The 8 cubic-bezier coordinates, from either the terse positional form
    /// (`bezier(10, 90, 40, 10, …)`) or the named form (`x1:`, `cy2:`, …).
    fn bezier_coords(&mut self, el: &ElementNode) -> Option<[f32; 8]> {
        const NAMES: [&str; 8] = ["x1", "y1", "cx1", "cy1", "cx2", "cy2", "x2", "y2"];
        let positional: Vec<Expr> = el
            .content
            .iter()
            .filter(|a| a.name.is_none())
            .map(|a| a.value.clone())
            .collect();
        let mut out = [0.0f32; 8];
        if positional.len() == 8 {
            for (slot, expr) in out.iter_mut().zip(&positional) {
                *slot = self.eval_num(expr);
            }
            return Some(out);
        }
        for (slot, name) in out.iter_mut().zip(NAMES) {
            *slot = self.shape_num(el, name)?;
        }
        Some(out)
    }

    /// RFC-0020 §2 Tier 2: a `path(d: …)` command rasterized through the
    /// MSDF pipeline. The synthetic SVG's viewBox equals the canvas size, so
    /// `d` coordinates are canvas-local 1:1; the resulting glyph is drawn
    /// over the whole canvas rect and tinted by `fill`. Content-keyed, so a
    /// re-render of an unchanged path is a pure cache hit; only a genuinely
    /// new `d` (or canvas size) dispatches a generation.
    fn emit_canvas_path(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        // RFC-0037: a `path` with a body is Tier-2, tessellated and filled by
        // the `CanvasFill` pipeline. A `path(d: …)` with no body is Tier-1,
        // baked into the MSDF atlas below. Both are the same command, drawn by
        // whichever pipeline suits the shape: static art amortises a bake,
        // geometry that changes with the data does not.
        if !el.children.is_empty() {
            self.emit_filled_path(el, canvas, opacity, transform, frame);
            return;
        }
        let Some(fill) = self.shape_color(el, "fill") else {
            return; // no fill → nothing to rasterize (stroke is rejected upstream)
        };
        let Some(d) = self.shape_string(el, "d") else {
            return;
        };
        let shape_opacity = opacity * self.shape_num(el, "opacity").unwrap_or(1.0);
        let (w, h) = (canvas.w.max(1.0), canvas.h.max(1.0));

        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            d.hash(&mut hasher);
            w.to_bits().hash(&mut hasher);
            h.to_bits().hash(&mut hasher);
            format!("canvas-path:{:016x}", hasher.finish())
        };
        let glyph = self.vector_jit.lookup_or_dispatch_svg(&key, || {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}"><path d="{d}" fill="#000000"/></svg>"##
            )
            .into_bytes()
        });
        // Cache miss: skip this tick (INV-9, the frame ships without
        // stalling); the generated field lands via the ordinary JIT drain.
        let Some(glyph) = glyph else { return };

        // A `VectorInstance` carries no transform: bake translate/scale into
        // the rect like `Image` does (rotation stays a box-primitive feature).
        let tl = transform.apply_point([canvas.x, canvas.y]);
        let rgb = [fill[0], fill[1], fill[2], fill[3] * shape_opacity];
        frame.push_vector(byard_core::frame::VectorInstance::new(
            byard_core::frame::Rect::new(
                tl[0],
                tl[1],
                canvas.w * transform.scale[0],
                canvas.h * transform.scale[1],
            ),
            glyph.uv_rect,
            rgb,
            glyph.px_range,
            glyph.layer,
        ));
    }

    /// A Tier-2 filled path (RFC-0037): evaluate the commands, tessellate them
    /// if they changed, and push the mesh.
    ///
    /// The evaluation happens every tick, because a command's coordinates are
    /// ordinary expressions and a chart's are ordinary data. The
    /// *tessellation* happens only when the numbers those expressions produced
    /// differ from last time, which is what keeps a live chart inside the
    /// frame budget: the expensive step is the one that is skipped
    /// (INV-23, RFC-0032's dirty model).
    fn emit_filled_path(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let commands = self.eval_path_commands(&el.children, canvas);
        if commands.len() < 2 {
            // One point is not a shape. Silent rather than an error, because
            // an empty `for` over an empty series is a perfectly ordinary
            // frame of a chart that has no data yet.
            return;
        }
        let fill = self.shape_color(el, "fill").unwrap_or([0.0; 4]);
        let gradient = Self::shape_arg(el, "gradient")
            .cloned()
            .and_then(|expr| self.resolve_gradient_expr(&expr, 0.0));
        if gradient.is_none() && fill[3] <= 0.0 {
            return; // nothing to paint
        }
        let even_odd = Self::shape_token(el, "winding").as_deref() == Some("even_odd");

        // The flattening tolerance is derived from the path's on-screen size:
        // a sparkline in a 40px card and a chart across a 4K window are the
        // same commands and want very different triangle counts (RFC-0037).
        let scale = transform.scale[0]
            .abs()
            .max(transform.scale[1].abs())
            .max(0.01);
        let extent = (canvas.w.max(canvas.h) * scale).max(1.0);
        let tolerance = (extent / 800.0).clamp(0.05, 0.5);

        let key = path_fingerprint(&commands, tolerance, even_odd);
        let mesh = if let Some(cached) = self.path_meshes.get_mut(&key) {
            cached.last_used = self.frame_seq;
            std::sync::Arc::clone(&cached.mesh)
        } else {
            let mesh = std::sync::Arc::new(tessellate_path(&commands, tolerance, even_odd));
            self.tessellations += 1;
            self.path_meshes.insert(
                key,
                CachedMesh {
                    mesh: std::sync::Arc::clone(&mesh),
                    last_used: self.frame_seq,
                },
            );
            mesh
        };
        if mesh.indices.is_empty() {
            return;
        }

        let shape_opacity = opacity * self.shape_num(el, "opacity").unwrap_or(1.0);
        frame.push_fill(byard_core::frame::CanvasFill {
            mesh,
            color: fill,
            gradient,
            transform,
            opacity: shape_opacity,
            dirty: true,
        });
    }

    /// Evaluates a path body into absolute points (RFC-0037).
    ///
    /// Absolute, and offset by the canvas' own origin, because everything
    /// downstream of here works in the frame's logical-pixel space and a mesh
    /// that remembered it was relative would have to be re-tessellated every
    /// time its canvas moved.
    fn eval_path_commands(
        &mut self,
        members: &[Member],
        canvas: crate::interp::intrinsics::Rect,
    ) -> Vec<PathCommand> {
        let mut out = Vec::new();
        for member in members {
            let Member::Element(cmd) = member else {
                continue;
            };
            let coords: Vec<f32> = {
                let params = super::intrinsics::path_command_params(cmd.name.as_str());
                let mut values = Vec::with_capacity(params.len());
                for (i, (pname, _)) in params.iter().enumerate() {
                    let expr = cmd
                        .content
                        .iter()
                        .find(|a| a.name.as_ref().is_some_and(|n| n.as_str() == *pname))
                        .or_else(|| cmd.content.iter().filter(|a| a.name.is_none()).nth(i))
                        .map(|a| a.value.clone());
                    values.push(expr.map_or(0.0, |e| self.eval_num(&e)));
                }
                values
            };
            let at = |i: usize| [canvas.x + coords[i], canvas.y + coords[i + 1]];
            match cmd.name.as_str() {
                "move" => out.push(PathCommand::Move(at(0))),
                "line" => out.push(PathCommand::Line(at(0))),
                "quad" => out.push(PathCommand::Quad(at(0), at(2))),
                "cubic" => out.push(PathCommand::Cubic(at(0), at(2), at(4))),
                "close" => out.push(PathCommand::Close),
                _ => {}
            }
        }
        out
    }

    /// How many paths this interpreter has tessellated, ever (RFC-0037,
    /// INV-18/INV-19).
    ///
    /// The number the caching claim is made of: a chart whose data did not
    /// change must not move it. Exposed rather than inferred, because "the
    /// frame was fast" is not evidence that the expensive step was skipped.
    #[must_use]
    pub const fn tessellations(&self) -> u64 {
        self.tessellations
    }

    /// A canvas `text(…)` command: a `TextLine` anchored at `(x, y)` with
    /// optional `align` (start/center/end around `x`), `y` is the vertical
    /// center of the run, matching the RFC's centred-label example.
    fn emit_canvas_text(
        &mut self,
        el: &ElementNode,
        canvas: crate::interp::intrinsics::Rect,
        opacity: f32,
        transform: byard_core::frame::Transform,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let Some(content) = el.content.iter().find(|a| a.name.is_none()) else {
            return;
        };
        let expr = content.value.clone();
        let text = match self.eval_pure(&expr) {
            Value::Str(s) => s,
            other => format!("{other:?}"),
        };
        if text.is_empty() {
            return;
        }
        let size = self.shape_num(el, "size").unwrap_or(self.theme.font_size);
        let color = self
            .shape_color(el, "color")
            .unwrap_or_else(|| super::intrinsics::color_to_rgba(self.theme.on_surface(), false));
        let x = canvas.x + self.shape_num(el, "x").unwrap_or(0.0);
        let y = canvas.y + self.shape_num(el, "y").unwrap_or(0.0);
        let measured = self.measure_text(&text, size).0;
        let tx = match Self::shape_token(el, "align").as_deref() {
            Some("center") => x - measured / 2.0,
            Some("end") => x - measured,
            _ => x,
        };
        // Anchor `y` at the run's vertical center (≈0.6em above the top of
        // the em box reads optically centred for Latin text).
        let ty = y - size * 0.6;
        let anchor = transform.apply_point([tx, ty]);
        frame.push_text(byard_core::TextLine {
            x: anchor[0],
            y: anchor[1],
            text,
            font_size: size * transform.uniform_scale(),
            weight: 400,
            family: None,
            color: dim_alpha(color, opacity),
            dirty: true,
        });
    }

    fn build_layout_tree(
        &mut self,
        node: &RenderNode,
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        use byard_core::atlas::layout::LeafSize;
        match node {
            // Reactive `when`/`for` are expanded to their concrete children by
            // `build_children` before reaching here (RFC-0018), so they never
            // arrive as a single layout node.
            RenderNode::When { .. } | RenderNode::For { .. } => {
                unreachable!("when/for are expanded by build_children before build_layout_tree")
            }
            // An effect has no layout box; `expand_concrete` drops it before
            // the build walk can reach one.
            RenderNode::Effect { .. } => {
                unreachable!("effects are dropped by expand_concrete before build_layout_tree")
            }
            // RFC-0026: a navigation container lays out one wrapper per live
            // screen. The screen the navigation names is in the container's
            // normal flow, so the container measures to it exactly as it would
            // to an ordinary child; a screen that is only transitioning in or
            // out is absolute over the same rect, so the two overlap without
            // either displacing the other or perturbing the measured size.
            RenderNode::Nav { pool, .. } => {
                use byard_core::atlas::layout::{ContainerStyle, FlexDir};
                let Some(p) = pools.navs.get(*pool) else {
                    let id = self.atlas.add_leaf(LeafSize::new(0.0, 0.0))?;
                    flat_ids.push(id);
                    return Ok(id);
                };
                let mut screen_ids = Vec::with_capacity(p.live.len());
                let mut temp_flat = Vec::new();
                for screen in &p.live {
                    let mut child_flat = Vec::new();
                    let child_ids =
                        self.build_children(&p.entries[screen.entry].nodes, pools, &mut child_flat);
                    let style = ContainerStyle::default()
                        .with_direction(FlexDir::Column)
                        .with_grow(1.0)
                        .with_absolute(!screen.in_flow);
                    let screen_id = self.atlas.add_container(style, &child_ids)?;
                    temp_flat.push(screen_id);
                    temp_flat.extend(child_flat);
                    screen_ids.push(screen_id);
                }
                let style = self.eval_container_style("NavStack", &p.attrs);
                let id = self.atlas.add_container(style, &screen_ids)?;
                flat_ids.push(id);
                flat_ids.extend(temp_flat);
                Ok(id)
            }
            RenderNode::Spacer { attrs } => {
                // RFC-0005: a `Spacer` is a *flexible* gap, `grow` (default 1)
                // is its share of the parent's free space, `basis` its size
                // before growing. Both are read through the ordinary prop path,
                // so they are reactive (and animatable) like any other value.
                let grow = self.eval_float_prop(attrs, "grow").unwrap_or(1.0) as f32;
                let basis = self.eval_float_prop(attrs, "basis").unwrap_or(0.0) as f32;
                let id = self.atlas.add_flex_leaf(grow, basis)?;
                flat_ids.push(id);
                Ok(id)
            }
            // RFC-0017: an `Overlay` occupies zero space in its parent's flow,
            // its children are laid out separately against the viewport in the
            // deferred overlay phase. A 0×0 leaf keeps the parallel flat-id
            // cursor aligned without displacing any sibling.
            RenderNode::Overlay { .. } => {
                let id = self.atlas.add_leaf(LeafSize::new(0.0, 0.0))?;
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Image { attrs, .. } => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(100.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(100.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                Ok(id)
            }
            // A `Canvas` is a fixed-size drawing surface (RFC-0020 §1): both
            // dimensions are required (enforced by `validate_canvas`); the 0
            // fallback keeps a mis-declared canvas laid out (collapsed) rather
            // than aborting the whole tree.
            RenderNode::Canvas {
                attrs,
                env_snapshot,
                ..
            } => {
                // Sized from the scope it was instantiated in, like any other
                // element (see the `Box` arm below).
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let w = self.eval_px_prop(attrs, "width").unwrap_or(0.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(0.0);
                self.env.truncate(env_base);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                Ok(id)
            }
            // RFC-0039: a native view is laid out by asking it. The `width`
            // and `height` props, if written, are what layout already knows,
            // and `measure` gets the chance to answer with something else (an
            // intrinsic size) or to keep them.
            //
            // A view that leaves both axes free is a flex leaf rather than a
            // zero-sized one: "fill" is the answer a chart or a map gives, and
            // a leaf of size zero would be an invisible widget with no error
            // anywhere, which is the failure mode INV-4 exists to prevent.
            RenderNode::Native {
                attrs,
                env_snapshot,
                slot,
                ..
            } => {
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let known = byard_core::render::Measure {
                    width: self.eval_px_prop(attrs, "width"),
                    height: self.eval_px_prop(attrs, "height"),
                };
                self.env.truncate(env_base);
                let answer = self
                    .native_views
                    .get(*slot)
                    .map_or(known, |v| v.measure(known));
                let id = match (answer.width, answer.height) {
                    (Some(w), Some(h)) => self.atlas.add_leaf(LeafSize::new(w, h))?,
                    (w, h) => {
                        // One free axis still fills the other: a fixed extent
                        // on the axis that has one, growth on the axis that
                        // does not.
                        let leaf = self.atlas.add_flex_leaf(1.0, 0.0)?;
                        let _ = (w, h);
                        leaf
                    }
                };
                flat_ids.push(id);
                Ok(id)
            }
            // A VectorIcon is a square leaf sized by its `size` prop (default 24),
            // RFC-0009 §1.
            RenderNode::Vector { attrs, .. } => {
                let s = self.eval_int_prop(attrs, "size").unwrap_or(24) as f32;
                let id = self.atlas.add_leaf(LeafSize::new(s, s))?;
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Text { attrs, content, .. } => {
                let text = match self.binding_value(*content) {
                    Some(Value::Str(s)) => s,
                    other => other.map_or_else(String::new, |v| format!("{v:?}")),
                };
                let typo_size = self.eval_typo_size(attrs);
                #[allow(clippy::cast_precision_loss)]
                let font_size = self
                    .eval_int_prop(attrs, "size")
                    .or(typo_size)
                    .unwrap_or(self.theme.font_size as i64) as f32;
                // RFC-0005 default text wrap: `wrap` defaults to `true`. A
                // wrapping `Text` becomes a measured leaf that the atlas sizes to
                // the width its parent offers during layout (via the shared
                // `TextMeasurer`), so it reflows without an explicit `width`. An
                // explicit `width` fixes the wrap width; `wrap: false` opts out to
                // a fixed natural single-line leaf (may overflow, the caller's
                // choice). `fallback` is the natural size for the no-sizer path.
                let weight = self.resolve_weight(attrs);
                let (nat_w, nat_h) =
                    self.measure_text_wrapped(&text, font_size, None, weight, None);
                if self.eval_bool_prop(attrs, "wrap") == Some(false) {
                    let id = self.atlas.add_leaf(LeafSize::new(nat_w, nat_h))?;
                    flat_ids.push(id);
                    return Ok(id);
                }
                #[allow(clippy::cast_precision_loss)]
                let explicit_w = self.eval_px_prop(attrs, "width");
                let id = self.atlas.add_text_leaf(byard_core::atlas::TextLeaf {
                    content: text,
                    font_size,
                    weight,
                    family: None,
                    width: explicit_w,
                    fallback: (nat_w, nat_h),
                })?;
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Box {
                name,
                attrs,
                children,
                env_snapshot,
                measure,
                ..
            } => {
                // The same restore the paint walk does, for the same reason and
                // one pass earlier: a box's *size* is as much a function of the
                // scope it was instantiated in as its colour is. `width: row.w`
                // inside a `for`, a user view's `width: size`, layout reads
                // those attrs through the ordinary prop path, so without the
                // instance environment they resolve to nothing and the element
                // silently falls back to its default (a `width`-less box fills
                // its parent). Empty at the top level (no-op).
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let out = self.build_box_layout(name, attrs, children, pools, flat_ids);
                self.env.truncate(env_base);
                // RFC-0038: pair the element with the node that will hold its
                // resolved rect. Recorded here because this is the one point
                // where both are in hand, and skipped entirely by an element
                // that declared no `on measure`.
                if let (Some(slot), Ok(id)) = (measure, &out) {
                    self.measure_targets.push((*id, *slot));
                }
                out
            }
        }
    }

    /// The `Box`-family layout body, split out so its one caller can bracket it
    /// with the box's restored instance environment across every early return.
    fn build_box_layout(
        &mut self,
        name: &Symbol,
        attrs: &[Attr],
        children: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        use byard_core::atlas::layout::LeafSize;
        // Value widgets are leaf nodes with intrinsic default sizes (M16/M19).
        match name.as_str() {
            "Toggle" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(50.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(30.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            "Slider" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(200.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(24.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            // RFC-0018 Checkbox: an 18×18 square by default; `width`/
            // `height` override. Sized square unless overridden so the
            // container and checkmark geometry stay proportional.
            "Checkbox" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(18.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(18.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            // RFC-0018 RadioButton: a 20×20 circle by default.
            "RadioButton" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(20.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(20.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            "TextField" => {
                let w = self.eval_px_prop(attrs, "width").unwrap_or(200.0);
                let h = self.eval_px_prop(attrs, "height").unwrap_or(36.0);
                let id = self.atlas.add_leaf(LeafSize::new(w, h))?;
                flat_ids.push(id);
                return Ok(id);
            }
            _ => {}
        }
        // RFC-0005 windowed ScrollView: when opted in, build only the
        // visible slice of a single uniform-height list child, bracketed
        // by spacer leaves for the elided rows, so layout is O(visible),
        // not O(list). The same window is recomputed in the render pass.
        if name.as_str() == "ScrollView" {
            if let [
                RenderNode::Box {
                    name: list_name,
                    attrs: list_attrs,
                    children: rows_raw,
                    ..
                },
            ] = children
            {
                // RFC-0018: expand a reactive `for` (or literal rows) to
                // the concrete row nodes, then window over them, so
                // virtualization still lays out only the visible slice.
                let rows = self.expand_concrete(rows_raw, pools);
                if let Some(win) = self.scroll_window(attrs, rows.len()) {
                    let mut temp_flat = Vec::new();
                    let list_id = self.build_windowed_list(
                        list_name,
                        list_attrs,
                        &rows,
                        win,
                        pools,
                        &mut temp_flat,
                    )?;
                    let style = self.eval_container_style(name.as_str(), attrs);
                    let id = self.atlas.add_container(style, &[list_id])?;
                    flat_ids.push(id);
                    flat_ids.extend(temp_flat);
                    return Ok(id);
                }
            }
        }
        // RFC-0018 `Grid`: a CSS-grid container. Built via its own path so
        // the parent gets grid tracks/gaps and each child can carry an
        // explicit placement.
        if name.as_str() == "Grid" {
            return self.build_grid(attrs, children, pools, flat_ids);
        }
        // RFC-0018 `ZStack`: overlapping children, a single-cell grid.
        if name.as_str() == "ZStack" {
            return self.build_zstack(attrs, children, pools, flat_ids);
        }
        let mut temp_flat = Vec::new();
        // RFC-0018: expand reactive `when`/`for` children before layout.
        let child_ids = self.build_children(children, pools, &mut temp_flat);
        let style = self.eval_container_style(name.as_str(), attrs);
        let id = self.atlas.add_container(style, &child_ids)?;
        flat_ids.push(id);
        flat_ids.extend(temp_flat);
        Ok(id)
    }

    /// Builds the atlas subtree for a `Grid` (RFC-0018). Children are expanded
    /// (reactive `when`/`for` first), each built and, if it carries `col`/`row`/
    /// `col_span`/`row_span`, placed explicitly; the rest auto-place. The
    /// container is emitted in the same parent-then-children `flat_ids` order the
    /// generic container path uses, so the render walk's parallel cursor stays
    /// aligned.
    fn build_grid(
        &mut self,
        attrs: &[Attr],
        children: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        let concrete = self.expand_concrete(children, pools);
        let mut child_ids = Vec::with_capacity(concrete.len());
        let mut placements: Vec<(
            byard_core::atlas::layout::AtlasNodeId,
            byard_core::atlas::GridItemPlacement,
        )> = Vec::new();
        let mut temp_flat = Vec::new();
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            if let Ok(cid) = self.build_layout_tree(c.node, pools, &mut temp_flat) {
                if let Some(p) = self.grid_child_placement(c.node) {
                    placements.push((cid, p));
                }
                child_ids.push(cid);
            }
        }
        self.anim_slot = enclosing;
        let base = self.eval_container_style("Grid", attrs);
        let (cols, rows) = self.eval_grid_templates(attrs);
        let (col_gap, row_gap) = self.eval_grid_gaps(attrs);
        let id = self
            .atlas
            .add_grid_container(base, &cols, &rows, col_gap, row_gap, &child_ids)?;
        for (cid, p) in placements {
            // A rejected placement (e.g. a foreign node) is non-fatal, the child
            // simply auto-places, so it never aborts the frame.
            let _ = self.atlas.set_grid_item(cid, p);
        }
        flat_ids.push(id);
        flat_ids.extend(temp_flat);
        Ok(id)
    }

    /// Reads a grid child's explicit placement (`col`/`row`/`col_span`/
    /// `row_span`) from its attrs, or `None` if it carries none (→ auto-placed).
    /// Only `Box`-family children (Box/Column/Row/Grid/…) carry placement.
    fn grid_child_placement(
        &mut self,
        child: &RenderNode,
    ) -> Option<byard_core::atlas::GridItemPlacement> {
        let RenderNode::Box { attrs, .. } = child else {
            return None;
        };
        let col = self.eval_int_prop(attrs, "col");
        let row = self.eval_int_prop(attrs, "row");
        let col_span = self.eval_int_prop(attrs, "col_span");
        let row_span = self.eval_int_prop(attrs, "row_span");
        if col.is_none() && row.is_none() && col_span.is_none() && row_span.is_none() {
            return None;
        }
        Some(byard_core::atlas::GridItemPlacement {
            col_start: col.and_then(|n| i16::try_from(n).ok()),
            col_span: col_span
                .and_then(|n| u16::try_from(n.max(1)).ok())
                .unwrap_or(1),
            row_start: row.and_then(|n| i16::try_from(n).ok()),
            row_span: row_span
                .and_then(|n| u16::try_from(n.max(1)).ok())
                .unwrap_or(1),
        })
    }

    /// Parses a `Grid`'s `columns:`/`rows:` templates (RFC-0018). A missing or
    /// malformed `columns` defaults to a single `auto` track (one column);
    /// missing `rows` leaves rows implicit (Taffy auto-creates them). A malformed
    /// template also pushes a [`CompileError::InvalidGridTemplate`].
    ///
    /// [`CompileError::InvalidGridTemplate`]: crate::diagnostics::CompileError::InvalidGridTemplate
    fn eval_grid_templates(
        &mut self,
        attrs: &[Attr],
    ) -> (
        Vec<byard_core::atlas::GridTrack>,
        Vec<byard_core::atlas::GridTrack>,
    ) {
        let cols = self
            .eval_grid_axis(attrs, "columns")
            .unwrap_or_else(|| vec![byard_core::atlas::GridTrack::Auto]);
        let rows = self.eval_grid_axis(attrs, "rows").unwrap_or_default();
        (cols, rows)
    }

    /// Resolves one grid-template axis attribute to tracks, pushing an
    /// `InvalidGridTemplate` diagnostic on a malformed string. `None` = the
    /// attribute is absent (or not a string).
    fn eval_grid_axis(
        &mut self,
        attrs: &[Attr],
        name: &str,
    ) -> Option<Vec<byard_core::atlas::GridTrack>> {
        let attr = attrs.iter().find(|a| a.name.as_str() == name)?;
        let AttrKind::Prop { value } = &attr.kind else {
            return None;
        };
        let Value::Str(s) = self.eval_pure(value) else {
            return None;
        };
        let parsed = super::intrinsics::parse_grid_template(&s);
        if parsed.is_none() {
            self.errors.push(CompileError::InvalidGridTemplate {
                span: attr.span,
                template: s,
            });
        }
        parsed
    }

    /// Resolves a `Grid`'s per-axis gaps: `col_gap`/`row_gap` each fall back to
    /// the shared `gap` (default 0). Returns `(col_gap, row_gap)`.
    fn eval_grid_gaps(&mut self, attrs: &[Attr]) -> (f32, f32) {
        let gap = self.eval_px_prop(attrs, "gap").unwrap_or(0.0);
        let col_gap = self.eval_px_prop(attrs, "col_gap").unwrap_or(gap);
        let row_gap = self.eval_px_prop(attrs, "row_gap").unwrap_or(gap);
        (col_gap, row_gap)
    }

    /// Builds the atlas subtree for a `ZStack` (RFC-0018): a single-cell grid in
    /// which every child overlaps. Emitted in the same parent-then-children
    /// `flat_ids` order as the generic container path so the render walk's cursor
    /// stays aligned, and rendered through the ordinary Box paint path (bg +
    /// children in declaration order, last on top).
    fn build_zstack(
        &mut self,
        attrs: &[Attr],
        children: &[RenderNode],
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        let concrete = self.expand_concrete(children, pools);
        let mut child_ids = Vec::with_capacity(concrete.len());
        let mut temp_flat = Vec::new();
        let enclosing = self.anim_slot;
        for c in concrete {
            self.anim_slot = c.slot;
            if let Ok(cid) = self.build_layout_tree(c.node, pools, &mut temp_flat) {
                child_ids.push(cid);
            }
        }
        self.anim_slot = enclosing;
        let base = self.eval_container_style("ZStack", attrs);
        let align = self.eval_stack_align(attrs);
        let id = self.atlas.add_stack_container(base, align, &child_ids)?;
        flat_ids.push(id);
        flat_ids.extend(temp_flat);
        Ok(id)
    }

    /// Resolves a `ZStack`'s `alignment` prop to a [`StackAlign`], default
    /// `Center`.
    ///
    /// [`StackAlign`]: byard_core::atlas::StackAlign
    fn eval_stack_align(&mut self, attrs: &[Attr]) -> byard_core::atlas::StackAlign {
        use byard_core::atlas::StackAlign;
        for attr in attrs {
            if attr.name.as_str() != "alignment" {
                continue;
            }
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            if let Some(s) = Self::enum_token(value) {
                return match s {
                    "top_start" => StackAlign::TopStart,
                    "top_end" => StackAlign::TopEnd,
                    "bottom_start" => StackAlign::BottomStart,
                    "bottom_end" => StackAlign::BottomEnd,
                    "top" => StackAlign::Top,
                    "bottom" => StackAlign::Bottom,
                    "start" => StackAlign::Start,
                    "end" => StackAlign::End,
                    _ => StackAlign::Center,
                };
            }
        }
        StackAlign::Center
    }

    /// Builds the atlas subtree for a windowed `ScrollView`'s list child
    /// (RFC-0005): a leading spacer sized to the rows scrolled off the top, the
    /// materialised rows `win.start..win.end`, then a trailing spacer for the
    /// rows below the window. The two spacers preserve the container's content
    /// extent (so the scroll clamp is exact) and every visible row's position,
    /// while only `end − start` rows are ever laid out. `flat_ids` receives the
    /// same `[container, top-spacer, rows…, bottom-spacer]` order the render pass
    /// walks, keeping the parallel cursor aligned.
    fn build_windowed_list(
        &mut self,
        list_name: &Symbol,
        list_attrs: &[Attr],
        rows: &[Concrete<'_>],
        win: WindowSpec,
        pools: Pools<'_>,
        flat_ids: &mut Vec<byard_core::atlas::layout::AtlasNodeId>,
    ) -> Result<byard_core::atlas::layout::AtlasNodeId, byard_core::atlas::AtlasError> {
        use byard_core::atlas::layout::LeafSize;
        #[allow(clippy::cast_precision_loss)]
        let top_h = win.start as f32 * win.row_height;
        #[allow(clippy::cast_precision_loss)]
        let bottom_h = (win.n - win.end) as f32 * win.row_height;

        let mut child_ids = Vec::with_capacity(win.end - win.start + 2);
        let mut temp = Vec::new();
        let top = self.atlas.add_leaf(LeafSize::new(0.0, top_h))?;
        temp.push(top);
        child_ids.push(top);
        let enclosing = self.anim_slot;
        for row in &rows[win.start..win.end] {
            self.anim_slot = row.slot;
            let id = self.build_layout_tree(row.node, pools, &mut temp)?;
            child_ids.push(id);
        }
        self.anim_slot = enclosing;
        let bottom = self.atlas.add_leaf(LeafSize::new(0.0, bottom_h))?;
        temp.push(bottom);
        child_ids.push(bottom);

        let mut style = self.eval_container_style(list_name.as_str(), list_attrs);
        // Rows are positioned purely by `row_height` (spacing folded in); a flex
        // gap would add phantom space around the spacers and desync the window.
        style.gap = 0.0;
        let id = self.atlas.add_container(style, &child_ids)?;
        flat_ids.push(id);
        flat_ids.extend(temp);
        Ok(id)
    }

    #[allow(clippy::similar_names)]
    #[allow(clippy::too_many_arguments)]
    fn render_node_with_atlas(
        &mut self,
        node: &RenderNode,
        atlas_node: byard_core::atlas::layout::AtlasNodeId,
        frame: &mut byard_core::frame::RenderFrame,
        flat_ids: &[byard_core::atlas::layout::AtlasNodeId],
        flat_idx: &mut usize,
        parent_rect: crate::interp::intrinsics::Rect,
        // Opacity inherited from ancestors (RFC-0011 T4 approximation): folded
        // into this element's own `opacity` and multiplied into the alpha of
        // every primitive it emits, so a translucent parent dims its text and
        // widgets too, not only its own background.
        inherited_opacity: f32,
        // Paint-time transform inherited from ancestors (RFC-0011 group
        // transforms): composed with this element's own transform so a scaled or
        // translated container carries its children, text, and widgets with it,
        // not only its own background box. `IDENTITY` at the root.
        inherited_transform: byard_core::frame::Transform,
        // The nearest enclosing `ScrollView` viewport, in screen space (RFC-0005
        // emission culling). A node whose scroll-shifted rect falls entirely
        // outside it is skipped, the scissor already hides such fragments, so
        // this only spares the CPU the emission. `None` outside any scroll
        // container (the whole viewport is live).
        cull_clip: Option<byard_core::frame::Rect>,
        // Accumulated scroll displacement from every enclosing `ScrollView`
        // (RFC-0005), in screen px. Paint applies it through the inherited
        // transform; **hit-testing** cannot ride that path, RFC-0011/INV-8
        // deliberately keeps paint transforms out of hit rects (a hover-scale
        // must not move its own hit target), so the scroll displacement
        // travels separately and shifts every hit rect registered inside the
        // scrolled subtree. This is what keeps a scrolled button interactive
        // at its *on-screen* position, not its laid-out one.
        scroll_shift: (f32, f32),
        // Set only on a windowed `ScrollView`'s list child (RFC-0005 windowed
        // layout): this node renders just rows `start..end`, bracketed by the two
        // spacer leaves the build pass emitted, so the flat-id cursor stays
        // aligned. `None` everywhere else, the ordinary full child walk.
        window: Option<WindowSpec>,
        // RFC-0018: the reactive `for` pools, for expanding `when`/`for` children.
        pools: Pools<'_>,
    ) {
        debug_assert_eq!(flat_ids[*flat_idx], atlas_node);
        *flat_idx += 1;

        match node {
            // Reactive `when`/`for` are expanded to concrete children before the
            // walk reaches them (RFC-0018), so they never arrive as a paint node.
            RenderNode::When { .. } | RenderNode::For { .. } => {
                unreachable!("when/for are expanded before render_node_with_atlas")
            }
            RenderNode::Effect { .. } => {
                unreachable!("effects are dropped by expand_concrete before the paint walk")
            }
            // A `Spacer` is layout-only. An `Overlay` renders nothing in the main
            // flow, its 0×0 leaf holds a slot in the flat-id cursor (already
            // advanced above) while its children are emitted separately in the
            // deferred overlay phase (RFC-0017).
            RenderNode::Spacer { .. } | RenderNode::Overlay { .. } => {}
            RenderNode::Text {
                attrs,
                state_blocks,
                content,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    // RFC-0016: overlay any active `on <state>` block against the
                    // live engine mask before reading paint properties. RFC-0024:
                    // fold in the universal `selected:`/`invalid:` states.
                    let elem_idx = self.atlas.node_index(atlas_node);
                    let state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let attrs = resolve_state_attrs(attrs, state_blocks, state);
                    let attrs = attrs.as_ref();
                    let text = match self.binding_value(*content) {
                        Some(Value::Str(s)) => s,
                        other => other.map_or_else(String::new, |v| format!("{v:?}")),
                    };
                    // M22: fall back to theme on-surface color when unset.
                    let color = self
                        .eval_color_prop(attrs, "color")
                        .unwrap_or(self.theme.on_surface());
                    // Resolve `typo:` token to font size; inline `size:` overrides
                    // (RFC-0005 `Typo`, completed by RFC-0022).
                    let typo_size = self.eval_typo_size(attrs);
                    let size =
                        self.eval_int_prop(attrs, "size")
                            .or(typo_size)
                            .unwrap_or(self.theme.font_size as i64) as f32;
                    let mut rgba = super::intrinsics::color_to_rgba(color, false);
                    rgba[3] *= inherited_opacity;
                    // RFC-0011 group transforms: a `Text` carries no transform of
                    // its own, so an ancestor's scale/translate is baked into the
                    // baseline anchor and the font size (glyph extents scale from
                    // the anchor, so this scales the run about the ancestor pivot).
                    // Rotation can't be baked per-glyph and is left to box
                    // primitives (shader-applied), a documented limitation.
                    let anchor = inherited_transform.apply_point([rect.x, rect.y]);
                    let scaled_size = size * inherited_transform.uniform_scale();
                    // RFC-0005 default text wrap: shape the run to the width layout
                    // resolved for this leaf (its parent-offered width), scaled by
                    // any ancestor scale (the run's glyphs scale about the pivot).
                    // `wrap: false` opts out to a single-line run. This mirrors the
                    // atlas's measure pass, so the rendered line breaks match the
                    // laid-out height.
                    let weight = self.resolve_weight(attrs);
                    let wrap_w = if self.eval_bool_prop(attrs, "wrap") == Some(false) {
                        None
                    } else {
                        Some(rect.width * inherited_transform.uniform_scale())
                    };
                    frame.push_text_wrapped(
                        byard_core::TextLine {
                            x: anchor[0],
                            y: anchor[1],
                            text,
                            font_size: scaled_size,
                            weight,
                            family: None,
                            color: rgba,
                            dirty: true,
                        },
                        wrap_w,
                    );

                    let has_events = attrs
                        .iter()
                        .any(|a| matches!(a.kind, AttrKind::Event { .. }));
                    if has_events {
                        let self_rect = crate::interp::intrinsics::Rect::new(
                            rect.x,
                            rect.y,
                            rect.width,
                            rect.height,
                        );
                        let hit_rect = scrolled_hit_rect(
                            crate::interp::intrinsics::inflate_hit_rect(self_rect, parent_rect),
                            scroll_shift,
                            cull_clip,
                        );
                        self.register_event_attrs(attrs, hit_rect, elem_idx);
                    }
                }
            }
            RenderNode::Box {
                name,
                attrs,
                state_blocks,
                children,
                action,
                bound_sig,
                env_snapshot,
                anchor_name,
                measure: _,
            } => {
                // RFC-0019 §2: restore the instance environment captured at lower
                // time so event actions re-lowered below (a forwarded callback,
                // or any param reference in `attrs`) resolve against the scope
                // this box was instantiated in. Empty at the top level (no-op).
                let env_base = self.env.len();
                for (k, v) in env_snapshot {
                    self.env.push(k.clone(), v.clone());
                }
                let mut current_rect = parent_rect;
                // Opacity children inherit from this box: its effective opacity
                // when it has a resolved rect (set below), else whatever it
                // inherited unchanged.
                let mut child_opacity = inherited_opacity;
                // Likewise the composed paint transform children inherit (RFC-0011
                // group transforms): this box's own transform ∘ its ancestors',
                // set once the rect is known, else passed through unchanged.
                let mut child_transform = inherited_transform;
                // And the accumulated scroll displacement (RFC-0005): grown by
                // this box when it is a `ScrollView`, passed through otherwise.
                let mut child_scroll_shift = scroll_shift;
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    current_rect = crate::interp::intrinsics::Rect::new(
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                    );
                    // RFC-0036: this element is an anchor, so record where it
                    // landed. Recorded from the *painted* rect, transform and
                    // scroll shift included, because an overlay has to point at
                    // where the user sees the element, not where it would have
                    // been without them.
                    if let Some(name) = anchor_name {
                        let tl = inherited_transform.apply_point([rect.x, rect.y]);
                        self.anchor_rects.insert(
                            name.clone(),
                            crate::interp::intrinsics::Rect::new(
                                tl[0] + scroll_shift.0,
                                tl[1] + scroll_shift.1,
                                rect.width * inherited_transform.scale[0],
                                rect.height * inherited_transform.scale[1],
                            ),
                        );
                    }
                    let elem_idx = self.atlas.node_index(atlas_node);
                    // RFC-0012 S5: a `disabled:` element still lays out and paints,
                    // but the router gates every handler it registers below and
                    // reports the `DISABLED` interaction state. Marked here, before
                    // resolving state styles, so an `on disabled { … }` block takes
                    // effect on the very frame the element becomes disabled.
                    if self.eval_bool_prop(attrs, "disabled") == Some(true) {
                        if let Some(idx) = elem_idx {
                            self.router.set_disabled(idx);
                        }
                    }
                    // RFC-0016: overlay any active `on <state>` block over the
                    // base attributes against the live engine `StyleState` mask
                    // *before* reading paint properties. Stateless boxes borrow
                    // `attrs` unchanged (no clone). The base `attrs` still drive
                    // event/handler registration below so hit targets are stable.
                    // RFC-0024: fold the prop/value-driven states (checked,
                    // selected, invalid, indeterminate) into the router's
                    // pointer/focus/drag mask so `on checked { … }` etc. resolve.
                    let paint_state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, *bound_sig, name.as_str()));
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, paint_state);
                    let paint_attrs = paint_attrs.as_ref();
                    // Resolve the paint-time transform once, up front, so it can
                    // be applied both to a plain container's `bg` fill *and* to
                    // the self-owned visuals of `Toggle`/`Slider`/`TextField`
                    // (their track/fill/thumb/underline/caret are the element's
                    // own quads, so RFC-0011's element-local transform applies to
                    // them exactly as it does to a `Box` fill).
                    // The element's own transform, then composed with the one
                    // inherited from its ancestors (RFC-0011 group transforms) so
                    // this box's fill, its widget visuals, its children, and its
                    // text all move/scale as a group. Passed on to children below.
                    let own_transform = self.resolve_transform(paint_attrs, current_rect);
                    let transform = inherited_transform.compose(&own_transform);
                    child_transform = transform;
                    let bg = self.eval_color_prop(paint_attrs, "bg");
                    let radii = self.resolve_radii(paint_attrs, "radius");
                    // RFC-0031 §S1: the corner profile `radii` are measured
                    // with. Paint-class, so it never touches layout, and it
                    // reaches the fill, the border, every shadow, the backdrop
                    // pane and the ripple clip from this one read, §Q2's
                    // "a shadow with a different corner profile than its caster
                    // reads as a rendering bug", applied to the whole element.
                    let smooth = self.resolve_smooth(paint_attrs);
                    // `border` is a Color (catalog DECORATION); a present border
                    // draws a 2px ring of that colour.
                    let border_color = self.eval_color_prop(paint_attrs, "border");
                    // `border_width` is an animatable paint prop (RFC-0010): it
                    // resolves through `eval_pure`, so `border_width: n with
                    // anim.*` interpolates like any other scalar. Defaults to 2px
                    // when a border colour is present, 0 when there is no border.
                    let border_width = if border_color.is_some() {
                        self.eval_float_prop(paint_attrs, "border_width")
                            .map_or(2.0, |v| v as f32)
                    } else {
                        0.0
                    };
                    // `shadow` is a token (`sm`/`md`/`lg`) → an offset+blur drop
                    // shadow; any other non-empty value falls back to `md`.
                    // `shadow` (RFC-0011 custom shadows): a preset token, a
                    // single (named/positional) tuple, or an array of tuples for
                    // CSS-style layered shadows. Each becomes its own shadow-only
                    // decorated box beneath the surface.
                    let shadows = self.resolve_shadows(paint_attrs);
                    // The element's *effective* opacity: its own `opacity` prop
                    // folded with whatever it inherited (RFC-0011 T4). Used for
                    // this box's own fill and passed down so children (a Button's
                    // label, a widget's visuals) dim with it.
                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(paint_attrs, "opacity")
                            .map_or(1.0, |v| v as f32);
                    child_opacity = opacity;
                    let translucent = (opacity - 1.0).abs() > f32::EPSILON;
                    // RFC-0001 §3.1: a gradient is a `DecoratedBox` feature, so
                    // its presence promotes the box off the flat SolidBox path
                    // exactly as a border/shadow/opacity does.
                    let gradient = self.resolve_gradient(paint_attrs);
                    // `Toggle`/`Slider` own their visuals (track/fill/thumb) and
                    // treat `bg` as the *accent* colour, not a full-rect fill,
                    // painting the rect here would draw a slab behind the control.
                    let owns_visuals = matches!(
                        name.as_str(),
                        "Toggle" | "Slider" | "Checkbox" | "RadioButton"
                    );
                    // A gradient is a fill in its own right, so a box with a ramp
                    // and no `bg` still has a surface to paint (the ramp over a
                    // transparent base).
                    if !owns_visuals && (bg.is_some() || gradient.is_some()) {
                        let base = byard_core::BoxInstance {
                            rect: [rect.x, rect.y, rect.width, rect.height],
                            color: bg
                                .map_or([0.0; 4], |c| super::intrinsics::color_to_rgba(c, false)),
                            radii,
                            transform,
                            smooth,
                        };
                        let border_rgba = border_color
                            .map_or([0.0; 4], |c| super::intrinsics::color_to_rgba(c, false));
                        // Cast the shadows first so they sit *beneath* the fill.
                        // Reversed: first-listed is pushed last → nearest z → on
                        // top of later shadows (CSS box-shadow order), all still
                        // behind the surface pushed after them.
                        for sh in shadows.iter().rev() {
                            frame.push_decorated(shadow_decorated(base, opacity, sh));
                        }
                        if translucent || gradient.is_some() {
                            // A translucent or gradient-filled box blends its fill
                            // as one unit on the decorated pipeline; keep it whole.
                            frame.push_decorated(byard_core::frame::DecoratedBox {
                                base,
                                border_width,
                                border_color: border_rgba,
                                opacity,
                                gradient,
                                // Re-walked and re-emitted every tick;
                                // mirror Text's always-dirty lowering.
                                dirty: true,
                                ..Default::default()
                            });
                        } else if border_color.is_some() {
                            // Paint the opaque fill on the SolidBox pass so it stays
                            // *behind* this container's children (they also paint as
                            // solids, pushed after it, and the decorated pass runs
                            // after every solid). Then add the border as a decorated
                            // overlay whose interior is transparent: it only strokes
                            // the edge, so it can never occlude the children drawn
                            // beneath it (fixes the parent-card-over-child-widget
                            // z-order bug).
                            frame.push_instance(base);
                            frame.push_decorated(byard_core::frame::DecoratedBox {
                                base: byard_core::BoxInstance {
                                    color: [0.0; 4],
                                    ..base
                                },
                                border_width,
                                border_color: border_rgba,
                                opacity: 1.0,
                                dirty: true,
                                ..Default::default()
                            });
                        } else {
                            frame.push_instance(base);
                        }
                    }

                    // RFC-0023 §2: backdrop blur, emitted right after this
                    // element's background (the §4 compositing slot), so the
                    // pane samples everything painted behind it, its own
                    // background included, and its children render on top.
                    self.emit_backdrop(
                        paint_attrs,
                        current_rect,
                        radii,
                        smooth,
                        transform,
                        opacity,
                        frame,
                    );

                    // RFC-0023: ripple ink, emitted after this element's
                    // background and before its children, which stamps its
                    // draw-order depth into exactly the RFC's compositing slot
                    // (background → ripple → children).
                    self.emit_ripples(
                        paint_attrs,
                        elem_idx,
                        current_rect,
                        radii,
                        smooth,
                        transform,
                        opacity,
                        scroll_shift,
                        frame,
                    );

                    let element_name = name.as_str();
                    // Inflate to the 44×44 slop, then move the target to where
                    // the content actually is on screen (RFC-0005 scroll shift)
                    // and clip it to the scroll viewport.
                    let hit_rect = scrolled_hit_rect(
                        crate::interp::intrinsics::inflate_hit_rect(current_rect, parent_rect),
                        scroll_shift,
                        cull_clip,
                    );

                    // RFC-0016: an element that styles `on hover`/`on pressed` but
                    // registers no handler still needs the engine to track the
                    // pointer over it, so register a bare hover/press hit region.
                    // RFC-0023: same for a `ripple:` element, the press gesture
                    // must resolve to this element for the ink to spawn, even
                    // when it registers no handler of its own.
                    if let Some(idx) = elem_idx {
                        let tracks_pointer_states = state_blocks.iter().any(|sb| {
                            sb.states.iter().any(|s| {
                                matches!(s, StyleStateKind::Hover | StyleStateKind::Pressed)
                            })
                        });
                        let has_ripple = paint_attrs.iter().any(|a| a.name.as_str() == "ripple");
                        if tracks_pointer_states || has_ripple {
                            self.router.track_region(idx, hit_rect);
                        }
                    }

                    // ── Widget-specific visual lowering & handler registration (M16/M19) ──
                    match element_name {
                        "Toggle" => {
                            self.render_toggle(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "Slider" => {
                            self.render_slider(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "Checkbox" => {
                            self.render_checkbox(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "RadioButton" => {
                            self.render_radio(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        "TextField" => {
                            self.render_text_field(
                                *bound_sig,
                                paint_attrs,
                                current_rect,
                                hit_rect,
                                elem_idx,
                                transform,
                                opacity,
                                frame,
                            );
                        }
                        _ => {
                            // General interactive elements: register event-attr handlers.
                            let has_event_attrs = attrs
                                .iter()
                                .any(|a| matches!(a.kind, AttrKind::Event { .. }));
                            let is_interactive = matches!(element_name, "Button")
                                || has_event_attrs
                                || action.is_some();

                            if is_interactive {
                                self.register_event_attrs(attrs, hit_rect, elem_idx);

                                if let Some(action_expr) = action {
                                    if let Ok(action_closure) = self.lower_action(action_expr, None)
                                    {
                                        if let Some(idx) = elem_idx {
                                            self.router.on(
                                                idx,
                                                hit_rect,
                                                crate::interp::events::EventKind::Tap,
                                                action_closure,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // ── `focused:` reflected prop → register as focusable (M16/M18) ──
                    // TextField, Checkbox, and RadioButton register their own
                    // focusable inside their render fns (they are focusable *by
                    // default*, RFC-0018), so exclude them here to avoid
                    // double-registration.
                    if !matches!(element_name, "TextField" | "Checkbox" | "RadioButton") {
                        self.register_focusable(attrs, hit_rect, elem_idx);
                    }
                }
                // RFC-0021 collapsing header: when set, the first child is pinned
                // to the viewport top by this many screen px (undoing the scroll
                // translate), so it stays put while the content scrolls under it.
                let mut header_pin: Option<f32> = None;
                // RFC-0005 `ScrollView`: clip children to this viewport and
                // translate the content by `−offset`. The overflow is scissored
                // by the encoder (an off-viewport child costs no fragments), and
                // the content was measured unbounded (layout `scroll`). `offset`
                // is a two-way `Vec2` the app can read or drive on either axis;
                // wheel and drag write it below. Rotation of a scroll viewport is
                // out of scope (the clip is an axis-aligned screen rect).
                // RFC-0037 clip masks: `Clip #[rrect: r]` clips its subtree
                // to its own laid-out box. A zero (or absent) radius is a
                // plain rectangular clip, which costs exactly what a
                // `ScrollView`'s does — a scissor — so wrapping content in a
                // square `Clip` is not a new expense.
                let mask_clip = if name.as_str() == "Clip" {
                    let tl = inherited_transform.apply_point([current_rect.x, current_rect.y]);
                    let rect = byard_core::frame::Rect::new(
                        tl[0],
                        tl[1],
                        current_rect.w * inherited_transform.scale[0],
                        current_rect.h * inherited_transform.scale[1],
                    );
                    // Read the same way a box's `radius` is, so `rrect: 16`
                    // and `rrect: (16, 0, 0, 16)` both mean here what they
                    // already mean there. The radius scales with the subtree,
                    // as a decorated box's corners do: a scaled card's clip
                    // has to keep matching the card.
                    let mut radii = self.resolve_radii(attrs, "rrect");
                    for r in &mut radii {
                        *r *= inherited_transform.scale[0];
                    }
                    frame.begin_clip_rounded(rect, radii);
                    Some(())
                } else {
                    None
                };
                let scroll_clip = if name.as_str() == "ScrollView" {
                    let (ox, oy) = self.resolve_axis_pair(attrs, "offset", (0.0, 0.0));
                    let tl = inherited_transform.apply_point([current_rect.x, current_rect.y]);
                    let clip = byard_core::frame::Rect::new(
                        tl[0],
                        tl[1],
                        current_rect.w * inherited_transform.scale[0],
                        current_rect.h * inherited_transform.scale[1],
                    );
                    frame.begin_clip(clip);
                    child_transform.translate[0] -= ox * inherited_transform.scale[0];
                    child_transform.translate[1] -= oy * inherited_transform.scale[1];
                    // Hit rects travel with the content (paint rides the
                    // transform above; hit-testing rides this, see the
                    // `scroll_shift` parameter docs).
                    child_scroll_shift.0 -= ox * inherited_transform.scale[0];
                    child_scroll_shift.1 -= oy * inherited_transform.scale[1];
                    // RFC-0021 pull-to-refresh: shift the content down by the pull
                    // region and draw the default indicator in the revealed gap.
                    let pull_elem = self.atlas.node_index(atlas_node);
                    let pull = pull_elem
                        .and_then(|e| self.pull_distance.get(&e).copied())
                        .unwrap_or(0.0);
                    if pull > 0.0 {
                        let gap_px = pull * inherited_transform.scale[1];
                        child_transform.translate[1] += gap_px;
                        child_scroll_shift.1 += gap_px;
                        let progress = (pull / PULL_THRESHOLD).clamp(0.0, 1.0);
                        let active = pull_elem
                            .and_then(|e| self.refreshing_seen.get(&e).copied())
                            .unwrap_or(false);
                        Self::push_pull_indicator(frame, clip, gap_px, progress, active);
                    }
                    // RFC-0021 collapsing header: drive `scroll_fraction` (0 →
                    // expanded, 1 → collapsed) from the vertical scroll over the
                    // header's collapsible range (its natural height minus
                    // `collapse_min`), and pin the header to the viewport top so it
                    // stays while the content scrolls under it. The header keeps its
                    // laid-out height; its descendants read `scroll_fraction` to
                    // interpolate their own size/opacity (RFC-0021 §4 mechanism).
                    if let Some(frac_sig) = *bound_sig {
                        let h_max = self
                            .atlas
                            .children(atlas_node)
                            .first()
                            .and_then(|h| self.atlas.resolved_rect(*h).ok().flatten())
                            .map_or(0.0, |r| r.height);
                        #[allow(clippy::cast_possible_truncation)]
                        let c_min = self
                            .eval_float_prop(attrs, "collapse_min")
                            .map_or(56.0, |v| v as f32);
                        let dist = (h_max - c_min).max(1.0);
                        let frac = (oy / dist).clamp(0.0, 1.0);
                        if !matches!(
                            self.ctx.peek_signal(frac_sig),
                            Value::Float(f) if (f - f64::from(frac)).abs() < 1e-4
                        ) {
                            self.ctx
                                .write_signal(frac_sig, Value::Float(f64::from(frac)));
                        }
                        header_pin = Some(oy * inherited_transform.scale[1]);
                    }
                    // Record a wheel/drag scroll target for whichever of
                    // `offset.x`/`offset.y` is a writable signal (e.g.
                    // `offset: (panX, scrollY)`): the input scrolls by writing it,
                    // clamped to the content extent. `dispatch_events` consumes
                    // these next tick (render-then-dispatch handshake).
                    let (sig_x, sig_y) = self.resolve_offset_sigs(attrs);
                    // A `pull_refresh` view needs a target even with no `offset`
                    // var, its pull region is engine state, driven by the drag.
                    let pull_refresh = self.eval_bool_prop(attrs, "pull_refresh").unwrap_or(false);
                    if sig_x.is_some() || sig_y.is_some() || pull_refresh {
                        let (content_w, content_h) = self
                            .atlas
                            .content_size(atlas_node)
                            .ok()
                            .flatten()
                            .unwrap_or((current_rect.w, current_rect.h));
                        let refreshing_sig = self.resolve_named_var_sig(attrs, "refreshing");
                        // RFC-0021 behaviours resolved from props.
                        let snap = match Self::enum_prop(attrs, "snap") {
                            Some("page") => SnapMode::Page,
                            Some("item") => SnapMode::Item,
                            _ => SnapMode::None,
                        };
                        let snap_align = match Self::enum_prop(attrs, "snap_align") {
                            Some("center") => SnapAlign::Center,
                            Some("end") => SnapAlign::End,
                            _ => SnapAlign::Start,
                        };
                        let snap_spring = self.resolve_snap_spring(attrs);
                        let page_sig = self.resolve_named_var_sig(attrs, "page");
                        let has_end = attrs.iter().any(|a| {
                            a.name.as_str() == "end_reached"
                                && matches!(a.kind, AttrKind::Event { .. })
                        });
                        #[allow(clippy::cast_possible_truncation)]
                        let end_threshold = has_end.then(|| {
                            self.eval_float_prop(attrs, "end_threshold")
                                .map_or(0.8, |v| v as f32)
                        });
                        let sv_elem = self.atlas.node_index(atlas_node);
                        let x_max = (content_w - current_rect.w).max(0.0);
                        let y_max = (content_h - current_rect.h).max(0.0);
                        // RFC-0021 `snap: item`: precompute the rest offset for each
                        // direct child on the scrolling axis (vertical preferred,
                        // matching `scrollable_axis`), aligned per `snap_align`.
                        if let (Some(elem), SnapMode::Item) = (sv_elem, snap) {
                            let horizontal = y_max <= 0.0 && x_max > 0.0;
                            let (vp, axis_max) = if horizontal {
                                (current_rect.w, x_max)
                            } else {
                                (current_rect.h, y_max)
                            };
                            let bounds = self.item_snap_offsets(
                                atlas_node, horizontal, vp, axis_max, snap_align,
                            );
                            self.scroll_item_bounds.insert(elem, bounds);
                        }
                        self.scroll_targets.push(ScrollTarget {
                            rect: crate::interp::intrinsics::Rect::new(
                                clip.x,
                                clip.y,
                                clip.width,
                                clip.height,
                            ),
                            x: sig_x.map(|sig| ScrollAxis { sig, max: x_max }),
                            y: sig_y.map(|sig| ScrollAxis { sig, max: y_max }),
                            elem: sv_elem,
                            snap,
                            snap_spring,
                            page_sig,
                            end_threshold,
                            pull_refresh,
                            refreshing_sig,
                        });
                    }
                    Some(clip)
                } else {
                    None
                };
                // Children cull against this box's own scroll viewport when it is
                // a `ScrollView`, otherwise against whatever viewport an ancestor
                // `ScrollView` established, so rows nested under an inner `Column`
                // are culled too, not just the `ScrollView`'s direct child.
                let child_clip = scroll_clip.or(cull_clip);
                // A windowed `ScrollView` hands its computed row window to its
                // single list child (mirrors the build pass); nothing else
                // propagates one.
                let child_window = if name.as_str() == "ScrollView" {
                    match children.as_slice() {
                        [RenderNode::Box { children: rows, .. }] => {
                            // RFC-0018: count the expanded rows (a reactive `for`
                            // is one node that expands to N), mirroring the build.
                            let n = self.expand_concrete(rows, pools).len();
                            self.scroll_window(attrs, n)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let render_child = |this: &mut Self,
                                    child: Concrete<'_>,
                                    frame: &mut byard_core::frame::RenderFrame,
                                    flat_idx: &mut usize| {
                    // The instance this child belongs to, so every `with`
                    // it evaluates keys to its own row rather than to the
                    // written span shared by all of them.
                    this.anim_slot = child.slot;
                    let child = child.node;
                    let child_id = flat_ids[*flat_idx];
                    // RFC-0005 emission culling (north star): a child the
                    // scroll has pushed entirely out of the viewport is never
                    // pushed to the frame, a long list costs only its visible
                    // slice. Advance the cursor past the skipped subtree so the
                    // remaining children stay aligned.
                    if let Some(clip) = child_clip {
                        if this.child_fully_clipped(child_id, child_transform, clip) {
                            *flat_idx += this.flat_len(child, pools);
                            return;
                        }
                    }
                    this.render_node_with_atlas(
                        child,
                        child_id,
                        frame,
                        flat_ids,
                        flat_idx,
                        current_rect,
                        child_opacity,
                        child_transform,
                        child_clip,
                        child_scroll_shift,
                        child_window,
                        pools,
                    );
                };
                if let Some(win) = window {
                    // This box is a windowed list child (RFC-0005): the build pass
                    // wrapped its rows in a leading + trailing spacer leaf. Consume
                    // the leading spacer, render only rows `start..end`, then the
                    // trailing spacer, keeping the flat-id cursor in lockstep.
                    *flat_idx += 1;
                    // RFC-0018: expand a reactive `for` (or literal rows) and
                    // paint only the windowed slice, mirroring the build pass.
                    let rows = self.expand_concrete(children, pools);
                    for &row in &rows[win.start..win.end] {
                        render_child(self, row, frame, flat_idx);
                    }
                    *flat_idx += 1;
                } else if let Some(pin) = header_pin {
                    // RFC-0021 collapsing header: the first child (the header) is
                    // pinned to the viewport top (its scroll translate undone on Y)
                    // *and* drawn last so it paints on top of the content that
                    // scrolls up under it, draw-order depth is emission order, so a
                    // header emitted first would sit behind the content. Skip the
                    // header's flat ids, paint the rest, then rewind and paint the
                    // header over them.
                    let concrete = self.expand_concrete(children, pools);
                    if let Some((&header, rest)) = concrete.split_first() {
                        let header_start = *flat_idx;
                        *flat_idx += self.flat_len(header.node, pools);
                        for &child in rest {
                            render_child(self, child, frame, flat_idx);
                        }
                        let after = *flat_idx;
                        *flat_idx = header_start;
                        let mut pin_tf = child_transform;
                        pin_tf.translate[1] += pin;
                        let child_id = flat_ids[*flat_idx];
                        // Bind `scroll_fraction` in the *render* env so the header's
                        // prop expressions (`opacity: 1.0 - scroll_fraction`, …)
                        // resolve it to the live signal, `eval_pure` re-lowers
                        // against the current env each frame, so the lower-time scope
                        // alone isn't enough. Truncated right after (header-only).
                        let fenv = self.env.len();
                        if let Some(fs) = *bound_sig {
                            self.env
                                .push(Symbol::intern("scroll_fraction"), Value::Signal(fs));
                        }
                        self.anim_slot = header.slot;
                        self.render_node_with_atlas(
                            header.node,
                            child_id,
                            frame,
                            flat_ids,
                            flat_idx,
                            current_rect,
                            child_opacity,
                            pin_tf,
                            child_clip,
                            // The pin undoes the vertical scroll translate, so
                            // the header's hit rects stay at the viewport top
                            // exactly like its pixels do.
                            (child_scroll_shift.0, child_scroll_shift.1 + pin),
                            child_window,
                            pools,
                        );
                        self.env.truncate(fenv);
                        *flat_idx = after;
                    }
                } else {
                    // RFC-0018: expand reactive `when`/`for` into concrete children
                    // in the same order the layout pass did.
                    for child in self.expand_concrete(children, pools) {
                        render_child(self, child, frame, flat_idx);
                    }
                }
                if scroll_clip.is_some() {
                    frame.end_clip();
                }
                if mask_clip.is_some() {
                    frame.end_clip();
                }
                // Close the RFC-0019 instance-env scope opened at the top of this
                // arm (balanced with `env_base`), restoring the caller's env for
                // the remaining siblings.
                self.env.truncate(env_base);
            }
            // RFC-0026: a navigation container. Its own surface is an ordinary
            // background fill; the interesting part is the per-screen transform
            //, the transition's whole cost is two `f32` offsets and an alpha,
            // composed into the transform every subtree already inherits, so a
            // screen sliding in costs no relayout and no extra pass (INV-8).
            RenderNode::Nav { pool, .. } => {
                let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) else {
                    return;
                };
                let Some(p) = pools.navs.get(*pool) else {
                    return;
                };
                let nav_rect =
                    crate::interp::intrinsics::Rect::new(rect.x, rect.y, rect.width, rect.height);
                let elem_idx = self.atlas.node_index(atlas_node);
                let state = elem_idx
                    .map_or_else(crate::interp::events::StyleState::empty, |i| {
                        self.router.style_state(i)
                    })
                    .union(self.prop_style_state(&p.attrs, None, ""));
                let paint_attrs = resolve_state_attrs(&p.attrs, &p.state_blocks, state);
                let paint_attrs = paint_attrs.as_ref();
                let own_transform = self.resolve_transform(paint_attrs, nav_rect);
                let transform = inherited_transform.compose(&own_transform);
                let opacity = inherited_opacity
                    * self
                        .eval_float_prop(paint_attrs, "opacity")
                        .map_or(1.0, |v| v as f32);
                if let Some(bg) = self.eval_color_prop(paint_attrs, "bg") {
                    frame.push_instance(byard_core::BoxInstance {
                        rect: [rect.x, rect.y, rect.width, rect.height],
                        color: dim_alpha(super::intrinsics::color_to_rgba(bg, false), opacity),
                        radii: self.resolve_radii(paint_attrs, "radius"),
                        transform,
                        smooth: self.resolve_smooth(paint_attrs),
                    });
                }
                // `route_change` and any pointer handlers on the container.
                let hit_rect = scrolled_hit_rect(nav_rect, scroll_shift, cull_clip);
                self.register_event_attrs(&p.attrs, hit_rect, elem_idx);

                // A screen mid-transition is partly outside the container, so
                // everything it paints is clipped to the container's bounds,
                // otherwise a sliding screen would smear across its siblings.
                let clip = byard_core::frame::Rect::new(
                    transform.apply_point([nav_rect.x, nav_rect.y])[0],
                    transform.apply_point([nav_rect.x, nav_rect.y])[1],
                    nav_rect.w * transform.scale[0],
                    nav_rect.h * transform.scale[1],
                );
                frame.begin_clip(clip);
                for screen in &p.live {
                    let motion = p.transition.screen_motion(
                        p.progress,
                        p.popping,
                        screen.incoming,
                        nav_rect.w,
                        nav_rect.h,
                    );
                    let mut screen_transform = transform;
                    screen_transform.translate[0] += motion.dx * transform.scale[0];
                    screen_transform.translate[1] += motion.dy * transform.scale[1];
                    // Hit rects ride their own channel (RFC-0011/INV-8 keep
                    // paint transforms out of hit-testing), so the same offset
                    // travels separately, a half-slid screen is tappable
                    // exactly where it is drawn.
                    let screen_shift = (
                        scroll_shift.0 + motion.dx * transform.scale[0],
                        scroll_shift.1 + motion.dy * transform.scale[1],
                    );
                    let screen_id = flat_ids[*flat_idx];
                    *flat_idx += 1;
                    let screen_rect = self
                        .atlas
                        .resolved_rect(screen_id)
                        .ok()
                        .flatten()
                        .map_or(nav_rect, |r| {
                            crate::interp::intrinsics::Rect::new(r.x, r.y, r.width, r.height)
                        });
                    for child in self.expand_concrete(&p.entries[screen.entry].nodes, pools) {
                        let child_id = flat_ids[*flat_idx];
                        self.anim_slot = child.slot;
                        self.render_node_with_atlas(
                            child.node,
                            child_id,
                            frame,
                            flat_ids,
                            flat_idx,
                            screen_rect,
                            opacity * motion.opacity,
                            screen_transform,
                            Some(clip),
                            screen_shift,
                            None,
                            pools,
                        );
                    }
                }
                frame.end_clip();
                // The element index a settled `route_change` fires against.
                if let Some(nav) = self.nav_elems.get_mut(*pool) {
                    *nav = elem_idx;
                }
                if p.swipe_back {
                    self.nav_targets.push((hit_rect, *pool));
                }
            }
            RenderNode::Image {
                attrs,
                state_blocks,
                src,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    // RFC-0016: overlay active `on <state>` blocks before reading
                    // paint properties (fit/radius/opacity). RFC-0024: fold in the
                    // universal `selected:`/`invalid:` states.
                    let state = self
                        .atlas
                        .node_index(atlas_node)
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let attrs = resolve_state_attrs(attrs, state_blocks, state);
                    let attrs = attrs.as_ref();
                    let src_val = self
                        .binding_value(*src)
                        .and_then(|v| if let Value::Str(s) = v { Some(s) } else { None })
                        .unwrap_or_default();
                    let fit = self.eval_fit_prop(attrs);
                    let radii = self.resolve_radii(attrs, "radius");
                    let smooth = self.resolve_smooth(attrs);
                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(attrs, "opacity")
                            .map_or(1.0, |v| v as f32);
                    // RFC-0011 group transforms: an `Image` carries no transform
                    // field, so an ancestor's scale/translate is baked into its
                    // rect (top-left through the transform, extents scaled per
                    // axis). Rotation isn't representable here and is left to box
                    // primitives, same limitation as `Text`.
                    let tl = inherited_transform.apply_point([rect.x, rect.y]);
                    let tw = rect.width * inherited_transform.scale[0];
                    let th = rect.height * inherited_transform.scale[1];
                    frame.push_texture(byard_core::frame::TextureSampler {
                        rect: [tl[0], tl[1], tw, th],
                        src: src_val,
                        fit,
                        radii,
                        // RFC-0031 §S3: an image's rounded clip follows the same
                        // corner profile as the boxes around it.
                        smooth,
                        opacity,
                        // Re-emitted every tick; mirror Text's
                        // always-dirty lowering.
                        dirty: true,
                    });
                }
            }
            // RFC-0039: hand the view its rect and let it draw. Props are
            // re-evaluated every tick through the same `eval_*` chokepoints an
            // intrinsic's are, so a native view animates and reacts for free
            // (RFC-0010), and the view itself never learns that a value came
            // from a signal.
            RenderNode::Native {
                name,
                attrs,
                state_blocks,
                action,
                env_snapshot,
                slot,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    let elem_idx = self.atlas.node_index(atlas_node);
                    let state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, state);
                    let paint_attrs = paint_attrs.as_ref();

                    // RFC-0019 §2: prop expressions resolve against the scope
                    // the element was written in.
                    let env_base = self.env.len();
                    for (k, v) in env_snapshot {
                        self.env.push(k.clone(), v.clone());
                    }
                    self.apply_native_props(name.as_str(), *slot, paint_attrs);
                    self.env.truncate(env_base);

                    let view_rect = crate::interp::intrinsics::Rect::new(
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                    );
                    if let Some(view) = self.native_views.get_mut(*slot) {
                        let outcome = frame.render_native(
                            view.as_mut(),
                            byard_core::render::Layout::new([
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ]),
                        );
                        if outcome.repaint {
                            // The view is animating from state the engine
                            // cannot see, so it says so once per frame rather
                            // than holding a subscription open (RFC-0032).
                            frame.request_full_redraw();
                        }
                        // Whose calls these are is known here and nowhere
                        // else: a view is handed a context, never an identity
                        // (RFC-0039).
                        if !outcome.calls.is_empty() {
                            self.native_call_owners.push((*slot, outcome.calls));
                        }
                    }

                    // Events: the view's rect, under the same hit-rect rules an
                    // intrinsic follows (RFC-0003 E8 inflation included), and
                    // recorded so `dispatch_events` can offer it the raw event
                    // its `on_event` expects.
                    let hit_rect = scrolled_hit_rect(
                        crate::interp::intrinsics::inflate_hit_rect(view_rect, parent_rect),
                        scroll_shift,
                        cull_clip,
                    );
                    self.native_targets.push(NativeTarget {
                        slot: *slot,
                        rect: hit_rect,
                    });
                    if let Some(idx) = elem_idx {
                        if state_blocks.iter().any(|sb| {
                            sb.states.iter().any(|st| {
                                matches!(st, StyleStateKind::Hover | StyleStateKind::Pressed)
                            })
                        }) {
                            self.router.track_region(idx, hit_rect);
                        }
                    }
                    let has_events = attrs
                        .iter()
                        .any(|a| matches!(a.kind, AttrKind::Event { .. }))
                        || action.is_some();
                    if has_events {
                        let env_base = self.env.len();
                        for (k, v) in env_snapshot {
                            self.env.push(k.clone(), v.clone());
                        }
                        self.register_event_attrs(attrs, hit_rect, elem_idx);
                        if let Some(action_expr) = action {
                            if let (Ok(closure), Some(idx)) =
                                (self.lower_action(action_expr, None), elem_idx)
                            {
                                self.router.on(
                                    idx,
                                    hit_rect,
                                    crate::interp::events::EventKind::Tap,
                                    closure,
                                );
                            }
                        }
                        self.env.truncate(env_base);
                    }
                }
            }
            RenderNode::Vector { attrs, src } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    let handle = self
                        .binding_value(*src)
                        .and_then(|v| if let Value::Str(s) = v { Some(s) } else { None })
                        .unwrap_or_default();
                    let base_rgb = self
                        .eval_color_prop(attrs, "color")
                        .map_or([1.0, 1.0, 1.0, 1.0], |c| {
                            super::intrinsics::color_to_rgba(c, false)
                        });
                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(attrs, "opacity")
                            .map_or(1.0, |v| v as f32);

                    // Cache hit: a resident glyph, tinted and opacity-applied.
                    // Cache miss: a zero-opacity placeholder so the frame ships
                    // without stalling (INV-9); the dispatch itself happened
                    // inside `lookup_or_dispatch`.
                    let (uv_rect, layer, px_range, alpha) =
                        match self.vector_jit.lookup_or_dispatch(&handle) {
                            Some(glyph) => (
                                glyph.uv_rect,
                                glyph.layer,
                                glyph.px_range,
                                base_rgb[3] * opacity,
                            ),
                            None => (
                                byard_core::frame::Rect::new(0.0, 0.0, 0.0, 0.0),
                                0,
                                super::intrinsics::VECTOR_DEFAULT_PX_RANGE,
                                0.0,
                            ),
                        };
                    let rgb = [base_rgb[0], base_rgb[1], base_rgb[2], alpha];

                    frame.push_vector(byard_core::frame::VectorInstance::new(
                        byard_core::frame::Rect::new(rect.x, rect.y, rect.width, rect.height),
                        uv_rect,
                        rgb,
                        px_range,
                        layer,
                    ));
                }
            }
            // RFC-0020: the `Canvas` drawing surface. Shape parameters are
            // re-evaluated every tick through `eval_pure`, so a reactive or
            // `with`-animated `sweep`/`dash_offset`/color animates with zero
            // extra plumbing (RFC-0010's single evaluation chokepoint).
            RenderNode::Canvas {
                attrs,
                state_blocks,
                shapes,
                action,
                env_snapshot,
            } => {
                if let Ok(Some(rect)) = self.atlas.resolved_rect(atlas_node) {
                    let elem_idx = self.atlas.node_index(atlas_node);
                    // RFC-0016: overlay active `on <state>` blocks before
                    // reading paint properties. RFC-0024: fold in universal states.
                    let state = elem_idx
                        .map_or_else(crate::interp::events::StyleState::empty, |i| {
                            self.router.style_state(i)
                        })
                        .union(self.prop_style_state(attrs, None, ""));
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, state);
                    let paint_attrs = paint_attrs.as_ref();

                    // RFC-0019 §2: restore the instance environment so shape
                    // parameter expressions and event actions resolve against
                    // the scope the canvas was instantiated in.
                    let env_base = self.env.len();
                    for (k, v) in env_snapshot {
                        self.env.push(k.clone(), v.clone());
                    }

                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(paint_attrs, "opacity")
                            .map_or(1.0, |v| v as f32);
                    let canvas_rect = crate::interp::intrinsics::Rect::new(
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                    );

                    // Background fill: a plain solid behind every shape.
                    if let Some(bg) = self.eval_color_prop(paint_attrs, "bg") {
                        frame.push_instance(byard_core::BoxInstance {
                            rect: [rect.x, rect.y, rect.width, rect.height],
                            color: dim_alpha(super::intrinsics::color_to_rgba(bg, false), opacity),
                            radii: [0.0; 4],
                            transform: inherited_transform,
                            smooth: 0.0,
                        });
                    }

                    // Shape commands, in declaration order (painter's order,
                    // each `push_canvas_shape` advances the global emission
                    // depth, RFC-0011). A `Canvas` that declares a combine mode
                    // (RFC-0031 §S4) collects them into one group instead, and
                    // pushes its head once at the end.
                    let combine = self.resolve_group_mode(paint_attrs);
                    let mut sink = combine.map(|_| ShapeGroupSink::default());
                    self.emit_canvas_items(
                        shapes,
                        canvas_rect,
                        opacity,
                        inherited_transform,
                        sink.as_mut(),
                        frame,
                    );
                    if let (Some((mode, param)), Some(sink)) = (combine, sink) {
                        self.push_shape_group(
                            mode,
                            param,
                            sink,
                            opacity,
                            inherited_transform,
                            attr_span(attrs),
                            frame,
                        );
                    }

                    // Events: the canvas rect only, individual shapes are not
                    // hit-testable (RFC-0020 resolved question). Shifted to
                    // the on-screen position like every hit rect (RFC-0005).
                    let hit_rect = scrolled_hit_rect(
                        crate::interp::intrinsics::inflate_hit_rect(canvas_rect, parent_rect),
                        scroll_shift,
                        cull_clip,
                    );
                    // RFC-0016: an `on hover`/`on pressed` block with no handler
                    // still needs pointer tracking, mirroring the Box path.
                    if let Some(idx) = elem_idx {
                        if state_blocks.iter().any(|sb| {
                            sb.states.iter().any(|s| {
                                matches!(s, StyleStateKind::Hover | StyleStateKind::Pressed)
                            })
                        }) {
                            self.router.track_region(idx, hit_rect);
                        }
                    }
                    let has_events = attrs
                        .iter()
                        .any(|a| matches!(a.kind, AttrKind::Event { .. }))
                        || action.is_some();
                    if has_events {
                        self.register_event_attrs(attrs, hit_rect, elem_idx);
                        if let Some(action_expr) = action {
                            if let Ok(closure) = self.lower_action(action_expr, None) {
                                if let Some(idx) = elem_idx {
                                    self.router.on(
                                        idx,
                                        hit_rect,
                                        crate::interp::events::EventKind::Tap,
                                        closure,
                                    );
                                }
                            }
                        }
                    }
                    self.env.truncate(env_base);
                }
            }
        }
    }

    // ── Widget rendering helpers (M16/M19) ─────────────────────────────

    /// Renders a `Toggle` widget: track + thumb (M19), and registers a Tap
    /// handler to flip the bound bool (M16).
    #[allow(clippy::too_many_arguments)]
    fn render_toggle(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let is_on = bound_sig.is_some_and(|s| self.ctx.peek_signal(s).as_bool().unwrap_or(false));

        // The full-height pill track. `bg` is the ON accent (default: theme
        // primary); OFF is a muted surface tint.
        let accent = self
            .eval_color_prop(attrs, "bg")
            .unwrap_or(self.theme.primary());
        let track_color = if is_on {
            super::intrinsics::color_to_rgba(accent, false)
        } else {
            [0.40_f32, 0.42, 0.48, 1.0]
        };
        let radius = rect.h / 2.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [rect.x, rect.y, rect.w, rect.h],
            color: dim_alpha(track_color, opacity),
            radii: [radius; 4],
            transform,
            smooth: 0.0,
        });

        // Thumb: a white circle inset from the track edges, sliding L↔R.
        let pad = (rect.h * 0.12).max(2.0);
        let thumb_size = (rect.h - pad * 2.0).max(2.0);
        let thumb_y = rect.y + pad;
        let thumb_x = if is_on {
            rect.x + rect.w - thumb_size - pad
        } else {
            rect.x + pad
        };
        frame.push_instance(byard_core::BoxInstance {
            rect: [thumb_x, thumb_y, thumb_size, thumb_size],
            color: dim_alpha([1.0, 1.0, 1.0, 1.0], opacity),
            radii: [thumb_size / 2.0; 4],
            transform,
            smooth: 0.0,
        });

        // Tap handler to flip the bool (M16).
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let flip: super::events::Action = Box::new(move |ctx, _| {
                let cur = ctx.peek_signal(sig).as_bool().unwrap_or(false);
                ctx.write_signal(sig, Value::Bool(!cur));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::Tap, flip);
        }
    }

    /// Renders a `Checkbox` widget (RFC-0018): a rounded square that fills with
    /// the accent colour and shows an engine-drawn checkmark when checked, a
    /// muted filled slot when unchecked, and a horizontal dash for the
    /// `indeterminate` mixed state, all borderless SolidBoxes (a 2px ring reads
    /// as a heavy dark outline at control sizes). Registers Tap + Space (KeyDown)
    /// to flip the bound bool, a `change` write-back (RFC-0003 E1), and a focus
    /// target so Tab and click reach it. Like `Toggle`, it owns its visuals,
    /// `bg` is the checked accent, never a full-rect slab.
    ///
    /// The checkmark is drawn as two rounded stroke quads rotated to each
    /// segment's angle through the paint-transform system, so it stays crisp at
    /// any size/DPI and needs no atlas asset for the leaf mark. (RFC-0018's
    /// resolved MSDF-baked checkmark is a rendering refinement tracked for the
    /// vector subsystem; the geometry and interaction contract here are final.)
    #[allow(clippy::too_many_arguments)]
    fn render_checkbox(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let is_on = bound_sig.is_some_and(|s| self.ctx.peek_signal(s).as_bool().unwrap_or(false));
        let indeterminate = self.eval_bool_prop(attrs, "indeterminate").unwrap_or(false);
        // A checkbox is square: use the smaller side so a non-square rect still
        // yields crisp, proportional geometry, centred in the laid-out box.
        let side = rect.w.min(rect.h);
        let ox = rect.x + (rect.w - side) / 2.0;
        let oy = rect.y + (rect.h - side) / 2.0;
        let radius = (side * 0.18).max(2.0);
        let filled = is_on || indeterminate;

        // The container is a rounded `DecoratedBox` so it can carry a `border`
        // (RFC-0024 lets a style set `border`/`on checked { border }`). `bg` is
        // the checked accent (default: theme primary); when unchecked, a styled
        // `border` yields an outlined box with a transparent interior (the M3
        // look), otherwise a muted filled slot (`Toggle`'s OFF tint). The
        // container is pushed *before* the mark, both on the decorated pipeline,
        // so the white mark lands on top.
        let bg = self.eval_color_prop(attrs, "bg");
        let border = self.eval_color_prop(attrs, "border");
        let border_width = if border.is_some() {
            self.eval_float_prop(attrs, "border_width")
                .map_or(2.0, |v| v as f32)
        } else {
            0.0
        };
        let border_rgba = border.map_or([0.0; 4], |c| {
            dim_alpha(super::intrinsics::color_to_rgba(c, false), opacity)
        });
        let accent = bg.unwrap_or(self.theme.primary());
        let fill = if filled {
            dim_alpha(super::intrinsics::color_to_rgba(accent, false), opacity)
        } else if border.is_some() {
            [0.0; 4]
        } else {
            dim_alpha([0.40, 0.42, 0.48, 1.0], opacity)
        };
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: byard_core::BoxInstance {
                rect: [ox, oy, side, side],
                color: fill,
                radii: [radius; 4],
                transform,
                smooth: 0.0,
            },
            border_width,
            border_color: border_rgba,
            dirty: true,
            ..Default::default()
        });

        // The mark, in white, on the filled square (also decorated, so it paints
        // above the container).
        let mark = dim_alpha([1.0, 1.0, 1.0, 1.0], opacity);
        if indeterminate {
            // Mixed state: a single horizontal bar (checked overrides mixed only
            // in the container fill; the dash renders whenever `indeterminate`).
            let bar_w = side * 0.5;
            let bar_h = (side * 0.12).max(2.0);
            frame.push_decorated(byard_core::frame::DecoratedBox {
                base: byard_core::BoxInstance {
                    rect: [
                        ox + (side - bar_w) / 2.0,
                        oy + (side - bar_h) / 2.0,
                        bar_w,
                        bar_h,
                    ],
                    color: mark,
                    radii: [bar_h / 2.0; 4],
                    transform,
                    smooth: 0.0,
                },
                dirty: true,
                ..Default::default()
            });
        } else if is_on {
            // A two-stroke checkmark: canonical vertices in the unit square,
            // each segment a rounded quad rotated to its angle.
            let t = (side * 0.13).max(2.0);
            let pts = [
                [ox + side * 0.26, oy + side * 0.52], // short-stroke start
                [ox + side * 0.44, oy + side * 0.70], // bottom vertex
                [ox + side * 0.76, oy + side * 0.32], // long-stroke end
                [ox + side * 0.41, oy + side * 0.70], // long-stroke start
            ];
            for (a, b) in [(pts[0], pts[1]), (pts[3], pts[2])] {
                push_stroke_quad(frame, a, b, t, mark, transform);
            }
        }

        // Handlers: Tap + Space flip the bool; `change` write-back; focusable.
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let flip: super::events::Action = Box::new(move |ctx, _| {
                let cur = ctx.peek_signal(sig).as_bool().unwrap_or(false);
                ctx.write_signal(sig, Value::Bool(!cur));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::Tap, flip);

            // Space toggles when focused (WAI-ARIA checkbox keyboard pattern).
            let key_flip: super::events::Action = Box::new(move |ctx, payload| {
                if let Some(Value::Str(key)) = payload {
                    if matches!(key.as_str(), " " | "Space" | "Spacebar") {
                        let cur = ctx.peek_signal(sig).as_bool().unwrap_or(false);
                        ctx.write_signal(sig, Value::Bool(!cur));
                    }
                }
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::KeyDown, key_flip);

            // Change write-back from the platform (RFC-0003 E1).
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );

            // Focus target so Tab and click reach the box. Uses the element's own
            // `focused:` var when given, else a private signal for the registry.
            let focused_sig = self.resolve_focused_sig(attrs);
            let fsig = focused_sig.unwrap_or_else(|| self.ctx.create_signal(Value::Bool(false)));
            self.router.focusable(idx, hit_rect, fsig);
        }
    }

    /// Renders a `RadioButton` widget (RFC-0018): an outer ring plus an inner
    /// filled dot when selected. Selection is `bind == value`: the bound group
    /// `var`'s current string equals this button's `value`. Tapping writes this
    /// button's `value` to the group var, so the previously selected sibling
    /// deselects reactively (automatic mutual exclusion, every radio in the
    /// group reads the same var). Registers the group ordering for arrow-key
    /// navigation, a Tap handler, arrow KeyDown handlers (move selection within
    /// the group, wrapping), a `change` write-back (RFC-0003 E1), and a focus
    /// target so Tab/click reach it. Owns its visuals, `bg` is the selected
    /// accent. The ring is the radio's defining affordance (unlike `Checkbox`,
    /// whose square border was dropped); its interior is transparent so the dot
    /// shows through and it composes over any background.
    #[allow(clippy::too_many_arguments)]
    fn render_radio(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let value = self.eval_str_prop(attrs, "value").unwrap_or_default();
        let selected = bound_sig.is_some_and(|s| match self.ctx.peek_signal(s) {
            Value::Str(v) => v == value,
            _ => false,
        });

        // A radio is circular: use the smaller side, centred in the laid-out box.
        let side = rect.w.min(rect.h);
        let ox = rect.x + (rect.w - side) / 2.0;
        let oy = rect.y + (rect.h - side) / 2.0;
        let r = side / 2.0;

        let accent = self
            .eval_color_prop(attrs, "bg")
            .unwrap_or(self.theme.primary());
        let accent_rgba = super::intrinsics::color_to_rgba(accent, false);
        let ring_color = if selected {
            accent_rgba
        } else {
            [0.55, 0.57, 0.62, 1.0]
        };

        // Inner dot FIRST (solid pipeline) so it sits beneath the ring; the ring's
        // interior is transparent, so the dot shows through regardless. Only drawn
        // when selected.
        if selected {
            let dot = side * 0.5;
            let inset = (side - dot) / 2.0;
            frame.push_instance(byard_core::BoxInstance {
                rect: [ox + inset, oy + inset, dot, dot],
                color: dim_alpha(accent_rgba, opacity),
                radii: [dot / 2.0; 4],
                transform,
                smooth: 0.0,
            });
        }

        // Outer ring: a transparent-interior DecoratedBox with a full-radius
        // border. Width scales with size so it reads as a ring, not a hairline.
        let ring_w = (side * 0.12).max(2.0);
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: byard_core::BoxInstance {
                rect: [ox, oy, side, side],
                color: [0.0; 4],
                radii: [r; 4],
                transform,
                smooth: 0.0,
            },
            border_width: ring_w,
            border_color: dim_alpha(ring_color, opacity),
            dirty: true,
            ..Default::default()
        });

        // ── Group registration + handlers ────────────────────────────────────
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            // Record this button's value in its group's ordered list (shared via
            // an Rc so the arrow handlers below see the whole group once the
            // render has finished populating it).
            let group = self.radio_groups.entry(sig).or_default().clone();
            group.borrow_mut().push(value.clone());

            // Tap → select this button (write its value to the group var).
            let val = value.clone();
            let select: super::events::Action = Box::new(move |ctx, _| {
                ctx.write_signal(sig, Value::Str(val.clone()));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::Tap, select);

            // Arrow keys → move selection to the next/previous value in the group,
            // wrapping at both ends (WAI-ARIA radio group). Down/Right advance;
            // Up/Left retreat.
            let grp = group.clone();
            let arrows: super::events::Action = Box::new(move |ctx, payload| {
                let Some(Value::Str(key)) = payload else {
                    return;
                };
                let forward = match key.as_str() {
                    "ArrowDown" | "ArrowRight" => true,
                    "ArrowUp" | "ArrowLeft" => false,
                    _ => return,
                };
                let vals = grp.borrow();
                let n = vals.len();
                if n == 0 {
                    return;
                }
                let cur = match ctx.peek_signal(sig) {
                    Value::Str(s) => s,
                    _ => String::new(),
                };
                let here = vals.iter().position(|v| *v == cur).unwrap_or(0);
                // Wrap-around in unsigned arithmetic (no signed casts): forward is
                // `+1`, backward is `+ (n − 1)`, both mod `n`.
                let next = if forward {
                    (here + 1) % n
                } else {
                    (here + n - 1) % n
                };
                ctx.write_signal(sig, Value::Str(vals[next].clone()));
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::KeyDown, arrows);

            // Change write-back from the platform (RFC-0003 E1).
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );

            // Focus target so Tab and click reach the button.
            let focused_sig = self.resolve_focused_sig(attrs);
            let fsig = focused_sig.unwrap_or_else(|| self.ctx.create_signal(Value::Bool(false)));
            self.router.focusable(idx, hit_rect, fsig);
        }
    }

    /// Renders a `Slider` widget: track + fill + thumb (M19), and registers
    /// PointerDown + PointerDrag handlers to write the value (M16).
    #[allow(clippy::too_many_arguments)]
    fn render_slider(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        // Keep the authored `f64` values for the value-write path: computing the
        // emitted value in `f64` avoids the `f32`→`f64` widening artifact (a drag
        // landing on 0.6 was stored as `f64::from(0.6_f32)` =
        // 0.6000000238418579). The `f32` casts below are only for pixel-space
        // visual layout (track/fill/thumb), where the noise is invisible.
        let min_f = self.eval_float_prop(attrs, "min").unwrap_or(0.0);
        let max_f = self.eval_float_prop(attrs, "max").unwrap_or(1.0);
        let step_f = self.eval_float_prop(attrs, "step");
        let min = min_f as f32;
        let max = max_f as f32;
        let cur_val = bound_sig.map_or(min, |s| match self.ctx.peek_signal(s) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => min,
        });
        let t = if (max - min).abs() > f32::EPSILON {
            ((cur_val - min) / (max - min)).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // `bg` is the fill accent (default: theme primary); the unfilled track
        // is a muted tint.
        let accent = self
            .eval_color_prop(attrs, "bg")
            .unwrap_or(self.theme.primary());
        let accent_rgba = super::intrinsics::color_to_rgba(accent, false);

        // Track (unfilled remainder).
        let track_h = (rect.h * 0.28).clamp(4.0, 8.0);
        let track_y = rect.y + (rect.h - track_h) / 2.0;
        let track_r = track_h / 2.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [rect.x, track_y, rect.w, track_h],
            color: dim_alpha([0.40, 0.42, 0.48, 1.0], opacity),
            radii: [track_r; 4],
            transform,
            smooth: 0.0,
        });

        // Fill up to the thumb.
        let fill_w = t * rect.w;
        if fill_w > 0.0 {
            frame.push_instance(byard_core::BoxInstance {
                rect: [rect.x, track_y, fill_w, track_h],
                color: dim_alpha(accent_rgba, opacity),
                radii: [track_r; 4],
                transform,
                smooth: 0.0,
            });
        }

        // Thumb: white circle with a thin accent ring (drawn as accent disc
        // under a slightly smaller white disc).
        let thumb_size = (rect.h * 0.85).clamp(14.0, 22.0);
        let thumb_x = rect.x + t * (rect.w - thumb_size);
        let thumb_y = rect.y + (rect.h - thumb_size) / 2.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [thumb_x, thumb_y, thumb_size, thumb_size],
            color: dim_alpha(accent_rgba, opacity),
            radii: [thumb_size / 2.0; 4],
            transform,
            smooth: 0.0,
        });
        let inner = thumb_size - 5.0;
        frame.push_instance(byard_core::BoxInstance {
            rect: [thumb_x + 2.5, thumb_y + 2.5, inner, inner],
            color: dim_alpha([1.0, 1.0, 1.0, 1.0], opacity),
            radii: [inner / 2.0; 4],
            transform,
            smooth: 0.0,
        });

        // Handlers: PointerDown + PointerDrag (M16).
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let track_x = rect.x;
            let track_w = rect.w;
            let make_drag_action =
                |min: f64, max: f64, step: Option<f64>| -> super::events::Action {
                    Box::new(move |ctx, _| {
                        let pos = super::events::CURRENT_EVENT_POS.with(std::cell::Cell::get);
                        // Pixel positions are `f32`; widen before the value math so
                        // the stored value never carries `f32` rounding noise.
                        let t = ((f64::from(pos.0) - f64::from(track_x)) / f64::from(track_w))
                            .clamp(0.0, 1.0);
                        let raw = min + t * (max - min);
                        // Quantise so the value never carries more decimals than
                        // the step (or, with no step, a readable default), see
                        // `step_decimals`/`SLIDER_DEFAULT_DECIMALS`.
                        let val = match step {
                            Some(s) => round_to_decimals((raw / s).round() * s, step_decimals(s)),
                            None => round_to_decimals(raw, SLIDER_DEFAULT_DECIMALS),
                        };
                        ctx.write_signal(sig, Value::Float(val));
                    })
                };
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::PointerDown,
                make_drag_action(min_f, max_f, step_f),
            );
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::PointerDrag,
                make_drag_action(min_f, max_f, step_f),
            );
        }
    }

    /// Renders a `TextField` widget: background box + text/placeholder (M19),
    /// and registers keyboard handlers for text input (M16/M17).
    #[allow(clippy::too_many_arguments)]
    fn render_text_field(
        &mut self,
        bound_sig: Option<super::env::SignalId>,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        let placeholder = self.eval_str_prop(attrs, "placeholder").unwrap_or_default();
        let cur_text = bound_sig
            .map(|s| match self.ctx.peek_signal(s) {
                Value::Str(t) => t,
                _ => String::new(),
            })
            .unwrap_or_default();

        let (display_text, is_placeholder) = if cur_text.is_empty() {
            (placeholder, true)
        } else {
            (cur_text, false)
        };

        let text_color = if is_placeholder {
            0x0088_8888_i64
        } else {
            0x00ff_ffff_i64
        };
        let font_size = self.eval_int_prop(attrs, "size").unwrap_or(16) as f32;
        let is_focused = elem_idx.is_some_and(|i| self.router.is_focused(i));

        // Focus underline (Material-style): a thin accent bar along the bottom
        // edge when the field holds focus.
        if is_focused {
            let bar_h = 2.0_f32;
            frame.push_instance(byard_core::BoxInstance {
                rect: [rect.x, rect.y + rect.h - bar_h, rect.w, bar_h],
                color: dim_alpha(
                    super::intrinsics::color_to_rgba(self.theme.primary(), false),
                    opacity,
                ),
                radii: [0.0; 4],
                transform,
                smooth: 0.0,
            });
        }

        let pad_x = 10.0_f32;
        let text_x = rect.x + pad_x;
        let text_y = rect.y + (rect.h - font_size) / 2.0;
        // NOTE: `TextLine` carries no `Transform` field (RFC-0011 engine slice:
        // only box primitives were given one), so the field's *text* does not
        // follow `translate`/`scale`/`rotate`, the box visuals below (underline,
        // caret) and its `bg` fill do. Same limitation as the `Text` intrinsic.
        if !display_text.is_empty() {
            frame.push_text(byard_core::TextLine {
                weight: 400,
                family: None,
                x: text_x,
                y: text_y,
                text: display_text.clone(),
                font_size,
                color: dim_alpha(super::intrinsics::color_to_rgba(text_color, false), opacity),
                dirty: true,
            });
        }

        // Caret at the end of the entered text while focused (M17/M19).
        if is_focused {
            let measured = if is_placeholder {
                0.0
            } else {
                self.measure_text(&display_text, font_size).0
            };
            frame.push_instance(byard_core::BoxInstance {
                rect: [text_x + measured + 1.0, text_y, 1.5, font_size],
                color: dim_alpha([1.0, 1.0, 1.0, 1.0], opacity),
                radii: [0.0; 4],
                transform,
                smooth: 0.0,
            });
        }

        // Handlers: TextInput appends, KeyDown handles Backspace/Enter/Tab (M16/M17).
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            // TextInput: append typed text
            let text_input: super::events::Action = Box::new(move |ctx, payload| {
                if let Some(Value::Str(ch)) = payload {
                    let cur = match ctx.peek_signal(sig) {
                        Value::Str(s) => s,
                        _ => String::new(),
                    };
                    ctx.write_signal(sig, Value::Str(cur + ch.as_str()));
                }
            });
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::TextInput,
                text_input,
            );

            // KeyDown: Backspace deletes, Enter/Escape handled (submit fires via Change)
            let key_down: super::events::Action = Box::new(move |ctx, payload| {
                if let Some(Value::Str(key)) = payload {
                    match key.as_str() {
                        "Backspace" => {
                            let cur = match ctx.peek_signal(sig) {
                                Value::Str(s) => s,
                                _ => String::new(),
                            };
                            let mut s = cur;
                            s.pop();
                            ctx.write_signal(sig, Value::Str(s));
                        }
                        "Delete" => {
                            ctx.write_signal(sig, Value::Str(String::new()));
                        }
                        _ => {}
                    }
                }
            });
            self.router
                .on(idx, hit_rect, super::events::EventKind::KeyDown, key_down);

            // Change event: write-back from platform (E1).
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );

            // Register as focusable so Tab and click steal focus (M18).
            // TextField uses its own focused-var if provided via `focused:` attr;
            // otherwise we create a dummy signal just for the focusable registry.
            let focused_sig = self.resolve_focused_sig(attrs);
            let fsig = focused_sig.unwrap_or_else(|| self.ctx.create_signal(Value::Bool(false)));
            self.router.focusable(idx, hit_rect, fsig);
        }
    }

    /// Resolves the `focused:` attribute to a `SignalId`, if present.
    fn resolve_focused_sig(&self, attrs: &[Attr]) -> Option<super::env::SignalId> {
        use crate::parser::ast::Expr;
        for attr in attrs {
            if attr.name.as_str() == "focused" {
                if let AttrKind::Prop {
                    value: Expr::Ident(name, _),
                } = &attr.kind
                {
                    if let Some(Value::Signal(sig)) = self.env.lookup(name) {
                        return Some(*sig);
                    }
                }
            }
        }
        None
    }

    /// Registers handlers for all event-kind attrs (`#[tap => …]`, etc.).
    /// Places this frame's native-view controller requests (RFC-0039).
    ///
    /// A view named a controller and a method; from here the request is
    /// indistinguishable from one an action raised, because it *is* one: the
    /// same queue, the same dispatcher, the same reply channel. What is extra
    /// is the note of who to hand the answer to, which is the whole of the
    /// "async across the boundary" surface.
    fn dispatch_native_calls(&mut self, frame: &mut byard_core::frame::RenderFrame) {
        let owners = std::mem::take(&mut self.native_call_owners);
        if owners.is_empty() {
            // Nothing asked for anything, which is the steady state of every
            // widget that is not fetching something.
            frame.take_native_calls();
            return;
        }
        let calls = frame.take_native_calls();
        for (slot, range) in owners {
            for call in calls.get(range).into_iter().flatten() {
                let Some(controller) = self.controller_id_by_name(&call.controller) else {
                    // A view asking for a controller the app never provided is
                    // an assembly mistake, and a silent no-answer is the worst
                    // possible way to learn about it (INV-4).
                    self.perf_warnings.push(PerfWarning::UnprovidedNativeCall {
                        controller: call.controller.clone(),
                        method: call.method.clone(),
                    });
                    continue;
                };
                let continuation = self.open_native_continuation(slot, call.key);
                self.queue_native_call(controller, &call.method, call.args.clone(), continuation);
            }
        }
        self.drain_calls();
    }

    /// Offers each event to the native views under the pointer, innermost
    /// first, and reports which events a view took (RFC-0039 × RFC-0003).
    ///
    /// Innermost first is "last recorded first": the render walk is
    /// depth-first, so a view nested inside another was pushed after it, and
    /// walking the targets in reverse is the same front-to-back order the
    /// router's own hit testing uses.
    ///
    /// A keyboard event has no position, so it is offered to no view here; a
    /// view that wants keys gets them the way an intrinsic does, through the
    /// focus path, and inventing a second rule for package elements is exactly
    /// the divergence this ABI exists to avoid.
    fn dispatch_to_native_views(&mut self, events: &[byard_core::InputEvent]) -> Vec<bool> {
        use byard_core::render::{Event as ViewEvent, Handled, Layout};

        let mut consumed = vec![false; events.len()];
        if self.native_targets.is_empty() {
            return consumed;
        }
        let targets = self.native_targets.clone();
        for (i, ev) in events.iter().enumerate() {
            let (px, py) = ev.pos;
            for target in targets.iter().rev() {
                let r = target.rect;
                if px < r.x || px >= r.x + r.w || py < r.y || py >= r.y + r.h {
                    continue;
                }
                let Some(view) = self.native_views.get_mut(target.slot) else {
                    continue;
                };
                let layout = Layout::new([r.x, r.y, r.w, r.h]);
                let handled = view.on_event(
                    &ViewEvent {
                        kind: ev.kind,
                        local: layout.local((px, py)),
                        delta: ev.delta,
                        payload: ev.payload.clone(),
                    },
                    layout,
                );
                if handled == Handled::Yes {
                    consumed[i] = true;
                    break;
                }
            }
        }
        consumed
    }

    /// Makes this element's native-view instance and returns its slot
    /// (RFC-0039).
    ///
    /// Called at lower time, which is the element's birth: the view exists for
    /// as long as this lowered node does, and a re-lower (a hot reload, a
    /// structural change) drops it, which is its unmount. There is no separate
    /// bookkeeping to keep in step, and therefore nothing to get out of step
    /// (INV-31).
    fn mount_native_view(&mut self, name: &str, span: crate::diagnostics::Span) -> usize {
        let slot = self.native_views.len();
        if let Some(view) = byard_core::render::registry::create(name) {
            self.native_views.push(view);
        } else {
            // The catalog answered for this name a moment ago, so failing here
            // means the registry changed underneath the lowering, which is not
            // something an app can do by accident. Said with a span rather
            // than papered over with a blank element (INV-4).
            self.errors.push(CompileError::UnknownView {
                span,
                name: name.to_string(),
                hint: None,
            });
            self.native_views.push(Box::new(MissingNativeView));
        }
        slot
    }

    /// Evaluates this element's declared props and hands them to its view
    /// (RFC-0039).
    ///
    /// Only the props the view declared, and only the ones the element wrote:
    /// an unwritten prop is the view's own initial value, not a `None` it has
    /// to interpret, and an unknown one was already a compile error with a
    /// span before lowering ever reached here.
    fn apply_native_props(&mut self, name: &str, slot: usize, attrs: &[Attr]) {
        let Some(info) = byard_core::render::registry::info(name) else {
            return;
        };
        for prop in info.props {
            let Some(attr) = attrs.iter().find(|a| a.name.as_str() == prop.name) else {
                continue;
            };
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            let evaluated = self.eval_pure(value);
            // A signal, a memo or a callback has no data form, and a view is
            // data-only by construction (INV-13): the same rule the controller
            // boundary follows, for the same reason.
            let Some(host) = super::bridge::value_to_host(&evaluated) else {
                self.errors.push(CompileError::NonDataViewProp {
                    span: attr.span,
                    prop: prop.name.to_string(),
                    view: name.to_string(),
                });
                continue;
            };
            if let Some(view) = self.native_views.get_mut(slot) {
                view.set_prop(prop.name, &host);
            }
        }
    }

    fn register_event_attrs(
        &mut self,
        attrs: &[Attr],
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
    ) {
        for attr in attrs {
            if let AttrKind::Event { payload, action } = &attr.kind {
                let event_kind = match attr.name.as_str() {
                    "tap" | "click" => super::events::EventKind::Tap, // "click" is an alias (RFC-0012 §A)
                    "pointer_down" => super::events::EventKind::PointerDown,
                    "pointer_up" => super::events::EventKind::PointerUp,
                    "pointer_move" => super::events::EventKind::PointerMove,
                    "scroll" => super::events::EventKind::Scroll,
                    "wheel" => super::events::EventKind::Wheel,
                    // RFC-0021 advanced scroll behaviours (engine-fired).
                    "end_reached" => super::events::EventKind::EndReached,
                    "page_change" => super::events::EventKind::PageChange,
                    "scroll_end" => super::events::EventKind::ScrollEnd,
                    "refresh" => super::events::EventKind::Refresh,
                    // RFC-0026: fired when a navigation settles.
                    "route_change" => super::events::EventKind::RouteChange,
                    "change" => super::events::EventKind::Change,
                    "key_down" => super::events::EventKind::KeyDown,
                    "key_up" => super::events::EventKind::KeyUp,
                    "text_input" => super::events::EventKind::TextInput,
                    // RFC-0012 §A: the six modeled-but-previously-unexposed events.
                    "hover" => super::events::EventKind::Hover,
                    "pointer_enter" => super::events::EventKind::PointerEnter,
                    "pointer_exit" => super::events::EventKind::PointerExit,
                    "long_press" => super::events::EventKind::LongPress,
                    "double_tap" => super::events::EventKind::DoubleTap,
                    "secondary" => super::events::EventKind::Secondary,
                    // RFC-0012 S2: `focus =>`/`blur =>` sugar over `focused_sig`'s
                    // edges, registered as ordinary handlers here; `steal_focus`
                    // fires them directly (see `interp::events::EventKind::Focus`).
                    "focus" => super::events::EventKind::Focus,
                    "blur" => super::events::EventKind::Blur,
                    _ => continue,
                };
                if let Ok(closure) = self.lower_action(action, payload.clone()) {
                    if let Some(idx) = elem_idx {
                        self.router.on(idx, hit_rect, event_kind, closure);
                    }
                }
            }
        }
    }

    /// Registers an element as focusable if it has a `focused:` prop attr
    /// (M16/M18), **or** a `focus =>`/`blur =>` handler (RFC-0012 S2), the
    /// sugar rides `focused_sig`'s edges, so an element that only wants the
    /// one-shot event (no bound `var`) still needs a signal for
    /// `steal_focus` to flip. That signal is a fresh internal one when
    /// `focused:` wasn't given, mirroring `render_text_field`'s same
    /// bind-or-create pattern for its own `focused_sig`.
    fn register_focusable(
        &mut self,
        attrs: &[Attr],
        hit_rect: crate::interp::intrinsics::Rect,
        elem_idx: Option<u32>,
    ) {
        // Without an index there is nowhere to register the focusable, so a
        // freshly created internal signal below would just be dropped,
        // bail out first rather than allocate one for nothing.
        let Some(idx) = elem_idx else {
            return;
        };
        let has_focus_sugar = attrs.iter().any(|a| {
            matches!(a.kind, AttrKind::Event { .. }) && matches!(a.name.as_str(), "focus" | "blur")
        });
        let sig = self
            .resolve_focused_sig(attrs)
            .or_else(|| has_focus_sugar.then(|| self.ctx.create_signal(Value::Bool(false))));
        if let Some(sig) = sig {
            self.router.focusable(idx, hit_rect, sig);
        }
    }

    fn eval_color_prop(&mut self, attrs: &[Attr], name: &str) -> Option<i64> {
        // Resolve the matching attribute value; a `with` colour animation
        // (RFC-0010 A3) is driven through the OKLab path rather than the scalar
        // one, since a packed `0xRRGGBB` can't be interpolated component-wise.
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == name => Some(value),
            _ => None,
        })?;
        if let Expr::Animated {
            value: target,
            anim,
            span,
        } = value
        {
            return Some(self.eval_animated_color(target, anim, self.anim_key(*span)));
        }
        // A keyframed colour (RFC-0025 §3) blends its two surrounding steps in
        // the same perceptual space, for the same reason.
        if crate::interp::anim::is_keyframes_call(value) {
            return self.eval_keyframe_color(value);
        }
        self.eval_pure(value).as_int()
    }

    /// Samples a keyframed colour sequence (RFC-0025 §3 × RFC-0010 A3): the two
    /// steps surrounding "now", mixed in OKLab so the sweep has no muddy
    /// mid-points. Returns `None` only if the sequence is malformed (already
    /// reported) or its steps are not colours.
    fn eval_keyframe_color(&mut self, expr: &Expr) -> Option<i64> {
        let blend = self.keyframe_blend(expr)?;
        let lo = self.eval_pure(blend.lo).as_int()?;
        if blend.t <= 0.0 {
            return Some(lo);
        }
        let hi = self.eval_pure(blend.hi).as_int()?;
        Some(mix_hex_oklab(lo, hi, blend.t))
    }

    /// Drives one colour `with` animation (RFC-0010 A3): interpolates from the
    /// current colour to the target in OKLab (one [`Motion`] per channel, plus
    /// a fourth for the alpha byte, a translucent `backdrop_tint`/`bg` fades
    /// in and out rather than popping, RFC-0023), so the transition is
    /// perceptually uniform and interruptible. Returns the current colour
    /// packed as `0xAARRGGBB`, opaque targets carry `AA = 0xFF`, which every
    /// consumer's alpha auto-detect reads as the same opaque colour.
    ///
    /// [`Motion`]: byard_core::frame::Motion
    fn eval_animated_color(&mut self, target: &Expr, anim: &Expr, key: AnimKey) -> i64 {
        let target_int = self.eval_pure(target).as_int().unwrap_or(0);
        // Without an advancing clock, jump straight to the target (mirrors the
        // scalar path, never latch `has_active_animations` on t=0).
        if !self.clock_set {
            return target_int;
        }
        let Ok(spec) = crate::interp::anim::resolve_motion(anim) else {
            return target_int;
        };
        let packed = pack_curve(spec.curve);
        let now = self.now_ms;
        // RFC-0025: a repeating/delayed colour runs on its own timeline. Its
        // endpoints are two *colours*, so the channels are driven together off
        // one shared phase rather than each settling on its own.
        if !spec.is_plain() {
            return self.eval_looped_color(target_int, &spec, key);
        }
        let target_ch = color_channels(target_int);
        let motions = self.color_animations.entry(key).or_insert_with(|| {
            [0, 1, 2, 3].map(|i| byard_core::frame::Motion {
                from: target_ch[i],
                to: target_ch[i],
                start_ms: now,
                curve: packed,
            })
        });
        let mut current = [0.0_f32; 4];
        let mut all_settled = true;
        for (i, m) in motions.iter_mut().enumerate() {
            if (m.to - target_ch[i]).abs() > 1e-5 {
                let here = m.sample(now);
                m.from = here;
                m.to = target_ch[i];
                m.start_ms = now;
            }
            m.curve = packed;
            current[i] = m.sample(now);
            if !m.is_settled_with_eps(now, ANIM_SETTLE_EPS_POS, ANIM_SETTLE_EPS_VEL) {
                all_settled = false;
            }
        }
        if !all_settled {
            self.any_active = true;
        }
        color_from_channels(current)
    }

    /// Drives one repeating / delayed colour animation (RFC-0025 × RFC-0010 A3).
    ///
    /// The four channels are two *colours* apart, not four independent scalars,
    /// so they share one period (the longest channel's) and one phase: the whole
    /// colour arrives together, and an alternating loop reverses as one.
    fn eval_looped_color(
        &mut self,
        target_int: i64,
        spec: &crate::interp::anim::MotionSpec<'_>,
        key: AnimKey,
    ) -> i64 {
        let from_int = spec.from.map_or(target_int, |expr| {
            self.eval_pure(expr).as_int().unwrap_or(target_int)
        });
        let now = self.now_ms;
        let curve = pack_curve(spec.curve);
        let (from_ch, to_ch) = (color_channels(from_int), color_channels(target_int));
        let motions: [byard_core::frame::Motion; 4] =
            std::array::from_fn(|i| byard_core::frame::Motion {
                from: from_ch[i],
                to: to_ch[i],
                start_ms: now,
                curve,
            });
        match self.loop_at(&motions, spec, key) {
            Some(t_secs) => {
                color_from_channels(std::array::from_fn(|i| motions[i].sample_secs(t_secs)))
            }
            None => color_from_channels(from_ch),
        }
    }

    fn eval_int_prop(&mut self, attrs: &[Attr], name: &str) -> Option<i64> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    let val = self.eval_pure(value);
                    return val.as_int();
                }
            }
            None
        })
    }

    /// Reads a **pixel extent** prop (`width`, `height`) as an `f32`.
    ///
    /// Separate from [`eval_int_prop`](Self::eval_int_prop) because a pixel
    /// extent is not an integer quantity: layout is `f32` throughout, a
    /// measured rect (RFC-0038) is fractional, and `800.0 / 3.0` is a perfectly
    /// ordinary width to write. Read through the integer path, every one of
    /// those resolved to `None` and the element silently fell back to its
    /// default size, which is the failure INV-4 exists to forbid.
    fn eval_px_prop(&mut self, attrs: &[Attr], name: &str) -> Option<f32> {
        #[allow(clippy::cast_possible_truncation)]
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    return match self.eval_pure(value) {
                        Value::Int(n) => Some(n as f32),
                        Value::Float(f) => Some(f as f32),
                        _ => None,
                    };
                }
            }
            None
        })
    }

    fn eval_float_prop(&mut self, attrs: &[Attr], name: &str) -> Option<f64> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    let val = self.eval_pure(value);
                    return match val {
                        Value::Float(f) => Some(f),
                        Value::Int(n) => Some(n as f64),
                        _ => None,
                    };
                }
            }
            None
        })
    }

    fn eval_bool_prop(&mut self, attrs: &[Attr], name: &str) -> Option<bool> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    if let Value::Bool(b) = self.eval_pure(value) {
                        return Some(b);
                    }
                }
            }
            None
        })
    }

    /// Resolves the `typo:` prop to a font size in logical pixels (RFC-0005
    /// `Typo`, completed by RFC-0022). Accepts either a bare token
    /// (`typo: titleLarge` → a `Str`, resolved against the theme's typography
    /// then the built-in M3 scale) or a theme accessor (`typo: t.titleLarge` →
    /// an `Int` size projected by [`lower_theme_member`](Self::lower_theme_member)).
    /// The weight of the `typo:` token an element names, if it names one
    /// (RFC-0034).
    fn typo_weight(&mut self, attrs: &[Attr]) -> Option<u16> {
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "typo" => Some(value.clone()),
            _ => None,
        })?;
        // A bare token reads as an identifier; a theme accessor has already
        // resolved to a size and carries no weight with it, which is exactly
        // the gap this closes.
        if let Expr::Ident(sym, _) = &value {
            return self.theme.typo_weight(sym.as_str());
        }
        None
    }

    fn eval_typo_size(&mut self, attrs: &[Attr]) -> Option<i64> {
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "typo" => Some(value),
            _ => None,
        })?;
        match self.eval_pure(value) {
            // A theme accessor already resolved to a concrete pixel size.
            Value::Int(px) => Some(px),
            Value::Float(px) =>
            {
                #[allow(clippy::cast_possible_truncation)]
                Some(px as i64)
            }
            // A bare token name → theme typography, falling back to M3 sizes.
            Value::Str(token) => self
                .theme
                .typo_size(&token)
                .or_else(|| super::theme::resolve_typo(&token))
                .map(|s| {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        s as i64
                    }
                }),
            _ => None,
        }
    }

    fn eval_str_prop(&mut self, attrs: &[Attr], name: &str) -> Option<String> {
        attrs.iter().find_map(|a| {
            if a.name.as_str() == name {
                if let AttrKind::Prop { value } = &a.kind {
                    let val = self.eval_pure(value);
                    return match val {
                        Value::Str(s) => Some(s),
                        _ => None,
                    };
                }
            }
            None
        })
    }

    /// The literal keyword token of an *enum* (keyword) prop value, read straight
    /// from the AST bareword, e.g. the `page` in `snap: page`.
    ///
    /// Enum props (`PropType::Enum`) are a closed keyword set validated by the
    /// checker as a bare [`Expr::Ident`] (`intrinsics.rs`, `check_attr_value`),
    /// never as a reactive expression. Reading them with [`eval_pure`] would
    /// instead resolve the identifier through the environment, so a same-named
    /// `var` silently *shadows* the keyword: `snap: page` next to a `var page`
    /// evaluates to the variable's `Int`, not the token `"page"`, and the
    /// behaviour turns off (RFC-0021). Taking the token from the AST matches the
    /// checker exactly, can never be shadowed, and skips lowering an expression
    /// for a value that is always a compile-time keyword.
    fn enum_token(value: &Expr) -> Option<&str> {
        match value {
            Expr::Ident(sym, _) => Some(sym.as_str()),
            // A keyword may also be written as a plain string literal
            // (`fit: "cover"`). A single, non-interpolated run is a token; an
            // interpolated string is not.
            Expr::StrLit(parts, _) => match parts.as_slice() {
                [StrPart::Text(s)] => Some(s.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    /// [`enum_token`](Self::enum_token) for a named attribute: the keyword token
    /// of enum prop `name`, or `None` if absent or not a bareword keyword.
    fn enum_prop<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a str> {
        attrs.iter().find_map(|a| match &a.kind {
            AttrKind::Prop { value } if a.name.as_str() == name => Self::enum_token(value),
            _ => None,
        })
    }

    /// Resolves the RFC-0021 `snap_spring` prop into a packed spring curve, or
    /// `None` for the engine default. Reuses the `with`-animation curve grammar
    /// (`anim.spring(stiffness: …, damping: …)`); a malformed curve is reported
    /// and falls back to the default.
    fn resolve_snap_spring(&mut self, attrs: &[Attr]) -> Option<byard_core::frame::MotionCurve> {
        let value = attrs.iter().find_map(|a| match &a.kind {
            AttrKind::Prop { value } if a.name.as_str() == "snap_spring" => Some(value),
            _ => None,
        })?;
        match crate::interp::anim::resolve_curve(value) {
            Ok(curve) => Some(pack_curve(curve)),
            Err(e) => {
                self.errors.push(e);
                None
            }
        }
    }

    fn eval_fit_prop(&mut self, attrs: &[Attr]) -> byard_core::frame::ImageFit {
        match Self::enum_prop(attrs, "fit") {
            Some("contain") => byard_core::frame::ImageFit::Contain,
            Some("cover") => byard_core::frame::ImageFit::Cover,
            Some("none") => byard_core::frame::ImageFit::None,
            _ => byard_core::frame::ImageFit::Fill,
        }
    }

    /// Emits the RFC-0023 §2 backdrop-blur pane for one element, called from
    /// the `Box` paint arm right after the background push, the RFC-0023 §4
    /// compositing slot (background → blur → tint → ripple → children): the
    /// pane samples everything already emitted, its own background included.
    ///
    /// `blur` (clamped to [`byard_core::frame::BLUR_MAX_RADIUS`]) enables the
    /// pane; both it and `backdrop_tint` resolve through the RFC-0010
    /// animation chokepoints, so `blur: 0 with anim.spring()` +
    /// `on hover { blur: 16 }` animates the glass for free. A tint *without*
    /// blur needs no barrier or off-screen work at all, it lowers to a plain
    /// translucent fill over the content behind, which is the identical
    /// composite at zero cost.
    #[allow(clippy::too_many_arguments)]
    fn emit_backdrop(
        &mut self,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
        radii: [f32; 4],
        smooth: f32,
        transform: byard_core::frame::Transform,
        opacity: f32,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{
            BLUR_MAX_RADIUS, BLUR_QUALITY_AUTO, BLUR_QUALITY_HIGH, BLUR_QUALITY_LOW,
            BackdropInstance,
        };

        let blur = self
            .eval_float_prop(attrs, "blur")
            .map_or(0.0, |v| (v as f32).clamp(0.0, BLUR_MAX_RADIUS));
        let tint = self.eval_color_prop(attrs, "backdrop_tint");
        // Alpha auto-detect (lexer tag or magnitude, RFC-0011): a tint is
        // translucent by nature, `0x00FFFFFF` included.
        let tint_rgba = tint.map_or([0.0; 4], super::intrinsics::color_rgba_auto);

        if blur <= 0.0 {
            // Tint-only: a translucent wash over the (unblurred) content
            // behind, a plain alpha-blended fill composites identically.
            if tint_rgba[3] > 0.0 {
                frame.push_decorated(byard_core::frame::DecoratedBox {
                    base: byard_core::BoxInstance {
                        rect: [rect.x, rect.y, rect.w, rect.h],
                        color: tint_rgba,
                        radii,
                        transform,
                        smooth,
                    },
                    opacity,
                    dirty: true,
                    ..Default::default()
                });
            }
            return;
        }

        let saturation = self
            .eval_float_prop(attrs, "blur_saturation")
            .map_or(BLUR_DEFAULT_SATURATION, |v| (v as f32).max(0.0));
        let quality = attrs
            .iter()
            .find(|a| a.name.as_str() == "blur_quality")
            .and_then(|a| match &a.kind {
                AttrKind::Prop { value } => Self::enum_token(value),
                _ => None,
            })
            .map_or(BLUR_QUALITY_AUTO, |token| match token {
                "high" => BLUR_QUALITY_HIGH,
                "low" => BLUR_QUALITY_LOW,
                _ => BLUR_QUALITY_AUTO,
            });

        frame.push_backdrop(BackdropInstance {
            rect: [rect.x, rect.y, rect.w, rect.h],
            radii,
            smooth,
            blur,
            tint: tint_rgba,
            saturation,
            quality,
            opacity,
            transform,
            depth: 0.0, // stamped by `push_backdrop`
        });
    }

    /// Spawns and emits the RFC-0023 ripple ink for one element, called from
    /// the `Box` paint arm between the background push and the child walk,
    /// which is exactly the RFC's compositing slot (background → ripple →
    /// children), resolved by the emission-order draw depth.
    ///
    /// Enabled by a `ripple:` colour on the element. A ripple *spawns* on the
    /// rising edge of a press gesture while the resolved `ripple_active` is
    /// true (typically flipped by `on pressed { ripple_active: true }`), at the
    /// pointer-down point; the gesture's press timestamp is the spawn latch, so
    /// a hold spawns once while each rapid tap spawns its own (their ink
    /// pools in the pipeline where the circles overlap).
    /// Colour/duration/radius snapshot at spawn;
    /// the element rect, clip radii, transform, and opacity are re-read each
    /// frame so ink tracks a moving element. Expansion is an ease-out ramp and
    /// fade a linear one, both sampled through the shared [`Motion`] closed
    /// forms (RFC-0010 as landed).
    ///
    /// [`Motion`]: byard_core::frame::Motion
    #[allow(clippy::too_many_arguments)]
    fn emit_ripples(
        &mut self,
        attrs: &[Attr],
        elem_idx: Option<u32>,
        rect: crate::interp::intrinsics::Rect,
        radii: [f32; 4],
        smooth: f32,
        transform: byard_core::frame::Transform,
        opacity: f32,
        // Accumulated scroll displacement (RFC-0005): the press position is
        // in screen space while `rect` is layout space, so the tap point maps
        // back through the shift.
        scroll_shift: (f32, f32),
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{Motion, MotionCurve, RippleInstance};

        let Some(color) = self.eval_color_prop(attrs, "ripple") else {
            return;
        };
        let Some(elem) = elem_idx else {
            return;
        };

        // Spawn on a fresh press gesture. Gated on an advancing clock, without
        // one a ripple could never expand, fade, or retire (mirrors
        // `eval_animated`'s inert-host rule).
        if self.clock_set && self.eval_bool_prop(attrs, "ripple_active") == Some(true) {
            if let Some((pos, press_ms)) = self.router.press_gesture(elem) {
                if self.ripple_spawned != Some((elem, press_ms)) {
                    self.ripple_spawned = Some((elem, press_ms));
                    let duration_ms = self
                        .eval_float_prop(attrs, "ripple_duration")
                        .map_or(RIPPLE_DEFAULT_DURATION_MS, |v| v as f32)
                        .max(1.0);
                    let max_radius = self
                        .eval_float_prop(attrs, "ripple_radius")
                        .map(|v| v as f32);
                    self.ripples.push(ActiveRipple {
                        elem,
                        center_rel: [
                            (pos.0 - scroll_shift.0 - rect.x).clamp(0.0, rect.w),
                            (pos.1 - scroll_shift.1 - rect.y).clamp(0.0, rect.h),
                        ],
                        // Alpha auto-detect (lexer tag or magnitude), ripple
                        // ink is typically translucent.
                        color: super::intrinsics::color_rgba_auto(color),
                        start_ms: self.now_ms,
                        duration_ms,
                        max_radius,
                    });
                }
            }
        }

        // Emit every live ripple owned by this element; `any_active` is set
        // after the loop so the iteration's borrow of `self.ripples` never
        // overlaps the mutation.
        let now = self.now_ms;
        let mut emitted_any = false;
        for r in &self.ripples {
            if r.elem != elem {
                continue;
            }
            // Auto max radius: the distance from the tap point to the farthest
            // element corner, so the ink always covers the whole surface
            // (RFC-0023 §"Ripple properties").
            let max_r = r.max_radius.unwrap_or_else(|| {
                let dx = r.center_rel[0].max(rect.w - r.center_rel[0]);
                let dy = r.center_rel[1].max(rect.h - r.center_rel[1]);
                dx.hypot(dy)
            });
            let curve = |kind: u32| MotionCurve {
                kind,
                params: [r.duration_ms, 0.0, 0.0],
            };
            let expand = Motion {
                from: 0.0,
                to: max_r,
                start_ms: r.start_ms,
                curve: curve(MotionCurve::EASE_OUT),
            };
            let fade = Motion {
                from: 1.0,
                to: 0.0,
                start_ms: r.start_ms,
                curve: curve(MotionCurve::LINEAR),
            };
            frame.push_ripple(RippleInstance {
                rect: [rect.x, rect.y, rect.w, rect.h],
                params: [
                    rect.x + r.center_rel[0],
                    rect.y + r.center_rel[1],
                    expand.sample(now),
                    fade.sample(now) * opacity,
                ],
                color: r.color,
                radii,
                smooth,
                t_translate: transform.translate,
                t_scale: transform.scale,
                t_rotate: transform.rotate,
                t_origin: transform.origin,
                depth: 0.0, // stamped by `push_ripple`
            });
            emitted_any = true;
        }
        if emitted_any {
            self.any_active = true;
        }
    }

    fn eval_container_style(
        &mut self,
        element_name: &str,
        attrs: &[Attr],
    ) -> byard_core::atlas::layout::ContainerStyle {
        use byard_core::atlas::layout::{Align, ContainerStyle, FlexDir, Justify};

        let val_to_f32 = |v: &Value| -> Option<f32> {
            match v {
                Value::Int(n) => Some(*n as f32),
                Value::Float(f) => Some(*f as f32),
                _ => None,
            }
        };

        let mut style = ContainerStyle::default();
        style.direction = match element_name {
            "Row" => FlexDir::Row,
            _ => FlexDir::Column,
        };
        // RFC-0005 `ScrollView`: a scroll container, content is measured at
        // natural size and overflows the fixed viewport (clipped + scrolled by
        // the renderer), rather than flex-shrunk to fit. `axis` (default
        // `vertical`) picks the overflowing axes; `both` scrolls in 2D.
        if element_name == "ScrollView" {
            style = style.with_scroll_axes(false, true);
        }
        for attr in attrs {
            if let AttrKind::Prop { value } = &attr.kind {
                // Evaluate only the layout props this resolver consumes.
                // Paint/effect attrs (`bg`, `blur`, `opacity`, …) must NOT be
                // evaluated here: layout runs in the build phase against the
                // *base* attrs, and driving a paint prop through the RFC-0010
                // `with` chokepoint from here would fight the paint pass's
                // state-resolved evaluation of the same `Motion`, a
                // retarget ping-pong that freezes state-driven animations
                // short of their target. Layout props themselves are never
                // animatable (`LayoutPropNotAnimatable`), so skipping the
                // rest removes wasted work without changing behaviour.
                let val = match attr.name.as_str() {
                    "width" | "height" | "gap" | "grow" | "basis" | "pt" | "padding_top"
                    | "padding-top" | "pr" | "padding_right" | "padding-right" | "pb"
                    | "padding_bottom" | "padding-bottom" | "pl" | "padding_left"
                    | "padding-left" | "mt" | "margin_top" | "margin-top" | "mr"
                    | "margin_right" | "margin-right" | "mb" | "margin_bottom"
                    | "margin-bottom" | "ml" | "margin_left" | "margin-left" | "mx"
                    | "margin_x" | "margin_horizontal" | "margin-horizontal" | "my"
                    | "margin_y" | "margin_vertical" | "margin-vertical" => self.eval_pure(value),
                    _ => Value::Unit,
                };
                match attr.name.as_str() {
                    "axis" if element_name == "ScrollView" => {
                        if let Some(s) = Self::enum_token(value) {
                            let (x, y) = match s {
                                "horizontal" => (true, false),
                                "both" => (true, true),
                                _ => (false, true),
                            };
                            style = style.with_scroll_axes(x, y);
                            // A ScrollView is `Column`, so its cross axis is x. To
                            // scroll horizontally, content must keep its natural
                            // width instead of being stretched to the viewport, or
                            // Taffy caps the content extent at the viewport and
                            // there is nothing to scroll. A `stretch` on the block
                            // axis (vertical-only) is still what fills row width.
                            if x {
                                style.align = Align::Start;
                            }
                        }
                    }
                    "width" => style.width = val_to_f32(&val),
                    "height" => style.height = val_to_f32(&val),
                    "direction" => {
                        if let Some(s) = Self::enum_token(value) {
                            style.direction = match s {
                                "column" => FlexDir::Column,
                                _ => FlexDir::Row,
                            };
                        }
                    }
                    "gap" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.gap = v;
                        }
                    }
                    "p" | "padding" => {
                        style.padding = self.resolve_spacing(value, "p");
                    }
                    "pt" | "padding_top" | "padding-top" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.top = v;
                        }
                    }
                    "pr" | "padding_right" | "padding-right" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.right = v;
                        }
                    }
                    "pb" | "padding_bottom" | "padding-bottom" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.bottom = v;
                        }
                    }
                    "pl" | "padding_left" | "padding-left" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.padding.left = v;
                        }
                    }
                    "m" | "margin" => {
                        style.margin = self.resolve_spacing(value, "m");
                    }
                    "mt" | "margin_top" | "margin-top" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.top = v;
                        }
                    }
                    "mr" | "margin_right" | "margin-right" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.right = v;
                        }
                    }
                    "mb" | "margin_bottom" | "margin-bottom" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.bottom = v;
                        }
                    }
                    "ml" | "margin_left" | "margin-left" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.left = v;
                        }
                    }
                    "mx" | "margin_x" | "margin_horizontal" | "margin-horizontal" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.left = v;
                            style.margin.right = v;
                        }
                    }
                    "my" | "margin_y" | "margin_vertical" | "margin-vertical" => {
                        if let Some(v) = val_to_f32(&val) {
                            style.margin.top = v;
                            style.margin.bottom = v;
                        }
                    }
                    "align" => {
                        if let Some(s) = Self::enum_token(value) {
                            style.align = match s {
                                "start" => Align::Start,
                                "center" => Align::Center,
                                "end" => Align::End,
                                _ => Align::Stretch,
                            };
                        }
                    }
                    "justify" => {
                        if let Some(s) = Self::enum_token(value) {
                            style.justify = match s {
                                "center" => Justify::Center,
                                "end" => Justify::End,
                                "between" => Justify::Between,
                                "around" => Justify::Around,
                                "evenly" => Justify::Evenly,
                                _ => Justify::Start,
                            };
                        }
                    }
                    "grow" => {
                        if let Some(n) = val.as_int() {
                            style.grow = n as f32;
                        }
                    }
                    _ => {}
                }
            }
        }
        style
    }

    /// Resolves a `Len`-typed `p`/`m` attribute value into a `Spacing` quad
    /// (RFC-0005 §1 erratum), emitting span-anchored `CompileError`s
    /// for the four error classes:
    ///
    /// - an unknown side name → [`CompileError::UnknownAttribute`] with a hint;
    /// - a side set twice, an axis shorthand plus one of its component sides, or
    ///   a tuple mixing named and positional fields →
    ///   [`CompileError::ConflictingSpacingField`];
    /// - a non-numeric side value → [`CompileError::AttributeTypeMismatch`];
    /// - a positional tuple of arity 3 or > 4 → [`CompileError::ArityMismatch`].
    ///
    /// Accepted forms: scalar (`p: 5`), inferred pair (`p: (vertical, horizontal)`),
    /// inferred quad CSS `(top, right, bottom, left)`, and the verbose named form
    /// (`p: (top: 4, horizontal: 8)`). A single parenthesized value parses to the
    /// inner expression, so it arrives as a scalar.
    fn resolve_spacing(&mut self, expr: &Expr, prop: &str) -> byard_core::atlas::layout::Spacing {
        use byard_core::atlas::layout::Spacing;
        match expr {
            Expr::Tuple(args, span) => {
                let any_named = args.iter().any(|a| a.name.is_some());
                let all_named = args.iter().all(|a| a.name.is_some());
                if any_named && !all_named {
                    self.errors.push(CompileError::ConflictingSpacingField {
                        span: *span,
                        message: "a spacing tuple cannot mix named and positional fields"
                            .to_string(),
                    });
                    return Spacing::default();
                }
                if all_named {
                    self.resolve_named_spacing(args)
                } else {
                    self.resolve_positional_spacing(args, *span, prop)
                }
            }
            other => {
                let val = self.eval_pure(other);
                if let Some(v) = spacing_value(&val) {
                    Spacing::all(v)
                } else {
                    self.errors.push(CompileError::AttributeTypeMismatch {
                        span: other.span(),
                        expected: "a length (an integer)".to_string(),
                    });
                    Spacing::default()
                }
            }
        }
    }

    /// Verbose named spacing form (`p: (top: 4, horizontal: 8)`).
    fn resolve_named_spacing(&mut self, args: &[Arg]) -> byard_core::atlas::layout::Spacing {
        use byard_core::atlas::layout::Spacing;
        const SIDES: &[&str] = &["top", "bottom", "left", "right", "horizontal", "vertical"];

        let (mut top, mut right, mut bottom, mut left) = (None, None, None, None);
        for arg in args {
            // `all_named` guarantees a name is present.
            let Some(name) = &arg.name else { continue };
            let span = arg.value.span();
            let val = self.eval_pure(&arg.value);
            let Some(v) = spacing_value(&val) else {
                self.errors.push(CompileError::AttributeTypeMismatch {
                    span,
                    expected: "a length (an integer)".to_string(),
                });
                continue;
            };
            match name.as_str() {
                "top" => assign_side(&mut top, v, "top", span, &mut self.errors),
                "bottom" => assign_side(&mut bottom, v, "bottom", span, &mut self.errors),
                "left" => assign_side(&mut left, v, "left", span, &mut self.errors),
                "right" => assign_side(&mut right, v, "right", span, &mut self.errors),
                "horizontal" => {
                    assign_side(&mut left, v, "left", span, &mut self.errors);
                    assign_side(&mut right, v, "right", span, &mut self.errors);
                }
                "vertical" => {
                    assign_side(&mut top, v, "top", span, &mut self.errors);
                    assign_side(&mut bottom, v, "bottom", span, &mut self.errors);
                }
                unknown => {
                    let hint = crate::util::closest_match(unknown, SIDES.iter().copied())
                        .map(str::to_string);
                    self.errors.push(CompileError::UnknownAttribute {
                        span,
                        name: unknown.to_string(),
                        hint,
                    });
                }
            }
        }
        Spacing {
            top: top.unwrap_or(0.0),
            right: right.unwrap_or(0.0),
            bottom: bottom.unwrap_or(0.0),
            left: left.unwrap_or(0.0),
        }
    }

    /// Inferred positional spacing forms: pair `(vertical, horizontal)` or quad
    /// CSS `(top, right, bottom, left)`. Any other arity is an error.
    fn resolve_positional_spacing(
        &mut self,
        args: &[Arg],
        span: Span,
        prop: &str,
    ) -> byard_core::atlas::layout::Spacing {
        use byard_core::atlas::layout::Spacing;
        let mut vals = Vec::with_capacity(args.len());
        for arg in args {
            let val = self.eval_pure(&arg.value);
            if let Some(v) = spacing_value(&val) {
                vals.push(v);
            } else {
                self.errors.push(CompileError::AttributeTypeMismatch {
                    span: arg.value.span(),
                    expected: "a length (an integer)".to_string(),
                });
                vals.push(0.0);
            }
        }
        match vals.len() {
            2 => Spacing::symmetric(vals[0], vals[1]),
            4 => Spacing {
                top: vals[0],
                right: vals[1],
                bottom: vals[2],
                left: vals[3],
            },
            n => {
                self.errors.push(CompileError::ArityMismatch {
                    span,
                    name: prop.to_string(),
                    expected: 4,
                    found: n,
                });
                Spacing::default()
            }
        }
    }

    /// Resolves a `radius`-typed attribute into per-corner radii
    /// `[top_left, top_right, bottom_right, bottom_left]`, the exact order
    /// `BoxInstance::radii`/`TextureSampler::radii` expect (`frame.rs`).
    ///
    /// RFC-0005 §"Decoration" documents `radius: Len` as "scalar = all, quad =
    /// per-corner". Accepted forms: a scalar (`radius: 16`, all four
    /// corners) and the positional CSS-order quad (`radius: (4, 8, 12, 16)`).
    /// Unlike `p`/`m`'s generic `Len` contract, there is no pair shorthand and
    /// no named-field form for `radius`, the RFC documents only scalar/quad,
    /// so this resolver doesn't invent additional surface. A non-4 tuple
    /// arity is a `CompileError::ArityMismatch`; a non-numeric corner is an
    /// `AttributeTypeMismatch`; a named field is a `ConflictingSpacingField`
    /// (reusing the existing diagnostic, the message states the real cause).
    /// Resolves the `shadow` attribute into zero or more drop shadows
    /// (RFC-0011 custom shadows). Accepts a preset token (`sm`/`md`/`lg`/`none`),
    /// a single tuple, named `(y: 4, blur: 8, spread: 0, color: 0x…)` or
    /// positional `(x, y, blur, spread, color)`, or an array of tuples for
    /// CSS-style layered shadows.
    fn resolve_shadows(&mut self, attrs: &[Attr]) -> Vec<ShadowSpec> {
        let Some(value) = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "shadow" => Some(value),
            _ => None,
        }) else {
            return Vec::new();
        };
        match value {
            // Layered shadows: first-listed paints on top (CSS order), so the
            // caller emits them reversed to sit nearest.
            Expr::Array(items, _) => items
                .iter()
                .filter_map(|e| self.shadow_from_expr(e))
                .collect(),
            other => self.shadow_from_expr(other).into_iter().collect(),
        }
    }

    /// One shadow from a tuple, or a preset token; `None` for `none`/unknown.
    fn shadow_from_expr(&mut self, value: &Expr) -> Option<ShadowSpec> {
        if let Expr::Tuple(args, _) = value {
            return Some(self.shadow_from_tuple(args));
        }
        // A preset token (`sm`/`md`/`lg`); `none`/anything else → no shadow.
        let (dy, blur) = match self.eval_pure(value) {
            Value::Str(t) => match t.as_str() {
                "sm" => (1.0, 3.0),
                "md" => (3.0, 8.0),
                "lg" => (6.0, 16.0),
                _ => return None,
            },
            _ => return None,
        };
        // Preset alpha scales gently with size (sm 0x44 → lg 0x66).
        #[allow(clippy::cast_possible_truncation)]
        let alpha = (0x44 + (blur as i64 - 3) * 2).clamp(0x44, 0x66);
        Some(ShadowSpec {
            dx: 0.0,
            dy,
            blur,
            spread: 0.0,
            color: super::intrinsics::color_to_rgba(alpha << 24, true),
        })
    }

    /// Builds a [`ShadowSpec`] from a `shadow` tuple. Named fields (`x`/`dx`,
    /// `y`/`dy`, `blur`, `spread`, `color`) take any order; a positional tuple
    /// maps by slot `(x, y, blur, spread, color)`, each optional (later slots
    /// default), with `color` always the fifth slot so it is unambiguous.
    fn shadow_from_tuple(&mut self, args: &[Arg]) -> ShadowSpec {
        let mut s = ShadowSpec {
            dx: 0.0,
            dy: 0.0,
            blur: 0.0,
            spread: 0.0,
            color: super::intrinsics::color_to_rgba(DEFAULT_SHADOW_COLOR, true),
        };
        if args.iter().any(|a| a.name.is_some()) {
            for a in args {
                let Some(field) = a.name.as_ref().map(crate::Symbol::as_str) else {
                    continue;
                };
                match field {
                    "x" | "dx" => s.dx = self.eval_num(&a.value),
                    "y" | "dy" => s.dy = self.eval_num(&a.value),
                    "blur" => s.blur = self.eval_num(&a.value),
                    "spread" => s.spread = self.eval_num(&a.value),
                    "color" => s.color = self.eval_shadow_color(&a.value),
                    _ => {}
                }
            }
        } else {
            for (i, a) in args.iter().enumerate() {
                match i {
                    0 => s.dx = self.eval_num(&a.value),
                    1 => s.dy = self.eval_num(&a.value),
                    2 => s.blur = self.eval_num(&a.value),
                    3 => s.spread = self.eval_num(&a.value),
                    4 => s.color = self.eval_shadow_color(&a.value),
                    _ => {}
                }
            }
        }
        s
    }

    /// Resolves the `gradient` prop into a [`Gradient`](byard_core::frame::Gradient)
    ///, a linear colour ramp painted over the element's fill (RFC-0001 §3.1's
    /// `DecoratedBox` remit).
    ///
    /// Surface (named fields, any order, all optional):
    /// `gradient: (angle: 90deg, from: 0x00FFFFFF, mid: 0x33FFFFFF, to: 0x00FFFFFF, mid_pos: 0.5)`.
    /// A bare `mid` is what makes a *band* (transparent → bright → transparent)
    /// expressible; omit it and the ramp is an ordinary two-stop fade. The
    /// separate `gradient_offset` prop shifts the ramp along its axis and, being
    /// an ordinary numeric prop, animates through the RFC-0010/RFC-0025
    /// chokepoints, a looping offset is a travelling sweep.
    ///
    /// RFC-0035 adds a `kind`, and with it the two fields the other shapes
    /// need:
    ///
    /// ```text
    /// gradient: (kind: radial, center: (1.0, 0.0), radius: 0.9, from: …, to: …)
    /// gradient: (kind: conic, center: (0.5, 0.5), start: -90deg, from: …, mid: …, to: …)
    /// ```
    ///
    /// `kind` is absent in every file written before this, and absent means
    /// `linear`, so nothing already written changes meaning, which is the one
    /// thing RFC-0035's resolved question about the existing surface asks for.
    fn resolve_gradient(&mut self, attrs: &[Attr]) -> Option<byard_core::frame::Gradient> {
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "gradient" => Some(value),
            _ => None,
        })?;
        let value = value.clone();
        let offset = self
            .eval_float_prop(attrs, "gradient_offset")
            .map_or(0.0, |v| v as f32);
        self.resolve_gradient_expr(&value, offset)
    }

    /// The gradient a `(kind: …, from: …, to: …)` tuple describes.
    ///
    /// Split out from [`resolve_gradient`](Self::resolve_gradient) so a filled
    /// path reads its gradient through the *same* parser a box does
    /// (RFC-0037 resolved question: share the descriptor, do not fork it). The
    /// alternative is two parsers that agree until one of them learns
    /// something, which is how a path gradient and a box gradient end up
    /// meaning different things by the same words.
    fn resolve_gradient_expr(
        &mut self,
        value: &Expr,
        offset: f32,
    ) -> Option<byard_core::frame::Gradient> {
        let Expr::Tuple(args, span) = value else {
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: value.span(),
                expected: "a gradient, e.g. `(angle: 90deg, from: 0x00FFFFFF, to: 0xFFFFFFFF)`"
                    .to_string(),
            });
            return None;
        };
        let mut angle = 0.0_f32;
        let (mut from, mut to, mut mid) = (None, None, None);
        let mut mid_pos = 0.5_f32;
        let mut kind = byard_core::frame::GradientKind::Linear;
        let mut center = [0.5_f32, 0.5_f32];
        let mut radius = 0.5_f32;
        let mut radius_written = false;
        let mut start_span = None;
        for arg in args {
            let Some(field) = arg.name.as_ref().map(crate::Symbol::as_str) else {
                self.errors.push(CompileError::ConflictingSpacingField {
                    span: *span,
                    message: "`gradient` takes named fields \
                              (angle / from / mid / to / mid_pos)"
                        .to_string(),
                });
                return None;
            };
            match field {
                "angle" => angle = self.eval_num(&arg.value),
                "from" => from = Some(self.eval_gradient_stop(&arg.value)),
                "to" => to = Some(self.eval_gradient_stop(&arg.value)),
                "mid" => mid = Some(self.eval_gradient_stop(&arg.value)),
                "mid_pos" => mid_pos = self.eval_num(&arg.value).clamp(0.0, 1.0),
                // RFC-0035. `start` is the conic sweep's own name for the
                // angle it begins at; it writes the same field, because a
                // gradient has exactly one angle and two spellings for it
                // would be two things to keep in step.
                "start" => {
                    angle = self.eval_num(&arg.value);
                    start_span = Some(arg.value.span());
                }
                "kind" => {
                    kind = match Self::enum_token(&arg.value) {
                        Some("radial") => byard_core::frame::GradientKind::Radial,
                        Some("conic") => byard_core::frame::GradientKind::Conic,
                        Some("linear") => byard_core::frame::GradientKind::Linear,
                        other => {
                            let hint = other
                                .and_then(|o| {
                                    crate::util::closest_match(o, ["linear", "radial", "conic"])
                                })
                                .map(String::from);
                            self.errors.push(CompileError::UnknownAttribute {
                                span: arg.value.span(),
                                name: format!("gradient.kind = {}", other.unwrap_or("?")),
                                hint,
                            });
                            byard_core::frame::GradientKind::Linear
                        }
                    };
                }
                "center" => {
                    let (x, y) = self.resolve_axis_pair_value(&arg.value, (center[0], center[1]));
                    center = [x, y];
                }
                "radius" => {
                    radius = self.eval_num(&arg.value);
                    radius_written = true;
                }
                unknown => {
                    let hint = crate::util::closest_match(
                        unknown,
                        [
                            "angle", "from", "mid", "to", "mid_pos", "kind", "center", "radius",
                            "start",
                        ],
                    )
                    .map(String::from);
                    self.errors.push(CompileError::UnknownAttribute {
                        span: arg.value.span(),
                        name: format!("gradient.{unknown}"),
                        hint: hint.map(|h| format!("gradient.{h}")),
                    });
                }
            }
        }
        // A ramp needs two ends; a single stop is a flat wash the caller could
        // have written as `bg`, so it is a mistake worth naming.
        let (Some(from), Some(to)) = (from, to) else {
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: *span,
                expected: "a gradient with both `from:` and `to:` colours".to_string(),
            });
            return None;
        };
        // RFC-0035 §Compiler validation, all three of it.
        if kind == byard_core::frame::GradientKind::Radial && radius <= 0.0 {
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: *span,
                expected: if radius_written {
                    "a radial gradient with a positive `radius:` (it is a \
                     fraction of the element's half-diagonal, so `0` paints \
                     nothing but the last stop)"
                        .to_string()
                } else {
                    "a radial gradient with a `radius:`".to_string()
                },
            });
        }
        // An angle outside one turn is not wrong, it is just written the long
        // way; the shader wraps anyway, and normalising here keeps the encoded
        // bytes of `start: -90deg` and `start: 270deg` identical, so the paint
        // digest sees one value rather than two spellings of it.
        if kind == byard_core::frame::GradientKind::Conic {
            let tau = std::f32::consts::TAU;
            angle = angle.rem_euclid(tau);
            let _ = start_span;
        }
        Some(byard_core::frame::Gradient {
            kind,
            angle,
            center,
            radius,
            from,
            mid: mid.unwrap_or_else(|| std::array::from_fn(|i| f32::midpoint(from[i], to[i]))),
            to,
            mid_pos,
            offset,
        })
    }

    /// Evaluates one gradient stop colour to linear RGBA. A `with`-animated stop
    /// is driven through the OKLab colour path (RFC-0010 A3) like any other
    /// animated colour, so a gradient can crossfade.
    fn eval_gradient_stop(&mut self, expr: &Expr) -> [f32; 4] {
        let packed = match expr {
            Expr::Animated {
                value: target,
                anim,
                span,
            } => self.eval_animated_color(target, anim, self.anim_key(*span)),
            other if crate::interp::anim::is_keyframes_call(other) => {
                self.eval_keyframe_color(other).unwrap_or(0)
            }
            other => self.eval_pure(other).as_int().unwrap_or(0),
        };
        super::intrinsics::color_rgba_auto(packed)
    }

    /// Evaluates a numeric shadow field (offset/blur/spread) to `f32`.
    #[allow(clippy::cast_possible_truncation)]
    fn eval_num(&mut self, e: &Expr) -> f32 {
        match self.eval_pure(e) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => 0.0,
        }
    }

    /// Evaluates a shadow `color` field (a `0xAARRGGBB` literal) to RGBA.
    fn eval_shadow_color(&mut self, e: &Expr) -> [f32; 4] {
        let packed = self.eval_pure(e).as_int().unwrap_or(DEFAULT_SHADOW_COLOR);
        super::intrinsics::color_to_rgba(packed, true)
    }

    /// The combine mode a `Canvas` declares, and its parameter (RFC-0031 §S4).
    ///
    /// `fuse: <px>` is the smoothing radius, the distance over which two
    /// surfaces bridge into one. `morph: <scalar>` is the sequence mode: the
    /// canvas's shapes become an ordered set and the scalar indexes it. They
    /// are mutually exclusive (§Q4, diagnosed at check time); `fuse` wins here
    /// so a source that slipped past the check still renders one thing rather
    /// than something mode-dependent.
    ///
    /// Both read through `eval_float_prop`, which puts them on the RFC-0010
    /// animation chokepoint, the whole design. The Material 3 loader is seven
    /// shapes and *one* animated scalar; an animating fusion is new
    /// per-instance data and never a re-tessellation.
    fn resolve_group_mode(&mut self, attrs: &[Attr]) -> Option<(u32, f32)> {
        #[allow(clippy::cast_possible_truncation)]
        if let Some(k) = self.eval_float_prop(attrs, "fuse") {
            return Some((byard_core::frame::GROUP_FUSE, (k as f32).max(0.0)));
        }
        #[allow(clippy::cast_possible_truncation)]
        self.eval_float_prop(attrs, "morph")
            .map(|phase| (byard_core::frame::GROUP_MORPH, phase as f32))
    }

    /// Pushes a collected shape group's head (RFC-0031 §S4).
    ///
    /// The head's `params` are the union of its members' bounds expressed as a
    /// rect, because `CanvasShape::bounds` is what sizes the instance quad and
    /// §S4 makes a head's quad the union rather than its own geometry. Its
    /// stroke, cap, dash and opacity come from the first member: a group has
    /// one outline (§S8) and there is no group-level stroke syntax, so the
    /// first shape's paint is the group's.
    #[allow(clippy::too_many_arguments)]
    fn push_shape_group(
        &mut self,
        mode: u32,
        param: f32,
        sink: ShapeGroupSink,
        opacity: f32,
        transform: byard_core::frame::Transform,
        span: Span,
        frame: &mut byard_core::frame::RenderFrame,
    ) {
        use byard_core::frame::{CANVAS_SHAPE_RECT, CanvasShape, MAX_GROUP_MEMBERS};
        let (Some(bounds), Some(paint)) = (sink.bounds, sink.paint) else {
            return; // an empty group draws nothing
        };
        // §S4: fusion bulges *outward* by up to the smoothing radius, so the
        // union quad is inflated by `k`, an under-inflated quad clips exactly
        // the bridge the feature exists to draw. Morphing never leaves its
        // members' union, so it pays nothing for this.
        let inflate = if mode == byard_core::frame::GROUP_FUSE {
            param.max(0.0)
        } else {
            0.0
        };
        let bounds = byard_core::frame::Rect::new(
            bounds.x - inflate,
            bounds.y - inflate,
            bounds.width + inflate * 2.0,
            bounds.height + inflate * 2.0,
        );
        // The written-shape count is diagnosed with a source position by
        // `validate_canvas`. This is the other half: a `for` inside the body
        // can generate members from data, and how many is only knowable here.
        // Deduped, because the render walk runs every tick and a diagnostic
        // that repeats sixty times a second is noise rather than information.
        if sink.overflowed > MAX_GROUP_MEMBERS
            && !self
                .errors
                .iter()
                .any(|e| matches!(e, CompileError::TooManyGroupMembers { .. }))
        {
            self.errors.push(CompileError::TooManyGroupMembers {
                span,
                max: MAX_GROUP_MEMBERS,
                found: sink.overflowed,
            });
        }
        frame.push_shape_group(
            CanvasShape {
                // The head's own kind is never evaluated, the shader branches
                // on the combine mode first, but `bounds()` is, so a rect
                // carrying the union is exactly the quad §S4 asks for.
                kind: CANVAS_SHAPE_RECT,
                params: [
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ],
                stroke_color: paint.stroke_color,
                fill_color: paint.fill_color,
                stroke_width: paint.stroke_width,
                cap: paint.cap,
                dash: paint.dash,
                dash_offset: paint.dash_offset,
                opacity,
                transform,
                group_mode: mode,
                group_param: param,
                dirty: true,
                ..CanvasShape::default()
            },
            &sink.members,
        );
    }

    /// The element's corner smoothing (RFC-0031 §S1), clamped to `0..=1`.
    ///
    /// It resolves through `eval_float_prop`, so it passes the RFC-0010
    /// animation chokepoint like every other paint scalar: `smooth: 0.6 with
    /// anim.spring()` interpolates with no plumbing of its own, and, being
    /// paint-class, never marks the layout tree.
    ///
    /// Absent means `0.0`, which the shaders short-circuit to the L² field they
    /// evaluated before this property existed.
    #[allow(clippy::cast_possible_truncation)]
    fn resolve_smooth(&mut self, attrs: &[Attr]) -> f32 {
        self.eval_float_prop(attrs, "smooth")
            .map_or(0.0, |v| (v as f32).clamp(0.0, 1.0))
    }

    fn resolve_radii(&mut self, attrs: &[Attr], name: &str) -> [f32; 4] {
        let Some(attr) = attrs.iter().find(|a| a.name.as_str() == name) else {
            return [0.0; 4];
        };
        let AttrKind::Prop { value } = &attr.kind else {
            return [0.0; 4];
        };
        match value {
            Expr::Tuple(args, span) => {
                if args.iter().any(|a| a.name.is_some()) {
                    self.errors.push(CompileError::ConflictingSpacingField {
                        span: *span,
                        message: format!(
                            "`{name}` does not accept named corner fields; use a \
                             positional quad (top_left, top_right, bottom_right, \
                             bottom_left)"
                        ),
                    });
                    return [0.0; 4];
                }
                if args.len() != 4 {
                    self.errors.push(CompileError::ArityMismatch {
                        span: *span,
                        name: name.to_string(),
                        expected: 4,
                        found: args.len(),
                    });
                    return [0.0; 4];
                }
                let mut radii = [0.0_f32; 4];
                for (slot, arg) in radii.iter_mut().zip(args) {
                    let val = self.eval_pure(&arg.value);
                    if let Some(v) = spacing_value(&val) {
                        *slot = v;
                    } else {
                        self.errors.push(CompileError::AttributeTypeMismatch {
                            span: arg.value.span(),
                            expected: "a length (an integer)".to_string(),
                        });
                    }
                }
                radii
            }
            other => {
                let val = self.eval_pure(other);
                if let Some(v) = spacing_value(&val) {
                    [v; 4]
                } else {
                    self.errors.push(CompileError::AttributeTypeMismatch {
                        span: other.span(),
                        expected: "a length (an integer)".to_string(),
                    });
                    [0.0; 4]
                }
            }
        }
    }

    /// Resolves the paint-time transform attributes (RFC-0011:
    /// `translate`/`scale`/`rotate`/`origin`; `opacity` stays on its own
    /// existing path, see the doc comment on `DecoratedBox`/`Transform` in
    /// `frame.rs` for why). `rect` is the element's own laid-out rect,
    /// logical pixels, needed to resolve a token/fractional `origin` into an
    /// absolute pivot.
    fn resolve_transform(
        &mut self,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
    ) -> byard_core::frame::Transform {
        let translate = self.resolve_axis_pair(attrs, "translate", (0.0, 0.0));
        let scale = self.resolve_axis_pair(attrs, "scale", (1.0, 1.0));
        let rotate = self.resolve_rotate(attrs).unwrap_or(0.0);
        let origin = self.resolve_origin(attrs, rect);
        byard_core::frame::Transform {
            translate: [translate.0, translate.1],
            scale: [scale.0, scale.1],
            rotate,
            origin: [origin.0, origin.1],
            opacity: 1.0,
        }
    }

    /// Resolves a two-axis prop (`translate`/`scale`) to `(x, y)`, RFC-0011's
    /// dual surface: a bare scalar fills both axes; `(a, b)` binds positionally;
    /// `(x: a, y: b)` sets any subset, order-independent, leaving the rest at
    /// `default`. The sub-property form (`name.x: v` / `name.y: v`, a separate
    /// `Attr` with `axis: Some(_)`) then overrides individual axes on top of
    /// whatever the base `name: value` attribute (if any) already resolved,
    /// so `translate.y: 2` alone is exactly `translate: (y: 2)`.
    fn resolve_axis_pair(&mut self, attrs: &[Attr], name: &str, default: (f32, f32)) -> (f32, f32) {
        let mut result = default;
        if let Some(attr) = attrs
            .iter()
            .find(|a| a.name.as_str() == name && a.axis.is_none())
        {
            if let AttrKind::Prop { value } = &attr.kind {
                result = self.resolve_axis_pair_value(value, default);
            }
        }
        for attr in attrs
            .iter()
            .filter(|a| a.name.as_str() == name && a.axis.is_some())
        {
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            let span = value.span();
            let val = self.eval_pure(value);
            let Some(v) = spacing_value(&val) else {
                self.errors.push(CompileError::AttributeTypeMismatch {
                    span,
                    expected: "a number".to_string(),
                });
                continue;
            };
            let Some(axis) = attr.axis.as_ref() else {
                continue;
            };
            match axis.as_str() {
                "x" => result.0 = v,
                "y" => result.1 = v,
                unknown => {
                    let hint = crate::util::closest_match(unknown, ["x", "y"]).map(String::from);
                    self.errors.push(CompileError::UnknownAttribute {
                        span: attr.span,
                        name: format!("{name}.{unknown}"),
                        hint: hint.map(|h| format!("{name}.{h}")),
                    });
                }
            }
        }
        result
    }

    /// Parses one `translate`/`scale`-shaped [`Expr`] (scalar, positional
    /// tuple, or named tuple) into `(x, y)`, the value-shape half of
    /// [`Self::resolve_axis_pair`], factored out so [`Self::resolve_origin`]
    /// can reuse the exact same tuple grammar for its own fractional pair.
    fn resolve_axis_pair_value(&mut self, value: &Expr, default: (f32, f32)) -> (f32, f32) {
        match value {
            Expr::Tuple(args, span) => {
                let any_named = args.iter().any(|a| a.name.is_some());
                let all_named = args.iter().all(|a| a.name.is_some());
                if any_named && !all_named {
                    self.errors.push(CompileError::ConflictingSpacingField {
                        span: *span,
                        message: "cannot mix named and positional fields".to_string(),
                    });
                    return default;
                }
                if all_named {
                    let (mut x, mut y) = (None, None);
                    for arg in args {
                        let Some(name) = &arg.name else { continue };
                        let span = arg.value.span();
                        let val = self.eval_pure(&arg.value);
                        let Some(v) = spacing_value(&val) else {
                            self.errors.push(CompileError::AttributeTypeMismatch {
                                span,
                                expected: "a number".to_string(),
                            });
                            continue;
                        };
                        match name.as_str() {
                            "x" => assign_side(&mut x, v, "x", span, &mut self.errors),
                            "y" => assign_side(&mut y, v, "y", span, &mut self.errors),
                            unknown => {
                                let hint = crate::util::closest_match(unknown, ["x", "y"])
                                    .map(String::from);
                                self.errors.push(CompileError::UnknownAttribute {
                                    span,
                                    name: unknown.to_string(),
                                    hint,
                                });
                            }
                        }
                    }
                    (x.unwrap_or(default.0), y.unwrap_or(default.1))
                } else if args.len() == 2 {
                    let x = self.eval_pure(&args[0].value);
                    let y = self.eval_pure(&args[1].value);
                    let x = spacing_value(&x).unwrap_or_else(|| {
                        self.errors.push(CompileError::AttributeTypeMismatch {
                            span: args[0].value.span(),
                            expected: "a number".to_string(),
                        });
                        default.0
                    });
                    let y = spacing_value(&y).unwrap_or_else(|| {
                        self.errors.push(CompileError::AttributeTypeMismatch {
                            span: args[1].value.span(),
                            expected: "a number".to_string(),
                        });
                        default.1
                    });
                    (x, y)
                } else {
                    self.errors.push(CompileError::ArityMismatch {
                        span: *span,
                        name: "translate/scale/origin".to_string(),
                        expected: 2,
                        found: args.len(),
                    });
                    default
                }
            }
            other => {
                let val = self.eval_pure(other);
                if let Some(v) = spacing_value(&val) {
                    (v, v)
                } else if let Some(pair) = axis_pair_of_value(&val, default) {
                    // A *computed* pair, what keyframed coordinates resolve to
                    // (RFC-0025: `translate: anim.keyframes(0%: (-100, 0), …)`
                    // blends component-wise and arrives here as a tuple value).
                    pair
                } else {
                    self.errors.push(CompileError::AttributeTypeMismatch {
                        span: other.span(),
                        expected: "a number".to_string(),
                    });
                    default
                }
            }
        }
    }

    /// Resolves `rotate` (RFC-0011): the terse `rotate: 90deg` form or the
    /// verbose `rotate: (angle: 90deg)` single-field tuple, both already
    /// canonicalized to radians by the lexer's `Expr::AngleLit`. Absent →
    /// `None` (caller defaults to `0.0`, no rotation).
    fn resolve_rotate(&mut self, attrs: &[Attr]) -> Option<f32> {
        let attr = attrs
            .iter()
            .find(|a| a.name.as_str() == "rotate" && a.axis.is_none())?;
        let AttrKind::Prop { value } = &attr.kind else {
            return None;
        };
        let inner = match value {
            Expr::Tuple(args, _)
                if args.len() == 1
                    && args[0].name.as_ref().map(Symbol::as_str) == Some("angle") =>
            {
                &args[0].value
            }
            other => other,
        };
        let val = self.eval_pure(inner);
        let Some(rad) = spacing_value(&val) else {
            // A non-numeric `rotate` (e.g. `rotate: center`, or a reactive var
            // that didn't resolve to a number) is a real mistake, not a no-op,
            // flag it the same way `translate`/`scale` flag theirs instead of
            // silently painting with no rotation.
            self.errors.push(CompileError::AttributeTypeMismatch {
                span: inner.span(),
                expected: "an angle (e.g. 90deg, 1.5rad)".to_string(),
            });
            return None;
        };
        Some(rad)
    }

    /// Resolves `origin` (RFC-0011 T2) to an absolute logical-pixel pivot in
    /// the same coordinate space as `rect`: a named token (`center` and the
    /// four corners/edges), or a fractional `(fx, fy)` tuple relative to
    /// `rect` (positional or named, reusing [`Self::resolve_axis_pair_value`]'s
    /// tuple grammar). Absent, or an unrecognized token, defaults to `center`
    ///, RFC-0011's own stated default, rather than hard-failing.
    ///
    /// Deliberately out of scope for now: the `px` absolute-origin suffix
    /// (T2's third form) needs a new lexer literal this slice doesn't add;
    /// only the token and fractional forms are implemented.
    fn resolve_origin(
        &mut self,
        attrs: &[Attr],
        rect: crate::interp::intrinsics::Rect,
    ) -> (f32, f32) {
        let center = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let Some(attr) = attrs
            .iter()
            .find(|a| a.name.as_str() == "origin" && a.axis.is_none())
        else {
            return center;
        };
        let AttrKind::Prop { value } = &attr.kind else {
            return center;
        };
        if let Expr::Ident(sym, span) = value {
            const TOKENS: &[&str] = &[
                "center",
                "top_left",
                "top_right",
                "bottom_left",
                "bottom_right",
                "top",
                "bottom",
                "left",
                "right",
            ];
            return match sym.as_str() {
                "center" => center,
                "top_left" => (rect.x, rect.y),
                "top_right" => (rect.x + rect.w, rect.y),
                "bottom_left" => (rect.x, rect.y + rect.h),
                "bottom_right" => (rect.x + rect.w, rect.y + rect.h),
                "top" => (rect.x + rect.w * 0.5, rect.y),
                "bottom" => (rect.x + rect.w * 0.5, rect.y + rect.h),
                "left" => (rect.x, rect.y + rect.h * 0.5),
                "right" => (rect.x + rect.w, rect.y + rect.h * 0.5),
                unknown => {
                    let hint = crate::util::closest_match(unknown, TOKENS.iter().copied())
                        .map(String::from);
                    self.errors.push(CompileError::UnknownAttribute {
                        span: *span,
                        name: format!("origin: {unknown}"),
                        hint,
                    });
                    center
                }
            };
        }
        let (fx, fy) = self.resolve_axis_pair_value(value, (0.5, 0.5));
        (rect.x + fx * rect.w, rect.y + fy * rect.h)
    }

    /// Checks every `anchor_to:` against the `as <name>` tags declared before
    /// it, in lexical order (RFC-0036).
    ///
    /// Lexical order is the whole guarantee: an overlay may only anchor to a
    /// name that appears earlier in the same view, which makes an anchor cycle
    /// impossible to write rather than something to detect at runtime, and
    /// means the anchor's rect is already a fact by the time the overlay pass
    /// places against it.
    fn check_anchor_refs(&mut self, view: &ViewDecl) {
        let mut declared: Vec<String> = Vec::new();
        Self::walk_anchor_members(&view.body, &mut declared, &mut self.errors);
    }

    /// Walks members in written order, collecting `as` tags and checking
    /// `anchor_to:` against what has been collected so far.
    fn walk_anchor_members(
        members: &[Member],
        declared: &mut Vec<String>,
        errors: &mut Vec<CompileError>,
    ) {
        for member in members {
            match member {
                Member::Element(el) => Self::walk_anchor_element(el, declared, errors),
                Member::For { body, .. } | Member::Route { body, .. } => {
                    Self::walk_anchor_members(body, declared, errors);
                }
                Member::When { then, els, .. } => {
                    Self::walk_anchor_members(then, declared, errors);
                    if let Some(e) = els {
                        Self::walk_anchor_members(e, declared, errors);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk_anchor_element(
        el: &ElementNode,
        declared: &mut Vec<String>,
        errors: &mut Vec<CompileError>,
    ) {
        for attr in &el.attrs {
            if attr.name.as_str() != "anchor_to" {
                continue;
            }
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            // Only a literal name can be checked. A computed one is allowed
            // through rather than rejected: the check exists to catch typos,
            // not to forbid the (rare, deliberate) dynamic case.
            let Expr::StrLit(parts, span) = value else {
                continue;
            };
            let [crate::parser::ast::StrPart::Text(name)] = parts.as_slice() else {
                continue;
            };
            if !declared.iter().any(|d| d == name) {
                errors.push(CompileError::UnknownAnchor {
                    span: *span,
                    name: name.clone(),
                    hint: closest_anchor(name, declared),
                });
            }
        }
        // Recorded *after* its own attrs are checked, so an element cannot
        // anchor to itself.
        if let Some(name) = &el.anchor_name {
            declared.push(name.as_str().to_string());
        }
        Self::walk_anchor_members(&el.children, declared, errors);
    }

    /// Processes a whole `View`: its declarations first (so bindings can resolve
    /// names), then lowers its top-level elements into a render tree, handling
    /// `when`/`for` structural members (M20).
    pub fn lower_view(&mut self, view: &ViewDecl, known_views: &[&str]) -> Vec<RenderNode> {
        self.check_anchor_refs(view);
        // RFC-0018: a fresh tree gets fresh `when`/`for` pools; the previous
        // tree's pool ids are discarded with it (hot-reload re-lowers the tree).
        self.for_pools.clear();
        self.when_pools.clear();
        // RFC-0026: likewise the navigation stacks, a re-lowered tree carries
        // fresh pool ids, and a stale pool would keep a discarded screen alive.
        self.nav_pools.clear();
        self.nav_elems.clear();
        self.nav_shared.clear();
        // RFC-0039: the previous tree's native views die with it, which is
        // their unmount. Each is told, in mount order, and the generation
        // moves so a reply still in flight for one of them finds the door
        // closed rather than the next widget to take its slot.
        for view in &mut self.native_views {
            view.on_unmount();
        }
        self.native_views.clear();
        self.native_call_owners.clear();
        self.native_generation = self.native_generation.wrapping_add(1);
        self.eval_view_decls(view);
        // A view that declares a `content` slot (RFC-0007 D-A) may reference it in
        // its body. When the view is lowered *standalone*, e.g. `byard check`
        // validates each `ViewDecl` independently, or a slot view is a root,
        // there is no calling instance, so push an empty slot frame: the bare
        // `content` reference then splices nothing instead of being mistaken for
        // an `UnknownView`. A real call (`lower_user_view_call`) pushes the
        // caller's block over this before lowering the body.
        let has_content_slot = view
            .params
            .iter()
            .any(|p| p.name.as_str() == RESERVED_CONTENT);
        if has_content_slot {
            self.slot_stack.push(Vec::new());
        }
        let nodes = self.lower_members(&view.body, known_views);
        if has_content_slot {
            self.slot_stack.pop();
        }
        nodes
    }

    // ── lowering ────────────────────────────────────────────────────────

    /// Lowers `expr` to a reactive computation against the current environment.
    fn lower_expr(&mut self, expr: &Expr, payload_name: Option<&Symbol>) -> Lowered {
        match expr {
            Expr::IntLit(n, _) => {
                let n = *n;
                Box::new(move |_| Value::Int(n))
            }
            Expr::FloatLit(f, _) => {
                let f = *f;
                Box::new(move |_| Value::Float(f))
            }
            // Already canonicalized to radians by the lexer (RFC-0011 T1),
            // from here on an angle is just a plain `Float`.
            Expr::AngleLit(rad, _) => {
                let rad = *rad;
                Box::new(move |_| Value::Float(rad))
            }
            Expr::StrLit(parts, _) => self.lower_strlit(parts, payload_name),
            Expr::Ident(name, span) => {
                // A bare reference to a callback prop (RFC-0019 §4), reached only
                // when it is *not* the callee of a call (`on_tap()` is handled in
                // `lower_call`), is invalid: callbacks are fire-and-forget, not
                // first-class values.
                if let Some(&Value::Fn(id)) = self.env.lookup(name) {
                    if self.fn_table.get(id.0 as usize).is_some_and(|e| e.2) {
                        self.errors.push(CompileError::CallbackNotInvocable {
                            span: *span,
                            name: name.as_str().to_string(),
                        });
                    }
                }
                self.lower_ident(name, payload_name)
            }
            Expr::Array(elems, _) => {
                let mut cs: Vec<Lowered> = elems
                    .iter()
                    .map(|e| self.lower_expr(e, payload_name))
                    .collect();
                Box::new(move |ctx| Value::List(cs.iter_mut().map(|c| c(ctx)).collect()))
            }
            Expr::Tuple(elems, _) => {
                let mut cs: Vec<(Option<Symbol>, Lowered)> = elems
                    .iter()
                    .map(|arg| (arg.name.clone(), self.lower_expr(&arg.value, payload_name)))
                    .collect();
                Box::new(move |ctx| {
                    Value::Tuple(
                        cs.iter_mut()
                            .map(|(name, c)| (name.clone(), c(ctx)))
                            .collect(),
                    )
                })
            }
            Expr::Ternary {
                cond, then, els, ..
            } => {
                let mut cc = self.lower_expr(cond, payload_name);
                let mut tc = self.lower_expr(then, payload_name);
                let mut ec = self.lower_expr(els, payload_name);
                Box::new(move |ctx| {
                    if cc(ctx).as_bool().unwrap_or(false) {
                        tc(ctx)
                    } else {
                        ec(ctx)
                    }
                })
            }
            // Binary operators. Arithmetic (`+ - * /`, RFC-0020) promotes
            // Int↔Float; comparison (`== != < <= > >=`, RFC-0027 §1) yields a
            // `Bool`; `+` also does string/list concat (RFC-0027 §3/§4). The
            // short-circuiting `&&`/`||` (RFC-0027 §2) are lowered as control
            // flow so the un-taken side is neither evaluated nor read-tracked.
            Expr::Binary { op, lhs, rhs, .. } => {
                let op = *op;
                let mut lc = self.lower_expr(lhs, payload_name);
                let mut rc = self.lower_expr(rhs, payload_name);
                match op {
                    BinOp::And => Box::new(move |ctx| {
                        // Evaluate, and thereby read-track, the RHS only when
                        // the LHS is true (RFC-0027 §2, mirrors `when`).
                        if lc(ctx).as_bool().unwrap_or(false) {
                            Value::Bool(rc(ctx).as_bool().unwrap_or(false))
                        } else {
                            Value::Bool(false)
                        }
                    }),
                    BinOp::Or => Box::new(move |ctx| {
                        if lc(ctx).as_bool().unwrap_or(false) {
                            Value::Bool(true)
                        } else {
                            Value::Bool(rc(ctx).as_bool().unwrap_or(false))
                        }
                    }),
                    _ => Box::new(move |ctx| eval_binary(op, lc(ctx), rc(ctx))),
                }
            }
            // Prefix unary (`!b`, `-x`), RFC-0027 §2. `!` negates a `Bool`;
            // `-` negates a numeric. A type mismatch degrades to `Unit`
            // (the checker reports it, INV-4: no panic).
            Expr::Unary { op, rhs, .. } => {
                let op = *op;
                let mut rc = self.lower_expr(rhs, payload_name);
                Box::new(move |ctx| match (op, rc(ctx)) {
                    (UnOp::Not, Value::Bool(b)) => Value::Bool(!b),
                    (UnOp::Neg, Value::Int(n)) => Value::Int(n.wrapping_neg()),
                    (UnOp::Neg, Value::Float(f)) => Value::Float(-f),
                    _ => Value::Unit,
                })
            }
            // Indexing `base[index]` (RFC-0027 §4). Out-of-range or a
            // non-list/non-int index degrades to `Unit` (INV-4), never a panic.
            Expr::Index { base, index, .. } => {
                let mut bc = self.lower_expr(base, payload_name);
                let mut ic = self.lower_expr(index, payload_name);
                Box::new(move |ctx| index_value(&bc(ctx), &ic(ctx)))
            }
            // A record literal `{ ..spread, k: v }` (RFC-0027 §6): the spread's
            // fields seed the record, then written fields append/override, all
            // keeping declaration order.
            Expr::Record { fields, spread, .. } => {
                let mut spread_c = spread.as_ref().map(|s| self.lower_expr(s, payload_name));
                let mut field_cs: Vec<(Symbol, Lowered)> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.lower_expr(v, payload_name)))
                    .collect();
                Box::new(move |ctx| {
                    let mut out: Vec<(Symbol, Value)> = match spread_c.as_mut().map(|c| c(ctx)) {
                        Some(Value::Record(fs)) => fs,
                        _ => Vec::new(),
                    };
                    for (k, c) in &mut field_cs {
                        let v = c(ctx);
                        if let Some(slot) = out.iter_mut().find(|(ek, _)| ek == k) {
                            slot.1 = v;
                        } else {
                            out.push((k.clone(), v));
                        }
                    }
                    Value::Record(out)
                })
            }
            Expr::Call { callee, args, .. } => self.lower_call(callee, args, payload_name),
            // RFC-0028 §4: an async call with result arms. Only ever reached
            // through an action, `eval_pure` rejects it first, so by the time
            // it lowers here it is known to be in effect position.
            Expr::ControllerCall {
                call,
                ok,
                err,
                span,
            } => {
                let Expr::Call {
                    callee,
                    args,
                    span: call_span,
                } = call.as_ref()
                else {
                    return Box::new(|_| Value::Unit);
                };
                let _ = call_span;
                self.lower_controller_call(callee, args, ok.as_ref(), err.as_ref(), *span)
                    .unwrap_or_else(|| {
                        // Arms were written on a receiver that resolves to
                        // something other than a controller. Reported rather
                        // than lowered as a plain call: the arms would never
                        // run, and a result handler that silently never fires
                        // is the worst shape this mistake could take.
                        //
                        // A receiver that resolves to *nothing* is already
                        // covered by the `inject` diagnostic at its
                        // declaration, and saying it twice would point the
                        // developer at the call instead of the cause.
                        if self.receiver_is_bound(callee) {
                            self.errors.push(CompileError::UnknownControllerMethod {
                                span: *span,
                                controller: "this receiver".to_string(),
                                name: "an `ok`/`err` arm needs an injected controller".to_string(),
                            });
                        }
                        Box::new(|_| Value::Unit)
                    })
            }
            Expr::ClassRef(class, _) => {
                let s = format!(".{class}");
                Box::new(move |_| Value::Str(s.clone()))
            }
            Expr::Postfix { target, op, span } => {
                if let Ok(sig) = self.resolve_var(target, *span) {
                    let op = *op;
                    Box::new(move |ctx| {
                        let cur = ctx.peek_signal(sig).as_int().unwrap_or(0);
                        let new = match op {
                            PostfixOp::Inc => cur + 1,
                            PostfixOp::Dec => cur - 1,
                        };
                        ctx.write_signal(sig, Value::Int(new));
                        Value::Unit
                    })
                } else {
                    Box::new(|_| Value::Unit)
                }
            }
            Expr::Assign {
                target,
                op,
                value,
                span,
            } => {
                if let Ok(sig) = self.resolve_var(target, *span) {
                    let op = *op;
                    let mut rhs = self.lower_expr(value, payload_name);
                    Box::new(move |ctx| {
                        let val = rhs(ctx);
                        let new = match op {
                            AssignOp::Assign => val,
                            AssignOp::Add => {
                                let cur = ctx.peek_signal(sig).as_int().unwrap_or(0);
                                Value::Int(cur + val.as_int().unwrap_or(0))
                            }
                            AssignOp::Sub => {
                                let cur = ctx.peek_signal(sig).as_int().unwrap_or(0);
                                Value::Int(cur - val.as_int().unwrap_or(0))
                            }
                        };
                        ctx.write_signal(sig, new);
                        Value::Unit
                    })
                } else {
                    Box::new(|_| Value::Unit)
                }
            }
            // A `style { … }` value (RFC-0016) is consumed structurally, bound
            // via `let` into the style table and spliced by `..` at lower time
            // (see `register_style`/`expand_style_spreads`), never projected as
            // a scalar. Reaching here means it was used where a value was
            // expected, which has no meaning; yield Unit.
            Expr::StyleValue { .. } | Expr::Merge { .. } => Box::new(|_| Value::Unit),
            // `value with anim.*(…)` (RFC-0010): lower to the *target* value.
            // The curve is validated by the checker; the `Motion` runtime that
            // actually drives the on-screen transition lands in the follow-up
            // slice, so for now the target resolves instantly (as it did before
            // any `with` was written), which is a safe, correct fallback.
            Expr::Animated { value, .. } => self.lower_expr(value, payload_name),
            // A keyframe step (RFC-0025) only means anything inside
            // `anim.keyframes(…)`, which is driven at the `eval_pure`
            // chokepoint. Reaching the reactive lowering path means it was
            // written somewhere else; lower to its value, the same inert
            // fallback `Animated` takes.
            #[allow(clippy::match_same_arms)] // a different rule, same fallback
            Expr::KeyframeStep { value, .. } => self.lower_expr(value, payload_name),
            // A callback-prop action block (RFC-0019): lower each statement in
            // order and run them in sequence when the callback fires, returning
            // the last statement's value (`Unit` for the no-op default `{}`).
            // Mutations inside route through the reactive system exactly like any
            // event-handler action, because each `Assign`/`Postfix` statement
            // lowers to a signal write.
            Expr::Block(stmts, _) => {
                let mut cs: Vec<Lowered> = stmts
                    .iter()
                    .map(|s| self.lower_expr(s, payload_name))
                    .collect();
                Box::new(move |ctx| {
                    let mut last = Value::Unit;
                    for c in &mut cs {
                        last = c(ctx);
                    }
                    last
                })
            }
            // A `theme.<token>` access (RFC-0022): reads the reactive scheme
            // signal and projects the token's value for the active scheme. Any
            // other member access needs controller metadata (not modeled in
            // Phase 2); lambdas/assignments are actions, not projected values.
            Expr::Member { base, field, span } => {
                if let Some(lowered) = self.lower_theme_member(base, field, *span) {
                    return lowered;
                }
                // Data member access (RFC-0027 §4/§6): `xs.len` (list length) and
                // `r.field` (record field). Unknown members degrade to `Unit`;
                // the checker reports genuinely unknown ones (INV-4).
                let field = field.clone();
                let mut base_c = self.lower_expr(base, payload_name);
                Box::new(move |ctx| data_member(&base_c(ctx), &field))
            }
            Expr::Lambda { .. } | Expr::Error(_) => Box::new(|_| Value::Unit),
        }
    }

    /// Lowers a `theme.<field>` access to a reactive projection (RFC-0022 §1),
    /// or returns `None` when `base` does not resolve to an injected
    /// [`Value::Theme`] (leaving the caller to fall back to `Unit`).
    ///
    /// The returned closure reads the active-scheme signal *tracked*, so any
    /// binding that projects a token re-runs when the scheme flips. Token data
    /// is resolved once, here, and captured by value, the closure never borrows
    /// the interpreter.
    fn lower_theme_member(&mut self, base: &Expr, field: &Symbol, span: Span) -> Option<Lowered> {
        let Expr::Ident(base_name, _) = base else {
            return None;
        };
        let sig = match self.env.lookup(base_name) {
            Some(Value::Theme(sig)) => *sig,
            _ => return None,
        };
        let f = field.as_str();

        // Reserved reactive members: the scheme flag itself.
        if f == "dark" {
            return Some(Box::new(move |ctx| ctx.read_signal(sig)));
        }
        if f == "mode" {
            return Some(Box::new(move |ctx| {
                let dark = ctx.read_signal(sig).as_bool().unwrap_or(false);
                Value::Str(if dark { "dark" } else { "light" }.to_string())
            }));
        }

        // Color tokens differ per scheme → capture both resolved values.
        let light = self.theme.color(f, false);
        let dark = self.theme.color(f, true);
        if light.is_some() || dark.is_some() {
            return Some(Box::new(move |ctx| {
                let is_dark = ctx.read_signal(sig).as_bool().unwrap_or(false);
                let v = if is_dark {
                    dark.or(light)
                } else {
                    light.or(dark)
                };
                Value::Int(v.unwrap_or(0))
            }));
        }

        // Typography tokens: project the size (the current `typo:`/`size:`
        // pipeline is size-only; weight/family land with font byte-loading).
        if let Some(size) = self.theme.typo_size(f) {
            #[allow(clippy::cast_possible_truncation)]
            let size = size as i64;
            // Still read the signal so the binding is theme-scoped and re-runs on
            // a scheme flip (typography can differ per scheme in future themes).
            return Some(Box::new(move |ctx| {
                let _ = ctx.read_signal(sig);
                Value::Int(size)
            }));
        }

        // Shape (corner-radius) tokens.
        if let Some(radius) = self.theme.shape(f) {
            #[allow(clippy::cast_possible_truncation)]
            let radius = radius.round() as i64;
            return Some(Box::new(move |ctx| {
                let _ = ctx.read_signal(sig);
                Value::Int(radius)
            }));
        }

        // A member of a theme that names no known token is a hard error.
        self.errors.push(CompileError::UnknownThemeToken {
            span,
            field: f.to_string(),
            theme: self.theme.name.clone(),
        });
        Some(Box::new(|_| Value::Unit))
    }

    fn lower_ident(&self, name: &Symbol, payload_name: Option<&Symbol>) -> Lowered {
        if let Some(pname) = payload_name {
            if pname == name {
                return Box::new(move |_| {
                    CURRENT_PAYLOAD.with(|cell| cell.borrow().clone().unwrap_or(Value::Unit))
                });
            }
        }
        match name.as_str() {
            "true" => return Box::new(|_| Value::Bool(true)),
            "false" => return Box::new(|_| Value::Bool(false)),
            _ => {}
        }
        match self.env.lookup(name) {
            Some(Value::Signal(sig)) => {
                let sig = *sig;
                Box::new(move |ctx| ctx.read_signal(sig))
            }
            Some(Value::Memo(scope)) => {
                let m = *scope;
                Box::new(move |ctx| ctx.read_memo(m))
            }
            Some(v) => {
                let v = v.clone();
                Box::new(move |_| v.clone())
            }
            // An unresolved identifier is treated as an enum/style token
            // (e.g. `center`, `cover`); intrinsics validate it (M10).
            None => {
                let token = name.as_str().to_string();
                Box::new(move |_| Value::Str(token.clone()))
            }
        }
    }

    fn lower_strlit(&mut self, parts: &[StrPart], payload_name: Option<&Symbol>) -> Lowered {
        enum Part {
            Text(String),
            Interp(Lowered),
        }
        let mut lowered: Vec<Part> = parts
            .iter()
            .map(|p| match p {
                StrPart::Text(t) => Part::Text(t.clone()),
                StrPart::Interp(e) => Part::Interp(self.lower_expr(e, payload_name)),
            })
            .collect();
        Box::new(move |ctx| {
            let mut s = String::new();
            for part in &mut lowered {
                match part {
                    Part::Text(t) => s.push_str(t),
                    Part::Interp(c) => s.push_str(&format_scalar(&c(ctx))),
                }
            }
            Value::Str(s)
        })
    }

    fn lower_call(
        &mut self,
        callee: &Expr,
        args: &[crate::parser::ast::Arg],
        payload_name: Option<&Symbol>,
    ) -> Lowered {
        // RFC-0026 `navigate`/`back`/`replace`. Contextual: the names are only
        // special when their first argument really is a navigation container's
        // `var`, so nothing stops an app binding them for its own purposes.
        if let Some(lowered) = self.lower_nav_action(callee, args, payload_name) {
            return lowered;
        }
        // `untrack(expr)`, the reserved escape hatch (D2).
        if let Expr::Ident(name, _) = callee {
            if name.as_str() == "untrack" {
                if let Some(arg) = args.first() {
                    let mut inner = self.lower_expr(&arg.value, payload_name);
                    return Box::new(move |ctx| untrack(|| inner(ctx)));
                }
            }
            // A zero-arg call to a `fn`/`let` memo reads that memo.
            if let Some(Value::Memo(scope)) = self.env.lookup(name) {
                let m = *scope;
                return Box::new(move |ctx| ctx.read_memo(m));
            }
            // Parameterized fn call (M25) *or* callback-prop invocation
            // (RFC-0019 §3): inline the body with args bound as memos. For a
            // callback, the body is the *caller's* action block, still resolved
            // here, where the caller's `var`s remain live below the callee frame
            // in the shared flat env, so `count++` routes to the caller's signal.
            if let Some(Value::Fn(id)) = self.env.lookup(name).cloned() {
                if (id.0 as usize) < self.fn_table.len() {
                    let (params, body, is_callback) = self.fn_table[id.0 as usize].clone();
                    // A callback invoked with the wrong arity is a hard error
                    // (RFC-0019 §4); a plain `fn` keeps the historical lenient
                    // zip (extra args ignored, missing bound to nothing).
                    if is_callback && params.len() != args.len() {
                        self.errors.push(CompileError::CallbackArityMismatch {
                            span: callee.span(),
                            name: name.as_str().to_string(),
                            expected: params.len(),
                            found: args.len(),
                        });
                    }
                    // Bind each arg as a reactive memo so signal reads inside the
                    // body are tracked by the enclosing scope.
                    let snapshot = self.env.len();
                    for (param, arg) in params.iter().zip(args.iter()) {
                        let arg_lowered = self.lower_expr(&arg.value, payload_name);
                        let scope = self.ctx.open_memo(arg_lowered);
                        self.env.push(param.clone(), Value::Memo(scope));
                    }
                    // Lower the body with arg bindings in scope.
                    let body_lowered = self.lower_expr(&body, payload_name);
                    // Restore env.
                    self.env.truncate(snapshot);
                    return body_lowered;
                }
            }
        }
        // Collection method calls (RFC-0027 §4): `xs.push(v)`, `.removeAt(i)`,
        // `.contains(v)`, `.map(f)`, `.filter(f)`, each pure and value-returning.
        if let Expr::Member { base, field, .. } = callee {
            if let Some(lowered) = self.lower_collection_method(base, field, args, payload_name) {
                return lowered;
            }
        }
        // A call on an injected controller handle with no arms: fire-and-forget
        // (RFC-0028 §4). It takes the *same* lowering as the arm-bearing form,
        // so whether anyone reads the answer cannot change what the call does.
        if let Some(lowered) = self.lower_controller_call(callee, args, None, None, callee.span()) {
            return lowered;
        }
        Box::new(|_| Value::Unit)
    }

    /// Lowers a collection method call (RFC-0027 §4). Returns `None` for a
    /// non-collection method name so `anim.*` curve calls and other member calls
    /// keep their existing (non-data) handling.
    fn lower_collection_method(
        &mut self,
        base: &Expr,
        name: &Symbol,
        args: &[crate::parser::ast::Arg],
        payload_name: Option<&Symbol>,
    ) -> Option<Lowered> {
        let mut base_c = self.lower_expr(base, payload_name);
        match name.as_str() {
            "push" => {
                let mut arg = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let v = arg(ctx);
                    match base_c(ctx) {
                        Value::List(mut xs) => {
                            xs.push(v);
                            Value::List(xs)
                        }
                        other => other,
                    }
                }))
            }
            "removeAt" => {
                let mut arg = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let i = arg(ctx).as_int().and_then(|i| usize::try_from(i).ok());
                    match base_c(ctx) {
                        Value::List(mut xs) => {
                            // Out-of-range → unchanged list (INV-4, no panic).
                            if let Some(i) = i.filter(|i| *i < xs.len()) {
                                xs.remove(i);
                            }
                            Value::List(xs)
                        }
                        other => other,
                    }
                }))
            }
            "contains" => {
                let mut arg = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let needle = arg(ctx);
                    match base_c(ctx) {
                        Value::List(xs) => {
                            Value::Bool(xs.iter().any(|x| structural_eq(x, &needle)))
                        }
                        _ => Value::Bool(false),
                    }
                }))
            }
            "map" | "filter" => {
                // The predicate/transform must be a lambda (RFC-0027 §5). Lower
                // its body once with the parameter routed through the per-element
                // slot (like an event payload), evaluated per element below.
                let (param, body) = match &args.first()?.value {
                    Expr::Lambda { params, body, .. } => (params.first().cloned(), body.clone()),
                    _ => return None,
                };
                let mut body_c = self.lower_expr(&body, param.as_ref());
                let is_map = name.as_str() == "map";
                Some(Box::new(move |ctx| {
                    let Value::List(xs) = base_c(ctx) else {
                        return Value::List(Vec::new());
                    };
                    let mut out = Vec::with_capacity(xs.len());
                    for elem in xs {
                        let mapped = with_lambda_elem(elem.clone(), || body_c(ctx));
                        if is_map {
                            out.push(mapped);
                        } else if mapped.as_bool().unwrap_or(false) {
                            out.push(elem);
                        }
                    }
                    Value::List(out)
                }))
            }
            _ => None,
        }
    }

    // ── actions (mutations & bare expressions) ──────────────────────────

    /// Evaluates an expression with no reactive scope active (an *action*, not a
    /// projection). Mutations route through the mark cascade; a mutation on a
    /// non-`var` l-value is [`CompileError::NotAssignable`].
    ///
    /// # Errors
    ///
    /// Returns [`CompileError::NotAssignable`] if a mutation targets something
    /// other than a `var`.
    pub fn eval_action(&mut self, expr: &Expr) -> Result<Value, CompileError> {
        match expr {
            Expr::Postfix { target, op, span } => {
                let sig = self.resolve_var(target, *span)?;
                let cur = self.ctx.peek_signal(sig).as_int().unwrap_or(0);
                let new = match op {
                    PostfixOp::Inc => cur + 1,
                    PostfixOp::Dec => cur - 1,
                };
                self.ctx.write_signal(sig, Value::Int(new));
                Ok(Value::Unit)
            }
            Expr::Assign {
                target,
                op,
                value,
                span,
            } => {
                let sig = self.resolve_var(target, *span)?;
                let rhs = self.eval_pure(value);
                let new = match op {
                    AssignOp::Assign => rhs,
                    AssignOp::Add => {
                        let cur = self.ctx.peek_signal(sig).as_int().unwrap_or(0);
                        Value::Int(cur + rhs.as_int().unwrap_or(0))
                    }
                    AssignOp::Sub => {
                        let cur = self.ctx.peek_signal(sig).as_int().unwrap_or(0);
                        Value::Int(cur - rhs.as_int().unwrap_or(0))
                    }
                };
                self.ctx.write_signal(sig, new);
                Ok(Value::Unit)
            }
            // A brace action `=> { stmt* }` parses as a zero-parameter lambda
            // over a block (RFC-0019). Unwrap it and run each statement as an
            // action in order, so a multi-statement handler (e.g. the todo
            // "Add": `todos = todos.push(…)` then `draft = ""`) writes every
            // `var` it names, not just the last expression's value.
            Expr::Lambda { params, body, .. } if params.is_empty() => self.eval_action(body),
            Expr::Block(stmts, _) => {
                let mut last = Value::Unit;
                for stmt in stmts {
                    last = self.eval_action(stmt)?;
                }
                Ok(last)
            }
            other => Ok(self.eval_pure(other)),
        }
    }

    /// Evaluates `expr` once, immediately, with no scope active (so nothing
    /// subscribes). Used to seed `var`s and to evaluate action operands.
    fn eval_pure(&mut self, expr: &Expr) -> Value {
        // RFC-0028 §4: a call is an effect. `eval_pure` is every place a value
        // is *needed*, a `var` seed, an attribute, a memo body, so rejecting
        // it here rejects it in all of them at once, rather than in each of
        // them separately and eventually in all but one.
        if let Expr::ControllerCall { span, .. } = expr {
            self.errors.push(CompileError::EffectInPureContext {
                span: *span,
                context: "a value position".to_string(),
            });
            return Value::Unit;
        }
        // A `with` animation (RFC-0010) is driven here, at the single evaluation
        // chokepoint, so every animatable scalar prop (opacity/scale/translate/
        // rotate, all of which resolve through `eval_pure`) animates without
        // per-prop plumbing. A non-animated value takes the ordinary path.
        if let Expr::Animated { value, anim, span } = expr {
            return self.eval_animated(value, anim, self.anim_key(*span));
        }
        // An `anim.keyframes(…)` sequence (RFC-0025 §3) *is* the property value,
        // so it is driven at the same chokepoint: every animatable scalar (and
        // the coordinate pairs, which interpolate component-wise) gets keyframes
        // for free, with no per-prop plumbing.
        if crate::interp::anim::is_keyframes_call(expr) {
            return self.eval_keyframes(expr);
        }
        let mut compute = self.lower_expr(expr, None);
        compute(&mut self.ctx)
    }

    /// Drives one `with` animation (RFC-0010): resolves the target and curve,
    /// advances (or seeds) the persisted [`Motion`](byard_core::frame::Motion)
    /// keyed by `key`, and returns the value sampled at the current engine time.
    /// A target change reseeds `from` to the current on-screen value so a
    /// mid-flight reversal is continuous.
    fn eval_animated(&mut self, target: &Expr, anim: &Expr, key: AnimKey) -> Value {
        let target_value = self.eval_pure(target);
        // No advancing clock (a host that never calls `set_now_ms`, e.g. a
        // non-animating test path): resolve straight to the target so an
        // animation can never latch `has_active_animations` on `t = 0` forever.
        if !self.clock_set {
            return target_value;
        }
        let Ok(spec) = crate::interp::anim::resolve_motion(anim) else {
            // The checker already reported this; render the target inertly.
            return target_value;
        };
        let target_val = match &target_value {
            #[allow(clippy::cast_possible_truncation)]
            Value::Float(f) => *f as f32,
            #[allow(clippy::cast_precision_loss)]
            Value::Int(n) => *n as f32,
            // A coordinate pair animates component-wise off one shared clock, so
            // `translate: (0, 0) with anim.spring(delay: i * 50ms)` (RFC-0025's
            // stagger shape) moves as one. Only the RFC-0025 paths handle a pair;
            // a plain `with` keeps its historical pass-through.
            Value::Tuple(items) if !spec.is_plain() => {
                return self.eval_looped_pair(items, &spec, key);
            }
            // Anything else can't be interpolated, pass it through untouched
            // (the checker already restricts `with` to numeric props).
            _ => return target_value,
        };
        // RFC-0025: a repeating, delayed or explicitly-started animation runs on
        // its own timeline; everything else keeps the original single-shot path
        // below, byte for byte.
        if !spec.is_plain() {
            return Value::Float(f64::from(self.eval_looped(target_val, &spec, key)));
        }
        let packed = pack_curve(spec.curve);
        let now = self.now_ms;
        let motion = self
            .animations
            .entry(key)
            .or_insert_with(|| byard_core::frame::Motion {
                from: target_val,
                to: target_val,
                start_ms: now,
                curve: packed,
            });
        // Retarget on a goal change: reseed `from` to where the property
        // actually is right now (interruptible spring), restart the clock.
        if (motion.to - target_val).abs() > f32::EPSILON {
            let current = motion.sample(now);
            motion.from = current;
            motion.to = target_val;
            motion.start_ms = now;
        }
        // Keep the curve in sync (a hot-reload may have edited it).
        motion.curve = packed;
        let sampled = motion.sample(now);
        // `Motion::DEFAULT_EPS_*` are pixel-scaled (0.5), far too loose for the
        // ratio/opacity/radian props that also animate through this one generic
        // path, with them an ease-out could read "settled" while still visibly
        // short of the target. Use tight, unit-agnostic epsilons: position is
        // the final-value accuracy gate; the velocity gate keeps a spring's
        // overshoot alive instead of freezing it at the first target crossing.
        let settled = motion.is_settled_with_eps(now, ANIM_SETTLE_EPS_POS, ANIM_SETTLE_EPS_VEL);
        if !settled {
            self.any_active = true;
        }
        Value::Float(f64::from(sampled))
    }

    /// Drives one repeating / delayed / explicitly-started `with` animation
    /// (RFC-0025 §1, §5) and returns the value sampled at the current engine
    /// time.
    ///
    /// A repeating animation needs no persisted endpoints, it is fully
    /// determined by `from`, `to`, the curve and its own timeline, all of which
    /// are recomputed each frame. What *is* persisted is the timeline
    /// ([`LoopClock`]), which the RFC-0025 clock reduces to an offset inside one
    /// iteration ([`loop_phase`](byard_core::frame::loop_phase)) for the curve to
    /// be sampled at. A finite repeat that has played out holds its final value
    /// and leaves the active set; an infinite one never does, which is what keeps
    /// frames flowing for a spinner.
    fn eval_looped(
        &mut self,
        target_val: f32,
        spec: &crate::interp::anim::MotionSpec<'_>,
        key: AnimKey,
    ) -> f32 {
        let from_val = spec
            .from
            .map_or(target_val, |expr| self.eval_number(expr, target_val));
        let motion = byard_core::frame::Motion {
            from: from_val,
            to: target_val,
            start_ms: self.now_ms,
            curve: pack_curve(spec.curve),
        };
        match self.loop_at(&[motion], spec, key) {
            Some(phase) => motion.sample_secs(phase),
            None => from_val,
        }
    }

    /// The pair form of [`Self::eval_looped`]: each component gets its own
    /// endpoints, all sampled off one shared phase so the axes stay in lockstep
    /// (a diagonal entrance must not arrive one axis at a time). A `from:` may be
    /// a pair or a scalar broadcast to both axes.
    fn eval_looped_pair(
        &mut self,
        items: &[(Option<Symbol>, Value)],
        spec: &crate::interp::anim::MotionSpec<'_>,
        key: AnimKey,
    ) -> Value {
        let Some(targets) = items
            .iter()
            .map(|(_, v)| spacing_value(v))
            .collect::<Option<Vec<f32>>>()
        else {
            // A non-numeric component can't be interpolated; pass the pair
            // through as written.
            return Value::Tuple(items.to_vec());
        };
        let from_value = spec.from.map(|expr| self.eval_pure(expr));
        let froms: Vec<f32> = targets
            .iter()
            .enumerate()
            .map(|(axis, target)| match &from_value {
                Some(Value::Tuple(from_items)) => from_items
                    .get(axis)
                    .and_then(|(_, v)| spacing_value(v))
                    .unwrap_or(*target),
                Some(scalar) => spacing_value(scalar).unwrap_or(*target),
                None => *target,
            })
            .collect();
        let curve = pack_curve(spec.curve);
        let now = self.now_ms;
        let motions: Vec<byard_core::frame::Motion> = froms
            .iter()
            .zip(&targets)
            .map(|(from, to)| byard_core::frame::Motion {
                from: *from,
                to: *to,
                start_ms: now,
                curve,
            })
            .collect();
        let phase = self.loop_at(&motions, spec, key);
        Value::Tuple(
            items
                .iter()
                .zip(&motions)
                .map(|((name, _), motion)| {
                    let sampled = match phase {
                        Some(t_secs) => motion.sample_secs(t_secs),
                        None => motion.from,
                    };
                    (name.clone(), Value::Float(f64::from(sampled)))
                })
                .collect(),
        )
    }

    /// The shared body of every repeating animation (RFC-0025 §1, §5): advances
    /// the timeline for `key`, applies the delay, and returns the offset *inside
    /// one iteration* at which `motions` should be sampled, or `None` while the
    /// delay is still holding the start value.
    ///
    /// All components share one period (the longest), so a multi-channel
    /// animation, a pair of axes, a colour's four channels, completes each play
    /// as a unit and alternates as a unit.
    fn loop_at(
        &mut self,
        motions: &[byard_core::frame::Motion],
        spec: &crate::interp::anim::MotionSpec<'_>,
        key: AnimKey,
    ) -> Option<f32> {
        use byard_core::frame::{Motion, loop_phase};

        // A goal change restarts the sequence from its own start value, an
        // oscillation has two fixed endpoints, so there is nothing to reseed
        // `from` from (unlike the interruptible one-shot spring).
        // A `restart:` witness joins the endpoints in the fingerprint, so a
        // change to it restarts the timeline exactly as a retarget would, the
        // reference-free "play that again" (RFC-0025 §5's replay case). It is
        // never *cancellable*: a replay is meant to run its delays again.
        let restart = spec.restart.map(|expr| self.restart_key(expr));
        let fingerprint = endpoint_key(motions) ^ restart.unwrap_or(0);
        let cancellable = spec.delay.is_cancellable() && restart.is_none();
        let (elapsed, honor_delay) = self.loop_elapsed(key, fingerprint, cancellable);
        // §5: on a *retarget* a `delay:` is cancelled, the animation heads for
        // the new target at once, so a delayed transition can never overwrite a
        // more recent interaction, while a stagger's offset is honoured again
        // and the cascade replays in order. On the first mount both wait.
        let delay_ms = if honor_delay {
            self.eval_delay(&spec.delay)
        } else {
            0
        };
        // Still inside the delay: hold the start value, and stay in the active
        // set so the frames that will start the motion keep coming.
        if elapsed < delay_ms {
            self.any_active = true;
            return None;
        }
        let period = motions
            .iter()
            .map(|m| m.natural_duration_ms(ANIM_SETTLE_EPS_POS))
            .max()
            .unwrap_or(Motion::MIN_PERIOD_MS);
        let phase = loop_phase(period, elapsed - delay_ms, spec.repeat, spec.reverse);
        if !phase.finished {
            self.any_active = true;
        }
        Some(phase.t_secs)
    }

    /// Samples an `anim.keyframes(…)` value at the current engine time
    /// (RFC-0025 §3). Scalars interpolate numerically; a coordinate pair
    /// interpolates component-wise, so `translate` keyframes work.
    fn eval_keyframes(&mut self, expr: &Expr) -> Value {
        let Some(blend) = self.keyframe_blend(expr) else {
            return Value::Unit;
        };
        let lo = self.eval_pure(blend.lo);
        if blend.t <= 0.0 {
            return lo;
        }
        let hi = self.eval_pure(blend.hi);
        lerp_value(&lo, &hi, blend.t)
    }

    /// Positions a keyframe sequence on the engine clock: which two steps
    /// surround "now" and how far between them the value sits (RFC-0025 §3).
    ///
    /// Returns `None` when the sequence is malformed (the checker has already
    /// reported it) or the host never advanced the clock. Marks the animation
    /// active unless a finite sequence has played out, the settling contract
    /// the whole animation system shares.
    fn keyframe_blend<'a>(&mut self, expr: &'a Expr) -> Option<KeyframeBlend<'a>> {
        use byard_core::frame::{MAX_KEYFRAME_STEPS, MotionCurve, keyframe_cursor, loop_phase};

        let track = crate::interp::anim::resolve_keyframes(expr)?.ok()?;
        let key = self.anim_key(expr.span());
        // Without an advancing clock, resolve to the sequence's first value,
        // mirrors the `with` path, and never latches the active set at `t = 0`.
        if !self.clock_set {
            let first = track.steps.first()?.value;
            return Some(KeyframeBlend {
                lo: first,
                hi: first,
                t: 0.0,
            });
        }
        let delay_ms = self.eval_delay(&track.delay);
        // A keyframe track has no endpoints to retarget: its steps *are* the
        // values, so a reactive step just re-blends where the sequence already
        // is rather than restarting it. An explicit `restart:` witness is the one
        // thing that does start it over.
        let fingerprint = track.restart.map_or(0, |expr| self.restart_key(expr));
        let (elapsed, _) = self.loop_elapsed(key, fingerprint, false);
        if elapsed < delay_ms {
            self.any_active = true;
            let first = track.steps.first()?.value;
            return Some(KeyframeBlend {
                lo: first,
                hi: first,
                t: 0.0,
            });
        }
        let phase = loop_phase(
            track.duration_ms,
            elapsed - delay_ms,
            track.repeat,
            track.reverse,
        );
        if !phase.finished {
            self.any_active = true;
        }
        // The step table is capped by the RFC (and validated on resolution), so
        // the timing lookup runs on the stack with no allocation.
        let mut percents = [0.0_f32; MAX_KEYFRAME_STEPS];
        let mut easings = [MotionCurve::LINEAR; MAX_KEYFRAME_STEPS];
        let len = track.steps.len().min(MAX_KEYFRAME_STEPS);
        for (slot, step) in track.steps.iter().enumerate().take(len) {
            percents[slot] = step.percent;
            easings[slot] = step.easing;
        }
        #[allow(clippy::cast_precision_loss)]
        let progress = phase.t_secs / (track.duration_ms as f32 / 1000.0);
        let cursor = keyframe_cursor(&percents[..len], &easings[..len], progress);
        Some(KeyframeBlend {
            lo: track.steps[cursor.lo].value,
            hi: track.steps[cursor.hi].value,
            t: cursor.t,
        })
    }

    /// Evaluates an animation's start offset in milliseconds (RFC-0025 §5).
    /// `anim.stagger(…)` multiplies its per-item step by the loop index, which
    /// is why the offset stays an expression until here.
    fn eval_delay(&mut self, delay: &crate::interp::anim::Delay<'_>) -> u32 {
        use crate::interp::anim::Delay;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = |v: f32| v.max(0.0).min(f32::from(u16::MAX)) as u32;
        match delay {
            Delay::None => 0,
            Delay::Offset(expr) => ms(self.eval_number(expr, 0.0)),
            #[allow(clippy::cast_precision_loss)]
            Delay::Stagger { step_ms, index } => ms(self.eval_number(index, 0.0) * *step_ms as f32),
        }
    }

    /// Hashes a `restart:` witness value (RFC-0025 §5's replay case).
    ///
    /// Only *change* matters, never order or magnitude, so any value the language
    /// can produce is usable as a replay trigger, a counter, a bool, a selected
    /// id, a route name.
    fn restart_key(&mut self, expr: &Expr) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match self.eval_pure(expr) {
            Value::Int(n) => n.hash(&mut hasher),
            Value::Float(f) => f.to_bits().hash(&mut hasher),
            Value::Bool(b) => b.hash(&mut hasher),
            Value::Str(s) => s.hash(&mut hasher),
            // A structural value hashes through its rendering, coarse, but a
            // replay trigger only needs "is this the same as last frame?".
            other => format!("{other:?}").hash(&mut hasher),
        }
        // Never 0: that is the "no witness" sentinel the keyframe path uses.
        hasher.finish() | 1
    }

    /// Evaluates `expr` as an `f32`, falling back to `default` for a
    /// non-numeric result (the checker restricts these positions to numbers).
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn eval_number(&mut self, expr: &Expr, default: f32) -> f32 {
        match self.eval_pure(expr) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => default,
        }
    }

    /// Forgets the animation state of the rows `from..to` of `pool`, and of
    /// everything nested inside them (RFC-0025).
    ///
    /// A nested `for` is lowered once per *outer* row, so the pools inside a
    /// vanished row belong to that row alone: walking its lowered nodes for
    /// them, and dropping their slots too, is exact rather than approximate.
    /// Doing it any other way would either leave a nested row's timeline behind
    /// (it resumes mid-flight under whatever takes its place) or drop a live
    /// sibling's (it restarts for no reason the author can see).
    fn drop_slot_state(&mut self, pool: usize, from: usize, to: usize) {
        let mut dead: Vec<u32> = Vec::new();
        let mut queue: Vec<(usize, usize, usize)> = vec![(pool, from, to)];
        while let Some((p, lo, hi)) = queue.pop() {
            let Some(entry) = self.for_pools.get(p) else {
                continue;
            };
            let hi = hi.min(entry.bodies.len()).min(entry.item_slots.len());
            for i in lo..hi {
                dead.push(entry.item_slots[i].0);
                // Descend into the row's own nested pools, whole: every one of
                // their rows is going away with it.
                let mut nested = Vec::new();
                collect_pools(&entry.bodies[i], &mut nested);
                for q in nested {
                    let len = self.for_pools.get(q).map_or(0, |n| n.bodies.len());
                    queue.push((q, 0, len));
                }
            }
        }
        if dead.is_empty() {
            return;
        }
        let dead: std::collections::HashSet<u32> = dead.into_iter().collect();
        self.animations.retain(|k, _| !dead.contains(&k.slot));
        self.color_animations.retain(|k, _| !dead.contains(&k.slot));
        self.anim_clocks.retain(|k, _| !dead.contains(&k.slot));
    }

    /// The animation key for `span` on the instance currently being rendered.
    const fn anim_key(&self, span: Span) -> AnimKey {
        AnimKey::new(span, self.anim_slot)
    }

    /// Forgets every animation whose source node lies inside `range`, what an
    /// unmounted `when` branch takes with it (RFC-0025: "no separate stop
    /// animation API, the animation lives and dies with its element").
    ///
    /// Animation state is keyed by the source span of the `with`/keyframes node,
    /// so "inside this branch" is exactly "inside this source range". Rare
    /// (branch flips only), so a linear sweep of the three maps is the right
    /// trade against paying for a reverse index every frame.
    fn drop_animation_state(&mut self, range: Span) {
        if range.end <= range.start {
            return;
        }
        let inside = |key: &AnimKey| key.span.start >= range.start && key.span.end <= range.end;
        self.animations.retain(|key, _| !inside(key));
        self.color_animations.retain(|key, _| !inside(key));
        self.anim_clocks.retain(|key, _| !inside(key));
    }

    /// Advances the RFC-0025 timeline keyed by `key`, returning the milliseconds
    /// elapsed on it and whether its `delay` still applies.
    ///
    /// Three jobs, all of them about *when* rather than *what*:
    /// - seeds the timeline the first time the animation is seen (delay honoured, an entrance is meant to wait);
    /// - implements §2's offscreen pause, missing a whole render means the
    ///   element was not drawn (it left the viewport, or its `when` branch
    ///   collapsed), so the timeline is shifted forward by the time it was away
    ///   instead of counting it as motion: the animation resumes where it
    ///   stopped and costs nothing while it is gone;
    /// - restarts the timeline when the endpoints change (§5). A restart drops a
    ///   `cancellable` delay so the property heads for its new target at once,
    ///   and keeps a stagger's, so the cascade replays in order.
    fn loop_elapsed(&mut self, key: AnimKey, endpoints: u64, cancellable: bool) -> (u32, bool) {
        let (now, seq) = (self.now_ms, self.frame_seq);
        let clock = self.anim_clocks.entry(key).or_insert(LoopClock {
            start_ms: now,
            last_seen_ms: now,
            last_seen_seq: seq,
            endpoints,
            honor_delay: true,
        });
        if seq.saturating_sub(clock.last_seen_seq) > 1 {
            clock.start_ms = clock
                .start_ms
                .saturating_add(now.saturating_sub(clock.last_seen_ms));
        }
        clock.last_seen_seq = seq;
        if clock.endpoints != endpoints {
            clock.endpoints = endpoints;
            clock.start_ms = now;
            clock.honor_delay = !cancellable;
        }
        clock.last_seen_ms = now;
        (now.saturating_sub(clock.start_ms), clock.honor_delay)
    }

    fn resolve_var(&self, target: &Expr, span: Span) -> Result<SignalId, CompileError> {
        if let Expr::Ident(name, _) = target {
            if let Some(Value::Signal(sig)) = self.env.lookup(name) {
                return Ok(*sig);
            }
        }
        // `theme.dark = …` writes the reactive scheme signal (RFC-0022 §1), so a
        // scheme flip drives Mark-and-Pull across every token reference.
        if let Some(sig) = self.resolve_theme_scheme_target(target) {
            return Ok(sig);
        }
        Err(CompileError::NotAssignable { span })
    }

    /// Resolves `theme.dark` (the assignable/bindable scheme flag) to its backing
    /// scheme signal, or `None` if `target` is not that member (RFC-0022 §1).
    fn resolve_theme_scheme_target(&self, target: &Expr) -> Option<SignalId> {
        let Expr::Member { base, field, .. } = target else {
            return None;
        };
        if field.as_str() != "dark" {
            return None;
        }
        let Expr::Ident(base_name, _) = base.as_ref() else {
            return None;
        };
        match self.env.lookup(base_name) {
            Some(Value::Theme(sig)) => Some(*sig),
            _ => None,
        }
    }

    /// Lowers an action expression to an event handler closure, capturing any optional payload bindings.
    ///
    /// # Errors
    ///
    /// Returns a [`CompileError`] if variable resolution or assignment validation fails.
    #[allow(clippy::needless_pass_by_value)]
    pub fn lower_action(
        &mut self,
        expr: &Expr,
        payload_name: Option<Symbol>,
    ) -> Result<Action, CompileError> {
        // A brace action `=> { stmt* }` parses as a zero-parameter lambda over a
        // block (RFC-0019). Lower the block body directly, its statements each
        // lower to a signal write, so a multi-statement handler runs every
        // statement, not the inert lambda value (which lowers to `Unit`).
        let expr = match expr {
            Expr::Lambda { params, body, .. } if params.is_empty() => body.as_ref(),
            other => other,
        };
        let mut compute = self.in_action_position(|s| s.lower_expr(expr, payload_name.as_ref()));
        Ok(Box::new(move |ctx, payload| {
            CURRENT_PAYLOAD.with(|cell| {
                *cell.borrow_mut() = payload.cloned();
            });
            let _ = compute(ctx);
            CURRENT_PAYLOAD.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }))
    }

    /// Reads a scroll offset signal as an `f32` (an `Int` or `Float` `var`);
    /// anything else reads as the origin.
    fn peek_scroll(&self, sig: SignalId) -> f32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        match self.peek(sig) {
            Value::Int(n) => n as f32,
            Value::Float(f) => f as f32,
            _ => 0.0,
        }
    }

    /// Writes `value` back to a scroll offset signal, preserving its `Int`/`Float`
    /// kind so a whole-pixel `var off: Int` never becomes a `Float` mid-scroll.
    fn write_scroll(&mut self, sig: SignalId, value: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let v = match self.peek(sig) {
            Value::Int(_) => Value::Int(value.round() as i64),
            _ => Value::Float(f64::from(value)),
        };
        self.write_var(sig, v);
    }

    /// Nudges one scroll axis by `delta` logical px (wheel/trackpad), clamped to
    /// `[0, max]`. A forward delta reveals earlier content, so the offset shrinks.
    fn nudge_scroll(&mut self, axis: ScrollAxis, delta: f32) {
        let next = (self.peek_scroll(axis.sig) - delta).clamp(0.0, axis.max);
        self.write_scroll(axis.sig, next);
    }

    /// Estimates and records the signed scroll velocity for `elem` (RFC-0021 fling
    /// projection): the offset change since the last input over its `time_ms`
    /// (px/s, positive = offset increasing). Skipped when the clock does not
    /// advance (`dt == 0`, e.g. unit tests) so velocity stays 0 and a settle snaps
    /// to the nearest boundary.
    fn record_scroll_velocity(&mut self, elem: u32, offset: f32, time_ms: u64) {
        if let Some((prev_off, prev_t)) = self.scroll_vel_last.get(&elem).copied() {
            let dt = time_ms.saturating_sub(prev_t);
            if dt > 0 {
                self.scroll_vel
                    .insert(elem, (offset - prev_off) / (dt as f32 / 1000.0));
            }
        }
        self.scroll_vel_last.insert(elem, (offset, time_ms));
    }

    /// The scrollable axis of a target and its viewport extent (the one with
    /// travel, `max > 0`), vertical preferred. RFC-0021 snap/pagination helper.
    fn scrollable_axis(t: &ScrollTarget) -> Option<(ScrollAxis, f32)> {
        t.y.filter(|a| a.max > 0.0)
            .map(|a| (a, t.rect.h))
            .or_else(|| t.x.filter(|a| a.max > 0.0).map(|a| (a, t.rect.w)))
    }

    /// Reflects the current page for `t` from its offset (`round(offset /
    /// viewport)`), writing the `page:` var and firing `page_change` on a change.
    /// Runs continuously (every scroll) so pagination tracks wheel/trackpad
    /// scrolling, not just snap settles (RFC-0021).
    fn reflect_page(&mut self, t: &ScrollTarget) {
        let (Some(psig), Some(elem)) = (t.page_sig, t.elem) else {
            return;
        };
        let Some((axis, vp)) = Self::scrollable_axis(t) else {
            return;
        };
        if vp <= 0.0 {
            return;
        }
        #[allow(clippy::cast_possible_truncation)]
        let page = (self.peek_scroll(axis.sig) / vp).round() as i64;
        if self.ctx.peek_signal(psig).as_int() != Some(page) {
            self.ctx.write_signal(psig, Value::Int(page));
            self.router.fire_event(
                &mut self.ctx,
                elem,
                super::events::EventKind::PageChange,
                Some(&Value::Int(page)),
            );
        }
        // Treat our reflection as the synced value so `page`→offset never fights.
        self.scroll_page_last.insert(elem, page);
    }

    /// The offset `t`'s scrollable `axis` should rest at, snapping the current
    /// offset to the mode's nearest boundary (RFC-0021): a viewport multiple for
    /// [`SnapMode::Page`], or the nearest precomputed direct-child boundary for
    /// [`SnapMode::Item`] (already `snap_align`-adjusted in
    /// [`scroll_item_bounds`](Self::scroll_item_bounds)). `None` for `snap: none`
    /// or when an item view has no measured children yet.
    fn snap_target_offset(&self, t: &ScrollTarget, axis: ScrollAxis, vp: f32) -> Option<f32> {
        let cur = self.peek_scroll(axis.sig);
        match t.snap {
            SnapMode::None => None,
            SnapMode::Page => Some(((cur / vp).round() * vp).clamp(0.0, axis.max)),
            SnapMode::Item => {
                let bounds = self.scroll_item_bounds.get(&t.elem?)?;
                bounds
                    .iter()
                    .copied()
                    .min_by(|a, b| (a - cur).abs().total_cmp(&(b - cur).abs()))
                    .map(|v| v.clamp(0.0, axis.max))
            }
        }
    }

    /// RFC-0021 `snap: item`: the rest offset for each direct child of the
    /// `ScrollView` atlas node, on the scrolling axis, aligned per `snap_align`
    /// and clamped to `[0, axis_max]`. Read once per render from the laid-out
    /// child rects (offset is a paint-time translate, so layout positions are the
    /// natural, offset-free content coordinates). A child whose start is `s` and
    /// extent `w` rests at `s` (start), `s − (vp − w)/2` (centre), or `s − (vp − w)`
    /// (end).
    fn item_snap_offsets(
        &self,
        sv_node: byard_core::atlas::layout::AtlasNodeId,
        horizontal: bool,
        vp: f32,
        axis_max: f32,
        align: SnapAlign,
    ) -> Vec<f32> {
        let Ok(Some(sv)) = self.atlas.resolved_rect(sv_node) else {
            return Vec::new();
        };
        // The items are the flex children that lay out along the scroll axis. A
        // scrolling `ScrollView` almost always wraps them in one `Row`/`Column`
        // (needed to flow on the scroll axis), so when there is a single content
        // child, descend into it and snap to *its* children; otherwise the direct
        // children are the items.
        let direct = self.atlas.children(sv_node);
        let items = match direct.as_slice() {
            [only] => {
                let inner = self.atlas.children(*only);
                if inner.is_empty() { direct } else { inner }
            }
            _ => direct,
        };
        let mut offsets: Vec<f32> = items
            .into_iter()
            .filter_map(|child| self.atlas.resolved_rect(child).ok().flatten())
            .map(|r| {
                let (start, extent) = if horizontal {
                    (r.x - sv.x, r.width)
                } else {
                    (r.y - sv.y, r.height)
                };
                let aligned = match align {
                    SnapAlign::Start => start,
                    SnapAlign::Center => start - (vp - extent) / 2.0,
                    SnapAlign::End => start - (vp - extent),
                };
                aligned.clamp(0.0, axis_max)
            })
            .collect();
        // Ascending, so the fling-projection ±1 clamp indexes adjacent items.
        offsets.sort_by(f32::total_cmp);
        offsets
    }

    /// The offset a settle should target, applying RFC-0021 fling projection: above
    /// the fling velocity threshold it advances one boundary in the fling direction
    /// (clamped ±1 of the nearest, no multi-item skip); otherwise the nearest.
    /// Shares boundary geometry with [`snap_target_offset`](Self::snap_target_offset).
    fn snap_settle_target(&self, t: &ScrollTarget, axis: ScrollAxis, vp: f32) -> Option<f32> {
        /// Fling velocity (px/s) above which the settle projects rather than snaps
        /// to nearest (RFC-0021 resolved question: 150 dp/s).
        const FLING: f32 = 150.0;
        let cur = self.peek_scroll(axis.sig);
        let vel = t
            .elem
            .and_then(|e| self.scroll_vel.get(&e).copied())
            .unwrap_or(0.0);
        // The ordered boundary offsets on this axis.
        let bounds: Vec<f32> = match t.snap {
            SnapMode::None => return None,
            SnapMode::Page => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let last = (axis.max / vp).ceil().max(0.0) as usize;
                #[allow(clippy::cast_precision_loss)]
                (0..=last)
                    .map(|i| ((i as f32) * vp).min(axis.max))
                    .collect()
            }
            SnapMode::Item => self.scroll_item_bounds.get(&t.elem?)?.clone(),
        };
        if bounds.is_empty() {
            return None;
        }
        let nearest = bounds
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (**a - cur).abs().total_cmp(&(**b - cur).abs()))
            .map(|(i, _)| i)?;
        let target = if vel > FLING {
            // first boundary strictly ahead of the current offset
            bounds
                .iter()
                .position(|&b| b > cur + 0.5)
                .unwrap_or(nearest)
        } else if vel < -FLING {
            // last boundary strictly behind
            bounds
                .iter()
                .rposition(|&b| b < cur - 0.5)
                .unwrap_or(nearest)
        } else {
            nearest
        };
        // Clamp to ±1 of the nearest so a fling never skips more than one boundary.
        let target = target.clamp(nearest.saturating_sub(1), nearest + 1);
        Some(bounds[target].clamp(0.0, axis.max))
    }

    /// Begins a smooth snap of `t`'s scrollable axis to its nearest boundary: reflects
    /// `page` right away (the indicator jumps to the destination as the glide
    /// starts) and seeds a spring from the current offset to the page boundary
    /// (RFC-0021 §2, RFC-0010 spring). [`advance_snap_anims`](Self::advance_snap_anims)
    /// drives it to rest and fires `scroll_end`. When no clock is advancing (a
    /// non-animating host or a test), it resolves to the boundary instantly so the
    /// behaviour is identical minus the animation. Shared by drag-release and the
    /// scroll-quiet settle; a no-op if already resting on a page.
    fn begin_snap(&mut self, t: &ScrollTarget) {
        /// Sub-pixel tolerance for "already on a boundary".
        const EPS: f32 = 0.5;
        if t.snap == SnapMode::None {
            return;
        }
        let (Some((axis, vp)), Some(elem)) = (Self::scrollable_axis(t), t.elem) else {
            return;
        };
        if vp <= 0.0 {
            return;
        }
        let cur = self.peek_scroll(axis.sig);
        // A fling above the velocity threshold projects one boundary ahead; a slow
        // settle snaps to the nearest. Consume the velocity so the next gesture
        // starts fresh.
        let Some(target) = self.snap_settle_target(t, axis, vp) else {
            return;
        };
        self.scroll_vel.remove(&elem);
        self.scroll_vel_last.remove(&elem);
        if (cur - target).abs() < EPS {
            self.snap_anims.remove(&elem);
            return; // already on a boundary, nothing to glide
        }
        // Destination page is known now, reflect it immediately (fires
        // `page_change`) so pagination leads the glide, and settle it exactly once
        // the spring (or the instant fallback) arrives.
        self.reflect_page(t);
        if self.clock_set {
            let curve = t
                .snap_spring
                .unwrap_or_else(|| pack_curve(crate::interp::anim::Curve::DEFAULT_SPRING));
            self.snap_anims.insert(
                elem,
                SnapAnim {
                    sig: axis.sig,
                    motion: byard_core::frame::Motion {
                        from: cur,
                        to: target,
                        start_ms: self.now_ms,
                        curve,
                    },
                    target,
                },
            );
            self.any_active = true;
        } else {
            self.finish_snap(elem, axis.sig, target);
        }
    }

    /// Pins `sig` to the exact page `target`, clears any pending glide, and fires
    /// `scroll_end` for `elem` (RFC-0021 snap completion).
    fn finish_snap(&mut self, elem: u32, sig: SignalId, target: f32) {
        self.snap_anims.remove(&elem);
        self.write_scroll(sig, target);
        self.router.fire_event(
            &mut self.ctx,
            elem,
            super::events::EventKind::ScrollEnd,
            None,
        );
    }

    /// Advances every in-flight snap spring one `render` (RFC-0021 smooth snap):
    /// samples each [`SnapAnim`](SnapAnim) at the engine clock, writes the offset,
    /// and, once the spring settles, pins the offset exactly on the page and
    /// fires `scroll_end`. A live drag on an elem cancels its glide (the finger
    /// takes over). Keeps `any_active` set while any spring is still moving so the
    /// host keeps presenting frames until it rests.
    fn advance_snap_anims(&mut self) {
        /// Pixel/velocity settle gates for a scroll offset (sub-pixel is
        /// imperceptible; the exact target is pinned on settle regardless).
        const EPS_POS: f32 = 0.5;
        const EPS_VEL: f32 = 2.0;
        if self.snap_anims.is_empty() {
            return;
        }
        let drag_elem = self.scroll_drag.and_then(|d| d.elem);
        let now = self.now_ms;
        for (elem, anim) in self
            .snap_anims
            .iter()
            .map(|(e, a)| (*e, *a))
            .collect::<Vec<_>>()
        {
            if drag_elem == Some(elem) {
                self.snap_anims.remove(&elem); // the finger reclaimed this view
                continue;
            }
            if anim.motion.is_settled_with_eps(now, EPS_POS, EPS_VEL) {
                self.finish_snap(elem, anim.sig, anim.target);
            } else {
                self.write_scroll(anim.sig, anim.motion.sample(now));
                self.any_active = true;
            }
        }
    }

    /// On drag release, snap the `ScrollView` under the press point (RFC-0021).
    fn snap_scroll_on_release(&mut self, start_pos: (f32, f32)) {
        let (px, py) = start_pos;
        let Some(t) = self
            .scroll_targets
            .iter()
            .rev()
            .find(|t| {
                px >= t.rect.x
                    && px < t.rect.x + t.rect.w
                    && py >= t.rect.y
                    && py < t.rect.y + t.rect.h
            })
            .copied()
        else {
            return;
        };
        self.begin_snap(&t);
    }

    /// Reflects `page` for every scroll target after this tick's scroll writes
    /// (RFC-0021 continuous pagination).
    fn reflect_pages(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            self.reflect_page(&t);
        }
    }

    /// RFC-0021 snap settle: once a `snap`-enabled `ScrollView` has gone quiet,
    /// no wheel/trackpad scroll input for [`SETTLE_FRAMES`] renders (so trackpad
    /// momentum, a stream of shrinking deltas, cannot trigger a snap mid-fling
    /// that fights the next event), glide its offset to the nearest page via
    /// [`begin_snap`](Self::begin_snap). Frame-counted, not clock-based, so it
    /// settles identically whether or not the host advances `now_ms`. A live drag
    /// never settles on stillness (it snaps on release). Runs each `render`, over
    /// the previous frame's targets.
    fn settle_snaps(&mut self) {
        /// Renders of quiet (no scroll input) before a `snap: page` view settles.
        const SETTLE_FRAMES: u64 = 4;
        /// Sub-pixel tolerance for "already on a page".
        const EPS: f32 = 0.5;
        let drag_elem = self.scroll_drag.and_then(|d| d.elem);
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let Some(elem) = t.elem else { continue };
            // A drag in progress, or a glide already running, owns the offset.
            if t.snap == SnapMode::None
                || drag_elem == Some(elem)
                || self.snap_anims.contains_key(&elem)
            {
                continue;
            }
            let Some((axis, vp)) = Self::scrollable_axis(&t) else {
                continue;
            };
            if vp <= 0.0 {
                continue;
            }
            let cur = self.peek_scroll(axis.sig);
            let Some(boundary) = self.snap_target_offset(&t, axis, vp) else {
                continue;
            };
            if (cur - boundary).abs() < EPS {
                continue; // already resting on a boundary
            }
            // Off a boundary: wait for the scroll to go quiet, then glide. `quiet`
            // is how many renders since the last scroll input touched this elem;
            // momentum keeps resetting it, so we only snap once the fling ends.
            let quiet = self
                .scroll_quiet
                .get(&elem)
                .map_or(SETTLE_FRAMES, |last| self.frame_seq.saturating_sub(*last));
            if quiet >= SETTLE_FRAMES {
                self.begin_snap(&t);
            } else {
                self.any_active = true; // keep frames coming until it goes quiet
            }
        }
    }

    /// RFC-0021 reflected `page:` (the reverse direction): when the app sets the
    /// `page` var, scroll the `ScrollView`'s offset to that page. Edge-triggered
    /// against [`scroll_page_last`](Self::scroll_page_last) so it fires only on an
    /// external change (a drag never writes `page` mid-gesture, and our own snap
    /// updates the tracker), never level-triggered against the live offset, so it
    /// can't fight scrolling. Runs at the top of `render` over the previous
    /// frame's targets.
    fn sync_page_offsets(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let (Some(psig), Some(elem)) = (t.page_sig, t.elem) else {
                continue;
            };
            let Some(page) = self.ctx.peek_signal(psig).as_int() else {
                continue;
            };
            if self.scroll_page_last.get(&elem) == Some(&page) {
                continue; // no external change since we last synced
            }
            let axis_vp =
                t.y.filter(|a| a.max > 0.0)
                    .map(|a| (a, t.rect.h))
                    .or_else(|| t.x.filter(|a| a.max > 0.0).map(|a| (a, t.rect.w)));
            if let Some((axis, vp)) = axis_vp {
                #[allow(clippy::cast_precision_loss)]
                let target = (page.max(0) as f32 * vp).clamp(0.0, axis.max);
                self.write_scroll(axis.sig, target);
            }
            self.scroll_page_last.insert(elem, page);
        }
    }

    /// RFC-0021 `on_end_reached`: fires `end_reached` once for any `ScrollView`
    /// whose visible bottom has crossed `end_threshold` of its content, debounced
    /// via [`end_reached_fired`](Self::end_reached_fired) until the offset falls
    /// back below the threshold (so appending items re-arms it).
    fn fire_end_reached(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let (Some(threshold), Some(elem)) = (t.end_threshold, t.elem) else {
                continue;
            };
            let axis_vp =
                t.y.filter(|a| a.max > 0.0)
                    .map(|a| (a, t.rect.h))
                    .or_else(|| t.x.filter(|a| a.max > 0.0).map(|a| (a, t.rect.w)));
            let Some((axis, vp)) = axis_vp else { continue };
            if axis.max <= 0.0 {
                continue;
            }
            let frac = (self.peek_scroll(axis.sig) + vp) / (axis.max + vp);
            if frac >= threshold {
                if self.end_reached_fired.insert(elem) {
                    self.router.fire_event(
                        &mut self.ctx,
                        elem,
                        super::events::EventKind::EndReached,
                        None,
                    );
                }
            } else {
                self.end_reached_fired.remove(&elem);
            }
        }
    }

    /// Snapshots one axis of a drag at the press: its live offset becomes the
    /// baseline the pointer travel is subtracted from (RFC-0005, IMPL-10).
    fn capture_drag_axis(&self, axis: ScrollAxis) -> ScrollDragAxis {
        let is_int = matches!(self.peek(axis.sig), Value::Int(_));
        ScrollDragAxis {
            sig: axis.sig,
            start_offset: self.peek_scroll(axis.sig),
            max: axis.max,
            is_int,
        }
    }

    /// Applies a drag's pointer `travel` (current − press) to one axis: the
    /// content follows the pointer, so the offset is the press offset minus the
    /// travel, clamped to `[0, max]` (RFC-0005 drag-to-scroll).
    fn write_drag_axis(&mut self, axis: ScrollDragAxis, travel: f32) {
        let next = (axis.start_offset - travel).clamp(0.0, axis.max);
        let value = if axis.is_int {
            #[allow(clippy::cast_possible_truncation)]
            Value::Int(next.round() as i64)
        } else {
            Value::Float(f64::from(next))
        };
        self.write_var(axis.sig, value);
    }

    /// Draws the default pull-to-refresh indicator (RFC-0021): a light ring
    /// centred in the revealed gap that grows and fades in with `progress`
    /// (0 → threshold) and holds solid while a refresh is `active`. `gap_px` is the
    /// pull region's on-screen height; `clip` the viewport rect. Drawn as a
    /// border-only [`DecoratedBox`] (transparent fill + full radii = a ring).
    fn push_pull_indicator(
        frame: &mut byard_core::frame::RenderFrame,
        clip: byard_core::frame::Rect,
        gap_px: f32,
        progress: f32,
        active: bool,
    ) {
        let r = (gap_px * 0.28).clamp(3.0, 11.0);
        let cx = clip.x + clip.width / 2.0;
        let cy = clip.y + gap_px / 2.0;
        let opacity = if active { 1.0 } else { progress };
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: byard_core::BoxInstance {
                rect: [cx - r, cy - r, 2.0 * r, 2.0 * r],
                color: [0.0; 4],
                radii: [r; 4],
                transform: byard_core::frame::Transform::IDENTITY,
                smooth: 0.0,
            },
            border_width: 2.0,
            border_color: [0.85, 0.84, 0.88, 1.0],
            opacity,
            dirty: true,
            ..Default::default()
        });
    }

    /// The elastic pull height for a raw over-drag, in logical px (RFC-0021
    /// pull-to-refresh): a diminishing-returns curve that asymptotes to
    /// [`PULL_MAX`], so the region resists ever harder the further it is pulled.
    fn pull_elastic(raw: f32) -> f32 {
        if raw <= 0.0 {
            0.0
        } else {
            PULL_MAX * raw / (raw + PULL_MAX)
        }
    }

    /// RFC-0021 pull-to-refresh: a downward over-drag past the top grows the pull
    /// region for `d`'s elem. `pointer_y` is the live pointer; the over-drag is the
    /// travel beyond whatever upward scroll the press had available (`0` for a
    /// non-scrolling pull view). A live drag cancels any retract spring.
    fn drive_pull_drag(&mut self, d: ScrollDrag, pointer_y: f32) {
        let Some(elem) = d.elem else { return };
        let start_y = d.y.map_or(0.0, |a| a.start_offset);
        let over = (pointer_y - d.start_pos.1 - start_y).max(0.0);
        if over > 0.0 {
            self.pull_anims.remove(&elem);
            self.pull_distance.insert(elem, Self::pull_elastic(over));
            self.any_active = true;
        } else {
            self.pull_distance.remove(&elem);
        }
    }

    /// RFC-0021 pull-to-refresh release: past [`PULL_THRESHOLD`] fire `refresh`,
    /// set `refreshing = true`, and rest the indicator at [`PULL_REST`]; otherwise
    /// retract the pull region to `0`.
    fn release_pull(&mut self, d: ScrollDrag) {
        let Some(elem) = d.elem else { return };
        let pull = self.pull_distance.get(&elem).copied().unwrap_or(0.0);
        if pull >= PULL_THRESHOLD {
            self.router
                .fire_event(&mut self.ctx, elem, super::events::EventKind::Refresh, None);
            if let Some(sig) = d.refreshing_sig {
                // The app owns the `refreshing` lifecycle: hold the indicator at
                // rest until it clears the flag (retracts via `sync_refreshing`).
                self.ctx.write_signal(sig, Value::Bool(true));
                self.refreshing_seen.insert(elem, true);
                self.begin_pull_settle(elem, PULL_REST);
            } else {
                // No `refreshing` binding, a momentary trigger; retract now.
                self.begin_pull_settle(elem, 0.0);
            }
        } else {
            self.begin_pull_settle(elem, 0.0);
        }
    }

    /// Springs `elem`'s pull region toward `target` (RFC-0021): the indicator rest
    /// height while refreshing, or `0` to retract it. Resolves instantly with no
    /// advancing clock (tests / non-animating hosts), mirroring `begin_snap`.
    fn begin_pull_settle(&mut self, elem: u32, target: f32) {
        if !self.clock_set {
            self.pull_anims.remove(&elem);
            if target <= 0.0 {
                self.pull_distance.remove(&elem);
            } else {
                self.pull_distance.insert(elem, target);
            }
            return;
        }
        let from = self.pull_distance.get(&elem).copied().unwrap_or(0.0);
        let curve = pack_curve(crate::interp::anim::Curve::DEFAULT_SPRING);
        self.pull_anims.insert(
            elem,
            PullAnim {
                motion: byard_core::frame::Motion {
                    from,
                    to: target,
                    start_ms: self.now_ms,
                    curve,
                },
                target,
            },
        );
        self.any_active = true;
    }

    /// Advances every in-flight pull-region spring one `render` (RFC-0021): writes
    /// the sampled height into [`pull_distance`](Self::pull_distance) and, on
    /// settle, pins the exact target (`0` retracts the indicator entirely).
    fn advance_pull_anims(&mut self) {
        const EPS_POS: f32 = 0.5;
        const EPS_VEL: f32 = 2.0;
        if self.pull_anims.is_empty() {
            return;
        }
        let now = self.now_ms;
        for (elem, anim) in self
            .pull_anims
            .iter()
            .map(|(e, a)| (*e, *a))
            .collect::<Vec<_>>()
        {
            if anim.motion.is_settled_with_eps(now, EPS_POS, EPS_VEL) {
                self.pull_anims.remove(&elem);
                if anim.target <= 0.0 {
                    self.pull_distance.remove(&elem);
                } else {
                    self.pull_distance.insert(elem, anim.target);
                }
            } else {
                self.pull_distance.insert(elem, anim.motion.sample(now));
                self.any_active = true;
            }
        }
    }

    /// RFC-0021 reflected `refreshing:`: honour an app-driven change (edge-triggered
    /// against [`refreshing_seen`](Self::refreshing_seen), never fighting the
    /// engine's own set on trigger). Clearing it (`true → false`) retracts the
    /// indicator; setting it programmatically (`false → true`) shows it at rest.
    /// Runs at the top of `render` over the previous frame's targets.
    fn sync_refreshing(&mut self) {
        let targets: Vec<ScrollTarget> = self.scroll_targets.clone();
        for t in targets {
            let (Some(sig), Some(elem)) = (t.refreshing_sig, t.elem) else {
                continue;
            };
            let r = matches!(self.ctx.peek_signal(sig), Value::Bool(true));
            if self.refreshing_seen.get(&elem) == Some(&r) {
                continue;
            }
            self.begin_pull_settle(elem, if r { PULL_REST } else { 0.0 });
            self.refreshing_seen.insert(elem, r);
        }
    }

    /// Converts winit-sourced input events to interpreter event payloads and dispatches them to the `EventRouter`.
    pub fn dispatch_events(&mut self, events: &[byard_core::InputEvent]) {
        use crate::interp::events::{EventKind as CompKind, InputEvent as CompEvent};
        use byard_core::platform::{EventKind as CoreKind, InputPayload};

        /// Logical pixels a `ScrollView` scrolls per wheel line (RFC-0005).
        const WHEEL_LINE_PX: f32 = 40.0;

        // RFC-0030 §I1: hit-testing, gesture recognition and handler
        // invocation, all interpreter work, all gone in an AOT build.
        byard_core::profile_scope!(
            "interp.dispatch_events",
            byard_core::telemetry::ScopeKind::Interpreter
        );

        // RFC-0039: offer each event to the native views under the pointer,
        // innermost first, before anything else looks at it. A view that
        // handles one stops it there, which is the rule an intrinsic's handler
        // follows; a view that declines is invisible to the rest of routing,
        // which is the rule an element with no listener follows (RFC-0003).
        let consumed = self.dispatch_to_native_views(events);

        let comp_events: Vec<CompEvent> = events
            .iter()
            .enumerate()
            .filter(|(i, _)| !consumed[*i])
            .map(|(_, ev)| {
                let kind = match ev.kind {
                    CoreKind::PointerDown => CompKind::PointerDown,
                    CoreKind::PointerUp => CompKind::PointerUp,
                    CoreKind::Tap => CompKind::Tap,
                    CoreKind::PointerMove => CompKind::PointerMove,
                    CoreKind::Scroll => CompKind::Scroll,
                    CoreKind::Wheel => CompKind::Wheel,
                    CoreKind::Change => CompKind::Change,
                    CoreKind::KeyDown => CompKind::KeyDown,
                    CoreKind::KeyUp => CompKind::KeyUp,
                    CoreKind::TextInput => CompKind::TextInput,
                    CoreKind::PointerEnter => CompKind::PointerEnter,
                    CoreKind::PointerExit => CompKind::PointerExit,
                    CoreKind::Hover => CompKind::Hover,
                    CoreKind::LongPress => CompKind::LongPress,
                    CoreKind::DoubleTap => CompKind::DoubleTap,
                    CoreKind::Secondary => CompKind::Secondary,
                };
                let value = ev.payload.as_ref().map(|p| match p {
                    InputPayload::Str(s) => Value::Str(s.clone()),
                    InputPayload::Bool(b) => Value::Bool(*b),
                    InputPayload::Float(f) => Value::Float(f64::from(*f)),
                    InputPayload::Key(k) => Value::Str(k.clone()),
                });
                CompEvent {
                    kind,
                    pos: ev.pos,
                    delta: ev.delta,
                    value,
                    time_ms: ev.time_ms,
                }
            })
            .collect();

        // RFC-0005 `ScrollView` wheel: a wheel/scroll over a recorded scroll
        // target nudges whichever of `offset.x`/`offset.y` is writable, each
        // clamped to `[0, content − viewport]`. Wheel deltas are line-based (× a
        // per-line step); trackpad `Scroll` deltas are already pixels. Done here,
        // before the render, so the same tick paints the new offset (paint-time
        // translate, no relayout, INV-8).
        for (i, ev) in events.iter().enumerate() {
            if consumed[i] {
                // A native view took this wheel event (a chart panning its own
                // axis, say), so the ScrollView underneath must not also
                // scroll on it.
                continue;
            }
            let step = match ev.kind {
                CoreKind::Wheel => WHEEL_LINE_PX,
                CoreKind::Scroll => 1.0,
                _ => continue,
            };
            let (px, py) = ev.pos;
            let Some(t) = self
                .scroll_targets
                .iter()
                .rev()
                .find(|t| {
                    px >= t.rect.x
                        && px < t.rect.x + t.rect.w
                        && py >= t.rect.y
                        && py < t.rect.y + t.rect.h
                })
                .copied()
            else {
                continue;
            };
            // Wheel forward (delta > 0) reveals earlier content → offset shrinks.
            if let Some(axis) = t.x {
                self.nudge_scroll(axis, ev.delta.0 * step);
            }
            if let Some(axis) = t.y {
                self.nudge_scroll(axis, ev.delta.1 * step);
            }
            // RFC-0021: mark this elem freshly scrolled and cancel any in-flight
            // snap glide, the user is driving again, so `settle_snaps` restarts
            // its quiet countdown and only snaps once the fling truly ends.
            if let Some(elem) = t.elem {
                self.scroll_quiet.insert(elem, self.frame_seq);
                self.snap_anims.remove(&elem);
                if let Some((axis, _)) = Self::scrollable_axis(&t) {
                    let off = self.peek_scroll(axis.sig);
                    self.record_scroll_velocity(elem, off, ev.time_ms);
                }
            }
        }

        // RFC-0005 `ScrollView` drag-to-scroll: a pointer press on inert scroll
        // content starts a drag; each move slides the offset (on every writable
        // axis) so the content tracks the pointer, a pure function of the
        // press-relative travel, no accumulated drift (IMPL-10); release ends it.
        // The press defers to interactive children via `claims_pointer`, so a
        // button or slider inside the list still wins its own gesture.
        for ev in events {
            match ev.kind {
                CoreKind::PointerDown => {
                    let (px, py) = ev.pos;
                    // RFC-0026: an edge swipe outranks everything under it,
                    // that narrow strip is the platform's back gesture, and a
                    // scrollable or tappable child there must not steal it.
                    if self.begin_nav_swipe(ev.pos) {
                        continue;
                    }
                    let target = if self.router.claims_pointer(ev.pos) {
                        None
                    } else {
                        self.scroll_targets
                            .iter()
                            .rev()
                            .find(|t| {
                                px >= t.rect.x
                                    && px < t.rect.x + t.rect.w
                                    && py >= t.rect.y
                                    && py < t.rect.y + t.rect.h
                            })
                            .copied()
                    };
                    // A press reclaims the view, cancel any in-flight snap glide
                    // so the finger, not the spring, owns the offset (RFC-0021).
                    if let Some(elem) = target.and_then(|t| t.elem) {
                        self.snap_anims.remove(&elem);
                    }
                    self.scroll_drag = target.map(|t| ScrollDrag {
                        start_pos: (px, py),
                        x: t.x.map(|a| self.capture_drag_axis(a)),
                        y: t.y.map(|a| self.capture_drag_axis(a)),
                        elem: t.elem,
                        pull_refresh: t.pull_refresh,
                        refreshing_sig: t.refreshing_sig,
                    });
                    // A fresh press cancels a pull view's retract spring and resets
                    // fling-velocity tracking so this gesture starts clean (RFC-0021).
                    if let Some(elem) = target.and_then(|t| t.elem) {
                        self.pull_anims.remove(&elem);
                        self.scroll_vel.remove(&elem);
                        self.scroll_vel_last.remove(&elem);
                    }
                }
                CoreKind::PointerMove => {
                    if self.nav_swipe.is_some() {
                        self.drive_nav_swipe(ev.pos);
                        continue;
                    }
                    if let Some(d) = self.scroll_drag {
                        if let Some(a) = d.x {
                            let travel = ev.pos.0 - d.start_pos.0;
                            self.write_drag_axis(a, travel);
                        }
                        if let Some(a) = d.y {
                            let travel = ev.pos.1 - d.start_pos.1;
                            self.write_drag_axis(a, travel);
                        }
                        if d.pull_refresh {
                            self.drive_pull_drag(d, ev.pos.1);
                        }
                        // RFC-0021 fling projection: track the drag's velocity on
                        // its scrollable axis (vertical preferred) for the release.
                        if let Some(elem) = d.elem {
                            let axis = d.y.filter(|a| a.max > 0.0).or(d.x);
                            if let Some(a) = axis {
                                let off = self.peek_scroll(a.sig);
                                self.record_scroll_velocity(elem, off, ev.time_ms);
                            }
                        }
                    }
                }
                CoreKind::PointerUp | CoreKind::Tap => {
                    // RFC-0026: a released edge swipe either completes its pop
                    // or springs back, the finger's progress hands straight
                    // over to the spring.
                    self.release_nav_swipe();
                    // RFC-0021 snap: on release, settle the offset to the nearest
                    // page for a `snap: page` ScrollView (before clearing the drag).
                    if let Some(d) = self.scroll_drag {
                        self.snap_scroll_on_release(d.start_pos);
                        if d.pull_refresh {
                            self.release_pull(d);
                        }
                    }
                    self.scroll_drag = None;
                }
                _ => {}
            }
        }

        // RFC-0021: after this tick's scroll writes, reflect `page` continuously
        // (so pagination tracks wheel/trackpad scrolling, not just snap settles)
        // and fire `on_end_reached` for anything past its `end_threshold`.
        self.reflect_pages();
        self.fire_end_reached();
        // RFC-0026: a navigation that settled during this frame's render.
        self.fire_route_changes();

        self.router
            .dispatch_tick(&mut self.ctx, Some(&self.atlas), comp_events);
    }
}

/// One `Overlay`'s built layout (RFC-0017): its absolute wrapper node plus a
/// per-child emission slot. Holds borrows into the frozen render tree, so its
/// lifetime is scoped to a single [`Interpreter::render`] call.
struct OverlayLayout<'a> {
    /// The `RenderNode::Overlay` this describes (source of `attrs`/`children`).
    node: &'a RenderNode,
    /// The absolute wrapper container in the atlas; its node index doubles as
    /// the modal scrim's element id.
    wrapper_id: byard_core::atlas::layout::AtlasNodeId,
    /// One slot per built child, in declaration order.
    children: Vec<OverlayChildSlot<'a>>,
}

/// A single overlay child ready to emit (RFC-0017): the child render node, its
/// atlas id, and the flat-id list its render walk consumes.
struct OverlayChildSlot<'a> {
    node: &'a RenderNode,
    id: byard_core::atlas::layout::AtlasNodeId,
    flat_ids: Vec<byard_core::atlas::layout::AtlasNodeId>,
}

/// The absolute, inset-0 anchor wrapper style for an overlay child (RFC-0017
/// §Positioning). Direction is `Column`, so `justify` drives the vertical edge
/// and `align` the horizontal one. An unanchored child keeps the default
/// (`Start`/`Stretch`), so a `grow` scrim fills the viewport; an anchored child
/// is pinned to the requested edge/centre.
/// Where an anchored overlay's box goes, given its anchor's rect (RFC-0036).
///
/// Pure, and separated from the render walk on purpose: placement is the part
/// with the arithmetic and the flip rule, and it is the part worth testing
/// without a GPU, a frame, or a layout tree.
///
/// `edge` picks the side of `anchor` to sit on, `align` lines the overlay up
/// along the other axis, and `gap` is the space between the two boxes.
#[must_use]
fn anchor_placement(
    anchor: crate::interp::intrinsics::Rect,
    size: (f32, f32),
    edge: &str,
    align: &str,
    gap: f32,
) -> (f32, f32) {
    let (w, h) = size;
    // Along the anchor's cross axis: `start` lines the near edges up, `end`
    // the far ones, `center` the midpoints.
    let across = |anchor_start: f32, anchor_len: f32, own_len: f32| match align {
        "end" => anchor_start + anchor_len - own_len,
        "center" => anchor_start + (anchor_len - own_len) / 2.0,
        _ => anchor_start,
    };
    match edge {
        "above" => (across(anchor.x, anchor.w, w), anchor.y - gap - h),
        "before" => (anchor.x - gap - w, across(anchor.y, anchor.h, h)),
        "after" => (anchor.x + anchor.w + gap, across(anchor.y, anchor.h, h)),
        // `below` is the default, and the one an autocomplete wants.
        _ => (across(anchor.x, anchor.w, w), anchor.y + anchor.h + gap),
    }
}

/// The opposite side, for the flip (RFC-0036 §"Flip logic").
#[must_use]
fn flipped_edge(edge: &str) -> &'static str {
    match edge {
        "above" => "below",
        "before" => "after",
        "after" => "before",
        _ => "above",
    }
}

/// Places an anchored overlay, flipping to the opposite edge when the first
/// choice would leave the viewport (RFC-0036).
///
/// Default-on, because an autocomplete that renders off the bottom of the
/// window is a bug in almost every case; `anchor_flip: false` opts out.
///
/// A flip happens at most once. If both sides overflow — a viewport too small
/// to hold the overlay either way — the side with more room wins and the box is
/// clamped into view rather than rendered where nobody can reach it.
#[must_use]
fn anchor_placement_flipped(
    anchor: crate::interp::intrinsics::Rect,
    size: (f32, f32),
    edge: &str,
    align: &str,
    gap: f32,
    flip: bool,
    viewport: (f32, f32),
) -> (f32, f32) {
    let (w, h) = size;
    let (vw, vh) = viewport;
    let fits = |(x, y): (f32, f32)| x >= 0.0 && y >= 0.0 && x + w <= vw && y + h <= vh;

    let first = anchor_placement(anchor, size, edge, align, gap);
    if !flip || fits(first) {
        return clamp_into(first, size, viewport);
    }
    let other = anchor_placement(anchor, size, flipped_edge(edge), align, gap);
    if fits(other) {
        return other;
    }
    // Neither fits. Keep whichever leaves more of the overlay on screen, then
    // clamp; a half-visible dropdown over its own field is the documented
    // last resort and beats one that is entirely off-screen.
    let visible = |(x, y): (f32, f32)| {
        let vx = (x + w).min(vw) - x.max(0.0);
        let vy = (y + h).min(vh) - y.max(0.0);
        vx.max(0.0) * vy.max(0.0)
    };
    let best = if visible(other) > visible(first) {
        other
    } else {
        first
    };
    clamp_into(best, size, viewport)
}

/// Slides a box back inside the viewport without resizing it.
#[must_use]
fn clamp_into(pos: (f32, f32), size: (f32, f32), viewport: (f32, f32)) -> (f32, f32) {
    let (w, h) = size;
    let (vw, vh) = viewport;
    // `max(0.0)` last, so a box larger than the viewport pins to the top-left
    // rather than to a negative coordinate.
    (pos.0.min(vw - w).max(0.0), pos.1.min(vh - h).max(0.0))
}

/// The nearest declared anchor name to `name`, for the diagnostic's hint.
fn closest_anchor(name: &str, declared: &[String]) -> Option<String> {
    declared
        .iter()
        .filter(|d| {
            // Cheap and good enough for a hint: same first letter and a
            // similar length is what a typo usually looks like.
            d.chars().next() == name.chars().next() && d.len().abs_diff(name.len()) <= 2
        })
        .min_by_key(|d| d.len().abs_diff(name.len()))
        .cloned()
}

fn anchor_wrapper_style(anchor: Option<&str>) -> byard_core::atlas::layout::ContainerStyle {
    use byard_core::atlas::layout::{Align, ContainerStyle, FlexDir, Justify};
    let mut style = ContainerStyle::default()
        .with_absolute(true)
        .with_direction(FlexDir::Column);
    let (justify, align) = match anchor {
        Some("center") => (Some(Justify::Center), Some(Align::Center)),
        Some("top") => (Some(Justify::Start), Some(Align::Center)),
        Some("bottom") => (Some(Justify::End), Some(Align::Center)),
        Some("start") => (Some(Justify::Center), Some(Align::Start)),
        // RFC-0036: an element-anchored child sizes to its content and is
        // moved into place afterwards, so it pins to the origin and stretches
        // on neither axis.
        Some("__anchored") => (Some(Justify::Start), Some(Align::Start)),
        Some("end") => (Some(Justify::Center), Some(Align::End)),
        // No anchor (a scrim): keep flow defaults so `grow` fills the viewport.
        _ => (None, None),
    };
    if let Some(j) = justify {
        style = style.with_justify(j);
    }
    if let Some(a) = align {
        style = style.with_align(a);
    }
    style
}

/// Renders a value for string interpolation (`"Count: {count}"`).
/// Coerces a spacing side/scalar value to `f32`; only numeric values are valid
/// `Len`s (a non-numeric side is a `TypeMismatch`).
fn spacing_value(v: &Value) -> Option<f32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    match v {
        Value::Int(n) => Some(*n as f32),
        Value::Float(f) => Some(*f as f32),
        _ => None,
    }
}

/// Inserts `attr` into a resolved style set, replacing any existing attribute
/// with the same name and sub-property axis (last-wins) or appending it, so a
/// spread/inline override cleanly supersedes an earlier value (RFC-0016).
fn override_attr(set: &mut Vec<Attr>, attr: Attr) {
    if let Some(existing) = set
        .iter_mut()
        .find(|a| a.name == attr.name && a.axis == attr.axis)
    {
        *existing = attr;
    } else {
        set.push(attr);
    }
}

/// Builds a flat attribute list for *validation only* (RFC-0016): the base
/// attributes followed by every `on <state>` block's attributes, so a state
/// block's `bg:`/`scale:`/… is checked against the intrinsic's §5 contract just
/// like an inline attribute. Never emitted, rendering keeps base and states
/// separate so states resolve per-frame against the live mask.
fn attrs_with_states(base: &[Attr], states: &[StateBlock]) -> Vec<Attr> {
    if states.is_empty() {
        return base.to_vec();
    }
    let mut all = base.to_vec();
    for sb in states {
        all.extend(sb.attrs.iter().cloned());
    }
    all
}

/// The `StyleState` bit a single [`StyleStateKind`] maps to (RFC-0024).
fn state_bit(kind: StyleStateKind) -> crate::interp::events::StyleState {
    use crate::interp::events::StyleState;
    match kind {
        StyleStateKind::Hover => StyleState::HOVER,
        StyleStateKind::Pressed => StyleState::PRESSED,
        StyleStateKind::Focused => StyleState::FOCUSED,
        StyleStateKind::Disabled => StyleState::DISABLED,
        StyleStateKind::Checked => StyleState::CHECKED,
        StyleStateKind::Selected => StyleState::SELECTED,
        StyleStateKind::Invalid => StyleState::INVALID,
        StyleStateKind::Indeterminate => StyleState::INDETERMINATE,
        StyleStateKind::Dragging => StyleState::DRAGGING,
    }
}

/// The combined-selector mask a state block requires (RFC-0024): every state in
/// `states` must be active for the block to apply.
fn state_block_mask(sb: &StateBlock) -> crate::interp::events::StyleState {
    sb.states
        .iter()
        .fold(crate::interp::events::StyleState::empty(), |m, &k| {
            m.union(state_bit(k))
        })
}

/// Resolves an element's effective attributes for the current interaction state
/// (RFC-0016 §"Resolution order", extended by RFC-0024 §2): a block applies when
/// its required mask is a **subset** of the live `StyleState` (all its states are
/// active). Matching blocks overlay the base last-wins, ordered by **specificity**
/// (number of states, a combined `on focused+hover` beats a single `on hover`)
/// then **declaration order** for equal specificity.
///
/// The common stateless case (no blocks) borrows the base with no allocation.
fn resolve_state_attrs<'a>(
    base: &'a [Attr],
    state_blocks: &[StateBlock],
    active: crate::interp::events::StyleState,
) -> std::borrow::Cow<'a, [Attr]> {
    if state_blocks.is_empty() {
        return std::borrow::Cow::Borrowed(base);
    }
    // Collect blocks whose full mask is active, tagged with (specificity, order).
    let mut matching: Vec<(u32, usize)> = state_blocks
        .iter()
        .enumerate()
        .filter_map(|(i, sb)| {
            let required = state_block_mask(sb);
            active.contains(required).then_some((required.count(), i))
        })
        .collect();
    if matching.is_empty() {
        return std::borrow::Cow::Borrowed(base);
    }
    // Apply lowest-specificity first, then declaration order, so a more specific
    // (or later) block wins on conflicting properties, the `(spec, idx)` tuples
    // sort lexicographically.
    matching.sort_unstable();
    let mut resolved = base.to_vec();
    for (_, idx) in matching {
        for a in &state_blocks[idx].attrs {
            override_state_attr(&mut resolved, a.clone());
        }
    }
    std::borrow::Cow::Owned(resolved)
}

/// Inserts a state-block attr over the resolved base set, keeping the base's
/// `with` animation shell when the state provides a bare value, the
/// RFC-0010 × RFC-0012/0016 state-driven-animation contract:
/// `blur: 0 with anim.spring()` + `on hover { blur: 16 }` must *animate* to
/// 16, not pop. The state changes the target; the base owns the curve. The
/// wrapped value reuses the base `Animated` node's span, which is the
/// persisted `Motion`'s key, so entering and leaving the state retargets
/// one interruptible animation instead of restarting a fresh one each flip.
fn override_state_attr(set: &mut Vec<Attr>, attr: Attr) {
    let Some(existing) = set
        .iter_mut()
        .find(|a| a.name == attr.name && a.axis == attr.axis)
    else {
        set.push(attr);
        return;
    };
    if let (
        AttrKind::Prop {
            value: Expr::Animated { anim, span, .. },
        },
        AttrKind::Prop { value: incoming },
    ) = (&existing.kind, &attr.kind)
    {
        if !matches!(incoming, Expr::Animated { .. }) {
            let wrapped = Attr {
                kind: AttrKind::Prop {
                    value: Expr::Animated {
                        value: Box::new(incoming.clone()),
                        anim: anim.clone(),
                        span: *span,
                    },
                },
                ..attr.clone()
            };
            *existing = wrapped;
            return;
        }
    }
    *existing = attr;
}

/// Multiplies a colour's alpha by `opacity`, folds an element's effective
/// opacity into the widget/text primitives it emits so a translucent control
/// dims as a whole, not just its background (RFC-0011 T4 approximation).
/// Evaluates one binary arithmetic operation (`+ - * /`, RFC-0020 enabler)
/// with numeric promotion: Int∘Int → Int (division truncates), any Float
/// operand → Float. Division by zero yields the zero of the promoted type and
/// a non-numeric operand yields [`Value::Unit`], the logic thread never
/// panics on user expressions. Pure and unit-testable.
fn eval_binary(op: BinOp, lhs: Value, rhs: Value) -> Value {
    match op {
        // Comparison (RFC-0027 §1) → Bool.
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            eval_compare(op, &lhs, &rhs)
        }
        // `&&`/`||` never reach here (lowered as control flow); the arithmetic
        // path handles `+ - * /` with string/list concat on `+`.
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::And | BinOp::Or => {
            match (lhs, rhs) {
                (Value::Int(a), Value::Int(b)) => Value::Int(match op {
                    BinOp::Add => a.wrapping_add(b),
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::Mul => a.wrapping_mul(b),
                    BinOp::Div => {
                        if b == 0 {
                            0
                        } else {
                            a.wrapping_div(b)
                        }
                    }
                    _ => 0,
                }),
                (Value::Int(a), Value::Float(b)) => eval_binary_f(op, a as f64, b),
                (Value::Float(a), Value::Int(b)) => eval_binary_f(op, a, b as f64),
                (Value::Float(a), Value::Float(b)) => eval_binary_f(op, a, b),
                // String concat / list concat (RFC-0027 §3/§4): `+` only.
                (a, b) if matches!(op, BinOp::Add) => eval_concat(a, b),
                _ => Value::Unit,
            }
        }
    }
}

/// String and list concatenation (RFC-0027 §3/§4). A `Str` on either side
/// coerces the other operand through the shared scalar formatter
/// ([`format_scalar`]); two `List`s concatenate; anything else is `Unit` (the
/// checker reports the mismatch, INV-4).
fn eval_concat(a: Value, b: Value) -> Value {
    // A `List` operand only concatenates with another `List`, it never string-
    // coerces (RFC-0027 §3). A `Str` on either side coerces the other *scalar*.
    match (a, b) {
        (Value::List(mut xs), Value::List(ys)) => {
            xs.extend(ys);
            Value::List(xs)
        }
        (Value::Str(mut s), other) if is_scalar(&other) => {
            s.push_str(&format_scalar(&other));
            Value::Str(s)
        }
        (other, Value::Str(s)) if is_scalar(&other) => {
            let mut out = format_scalar(&other);
            out.push_str(&s);
            Value::Str(out)
        }
        _ => Value::Unit,
    }
}

/// Whether a value is a formattable scalar (`Int`/`Float`/`Bool`/`Str`), the
/// operands `Str + _` will coerce (RFC-0027 §3). `List`/`Record`/`Unit` are not.
fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Str(_)
    )
}

/// Comparison (RFC-0027 §1) → `Bool`. Numeric operands compare with Int→Float
/// promotion; `Str` by value/lexicographic order; `Bool` by `==`/`!=` only;
/// `List`/`Record` by structural equality (ordering on them is a checker
/// `TypeMismatch`, handled there). Incompatible operands degrade to
/// `Bool(false)` at runtime (never a panic; the checker reports the mismatch).
fn eval_compare(op: BinOp, a: &Value, b: &Value) -> Value {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => {
            // Bool supports eq only; ordering is meaningless → not-equal-based.
            return Value::Bool(match op {
                BinOp::Eq => x == y,
                BinOp::Ne => x != y,
                _ => false,
            });
        }
        // Structural equality for List/Record (and any other same-shape pair).
        _ => {
            let eq = structural_eq(a, b);
            return Value::Bool(match op {
                BinOp::Eq => eq,
                BinOp::Ne => !eq,
                _ => false,
            });
        }
    };
    let Some(ord) = ord else {
        return Value::Bool(false);
    };
    Value::Bool(match op {
        BinOp::Eq => ord == Ordering::Equal,
        BinOp::Ne => ord != Ordering::Equal,
        BinOp::Lt => ord == Ordering::Less,
        BinOp::Le => ord != Ordering::Greater,
        BinOp::Gt => ord == Ordering::Greater,
        BinOp::Ge => ord != Ordering::Less,
        _ => false,
    })
}

/// Structural (element/field-wise) equality for lists and records (RFC-0027
/// §1), with numeric Int↔Float promotion at the leaves.
fn structural_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => (x - y).abs() < f64::EPSILON,
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => {
            (*x as f64 - *y).abs() < f64::EPSILON
        }
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        (Value::List(xs), Value::List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| structural_eq(x, y))
        }
        (Value::Record(xs), Value::Record(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|((kx, vx), (ky, vy))| kx == ky && structural_eq(vx, vy))
        }
        _ => false,
    }
}

/// Resolves a data member access (RFC-0027 §4/§6): `xs.len` (list/string
/// length → `Int`) or `r.field` (record field → its value). Anything else
/// degrades to `Unit` (INV-4).
fn data_member(base: &Value, field: &Symbol) -> Value {
    let f = field.as_str();
    match base {
        Value::List(xs) if f == "len" => Value::Int(i64::try_from(xs.len()).unwrap_or(i64::MAX)),
        Value::Str(s) if f == "len" => {
            Value::Int(i64::try_from(s.chars().count()).unwrap_or(i64::MAX))
        }
        Value::Record(fields) => fields
            .iter()
            .find(|(k, _)| k.as_str() == f)
            .map_or(Value::Unit, |(_, v)| v.clone()),
        _ => Value::Unit,
    }
}

/// Runs `f` with `elem` installed as the current per-element/lambda binding
/// (RFC-0027 §5), reusing the payload slot the lambda body was lowered against.
/// The previous slot value is saved and restored so a `map`/`filter` nested in
/// an event action never clobbers that action's payload.
fn with_lambda_elem<F: FnOnce() -> Value>(elem: Value, f: F) -> Value {
    let prev = CURRENT_PAYLOAD.with(|cell| cell.borrow_mut().replace(elem));
    let out = f();
    CURRENT_PAYLOAD.with(|cell| *cell.borrow_mut() = prev);
    out
}

/// Indexes `base[index]` (RFC-0027 §4): a `List` at an in-range integer index
/// yields the element; out-of-range or non-list/non-int degrades to `Unit`
/// (INV-4, never a panic). Negative indices are out of range.
fn index_value(base: &Value, index: &Value) -> Value {
    match (base, index) {
        (Value::List(xs), Value::Int(i)) => usize::try_from(*i)
            .ok()
            .and_then(|i| xs.get(i))
            .cloned()
            .unwrap_or(Value::Unit),
        _ => Value::Unit,
    }
}

/// The Float leg of [`eval_binary`]. `x / 0.0` yields `0.0`, not an
/// IEEE infinity/NaN, a NaN sweep or width would poison layout and paint.
fn eval_binary_f(op: BinOp, a: f64, b: f64) -> Value {
    Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => {
            if b == 0.0 {
                0.0
            } else {
                a / b
            }
        }
        _ => 0.0,
    })
}

fn dim_alpha(mut color: [f32; 4], opacity: f32) -> [f32; 4] {
    color[3] *= opacity;
    color
}

/// Converts a packed `0xRRGGBB` colour to OKLab `[L, a, b]` for perceptually
/// uniform interpolation (RFC-0010 A3).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)] // standard colour-space notation
fn oklab_from_hex(hex: i64) -> [f32; 3] {
    let r = srgb_to_linear(((hex >> 16) & 0xFF) as f32 / 255.0);
    let g = srgb_to_linear(((hex >> 8) & 0xFF) as f32 / 255.0);
    let b = srgb_to_linear((hex & 0xFF) as f32 / 255.0);
    // Björn Ottosson's linear-sRGB → OKLab.
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    ]
}

/// Converts OKLab `[L, a, b]` back to a packed `0xRRGGBB` colour, clamping any
/// out-of-gamut result (a spring can overshoot a channel).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)] // standard colour-space notation
fn hex_from_oklab(lab: [f32; 3]) -> i64 {
    let [big_l, a, b] = lab;
    let l_ = big_l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = big_l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = big_l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s;
    let g = -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s;
    let bl = -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s;
    let to_byte = |c: f32| -> i64 { (linear_to_srgb(c).clamp(0.0, 1.0) * 255.0).round() as i64 };
    (to_byte(r) << 16) | (to_byte(g) << 8) | to_byte(bl)
}

/// Reads an already-evaluated two-axis value (`(x, y)` positional or
/// `(x: …, y: …)` named) as a pair, leaving unset axes at `default`.
///
/// The value-level counterpart of `resolve_axis_pair_value`'s syntactic tuple
/// handling, for pairs that only exist after evaluation, a keyframed
/// `translate` (RFC-0025 §3) being the case that needs it.
fn axis_pair_of_value(value: &Value, default: (f32, f32)) -> Option<(f32, f32)> {
    let items = value.as_tuple()?;
    if items.iter().all(|(name, _)| name.is_some()) {
        let axis = |want: &str| {
            items
                .iter()
                .find(|(name, _)| name.as_ref().is_some_and(|n| n.as_str() == want))
                .and_then(|(_, v)| spacing_value(v))
        };
        return Some((
            axis("x").unwrap_or(default.0),
            axis("y").unwrap_or(default.1),
        ));
    }
    let [(_, x), (_, y)] = items else {
        return None;
    };
    Some((spacing_value(x)?, spacing_value(y)?))
}

/// Splits a packed colour into the four channels a colour animation drives:
/// OKLab `L`/`a`/`b` plus alpha as `0..=1` (RFC-0010 A3, RFC-0023's alpha ramp).
///
/// Alpha is auto-detected exactly as every other colour consumer does it (the
/// lexer's 8-digit tag, else the magnitude heuristic), which is what lets
/// `0x00FFFFFF → 0x80FFFFFF` ramp instead of popping.
fn color_channels(hex: i64) -> [f32; 4] {
    let lab = oklab_from_hex(hex);
    #[allow(clippy::cast_precision_loss)]
    let alpha = if super::intrinsics::color_has_alpha(hex) {
        ((hex >> 24) & 0xFF) as f32 / 255.0
    } else {
        1.0
    };
    [lab[0], lab[1], lab[2], alpha]
}

/// Packs the four animated channels back into `0xAARRGGBB`, tagged like an
/// 8-digit literal so a downstream consumer honours a mid-ramp alpha of exactly
/// zero instead of reading it as opaque.
fn color_from_channels(ch: [f32; 4]) -> i64 {
    let rgb = hex_from_oklab([ch[0], ch[1], ch[2]]);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let alpha_byte = i64::from((ch[3].clamp(0.0, 1.0) * 255.0).round() as u8);
    crate::lexer::COLOR_HAS_ALPHA_TAG | (alpha_byte << 24) | rgb
}

/// Mixes two packed colours in OKLab at factor `t` (RFC-0025 §3 keyframes).
fn mix_hex_oklab(a: i64, b: i64, t: f32) -> i64 {
    let (from, to) = (color_channels(a), color_channels(b));
    color_from_channels(std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t))
}

/// sRGB gamma → linear (per channel).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear → sRGB gamma (per channel).
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Packs the compiler's typed [`Curve`](crate::interp::anim::Curve) into the
/// engine's POD [`MotionCurve`](byard_core::frame::MotionCurve) (RFC-0010), so
/// a resolved curve crosses the frame boundary as plain data.
fn pack_curve(curve: crate::interp::anim::Curve) -> byard_core::frame::MotionCurve {
    use crate::interp::anim::{Curve, EaseKind};
    use byard_core::frame::MotionCurve;
    #[allow(clippy::cast_precision_loss)]
    match curve {
        Curve::Linear { ms } => MotionCurve {
            kind: MotionCurve::LINEAR,
            params: [ms as f32, 0.0, 0.0],
        },
        Curve::Ease { ms, kind } => MotionCurve {
            kind: match kind {
                EaseKind::In => MotionCurve::EASE_IN,
                EaseKind::Out => MotionCurve::EASE_OUT,
                EaseKind::InOut => MotionCurve::EASE_IN_OUT,
            },
            params: [ms as f32, 0.0, 0.0],
        },
        Curve::Spring {
            stiffness,
            damping,
            v0,
        } => MotionCurve {
            kind: MotionCurve::SPRING,
            params: [stiffness, damping, v0],
        },
    }
}

/// Assigns one resolved side of a named spacing tuple, recording a
/// [`CompileError::ConflictingSpacingField`] if the side was already set (either
/// directly or via an axis shorthand).
fn assign_side(
    slot: &mut Option<f32>,
    v: f32,
    side: &str,
    span: Span,
    errors: &mut Vec<CompileError>,
) {
    if slot.is_some() {
        errors.push(CompileError::ConflictingSpacingField {
            span,
            message: format!("spacing side `{side}` was set more than once"),
        });
    } else {
        *slot = Some(v);
    }
}

/// The single scalar-formatting function (RFC-0027 §3): the display form used
/// by both `Text("{x}")` interpolation and `Str + scalar` concatenation, so the
/// two paths agree byte-for-byte. Non-scalar values format to an empty string.
fn format_scalar(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => s.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests;
