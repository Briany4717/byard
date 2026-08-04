//! RFC-0010 / RFC-0025 typed animation curves and their resolution from an
//! `anim.*(…)` call.
//!
//! The parser produces an [`Expr::Animated`](crate::parser::ast::Expr::Animated)
//! whose `anim` side is an ordinary call/member expression; [`resolve_motion`]
//! turns that surface into a typed, argument-validated [`MotionSpec`] at
//! lowering time, the curve plus RFC-0025's repeat/reverse/delay/from
//! modifiers. [`resolve_keyframes`] does the same for an `anim.keyframes(…)`
//! sequence, which stands in *value* position rather than after `with` (it
//! supplies its own values). The parser stays free of any knowledge of the
//! curve catalog (mirrors D6: the surface is generic; meaning is assigned
//! later).
//!
//! Everything here is the *grammar → typed spec* half; `interp::eval` drives the
//! resulting [`Motion`](byard_core::frame::Motion) against the engine clock and
//! `byard-core`'s `frame` owns the repeat/keyframe time arithmetic.

use crate::diagnostics::{CompileError, Span};
use crate::parser::ast::{Arg, Expr};
use crate::symbol::Symbol;
use crate::util::closest_match;
use byard_core::frame::{MAX_KEYFRAME_STEPS, MotionCurve, RepeatMode};

/// The easing family for `anim.ease(…)`. `InOut` is the default when no family
/// is named (the most common, symmetric ease).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EaseKind {
    /// Accelerate from rest.
    In,
    /// Decelerate to rest.
    Out,
    /// Symmetric ease in then out.
    InOut,
}

/// A resolved, typed animation curve (RFC-0010 §"Typed animation spec").
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Curve {
    /// A fixed-duration linear ramp.
    Linear {
        /// Duration in milliseconds.
        ms: u32,
    },
    /// A fixed-duration eased ramp.
    Ease {
        /// Duration in milliseconds.
        ms: u32,
        /// The easing family.
        kind: EaseKind,
    },
    /// A duration-free damped spring, continuous under interruption.
    Spring {
        /// Spring stiffness.
        stiffness: f32,
        /// Damping coefficient.
        damping: f32,
        /// Initial velocity.
        v0: f32,
    },
}

impl Curve {
    /// The default spring (RFC-0010 A2): a snappy, iOS-feel `210/20` with no
    /// initial velocity, what a bare `anim.spring()` resolves to.
    pub const DEFAULT_SPRING: Curve = Curve::Spring {
        stiffness: 210.0,
        damping: 20.0,
        v0: 0.0,
    };
}

/// An animation's start offset (RFC-0025 §5).
///
/// The offset is kept as an *expression*, not a resolved number, because that
/// is what makes a stagger possible: `delay: i * 50ms` inside a `for` body has
/// to be evaluated in the body's own scope, per item, at sample time.
#[derive(Clone, Copy, Debug)]
pub enum Delay<'a> {
    /// No delay, motion begins the moment the property mounts or retargets.
    None,
    /// `delay: <ms>`: hold the start value for that many milliseconds. A fresh
    /// target **cancels** a pending delay (§5), so a delayed transition can
    /// never overwrite a more recent interaction.
    Offset(&'a Expr),
    /// `anim.stagger(<curve>, <step>, <index>)`: an offset of `step × index`,
    /// and deliberately **not** cancellable (§5), an entrance cascade is
    /// intentional sequencing, not a response to input.
    Stagger {
        /// Per-item step, in milliseconds.
        step_ms: u32,
        /// The item-index expression (a `for` loop's index).
        index: &'a Expr,
    },
}

impl Delay<'_> {
    /// Whether a retarget cancels this delay (RFC-0025 §5).
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        !matches!(self, Self::Stagger { .. })
    }

    /// Whether there is any offset to wait out at all.
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// A fully resolved `with anim.*(…)` animation: RFC-0010's curve plus
/// RFC-0025's repeat / reverse / delay / explicit-start modifiers.
#[derive(Clone, Copy, Debug)]
pub struct MotionSpec<'a> {
    /// The curve driving one play.
    pub curve: Curve,
    /// How many times it plays (RFC-0025 §1).
    pub repeat: RepeatMode,
    /// Whether alternate plays run back-to-front (`reverse: true`).
    pub reverse: bool,
    /// The start offset (§5).
    pub delay: Delay<'a>,
    /// `restart: <expr>`, replay the animation whenever this value changes.
    ///
    /// A mount-time animation (an entrance, a cascade) is otherwise observable
    /// exactly once: its endpoints never change, so nothing ever retargets it.
    /// This is the reference-free way to say "play that again", the same role a
    /// changed `key` plays in a retained-tree framework, and it is what makes
    /// RFC-0025's own stagger example re-runnable. A restart honours the
    /// animation's delays again, so a cascade replays *in order*.
    pub restart: Option<&'a Expr>,
    /// `from:`, the explicit start value of the animation.
    ///
    /// Not in the RFC's snippets, and required by them: a looping animation
    /// needs *two* endpoints, and a target alone only gives one (a `with`
    /// animation seeds `from = to` on mount, so `scale: 1.3 with
    /// anim.spring(repeat: infinite, reverse: true)` would oscillate between
    /// 1.3 and 1.3). `from:` names the other end, exactly as CSS keyframes'
    /// `from`/`to` pair does. A one-shot animation ignores it unless written.
    pub from: Option<&'a Expr>,
}

