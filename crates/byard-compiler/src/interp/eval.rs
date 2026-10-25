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

mod access;
mod animation;
mod canvas;
mod controller;
mod editing;
mod element;
mod expr;
mod instance;
mod measure;
mod navigation;
mod overlay;
mod props;
mod scroll;
mod states;
mod typography;

use animation::{AnimKey, LoopClock};
use canvas::{CachedMesh, MESH_CACHE_FRAMES, ShapeGroupSink, attr_span, transformed_rrect_outline};
use controller::UNBOUND_CONTROLLER;
use instance::MAX_INSTANCE_DEPTH;
use navigation::{NavKind, NavPool, NavSharedCell};
use overlay::OverlayLayout;
use props::ShadowSpec;
use scroll::{
    PULL_THRESHOLD, PullAnim, ScrollAxis, ScrollDrag, ScrollTarget, SnapAlign, SnapAnim, SnapMode,
    WindowSpec, scrolled_hit_rect,
};
use states::{resolve_state_attrs, viewport_holds};

use super::env::{Env, SignalId, Value};
use super::reactive::{FrameTarget, ReactiveCtx, ScopeId};
use super::{dates, env, events, intrinsics, shape, theme};
use crate::diagnostics::{CompileError, Span};
use crate::parser::ast::{
    Attr, AttrKind, ElementNode, Expr, Member, Param, StateBlock, StyleStateKind, Type, ViewDecl,
};
use crate::symbol::Symbol;

/// Decimal places a `Slider` without an explicit `step` quantises its value to.
///
/// A continuous slider otherwise emits the full `f64` precision of a
/// pixel-derived ratio (e.g. `0.6035294…`); rounding keeps the bound value
/// readable. Authors who need a specific granularity set `step:` instead.
const SLIDER_DEFAULT_DECIMALS: i32 = 3;

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
            | Member::Change { span, .. }
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

/// An IME composition (RFC-0040 §1): the preedit, drawn at the caret of the
/// field that owns it, and never part of that field's value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composition {
    /// The element the composition belongs to. Kept after focus leaves it,
    /// with the preedit cleared, so a platform that commits on blur still
    /// commits into the right field (§6).
    owner: u32,
    /// The text being composed; empty once cleared.
    text: String,
    /// The IME's cursor inside `text`, as a byte range; `None` hides it.
    cursor: Option<(usize, usize)>,
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
/// fields are reactive-scope ids the engine reads each tick.
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
        /// The `var` signal bound via `bind:` or `value:` (value widgets).
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
        /// The `path { … }` child of a `Clip`, lifted out of the children and
        /// used as the clip's mask (RFC-0037 `clip(path)`). `None` for every
        /// other box, and for a `Clip` that only rounds its corners.
        ///
        /// Kept as the unevaluated element rather than as a mesh because its
        /// commands are ordinary expressions: a mask whose outline follows a
        /// `var` has to be re-evaluated on the frame the `var` moves, exactly as
        /// a filled path in a `Canvas` is.
        clip_path: Option<std::rc::Rc<ElementNode>>,
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
    /// A texture-sampled image.
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
        /// no separate lifetime to manage (an extension never keeps a resource
        /// alive past the scope that owns it).
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
    /// How many event handlers had been registered when the view was drawn.
    /// Handlers are registered in paint order, so any registered after this
    /// point belong to elements drawn over the view.
    drawn_at: usize,
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

/// The reserved parameter/element name for a user view's child-block slot
/// (RFC-0007 D-A). A `View` declaring a `content` parameter accepts a
/// `{ ... }` block at its call sites; referencing `content` as an element inside
/// the body splices the caller-supplied block.
pub(crate) const RESERVED_CONTENT: &str = "content";

/// How far a `TextField`'s text sits from its left edge when the field writes
/// no padding.
const FIELD_PAD_X: f32 = 10.0;
/// A placeholder is drawn in the field's `color:` at this share of its alpha.
const PLACEHOLDER_ALPHA: f32 = 0.55;
/// Where the caret's left edge sits relative to the boundary it is at, in
/// logical pixels. A little left of it, so a caret between two letters is in
/// the gap and not on the left side of the second one (RFC-0041 §6).
const CARET_DX: f32 = -0.5;
/// A selection is painted in the theme's `primary` at this share of its alpha
/// when the field writes no `selection:` colour (RFC-0041 §6).
const SELECTION_ALPHA: f32 = 0.3;

