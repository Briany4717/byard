//! The typed, fully-owned AST (RFC-0002 §"Data structures"; RFC-0003 attrs/`Fn`).
//!
//! Every node owns all of its data, no borrows into the source text (INV-3),
//! so hot-reload can re-parse and structurally diff a new tree against the
//! running one without lifetime entanglement, and a `CompiledView` carrying
//! this AST is `Send` (INV-6) for the file-watcher → logic-thread channel.
//!
//! The AST is **immutable after parse**: reactivity/`is_reactive` metadata lives
//! in side-tables (RFC-0002 D3, RFC-0004 §10), never on these nodes.

use crate::diagnostics::Span;
use crate::symbol::Symbol;

/// A type annotation (`type := IDENT ("<" type ("," type)* ">")?`), extended
/// with the function type `Fn(...)` for callback props (RFC-0003 E2).
#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// A named (optionally generic) type, e.g. `Str`, `Int`, `List<Str>`.
    Named {
        /// The type's name.
        name: Symbol,
        /// Generic arguments, if any (`List<Str>` ⇒ `[Str]`).
        args: Vec<Type>,
        /// Source span.
        span: Span,
    },
    /// A function type `Fn(P0, P1, ...) -> R` (RFC-0003 E2). `ret` is `None`
    /// for a callback with no declared return.
    Function {
        /// Parameter types.
        params: Vec<Type>,
        /// Optional return type.
        ret: Option<Box<Type>>,
        /// Source span.
        span: Span,
    },
}

impl Type {
    /// The source span of this type.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Named { span, .. } | Self::Function { span, .. } => *span,
        }
    }
}

/// A `View`/`fn` parameter (`param := IDENT (":" type)?`). The annotation is
/// optional in the AST; D9's "annotation required here" rule is enforced by the
/// checker (M5), not the parser.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// Parameter name.
    pub name: Symbol,
    /// Declared type, if written.
    pub ty: Option<Type>,
    /// Default value expression (`= expr`), if written (RFC-0007 D-B). A
    /// defaulted parameter omitted at a user-view call site evaluates this in
    /// the callee scope.
    pub default: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A top-of-file package import (RFC-0008 Pillar A/B, decisions D-F/D-G).
///
/// Three surface forms, all resolved against the manifest-declared dependency
/// set (never a path string, the two-layer rule, RFC-0001 §1):
///
/// - `use material`, qualified access as `material.Card`;
/// - `use material as m`, qualified access under an explicit alias, `m.Card`;
/// - `use material.{Card, Chip}`, selective bare imports (`Card`, `Chip`),
///   legal only while unambiguous (a collision is a `NameCollision` demanding
///   an alias, D-G).
///
/// `alias` and `symbols` are grammatically exclusive: the selective form has
/// no `as` clause.
#[derive(Clone, Debug, PartialEq)]
pub struct UseDecl {
    /// The imported package name, as declared in `[dependencies]`.
    pub package: Symbol,
    /// The explicit alias (`as m`), if written.
    pub alias: Option<Symbol>,
    /// Selective bare imports (`.{A, B}`), each with its own span for precise
    /// per-symbol diagnostics. `None` for the whole-package forms.
    pub symbols: Option<Vec<(Symbol, Span)>>,
    /// Source span of the whole declaration.
    pub span: Span,
}

/// A whole `.byd` file is a list of [`ViewDecl`]s (D11: multiple `View`s per
/// file are allowed).
#[derive(Clone, Debug, PartialEq)]
pub struct ViewDecl {
    /// View name.
    pub name: Symbol,
    /// Declared parameters.
    pub params: Vec<Param>,
    /// The view body, declarations, elements, control flow, a style block.
    pub body: Vec<Member>,
    /// Source span.
    pub span: Span,
}