impl<'a> MotionSpec<'a> {
    /// The plain RFC-0010 spec for `curve`: one play, no offset, no explicit
    /// start, the shape every animation had before RFC-0025.
    #[must_use]
    pub fn once(curve: Curve) -> Self {
        Self {
            curve,
            repeat: RepeatMode::Once,
            reverse: false,
            delay: Delay::None,
            from: None,
            restart: None,
        }
    }

    /// Whether this spec needs the RFC-0025 repeat clock at all. When false the
    /// runtime takes the original single-shot path unchanged.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        !self.repeat.is_repeating()
            && self.delay.is_none()
            && self.from.is_none()
            && self.restart.is_none()
            && !self.reverse
    }
}

/// One step of an `anim.keyframes(…)` sequence (RFC-0025 §3).
#[derive(Clone, Copy, Debug)]
pub struct Keyframe<'a> {
    /// Position in the sequence, `0.0..=1.0`.
    pub percent: f32,
    /// The value at this step, evaluated by the interpreter, so a step may be
    /// a scalar (`200`) or a coordinate pair (`(50, 0)`).
    pub value: &'a Expr,
    /// Easing of the segment *arriving* at this step, as a
    /// [`MotionCurve`] tag (`LINEAR` by default).
    pub easing: u32,
}

/// A resolved `anim.keyframes(…)` track: a timed value sequence for one
/// property (RFC-0025 §3), with the same repeat/reverse/delay modifiers as a
/// curve.
#[derive(Clone, Debug)]
pub struct KeyframeTrack<'a> {
    /// The steps, ascending by percent, `2..=MAX_KEYFRAME_STEPS` of them.
    pub steps: Vec<Keyframe<'a>>,
    /// Total duration of one play, in milliseconds.
    pub duration_ms: u32,
    /// How many times the sequence plays.
    pub repeat: RepeatMode,
    /// Whether alternate plays run back-to-front.
    pub reverse: bool,
    /// The start offset.
    pub delay: Delay<'a>,
    /// `restart:`, replay the sequence whenever this value changes.
    pub restart: Option<&'a Expr>,
}

/// The closed set of curve names, for the unknown-curve suggestion.
const CURVE_NAMES: &[&str] = &[
    "linear",
    "ease",
    "ease_in",
    "ease_out",
    "ease_in_out",
    "spring",
    "keyframes",
    "stagger",
];

/// The modifier names every curve accepts (RFC-0025 §4), for suggestions.
const MODIFIER_NAMES: &[&str] = &[
    "repeat", "reverse", "delay", "loop", "duration", "from", "restart",
];

/// The easing names a keyframe step may carry, mapped to their curve tags.
const SEGMENT_EASINGS: &[(&str, u32)] = &[
    ("linear", MotionCurve::LINEAR),
    ("ease_in", MotionCurve::EASE_IN),
    ("ease_out", MotionCurve::EASE_OUT),
    ("ease_in_out", MotionCurve::EASE_IN_OUT),
    ("ease", MotionCurve::EASE_IN_OUT),
];

/// Resolves the `anim` side of an [`Expr::Animated`](crate::parser::ast::Expr)
/// into a typed [`Curve`], validating its arguments (RFC-0010).
///
/// The curve-only view of [`resolve_motion`], for the callers that borrow the
/// curve grammar without the animation runtime behind it (RFC-0021's
/// `snap_spring`).
///
/// # Errors
///
/// Returns the first diagnostic encountered (not an `anim.*` call, unknown
/// curve, or a bad argument list).
pub fn resolve_curve(anim: &Expr) -> Result<Curve, CompileError> {
    resolve_motion(anim).map(|spec| spec.curve)
}

/// Resolves the `anim` side of a `with` clause into a typed, validated
/// [`MotionSpec`] (RFC-0010 curve + RFC-0025 modifiers).
///
/// Accepts the parenthesised form (`anim.spring(stiffness: 210)`), the bare
/// form (`anim.spring`, every default), and any mix of positional curve
/// arguments with trailing modifiers (`anim.linear(800ms, repeat: infinite)`).
/// Any name outside [`CURVE_NAMES`] is [`CompileError::UnknownAnimation`] with a
/// Levenshtein suggestion; malformed arguments are
/// [`CompileError::InvalidAnimation`].
///
/// # Errors
///
/// Returns the first diagnostic encountered (not an `anim.*` call, unknown
/// curve, a bad argument list, or `anim.keyframes` used after `with`, keyframes
/// carry their own values and belong in value position).
pub fn resolve_motion(anim: &Expr) -> Result<MotionSpec<'_>, CompileError> {
    let (name, name_span, args) = destructure_anim_call(anim, true)?;
    if name.as_str() == "keyframes" {
        return Err(CompileError::InvalidAnimation {
            span: name_span,
            message: "`anim.keyframes(…)` supplies its own values, so it is the property value \
                      itself, write `translate: anim.keyframes(0%: …, 100%: …)`, not `with`"
                .to_string(),
        });
    }
    if name.as_str() == "stagger" {
        return resolve_stagger(&args, name_span);
    }
    let (curve_args, mods) = split_modifiers(&args)?;
    let curve = resolve_named_curve(&name, name_span, &curve_args, mods.duration_ms)?;
    Ok(MotionSpec {
        curve,
        repeat: mods.repeat.unwrap_or_default(),
        reverse: mods.reverse.unwrap_or(false),
        delay: mods.delay.map_or(Delay::None, Delay::Offset),
        from: mods.from,
        restart: mods.restart,
    })
}