thread_local! {
    /// Thread-local storage holding the active payload of the event currently being processed.
    pub static CURRENT_PAYLOAD: std::cell::RefCell<Option<Value>> = const { std::cell::RefCell::new(None) };

    /// The arguments of the lambda currently running, by name, for a lambda
    /// with more than one parameter (`reduce`'s `(acc, x) => …`). The single
    /// parameter of `map`/`filter` still goes through [`CURRENT_PAYLOAD`].
    static LAMBDA_ARGS: std::cell::RefCell<Vec<(Symbol, Value)>> = const { std::cell::RefCell::new(Vec::new()) };
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
    /// The `var`s a reactive-compatible reload kept, until the root is
    /// lowered again. Lowering evaluates the view's declarations, and a kept
    /// `var` must keep its signal there too, or the state the reload kept is
    /// replaced by its initial value one step later.
    kept_vars: std::collections::HashSet<Symbol>,
    /// The program's record types (`type Place = …`), by name.
    records: std::rc::Rc<super::shape::Records>,
    /// The declared type of each `var` whose type names a record, which is
    /// what a value written into it is compared with.
    var_types: std::collections::HashMap<SignalId, Type>,
    /// The assignments a shape mismatch has been reported for, by span. Kept
    /// here rather than in the lowered action, because actions are lowered
    /// again every render and a flag in one would be a fresh flag each frame.
    shapes_told: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<(u32, u32)>>>,
    /// Incremental LayoutAtlas.
    pub atlas: byard_core::atlas::layout::LayoutAtlas,
    /// Interactive events router.
    pub router: crate::interp::events::EventRouter,
    /// Glyph-accurate text measurer, created lazily on first layout so the
    /// non-rendering paths (parsing, reactivity tests) never load fonts.
    /// Shared (RFC-0041 §4): a `TextField`'s handlers measure with it to turn a
    /// pointer into a caret, and an action can reach nothing but what it holds.
    text_measurer: editing::SharedMeasurer,
    /// Active design-token theme (RFC-0022; the theme-default layer).
    pub theme: super::theme::Theme,
    /// The viewport this frame is rendered at, in logical pixels: what a
    /// responsive block (RFC-0016 `on width >= md`) compares against. Set at
    /// the top of every render, before layout reads it.
    viewport: (f32, f32),
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
    /// How far the colour tokens are from the *previous* scheme's appearance
    /// to the current one's, `0.0..=1.0`, as a reactive `Float` (RFC-0016
    /// animated token transitions). `1.0` at rest, which is where every token
    /// reads exactly its scheme's value and nothing is blended.
    theme_mix: Option<SignalId>,
    /// The mix as last written, and the engine time it was written at.
    theme_mix_value: f32,
    theme_mix_at: Option<u32>,
    /// The scheme the mix is currently heading towards, so a flip is noticed
    /// in the tick it happens in.
    theme_mix_dark: Option<bool>,
    /// Parameterized `fn` definitions (`fn f(params) => body`) *and*
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
    animations: rustc_hash::FxHashMap<AnimKey, byard_core::frame::Motion>,
    /// Persisted colour-animation state (RFC-0010 A3): one `Motion` per OKLab
    /// channel (`L`, `a`, `b`) plus one for the alpha byte, so a
    /// `bg`/`color`/`border`/`backdrop_tint` transition interpolates in a
    /// perceptually-uniform space, no muddy mid-points, with translucency
    /// animating alongside (RFC-0023: a tint fading in is an alpha ramp), and
    /// is interruptible like the scalar props. Keyed by the `with` node's span.
    color_animations: rustc_hash::FxHashMap<AnimKey, [byard_core::frame::Motion; 4]>,
    /// Loop clocks for repeating, delayed and keyframed animations (RFC-0025),
    /// keyed by the animation node's span like the two maps above. A repeating
    /// animation cannot sample against `now − start_ms` the way a one-shot does:
    /// it needs its own timeline, which is what a [`LoopClock`] carries, plus
    /// the last-sampled stamp that implements §2's offscreen pause.
    anim_clocks: rustc_hash::FxHashMap<AnimKey, LoopClock>,
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
    /// Whether an `on measure` ran during the last [`render`](Self::render).
    /// Its writes land after layout, so only the next frame shows them.
    measure_fired: bool,
    /// The program's top-level functions (RFC-0002), by canonical name, as
    /// entries of `fn_table`. Rebuilt by [`Self::load_views`].
    top_fns: std::collections::HashMap<Symbol, super::env::AstId>,
    /// The top-level functions being inlined right now, outermost first, so a
    /// cycle is reported instead of lowered forever.
    inlining: Vec<Symbol>,
    /// Names bound as parameters of a multi-parameter lambda while its body is
    /// being lowered; `lower_ident` reads them from [`LAMBDA_ARGS`].
    lambda_params: Vec<Symbol>,
    /// The parameters of the functions being inlined right now, each with the
    /// memo of the argument it was called with. A controller call written
    /// inside one reads them when it is placed, so its arms see what the
    /// function was called with rather than what the argument has become by
    /// the time the answer arrives.
    inlined_params: Vec<(Symbol, ScopeId)>,
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
    /// The view [`lower_view`](Self::lower_view) last lowered, by name and
    /// declaration span: lowering a different one is a new tree (RFC-0032
    /// §R4).
    lowered_view: Option<(crate::symbol::Symbol, Span)>,
    /// The IME composition in progress, if any (RFC-0040 §2).
    ///
    /// Engine state, not app state: it has no name in byld and never touches
    /// a `var`. There is at most one, because there is at most one focus.
    composition: Option<Composition>,
    /// The editing state of every `TextField` that has needed one: its caret,
    /// selection and undo (RFC-0041 §1). Empty until a field is focused.
    edit: std::rc::Rc<std::cell::RefCell<super::edit::EditStore>>,
    /// The clipboard copy, cut and paste use (RFC-0041 §3); in memory unless
    /// the host supplies the system's through [`set_clipboard`](Self::set_clipboard).
    clipboard: editing::SharedClipboard,
    /// Whether the caret is in the off half of its blink (RFC-0041 §6).
    /// Inverted, so that the default, a caret that shows, is the zero value.
    caret_off: std::rc::Rc<std::cell::Cell<bool>>,
    /// The caret's blink timer while a field has focus in a window that does.
    blink: Option<editing::Blink>,

    /// What the caret's blink is conditioned on, besides having a field.
    caret: editing::CaretFlags,
    /// How many meshes have been tessellated this session, the measurement
    /// behind the caching claim (a performance claim names the path it measures).
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
    /// RFC-0042: the accessibility tree being built by the paint walk, when
    /// the frame asked for one. Empty, and unallocated, otherwise.
    access: access::AccessScratch,
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
    /// until somebody is told (no silent failures).
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
        // Every view carries the same program-wide function table; register
        // it once, fresh, so a reload replaces rather than accumulates names.
        self.top_fns.clear();
        if let Some(first) = views.first() {
            self.records = std::rc::Rc::new(
                first
                    .records
                    .iter()
                    .map(|r| (r.name.clone(), r.clone()))
                    .collect(),
            );
        }
        if let Some(first) = views.first() {
            for decl in first.helpers.iter() {
                let id = super::env::AstId(u32::try_from(self.fn_table.len()).unwrap_or(u32::MAX));
                let params = decl.params.iter().map(|p| p.name.clone()).collect();
                self.fn_table.push((params, decl.body.clone(), false));
                self.top_fns.insert(decl.name.clone(), id);
            }
        }
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
        // In the tick the flip happens in, and before the pull. The first is
        // the requirement: a restart deferred to a later tick would paint one
        // frame of the new scheme at full strength and then jump back into the
        // blend, which is a flash rather than a transition. Before the pull
        // rather than after it is only economy (render re-pulls before it
        // paints, so either order reaches the frame), but it lets the token
        // bindings be computed once with both the new scheme and the restarted
        // mix instead of twice.
        self.advance_theme_mix();
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
    /// a saved `.svg` regenerates live (RFC-0009 §3). The dev runner calls
    /// this on the logic thread when the file watcher reports an SVG change; the
    /// regenerated field reuses the same atlas cell, so the consuming `View`
    /// never remounts. Returns `true` if a cached asset matched `path`.
    pub fn invalidate_vector_asset(&mut self, path: &std::path::Path) -> bool {
        self.vector_jit.invalidate_path(path)
    }

    /// Points the vector JIT at a persistent on-disk field cache (RFC-0009 §5),
    /// so cold `byard dev` starts load previously generated fields instead
    /// of regenerating them. The dev runner passes `.byard/cache/vectors/`.
    pub fn set_vector_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.vector_jit.set_cache_dir(dir);
    }

    /// Whether any `with` animation was still in flight as of the last
    /// [`render`](Self::render). The runner keeps requesting frames while this
    /// is true and lets the app idle (0 frames) once every animation settles.
    #[must_use]
    pub fn has_active_animations(&self) -> bool {
        self.any_active || self.theme_transitioning()
    }

    /// Whether the host should produce another frame without waiting for an
    /// event: an animation is still in flight, or an `on measure` ran in the
    /// last [`render`](Self::render) and its writes (made after layout) are
    /// only visible in the next one. A measure that did not change its
    /// element's rect does not run, so a still screen settles at zero frames.
    #[must_use]
    pub fn needs_another_frame(&self) -> bool {
        self.has_active_animations() || self.measure_fired
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

    // ── declarations ────────────────────────────────────────────────────

    /// Processes the declaration-level members of a `View` body (`var`/`let`/
    /// `fn`/`inject`/bare expression). Elements are lowered by the intrinsics
    /// layer.
    pub fn eval_view_decls(&mut self, view: &ViewDecl) {
        for member in &view.body {
            self.eval_member(member);
        }
    }

    fn eval_member(&mut self, member: &Member) {
        match member {
            Member::Var { name, init, ty, .. } => {
                // A `var` the last reload kept is the signal it already has.
                if self.kept_vars.remove(name) {
                    if let Some(&sig) = self.var_sigs.get(name) {
                        self.env.push(name.clone(), Value::Signal(sig));
                        return;
                    }
                }
                let sig = self.define_var(name.clone(), init);
                if let Some(ty) = ty
                    .as_ref()
                    .filter(|t| super::shape::names_a_record(t, &self.records))
                {
                    self.var_types.insert(sig, ty.clone());
                }
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
                    self.define_let_or_fn(name, init);
                }
            }
            Member::Fn {
                name, params, body, ..
            } => self.define_fn(name, params, body),
            Member::Expr(e) => {
                // A bare statement member is an action, so a call written
                // there is in effect position like any other (RFC-0028 §4).
                let result = self.in_action_position(|s| s.eval_action(e));
                if let Err(err) = result {
                    self.errors.push(err);
                }
            }
            Member::Inject { ty, name, span } => {
                // Resolve `inject T as name` from the ambient environment chain.
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

    // ── Controller boundary ─────────────────────────────────────────

    /// Provides an ambient value keyed by `ty` to this view and its
    /// descendants (`inject T as name` resolution, RFC-0002 §inject).
    /// Call before [`lower_view`](Self::lower_view) so the environment is
    /// ready when the view body is evaluated.
    pub fn inject_provider(&mut self, ty: &str, value: Value) {
        self.env.provide(Symbol::intern(ty), value);
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
        // describes a tree that is gone (RFC-0028 §5: a reply for a call site
        // that no longer exists is discarded, never applied). The ambient
        // controller handles are re-provided because the environment they
        // lived in was just discarded.
        self.reset_bridge_state();
        self.provide_controllers();
        // The theme is ambient too, and lived in the environment just
        // discarded. Without it every `inject Theme` fails after a hot
        // reload and every token resolves to nothing (RFC-0022). The same
        // scheme signal, so the scheme the app is showing survives.
        if let Some(sig) = self.theme_scheme {
            self.env.provide(Symbol::intern("Theme"), Value::Theme(sig));
        }
        for member in &new_view.body {
            match member {
                Member::Var { name, init, .. } => {
                    if matches!(kind, ReloadKind::ReactiveCompatible) {
                        if let Some(&sig) = old.get(name) {
                            // Keep the live Signal (and its value).
                            self.env.push(name.clone(), Value::Signal(sig));
                            self.var_sigs.insert(name.clone(), sig);
                            self.kept_vars.insert(name.clone());
                            continue;
                        }
                    }
                    self.define_var(name.clone(), init);
                }
                Member::Let { name, init, .. } => self.define_let_or_fn(name, init),
                Member::Fn {
                    name, params, body, ..
                } => self.define_fn(name, params, body),
                _ => {}
            }
        }
    }

    /// `fn f(params) => body`. With no parameters it is a memo, read by
    /// `f()`; with parameters its body is inlined at each call with the
    /// arguments bound, which is what [`Self::lower_call`] does for it.
    fn define_fn(&mut self, name: &Symbol, params: &[Param], body: &Expr) {
        if params.is_empty() {
            self.define_let(name.clone(), body);
        } else {
            let names: Vec<Symbol> = params.iter().map(|p| p.name.clone()).collect();
            self.define_callable(name, names, body);
        }
    }

    /// `let f = (params) => body` is a function like `fn`, called the same
    /// way: `f()` runs the body, which in an action can be an action block
    /// (`let load = () => { state = "loading" http.get(...) ... }`). Any
    /// other `let` is a memo.
    fn define_let_or_fn(&mut self, name: &Symbol, init: &Expr) {
        if let Expr::Lambda { params, body, .. } = init {
            // `() => { ... }` parses its block as a block literal, which is a
            // parameterless lambda of its own (the `on_tap: { count++ }`
            // form). The body to inline is the block, not a lambda that would
            // only produce itself when called.
            let body = match body.as_ref() {
                Expr::Lambda {
                    params: inner,
                    body: block,
                    ..
                } if inner.is_empty() && matches!(block.as_ref(), Expr::Block(..)) => block,
                other => other,
            };
            self.define_callable(name, params.clone(), body);
        } else {
            self.define_let(name.clone(), init);
        }
    }

    /// Binds `name` to a body [`Self::lower_call`] inlines at each call.
    fn define_callable(&mut self, name: &Symbol, params: Vec<Symbol>, body: &Expr) {
        let id = crate::interp::env::AstId(u32::try_from(self.fn_table.len()).unwrap_or(u32::MAX));
        self.fn_table.push((params, body.clone(), false));
        self.env.push(name.clone(), Value::Fn(id));
    }

    /// `let y = init` (and a `fn` with no parameters), open a computed memo.
    pub fn define_let(&mut self, name: Symbol, init: &Expr) -> ScopeId {
        let compute = self.lower_expr(init, None);
        let scope = self.ctx.open_memo(compute);
        self.env.push(name, Value::Memo(scope));
        scope
    }

    /// Opens a value binding projecting `expr` into a fresh frame target
    /// (used by intrinsics and by tests).
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

    // ── element lowering (RFC-0005) ─────────────────────────────────────

    /// Resolves the `bind:` or `value:` attribute of a value widget to a
    /// `SignalId`. Returns `None` if no such attribute exists or it doesn't
    /// name a `var`.
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

        // RFC-0016 responsive variants compare against this. Before layout,
        // because a breakpoint can change padding, width or direction, and a
        // layout built against last frame's viewport would lay out one width's
        // variant and paint another's.
        self.viewport = (width, height);
        // Every frame answers `needs_another_frame` for itself, including one
        // that returns before layout.
        self.measure_fired = false;

        // RFC-0034: the registered families ride every frame, not just the one
        // after registration. The relay keeps only the latest frame, so a pool
        // handed over once is a pool a dropped frame loses for good; the table
        // is `Arc`-shared, so carrying all of it is a pointer clone and the
        // render thread skips what it already holds.
        frame.set_fonts(self.fonts.clone());

        // RFC-0042: reads the frame's flag once. Every accessibility hook in
        // the walk below starts with the plain `bool` this leaves behind.
        self.access_begin(frame);

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

        // Drain any MSDF generations that finished since the last tick, before
        // the tree walk below, so a freshly-resident glyph is visible the same
        // tick it lands (RFC-0009 §2; signals are touched on the logic thread
        // only).
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
            self.access_end(frame);
            return;
        };
        // Drive layout with the shared text measurer so wrapping `Text` leaves
        // reflow to their parent's width (RFC-0005 default wrap). Disjoint field
        // borrows: `self.atlas` and `self.text_measurer`.
        {
            let mut measurer = self.text_measurer.borrow_mut();
            let measurer = measurer.get_or_insert_with(byard_core::text::TextMeasurer::new);
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
        }
        // RFC-0032 §R3 step 5. On the retained path this is the set of nodes
        // whose layout *inputs* moved; on a full rebuild every node is new, so
        // the atlas reports the whole tree and the frame is dirty everywhere,
        // which is the truth about a rebuilt frame.
        self.atlas.populate_frame_dirty(frame, retained_used);

        // RFC-0038: rects are final for this frame, so an element that asked
        // for its own is told now, before anything paints and while nothing can
        // still move it (a post-layout resolve reads finished rects and never
        // feeds the same frame's layout). Elements whose rect did not change are not
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

        // RFC-0036 `width: match(ref)`: the anchors' rects are facts now, so a
        // panel that asked to be as wide as one can be given that width and
        // laid out again. Nothing here can move an anchor, so the second pass
        // settles in one go rather than iterating.
        self.apply_match_widths(&overlay_layouts, width, height);

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
        self.access_end(frame);

        self.settle_text_editing();

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
            // anywhere, which is exactly the silent failure the engine forbids.
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
            RenderNode::Text {
                attrs,
                content,
                state_blocks,
            } => {
                // RFC-0016: a responsive `size:` changes what is measured, so
                // it has to be seen here as well as at paint.
                let attrs = resolve_state_attrs(
                    attrs,
                    state_blocks,
                    crate::interp::events::StyleState::empty(),
                    &|c| viewport_holds(c, self.viewport, &self.theme),
                );
                let attrs: &[Attr] = &attrs;
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
                let family = self.resolve_family(attrs);
                let (nat_w, nat_h) =
                    self.measure_text_wrapped(&text, font_size, None, weight, family.as_deref());
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
                    family,
                    width: explicit_w,
                    fallback: (nat_w, nat_h),
                })?;
                flat_ids.push(id);
                Ok(id)
            }
            RenderNode::Box {
                name,
                attrs,
                state_blocks,
                children,
                env_snapshot,
                measure,
                ..
            } => {
                // RFC-0016 responsive variants reach layout here, one pass ahead
                // of paint and with no interaction state, so only viewport
                // blocks can apply. An interaction block's layout properties
                // still do not relayout (hover must not move its own box), and
                // a breakpoint's do, since a narrow window is exactly when a
                // padding or a direction should change. Borrowed unchanged when
                // the box has no blocks, which is nearly every box.
                let attrs = resolve_state_attrs(
                    attrs,
                    state_blocks,
                    crate::interp::events::StyleState::empty(),
                    &|c| viewport_holds(c, self.viewport, &self.theme),
                );
                let attrs: &[Attr] = &attrs;
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
        // Value widgets are leaf nodes with intrinsic default sizes.
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
                let grow = self.eval_float_prop(attrs, "grow").unwrap_or(0.0) as f32;
                let id = self.atlas.add_growing_leaf(LeafSize::new(w, h), grow)?;
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
                let grow = self.eval_float_prop(attrs, "grow").unwrap_or(0.0) as f32;
                let id = self.atlas.add_growing_leaf(LeafSize::new(w, h), grow)?;
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

    /// Renders one node, with its hit regions registered under its
    /// ancestors' paint transform (RFC-0011 hierarchical transforms).
    ///
    /// A wrapper rather than a line at the top of the body because the body has
    /// a dozen return paths, and a hit frame that leaked from one node into its
    /// next sibling would be the kind of bug that only shows under a rotated
    /// parent. Set on the way in and restored on the way out, unconditionally.
    ///
    /// The frame is the inherited transform **minus the scroll displacement**:
    /// scrolling already reaches every hit rect through `scroll_shift`, and
    /// applying it a second time through the transform would put a scrolled
    /// button's target one scroll offset away from the button.
    #[allow(clippy::too_many_arguments, clippy::similar_names)]
    fn render_node_with_atlas(
        &mut self,
        node: &RenderNode,
        atlas_node: byard_core::atlas::layout::AtlasNodeId,
        frame: &mut byard_core::frame::RenderFrame,
        flat_ids: &[byard_core::atlas::layout::AtlasNodeId],
        flat_idx: &mut usize,
        parent_rect: crate::interp::intrinsics::Rect,
        inherited_opacity: f32,
        inherited_transform: byard_core::frame::Transform,
        cull_clip: Option<byard_core::frame::Rect>,
        scroll_shift: (f32, f32),
        window: Option<WindowSpec>,
        pools: Pools<'_>,
    ) {
        let mut hit = inherited_transform;
        hit.translate[0] -= scroll_shift.0;
        hit.translate[1] -= scroll_shift.1;
        // Opacity decides nothing about where a press lands.
        hit.opacity = 1.0;
        let previous = self.router.set_hit_frame(hit);
        self.render_node_with_atlas_inner(
            node,
            atlas_node,
            frame,
            flat_ids,
            flat_idx,
            parent_rect,
            inherited_opacity,
            inherited_transform,
            cull_clip,
            scroll_shift,
            window,
            pools,
        );
        self.router.restore_hit_frame(previous);
    }

    /// The body of [`render_node_with_atlas`](Self::render_node_with_atlas),
    /// which owns the hit-frame bookkeeping around it.
    #[allow(clippy::similar_names)]
    #[allow(clippy::too_many_arguments)]
    fn render_node_with_atlas_inner(
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
        // transform; **hit-testing** cannot ride that path, RFC-0011
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
                    let attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
                    let attrs = attrs.as_ref();
                    let text = match self.binding_value(*content) {
                        Some(Value::Str(s)) => s,
                        other => other.map_or_else(String::new, |v| format!("{v:?}")),
                    };
                    // Fall back to theme on-surface color when unset.
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
                    let mut rgba = super::intrinsics::color_rgba_auto(color);
                    rgba[3] *= inherited_opacity;
                    // RFC-0011: the run's own transform, composed with its
                    // ancestors'. Scale and translate are baked into the baseline
                    // anchor and the font size (glyph extents scale from the
                    // anchor, so this scales the run about the pivot), which
                    // keeps it shaped at its drawn size and sharp. Rotation is
                    // not something glyphon can draw, so a rotated run of its
                    // own opens a rotated group: the frame stores the run
                    // upright and the composite turns it. Inside a rotated
                    // ancestor, that ancestor's group already turns it.
                    let own = self.resolve_transform(
                        attrs,
                        crate::interp::intrinsics::Rect::new(
                            rect.x,
                            rect.y,
                            rect.width,
                            rect.height,
                        ),
                    );
                    let transform = inherited_transform.compose(&own);
                    let own_group = own.rotate.abs() > f32::EPSILON
                        && frame.begin_group_rotated(
                            1.0,
                            transform.rotate,
                            [
                                transform.origin[0] + transform.translate[0],
                                transform.origin[1] + transform.translate[1],
                            ],
                        );
                    if self.access.on {
                        self.access_text(
                            frame,
                            attrs,
                            elem_idx,
                            &text,
                            crate::interp::intrinsics::Rect::new(
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ),
                            &transform,
                        );
                    }
                    let anchor = transform.apply_point([rect.x, rect.y]);
                    let scaled_size = size * transform.uniform_scale();
                    // RFC-0005 default text wrap: shape the run to the width layout
                    // resolved for this leaf (its parent-offered width), scaled by
                    // any ancestor scale (the run's glyphs scale about the pivot).
                    // `wrap: false` opts out to a single-line run. This mirrors the
                    // atlas's measure pass, so the rendered line breaks match the
                    // laid-out height.
                    let weight = self.resolve_weight(attrs);
                    let family = self.resolve_family(attrs);
                    let wrap_w = if self.eval_bool_prop(attrs, "wrap") == Some(false) {
                        None
                    } else {
                        Some(rect.width * transform.uniform_scale())
                    };
                    frame.push_text_wrapped(
                        byard_core::TextLine {
                            x: anchor[0],
                            y: anchor[1],
                            text,
                            font_size: scaled_size,
                            weight,
                            family,
                            color: rgba,
                            dirty: true,
                        },
                        wrap_w,
                    );
                    if own_group {
                        frame.end_group();
                    }

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
                clip_path,
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
                // Whether this box opened an opacity group (RFC-0011 T4), so the
                // group is closed on the way out of exactly the box that opened it.
                let mut grouped = false;
                // The accessibility scope this box opened (RFC-0042), closed on
                // the way out with the node it described.
                let mut access_scope = access::Scope::default();
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
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, paint_state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
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
                    // RFC-0011 T4: a translucent box with children is faded as
                    // one picture rather than by multiplying its alpha into
                    // every primitive it contains. The difference is overlap:
                    // its text over its own background, two children crossing,
                    // each darkening the other where they meet. Inside the
                    // group everything draws opaque and the composite applies
                    // the alpha once. A leaf has nothing to overlap, and a box
                    // inside an open group falls back to per-instance, so both
                    // keep the path they always had.
                    //
                    // RFC-0011 text transforms: a rotated box with children is
                    // grouped too. Its subtree is stored upright and the
                    // composite turns the picture about the transform's pivot,
                    // which is how its text, which glyphon cannot rotate, turns
                    // with it. Scale and translate stay per primitive, where
                    // text is already shaped at its scaled size, so a rotated
                    // label is as sharp as an upright one of the same size.
                    let rotated = transform.rotate.abs() > f32::EPSILON;
                    let pivot = [
                        transform.origin[0] + transform.translate[0],
                        transform.origin[1] + transform.translate[1],
                    ];
                    grouped = ((opacity - 1.0).abs() > f32::EPSILON || rotated)
                        && !children.is_empty()
                        && frame.begin_group_rotated(
                            opacity,
                            if rotated { transform.rotate } else { 0.0 },
                            pivot,
                        );
                    let opacity = if grouped { 1.0 } else { opacity };
                    child_opacity = opacity;
                    // An 8-digit `bg` carries its own alpha byte (RFC-0005 §1).
                    let bg_rgba = bg.map_or([0.0; 4], super::intrinsics::color_rgba_auto);
                    // A background below full alpha is as translucent as a
                    // faded box: on the depth-writing SolidBox pass it would
                    // cull whatever later passes draw beneath it.
                    let translucent =
                        (opacity - 1.0).abs() > f32::EPSILON || (bg.is_some() && bg_rgba[3] < 1.0);
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
                            color: bg_rgba,
                            radii,
                            transform,
                            smooth,
                        };
                        let border_rgba =
                            border_color.map_or([0.0; 4], super::intrinsics::color_rgba_auto);
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

                    // ── Widget-specific visual lowering & handler registration ──
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

                    // ── `focused:` reflected prop → register as focusable ──
                    // TextField, Checkbox, and RadioButton register their own
                    // focusable inside their render fns (they are focusable *by
                    // default*, RFC-0018), so exclude them here to avoid
                    // double-registration.
                    if !matches!(element_name, "TextField" | "Checkbox" | "RadioButton") {
                        self.register_focusable(attrs, hit_rect, elem_idx);
                    }

                    // RFC-0042: describe the box now that its handlers and
                    // focus target are registered, which is what its actions
                    // and state are read from.
                    if self.access.on {
                        let tapped = elem_idx.is_some_and(|i| {
                            self.router
                                .has_handler(i, crate::interp::events::EventKind::Tap)
                        });
                        access_scope = self.access_enter_box(
                            frame,
                            &access::BoxFacts {
                                name: element_name,
                                attrs,
                                paint_attrs,
                                bound_sig: *bound_sig,
                                tapped,
                                elem: elem_idx,
                                rect: current_rect,
                                atlas_node,
                                transform,
                            },
                        );
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
                // Clips are cut where their content is stored: inside a rotated
                // group that is upright, and the composite turns clip and content
                // together (RFC-0011 text transforms).
                let clip_transform = frame.upright(inherited_transform);
                let mask_clip = if name.as_str() == "Clip" && clip_transform.rotate != 0.0 {
                    // RFC-0011: under a rotated ancestor the clip's outline is
                    // a rotated rectangle, which neither the scissor nor an
                    // axis-aligned clip entry can express. It becomes a path
                    // mask instead, so the content is cut along the edges it
                    // is drawn with. Its own `path` child, if it has one, nests
                    // inside that as it would anywhere else.
                    let mut radii = self.resolve_radii(attrs, "rrect");
                    for r in &mut radii {
                        *r *= clip_transform.scale[0];
                    }
                    let outline = transformed_rrect_outline(current_rect, radii, &clip_transform);
                    let opened = self.begin_clip_commands(&outline, frame);
                    let pathed = clip_path.as_ref().is_some_and(|p| {
                        let tl = clip_transform.apply_point([current_rect.x, current_rect.y]);
                        let rect = byard_core::frame::Rect::new(
                            tl[0],
                            tl[1],
                            current_rect.w * clip_transform.scale[0],
                            current_rect.h * clip_transform.scale[1],
                        );
                        self.begin_clip_path_mask(p, rect, frame)
                    });
                    match (opened, pathed) {
                        (true, pathed) => Some(pathed),
                        (false, true) => Some(false),
                        (false, false) => None,
                    }
                } else if name.as_str() == "Clip" {
                    let tl = clip_transform.apply_point([current_rect.x, current_rect.y]);
                    let rect = byard_core::frame::Rect::new(
                        tl[0],
                        tl[1],
                        current_rect.w * clip_transform.scale[0],
                        current_rect.h * clip_transform.scale[1],
                    );
                    // Read the same way a box's `radius` is, so `rrect: 16`
                    // and `rrect: (16, 0, 0, 16)` both mean here what they
                    // already mean there. The radius scales with the subtree,
                    // as a decorated box's corners do: a scaled card's clip
                    // has to keep matching the card.
                    let mut radii = self.resolve_radii(attrs, "rrect");
                    for r in &mut radii {
                        *r *= clip_transform.scale[0];
                    }
                    frame.begin_clip_rounded(rect, radii);
                    // RFC-0037 `clip(path)`: the mask opens *inside* the
                    // rounded clip rather than replacing it, so the two
                    // coverages multiply and `rrect` still means what it says
                    // on a clip that also has a path. Two entries, closed in
                    // reverse below.
                    let pathed = clip_path
                        .as_ref()
                        .is_some_and(|p| self.begin_clip_path_mask(p, rect, frame));
                    Some(pathed)
                } else {
                    None
                };
                let scroll_clip = if name.as_str() == "ScrollView" {
                    let (ox, oy) = self.resolve_axis_pair(attrs, "offset", (0.0, 0.0));
                    let tl = clip_transform.apply_point([current_rect.x, current_rect.y]);
                    let clip = byard_core::frame::Rect::new(
                        tl[0],
                        tl[1],
                        current_rect.w * clip_transform.scale[0],
                        current_rect.h * clip_transform.scale[1],
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
                            // RFC-0042: its words still name the button it is in.
                            this.access_culled(child, pools);
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
                if let Some(pathed) = mask_clip {
                    if pathed {
                        frame.end_clip();
                    }
                    frame.end_clip();
                }
                // After the clips it contains, so the group's picture includes
                // everything its box drew, clipped the way it was drawn.
                if grouped {
                    frame.end_group();
                }
                self.access_leave(frame, access_scope);
                // Close the RFC-0019 instance-env scope opened at the top of this
                // arm (balanced with `env_base`), restoring the caller's env for
                // the remaining siblings.
                self.env.truncate(env_base);
            }
            // RFC-0026: a navigation container. Its own surface is an ordinary
            // background fill; the interesting part is the per-screen transform
            //, the transition's whole cost is two `f32` offsets and an alpha,
            // composed into the transform every subtree already inherits, so a
            // screen sliding in costs no relayout and no extra pass (a
            // paint-class change never relayouts).
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
                let paint_attrs = resolve_state_attrs(&p.attrs, &p.state_blocks, state, &|c| {
                    viewport_holds(c, self.viewport, &self.theme)
                });
                let paint_attrs = paint_attrs.as_ref();
                let own_transform = self.resolve_transform(paint_attrs, nav_rect);
                let transform = inherited_transform.compose(&own_transform);
                let opacity = inherited_opacity
                    * self
                        .eval_float_prop(paint_attrs, "opacity")
                        .map_or(1.0, |v| v as f32);
                if let Some(bg) = self.eval_color_prop(paint_attrs, "bg") {
                    push_fill(
                        frame,
                        byard_core::BoxInstance {
                            rect: [rect.x, rect.y, rect.width, rect.height],
                            color: dim_alpha(super::intrinsics::color_rgba_auto(bg), opacity),
                            radii: self.resolve_radii(paint_attrs, "radius"),
                            transform,
                            smooth: self.resolve_smooth(paint_attrs),
                        },
                    );
                }
                // `route_change` and any pointer handlers on the container.
                let hit_rect = scrolled_hit_rect(nav_rect, scroll_shift, cull_clip);
                self.register_event_attrs(&p.attrs, hit_rect, elem_idx);
                // RFC-0042: tabs are a tab list; a stack's screens have no
                // role of their own and their content is simply the tree's.
                let host = p.kind == NavKind::Host;
                let nav_scope = if host && self.access.on {
                    self.access_enter_tabs(frame, &p.attrs, elem_idx, nav_rect, &transform)
                } else {
                    access::Scope::default()
                };

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
                    // Hit rects ride their own channel (RFC-0011 keeps
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
                    let tab_scope = if host && self.access.on {
                        self.access_enter_tab(
                            frame,
                            &p.entries[screen.entry].path,
                            screen.entry == p.current,
                            self.atlas.node_index(screen_id),
                            screen_rect,
                            &screen_transform,
                        )
                    } else {
                        access::Scope::default()
                    };
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
                    self.access_leave(frame, tab_scope);
                }
                frame.end_clip();
                self.access_leave(frame, nav_scope);
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
                    let attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
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
                    if self.access.on {
                        let elem = self.atlas.node_index(atlas_node);
                        self.access_image(
                            frame,
                            attrs,
                            elem,
                            crate::interp::intrinsics::Rect::new(
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ),
                            &inherited_transform,
                            false,
                        );
                    }
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
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
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
                        drawn_at: self.router.handler_count(),
                    });
                    if self.access.on {
                        self.access_native(
                            frame,
                            name.as_str(),
                            attrs,
                            elem_idx,
                            view_rect,
                            &inherited_transform,
                        );
                    }
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
                    if self.access.on {
                        let elem = self.atlas.node_index(atlas_node);
                        self.access_image(
                            frame,
                            attrs,
                            elem,
                            crate::interp::intrinsics::Rect::new(
                                rect.x,
                                rect.y,
                                rect.width,
                                rect.height,
                            ),
                            &inherited_transform,
                            false,
                        );
                    }
                    let handle = self
                        .binding_value(*src)
                        .and_then(|v| if let Value::Str(s) = v { Some(s) } else { None })
                        .unwrap_or_default();
                    let base_rgb = self
                        .eval_color_prop(attrs, "color")
                        .map_or([1.0, 1.0, 1.0, 1.0], super::intrinsics::color_rgba_auto);
                    let opacity = inherited_opacity
                        * self
                            .eval_float_prop(attrs, "opacity")
                            .map_or(1.0, |v| v as f32);

                    // Cache hit: a resident glyph, tinted and opacity-applied.
                    // Cache miss: a zero-opacity placeholder so the frame ships
                    // without stalling (the render thread never blocks on
                    // generation); the dispatch itself happened
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
                    let paint_attrs = resolve_state_attrs(attrs, state_blocks, state, &|c| {
                        viewport_holds(c, self.viewport, &self.theme)
                    });
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

                    if self.access.on {
                        self.access_image(
                            frame,
                            attrs,
                            elem_idx,
                            canvas_rect,
                            &inherited_transform,
                            true,
                        );
                    }

                    // Background fill: a plain solid behind every shape.
                    if let Some(bg) = self.eval_color_prop(paint_attrs, "bg") {
                        push_fill(
                            frame,
                            byard_core::BoxInstance {
                                rect: [rect.x, rect.y, rect.width, rect.height],
                                color: dim_alpha(super::intrinsics::color_rgba_auto(bg), opacity),
                                radii: [0.0; 4],
                                transform: inherited_transform,
                                smooth: 0.0,
                            },
                        );
                    }

                    // Shape commands, in declaration order (painter's order,
                    // each `push_canvas_shape` advances the global emission
                    // depth, RFC-0011). A `Canvas` that declares a combine mode
                    // (RFC-0031 §S4) collects them into one group instead, and
                    // pushes its head once at the end.
                    let combine = self.resolve_group_mode(paint_attrs);
                    let mut sink = combine.map(|(mode, _)| ShapeGroupSink {
                        morph_paths: (mode == byard_core::frame::GROUP_MORPH).then(Vec::new),
                        ..ShapeGroupSink::default()
                    });
                    self.emit_canvas_items(
                        shapes,
                        canvas_rect,
                        opacity,
                        inherited_transform,
                        sink.as_mut(),
                        frame,
                    );
                    if let (Some((mode, param)), Some(mut sink)) = (combine, sink) {
                        if let Some(paths) = sink.morph_paths.take() {
                            let resample = paint_attrs
                                .iter()
                                .find(|a| a.name.as_str() == "morph_mode")
                                .and_then(|a| match &a.kind {
                                    AttrKind::Prop { value } => Self::enum_token(value),
                                    _ => None,
                                })
                                == Some("resample");
                            self.push_morph_paths(
                                &paths,
                                param,
                                resample,
                                canvas_rect,
                                opacity,
                                inherited_transform,
                                frame,
                            );
                        }
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

    // ── Widget rendering helpers ─────────────────────────────

    /// Renders a `Toggle` widget: track + thumb, and registers a Tap
    /// handler to flip the bound bool.
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
        // The accent reads as opaque on purpose, here and on every control
        // that owns its visuals: the layered pieces (thumb over track, dot
        // under ring, disc under disc) are composed for an opaque accent and
        // sit on the SolidBox pass. Fading a control is `opacity:`'s job.
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

        // Tap handler to flip the bool.
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
        // A border is a plain stroke on the blended pass, so its alpha byte
        // counts, as on any `Box`.
        let border_rgba = border.map_or([0.0; 4], |c| {
            dim_alpha(super::intrinsics::color_rgba_auto(c), opacity)
        });
        // Opaque accent, as for `Toggle`.
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
        // Opaque accent, as for `Toggle`.
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

    /// Renders a `Slider` widget: track + fill + thumb, and registers
    /// PointerDown + PointerDrag handlers to write the value.
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
        // Opaque accent, as for `Toggle`.
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

        // Handlers: PointerDown + PointerDrag.
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
            // Change write-back (RFC-0003 E1), as the other value widgets
            // have: the way a value set from outside the pointer, an
            // assistive technology (RFC-0042), reaches the `var`.
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );
        }
    }

    /// Renders a `TextField` widget: background box + text/placeholder, and,
    /// while it has focus, the selection and the caret (RFC-0041 §6). Registers
    /// the handlers that edit it.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
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
        use super::events::EventKind as Kind;
        let placeholder = self.eval_str_prop(attrs, "placeholder").unwrap_or_default();
        let cur_text = bound_sig
            .map(|s| match self.ctx.peek_signal(s) {
                Value::Str(t) => t,
                _ => String::new(),
            })
            .unwrap_or_default();

        let is_focused = elem_idx.is_some_and(|i| self.router.is_focused(i));
        // RFC-0041: the field being edited, if it is the focused one and is
        // bound to a `var` there is something to edit in.
        let editing = elem_idx.filter(|_| is_focused && bound_sig.is_some());
        // RFC-0040: the preedit this field owns, drawn at the caret and never
        // part of the value.
        let preedit = self
            .composition
            .as_ref()
            .filter(|c| is_focused && elem_idx == Some(c.owner) && !c.text.is_empty())
            .map(|c| (c.text.clone(), c.cursor));

        let (display_text, is_placeholder) = if cur_text.is_empty() && preedit.is_none() {
            (placeholder, true)
        } else {
            (cur_text, false)
        };
        // What the caret and the selection index into: the value, which is
        // empty while a placeholder is showing in its place.
        let value: &str = if is_placeholder { "" } else { &display_text };

        // The field's own `color:` for what is typed, and the same colour
        // fainter for the placeholder. Unwritten, the field keeps the white
        // on grey it always had.
        let written = self
            .eval_color_prop(attrs, "color")
            .map(super::intrinsics::color_rgba_auto);
        let value_rgba =
            written.unwrap_or_else(|| super::intrinsics::color_to_rgba(0x00ff_ffff, false));
        let text_rgba = match (is_placeholder, written) {
            (false, _) => value_rgba,
            (true, Some(c)) => [c[0], c[1], c[2], c[3] * PLACEHOLDER_ALPHA],
            (true, None) => super::intrinsics::color_to_rgba(0x0088_8888, false),
        };
        // `size:` first, then the `typo:` token's size, as a `Text` does.
        let typo_size = self.eval_typo_size(attrs);
        let font_size = self
            .eval_int_prop(attrs, "size")
            .or(typo_size)
            .unwrap_or(16) as f32;

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

        // The text starts at the field's left padding when it writes one.
        let pads = attrs.iter().any(|a| {
            matches!(
                a.name.as_str(),
                "p" | "padding" | "pl" | "padding_left" | "padding-left"
            )
        });
        let pad_x = if pads {
            self.eval_container_style("Box", attrs).padding.left
        } else {
            FIELD_PAD_X
        };
        let text_x = rect.x + pad_x;
        let text_y = rect.y + (rect.h - font_size) / 2.0;
        // The content box the text lives in; a longer text scrolls inside it.
        let view_w = (rect.w - pad_x * 2.0).max(1.0);
        // NOTE: `TextLine` carries no `Transform` field (RFC-0011 engine slice:
        // only box primitives were given one), so the field's *text* does not
        // follow `translate`/`scale`/`rotate`, the box visuals below (underline,
        // caret) and its `bg` fill do. Same limitation as the `Text` intrinsic.
        // The field's own type properties, which it accepts from the
        // intrinsic catalogue and, until now, ignored: a field set in a
        // declared family rendered in the system font, which is the shape of
        // defect this whole change exists to remove.
        let weight = self.resolve_weight(attrs);
        let family = self.resolve_family(attrs);
        let font = super::edit::FieldFont {
            size: font_size,
            weight,
            family: family.clone(),
        };

        // RFC-0041: where the caret and the selection are, in pixels from the
        // start of the text, and how far the text has scrolled to keep the
        // caret in view. Measured through the shared measurer, whose cache
        // makes a prefix that was measured last frame free this frame.
        let mut caret_rel = 0.0_f32;
        let mut value_w = 0.0_f32;
        let mut selection: Option<(f32, f32)> = None;
        let mut scroll = 0.0_f32;
        // The preedit's width, and the IME's cursor inside it, as x ranges.
        let mut preedit_w = 0.0_f32;
        let mut preedit_cursor: Option<(f32, f32)> = None;
        if let Some(idx) = editing {
            let store = std::rc::Rc::clone(&self.edit);
            let seq = self.frame_seq;
            let mut measure = |t: &str| self.measure_in(t, &font);
            if let Some((text, cursor)) = &preedit {
                preedit_w = measure(text);
                preedit_cursor = cursor.map(|(b, e)| {
                    let at = |upto: usize| measure(&text[..floor_char_boundary(text, upto)]);
                    (at(b), at(e))
                });
            }
            let mut store = store.borrow_mut();
            let state = store.state(idx);
            state.sync(value);
            state.seen = seq;
            caret_rel = state.x_of(value, &font, state.caret, &mut measure);
            value_w = state.x_of(value, &font, value.len(), &mut measure);
            if state.has_selection() && preedit.is_none() {
                let (lo, hi) = state.range();
                selection = Some((
                    state.x_of(value, &font, lo, &mut measure),
                    state.x_of(value, &font, hi, &mut measure),
                ));
            }
            let cursor_x = caret_rel + preedit_cursor.map_or(preedit_w, |(_, e)| e);
            state.follow_caret(cursor_x, value_w + preedit_w, view_w);
            scroll = state.scroll;
        } else if let Some(idx) = elem_idx {
            // A field that is not focused keeps its state while it is on
            // screen and loses it with its element (§1).
            let seq = self.frame_seq;
            let mut store = self.edit.borrow_mut();
            if !store.is_empty() {
                store.touch(idx, seq);
            }
        }
        let origin_x = text_x - scroll;
        // The text is clipped to the content box when it overflows it, which is
        // the only time it can reach outside.
        let clipped = editing.is_some() && value_w + preedit_w > view_w;
        if clipped {
            frame.begin_clip(byard_core::frame::Rect::new(text_x, rect.y, view_w, rect.h));
        }

        // The selection, behind the text: the theme's `primary` at 30 %, or the
        // field's own `selection:` colour.
        if let Some((x0, x1)) = selection.filter(|(a, b)| b > a) {
            let color = self.eval_color_prop(attrs, "selection").map_or_else(
                || {
                    let c = super::intrinsics::color_to_rgba(self.theme.primary(), false);
                    [c[0], c[1], c[2], c[3] * SELECTION_ALPHA]
                },
                super::intrinsics::color_rgba_auto,
            );
            push_fill(
                frame,
                byard_core::BoxInstance {
                    rect: [origin_x + x0, text_y - 1.0, x1 - x0, font_size + 2.0],
                    color: dim_alpha(color, opacity),
                    radii: [0.0; 4],
                    transform,
                    smooth: 0.0,
                },
            );
        }

        // The value's text. A preedit is drawn at the caret, so the part of the
        // value after the caret moves out of its way (RFC-0040 §1).
        let caret_at = editing
            .and_then(|_| self.edit.borrow().get(elem_idx?).map(|s| s.caret))
            .unwrap_or(value.len());
        let split = preedit.is_some() && caret_at < value.len();
        let push_value = |frame: &mut byard_core::frame::RenderFrame, x: f32, text: &str| {
            if !text.is_empty() {
                frame.push_text(byard_core::TextLine {
                    weight,
                    family: family.clone(),
                    x,
                    y: text_y,
                    text: text.to_string(),
                    font_size,
                    color: dim_alpha(text_rgba, opacity),
                    dirty: true,
                });
            }
        };
        if split {
            let (head, tail) = value.split_at(caret_at);
            push_value(frame, origin_x, head);
            push_value(frame, origin_x + caret_rel + preedit_w, tail);
        } else {
            push_value(frame, origin_x, &display_text);
        }

        // The caret, in the field's own text colour: at the caret's place in
        // the value, or at the IME's cursor inside the preedit while composing
        // (RFC-0040).
        if is_focused {
            let mut caret_x = origin_x + caret_rel + CARET_DX;
            if let Some((text, _)) = &preedit {
                let color = dim_alpha(text_rgba, opacity);
                let start = origin_x + caret_rel;
                frame.push_text(byard_core::TextLine {
                    weight,
                    family: family.clone(),
                    x: start,
                    y: text_y,
                    text: text.clone(),
                    font_size,
                    color,
                    dirty: true,
                });
                // §5: the whole preedit thinly underlined, the IME's cursor
                // range, the active clause on IMEs that use it, thickly.
                let underline_y = text_y + font_size + 1.0;
                let mut bar = |x: f32, w: f32, h: f32| {
                    frame.push_instance(byard_core::BoxInstance {
                        rect: [x, underline_y, w, h],
                        color,
                        radii: [0.0; 4],
                        transform,
                        smooth: 0.0,
                    });
                };
                bar(start, preedit_w, 1.0);
                if let Some((b, e)) = preedit_cursor.filter(|(b, e)| e > b) {
                    bar(start + b, e - b, 2.0);
                }
                caret_x = start + preedit_cursor.map_or(preedit_w, |(_, e)| e) + CARET_DX;
            }
            // `None` from the IME hides its cursor; the caret rectangle is
            // still reported, since the candidate window needs a place.
            let caret_visible = preedit.as_ref().is_none_or(|(_, c)| c.is_some());
            // The blink (RFC-0041 §6): one timer wake per phase, started by the
            // first frame that draws the caret and stopped when it stops being
            // drawn.
            if let Some(idx) = editing {
                self.start_blink(idx);
            }
            if caret_visible && self.caret_shown() {
                frame.push_instance(byard_core::BoxInstance {
                    rect: [caret_x, text_y, 1.5, font_size],
                    color: dim_alpha(value_rgba, opacity),
                    radii: [0.0; 4],
                    transform,
                    smooth: 0.0,
                });
            }
            // RFC-0040 §4: where the platform puts its candidate window, in
            // viewport space, so through the field's transform.
            let a = transform.apply_point([caret_x, text_y]);
            let b = transform.apply_point([caret_x + 1.5, text_y + font_size]);
            frame.set_text_input(byard_core::frame::TextInputState {
                caret: byard_core::frame::Rect::new(
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    (b[0] - a[0]).abs(),
                    (b[1] - a[1]).abs(),
                ),
            });
        }
        if clipped {
            frame.end_clip();
        }

        // The field's own events. The router runs one handler per event kind
        // on the focused element, the last one registered, so the handlers
        // written on the field are registered first (pointer events, focus,
        // blur, key_up reach them as on any element) and the ones that share
        // a kind with the field's own editing are folded into it below.
        self.register_event_attrs(attrs, hit_rect, elem_idx);
        let on_edit = self.field_actions(attrs, &["input", "change"]);
        let on_typed = self.field_actions(attrs, &["text_input"]);
        let on_key = self.field_actions(attrs, &["key_down"]);
        let on_submit = self.field_actions(attrs, &["submit"]);

        // Handlers: typing and the keys edit the value at the caret, the
        // pointer places it and selects (RFC-0041).
        if let (Some(sig), Some(idx)) = (bound_sig, elem_idx) {
            let handle = self.field_handle(
                sig,
                idx,
                editing::FieldGeom {
                    font,
                    text_x,
                    view_w,
                    frame: self.router.current_frame(),
                },
            );
            // `input` and `change` see the value already written.
            let edited = self.field_actions(attrs, &["input", "change"]);
            let events = [
                (Kind::TextInput, handle.clone().text_input(on_typed, edited)),
                (
                    Kind::KeyDown,
                    handle.clone().key_down(on_key, on_edit, on_submit),
                ),
                (
                    Kind::PointerDown,
                    handle
                        .clone()
                        .pointer_down(self.field_actions(attrs, &["pointer_down"])),
                ),
                (Kind::PointerDrag, handle.clone().pointer_drag()),
                (
                    Kind::PointerUp,
                    handle
                        .clone()
                        .pointer_up(self.field_actions(attrs, &["pointer_up"])),
                ),
                (
                    Kind::DoubleTap,
                    handle.double_tap(self.field_actions(attrs, &["double_tap"])),
                ),
            ];
            for (kind, action) in events {
                self.router.on(idx, hit_rect, kind, action);
            }
            // A selection that starts in the field goes on past its edge.
            self.router.capture_drag(idx);

            // Change event: write-back from platform (E1).
            self.router.on(
                idx,
                hit_rect,
                super::events::EventKind::Change,
                super::events::write_back_action(sig),
            );

            // Register as focusable so Tab and click steal focus.
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
                    // possible way to learn about it.
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

    /// Applies this tick's IME events to the composition, and marks them, and
    /// any editing key the IME owns, as consumed (RFC-0040 §3, §6).
    ///
    /// - A preedit belongs to the focused field and replaces the last one; an
    ///   empty preedit clears the text but keeps the owner, because winit
    ///   sends one right before every commit.
    /// - A commit goes to the owner, which is not always the focused field,
    ///   through the same `TextInput` handler typing uses: committed text and
    ///   typed text are the same thing once they reach the field.
    /// - The IME turning off ends the composition, with nothing committed.
    /// - Backspace, Delete, Enter and the keys that move the caret are the
    ///   IME's while a preedit is showing, so they never also edit the
    ///   committed value or move the caret out from under the preedit.
    fn apply_composition(&mut self, events: &[byard_core::InputEvent], consumed: &mut [bool]) {
        use byard_core::platform::{EventKind as CoreKind, InputPayload};
        for (i, ev) in events.iter().enumerate() {
            match ev.kind {
                CoreKind::Composition => {
                    consumed[i] = true;
                    let Some(InputPayload::Preedit { text, cursor }) = &ev.payload else {
                        continue;
                    };
                    if text.is_empty() {
                        if let Some(c) = self.composition.as_mut() {
                            c.text.clear();
                            c.cursor = None;
                        }
                    } else if let Some(owner) = self.router.focused() {
                        self.composition = Some(Composition {
                            owner,
                            text: text.clone(),
                            cursor: *cursor,
                        });
                    }
                }
                CoreKind::CompositionCommit => {
                    consumed[i] = true;
                    let target = self
                        .composition
                        .take()
                        .map(|c| c.owner)
                        .or_else(|| self.router.focused());
                    if let (Some(target), Some(InputPayload::Key(text))) = (target, &ev.payload) {
                        // No key is down for a commit: it must not inherit the
                        // modifiers of the last one (RFC-0041 §1).
                        Self::clear_event_context(ev.time_ms);
                        self.router.fire_event(
                            &mut self.ctx,
                            target,
                            super::events::EventKind::TextInput,
                            Some(&Value::Str(text.clone())),
                        );
                    }
                }
                CoreKind::CompositionEnd => {
                    consumed[i] = true;
                    self.composition = None;
                }
                CoreKind::KeyDown | CoreKind::KeyUp if self.composing() => {
                    if let Some(InputPayload::Key(key)) = &ev.payload {
                        if matches!(
                            key.as_str(),
                            "Backspace"
                                | "Delete"
                                | "Enter"
                                | "ArrowLeft"
                                | "ArrowRight"
                                | "Home"
                                | "End"
                        ) {
                            consumed[i] = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether a preedit is showing in the focused field.
    fn composing(&self) -> bool {
        self.composition
            .as_ref()
            .is_some_and(|c| !c.text.is_empty() && self.router.is_focused(c.owner))
    }

    /// Discards the preedit of a field that has lost focus (RFC-0040 §6).
    ///
    /// The engine never commits it: it does not know the IME's conversion
    /// state, and a platform that commits on blur would then insert the text
    /// twice. The owner is kept so that platform's commit still lands in the
    /// field the text was typed into.
    fn drop_unfocused_preedit(&mut self) {
        if let Some(c) = self.composition.as_mut() {
            if !self.router.is_focused(c.owner) {
                c.text.clear();
                c.cursor = None;
            }
        }
    }

    /// Offers each event to the native views under the pointer, innermost
    /// first, and reports which events a view took (RFC-0039 × RFC-0003).
    ///
    /// An element drawn over a view that takes presses where the pointer is
    /// (a button over a map) gets the event instead: the view is not offered
    /// it at all, as an intrinsic underneath would not be.
    ///
    /// Innermost first is "last recorded first": the render walk is
    /// depth-first, so a view nested inside another was pushed after it, and
    /// walking the targets in reverse is the same front-to-back order the
    /// router's own hit testing uses.
    ///
    /// A keyboard, text or change event has no position (hosts send it at the
    /// origin), so it is offered to no view here; a view that wants keys gets
    /// them the way an intrinsic does, through the focus path, and inventing a
    /// second rule for package elements is exactly the divergence this ABI
    /// exists to avoid.
    fn dispatch_to_native_views(&mut self, events: &[byard_core::InputEvent]) -> Vec<bool> {
        use byard_core::render::{Event as ViewEvent, Handled, Layout};

        let mut consumed = vec![false; events.len()];
        if self.native_targets.is_empty() {
            return consumed;
        }
        let targets = self.native_targets.clone();
        for (i, ev) in events.iter().enumerate() {
            if !ev.kind.is_positional() {
                continue;
            }
            let (px, py) = ev.pos;
            for target in targets.iter().rev() {
                let r = target.rect;
                if px < r.x || px >= r.x + r.w || py < r.y || py >= r.y + r.h {
                    continue;
                }
                // Something drawn over the view that takes presses here gets
                // this one: what is on top is what the pointer is on.
                if self.router.claims_pointer_from((px, py), target.drawn_at) {
                    break;
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
    /// (an extension never keeps a resource alive past its owning scope).
    fn mount_native_view(&mut self, name: &str, span: crate::diagnostics::Span) -> usize {
        let slot = self.native_views.len();
        if let Some(view) = byard_core::render::registry::create(name) {
            self.native_views.push(view);
        } else {
            // The catalog answered for this name a moment ago, so failing here
            // means the registry changed underneath the lowering, which is not
            // something an app can do by accident. Said with a span rather
            // than papered over with a blank element (no silent failures).
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
            // data-only by construction (only `Send` data crosses the boundary,
            // never a signal, callback or view handle): the same rule the controller
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

    /// The actions written on a field for the events in `names`, lowered.
    /// The handler's own payload name (`input(text) => …`) is honoured.
    fn field_actions(&mut self, attrs: &[Attr], names: &[&str]) -> Vec<super::events::Action> {
        let mut out = Vec::new();
        for attr in attrs {
            if let AttrKind::Event { payload, action } = &attr.kind {
                if names.contains(&attr.name.as_str()) {
                    if let Ok(lowered) = self.lower_action(action, payload.clone()) {
                        out.push(lowered);
                    }
                }
            }
        }
        out
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
    /// **or** a `focus =>`/`blur =>` handler (RFC-0012 S2), the
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

    /// Processes a whole `View`: its declarations first (so bindings can resolve
    /// names), then lowers its top-level elements into a render tree, handling
    /// `when`/`for` structural members.
    pub fn lower_view(&mut self, view: &ViewDecl, known_views: &[&str]) -> Vec<RenderNode> {
        // RFC-0032 §R4: another view is another tree, and last frame's build
        // order says nothing about it. The same view lowered again (the HUD's
        // periodic refresh) keeps its eligibility: its structure is compared
        // as it always was. A hot reload of the same view invalidates in
        // `reload`, which knows the AST changed.
        let identity = (view.name.clone(), view.span);
        if self.lowered_view.as_ref() != Some(&identity) {
            self.invalidate_retained_layout();
            self.lowered_view = Some(identity);
        }
        self.check_anchor_refs(view);
        self.check_font_families(view);
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
        let mut consumed = self.dispatch_to_native_views(events);
        // RFC-0040: composition is engine state, applied here rather than
        // routed to an app handler, and never offered to anything else.
        self.apply_composition(events, &mut consumed);
        // The system's appearance is the theme's to follow, when it was told
        // to (`scheme = "system"`), and nothing for any element to route.
        // An assistive technology's requests to move focus or to scroll
        // (RFC-0042 §5) are engine state in the same way: applied here,
        // never offered to a handler.
        for (i, ev) in events.iter().enumerate() {
            match ev.kind {
                CoreKind::WindowFocus => {
                    consumed[i] = true;
                    if let Some(InputPayload::Bool(focused)) = ev.payload.as_ref() {
                        self.caret.window_blurred = !*focused;
                    }
                }
                CoreKind::SystemScheme => {
                    consumed[i] = true;
                    if let (true, Some(InputPayload::Bool(dark))) =
                        (self.theme.follows_system, ev.payload.as_ref())
                    {
                        if self.theme_is_dark() != *dark {
                            self.set_theme_dark(*dark);
                        }
                    }
                }
                CoreKind::AccessFocus => {
                    consumed[i] = true;
                    if let Some(InputPayload::Element(elem)) = ev.payload {
                        self.router.focus_element(&mut self.ctx, elem);
                    }
                }
                CoreKind::AccessScrollIntoView => {
                    consumed[i] = true;
                    self.scroll_into_view(ev.pos, ev.delta);
                }
                _ => {}
            }
        }

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
                    // Consumed by `apply_composition` above, never routed.
                    CoreKind::Composition
                    | CoreKind::CompositionCommit
                    | CoreKind::CompositionEnd
                    | CoreKind::SystemScheme
                    | CoreKind::WindowFocus
                    | CoreKind::AccessFocus
                    | CoreKind::AccessScrollIntoView => {
                        unreachable!(
                            "composition, window focus, scheme and accessibility events are consumed before routing"
                        )
                    }
                };
                let value = ev.payload.as_ref().map(|p| match p {
                    InputPayload::Str(s) => Value::Str(s.clone()),
                    InputPayload::Bool(b) => Value::Bool(*b),
                    InputPayload::Float(f) => Value::Float(f64::from(*f)),
                    InputPayload::Number(n) => Value::Float(*n),
                    InputPayload::Element(e) => Value::Int(i64::from(*e)),
                    InputPayload::Key(k) => Value::Str(k.clone()),
                    // Only a `Composition` carries one, and those are consumed.
                    InputPayload::Preedit { text, .. } => Value::Str(text.clone()),
                });
                CompEvent {
                    kind,
                    pos: ev.pos,
                    delta: ev.delta,
                    value,
                    time_ms: ev.time_ms,
                    mods: ev.mods,
                }
            })
            .collect();

        // RFC-0005 `ScrollView` wheel: a wheel/scroll over a recorded scroll
        // target nudges whichever of `offset.x`/`offset.y` is writable, each
        // clamped to `[0, content − viewport]`. Wheel deltas are line-based (× a
        // per-line step); trackpad `Scroll` deltas are already pixels. Done here,
        // before the render, so the same tick paints the new offset (paint-time
        // translate, no relayout).
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
        // press-relative travel, no accumulated drift; release ends it.
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
        // RFC-0040 §6: routing may have moved focus away from a composition.
        self.drop_unfocused_preedit();
        // RFC-0041 §6: and from the field whose caret was blinking, or an edit
        // may have started the blink over.
        self.settle_blink();
    }
}

fn dim_alpha(mut color: [f32; 4], opacity: f32) -> [f32; 4] {
    color[3] *= opacity;
    color
}

/// Pushes a plain fill onto the pass its alpha belongs to: an opaque one onto
/// `SolidBox`, which writes depth and occludes, and a translucent one onto the
/// blended decorated pass, which only tests it. A translucent fill on the solid
/// pass would stamp a nearer depth over its whole rect and cull whatever later
/// passes draw beneath it.
fn push_fill(frame: &mut byard_core::frame::RenderFrame, fill: byard_core::BoxInstance) {
    if fill.color[3] < 1.0 {
        frame.push_decorated(byard_core::frame::DecoratedBox {
            base: fill,
            opacity: 1.0,
            dirty: true,
            ..Default::default()
        });
    } else {
        frame.push_instance(fill);
    }
}

/// The largest char boundary in `s` at or before `i`, so an IME's byte
/// offset that lands inside a character (or past the end) still slices.
fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests;