/// A member of a `View` body. Replaces the prior draft's flat `Stmt`: a View
/// body holds declarations, elements, control flow, and a style block.
#[derive(Clone, Debug, PartialEq)]
pub enum Member {
    /// `var x = init`, a reactive source (lowers to `Signal::new_in`).
    Var {
        /// Binding name.
        name: Symbol,
        /// Declared type, if written (else inferred from `init`; D9).
        ty: Option<Type>,
        /// Initializer expression.
        init: Expr,
        /// Source span.
        span: Span,
    },
    /// `let y = expr`, a computed/constant binding (lowers to a memo).
    Let {
        /// Binding name.
        name: Symbol,
        /// Declared type, if written.
        ty: Option<Type>,
        /// Initializer expression.
        init: Expr,
        /// Source span.
        span: Span,
    },
    /// `fn f(params) -> ret => body`, a computed helper (memo).
    Fn {
        /// Function name.
        name: Symbol,
        /// Parameters.
        params: Vec<Param>,
        /// Declared return type, if written.
        ret: Option<Type>,
        /// Body expression.
        body: Expr,
        /// Source span.
        span: Span,
    },
    /// `inject T as name`, ambient lookup at the controller boundary.
    Inject {
        /// The injected type.
        ty: Type,
        /// Local binding name.
        name: Symbol,
        /// Source span.
        span: Span,
    },
    /// An intrinsic or user-`View` element.
    Element(ElementNode),
    /// `for item in iter { ... }`, structural reactivity.
    For {
        /// Loop variable.
        var: Symbol,
        /// The optional index variable of the `for i, item in items` form
        /// (RFC-0025 §"Stagger", whose `delay: i * 50ms` needs it). Bound to the
        /// item's zero-based position.
        index: Option<Symbol>,
        /// Iterable expression.
        iter: Expr,
        /// Loop body.
        body: Vec<Member>,
        /// Source span.
        span: Span,
    },
    /// `when cond { ... } else { ... }`, structural reactivity.
    When {
        /// Condition.
        cond: Expr,
        /// Then-branch members.
        then: Vec<Member>,
        /// Optional else-branch members.
        els: Option<Vec<Member>>,
        /// Source span.
        span: Span,
    },
    /// `route "/detail/:id" {|params| … }` / `tab "home" { … }`, one case of a
    /// navigation container (RFC-0026). A `tab` is a `route` whose pattern is a
    /// plain literal name and which never binds params, so both share one node;
    /// [`kind`](RouteKind) records which keyword was written, for diagnostics and
    /// for the placement rule (a `route` belongs to a `NavStack`, a `tab` to a
    /// `NavHost`).
    Route {
        /// Which keyword introduced this case.
        kind: RouteKind,
        /// The written pattern (`/detail/:id`, `home`), verbatim. Compiled to
        /// segments at lower time.
        pattern: String,
        /// The optional `{|params| … }` binding, bound to the route's extracted
        /// parameters as a record.
        params: Option<Symbol>,
        /// The case's body.
        body: Vec<Member>,
        /// Source span of the pattern literal alone (for pattern diagnostics).
        pattern_span: Span,
        /// Source span.
        span: Span,
    },
    /// `on mount => action` / `on unmount => action`, a lifecycle effect
    /// (RFC-0028 §4b).
    ///
    /// The entry point a data-backed screen needs: something has to ask for
    /// the data when the screen appears, and every other action position in
    /// `byld` is driven by an input the user has to perform first. It is a
    /// structural effect (RFC-0018), so it mounts and unmounts with its
    /// enclosing scope, and a `when` that brings a screen back runs `on mount`
    /// again rather than showing whatever the previous mount had loaded.
    Lifecycle {
        /// `true` for `on mount`, `false` for `on unmount`.
        on_mount: bool,
        /// The action to run at that edge.
        action: Expr,
        /// Source span.
        span: Span,
    },
    /// `every 5min => action` / `after 2s => action`, a timer effect
    /// (RFC-0029 §5).
    ///
    /// A structural effect like [`Member::Lifecycle`], and for the same
    /// reason: a timer belongs to the scope that declared it, so a screen that
    /// unmounts stops polling instead of leaving a task running against a view
    /// nobody can see (INV-10).
    ///
    /// Coarse by design. `every 16ms` is not a substitute for the animation
    /// runtime (RFC-0010), which evaluates on the GPU; a timer runs its action
    /// on the logic thread and is for seconds-scale refresh.
    Timer {
        /// `true` for `every` (repeating), `false` for `after` (one-shot).
        every: bool,
        /// The interval or delay, in milliseconds.
        dur_ms: u64,
        /// The action to run on each fire.
        action: Expr,
        /// Source span.
        span: Span,
    },
    /// `on measure => action`, an element's own laid-out rect delivered back to
    /// it as a reactive value (RFC-0038).
    ///
    /// Written as a member of the element it measures, because that is the
    /// element it is *about*: the enclosing element consumes it at lower time
    /// and it never reaches the child list, so a subtree that declares no
    /// `on measure` carries nothing.
    ///
    /// The action's payload binding is `it`, a `Size { w, h }` record. Unlike
    /// [`Member::Lifecycle`] and [`Member::Timer`], this is not a structural
    /// effect: it fires from a post-layout step, once per frame in which the
    /// element's rect changed, and never on its own schedule.
    Measure {
        /// The action to run when the rect changes, with `it` bound to the
        /// measured size.
        action: Expr,
        /// Source span.
        span: Span,
    },
    /// `style { .class #[...] ... }`, scoped style rules (static; D5).
    Style {
        /// The style rules.
        rules: Vec<StyleRule>,
        /// Source span.
        span: Span,
    },
    /// A bare expression statement (e.g. a call).
    Expr(Expr),
}