/// Recognizes an `anim.keyframes(…)` call in **value** position and resolves it
/// to a validated [`KeyframeTrack`] (RFC-0025 §3). Returns `None` for any other
/// expression, so the ordinary value path is untouched.
///
/// # Errors
///
/// A recognized call with a malformed sequence yields
/// [`CompileError::InvalidAnimation`] (fewer than two steps, unordered or
/// out-of-range percentages, a missing `duration:`, an unknown easing name) or
/// [`CompileError::TooManyKeyframes`] past the RFC's step cap.
pub fn resolve_keyframes(expr: &Expr) -> Option<Result<KeyframeTrack<'_>, CompileError>> {
    let (name, name_span, args) = destructure_anim_call(expr, true).ok()?;
    if name.as_str() != "keyframes" {
        return None;
    }
    Some(resolve_keyframe_args(&args, name_span))
}

/// Whether `expr` is an `anim.keyframes(…)` call, the cheap shape test the
/// evaluation chokepoint runs before doing any resolution work.
#[must_use]
pub fn is_keyframes_call(expr: &Expr) -> bool {
    matches!(
        destructure_anim_call(expr, true),
        Ok((name, ..)) if name.as_str() == "keyframes"
    )
}

/// Validates the argument list of an `anim.keyframes(…)` call (RFC-0025 §3/§4).
fn resolve_keyframe_args<'a>(
    args: &[&'a Arg],
    call_span: Span,
) -> Result<KeyframeTrack<'a>, CompileError> {
    let (step_args, mods) = split_modifiers(args)?;
    let mut steps = Vec::with_capacity(step_args.len());
    for arg in &step_args {
        let Expr::KeyframeStep {
            percent,
            value,
            easing,
            span,
        } = &arg.value
        else {
            return Err(CompileError::InvalidAnimation {
                span: arg.value.span(),
                message: "`anim.keyframes` takes timed steps, e.g. \
                          `anim.keyframes(0%: 0, 100%: 1, duration: 400ms)`"
                    .to_string(),
            });
        };
        if !(0.0..=1.0).contains(percent) {
            return Err(CompileError::InvalidAnimation {
                span: *span,
                message: "a keyframe percentage must be between 0% and 100%".to_string(),
            });
        }
        let easing = match easing {
            Some((name, span)) => resolve_segment_easing(name, *span)?,
            None => MotionCurve::LINEAR,
        };
        #[allow(clippy::cast_possible_truncation)]
        steps.push(Keyframe {
            percent: *percent as f32,
            value: value.as_ref(),
            easing,
        });
    }
    if steps.len() < 2 {
        return Err(CompileError::InvalidAnimation {
            span: call_span,
            message: "`anim.keyframes` needs at least two steps to interpolate between".to_string(),
        });
    }
    if steps.len() > MAX_KEYFRAME_STEPS {
        return Err(CompileError::TooManyKeyframes {
            span: call_span,
            found: steps.len(),
        });
    }
    if let Some(out_of_order) = steps.windows(2).position(|w| w[1].percent <= w[0].percent) {
        return Err(CompileError::InvalidAnimation {
            span: step_args[out_of_order + 1].value.span(),
            message: "keyframe steps must be written in ascending order of percentage".to_string(),
        });
    }
    let Some(duration_ms) = mods.duration_ms.filter(|ms| *ms > 0) else {
        return Err(CompileError::InvalidAnimation {
            span: call_span,
            message: "`anim.keyframes` needs a non-zero `duration:`, e.g. `duration: 2s`"
                .to_string(),
        });
    };
    if let Some(from) = mods.from {
        return Err(CompileError::InvalidAnimation {
            span: from.span(),
            message: "`from:` has no meaning for keyframes, the `0%` step is the start value"
                .to_string(),
        });
    }
    Ok(KeyframeTrack {
        steps,
        duration_ms,
        repeat: mods.repeat.unwrap_or_default(),
        reverse: mods.reverse.unwrap_or(false),
        delay: mods.delay.map_or(Delay::None, Delay::Offset),
        restart: mods.restart,
    })
}

/// Resolves `anim.stagger(<curve>, <step>, <index>)` (RFC-0025 §"Stagger"),
/// sugar for the `delay: index * step` pattern, with the non-cancellable delay
/// semantics of an entrance cascade. Arguments are positional or named
/// (`base`/`step`/`index`).
fn resolve_stagger<'a>(args: &[&'a Arg], call_span: Span) -> Result<MotionSpec<'a>, CompileError> {
    const FIELDS: [&str; 3] = ["base", "step", "index"];
    // Modifiers may sit on the stagger itself (`anim.stagger(spring(), 90ms, i,
    // restart: attempt)`) as well as on its base curve, the outer position is
    // the natural place for `restart:`, which is about the cascade, not the curve.
    let (args, stagger_mods) = split_modifiers(args)?;
    let mut slots: [Option<&Expr>; 3] = [None; 3];
    for (position, arg) in args.iter().enumerate() {
        let slot = match &arg.name {
            Some(name) => FIELDS
                .iter()
                .position(|f| *f == name.as_str())
                .ok_or_else(|| CompileError::InvalidAnimation {
                    span: arg.value.span(),
                    message: format!(
                        "unknown `anim.stagger` argument `{name}` (expected base / step / index)"
                    ),
                })?,
            None => position,
        };
        if slot >= FIELDS.len() || slots[slot].is_some() {
            return Err(CompileError::InvalidAnimation {
                span: arg.value.span(),
                message: "`anim.stagger` takes exactly three arguments: a curve, the per-item \
                          step, and the item index, e.g. `anim.stagger(spring(), 50ms, i)`"
                    .to_string(),
            });
        }
        slots[slot] = Some(&arg.value);
    }
    let [Some(base), Some(step), Some(index)] = slots else {
        return Err(CompileError::InvalidAnimation {
            span: call_span,
            message: "`anim.stagger` takes a curve, the per-item step, and the item index, \
                      e.g. `anim.stagger(spring(), 50ms, i)`"
                .to_string(),
        });
    };
    // The base curve may be written bare (`spring()`) inside `stagger`, since
    // the `anim.` namespace is already established by the enclosing call.
    let (name, name_span, curve_args) = destructure_anim_call(base, false)?;
    let (curve_args, mods) = split_modifiers(&curve_args)?;
    let curve = resolve_named_curve(&name, name_span, &curve_args, mods.duration_ms)?;
    let step_ms = duration_literal(step, "anim.stagger's step")?;
    Ok(MotionSpec {
        curve,
        repeat: mods.repeat.unwrap_or_default(),
        reverse: mods.reverse.unwrap_or(false),
        delay: Delay::Stagger { step_ms, index },
        from: mods.from,
        restart: mods.restart.or(stagger_mods.restart),
    })
}

