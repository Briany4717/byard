//! Animated values: `with`, keyframes and loops (RFC-0010, RFC-0025).
//!
//! A value written `target with anim.*(…)` is evaluated on the interpreter,
//! not lowered to a closure, because the motion it starts has to outlive the
//! closure that asked for it. Each animated node keeps its `Motion` in a map
//! keyed by its source span and the element instance running it (`AnimKey`),
//! retargets it when the endpoint moves, and drops it when the branch it sits
//! in goes away. Here are those evaluators, the loop clocks and keyframe
//! blending, and the colour mixing (in OKLab) that animated colours and the
//! theme cross-fade share.

use super::Interpreter;
use super::layout::collect_pools;
use super::props::spacing_value;
use crate::diagnostics::Span;
use crate::interp::env::Value;
use crate::parser::ast::Expr;
use crate::symbol::Symbol;

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
pub(super) struct LoopClock {
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

/// A fingerprint of a repeating animation's endpoints, used to notice a
/// retarget (RFC-0025 §5).
///
/// Repeating animations recompute their endpoints every frame, so nothing needs
/// to be stored to *sample* them, only enough to answer "are these the same
/// endpoints as last frame?". A hash of the raw bits answers exactly that, for
/// one scalar or four colour channels alike.
fn endpoint_key(motions: &[byard_core::frame::Motion]) -> u64 {
    use std::hash::Hasher;
    // Runs for every repeating animation on every frame, and only has to tell
    // this frame's endpoints from last frame's, so a fast hash is enough.
    let mut hasher = rustc_hash::FxHasher::default();
    for motion in motions {
        hasher.write_u64(u64::from(motion.from.to_bits()) << 32 | u64::from(motion.to.to_bits()));
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

/// Converts a packed `0xRRGGBB` colour to OKLab `[L, a, b]` for perceptually
/// uniform interpolation (RFC-0010 A3).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)] // standard colour-space notation
pub(super) fn oklab_from_hex(hex: i64) -> [f32; 3] {
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
pub(super) fn hex_from_oklab(lab: [f32; 3]) -> i64 {
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
pub(super) fn mix_hex_oklab(a: i64, b: i64, t: f32) -> i64 {
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
pub(super) fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Packs the compiler's typed [`Curve`](crate::interp::anim::Curve) into the
/// engine's POD [`MotionCurve`](byard_core::frame::MotionCurve) (RFC-0010), so
/// a resolved curve crosses the frame boundary as plain data.
pub(super) fn pack_curve(curve: crate::interp::anim::Curve) -> byard_core::frame::MotionCurve {
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

impl Interpreter {
    /// Samples a keyframed colour sequence (RFC-0025 §3 × RFC-0010 A3): the two
    /// steps surrounding "now", mixed in OKLab so the sweep has no muddy
    /// mid-points. Returns `None` only if the sequence is malformed (already
    /// reported) or its steps are not colours.
    pub(super) fn eval_keyframe_color(&mut self, expr: &Expr) -> Option<i64> {
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
    pub(super) fn eval_animated_color(&mut self, target: &Expr, anim: &Expr, key: AnimKey) -> i64 {
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

    /// Drives one `with` animation (RFC-0010): resolves the target and curve,
    /// advances (or seeds) the persisted [`Motion`](byard_core::frame::Motion)
    /// keyed by `key`, and returns the value sampled at the current engine time.
    /// A target change reseeds `from` to the current on-screen value so a
    /// mid-flight reversal is continuous.
    pub(super) fn eval_animated(&mut self, target: &Expr, anim: &Expr, key: AnimKey) -> Value {
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
        let target_val = match target_value {
            #[allow(clippy::cast_possible_truncation)]
            Value::Float(f) => f as f32,
            #[allow(clippy::cast_precision_loss)]
            Value::Int(n) => n as f32,
            // A coordinate pair animates component-wise off one shared clock, so
            // `translate: (0, 0) with anim.spring(delay: i * 50ms)` (RFC-0025's
            // stagger shape) moves as one. Only the RFC-0025 paths handle a pair;
            // a plain `with` keeps its historical pass-through.
            Value::Tuple(items) if !spec.is_plain() => {
                return self.eval_looped_pair(items, &spec, key);
            }
            // Anything else can't be interpolated, pass it through untouched
            // (the checker already restricts `with` to numeric props).
            other => return other,
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
        mut items: Vec<(Option<Symbol>, Value)>,
        spec: &crate::interp::anim::MotionSpec<'_>,
        key: AnimKey,
    ) -> Value {
        // A non-numeric component can't be interpolated; pass the pair through
        // as written.
        if items.iter().any(|(_, v)| spacing_value(v).is_none()) {
            return Value::Tuple(items);
        }
        let from_value = spec.from.map(|expr| self.eval_pure(expr));
        let curve = pack_curve(spec.curve);
        let now = self.now_ms;
        // A pair has two components, so this only reaches the heap for a wider
        // tuple.
        let motions: smallvec::SmallVec<[byard_core::frame::Motion; 2]> = items
            .iter()
            .enumerate()
            .map(|(axis, (_, v))| {
                let to = spacing_value(v).unwrap_or_default();
                let from = match &from_value {
                    Some(Value::Tuple(from_items)) => from_items
                        .get(axis)
                        .and_then(|(_, v)| spacing_value(v))
                        .unwrap_or(to),
                    Some(scalar) => spacing_value(scalar).unwrap_or(to),
                    None => to,
                };
                byard_core::frame::Motion {
                    from,
                    to,
                    start_ms: now,
                    curve,
                }
            })
            .collect();
        let phase = self.loop_at(&motions, spec, key);
        // The sampled pair keeps the target's field names, so it is written
        // over the target's own components rather than into a fresh tuple.
        for ((_, value), motion) in items.iter_mut().zip(&motions) {
            let sampled = match phase {
                Some(t_secs) => motion.sample_secs(t_secs),
                None => motion.from,
            };
            *value = Value::Float(f64::from(sampled));
        }
        Value::Tuple(items)
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
    pub(super) fn eval_keyframes(&mut self, expr: &Expr) -> Value {
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

    /// Forgets the animation state of the rows `from..to` of `pool`, and of
    /// everything nested inside them (RFC-0025).
    ///
    /// A nested `for` is lowered once per *outer* row, so the pools inside a
    /// vanished row belong to that row alone: walking its lowered nodes for
    /// them, and dropping their slots too, is exact rather than approximate.
    /// Doing it any other way would either leave a nested row's timeline behind
    /// (it resumes mid-flight under whatever takes its place) or drop a live
    /// sibling's (it restarts for no reason the author can see).
    pub(super) fn drop_slot_state(&mut self, pool: usize, from: usize, to: usize) {
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
    pub(super) const fn anim_key(&self, span: Span) -> AnimKey {
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
    pub(super) fn drop_animation_state(&mut self, range: Span) {
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
}