/// One result arm of an async controller call (RFC-0028 §4):
/// `ok report => { … }`.
///
/// The binding is exactly RFC-0019's callback-prop shape, one parameter bound
/// over an action body, which is why an arm needs no new evaluation machinery:
/// the reply is delivered as the arm's payload, the same way an event payload
/// reaches an `#[pointer_move(e) => …]` handler.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultArm {
    /// The name the reply (or error record) binds to inside the body.
    pub binding: Symbol,
    /// The arm's action body.
    pub action: Box<Expr>,
    /// Source span.
    pub span: Span,
}

/// Which navigation keyword introduced a [`Member::Route`] (RFC-0026).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    /// `route "/detail/:id" { … }`, a `NavStack` case; the pattern may carry
    /// `:param` and `*` segments.
    Route,
    /// `tab "home" { … }`, a `NavHost` case; the pattern is a plain name.
    Tab,
}

impl RouteKind {
    /// The keyword as written, for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Tab => "tab",
        }
    }

    /// The navigation container this case belongs inside.
    #[must_use]
    pub const fn container(self) -> &'static str {
        match self {
            Self::Route => "NavStack",
            Self::Tab => "NavHost",
        }
    }
}

/// An element: `IDENT ("(" content ")")? attr_block? ("{" children "}" | "=>" action)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ElementNode {
    /// Intrinsic (`Column`, `Text`, …) or user-`View` name.
    pub name: Symbol,
    /// Positional `(...)` content (a `Text`'s string, a `Button`'s label).
    pub content: Vec<Arg>,
    /// `#[...]` properties / config / events.
    pub attrs: Vec<Attr>,
    /// The `=> action` shorthand (the hoisted primary `tap` event), if present.
    pub action: Option<Expr>,
    /// The `{ ... }` children block.
    pub children: Vec<Member>,
    /// Source span.
    pub span: Span,
}

/// One `#[...]` attribute: either a property (`name: expr`) or an engine event
/// (`name(payload)? => expr`), RFC-0003 D4-bis. The kind is decided
/// syntactically by the separator; a mismatch against the intrinsic's contract
/// is a *checker* error (M10), not a parse error.
#[derive(Clone, Debug, PartialEq)]
pub struct Attr {
    /// Attribute name.
    pub name: Symbol,
    /// The sub-property axis, if written as `name.axis: value` (RFC-0011
    /// §"Dual surface", e.g. `translate.y: 2`). `None` for the ordinary
    /// `name: value` / `name(payload)? => action` forms.
    pub axis: Option<Symbol>,
    /// Whether this is a property binding or an engine event.
    pub kind: AttrKind,
    /// Source span.
    pub span: Span,
}

/// The two attribute flavors distinguished by the `:` vs `=>` separator.
#[derive(Clone, Debug, PartialEq)]
pub enum AttrKind {
    /// `name: value`, binds a value (including reactive props and callback
    /// props, since a function *value* is still a value).
    Prop {
        /// The bound value expression.
        value: Expr,
    },
    /// `name(payload)? => action`, maps an engine event to an action; the
    /// optional `payload` binds the event record (e.g. `pointer_move(e)`).
    Event {
        /// The optional payload binding.
        payload: Option<Symbol>,
        /// The action expression.
        action: Expr,
    },
    /// `..expr`, a style spread (RFC-0016): splice the attributes of the
    /// [`StyleValue`](Expr::StyleValue) `expr` resolves to into this list, in
    /// written order, before any inline attributes override them. The owning
    /// [`Attr`]'s `name` is empty for a spread.
    Spread {
        /// The style expression being spread (an identifier bound to a style,
        /// or an inline `style { … }`).
        value: Expr,
    },
}