/// Maps a keyframe step's easing name to its [`MotionCurve`] tag.
fn resolve_segment_easing(name: &Symbol, span: Span) -> Result<u32, CompileError> {
    SEGMENT_EASINGS
        .iter()
        .find(|(n, _)| *n == name.as_str())
        .map(|(_, tag)| *tag)
        .ok_or_else(|| CompileError::InvalidAnimation {
            span,
            message: format!(
                "unknown keyframe easing `{name}` (expected {})",
                SEGMENT_EASINGS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        })
}

/// The RFC-0025 §4 modifiers, as written on one curve call.
#[derive(Default)]
struct Modifiers<'a> {
    repeat: Option<RepeatMode>,
    reverse: Option<bool>,
    delay: Option<&'a Expr>,
    duration_ms: Option<u32>,
    from: Option<&'a Expr>,
    restart: Option<&'a Expr>,
}

/// Splits a curve call's arguments into the curve's own arguments and the
/// shared RFC-0025 modifiers, validating each modifier's value.
fn split_modifiers<'a>(args: &[&'a Arg]) -> Result<(Vec<&'a Arg>, Modifiers<'a>), CompileError> {
    let mut rest = Vec::with_capacity(args.len());
    let mut mods = Modifiers::default();
    // `loop:` and `repeat:` are two spellings of one field; writing both is
    // contradictory in either order.
    let mut repeat_written = None;
    for arg in args {
        let Some(name) = &arg.name else {
            rest.push(*arg);
            continue;
        };
        let span = arg.value.span();
        match name.as_str() {
            "repeat" | "loop" => {
                if repeat_written.is_some_and(|written| written != name.as_str()) {
                    return Err(CompileError::InvalidAnimation {
                        span,
                        message: "`loop:` is sugar for `repeat: infinite`, use one or the other"
                            .to_string(),
                    });
                }
                repeat_written = Some(name.as_str());
                mods.repeat = Some(if name.as_str() == "repeat" {
                    repeat_mode(&arg.value)?
                } else if bool_literal(&arg.value, "loop")? {
                    RepeatMode::Infinite
                } else {
                    RepeatMode::Once
                });
            }
            "reverse" => mods.reverse = Some(bool_literal(&arg.value, "reverse")?),
            "delay" => mods.delay = Some(&arg.value),
            "duration" => mods.duration_ms = Some(duration_literal(&arg.value, "duration")?),
            "from" => mods.from = Some(&arg.value),
            "restart" => mods.restart = Some(&arg.value),
            _ => rest.push(*arg),
        }
    }
    Ok((rest, mods))
}

/// Reads `repeat: 3` / `repeat: infinite` (RFC-0025 §4).
fn repeat_mode(expr: &Expr) -> Result<RepeatMode, CompileError> {
    match expr {
        Expr::Ident(sym, span) if sym.as_str() == "infinite" => {
            let _ = span;
            Ok(RepeatMode::Infinite)
        }
        Expr::IntLit(n, span) => u32::try_from(*n)
            .ok()
            .filter(|n| *n >= 1)
            .map(RepeatMode::Count)
            .ok_or_else(|| CompileError::InvalidAnimation {
                span: *span,
                message: "`repeat:` counts plays, so it must be at least 1 (or `infinite`)"
                    .to_string(),
            }),
        other => Err(CompileError::InvalidAnimation {
            span: other.span(),
            message: "`repeat:` takes a play count or `infinite`".to_string(),
        }),
    }
}

/// Reads a `true`/`false` modifier value (booleans are identifiers in `byld`).
fn bool_literal(expr: &Expr, name: &str) -> Result<bool, CompileError> {
    match expr {
        Expr::Ident(sym, _) if sym.as_str() == "true" => Ok(true),
        Expr::Ident(sym, _) if sym.as_str() == "false" => Ok(false),
        other => Err(CompileError::InvalidAnimation {
            span: other.span(),
            message: format!("`{name}:` takes `true` or `false`"),
        }),
    }
}

/// Reads a duration literal as whole milliseconds (`200ms`, `1.5s`, or a bare
/// integer count of ms), rejecting a fractional or out-of-range value rather
/// than silently truncating it.
fn duration_literal(expr: &Expr, what: &str) -> Result<u32, CompileError> {
    match expr {
        Expr::IntLit(ms, span) => u32::try_from(*ms).map_err(|_| CompileError::InvalidAnimation {
            span: *span,
            message: format!("`{what}` must be between 0 and {} milliseconds", u32::MAX),
        }),
        // `200.5` folds to a `FloatLit`; a duration must be whole ms.
        Expr::FloatLit(_, span) => Err(CompileError::InvalidAnimation {
            span: *span,
            message: format!("`{what}` must be a whole number of milliseconds"),
        }),
        other => Err(CompileError::InvalidAnimation {
            span: other.span(),
            message: format!("`{what}` must be a duration literal, e.g. `200ms` or `1.5s`"),
        }),
    }
}