/// One engine-owned interaction state an `on <state> { }` block (RFC-0016/
/// RFC-0024) can target. The engine reports these via `StyleState`; several
/// combined with `+` form a compound selector (RFC-0024), and matching blocks
/// apply by specificity (state count) then declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleStateKind {
    /// The pointer is over the element.
    Hover,
    /// The element is being pressed (pointer down inside it).
    Pressed,
    /// The element holds keyboard focus.
    Focused,
    /// The element is disabled (also gates event dispatch, RFC-0012 §S5).
    Disabled,
    /// A value-widget's value is true (`Checkbox`/`Toggle`), RFC-0024.
    Checked,
    /// The element is the active selection (`selected:`, or a `RadioButton`
    /// whose `bind == value`), RFC-0024.
    Selected,
    /// The element's `invalid:` prop is true (form validation), RFC-0024.
    Invalid,
    /// A `Checkbox`'s `indeterminate:` mixed state, RFC-0024.
    Indeterminate,
    /// The element is being dragged past the drag threshold, RFC-0024.
    Dragging,
}

impl StyleStateKind {
    /// Parses a state name; `None` for an unrecognized state (a compile error,
    /// [`CompileError::UnknownStyleState`]).
    ///
    /// [`CompileError::UnknownStyleState`]: crate::diagnostics::CompileError::UnknownStyleState
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "hover" => Some(Self::Hover),
            "pressed" => Some(Self::Pressed),
            "focused" => Some(Self::Focused),
            "disabled" => Some(Self::Disabled),
            "checked" => Some(Self::Checked),
            "selected" => Some(Self::Selected),
            "invalid" => Some(Self::Invalid),
            "indeterminate" => Some(Self::Indeterminate),
            "dragging" => Some(Self::Dragging),
            _ => None,
        }
    }

    /// The canonical spelling of this state, for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hover => "hover",
            Self::Pressed => "pressed",
            Self::Focused => "focused",
            Self::Disabled => "disabled",
            Self::Checked => "checked",
            Self::Selected => "selected",
            Self::Invalid => "invalid",
            Self::Indeterminate => "indeterminate",
            Self::Dragging => "dragging",
        }
    }

    /// Every state name, for `closest_match` suggestions.
    pub const NAMES: [&'static str; 9] = [
        "hover",
        "pressed",
        "focused",
        "disabled",
        "checked",
        "selected",
        "invalid",
        "indeterminate",
        "dragging",
    ];
}

/// An `on <state> { attr* }` block inside a `style { }` value (RFC-0016): the
/// attributes that apply only while the element is in the engine-owned `state`.
/// Resolved at render time against the live `StyleState` mask, the *only*
/// sanctioned dynamism in an otherwise-static style (D8).
#[derive(Clone, Debug, PartialEq)]
pub struct StateBlock {
    /// The interaction states that must **all** be active for this block to
    /// apply (RFC-0024 combined selectors: `on focused+hover { … }`). A single
    /// state is the common case; the block's *specificity* is `states.len()`.
    pub states: Vec<StyleStateKind>,
    /// The attributes overlaid onto the base while every `states` entry is active.
    pub attrs: Vec<Attr>,
    /// Source span.
    pub span: Span,
}

/// A style rule: `. IDENT #[ attrs ]` (D5).
#[derive(Clone, Debug, PartialEq)]
pub struct StyleRule {
    /// The class name (after the `.`).
    pub class: Symbol,
    /// The class's attributes.
    pub attrs: Vec<Attr>,
    /// Source span.
    pub span: Span,
}

/// A call / content argument: `(IDENT ":")? expr`.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    /// The optional `name:` label.
    pub name: Option<Symbol>,
    /// The argument value.
    pub value: Expr,
}