/// Resolves one named curve from its own (non-modifier) arguments. A
/// fixed-duration curve accepts its duration positionally (`anim.linear(800ms)`)
/// or through the `duration:` modifier (`anim.linear(duration: 800ms)`).
fn resolve_named_curve(
    name: &Symbol,
    name_span: Span,
    args: &[&Arg],
    duration_ms: Option<u32>,
) -> Result<Curve, CompileError> {
    let ease = |kind| -> Result<Curve, CompileError> {
        Ok(Curve::Ease {
            ms: single_duration(args, name_span, name.as_str(), duration_ms)?,
            kind,
        })
    };
    match name.as_str() {
        "spring" => resolve_spring(args, name_span),
        "linear" => Ok(Curve::Linear {
            ms: single_duration(args, name_span, "linear", duration_ms)?,
        }),
        "ease" | "ease_in_out" => ease(EaseKind::InOut),
        "ease_in" => ease(EaseKind::In),
        "ease_out" => ease(EaseKind::Out),
        other => Err(CompileError::UnknownAnimation {
            span: name_span,
            name: other.to_string(),
            hint: closest_match(other, CURVE_NAMES.iter().copied()).map(str::to_string),
        }),
    }
}

/// Splits `anim.<name>(<args>)` (or the bare `anim.<name>`) into its curve
/// name, that name's span, and the argument list. With `require_namespace`
/// cleared, the bare `spring()` form is accepted too, the shape
/// `anim.stagger(spring(), …)` uses, where the namespace is already
/// established. Anything else is a [`CompileError::InvalidAnimation`].
fn destructure_anim_call(
    anim: &Expr,
    require_namespace: bool,
) -> Result<(Symbol, Span, Vec<&Arg>), CompileError> {
    fn borrowed(args: &[Arg]) -> Vec<&Arg> {
        args.iter().collect()
    }
    match anim {
        // `anim.spring(...)` / `spring(...)`, a call.
        Expr::Call { callee, args, span } => match callee.as_ref() {
            Expr::Member { base, field, span } if is_anim_base(base) => {
                Ok((field.clone(), *span, borrowed(args)))
            }
            Expr::Ident(name, span) if !require_namespace => {
                Ok((name.clone(), *span, borrowed(args)))
            }
            _ => Err(not_a_curve(*span)),
        },
        // `anim.spring` / `spring`, the bare form, all defaults.
        Expr::Member { base, field, span } if is_anim_base(base) => {
            Ok((field.clone(), *span, Vec::new()))
        }
        Expr::Ident(name, span) if !require_namespace => Ok((name.clone(), *span, Vec::new())),
        other => Err(not_a_curve(other.span())),
    }
}

/// The "this isn't a curve" diagnostic, shared by every shape that fails to
/// destructure.
fn not_a_curve(span: Span) -> CompileError {
    CompileError::InvalidAnimation {
        span,
        message: "expected an animation curve, e.g. `anim.spring(...)` or `anim.linear(200ms)`"
            .to_string(),
    }
}

/// Whether `base` is the `anim` namespace identifier.
fn is_anim_base(base: &Expr) -> bool {
    matches!(base, Expr::Ident(sym, _) if sym.as_str() == "anim")
}

/// Resolves `anim.spring(stiffness: …, damping: …, initial_velocity: …)`. All
/// three are optional named arguments; omitted ones take the A2 defaults.
fn resolve_spring(args: &[&Arg], call_span: Span) -> Result<Curve, CompileError> {
    let Curve::Spring {
        mut stiffness,
        mut damping,
        mut v0,
    } = Curve::DEFAULT_SPRING
    else {
        unreachable!("DEFAULT_SPRING is a Spring")
    };
    for arg in args {
        let Some(name) = &arg.name else {
            return Err(CompileError::InvalidAnimation {
                span: arg.value.span(),
                message: "`anim.spring` takes named arguments \
                          (stiffness / damping / initial_velocity)"
                    .to_string(),
            });
        };
        // The name is validated *before* its value, so a misspelt modifier
        // reports "did you mean `reverse`?" rather than complaining that `true`
        // is not a number.
        match name.as_str() {
            "stiffness" => stiffness = literal_f32(&arg.value)?,
            "damping" => damping = literal_f32(&arg.value)?,
            "initial_velocity" => v0 = literal_f32(&arg.value)?,
            other => {
                // Modifier names are candidates too, so a misspelt `revrse:`
                // on a spring is caught here rather than read as a parameter.
                let candidates = ["stiffness", "damping", "initial_velocity"]
                    .into_iter()
                    .chain(MODIFIER_NAMES.iter().copied());
                let hint = closest_match(other, candidates)
                    .map_or_else(String::new, |h| format!(" (did you mean `{h}`?)"));
                return Err(CompileError::InvalidAnimation {
                    span: arg.value.span(),
                    message: format!("unknown `anim.spring` argument `{other}`{hint}"),
                });
            }
        }
    }
    let _ = call_span;
    Ok(Curve::Spring {
        stiffness,
        damping,
        v0,
    })
}