/// Binary operators. Arithmetic (`+ - * /`) is the original minimal surface
/// (RFC-0020 reactive shape params); RFC-0027 §1/§2 adds comparison
/// (`== != < <= > >=`) and logic (`&& ||`). Note `&&`/`||` are still lowered as
/// short-circuiting control flow (RFC-0027 §2), not through the eager
/// binary-op tables, the variants exist so the parser can name the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&` (short-circuit)
    And,
    /// `||` (short-circuit)
    Or,
}

/// Prefix unary operators (RFC-0027 §2): boolean `!` and numeric negation `-`.
/// `Neg` unifies with the leading-`-` sign form for non-literal operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// `!`, boolean negation.
    Not,
    /// `-`, numeric negation of a non-literal operand.
    Neg,
}

/// Assignment operators (`= += -=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    /// `=`
    Assign,
    /// `+=`
    Add,
    /// `-=`
    Sub,
}

/// Postfix mutation operators (`++ --`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostfixOp {
    /// `++`
    Inc,
    /// `--`
    Dec,
}

/// A piece of an interpolated string literal.
#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    /// A literal text run.
    Text(String),
    /// An interpolated `{ expr }`.
    Interp(Box<Expr>),
}

/// An expression. Every variant carries its own [`Span`].
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// An integer literal (`i64`; D9).
    IntLit(i64, Span),
    /// A float literal (`f64`; D9).
    FloatLit(f64, Span),
    /// An angle literal (`360deg`/`1.5rad`, RFC-0011 T1), already
    /// canonicalized to radians by the lexer.
    AngleLit(f64, Span),
    /// A string literal, possibly interpolated.
    StrLit(Vec<StrPart>, Span),
    /// An identifier reference.
    Ident(Symbol, Span),
    /// An array literal `[a, b, ...]`.
    Array(Vec<Expr>, Span),
    /// A parenthesized tuple `(a, b, ...)`, used for `Len` pairs/quads such as
    /// `p: (8, 16)` (RFC-0005 §1). A single parenthesized expression is *not* a
    /// tuple; it parses to the inner expression directly.
    Tuple(Vec<Arg>, Span),
    /// A leading-dot class reference, e.g. the `.title` in `#[style: .title]`
    /// (RFC-0002 §"Grammar" `style_rule`; resolved against the View's style map
    /// in M11).
    ClassRef(Symbol, Span),
    /// Member access `base.field`.
    Member {
        /// The receiver expression.
        base: Box<Expr>,
        /// The field name.
        field: Symbol,
        /// Source span.
        span: Span,
    },
    /// A call `callee(args)`.
    Call {
        /// The callee.
        callee: Box<Expr>,
        /// The arguments.
        args: Vec<Arg>,
        /// Source span.
        span: Span,
    },
    /// An async controller call with result arms (RFC-0028 §4):
    /// `api.forecast("Tokyo") ok r => { … } err e => { … }`.
    ///
    /// A **statement**, not a value: evaluating it packages the arguments,
    /// schedules the method on the async pool and returns immediately, so it
    /// is legal only in action position. In a `let`/memo or any other pure
    /// context it is
    /// [`EffectInPureContext`](crate::diagnostics::CompileError::EffectInPureContext),
    /// which is what keeps a projection a pure function of its reads.
    ///
    /// The no-arm form (`api.ping()`, fire-and-forget) is *not* parsed as this
    /// node: it is an ordinary [`Expr::Call`] whose callee resolves to a
    /// controller handle at lower time, so one lowering path serves both and a
    /// call cannot mean different things depending on whether anyone read its
    /// answer.
    ControllerCall {
        /// The call itself (`api.forecast("Tokyo")`), an [`Expr::Call`] whose
        /// callee is a [`Expr::Member`] on the injected handle.
        call: Box<Expr>,
        /// `ok name => action`: the success arm and the name its payload binds
        /// to.
        ok: Option<ResultArm>,
        /// `err name => action`: the failure arm.
        err: Option<ResultArm>,
        /// Source span.
        span: Span,
    },
    /// A lambda `|p| e` or `(p) => e`.
    Lambda {
        /// Parameter names (types inferred from the use site; E2).
        params: Vec<Symbol>,
        /// The body expression.
        body: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A brace-delimited action block `{ stmt* }` (RFC-0019): the body of a
    /// callback-prop literal (`on_tap: { count++ }`). Holds zero or more action
    /// statements evaluated in order; the value is the last statement's (or
    /// [`Value::Unit`] for the empty no-op default `{}`). Distinct from a
    /// `style { … }` value and from a View/`when`/`for` body, those consume
    /// their braces structurally and never reach expression position.
    ///
    /// [`Value::Unit`]: crate::interp::env::Value::Unit
    Block(Vec<Expr>, Span),
    /// An assignment `target op value` (`= += -=`).
    Assign {
        /// The l-value target.
        target: Box<Expr>,
        /// The operator.
        op: AssignOp,
        /// The new value.
        value: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A postfix mutation `target++` / `target--`.
    Postfix {
        /// The l-value target.
        target: Box<Expr>,
        /// The operator.
        op: PostfixOp,
        /// Source span.
        span: Span,
    },
    /// A prefix unary expression `op rhs` (`!b`, `-x`), RFC-0027 §2.
    Unary {
        /// The operator.
        op: UnOp,
        /// The operand.
        rhs: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// An index expression `base[index]` (RFC-0027 §4). Out-of-range access
    /// degrades to [`Value::Unit`](crate::interp::env::Value::Unit) with a
    /// logic-thread diagnostic (INV-4), never a panic.
    Index {
        /// The indexed receiver.
        base: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A record literal `{ k: v, .., ..spread }` (RFC-0027 §6): a name-keyed,
    /// ordered, immutable data aggregate. Distinct from [`Expr::Tuple`]
    /// (positional layout data) and from a callback [`Expr::Block`]; the parser
    /// disambiguates on a leading `IDENT :` or `..spread`.
    Record {
        /// The declared fields, in written order.
        fields: Vec<(Symbol, Expr)>,
        /// An optional `..spread` base whose fields seed the record before the
        /// written fields override them (`{ ..r, done: true }`).
        spread: Option<Box<Expr>>,
        /// Source span.
        span: Span,
    },
    /// A binary arithmetic expression `lhs op rhs` (`+ - * /`). Standard
    /// precedence (`* /` over `+ -`), left-associative, both tighter than the
    /// ternary/`with`/`merge` band, so `p * 360 with anim.spring()` animates
    /// the product (RFC-0010 × RFC-0020).
    Binary {
        /// The operator.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A ternary `cond ? then : els`.
    Ternary {
        /// The condition.
        cond: Box<Expr>,
        /// The then-branch.
        then: Box<Expr>,
        /// The else-branch.
        els: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// An animated attribute value `value with anim.*(…)` (RFC-0010): `value`
    /// is the (usually ternary) target and `anim` is the `anim.*` curve call,
    /// resolved to a typed `Curve` at lowering. `with` binds below the ternary,
    /// so `a ? b : c with k` parses as `(a ? b : c) with k`.
    Animated {
        /// The target value expression (scalar/ternary).
        value: Box<Expr>,
        /// The `anim.*` curve call, resolved to a typed `Curve` at lower time.
        anim: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// One timed step of a keyframe sequence, `50%: 200 ease_out` (RFC-0025
    /// §4). Appears only as an argument, and the parser assigns it no meaning
    /// beyond its shape (D6): `anim.keyframes(…)` is the one call that reads
    /// these, resolved with everything else at lower time.
    KeyframeStep {
        /// Position in the sequence as a `0..=1` fraction (`50%` → `0.5`),
        /// canonicalized by the lexer.
        percent: f64,
        /// The value this step animates to.
        value: Box<Expr>,
        /// The optional easing name governing the segment that *arrives* at this
        /// step (`ease_out`); absent means linear.
        easing: Option<(Symbol, Span)>,
        /// Source span.
        span: Span,
    },
    /// A first-class style value `style { name: value, … }` (RFC-0016): an
    /// ordered bundle of attributes, `let`-bound and applied to an element with
    /// the `..` spread. Static and composable; no cascade.
    StyleValue {
        /// The style's base attributes, in written order.
        attrs: Vec<Attr>,
        /// `on <state> { … }` interaction-state blocks (RFC-0016), applied at
        /// render time over the base when their state is active.
        states: Vec<StateBlock>,
        /// Source span.
        span: Span,
    },
    /// `left merge right` (RFC-0016 M3): composes two style values into one; on
    /// a conflicting attribute the right operand wins. Both operands resolve to
    /// styles at lower time.
    Merge {
        /// The base style.
        left: Box<Expr>,
        /// The overriding style.
        right: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A parse-error placeholder, so recovery can continue and collect more
    /// diagnostics (RFC-0002 §"Parser").
    Error(Span),
}

impl Expr {
    /// The source span of this expression.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::IntLit(_, span)
            | Self::FloatLit(_, span)
            | Self::AngleLit(_, span)
            | Self::StrLit(_, span)
            | Self::Ident(_, span)
            | Self::Array(_, span)
            | Self::Tuple(_, span)
            | Self::ClassRef(_, span)
            | Self::Member { span, .. }
            | Self::Call { span, .. }
            | Self::ControllerCall { span, .. }
            | Self::Lambda { span, .. }
            | Self::Block(_, span)
            | Self::Assign { span, .. }
            | Self::Postfix { span, .. }
            | Self::Unary { span, .. }
            | Self::Index { span, .. }
            | Self::Record { span, .. }
            | Self::Binary { span, .. }
            | Self::Ternary { span, .. }
            | Self::Animated { span, .. }
            | Self::KeyframeStep { span, .. }
            | Self::StyleValue { span, .. }
            | Self::Merge { span, .. }
            | Self::Error(span) => *span,
        }
    }
}

// INV-6: the AST must be `Send` so a `CompiledView` built from it can cross the
// file-watcher → logic-thread channel. If any node grew a non-`Send` field
// (e.g. an `Rc`), this would stop compiling.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<ViewDecl>();
    assert_send::<Member>();
    assert_send::<Expr>();
    assert_send::<Type>();
    assert_send::<UseDecl>();
};

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 1)
    }

    /// Build a small tree by hand, exercises every owning node and proves the
    /// AST can represent `Button("+") #[bg: 1] => count++`.
    #[test]
    fn hand_built_tree_round_trips() {
        let count = Symbol::intern("count");
        let element = ElementNode {
            name: Symbol::intern("Button"),
            content: vec![Arg {
                name: None,
                value: Expr::StrLit(vec![StrPart::Text("+".to_string())], sp()),
            }],
            attrs: vec![Attr {
                name: Symbol::intern("bg"),
                axis: None,
                kind: AttrKind::Prop {
                    value: Expr::IntLit(1, sp()),
                },
                span: sp(),
            }],
            action: Some(Expr::Postfix {
                target: Box::new(Expr::Ident(count.clone(), sp())),
                op: PostfixOp::Inc,
                span: sp(),
            }),
            children: Vec::new(),
            span: sp(),
        };
        let view = ViewDecl {
            name: Symbol::intern("Counter"),
            params: Vec::new(),
            body: vec![
                Member::Var {
                    name: count,
                    ty: None,
                    init: Expr::IntLit(0, sp()),
                    span: sp(),
                },
                Member::Element(element),
            ],
            span: sp(),
        };

        assert_eq!(view.body.len(), 2);
        let Member::Element(el) = &view.body[1] else {
            panic!("expected element");
        };
        assert!(matches!(el.action, Some(Expr::Postfix { .. })));
        assert_eq!(el.attrs[0].name, Symbol::intern("bg"));
    }

    #[test]
    fn event_attr_carries_optional_payload() {
        let attr = Attr {
            name: Symbol::intern("pointer_move"),
            axis: None,
            kind: AttrKind::Event {
                payload: Some(Symbol::intern("e")),
                action: Expr::Error(sp()),
            },
            span: sp(),
        };
        let AttrKind::Event { payload, .. } = &attr.kind else {
            panic!("expected event");
        };
        assert_eq!(*payload, Some(Symbol::intern("e")));
    }

    #[test]
    fn function_type_is_representable() {
        let ty = Type::Function {
            params: vec![Type::Named {
                name: Symbol::intern("ChangeEvent"),
                args: vec![Type::Named {
                    name: Symbol::intern("Str"),
                    args: Vec::new(),
                    span: sp(),
                }],
                span: sp(),
            }],
            ret: None,
            span: sp(),
        };
        assert_eq!(ty.span(), sp());
    }

    /// Runtime echo of the compile-time `assert_send` above (keeps the
    /// intent visible in the test report).
    #[test]
    fn ast_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ViewDecl>();
    }
}