/// Extracts the single positional duration (whole milliseconds) from
/// `anim.linear`/`ease`. A duration is a non-negative *integer* count of
/// milliseconds, a fractional value (`200.5`) is rejected rather than silently
/// truncated, and a value beyond `u32` is a range error.
fn single_duration(
    args: &[&Arg],
    call_span: Span,
    curve: &str,
    modifier_ms: Option<u32>,
) -> Result<u32, CompileError> {
    // `duration:` may supply it instead of the positional slot.
    if args.is_empty() {
        if let Some(ms) = modifier_ms {
            return Ok(ms);
        }
    }
    let [arg] = args else {
        return Err(CompileError::InvalidAnimation {
            span: call_span,
            message: format!(
                "`anim.{curve}` takes exactly one duration, e.g. `anim.{curve}(200ms)`"
            ),
        });
    };
    if let Some(name) = &arg.name {
        let hint = closest_match(name.as_str(), MODIFIER_NAMES.iter().copied())
            .map_or_else(String::new, |h| format!(" (did you mean `{h}`?)"));
        return Err(CompileError::InvalidAnimation {
            span: arg.value.span(),
            message: format!(
                "`anim.{curve}` has no `{name}` argument{hint}, its duration is positional,                  e.g. `anim.{curve}(200ms)`"
            ),
        });
    }
    match &arg.value {
        Expr::IntLit(ms, span) => u32::try_from(*ms).map_err(|_| CompileError::InvalidAnimation {
            span: *span,
            message: format!(
                "`anim.{curve}` duration must be between 0 and {} milliseconds",
                u32::MAX
            ),
        }),
        // `200.5` folds to a `FloatLit`; a duration must be whole ms.
        Expr::FloatLit(_, span) => Err(CompileError::InvalidAnimation {
            span: *span,
            message: format!("`anim.{curve}` duration must be a whole number of milliseconds"),
        }),
        other => Err(CompileError::InvalidAnimation {
            span: other.span(),
            message: format!("`anim.{curve}` duration must be a millisecond literal, e.g. `200ms`"),
        }),
    }
}

/// Reads a compile-time numeric literal (`IntLit`/`FloatLit`) as `f32`. Curve
/// parameters are constants, so a non-literal (e.g. a `var`) is rejected.
fn literal_f32(expr: &Expr) -> Result<f32, CompileError> {
    match expr {
        #[allow(clippy::cast_precision_loss)]
        Expr::IntLit(n, _) => Ok(*n as f32),
        #[allow(clippy::cast_possible_truncation)]
        Expr::FloatLit(f, _) => Ok(*f as f32),
        other => Err(CompileError::InvalidAnimation {
            span: other.span(),
            message: "animation arguments must be numeric literals".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ast::{AttrKind, Member};
    use crate::parser::parse;

    /// Parses `scale: 0 with <anim>` and resolves the curve, so tests can write
    /// the curve in surface syntax rather than hand-building an `Expr`.
    fn curve_of(anim_src: &str) -> Result<Curve, CompileError> {
        let src = format!("View V() {{ Box #[scale: 0 with {anim_src}] {{}} }}");
        let parsed = parse(&src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let Member::Element(el) = &parsed.views[0].body[0] else {
            panic!("expected an element");
        };
        let AttrKind::Prop {
            value: Expr::Animated { anim, .. },
        } = &el.attrs[0].kind
        else {
            panic!("expected an animated attribute");
        };
        resolve_curve(anim)
    }

    /// Parses `scale: 0 with <anim>` and hands the resolved [`MotionSpec`] to
    /// `check`. The spec borrows the AST, so the assertions run inside the
    /// closure rather than travelling out of it.
    fn with_spec<R>(anim_src: &str, check: impl FnOnce(Result<MotionSpec<'_>, CompileError>) -> R) {
        let src = format!("View V() {{ Box #[scale: 0 with {anim_src}] {{}} }}");
        let parsed = parse(&src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let Member::Element(el) = &parsed.views[0].body[0] else {
            panic!("expected an element");
        };
        let AttrKind::Prop {
            value: Expr::Animated { anim, .. },
        } = &el.attrs[0].kind
        else {
            panic!("expected an animated attribute");
        };
        check(resolve_motion(anim));
    }

    /// Parses `translate: <value>` and hands the resolved keyframe track (if the
    /// value is an `anim.keyframes(…)` call at all) to `check`.
    fn with_keyframes<R>(
        value_src: &str,
        check: impl FnOnce(Option<Result<KeyframeTrack<'_>, CompileError>>) -> R,
    ) {
        let src = format!("View V() {{ Box #[translate: {value_src}] {{}} }}");
        let parsed = parse(&src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let Member::Element(el) = &parsed.views[0].body[0] else {
            panic!("expected an element");
        };
        let AttrKind::Prop { value } = &el.attrs[0].kind else {
            panic!("expected a property");
        };
        check(resolve_keyframes(value));
    }

    #[test]
    fn bare_spring_uses_the_default_constants() {
        assert_eq!(curve_of("anim.spring()").unwrap(), Curve::DEFAULT_SPRING);
        // The parenless member form resolves identically.
        assert_eq!(curve_of("anim.spring").unwrap(), Curve::DEFAULT_SPRING);
    }

    #[test]
    fn spring_named_args_override_only_what_is_given() {
        assert_eq!(
            curve_of("anim.spring(stiffness: 300, damping: 25)").unwrap(),
            Curve::Spring {
                stiffness: 300.0,
                damping: 25.0,
                v0: 0.0,
            }
        );
    }

    #[test]
    fn linear_and_ease_take_a_duration_in_ms() {
        assert_eq!(
            curve_of("anim.linear(200ms)").unwrap(),
            Curve::Linear { ms: 200 }
        );
        // A bare integer is accepted as milliseconds too.
        assert_eq!(
            curve_of("anim.ease(150)").unwrap(),
            Curve::Ease {
                ms: 150,
                kind: EaseKind::InOut,
            }
        );
    }

    #[test]
    fn unknown_curve_suggests_the_closest_name() {
        let err = curve_of("anim.sprng()").unwrap_err();
        assert!(matches!(
            err,
            CompileError::UnknownAnimation { hint: Some(h), .. } if h == "spring"
        ));
    }

    #[test]
    fn spring_rejects_an_unknown_named_argument() {
        assert!(matches!(
            curve_of("anim.spring(stifness: 300)").unwrap_err(),
            CompileError::InvalidAnimation { .. }
        ));
    }

    #[test]
    fn linear_without_a_duration_is_an_error() {
        assert!(matches!(
            curve_of("anim.linear()").unwrap_err(),
            CompileError::InvalidAnimation { .. }
        ));
    }

    #[test]
    fn fractional_duration_is_rejected_not_truncated() {
        // `200.5` must be a hard error, not silently floored to 200ms.
        assert!(matches!(
            curve_of("anim.linear(200.5)").unwrap_err(),
            CompileError::InvalidAnimation { .. }
        ));
    }

    #[test]
    fn negative_duration_is_a_range_error() {
        assert!(matches!(
            curve_of("anim.ease(-5)").unwrap_err(),
            CompileError::InvalidAnimation { .. }
        ));
    }

    // ── RFC-0025: repeat / reverse / delay / from / keyframes / stagger ──

    #[test]
    fn a_plain_curve_resolves_to_the_unrepeated_spec() {
        with_spec("anim.spring()", |spec| {
            let spec = spec.unwrap();
            assert_eq!(spec.repeat, RepeatMode::Once);
            assert!(!spec.reverse && spec.delay.is_none() && spec.from.is_none());
            // A spec with no modifiers takes the original single-shot path.
            assert!(spec.is_plain());
        });
    }

    #[test]
    fn repeat_and_reverse_modifiers_resolve() {
        with_spec("anim.spring(repeat: infinite, reverse: true)", |spec| {
            let spec = spec.unwrap();
            assert_eq!(spec.repeat, RepeatMode::Infinite);
            assert!(spec.reverse);
            assert!(!spec.is_plain(), "a looping spec needs the repeat clock");
        });
        with_spec("anim.linear(800ms, repeat: 3)", |spec| {
            let spec = spec.unwrap();
            assert_eq!(spec.repeat, RepeatMode::Count(3));
            assert_eq!(spec.curve, Curve::Linear { ms: 800 });
        });
        // `loop: true` is sugar for `repeat: infinite`; `loop: false` is a
        // single play.
        with_spec("anim.linear(1s, loop: true)", |spec| {
            assert_eq!(spec.unwrap().repeat, RepeatMode::Infinite);
        });
        with_spec("anim.linear(1s, loop: false)", |spec| {
            assert_eq!(spec.unwrap().repeat, RepeatMode::Once);
        });
    }

    #[test]
    fn a_modifier_may_supply_the_duration_and_the_explicit_start() {
        with_spec("anim.ease_out(duration: 400ms, from: 0.4)", |spec| {
            let spec = spec.unwrap();
            assert_eq!(
                spec.curve,
                Curve::Ease {
                    ms: 400,
                    kind: EaseKind::Out
                }
            );
            assert!(matches!(spec.from, Some(Expr::FloatLit(..))));
        });
        // The three ease families are addressable by name (RFC-0025 §4).
        with_spec("anim.ease_in(200ms)", |spec| {
            assert!(matches!(
                spec.unwrap().curve,
                Curve::Ease {
                    kind: EaseKind::In,
                    ..
                }
            ));
        });
    }

    #[test]
    fn a_delay_is_kept_as_an_expression_and_is_cancellable() {
        // `delay: i * 50ms` must survive as an expression: the index is only
        // known per item, at sample time.
        with_spec("anim.spring(delay: i * 50ms)", |spec| {
            let spec = spec.unwrap();
            assert!(matches!(spec.delay, Delay::Offset(Expr::Binary { .. })));
            assert!(spec.delay.is_cancellable(), "a retarget cancels `delay:`");
        });
    }

    #[test]
    fn stagger_is_sugar_for_a_non_cancellable_indexed_delay() {
        with_spec("anim.stagger(spring(), 50ms, i)", |spec| {
            let spec = spec.unwrap();
            assert_eq!(spec.curve, Curve::DEFAULT_SPRING);
            let Delay::Stagger { step_ms, index } = spec.delay else {
                panic!("expected a stagger delay, got {:?}", spec.delay);
            };
            assert_eq!(step_ms, 50);
            assert!(matches!(index, Expr::Ident(..)));
            // An entrance cascade is intentional sequencing (§5).
            assert!(!spec.delay.is_cancellable());
        });
        // Named arguments address the same three slots, in any order.
        with_spec(
            "anim.stagger(step: 30ms, index: i, base: linear(120ms))",
            |spec| {
                let spec = spec.unwrap();
                assert_eq!(spec.curve, Curve::Linear { ms: 120 });
                assert!(matches!(spec.delay, Delay::Stagger { step_ms: 30, .. }));
            },
        );
        with_spec("anim.stagger(spring())", |spec| {
            assert!(matches!(
                spec.unwrap_err(),
                CompileError::InvalidAnimation { .. }
            ));
        });
    }

    #[test]
    fn a_restart_witness_resolves_on_curves_keyframes_and_staggers() {
        with_spec("anim.spring(restart: attempt)", |spec| {
            let spec = spec.unwrap();
            assert!(matches!(spec.restart, Some(Expr::Ident(..))));
            assert!(
                !spec.is_plain(),
                "a replayable animation needs its own timeline"
            );
        });
        // On a stagger the witness belongs to the *cascade*, so it may sit at the
        // stagger's own level, past the three positional arguments.
        with_spec(
            "anim.stagger(spring(), 90ms, i, restart: attempt)",
            |spec| {
                let spec = spec.unwrap();
                assert!(matches!(spec.restart, Some(Expr::Ident(..))));
                assert!(matches!(spec.delay, Delay::Stagger { step_ms: 90, .. }));
            },
        );
        with_keyframes(
            "anim.keyframes(0%: 0, 100%: 1, duration: 1s, restart: page)",
            |track| {
                let track = track.unwrap().unwrap();
                assert!(matches!(track.restart, Some(Expr::Ident(..))));
            },
        );
    }

    #[test]
    fn keyframes_after_with_points_at_value_position() {
        // Keyframes carry their own values, so `x with anim.keyframes(…)` has
        // two sources of truth, rejected with the fix in the message.
        with_spec("anim.keyframes(0%: 0, 100%: 1, duration: 1s)", |spec| {
            let CompileError::InvalidAnimation { message, .. } = spec.unwrap_err() else {
                panic!("expected an InvalidAnimation");
            };
            assert!(message.contains("property value"), "{message}");
        });
    }

    #[test]
    fn a_keyframe_track_resolves_its_steps_easings_and_modifiers() {
        with_keyframes(
            "anim.keyframes(0%: 0, 50%: 200 ease_out, 100%: 0, duration: 2s, loop: true)",
            |track| {
                let track = track.expect("recognized as keyframes").unwrap();
                assert_eq!(track.duration_ms, 2000);
                assert_eq!(track.repeat, RepeatMode::Infinite);
                let shape: Vec<_> = track.steps.iter().map(|s| (s.percent, s.easing)).collect();
                assert_eq!(
                    shape,
                    vec![
                        (0.0, MotionCurve::LINEAR),
                        (0.5, MotionCurve::EASE_OUT),
                        (1.0, MotionCurve::LINEAR),
                    ]
                );
            },
        );
        // An ordinary value is not a keyframe track.
        with_keyframes("(4, 2)", |track| assert!(track.is_none()));
        with_keyframes("anim.spring()", |track| assert!(track.is_none()));
    }

    #[test]
    fn a_keyframe_sequence_is_validated() {
        let err = |src: &str| {
            let mut out = None;
            with_keyframes(src, |track| {
                out = Some(track.expect("recognized as keyframes").unwrap_err());
            });
            out.unwrap()
        };
        // The RFC's step cap (9 steps > 8).
        let nine = (0..9)
            .map(|i| format!("{}%: {i}", i * 10))
            .collect::<Vec<_>>()
            .join(", ");
        assert!(matches!(
            err(&format!("anim.keyframes({nine}, duration: 1s)")),
            CompileError::TooManyKeyframes { found: 9, .. }
        ));
        // Every other malformed sequence is an InvalidAnimation with a reason.
        let message = |src: &str| match err(src) {
            CompileError::InvalidAnimation { message, .. } => message,
            other => panic!("expected InvalidAnimation, got {other:?}"),
        };
        assert!(message("anim.keyframes(0%: 0, duration: 1s)").contains("at least two steps"));
        assert!(message("anim.keyframes(0%: 0, 100%: 1)").contains("duration"));
        assert!(
            message("anim.keyframes(50%: 1, 20%: 2, duration: 1s)").contains("ascending"),
            "steps must be ordered"
        );
        assert!(message("anim.keyframes(0%: 0, 140%: 1, duration: 1s)").contains("0% and 100%"));
        assert!(message("anim.keyframes(0%: 0, 100%: 1 bouncy, duration: 1s)").contains("easing"));
        assert!(
            message("anim.keyframes(0%: 0, 100%: 1, duration: 0ms)").contains("non-zero"),
            "a zero duration has nothing to interpolate over"
        );
        assert!(message("anim.keyframes(0%: 0, 100%: 1, duration: 1s, from: 3)").contains("`0%`"));
        assert!(
            message("anim.keyframes(1, 2, duration: 1s)").contains("timed steps"),
            "plain arguments are not steps"
        );
    }

    #[test]
    fn malformed_modifiers_are_rejected_with_their_own_message() {
        let message = |src: &str| {
            let mut out = String::new();
            with_spec(src, |spec| match spec.unwrap_err() {
                CompileError::InvalidAnimation { message, .. } => out = message,
                other => panic!("expected InvalidAnimation, got {other:?}"),
            });
            out
        };
        assert!(message("anim.spring(repeat: 0)").contains("at least 1"));
        assert!(message("anim.spring(repeat: soon)").contains("play count"));
        assert!(message("anim.spring(reverse: 1)").contains("`true` or `false`"));
        assert!(message("anim.linear(200ms, loop: true, repeat: 2)").contains("one or the other"));
        assert!(
            message("anim.spring(revrse: true)").contains("did you mean `reverse`"),
            "a misspelt modifier is caught, not read as a spring parameter"
        );
        assert!(
            message("anim.linear(rpeat: 3)").contains("did you mean `repeat`"),
            "…on a fixed-duration curve too"
        );
    }
}
