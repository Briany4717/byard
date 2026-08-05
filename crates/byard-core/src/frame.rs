//! Shared data types for cross-subsystem communication.
//!
//! This module defines [`RenderFrame`] and [`TargetId`], the primitive types
//! that flow between the evaluator, atlas, encoder, and relay subsystems.
//! It is the **only** module that all subsystems may depend on.
//!
//! ```text
//! encoder  ──┐
//! atlas    ──┤─→  frame  ←─  relay
//! evaluator ─┘
//! ```
//!
//! Adding a dependency from one subsystem to another (e.g. `encoder` importing
//! from `evaluator`) is a design defect. If data needs to cross that boundary,
//! it must be modelled as a type in this module.

/// An opaque, copyable identifier for a dirty-flag target.
///
/// Internally packs three fields into a single 64-bit word:
///
/// - bits 0–31, `index`, the position inside the owning subsystem's table
/// - bits 32–47, `generation`, a monotonic counter that lets stale IDs be
///   detected when the underlying slot is reused
/// - bits 48–63, `kind`, a discriminant identifying which subsystem owns
///   the target (atlas node, encoder primitive, …)
///
/// The internal representation is private; consumers must use [`TargetId::new`]
/// to construct an ID and the accessor methods to read its parts.
///
/// Lives in `frame` rather than any subsystem module so all subsystems may
/// reference it without violating the dependency graph in RFC-0001 §9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetId(u64);

/// Discriminant identifying which subsystem owns a [`TargetId`].
///
/// Stored in the high 16 bits of every `TargetId` so subsystems can filter
/// the broadcast `mark_dirty_all` calls down to their own targets without
/// coordination.
///
/// `#[repr(u16)]` guarantees the in-memory representation matches the
/// `TargetId` bit layout, so `TargetKind::Foo as u16` is a zero-cost cast.
#[repr(u16)]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A layout node owned by `LayoutAtlas`.
    AtlasNode = 1,
    /// A render primitive owned by an Encoder pipeline (`SolidBox`,
    /// `TextGlyph`, …), addressed by its position in the `RenderFrame`.
    EncoderPrimitive = 2,
}

impl TargetId {
    /// Constructs a `TargetId` from its three components.
    ///
    /// The `index`, `generation`, and `kind` are packed into a single
    /// 64-bit word, see the [`TargetId`] type documentation for the
    /// bit layout.
    #[must_use]
    pub const fn new(index: u32, generation: u16, kind: u16) -> Self {
        let raw = (index as u64) | ((generation as u64) << 32) | ((kind as u64) << 48);
        Self(raw)
    }

    /// Returns the index part of the ID.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn index(self) -> u32 {
        // Truncation is intentional: we mask to the low 32 bits.
        (self.0 & 0xFFFF_FFFF) as u32
    }

    /// Returns the generation part of the ID.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn generation(self) -> u16 {
        // Truncation is intentional: we mask to bits 32-47.
        ((self.0 >> 32) & 0xFFFF) as u16
    }

    /// Returns the kind part of the ID.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn kind(self) -> u16 {
        // Truncation is intentional: we mask to the high 16 bits.
        ((self.0 >> 48) & 0xFFFF) as u16
    }

    /// Returns the raw 64-bit representation of the ID.
    #[must_use]
    pub const fn as_raw(self) -> u64 {
        self.0
    }
}

/// An axis-aligned rectangle in logical pixel coordinates.
///
/// Produced by the Atlas as the resolved position and size of a node,
/// consumed by the Encoder to issue draw commands. Lives in `frame`
/// because it crosses the subsystem boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    /// Top-left X coordinate in logical pixels.
    pub x: f32,
    /// Top-left Y coordinate in logical pixels.
    pub y: f32,
    /// Width in logical pixels.
    pub width: f32,
    /// Height in logical pixels.
    pub height: f32,
}

impl Rect {
    /// Constructs a new rectangle.
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if the rectangle contains the given point.
    ///
    /// Uses half-open bounds: the left (`x`) and top (`y`) edges are
    /// **inclusive**, while the right (`x + width`) and bottom
    /// (`y + height`) edges are **exclusive**. This matches the convention
    /// used by the spatial hash grid (sub-issue pending) and avoids
    /// off-by-one disagreements during hit-testing.
    #[must_use]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }

    /// Returns the smallest rectangle that fully covers both `self` and
    /// `other`.
    ///
    /// Used by the Encoder (RFC-0001 §3.3) to merge several dirty-region
    /// bounding boxes into the single bounding box passed to
    /// `wgpu::RenderPass::set_scissor_rect`. Degenerate (zero-area) rects are
    /// handled the same as any other rect: the union still expands to cover
    /// their `(x, y)` corner.
    #[must_use]
    pub fn union(&self, other: &Rect) -> Rect {
        let min_x = self.x.min(other.x);
        let min_y = self.y.min(other.y);
        let max_x = (self.x + self.width).max(other.x + other.width);
        let max_y = (self.y + self.height).max(other.y + other.height);
        Rect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        }
    }
}

/// A paint-time affine transform (RFC-0011): translate/scale/rotate about a
/// pivot, plus an opacity multiplier. Applied in the vertex/fragment shader
/// *after* Taffy has placed the element, layout geometry and hit-testing
/// rects are never affected, and Taffy is never re-run because a transform
/// changed (INV-8). The identity value is a free no-op in the shader.
///
/// Deliberately a decomposed TRS (not a baked matrix): smaller to upload,
/// trivial to interpolate per-component (RFC-0010's GPU springs animate one
/// field at a time), and legible in a debugger.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Transform {
    /// Logical-pixel offset applied after layout placement; siblings never move.
    pub translate: [f32; 2],
    /// Per-axis scale about `origin`; `[1.0, 1.0]` is unscaled.
    pub scale: [f32; 2],
    /// Rotation about `origin`, in radians.
    pub rotate: f32,
    /// The pivot for `scale`/`rotate`, resolved at lower time into the same
    /// absolute logical-pixel space as the element's laid-out rectangle
    /// (e.g. `center` resolves to the rect's own midpoint).
    pub origin: [f32; 2],
    /// Element alpha multiplier, `0.0..=1.0`.
    pub opacity: f32,
}

impl Transform {
    /// The no-op transform: no offset, unit scale, no rotation, full opacity.
    pub const IDENTITY: Transform = Transform {
        translate: [0.0, 0.0],
        scale: [1.0, 1.0],
        rotate: 0.0,
        origin: [0.0, 0.0],
        opacity: 1.0,
    };

    /// Whether this transform is a no-op (bit-exact match against `IDENTITY`
    ///, every field is set from either a literal default or an exact
    /// user-authored value, never accumulated arithmetic, so exact float
    /// comparison is safe here).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// Maps a point (in the same absolute logical-pixel space as `origin`)
    /// through this transform: scale and rotate it about `origin`, then translate.
    /// Used to propagate an ancestor transform onto a descendant's laid-out
    /// geometry (RFC-0011 group transforms).
    #[must_use]
    pub fn apply_point(&self, p: [f32; 2]) -> [f32; 2] {
        let dx = (p[0] - self.origin[0]) * self.scale[0];
        let dy = (p[1] - self.origin[1]) * self.scale[1];
        let (sin, cos) = self.rotate.sin_cos();
        [
            self.origin[0] + dx * cos - dy * sin + self.translate[0],
            self.origin[1] + dx * sin + dy * cos + self.translate[1],
        ]
    }

    /// Maps a free vector (a displacement) through this transform: scale and
    /// rotate only, no pivot, no translation.
    #[must_use]
    fn apply_vec(&self, v: [f32; 2]) -> [f32; 2] {
        let dx = v[0] * self.scale[0];
        let dy = v[1] * self.scale[1];
        let (sin, cos) = self.rotate.sin_cos();
        [dx * cos - dy * sin, dx * sin + dy * cos]
    }

    /// The mean of the two scale axes, the single factor a scalar-only sink
    /// (text `font_size`, a uniform glyph scale) uses when an ancestor's group
    /// transform is non-uniform. Exact for the common uniform-scale case.
    #[must_use]
    pub fn uniform_scale(&self) -> f32 {
        f32::midpoint(self.scale[0], self.scale[1])
    }

    /// Composes `self` (the outer / ancestor transform) with `inner` (a
    /// descendant's own transform), yielding the single [`Transform`] that maps
    /// the descendant's laid-out geometry as if `inner` were applied first and
    /// `self` after it, the basis of RFC-0011 group transforms (a scaled or
    /// translated container carries its children, text, and widgets with it).
    ///
    /// Exact when the outer transform has no rotation (the common scale/translate
    /// inheritance) or when scales are uniform. A rotating, *non-uniformly* scaled
    /// ancestor is approximated: a general affine no longer decomposes to a single
    /// pivot-based TRS, so full fidelity there would need render-to-texture
    /// (RFC-0011 T4). Opacity multiplies.
    #[must_use]
    pub fn compose(&self, inner: &Transform) -> Transform {
        let moved_origin = self.apply_point(inner.origin);
        let moved_translate = self.apply_vec(inner.translate);
        Transform {
            translate: [
                moved_translate[0] + moved_origin[0] - inner.origin[0],
                moved_translate[1] + moved_origin[1] - inner.origin[1],
            ],
            scale: [
                self.scale[0] * inner.scale[0],
                self.scale[1] * inner.scale[1],
            ],
            rotate: self.rotate + inner.rotate,
            // Keep the descendant's own pivot (in absolute coords); the ancestor's
            // effect is folded into `translate`/`scale`/`rotate` above.
            origin: inner.origin,
            opacity: self.opacity * inner.opacity,
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A packed, POD animation curve (RFC-0010), a `u32` tag plus three `f32`
/// parameters, so it crosses the frame boundary as plain data and the engine
/// never needs to know the compiler's typed `Curve`. The compiler packs its
/// resolved curve into this at lower time; both the CPU (settling) and the GPU
/// (drawing) read the same closed forms from it.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MotionCurve {
    /// The curve family, one of the `MotionCurve::*` tag constants.
    pub kind: u32,
    /// Curve parameters, interpreted by `kind`:
    /// - linear / ease: `[duration_ms, _, _]`
    /// - spring: `[stiffness, damping, initial_velocity]`
    pub params: [f32; 3],
}

impl MotionCurve {
    /// Fixed-duration linear ramp; `params[0]` is the duration in ms.
    pub const LINEAR: u32 = 0;
    /// Ease-in (cubic); `params[0]` is the duration in ms.
    pub const EASE_IN: u32 = 1;
    /// Ease-out (cubic); `params[0]` is the duration in ms.
    pub const EASE_OUT: u32 = 2;
    /// Ease-in-out (cubic); `params[0]` is the duration in ms.
    pub const EASE_IN_OUT: u32 = 3;
    /// Damped spring; `params` is `[stiffness, damping, initial_velocity]`.
    pub const SPRING: u32 = 4;
}

/// A paint-time animatable scalar (RFC-0010 §"The animatable value model").
///
/// Carries only endpoints and a curve, **no per-frame CPU work**: the CPU
/// rewrites `to` (and reseeds `from`/`start_ms`) once, on a target change, and
/// the shader interpolates every active frame. The CPU also evaluates the same
/// closed forms ([`sample`](Self::sample)/[`velocity`](Self::velocity)) to
/// decide when a motion has [`settled`](Self::is_settled) so the app can stop
/// requesting frames. Times are absolute engine milliseconds.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Motion {
    /// The value at the moment `to` last changed (the interruption point).
    pub from: f32,
    /// The current target (rewritten `O(1)` on mutation).
    pub to: f32,
    /// Absolute engine time (ms) when `to` was set.
    pub start_ms: u32,
    /// The curve driving `from` → `to`.
    pub curve: MotionCurve,
}

impl Motion {
    /// Default settling threshold on position (RFC-0010 A4): below half a
    /// logical pixel a transition is imperceptible. This is a safe scalar
    /// default; per-property callers whose unit isn't pixels (opacity 0..1,
    /// a colour channel, degrees) should pass their own to
    /// [`is_settled_with_eps`](Self::is_settled_with_eps).
    pub const DEFAULT_EPS_POS: f32 = 0.5;
    /// Default settling threshold on velocity, in units per second (RFC-0010 A4).
    pub const DEFAULT_EPS_VEL: f32 = 0.5;

    /// A settled motion pinned at `value` (no movement) with a linear curve.
    #[must_use]
    pub fn resting(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start_ms: 0,
            curve: MotionCurve {
                kind: MotionCurve::LINEAR,
                params: [0.0, 0.0, 0.0],
            },
        }
    }

    /// The animated value at absolute engine time `now_ms`.
    #[must_use]
    pub fn sample(&self, now_ms: u32) -> f32 {
        let t = seconds_since(self.start_ms, now_ms);
        match self.curve.kind {
            MotionCurve::SPRING => spring_position(self, t),
            MotionCurve::LINEAR => self.from + (self.to - self.from) * duration_progress(self, t),
            _ => {
                let p = ease(self.curve.kind, duration_progress(self, t));
                self.from + (self.to - self.from) * p
            }
        }
    }

    /// The analytic velocity (units per second) at `now_ms`.
    #[must_use]
    pub fn velocity(&self, now_ms: u32) -> f32 {
        let t = seconds_since(self.start_ms, now_ms);
        if self.curve.kind == MotionCurve::SPRING {
            spring_velocity(self, t)
        } else {
            // Finite-difference the eased/linear ramp, cheap and only used for
            // settling, where a derivative-free estimate is plenty.
            const H: f32 = 1.0 / 240.0;
            (self.sample_secs(t + H) - self.sample_secs(t)) / H
        }
    }

    /// Position at an explicit elapsed time `t` seconds, independent of
    /// [`start_ms`](Self::start_ms).
    ///
    /// This is the entry point a *looping* animation samples through
    /// (RFC-0025): the repeat clock ([`loop_phase`]) reduces wall time to an
    /// offset inside one iteration, and that offset, not `now − start_ms`, is
    /// what the curve is evaluated at. Also used by the finite-difference
    /// velocity of the non-spring curves.
    #[must_use]
    pub fn sample_secs(&self, t: f32) -> f32 {
        match self.curve.kind {
            MotionCurve::SPRING => spring_position(self, t),
            MotionCurve::LINEAR => self.from + (self.to - self.from) * clamp01(progress(self, t)),
            _ => {
                self.from
                    + (self.to - self.from) * ease(self.curve.kind, clamp01(progress(self, t)))
            }
        }
    }

    /// Whether the motion has effectively reached rest, using the default
    /// per-pixel epsilons ([`DEFAULT_EPS_POS`](Self::DEFAULT_EPS_POS) /
    /// [`DEFAULT_EPS_VEL`](Self::DEFAULT_EPS_VEL)).
    #[must_use]
    pub fn is_settled(&self, now_ms: u32) -> bool {
        self.is_settled_with_eps(now_ms, Self::DEFAULT_EPS_POS, Self::DEFAULT_EPS_VEL)
    }

    /// Whether the motion has reached rest under caller-supplied thresholds,
    /// within `eps_pos` of `to` and moving slower than `eps_vel`. The runtime
    /// scales these to the animated property's unit (px vs. opacity vs. colour
    /// channel) so settling is neither too eager nor too lax.
    #[must_use]
    pub fn is_settled_with_eps(&self, now_ms: u32, eps_pos: f32, eps_vel: f32) -> bool {
        (self.sample(now_ms) - self.to).abs() < eps_pos && self.velocity(now_ms).abs() < eps_vel
    }

    /// Shortest period a repeating animation may wrap at (one 60 Hz frame).
    pub const MIN_PERIOD_MS: u32 = 16;
    /// Longest period a repeating animation may wrap at, the cap an undamped
    /// spring (which never settles) falls back to.
    pub const MAX_PERIOD_MS: u32 = 10_000;

    /// How long one play of this motion lasts, in whole milliseconds, the
    /// period a repeating animation wraps at (RFC-0025 §1).
    ///
    /// A fixed-duration curve simply reports its duration. A spring has no
    /// duration, so its period is the time it takes to come to rest within
    /// `eps_pos`: the closed forms all decay under an `e^{-ζωt}` envelope, so
    /// `|d|·e^{-ζωt} = eps` inverts to `t = ln(|d| / eps) / (ζω)`, a genuine
    /// closed form, no iteration. That is exactly the "restarts when it settles"
    /// rule of RFC-0025, evaluated ahead of time so every curve family repeats
    /// through the one integer-millisecond clock.
    ///
    /// Clamped to [`MIN_PERIOD_MS`](Self::MIN_PERIOD_MS)`..=`
    /// [`MAX_PERIOD_MS`](Self::MAX_PERIOD_MS): a zero period would leave a
    /// repeat with nothing to wrap, and an undamped (`damping: 0`) spring never
    /// settles at all.
    #[must_use]
    #[allow(clippy::many_single_char_names)]
    pub fn natural_duration_ms(&self, eps_pos: f32) -> u32 {
        let eps = eps_pos.max(f32::EPSILON);
        let ms = if self.curve.kind == MotionCurve::SPRING {
            let [k, c, _] = self.curve.params;
            let omega = k.max(0.0).sqrt();
            let zeta = if omega == 0.0 {
                0.0
            } else {
                c.max(0.0) / (2.0 * omega)
            };
            // The envelope's decay rate: `ζω` under- and critically damped, the
            // slower of the two real roots when overdamped.
            let rate = if zeta > 1.0 {
                zeta * omega - omega * (zeta * zeta - 1.0).sqrt()
            } else {
                zeta * omega
            };
            let d = (self.from - self.to).abs().max(eps);
            if rate <= 0.0 {
                f32::INFINITY
            } else {
                (d / eps).ln() / rate * 1000.0
            }
        } else {
            self.curve.params[0]
        };
        #[allow(clippy::cast_precision_loss)]
        let lo = Self::MIN_PERIOD_MS as f32;
        #[allow(clippy::cast_precision_loss)]
        let hi = Self::MAX_PERIOD_MS as f32;
        // NaN-safe: an unorderable duration falls back to the cap rather than
        // to whatever `clamp` would do with it.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        if ms.is_finite() {
            ms.clamp(lo, hi) as u32
        } else {
            Self::MAX_PERIOD_MS
        }
    }
}

/// How many times a looping animation plays (RFC-0025 §1: `repeat: N`,
/// `repeat: infinite`, the default single play).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    /// Play once and hold the final value, RFC-0010's original behaviour.
    #[default]
    Once,
    /// Play exactly `n` times (`n ≥ 1`), then hold the final value.
    Count(u32),
    /// Play forever, until the element unmounts or leaves the viewport.
    Infinite,
}

impl RepeatMode {
    /// Whether this mode plays more than once (so the animation needs the
    /// repeat clock rather than the plain `now − start` path).
    #[must_use]
    pub fn is_repeating(self) -> bool {
        !matches!(self, Self::Once)
    }
}

/// Where a repeating animation is within its current iteration (RFC-0025 §1),
/// the output of [`loop_phase`], and the only bridge between wall time and a
/// curve's own `0..duration` domain.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LoopPhase {
    /// Offset *inside the current iteration*, in seconds, already
    /// direction-corrected: on an alternating (`reverse`) odd iteration this
    /// counts back down from the period, so the curve replays back-to-front.
    pub t_secs: f32,
    /// Zero-based iteration index, clamped to the last one once `finished`.
    pub iteration: u32,
    /// True once a finite repeat has played out. The caller holds the final
    /// value and drops the animation from the active set, an
    /// [`Infinite`](RepeatMode::Infinite) animation never reports this, which is
    /// what keeps frames flowing.
    pub finished: bool,
}

/// Reduces wall-clock `elapsed_ms` (since the animation's own start, delay
/// already subtracted) to a position inside one iteration of a `period_ms`
/// animation under `repeat`/`reverse` (RFC-0025 §1).
///
/// The wrap is done in **integer milliseconds** so an animation that has been
/// spinning for hours keeps exactly the same per-iteration precision as one that
/// just started (an `f32` seconds accumulator would quietly lose sub-frame
/// resolution after a few hours).
#[must_use]
pub fn loop_phase(period_ms: u32, elapsed_ms: u32, repeat: RepeatMode, reverse: bool) -> LoopPhase {
    let period = period_ms.max(1);
    #[allow(clippy::cast_precision_loss)]
    let secs = |ms: u32| ms as f32 / 1000.0;
    // Which play we are in, and how far into it.
    let iteration = elapsed_ms / period;
    let local = elapsed_ms % period;
    // The number of plays this mode allows; `None` = endless.
    let plays = match repeat {
        RepeatMode::Once => Some(1),
        RepeatMode::Count(n) => Some(n.max(1)),
        RepeatMode::Infinite => None,
    };
    if let Some(plays) = plays {
        if iteration >= plays {
            // Played out: hold the end of the *last* iteration. Alternating an
            // odd number of times ends back at the start value.
            let last = plays - 1;
            let ends_at_start = reverse && last % 2 == 1;
            return LoopPhase {
                t_secs: if ends_at_start { 0.0 } else { secs(period) },
                iteration: last,
                finished: true,
            };
        }
    }
    let forward = !reverse || iteration % 2 == 0;
    LoopPhase {
        t_secs: if forward {
            secs(local)
        } else {
            secs(period - local)
        },
        iteration,
        finished: false,
    }
}

/// Maximum number of steps in one `anim.keyframes(…)` sequence (RFC-0025
/// resolved question "max keyframe steps"): 8 covers every real UI pattern
/// (Material's indeterminate progress uses 3–4, a rich shimmer 4–5) and keeps
/// the per-animation step table compact.
pub const MAX_KEYFRAME_STEPS: usize = 8;

/// The pair of keyframe steps surrounding the current progress, and the eased
/// blend factor between them (RFC-0025 §3), [`keyframe_cursor`]'s answer.
///
/// Deliberately value-free: the *timing* half of keyframes lives here in
/// `byard-core` (pure, unit-tested arithmetic), while interpolating the actual
/// values, which may be scalars or coordinate pairs, stays with the
/// interpreter that knows what a value is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct KeyframeCursor {
    /// Index of the step being interpolated *from*.
    pub lo: usize,
    /// Index of the step being interpolated *to* (`== lo` when parked on a step).
    pub hi: usize,
    /// Eased `0..=1` blend factor from `lo`'s value to `hi`'s.
    pub t: f32,
}

/// Locates `progress` (`0..=1` through the sequence) within an ascending
/// `percents` table and applies the destination step's easing (RFC-0025 §3:
/// each step's easing governs the segment arriving *at* it).
///
/// `easings[i]` is the easing into step `i`; `easings[0]` is unused. Progress
/// before the first step or after the last parks on that step, so a sequence
/// that does not start at `0%` holds its first value until it begins.
#[must_use]
pub fn keyframe_cursor(percents: &[f32], easings: &[u32], progress: f32) -> KeyframeCursor {
    let park = |i: usize| KeyframeCursor {
        lo: i,
        hi: i,
        t: 0.0,
    };
    if percents.is_empty() {
        return park(0);
    }
    let last = percents.len() - 1;
    let p = clamp01(progress);
    if p <= percents[0] {
        return park(0);
    }
    if p >= percents[last] {
        return park(last);
    }
    // The first step strictly past `p` closes the active segment. The table is
    // capped at `MAX_KEYFRAME_STEPS`, so the linear scan is a handful of
    // comparisons, cheaper than a branchy binary search at this size.
    let hi = percents.iter().position(|&s| s > p).unwrap_or(last);
    let lo = hi - 1;
    let span = percents[hi] - percents[lo];
    let raw = if span <= 0.0 {
        1.0
    } else {
        (p - percents[lo]) / span
    };
    KeyframeCursor {
        lo,
        hi,
        t: ease_progress(easings.get(hi).copied().unwrap_or(MotionCurve::LINEAR), raw),
    }
}

/// Remaps a `0..=1` progress through one of the [`MotionCurve`] easing families
/// (`LINEAR` passes through). Public so a keyframe segment and a `with` curve
/// share exactly one easing implementation.
#[must_use]
pub fn ease_progress(kind: u32, p: f32) -> f32 {
    if kind == MotionCurve::LINEAR {
        clamp01(p)
    } else {
        ease(kind, clamp01(p))
    }
}

/// Elapsed seconds from `start_ms` to `now_ms`, never negative.
fn seconds_since(start_ms: u32, now_ms: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let ms = now_ms.saturating_sub(start_ms) as f32;
    ms / 1000.0
}

/// Clamps `x` to `[0, 1]`.
fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Raw (unclamped) progress `t / duration` for a fixed-duration curve; a
/// zero/absent duration is treated as an instant jump (progress ≥ 1).
fn progress(m: &Motion, t_seconds: f32) -> f32 {
    let dur_s = m.curve.params[0] / 1000.0;
    if dur_s <= 0.0 { 1.0 } else { t_seconds / dur_s }
}

/// Clamped progress for a fixed-duration curve.
fn duration_progress(m: &Motion, t_seconds: f32) -> f32 {
    clamp01(progress(m, t_seconds))
}

/// Cubic easing remap of a `0..=1` progress for the ease-* curve kinds.
fn ease(kind: u32, p: f32) -> f32 {
    match kind {
        MotionCurve::EASE_IN => p * p * p,
        MotionCurve::EASE_OUT => {
            let q = 1.0 - p;
            1.0 - q * q * q
        }
        // EASE_IN_OUT and any unknown kind fall back to symmetric cubic.
        _ => {
            if p < 0.5 {
                4.0 * p * p * p
            } else {
                let q = -2.0 * p + 2.0;
                1.0 - (q * q * q) / 2.0
            }
        }
    }
}

/// Damping ratios within this band of `1.0` are treated as critically damped.
///
/// A computed `zeta` never lands exactly on `1.0`, and the under-/over-damped
/// closed forms divide by `wd = ω√(1−ζ²)` / `r1−r2 = 2ω√(ζ²−1)`, both of which
/// vanish as `ζ → 1` and would amplify float error into extreme values. Routing
/// the whole neighbourhood through the (division-free) critical form is exact at
/// `ζ = 1` and an imperceptibly-close approximation across so narrow a band.
const SPRING_CRITICAL_BAND: f32 = 1e-2;

/// Analytic damped-spring position at elapsed time `t` seconds (RFC-0010).
/// `params` = `[stiffness, damping, initial_velocity]`; handles the under-,
/// critically-, and over-damped closed forms with an initial velocity. A
/// negative damping is clamped to zero so a mistuned curve can never turn into
/// unbounded exponential growth.
#[allow(clippy::many_single_char_names)] // standard spring-physics notation
fn spring_position(m: &Motion, t: f32) -> f32 {
    let [k, c, v0] = m.curve.params;
    let c = c.max(0.0);
    let d = m.from - m.to; // displacement from target at t=0
    let omega = k.max(0.0).sqrt();
    if omega == 0.0 {
        return m.to + d; // no restoring force: stays put
    }
    let zeta = c / (2.0 * omega);
    if (zeta - 1.0).abs() < SPRING_CRITICAL_BAND {
        // Critically damped (and the near-critical neighbourhood).
        let e = (-omega * t).exp();
        m.to + e * (d + (v0 + omega * d) * t)
    } else if zeta < 1.0 {
        // Underdamped.
        let wd = omega * (1.0 - zeta * zeta).sqrt();
        let e = (-zeta * omega * t).exp();
        let a = d;
        let b = (v0 + zeta * omega * d) / wd;
        m.to + e * (a * (wd * t).cos() + b * (wd * t).sin())
    } else {
        // Overdamped: two real roots.
        let s = omega * (zeta * zeta - 1.0).sqrt();
        let r1 = -zeta * omega + s;
        let r2 = -zeta * omega - s;
        let a = (v0 - r2 * d) / (r1 - r2);
        let b = d - a;
        m.to + a * (r1 * t).exp() + b * (r2 * t).exp()
    }
}

/// Analytic damped-spring velocity (units/second) at elapsed time `t`, the
/// exact derivative of [`spring_position`], so it is accurate even at `t = 0`
/// where the initial acceleration is large. Each branch satisfies `v(0) = v0`.
#[allow(clippy::many_single_char_names)] // standard spring-physics notation
fn spring_velocity(m: &Motion, t: f32) -> f32 {
    let [k, c, v0] = m.curve.params;
    let c = c.max(0.0);
    let d = m.from - m.to;
    let omega = k.max(0.0).sqrt();
    if omega == 0.0 {
        return 0.0;
    }
    let zeta = c / (2.0 * omega);
    if (zeta - 1.0).abs() < SPRING_CRITICAL_BAND {
        let e = (-omega * t).exp();
        e * (v0 - omega * (v0 + omega * d) * t)
    } else if zeta < 1.0 {
        let wd = omega * (1.0 - zeta * zeta).sqrt();
        let b = (v0 + zeta * omega * d) / wd;
        let e = (-zeta * omega * t).exp();
        e * ((b * wd - zeta * omega * d) * (wd * t).cos()
            - (d * wd + zeta * omega * b) * (wd * t).sin())
    } else {
        let s = omega * (zeta * zeta - 1.0).sqrt();
        let r1 = -zeta * omega + s;
        let r2 = -zeta * omega - s;
        let a = (v0 - r2 * d) / (r1 - r2);
        let b = d - a;
        a * r1 * (r1 * t).exp() + b * r2 * (r2 * t).exp()
    }
}

/// GPU-ready instance data for a single solid rectangle.
///
/// Shared between the logic thread (which populates [`RenderFrame::instances`])
/// and the Encoder (which uploads the slice to the GPU instance buffer). Lives
/// in `frame` rather than `encoder` because it crosses the subsystem boundary
/// between the Logic thread's layout pass and the Encoder's GPU dispatch,
/// see the RFC-0001 §9 dependency graph.
///
/// `#[repr(C)]` and `bytemuck` derives match the layout declared in
/// [`BoxInstance::layout`](crate::encoder::BoxInstance::layout).
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BoxInstance {
    /// Rectangle in logical pixels: `[x, y, width, height]`.
    pub rect: [f32; 4],
    /// Linear-space fill colour: `[r, g, b, a]`.
    pub color: [f32; 4],
    /// Per-corner border radii: `[top_left, top_right, bottom_right, bottom_left]`.
    pub radii: [f32; 4],
    /// Paint-time transform (RFC-0011); `Transform::IDENTITY` for an
    /// untransformed box.
    pub transform: Transform,
    /// Corner smoothing `0.0..=1.0` (RFC-0031 §S1): how far the corner profile
    /// travels from a circular arc (`0.0`, the default and the historical
    /// behaviour) towards a squircle (`1.0`). The shaders read it as the
    /// exponent of an Lⁿ norm, `n = 2 + smooth * 4`, and **short-circuit to the
    /// existing L² expression at `n == 2`**, so an unset `smooth` produces
    /// bit-identical pixels to before the property existed.
    ///
    /// Declared last so the earlier fields keep their byte offsets, which the
    /// hand-written [`BoxInstance::layout`](crate::encoder::BoxInstance::layout)
    /// states literally.
    pub smooth: f32,
}

impl Default for BoxInstance {
    /// A transparent, square-cornered, untransformed unit-less box: every
    /// numeric field zero except `transform`, which is the identity.
    fn default() -> Self {
        Self {
            rect: [0.0; 4],
            color: [0.0; 4],
            radii: [0.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        }
    }
}

/// Maps the authored corner smoothing (`0..=1`) onto the Lⁿ exponent the
/// rounded-box fields use (RFC-0031 §S1): `0.0 → 2` (the circular arc every
/// pipeline drew before this property existed) through `1.0 → 6` (a pronounced
/// squircle). Out-of-range input is clamped rather than rejected, an exponent
/// below 2 produces concave corners nobody wants (RFC-0031 §Q1).
#[must_use]
pub fn corner_exponent(smooth: f32) -> f32 {
    2.0 + smooth.clamp(0.0, 1.0) * 4.0
}

/// How an image is scaled/positioned inside its bounding rect.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ImageFit {
    /// Stretch to fill, ignoring aspect ratio.
    #[default]
    Fill,
    /// Scale uniformly to contain inside the rect (letterbox).
    Contain,
    /// Scale uniformly to cover the rect (crop).
    Cover,
    /// No scaling, image at natural size, top-left aligned.
    None,
}

/// A `DecoratedBox` extends a [`BoxInstance`] with an optional border and
/// drop shadow (M21 pipeline). Fields that don't apply are zeroed.
///
/// The Encoder promotes a plain `BoxInstance` to `DecoratedBox` when any of
/// border or shadow fields are non-trivial.
#[derive(Copy, Clone, Debug)]
pub struct DecoratedBox {
    /// The underlying fill/radii data. `base.transform`'s `translate`/`scale`/
    /// `rotate`/`origin` apply as usual; `base.transform.opacity` is **not**
    /// consulted for decorated boxes, [`DecoratedBox::opacity`] (below) is
    /// the one that reaches the shader, since it predates RFC-0011 and
    /// already has an established call-site contract.
    pub base: BoxInstance,
    /// Border width in logical pixels (0.0 = no border).
    pub border_width: f32,
    /// Border colour `[r, g, b, a]`.
    pub border_color: [f32; 4],
    /// Drop-shadow offset X in logical pixels.
    pub shadow_dx: f32,
    /// Drop-shadow offset Y in logical pixels.
    pub shadow_dy: f32,
    /// Drop-shadow blur radius in logical pixels.
    pub shadow_blur: f32,
    /// Drop-shadow spread in logical pixels (RFC-0011): grows (`> 0`) or shrinks
    /// (`< 0`) the shadow shape before blurring, like CSS `box-shadow` spread.
    pub shadow_spread: f32,
    /// Drop-shadow colour `[r, g, b, a]`.
    pub shadow_color: [f32; 4],
    /// Element opacity `0.0–1.0`.
    pub opacity: f32,
    /// An optional linear colour ramp composited over the fill (RFC-0001 §3.1:
    /// the `DecoratedBox` pipeline's declared remit is "rectangles with
    /// border-radius, **gradients**, box-shadows"). `None` is the historical
    /// behaviour, a flat `base.color`.
    pub gradient: Option<Gradient>,
    /// Whether this decoration changed since the last tick.
    ///
    /// The encoder's analogue of [`TextLine::dirty`] for the `DecoratedBox`
    /// pipeline (RFC-0001 §3.3): set upstream by the Evaluator → `RenderFrame`
    /// lowering, trusted by the Encoder when computing the incremental scissor
    /// union. A decoration's `base` is a [`BoxInstance`], which is a pure GPU
    /// `Pod` vertex type and therefore cannot itself carry a dirty bit, so the
    /// flag lives here on the (non-`Pod`) wrapper instead.
    pub dirty: bool,
}

impl Default for DecoratedBox {
    fn default() -> Self {
        Self {
            base: BoxInstance {
                rect: [0.0; 4],
                color: [0.0; 4],
                radii: [0.0; 4],
                transform: Transform::IDENTITY,
                smooth: 0.0,
            },
            border_width: 0.0,
            border_color: [0.0; 4],
            shadow_dx: 0.0,
            shadow_dy: 0.0,
            shadow_blur: 0.0,
            shadow_spread: 0.0,
            shadow_color: [0.0; 4],
            opacity: 1.0,
            gradient: None,
            dirty: false,
        }
    }
}

/// Which shape a [`Gradient`] paints (RFC-0035).
///
/// The tag is a lane of its own on the instance rather than bits stolen from a
/// neighbour (INV-28): `grad_axis` is fully occupied in every kind, and the one
/// spare float this pipeline had was taken by RFC-0031's `smooth`. Four bytes
/// per decorated instance is the honest price, and it keeps every lane with
/// exactly one owner.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum GradientKind {
    /// A directional ramp across the box. The historical behaviour, and what
    /// every existing `.byd` file means.
    #[default]
    Linear = 0,
    /// A centred falloff, circular in the box's own aspect.
    Radial = 1,
    /// An angular sweep around a centre.
    Conic = 2,
}

/// The tag value the shader reads for "this instance has no gradient".
///
/// Presence used to be inferred from `grad_axis.xy` being a unit vector, which
/// only worked because every kind was linear: a radial gradient centred on the
/// box's top-left corner is `(0, 0)`, and would have read as absent.
pub const GRADIENT_NONE: u32 = 3;

/// A three-stop colour ramp painted over a `DecoratedBox`'s fill
/// (RFC-0001 §3.1's `DecoratedBox` remit), in one of the three shapes
/// [`GradientKind`] names (RFC-0035).
///
/// The three stops and their `mid_pos` split are shared by every kind; what
/// differs is only how a fragment's position becomes the ramp parameter. The
/// paragraphs below describe the linear kind, which is what a gradient with no
/// `kind` written is.
///
/// The ramp runs along `angle` (0 = left→right, `π/2` = top→bottom) across the
/// element's own box, from `from` at the start, through `mid` at `mid_pos`, to
/// `to` at the end, enough for the ordinary two-stop case (`mid` on the line
/// between them) *and* for the highlight-band shape a shimmer needs
/// (transparent → bright → transparent), which two stops cannot express.
///
/// Each stop is straight (non-premultiplied) linear-space RGBA and the ramp is
/// composited **over** the fill, so a translucent ramp is a wash over the
/// element's own colour rather than a replacement.
///
/// `offset` shifts the ramp along its own axis and **wraps**: that is what makes
/// an animated offset (RFC-0010 `with`, RFC-0025 `repeat: infinite`) a seamless
/// travelling sweep instead of a jump at the end of each play. Note the sign: a
/// *rising* offset moves the ramp's colours **against** `angle`, so a
/// left-to-right sweep is `angle: 180deg` (a ramp pointing right-to-left) with an
/// offset counting up, the same relationship a scrolling background-position has
/// in CSS.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Gradient {
    /// Which shape this gradient paints (RFC-0035). `Linear` is the historical
    /// behaviour and the default.
    pub kind: GradientKind,
    /// Ramp direction in radians (`Linear`), or the sweep's start angle
    /// (`Conic`). Unused by `Radial`.
    pub angle: f32,
    /// Centre in normalized element space, `(0, 0)` top-left to `(1, 1)`
    /// bottom-right. `Radial` and `Conic` only.
    pub center: [f32; 2],
    /// Radius as a fraction of the element's half-diagonal. `Radial` only, and
    /// required to be positive by the compiler.
    pub radius: f32,
    /// Colour at the start of the ramp.
    pub from: [f32; 4],
    /// Colour at `mid_pos`.
    pub mid: [f32; 4],
    /// Colour at the end of the ramp.
    pub to: [f32; 4],
    /// Where `mid` sits along the ramp, `0.0..=1.0`.
    pub mid_pos: f32,
    /// Phase shift along the ramp, wrapping at 1.0.
    pub offset: f32,
}

impl Gradient {
    /// A two-stop ramp: `mid` is the midpoint of `from`/`to`, so the result is
    /// an ordinary linear gradient with no third stop to reason about.
    #[must_use]
    pub fn two_stop(angle: f32, from: [f32; 4], to: [f32; 4]) -> Self {
        Self {
            kind: GradientKind::Linear,
            angle,
            center: [0.5, 0.5],
            radius: 0.5,
            from,
            mid: std::array::from_fn(|i| f32::midpoint(from[i], to[i])),
            to,
            mid_pos: 0.5,
            offset: 0.0,
        }
    }

    /// The unit direction vector of the ramp.
    #[must_use]
    pub fn direction(&self) -> [f32; 2] {
        [self.angle.cos(), self.angle.sin()]
    }

    /// The four control floats the shader reads, whose meaning is per kind
    /// (RFC-0035 §Reference).
    ///
    /// Written here, once, so the encoder and the tests cannot disagree about
    /// which lane means what for which tag.
    #[must_use]
    pub fn axis(&self) -> [f32; 4] {
        match self.kind {
            GradientKind::Linear => {
                let [dx, dy] = self.direction();
                [dx, dy, self.mid_pos, self.offset]
            }
            GradientKind::Radial => [self.center[0], self.center[1], self.radius, self.mid_pos],
            GradientKind::Conic => [self.center[0], self.center[1], self.angle, self.mid_pos],
        }
    }
}

/// A single in-flight ripple, the RFC-0023 Material ink reveal, lowered to
/// the `Ripple` effects pipeline. One instance is one expanding, fading circle
/// clipped to its element's rounded rect, composited *above* the element's
/// background and *below* its children (the emission order between the
/// background push and the child walk gives it exactly that draw-order depth).
///
/// The logic thread samples the expansion/fade each tick through the shared
/// [`Motion`] closed forms (the RFC-0010 model as landed: the CPU evaluates the
/// curve at the engine clock while the animation is active and re-emits the
/// instance; the shader rasterises the circle analytically). `radius` and
/// `alpha` therefore carry the *current* sampled values, the GPU never needs
/// the engine clock.
///
/// Lives in `frame` because it crosses the Evaluator → Encoder subsystem
/// boundary (RFC-0001 §9), like every other primitive here. `#[repr(C)]` +
/// `bytemuck`: the struct uploads directly as the instance buffer; the field
/// order must match `encoder/ripple.wgsl`'s `InstanceInput`.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RippleInstance {
    /// The element rect `[x, y, width, height]` in logical pixels, both the
    /// quad geometry and the rounded-rect clip bounds.
    pub rect: [f32; 4],
    /// `[center_x, center_y, radius, alpha]`: the circle centre in absolute
    /// logical pixels (the tap point), its current sampled radius, and the
    /// current sampled fade alpha (`0..=1`, inherited opacity folded in).
    pub params: [f32; 4],
    /// Ink colour `[r, g, b, a]`; `a` is the ink's own peak alpha (the fade
    /// multiplies it in the shader via `params.w`).
    pub color: [f32; 4],
    /// Per-corner clip radii `[tl, tr, br, bl]`, the element's own border
    /// radii, so the ink never bleeds past a rounded corner (RFC-0023
    /// resolved question: always clip, no opt-out).
    pub radii: [f32; 4],
    /// Paint-time transform translate (RFC-0011 group transforms).
    pub t_translate: [f32; 2],
    /// Paint-time transform per-axis scale (RFC-0011 group transforms).
    pub t_scale: [f32; 2],
    /// Paint-time transform rotation in radians (RFC-0011).
    pub t_rotate: f32,
    /// Paint-time transform pivot (RFC-0011).
    pub t_origin: [f32; 2],
    /// Draw-order depth (NDC-z), stamped by [`RenderFrame::push_ripple`] like
    /// [`VectorInstance::depth`], between the element background's depth and
    /// its children's.
    pub depth: f32,
    /// Corner smoothing `0.0..=1.0` of the clipping element (RFC-0031 §S1).
    /// The ink is always clipped to its element's outline (RFC-0023), so the
    /// clip has to follow the *same* corner profile, a squircle button whose
    /// ripple squares off at the corners reads as a rendering error.
    pub smooth: f32,
}

/// [`BackdropInstance::quality`] tier: auto-select from the GPU capability
/// probed at startup (RFC-0023 resolved question "Blur quality tiers"). The
/// kernel is always the two-pass separable Gaussian, the tiers differ only
/// in base resolution: `auto` runs at 0.5× on capable GPUs and drops to the
/// cheap 0.25× on software/virtual adapters.
pub const BLUR_QUALITY_AUTO: u32 = 0;
/// [`BackdropInstance::quality`] tier: force the cheap 0.25× base
/// resolution (`blur_quality: low`).
pub const BLUR_QUALITY_LOW: u32 = 1;
/// [`BackdropInstance::quality`] tier: force the finest 0.75× base
/// resolution (`blur_quality: high`).
pub const BLUR_QUALITY_HIGH: u32 = 2;

/// Maximum accepted `blur` radius in logical pixels (RFC-0023 §2: "blur
/// radius is clamped to a maximum to bound kernel size"). The evaluator
/// clamps at emission; the encoder trusts the clamp.
pub const BLUR_MAX_RADIUS: f32 = 40.0;

/// One backdrop-blur surface (RFC-0023 §2): the iOS frosted-glass/vibrancy
/// effect. The element's rect samples the scene *behind* it, everything
/// emitted before this instance, its own background included (RFC-0023 §4
/// compositing order), blurs it, boosts saturation, blends `tint` on top,
/// and draws the result as the element's background, clipped to its rounded
/// rect. Children and later elements render on top.
///
/// Unlike every other primitive, a backdrop is a **barrier**: the encoder
/// must have rasterised everything emitted before it into the colour target
/// before it can sample. [`RenderFrame::push_backdrop`] therefore records a
/// pool-cursor snapshot ([`RenderFrame::backdrop_marks`]) alongside each
/// instance, and the encoder splits its single UI pass into segments at
/// those cursors, zero segments (the single classic pass) when no backdrop
/// is live.
///
/// Lives in `frame` because it crosses the Evaluator → Encoder subsystem
/// boundary (RFC-0001 §9).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BackdropInstance {
    /// The element rect `[x, y, width, height]` in logical pixels, both the
    /// quad geometry and the sampling region behind the element.
    pub rect: [f32; 4],
    /// Per-corner clip radii `[tl, tr, br, bl]`, the element's own border
    /// radii, so the glass pane matches its outline exactly.
    pub radii: [f32; 4],
    /// Gaussian blur σ in logical pixels, the CSS `backdrop-filter:
    /// blur(N)` convention the RFC cites as its inspiration, already
    /// clamped to [`BLUR_MAX_RADIUS`] by the evaluator; always `> 0` (a
    /// blur-less tint never becomes a backdrop, the evaluator lowers it to
    /// a plain translucent fill instead).
    pub blur: f32,
    /// `backdrop_tint` colour `[r, g, b, a]`, blended over the blurred
    /// sample (`a = 0` disables the tint).
    pub tint: [f32; 4],
    /// `blur_saturation` boost applied to the blurred sample before the tint
    /// (RFC-0023: default 1.8, the iOS vibrancy look; `1.0` is neutral).
    pub saturation: f32,
    /// Quality tier: one of [`BLUR_QUALITY_AUTO`] / [`BLUR_QUALITY_LOW`] /
    /// [`BLUR_QUALITY_HIGH`].
    pub quality: u32,
    /// Element opacity (ancestor-folded, RFC-0011) multiplied into the pane.
    pub opacity: f32,
    /// Paint-time transform (RFC-0011 group transforms) applied to the quad.
    pub transform: Transform,
    /// Draw-order depth (NDC-z), stamped by
    /// [`RenderFrame::push_backdrop`], after the element's background,
    /// before its ripple and children.
    pub depth: f32,
    /// Corner smoothing `0.0..=1.0` of the element the pane sits behind
    /// (RFC-0031 §S1/§Q2). The pane clips to the element's own outline, so it
    /// must use the element's own corner profile.
    pub smooth: f32,
}

/// Shape-kind discriminant for a [`CanvasShape`] (RFC-0020 §2, Tier 1): a
/// circular arc. `params = [cx, cy, r, start_rad, sweep_rad, 0, 0, 0]`.
pub const CANVAS_SHAPE_ARC: u32 = 0;
/// [`CanvasShape`] kind: a full circle. `params = [cx, cy, r, 0, 0, 0, 0, 0]`.
pub const CANVAS_SHAPE_CIRCLE: u32 = 1;
/// [`CanvasShape`] kind: a line segment. `params = [x1, y1, x2, y2, 0, 0, 0, 0]`.
pub const CANVAS_SHAPE_LINE: u32 = 2;
/// [`CanvasShape`] kind: a (rounded) rectangle.
/// `params = [x, y, w, h, radius, smooth, 0, 0]`, where `smooth` is the
/// RFC-0031 §S1 corner profile (`0` = the circular arc).
pub const CANVAS_SHAPE_RECT: u32 = 3;
/// [`CanvasShape`] kind: an *n*-fold symmetric rounded polygon or star
/// (RFC-0031 §"`ngon`"). `params = [cx, cy, r, corner, inner, rotate, n, 0]`.
///
/// One kind covers the great majority of the Material 3 Expressive shape
/// vocabulary, because those shapes are overwhelmingly *n*-fold rotationally
/// symmetric rounded polygons and stars: `inner: 0.8, n: 8` is a scallop,
/// `inner: 0.4, n: 5` a burst, `n: 4, corner: r` a circle approached from the
/// other direction.
///
/// - `r` is the circumradius, the distance to an outer point, exactly, whatever
///   `corner` is. That exactness is what makes two `ngon`s of the same `r`
///   morph into each other without the pair appearing to breathe.
/// - `corner` rounds every vertex *and* every inner notch by that radius.
/// - `inner` is normalised so `1.0` is the convex regular polygon and lower
///   values pull the notches in towards the centre.
/// - `n` is an integer ≥ 3 and is **not** animatable (§Q10): a fractional `n`
///   leaves a partial sector, whose seam sweeps the shape *while animating*,
///   the only time the feature is used. Changing shape is what `morph` is for.
pub const CANVAS_SHAPE_NGON: u32 = 4;

/// [`CanvasShape`] combine mode (RFC-0031 §S4): the shape stands alone. Every
/// shape emitted before RFC-0031 is this, and this mode's rendering path is the
/// one that existed then.
pub const GROUP_NONE: u32 = 0;
/// [`CanvasShape`] combine mode: organic fusion (RFC-0031 §S7), the members'
/// fields are unioned by a polynomial smooth-minimum whose blend factor also
/// mixes their colours, so the surface bridge and the colour transition are one
/// event.
pub const GROUP_FUSE: u32 = 1;
/// [`CanvasShape`] combine mode: sequence morphing (RFC-0031 §S10), the
/// members are a *sequence*, indexed by an animatable scalar that blends
/// `floor(phase)` into `floor(phase) + 1` and wraps at the member count.
pub const GROUP_MORPH: u32 = 2;

/// The most members one shape group may carry (RFC-0031 §S5/§Q3).
///
/// Eight, matching RFC-0025's keyframe cap and chosen the same way: it is the
/// point past which the per-fragment loop stops being free, and past which a
/// designer is describing something a group is the wrong tool for. Four cannot
/// express the seven-shape Material 3 Expressive loader, the RFC's motivating
/// use case, and sixteen doubles the worst-case fragment loop for cases better
/// written as several groups. Exceeding it is a compile-time diagnostic naming
/// the ninth shape, never a silent truncation.
pub const MAX_GROUP_MEMBERS: usize = 8;

/// One member of a shape group (RFC-0031 §S4): exactly the fields
/// `eval_shape` consumes, as a POD record in the per-frame shape storage
/// buffer.
///
/// A group *head* is an ordinary [`CanvasShape`] instance whose `group_mode` is
/// not [`GROUP_NONE`]; its members are the `group_count` records starting at
/// `group_first` in [`RenderFrame::shape_records`]. That indirection is the one
/// structural change RFC-0031 makes, and it exists because instanced rendering
/// otherwise gives each fragment sight of exactly one shape.
///
/// `#[repr(C)]` + `bytemuck`, uploaded as a storage buffer with no per-frame
/// allocation: the backing `Vec` is cleared and refilled like every other frame
/// vector.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShapeRecord {
    /// Shape params, first half, the same layout per kind as
    /// [`CanvasShape::params`]`[0..4]`.
    pub params0: [f32; 4],
    /// Shape params, second half, [`CanvasShape::params`]`[4..8]`.
    pub params1: [f32; 4],
    /// Fill colour `[r, g, b, a]`.
    pub fill_color: [f32; 4],
    /// Stroke colour `[r, g, b, a]`.
    pub stroke_color: [f32; 4],
    /// `[kind, cap, 0, 0]`, small integers carried exactly in `f32`, matching
    /// how [`CanvasShape`]'s own instance carries them.
    pub misc: [f32; 4],
}

impl ShapeRecord {
    /// The record form of `shape`, dropping the fields a member does not own:
    /// a group's stroke width, dashes, opacity and transform are the head's
    /// (RFC-0031 §S8), and its bounds are folded into the head's quad.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn from_shape(shape: &CanvasShape) -> Self {
        let p = shape.params;
        Self {
            params0: [p[0], p[1], p[2], p[3]],
            params1: [p[4], p[5], p[6], p[7]],
            fill_color: shape.fill_color,
            stroke_color: shape.stroke_color,
            misc: [shape.kind as f32, shape.cap as f32, 0.0, 0.0],
        }
    }
}

/// [`CanvasShape`] line-cap style (RFC-0020 §"Stroke and fill"): flat end
/// exactly at the path endpoint.
pub const CANVAS_CAP_BUTT: u32 = 0;
/// [`CanvasShape`] line-cap style: semicircular end centred on the endpoint.
pub const CANVAS_CAP_ROUND: u32 = 1;
/// [`CanvasShape`] line-cap style: square end extending half the stroke width
/// past the endpoint.
pub const CANVAS_CAP_SQUARE: u32 = 2;

/// A programmatic 2-D shape (RFC-0020): one `Canvas` shape command lowered to
/// a GPU primitive for the `CanvasShape` analytic-SDF pipeline (Tier 1,
/// arcs, circles, lines, rects). Coordinates in `params` are **absolute
/// logical pixels** (the evaluator has already offset the shape by its
/// `Canvas`'s resolved origin).
///
/// Complex `path(d: …)` commands do not use this type, they rasterize
/// through the `VectorMSDF` pipeline (RFC-0020 §2 Tier 2) as a
/// [`VectorInstance`]; `text(…)` commands lower to [`TextLine`]s.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasShape {
    /// Shape kind: one of [`CANVAS_SHAPE_ARC`], [`CANVAS_SHAPE_CIRCLE`],
    /// [`CANVAS_SHAPE_LINE`], [`CANVAS_SHAPE_RECT`].
    pub kind: u32,
    /// Shape-specific geometry (see each kind constant's doc for the layout).
    /// Angles are radians; `start` is measured from the positive X axis,
    /// increasing clockwise in screen space (positive Y is down).
    pub params: [f32; 8],
    /// Stroke colour `[r, g, b, a]`; `a == 0` disables the stroke.
    pub stroke_color: [f32; 4],
    /// Fill colour `[r, g, b, a]`; `a == 0` disables the fill. A filled arc
    /// paints the circular sector (pie wedge) swept by `start..start+sweep`.
    pub fill_color: [f32; 4],
    /// Stroke width in logical pixels.
    pub stroke_width: f32,
    /// Line-cap style: [`CANVAS_CAP_BUTT`], [`CANVAS_CAP_ROUND`], or
    /// [`CANVAS_CAP_SQUARE`].
    pub cap: u32,
    /// Dash pattern `(dash_length, gap_length)` in logical pixels along the
    /// path; a non-positive dash length renders a solid stroke.
    pub dash: [f32; 2],
    /// Dash phase offset in logical pixels (animatable, RFC-0020 §"Animation").
    pub dash_offset: f32,
    /// Shape opacity `0.0–1.0` (already multiplied by the canvas/inherited
    /// opacity by the lowering).
    pub opacity: f32,
    /// Paint-time transform (RFC-0011), inherited from the `Canvas` element's
    /// ancestors. `transform.opacity` is **not** consulted, `opacity` above
    /// is authoritative, mirroring [`DecoratedBox`]'s contract.
    pub transform: Transform,
    /// Combine mode (RFC-0031 §S4): [`GROUP_NONE`], [`GROUP_FUSE`] or
    /// [`GROUP_MORPH`]. Anything but `NONE` makes this instance a **group
    /// head**: its own `params` are ignored and its `rect` is the union of its
    /// members' bounds.
    pub group_mode: u32,
    /// The combine mode's parameter: the smoothing radius `k` for
    /// [`GROUP_FUSE`], the sequence position `phase` for [`GROUP_MORPH`].
    /// Ignored when the mode is [`GROUP_NONE`].
    pub group_param: f32,
    /// Index of this group's first member in [`RenderFrame::shape_records`].
    pub group_first: u32,
    /// How many members this group has (`<=` [`MAX_GROUP_MEMBERS`]).
    pub group_count: u32,
    /// Hash of this group's member records, **INV-26**.
    ///
    /// [`PaintDigest`] compares a primitive by its own bytes at its own pool
    /// position, and a group head's bytes are its mode, its parameter, its
    /// colours and its rect. Its *members* live outside it, in the storage
    /// buffer. Without this field a fusion group whose head is unchanged while
    /// a member's centre moved would be judged clean and would render the
    /// previous frame's shape.
    ///
    /// A `morph` group happens to escape that by accident, because its
    /// parameter is the phase and the phase moves. The accident is not
    /// something to rely on: it is exactly the kind of thing that works in the
    /// example and fails in an app, so the head folds a hash of its members and
    /// obeys the same rule as every other primitive.
    ///
    /// Written by [`RenderFrame::push_shape_group`]; zero for a non-group.
    pub member_hash: u64,
    /// Whether this shape changed since the last tick, the [`CanvasShape`]
    /// analogue of [`TextLine::dirty`] (RFC-0001 §3.3), consumed by the
    /// Encoder's incremental scissor union.
    pub dirty: bool,
}

impl Default for CanvasShape {
    fn default() -> Self {
        Self {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [0.0; 8],
            stroke_color: [0.0; 4],
            fill_color: [0.0; 4],
            stroke_width: 1.0,
            cap: CANVAS_CAP_BUTT,
            dash: [0.0; 2],
            dash_offset: 0.0,
            opacity: 1.0,
            transform: Transform::IDENTITY,
            group_mode: GROUP_NONE,
            group_param: 0.0,
            group_first: 0,
            group_count: 0,
            member_hash: 0,
            dirty: false,
        }
    }
}

impl CanvasShape {
    /// Conservative bounding box of this shape in logical pixels, including
    /// the stroke extent and cap overhang. Used by the Encoder both to size
    /// the instance quad and for the incremental dirty-scissor union
    /// (RFC-0001 §3.3). Over-estimating costs a few wasted fragments;
    /// under-estimating would visibly clip the shape, so every branch here
    /// leans generous.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        // Round/square caps overhang endpoints by half the stroke width; the
        // stroke body itself extends half the width to each side. A full
        // stroke width of margin covers both plus the AA fringe.
        let m = self.stroke_width.max(1.0);
        let [p0, p1, p2, p3, ..] = self.params;
        match self.kind {
            CANVAS_SHAPE_LINE => {
                let (x0, x1) = (p0.min(p2), p0.max(p2));
                let (y0, y1) = (p1.min(p3), p1.max(p3));
                Rect::new(x0 - m, y0 - m, (x1 - x0) + m * 2.0, (y1 - y0) + m * 2.0)
            }
            CANVAS_SHAPE_RECT => Rect::new(p0 - m, p1 - m, p2 + m * 2.0, p3 + m * 2.0),
            // Arc, circle and ngon: the circumscribed circle's box. An arc's
            // true box is a
            // subset, but sweep-dependent tightening is not worth the CPU per
            // frame, the quad is still tiny.
            _ => {
                let r = p2.max(0.0) + m;
                Rect::new(p0 - r, p1 - r, r * 2.0, r * 2.0)
            }
        }
    }
}

/// A texture-sampled rectangle: `Image` intrinsic lowered to a GPU primitive
/// (M21 pipeline). Texture data is identified by a host-opaque `texture_id`
/// (registered outside the engine boundary via the controller boundary, M23).
#[derive(Clone, Debug)]
pub struct TextureSampler {
    /// Rectangle in logical pixels `[x, y, width, height]`.
    pub rect: [f32; 4],
    /// Texture source path or ID (resolved by the controller boundary at M23).
    pub src: String,
    /// How the image is scaled within the rect.
    pub fit: ImageFit,
    /// Per-corner border radii.
    pub radii: [f32; 4],
    /// Corner smoothing `0.0..=1.0` (RFC-0031 §S1): the image's rounded clip
    /// follows the same profile as the boxes around it, so an avatar inside a
    /// squircle card is not the one square-ish corner in the layout.
    pub smooth: f32,
    /// Opacity `0.0–1.0`.
    pub opacity: f32,
    /// Whether this image primitive changed since the last tick.
    ///
    /// The `TextureSampler` analogue of [`TextLine::dirty`] (RFC-0001 §3.3),
    /// set upstream by the lowering, trusted by the Encoder's incremental
    /// scissor union. Also set by the Encoder itself the frame after an async
    /// decode completes (M29), so a freshly-loaded image paints without a full
    /// redraw.
    pub dirty: bool,
}

/// GPU-ready instance data for a single MSDF vector glyph (RFC-0009 §1, the
/// fifth pipeline). A `VectorIcon` lowers to one of these; the render thread
/// samples the multi-channel signed-distance-field atlas to draw a crisp,
/// resolution-independent monochrome glyph at any scale.
///
/// `#[repr(C)]` + `bytemuck` so the slice uploads to the instance buffer with
/// zero copy, exactly like [`BoxInstance`]. The shape is identical in dev (JIT
/// atlas) and release (AOT-baked atlas), INV-7, so the render path is the same
/// in both modes.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VectorInstance {
    /// UV rectangle within the MSDF atlas `[u0, v0, u1, v1]` (normalised 0–1).
    pub atlas_uv_rect: [f32; 4],
    /// Taffy-resolved screen rectangle in logical pixels `[x, y, width, height]`.
    pub screen_rect: [f32; 4],
    /// Linear-space tint colour `[r, g, b, a]` (monochrome glyph, RFC-0009).
    pub color: [f32; 4],
    /// Distance range baked at generation time, in atlas texels (§2-E). Drives
    /// the screen-space anti-aliasing in the fragment shader.
    pub px_range: f32,
    /// Array-texture layer holding this glyph's cell.
    pub atlas_layer: u32,
    /// Draw-order depth (NDC-z, RFC-0011 cross-pass paint order, see
    /// [`draw_depth`]). Set by [`RenderFrame::push_vector`], not by
    /// [`VectorInstance::new`] (which defaults it to the far plane); a bare
    /// `VectorInstance` built outside a `RenderFrame` therefore always loses
    /// the depth test to one that went through `push_vector`.
    pub depth: f32,
}

impl VectorInstance {
    /// Builds an instance from logical-pixel screen geometry and an atlas UV
    /// rect (both as [`Rect`]), a colour, the baked `px_range`, and the atlas
    /// layer. `depth` defaults to the far plane; [`RenderFrame::push_vector`]
    /// stamps the real draw-order value on push.
    #[must_use]
    pub fn new(screen: Rect, atlas_uv: Rect, color: [f32; 4], px_range: f32, layer: u32) -> Self {
        Self {
            atlas_uv_rect: [
                atlas_uv.x,
                atlas_uv.y,
                atlas_uv.x + atlas_uv.width,
                atlas_uv.y + atlas_uv.height,
            ],
            screen_rect: [screen.x, screen.y, screen.width, screen.height],
            color,
            px_range,
            atlas_layer: layer,
            depth: DRAW_DEPTH_CLEAR,
        }
    }
}

/// An owned MSDF-field upload destined for the vector atlas (RFC-0009 §2-C /
/// INV-8). A background worker generates the field and sends this record over a
/// channel; the **logic thread** allocates the UV slot and records the upload on
/// the next [`RenderFrame`]; only the **render thread** performs the actual
/// `Queue::write_texture` during frame application. Workers never touch the GPU
/// queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasUpload {
    /// Destination array-texture layer.
    pub layer: u32,
    /// Destination pixel x within the layer.
    pub x: u32,
    /// Destination pixel y within the layer.
    pub y: u32,
    /// Field cell width in pixels.
    pub width: u32,
    /// Field cell height in pixels.
    pub height: u32,
    /// The RGBA8 MSDF field bytes (`width * height * 4`), owned (INV-3).
    pub bytes: Vec<u8>,
    /// Caller-assigned identity for this upload (monotonic per-generator),
    /// echoed back through an acknowledgment channel once the render thread
    /// actually applies it, lets the caller resend an upload indefinitely
    /// until confirmed, rather than guessing a fixed retry window.
    pub id: u64,
}

/// A single line of text to be rendered in a frame.
///
/// Shared between the logic thread (which populates [`RenderFrame::texts`]) and
/// the Encoder's `TextGlyphPipeline`. Lives in `frame` rather than
/// `encoder::text_glyph` because it crosses the subsystem boundary between the
/// Evaluator/Atlas and the Encoder, see RFC-0001 §9.
///
/// All coordinates are in **logical pixels**, consistent with [`BoxInstance`].
#[derive(Debug, Clone)]
pub struct TextLine {
    /// X position of the text baseline in logical pixels.
    pub x: f32,
    /// Y position of the text baseline in logical pixels.
    pub y: f32,
    /// Text content.
    pub text: String,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Text colour: `[r, g, b, a]` in linear space, each component 0–1.
    pub color: [f32; 4],
    /// Whether this line's content changed since the last tick.
    ///
    /// Set upstream by the Evaluator → Atlas → `RenderFrame` pipeline, never
    /// derived locally by the Encoder. The Encoder trusts this bit completely
    /// in `--release` builds; see `encoder::text_glyph`'s module documentation.
    pub dirty: bool,
}

/// Logical-pixel dimensions of the surface that hosts a layout.
///
/// Passed to [`LayoutAtlas::compute`](crate::atlas::LayoutAtlas::compute) as
/// the available space for the root node.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Viewport {
    /// Width of the host surface in logical pixels.
    pub width: f32,
    /// Height of the host surface in logical pixels.
    pub height: f32,
}

impl Viewport {
    /// Constructs a new viewport.
    #[must_use]
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// A snapshot of all render primitives for a single frame.
///
/// Built by the Logic thread (Evaluator + Atlas) and read by the Render
/// thread (Encoder). The Logic thread mutates the frame during construction
/// via crate-private APIs; once handed off to the Render thread (via the
/// Relay's atomic pointer swap) it is treated as immutable for the duration
/// of that frame.
///
/// The structure is intentionally SoA-friendly for batched GPU dispatch: each
/// primitive type lives in its own `Vec` so the Encoder can cast a slice
/// directly to bytes and upload it with zero copy.
///
/// [`version`](RenderFrame::version) is a monotonic counter incremented by the
/// Logic thread whenever any content changes. The Encoder compares it against
/// the version it saw on the previous frame to detect missed-dirty-frame
/// scenarios (see `EncoderSubsystem::encode_frame_from_relay`).
#[derive(Debug, Default)]
pub struct RenderFrame {
    /// Resolved geometry produced by the Atlas.
    ///
    /// Each entry is a rectangle in logical pixels, ready for the Encoder
    /// to translate into a draw command.
    rects: Vec<Rect>,

    /// Per-entry dirty state, parallel to `rects`.
    ///
    /// `dirty[i]` is `true` when `rects[i]` changed since the previous tick.
    dirty: Vec<bool>,

    /// Which layout paths the atlas took while producing this frame
    /// (RFC-0032 §R7). Carried on the frame for the same reason the telemetry
    /// block is: the counters are thread-local to the **logic** thread and the
    /// readout is printed on the render thread, so the frame swap is the only
    /// hand-off either of them needs.
    atlas_paths: crate::atlas::layout::path_counters::Counts,

    /// Solid-rectangle instances populated by the Logic thread each tick.
    instances: Vec<BoxInstance>,

    /// Per-instance dirty state, parallel to `instances` (RFC-0032 §R3 step 6).
    /// `BoxInstance` is a GPU `Pod` type and has no room for the bit, so it
    /// lives here.
    instances_dirty: Vec<bool>,

    /// Decorated-box instances (M21), boxes with border/shadow/opacity.
    decorated: Vec<DecoratedBox>,

    /// Texture-sampled image instances (M21).
    textures: Vec<TextureSampler>,

    /// Text lines populated by the Logic thread each tick.
    texts: Vec<TextLine>,

    /// MSDF vector-glyph instances (RFC-0009 §1, the fifth pipeline).
    vector_instances: Vec<VectorInstance>,

    /// Programmatic 2-D shapes (RFC-0020): `Canvas` shape commands lowered to
    /// the `CanvasShape` analytic-SDF pipeline (the sixth pipeline).
    canvas_shapes: Vec<CanvasShape>,

    /// The per-frame shape-record pool every group head indexes into
    /// (RFC-0031 §S4). Cleared and refilled exactly like every other frame
    /// vector: no allocation on the steady-state path, no `Box`, no lifetime.
    shape_records: Vec<ShapeRecord>,

    /// In-flight ripple ink reveals (RFC-0023): one entry per live ripple,
    /// re-sampled and re-emitted each tick while it expands and fades. Carries
    /// its own draw-order depth (stamped at push, like `vector_instances`).
    ripples: Vec<RippleInstance>,

    /// Backdrop-blur surfaces (RFC-0023 §2): frosted-glass panes that sample,
    /// blur, saturate, and tint the scene behind them.
    backdrops: Vec<BackdropInstance>,
    /// One pool-cursor snapshot per backdrop, recorded at
    /// [`push_backdrop`](Self::push_backdrop): everything at pool indices
    /// strictly below the snapshot was emitted *behind* that backdrop, which
    /// is exactly the set the encoder must rasterise before sampling. The
    /// [`LayerMark`] shape is reused, it is precisely "a cursor into every
    /// pool".
    backdrop_marks: Vec<LayerMark>,

    /// The frame asked the encoder for a full, unscissored redraw
    /// (RFC-0006 §3.4 / RFC-0030 §C2).
    ///
    /// The incremental scissor union is derived from what *changed* between
    /// two frames. That is correct for an app mutating in place, and wrong at
    /// the two instants where the whole composition changes underneath it: the
    /// frame an overlay mounts over a previously clean scene, and the frame it
    /// dismisses. On the mount frame the app beneath is drawn for the first
    /// time in a while, so a union computed from a clean previous frame would
    /// leave the overlay, and the view under it, partially painted.
    ///
    /// This is an explicit request rather than a consequence of the instance
    /// and text counts happening to change, because "the counts happened to
    /// differ" is a coincidence that holds today and is nobody's invariant.
    full_redraw: bool,

    /// Pending MSDF-atlas uploads recorded by the logic thread this tick
    /// (RFC-0009 §2-C / INV-8). Applied by the render thread via a single
    /// `Queue::write_texture` each, during frame application, before the draw.
    atlas_uploads: Vec<AtlasUpload>,

    /// Per-primitive **draw-order depth**, one parallel vec per drawable pool.
    ///
    /// The Encoder draws in four type-grouped passes (solids → decorated →
    /// textures → text), which alone can never honour paint order *across*
    /// passes, a container's border (decorated) would always sit above its
    /// children (solids), and text above everything. To fix that coherently we
    /// stamp every primitive, in global emission order, with a monotonically
    /// *nearer* NDC-z (see [`draw_depth`]) and let a shared depth buffer
    /// (cleared to the far plane every frame, `LessEqual` test) resolve
    /// visibility. Emission order is tree pre-order = the intended painter's
    /// order, so a later-emitted primitive correctly wins.
    ///
    /// Kept as parallel `f32` vecs (not fields on the primitives) so the `Pod`
    /// instance structs and their vertex layouts stay byte-for-byte unchanged.
    solid_depths: Vec<f32>,
    decorated_depths: Vec<f32>,
    texture_depths: Vec<f32>,
    text_depths: Vec<f32>,
    canvas_depths: Vec<f32>,

    /// Content-clip rectangles (RFC-0005 `ScrollView`, §3.3 scissor). A
    /// [`ScrollView`] wraps its children in [`begin_clip`](Self::begin_clip) /
    /// [`end_clip`](Self::end_clip); every drawable emitted inside carries the
    /// index of the active clip in the parallel `*_clips` slices below, and the
    /// Encoder sets the GPU scissor to that rect (intersected with the dirty
    /// region) while drawing it. Distinct from the *dirty-region* scissor, which
    /// is a per-frame redraw optimisation, this is a semantic content clip.
    clips: Vec<ClipRect>,
    /// Stack of active clip indices during emission (not serialized). The top is
    /// stamped onto each `push_*`; nested clips store their **intersection** with
    /// the parent, so the Encoder only ever sets one rect. Empty after a frame.
    clip_stack: Vec<u16>,

    /// Per-primitive clip index, parallel to each drawable pool (like the
    /// `*_depths` vecs, kept off the `Pod` instance types so vertex layouts stay
    /// byte-for-byte unchanged). `None` = unclipped (the whole viewport).
    solid_clips: Vec<Option<u16>>,
    decorated_clips: Vec<Option<u16>>,
    texture_clips: Vec<Option<u16>>,
    text_clips: Vec<Option<u16>>,
    vector_clips: Vec<Option<u16>>,
    canvas_clips: Vec<Option<u16>>,
    ripple_clips: Vec<Option<u16>>,
    backdrop_clips: Vec<Option<u16>>,

    /// Per-text-line wrap width in logical pixels, parallel to `texts` (kept off
    /// the `TextLine` struct like the depth/clip vecs, so its 19 construction
    /// sites stay untouched). `Some(w)` shapes that line bounded to `w` so it
    /// wraps onto multiple lines; `None` is the natural single-line width.
    /// Populated by [`push_text_wrapped`](Self::push_text_wrapped); the plain
    /// [`push_text`](Self::push_text) records `None`.
    text_wrap: Vec<Option<f32>>,

    /// What this frame's native views emitted (RFC-0039): instance bytes and
    /// the pipeline that draws them, one batch per pipeline per emitting view.
    ///
    /// A pool like every other, and deliberately so. A native view is not a
    /// special case in the frame any more than it is in layout: it takes a
    /// draw-order depth from the same counter, a clip from the same stack, and
    /// a cursor in [`LayerMark`] so it segments with the rest.
    native: crate::render::NativeBatches,

    /// Textures this frame's native views asked the engine to make available,
    /// drained by the encoder (RFC-0039).
    native_textures: Vec<crate::render::TextureRequest>,

    /// Running global emission counter, mapped to a depth by [`draw_depth`].
    /// Reset each [`clear`](Self::clear); advanced by every `push_*` drawable.
    draw_seq: u32,

    /// Z-layer boundaries recorded by [`begin_layer`](Self::begin_layer)
    /// (RFC-0017 layered draw batches). Empty for the overwhelmingly common
    /// single-layer frame, the Encoder then draws the exact pre-layering
    /// stream. See [`LayerMark`] for the full model.
    layer_marks: Vec<LayerMark>,

    /// Monotonic version counter, incremented by the Logic thread whenever any
    /// content in this frame changes relative to the previous tick.
    ///
    /// The Encoder compares this against the last version it rendered. A version
    /// advance means the render thread skipped at least one dirty frame and must
    /// force a full redraw + text reshape to avoid displaying stale glyphs.
    version: u64,

    /// Where the dev runner's own surfaces begin in every pool, or `None` on a
    /// frame that carries none (RFC-0030 erratum "self-accounting"). See
    /// [`set_dev_base`](Self::set_dev_base).
    dev_base: Option<LayerMark>,

    /// This tick's CPU scope samples (RFC-0013 "Hand-off"), piggybacked on
    /// the existing atomic frame swap instead of a dedicated channel. Empty
    /// when the `telemetry` feature is off or nothing was profiled this tick.
    telemetry: crate::telemetry::SampleBlock,
}

/// A content-clip rectangle (RFC-0005 `ScrollView`). `rect` is in logical
/// pixels, like every other [`RenderFrame`] geometry; the Encoder scales it to
/// physical pixels and intersects it with the dirty-region scissor before the
/// draw. A [`ScrollView`]'s clip is the intersection of its own viewport with
/// any ancestor scroll viewport, so nested scrolling needs no per-draw math.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClipRect {
    /// The clip region in logical pixels.
    pub rect: Rect,
}

/// Axis-aligned intersection of two logical-pixel rects (empty if disjoint).
#[must_use]
fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// A z-layer boundary: the length of every drawable pool at the instant
/// [`RenderFrame::begin_layer`] was called (RFC-0017 layered draw batches).
///
/// The Encoder turns consecutive marks into per-pool index ranges and draws
/// each layer's primitives, solids, decorated, textures, vectors, **and
/// text**, as one interleaved group inside the single UI render pass, so a
/// later layer's *transparent* geometry (a modal scrim, a dialog shadow)
/// alpha-blends **over** an earlier layer's text and images instead of being
/// painted before them. Within a layer, the shared draw-order depth buffer
/// keeps resolving paint order exactly as before; across layers, draw order
/// itself is now correct for blending. A frame with no marks is one layer,
/// the exact pre-layering draw stream, byte for byte.
///
/// Kept as pool *cursors* (not per-primitive tags) because emission is
/// strictly sequential, the main tree first, then each overlay, so a layer
/// is always a contiguous range of every pool. Five `u32`s per layer instead
/// of one tag per primitive.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct LayerMark {
    /// `instances` (solid boxes) length at the boundary.
    pub solid: u32,
    /// `decorated` length at the boundary.
    pub decorated: u32,
    /// `textures` length at the boundary.
    pub texture: u32,
    /// `vector_instances` length at the boundary.
    pub vector: u32,
    /// `texts` length at the boundary.
    pub text: u32,
    /// `canvas_shapes` length at the boundary (RFC-0020).
    pub canvas: u32,
    /// `ripples` length at the boundary (RFC-0023).
    pub ripple: u32,
    /// `backdrops` length at the boundary (RFC-0023 §2).
    pub backdrop: u32,
    /// Native-view batch count at the boundary (RFC-0039).
    pub native: u32,
}

/// Applies `set` to every element of `pool` from index `from` onward.
fn mark_from<T>(pool: &mut [T], from: u32, mut set: impl FnMut(&mut T)) {
    let start = usize::try_from(from).unwrap_or(usize::MAX).min(pool.len());
    for item in &mut pool[start..] {
        set(item);
    }
}

/// NDC far-plane depth the shared draw-order depth buffer is cleared to at the
/// start of every frame. Every drawable's [`draw_depth`] is strictly nearer, so
/// it passes the `LessEqual` test against this cleared value.
pub const DRAW_DEPTH_CLEAR: f32 = 1.0;

/// NDC-z granted per emitted primitive. `1/65536` spaces ~65k primitives across
/// the usable near-1.0 range while staying far above f32 depth resolution
/// (~6e-8 near 1.0), so adjacent primitives never z-fight.
const DRAW_DEPTH_STEP: f32 = 1.0 / 65_536.0;

/// Maps a global emission sequence number to a draw-order NDC-z: earlier =
/// farther (toward `1.0`), later = nearer (toward `0.0`). Saturating, so a
/// pathologically deep frame clamps to the near plane rather than wrapping.
#[must_use]
pub fn draw_depth(seq: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let steps = seq.saturating_add(1) as f32 * DRAW_DEPTH_STEP;
    (DRAW_DEPTH_CLEAR - steps).max(0.0)
}

impl RenderFrame {
    /// Creates an empty frame.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears the frame, retaining internal buffer capacity.
    ///
    /// After the first frame, subsequent populations pay zero allocation cost
    /// as long as primitive counts stay within the high-water mark. Version is
    /// reset to zero; the Logic thread always calls [`set_version`](Self::set_version)
    /// immediately after acquiring a recycled frame.
    pub fn clear(&mut self) {
        self.atlas_paths = crate::atlas::layout::path_counters::Counts::default();
        self.full_redraw = false;
        self.rects.clear();
        self.dirty.clear();
        self.instances_dirty.clear();
        self.instances.clear();
        self.decorated.clear();
        self.textures.clear();
        self.texts.clear();
        self.vector_instances.clear();
        self.canvas_shapes.clear();
        self.shape_records.clear();
        self.ripples.clear();
        self.backdrops.clear();
        self.backdrop_marks.clear();
        self.atlas_uploads.clear();
        self.solid_depths.clear();
        self.decorated_depths.clear();
        self.texture_depths.clear();
        self.text_depths.clear();
        self.canvas_depths.clear();
        self.clips.clear();
        self.clip_stack.clear();
        self.solid_clips.clear();
        self.decorated_clips.clear();
        self.texture_clips.clear();
        self.text_clips.clear();
        self.vector_clips.clear();
        self.canvas_clips.clear();
        self.ripple_clips.clear();
        self.backdrop_clips.clear();
        self.text_wrap.clear();
        self.native.begin_frame();
        self.native_textures.clear();
        self.layer_marks.clear();
        self.dev_base = None;
        self.draw_seq = 0;
        self.version = 0;
        // `Vec::clear` only, not `SampleBlock::default()`, the latter would
        // drop the block's existing allocation and defeat the capacity
        // retention this method promises once telemetry is attached.
        self.telemetry.samples.clear();
        self.telemetry.dropped = 0;
    }

    /// Appends a resolved rectangle and its dirty state to the frame.
    pub fn push_rect(&mut self, rect: Rect, dirty: bool) {
        self.rects.push(rect);
        self.dirty.push(dirty);
    }

    /// Advances the global emission counter and returns the draw-order depth
    /// (NDC-z) for the primitive about to be pushed. See [`solid_depths`] for
    /// the ordering model.
    ///
    /// [`solid_depths`]: Self::solid_depths
    fn next_depth(&mut self) -> f32 {
        let d = draw_depth(self.draw_seq);
        self.draw_seq = self.draw_seq.saturating_add(1);
        d
    }

    /// The clip index stamped onto the next `push_*` (the top of the clip
    /// stack), or `None` when nothing is being clipped.
    fn active_clip(&self) -> Option<u16> {
        self.clip_stack.last().copied()
    }

    /// Opens a content clip (RFC-0005 `ScrollView`): every drawable pushed until
    /// the matching [`end_clip`](Self::end_clip) is clipped to `rect`. A nested
    /// clip is stored as its **intersection** with the enclosing clip, so the
    /// Encoder only ever sets one scissor rect. Returns the clip's index.
    pub fn begin_clip(&mut self, rect: Rect) -> u16 {
        let clipped = match self.clip_stack.last() {
            Some(&parent) => intersect_rect(self.clips[parent as usize].rect, rect),
            None => rect,
        };
        let id = u16::try_from(self.clips.len()).unwrap_or(u16::MAX);
        self.clips.push(ClipRect { rect: clipped });
        self.clip_stack.push(id);
        id
    }

    /// Closes the most recently opened content clip.
    pub fn end_clip(&mut self) {
        self.clip_stack.pop();
    }

    /// Appends a [`BoxInstance`] to the frame.
    ///
    /// The instance is recorded **dirty**, see
    /// [`instances_dirty`](Self::instances_dirty) for why that is the only
    /// safe default and what narrows it.
    pub fn push_instance(&mut self, instance: BoxInstance) {
        let d = self.next_depth();
        let c = self.active_clip();
        self.instances.push(instance);
        self.instances_dirty.push(true);
        self.solid_depths.push(d);
        self.solid_clips.push(c);
    }

    /// Per-instance dirty state, parallel to [`instances`](Self::instances).
    ///
    /// [`BoxInstance`] is a pure GPU `Pod` vertex type with no room for a
    /// dirty bit, so, unlike [`TextLine`] and friends, solid boxes carry
    /// theirs out of band, here.
    ///
    /// Every entry starts `true`, because "this box may have changed" is the
    /// only claim the frame can make on its own; [`PaintDigest::apply`]
    /// narrows it to what actually changed by comparing each instance against
    /// the bytes at the same position last frame.
    #[must_use]
    pub fn instances_dirty(&self) -> &[bool] {
        &self.instances_dirty
    }

    /// Appends a [`DecoratedBox`] (border/shadow/opacity) to the frame (M21).
    pub fn push_decorated(&mut self, d: DecoratedBox) {
        let depth = self.next_depth();
        let c = self.active_clip();
        self.decorated.push(d);
        self.decorated_depths.push(depth);
        self.decorated_clips.push(c);
    }

    /// Appends a [`TextureSampler`] (image) to the frame (M21).
    pub fn push_texture(&mut self, t: TextureSampler) {
        let d = self.next_depth();
        let c = self.active_clip();
        self.textures.push(t);
        self.texture_depths.push(d);
        self.texture_clips.push(c);
    }

    /// Appends a [`TextLine`] to the frame (natural single-line width).
    pub fn push_text(&mut self, text: TextLine) {
        self.push_text_wrapped(text, None);
    }

    /// Appends a [`TextLine`] shaped to an optional wrap width (RFC-0018 text
    /// wrap). `Some(w)` wraps the line onto multiple lines bounded to `w`
    /// logical pixels; `None` behaves exactly like [`push_text`](Self::push_text).
    pub fn push_text_wrapped(&mut self, text: TextLine, wrap_width: Option<f32>) {
        let d = self.next_depth();
        let c = self.active_clip();
        self.texts.push(text);
        self.text_depths.push(d);
        self.text_clips.push(c);
        self.text_wrap.push(wrap_width);
    }

    /// Appends a [`VectorInstance`] (MSDF glyph) to the frame (RFC-0009 §1).
    pub fn push_vector(&mut self, mut v: VectorInstance) {
        v.depth = self.next_depth();
        let c = self.active_clip();
        self.vector_instances.push(v);
        self.vector_clips.push(c);
    }

    /// Appends a [`RippleInstance`] (RFC-0023 ripple ink) to the frame,
    /// stamping its draw-order depth, pushed by the evaluator between an
    /// element's background and its children, which is exactly the RFC-0023
    /// compositing slot (background → ripple → children).
    pub fn push_ripple(&mut self, mut r: RippleInstance) {
        r.depth = self.next_depth();
        let c = self.active_clip();
        self.ripples.push(r);
        self.ripple_clips.push(c);
    }

    /// Appends a [`BackdropInstance`] (RFC-0023 §2 frosted-glass pane) to the
    /// frame, stamping its draw-order depth and recording the pool-cursor
    /// snapshot that tells the encoder what was emitted *behind* it (the
    /// content it must rasterise before sampling). Pushed by the evaluator
    /// right after the element's own background, the RFC-0023 §4 slot
    /// (background → blur → tint → ripple → children).
    pub fn push_backdrop(&mut self, mut b: BackdropInstance) {
        let mark = self.cursor();
        b.depth = self.next_depth();
        let c = self.active_clip();
        self.backdrops.push(b);
        self.backdrop_marks.push(mark);
        self.backdrop_clips.push(c);
    }

    /// Draws one native view into this frame (RFC-0039).
    ///
    /// The engine calls this where it lowers an intrinsic: the view is handed
    /// the box layout resolved for it and a [`RenderCtx`] over this frame's
    /// pools, and whatever it emits takes the next draw-order depth and the
    /// clip in force, exactly as a `push_*` would have.
    ///
    /// Returns whether the view asked for another frame
    /// ([`RenderCtx::request_repaint`]).
    ///
    /// [`RenderCtx`]: crate::render::RenderCtx
    /// [`RenderCtx::request_repaint`]: crate::render::RenderCtx::request_repaint
    pub fn render_native(
        &mut self,
        view: &mut dyn crate::render::NativeView,
        layout: crate::render::Layout,
    ) -> bool {
        let depth = self.next_depth();
        let clip = self.active_clip_shape();
        let mut cx =
            crate::render::RenderCtx::new(&mut self.native, &mut self.native_textures, depth);
        match clip {
            Some(shape) => cx.clip(shape, |cx| view.render(layout, cx)),
            None => view.render(layout, &mut cx),
        }
        cx.wants_repaint()
    }

    /// This frame's native-view batches, in emission order (RFC-0039).
    #[must_use]
    pub fn native_batches(&self) -> &[crate::render::NativeBatch] {
        self.native.batches()
    }

    /// Textures this frame's native views asked for (RFC-0039).
    #[must_use]
    pub fn native_textures(&self) -> &[crate::render::TextureRequest] {
        &self.native_textures
    }

    /// The clip rectangle in force, as a [`ClipShape`] a native view's batch
    /// can carry.
    ///
    /// The frame's own clips are indices into a table the encoder scissors
    /// with; a native batch carries its shape instead, because a package's
    /// pipeline is not obliged to be scissored the way the core pools are.
    ///
    /// [`ClipShape`]: crate::render::ClipShape
    fn active_clip_shape(&self) -> Option<crate::render::ClipShape> {
        let index = self.active_clip()?;
        let clip = self.clips.get(usize::from(index))?;
        let r = clip.rect;
        Some(crate::render::ClipShape::Rect([
            r.x, r.y, r.width, r.height,
        ]))
    }

    /// Appends a [`CanvasShape`] (RFC-0020 Tier-1 shape command) to the frame.
    pub fn push_canvas_shape(&mut self, s: CanvasShape) {
        let d = self.next_depth();
        let c = self.active_clip();
        self.canvas_shapes.push(s);
        self.canvas_depths.push(d);
        self.canvas_clips.push(c);
    }

    /// Appends a **shape group** (RFC-0031 §S4): `members` are copied into this
    /// frame's shape-record pool and `head` is pushed as the one instance that
    /// draws them, with its member range and member hash filled in here.
    ///
    /// Filling those three fields here rather than at the call site is the
    /// point of the method. `group_first`/`group_count` are pool positions only
    /// this type knows, and `member_hash` is **INV-26**: a shader reading data
    /// that lives outside the primitive makes that data part of the primitive's
    /// dirtiness, or a member moves and the head is judged clean. Computing it
    /// on the same pass that appends the members means there is no way to add a
    /// member and forget the hash.
    ///
    /// `members` beyond [`MAX_GROUP_MEMBERS`] are refused rather than
    /// truncated: the compiler diagnoses the ninth shape (§S5), so reaching
    /// here with more than eight is a bug, and drawing seven of eight members
    /// would hide it.
    ///
    /// # Panics
    ///
    /// Debug builds panic if `members` exceeds [`MAX_GROUP_MEMBERS`].
    pub fn push_shape_group(&mut self, mut head: CanvasShape, members: &[ShapeRecord]) {
        debug_assert!(
            members.len() <= MAX_GROUP_MEMBERS,
            "a shape group may carry at most {MAX_GROUP_MEMBERS} members, got {}",
            members.len()
        );
        let members = &members[..members.len().min(MAX_GROUP_MEMBERS)];
        head.group_first = u32::try_from(self.shape_records.len()).unwrap_or(u32::MAX);
        head.group_count = u32::try_from(members.len()).unwrap_or(0);
        head.member_hash = shape_record_hash(members);
        self.shape_records.extend_from_slice(members);
        self.push_canvas_shape(head);
    }

    /// Records a pending [`AtlasUpload`] for the render thread to apply before
    /// drawing this frame (RFC-0009 §2-C / INV-8).
    pub fn push_atlas_upload(&mut self, upload: AtlasUpload) {
        self.atlas_uploads.push(upload);
    }

    /// Opens a new z-layer (RFC-0017): everything pushed from here on is drawn
    ///, solids, decorated, textures, vectors, *and text*, interleaved, after
    /// **everything** already in the frame, inside the same GPU render pass.
    ///
    /// Called by the overlay phase before emitting each overlay, so a modal
    /// scrim genuinely alpha-blends over the main tree's text and images
    /// instead of being drawn before the frame-final text batch. Consecutive
    /// calls with no primitives in between are deduplicated, so an overlay
    /// that emits nothing costs nothing. A frame that never calls this is a
    /// single layer and renders through the exact pre-layering draw stream.
    pub fn begin_layer(&mut self) {
        let mark = self.cursor();
        if self.layer_marks.last() == Some(&mark) {
            return; // empty layer, dedup, an overlay that emitted nothing is free
        }
        self.layer_marks.push(mark);
    }

    /// The current cursor into every pool, without recording a layer boundary.
    ///
    /// Paired with [`mark_dirty_since`](Self::mark_dirty_since) by an overlay
    /// that is emitted *after* content whose length it does not control.
    #[must_use]
    pub fn cursor(&self) -> LayerMark {
        LayerMark {
            solid: u32::try_from(self.instances.len()).unwrap_or(u32::MAX),
            decorated: u32::try_from(self.decorated.len()).unwrap_or(u32::MAX),
            texture: u32::try_from(self.textures.len()).unwrap_or(u32::MAX),
            vector: u32::try_from(self.vector_instances.len()).unwrap_or(u32::MAX),
            text: u32::try_from(self.texts.len()).unwrap_or(u32::MAX),
            canvas: u32::try_from(self.canvas_shapes.len()).unwrap_or(u32::MAX),
            ripple: u32::try_from(self.ripples.len()).unwrap_or(u32::MAX),
            backdrop: u32::try_from(self.backdrops.len()).unwrap_or(u32::MAX),
            native: u32::try_from(self.native.len()).unwrap_or(u32::MAX),
        }
    }

    /// Marks every primitive emitted at or after `mark` as dirty.
    ///
    /// # Why an overlay sometimes has to do this
    ///
    /// The encoder's incremental machinery is **index-addressed**: the glyph
    /// cache compares `texts[i]` against what it shaped for `texts[i]` last
    /// frame, and trusts `TextLine::dirty` to say when they differ. That
    /// contract holds as long as one producer owns the pool, because an
    /// element that keeps its index keeps its identity.
    ///
    /// A dev overlay, the in-window HUD, is emitted *after* the app, by a
    /// different interpreter, into the same pools. Its indices therefore move
    /// whenever the app's counts change, and on such a frame index `i` holds
    /// the overlay's line where it held the app's a frame ago. Both producers
    /// truthfully report their own primitives unchanged, and the glyph cache
    /// would draw last frame's shaped buffer at that index, stale text, in
    /// release, silently.
    ///
    /// Neither producer can see that on its own, so the frame, which is the
    /// only thing that sees both, resolves it. Called only on frames where
    /// the base actually shifted, so the overlay costs nothing on the frames
    /// in between.
    pub fn mark_dirty_since(&mut self, mark: LayerMark) {
        mark_from(&mut self.instances_dirty, mark.solid, |slot| *slot = true);
        mark_from(&mut self.decorated, mark.decorated, |d| d.dirty = true);
        mark_from(&mut self.textures, mark.texture, |t| t.dirty = true);
        mark_from(&mut self.texts, mark.text, |t| t.dirty = true);
        mark_from(&mut self.canvas_shapes, mark.canvas, |c| c.dirty = true);
    }

    /// The z-layer boundaries recorded this frame (RFC-0017 layered draw
    /// batches); empty for a single-layer frame. See [`LayerMark`].
    #[must_use]
    pub fn layer_marks(&self) -> &[LayerMark] {
        &self.layer_marks
    }

    /// Records where the **dev runner's own surfaces** begin in every pool.
    ///
    /// # Why the frame carries this and not just the profiler
    ///
    /// A dev overlay's cost on the *logic* thread is bounded by a scope the
    /// overlay itself opens, so a thread-local owner (`telemetry::attribute_to`)
    /// captures all of it. Its cost on the *render* thread is not: by the time
    /// the encoder runs, the overlay's primitives are anonymous entries in the
    /// same pools as the app's, and the single largest term in the frame,
    /// glyph shaping, is charged per text line. Without a partition the
    /// encoder cannot tell which lines it is shaping on the app's behalf, so
    /// the overlay's shaping is billed to the app and §V4's self-accounting
    /// under-reports by most of its real cost.
    ///
    /// The partition is a cursor rather than a per-primitive tag because dev
    /// surfaces are always emitted **last**, after the whole app tree: one
    /// `LayerMark` answers "is this one theirs?" for every pool at once and
    /// costs nothing per primitive. `None`, the default, means the frame
    /// carries no dev surfaces at all, which is every frame of a shipped app.
    ///
    /// Call it with a [`cursor`](Self::cursor) taken **before** the first dev
    /// surface emits, once per frame.
    pub fn set_dev_base(&mut self, base: LayerMark) {
        self.dev_base = Some(base);
    }

    /// Where the dev runner's surfaces begin in every pool, if this frame
    /// carries any. See [`set_dev_base`](Self::set_dev_base).
    #[must_use]
    pub const fn dev_base(&self) -> Option<LayerMark> {
        self.dev_base
    }

    /// The index of the first text line owned by a dev surface, or
    /// `texts().len()` when the frame carries none.
    ///
    /// Clamped to the pool's length, so a stale or malformed base can only ever
    /// mean "no dev text", never an out-of-range split in the encoder.
    #[must_use]
    pub fn dev_text_start(&self) -> usize {
        self.dev_base
            .map_or(self.texts.len(), |b| {
                usize::try_from(b.text).unwrap_or(usize::MAX)
            })
            .min(self.texts.len())
    }

    /// Sets the frame's version counter.
    pub fn set_version(&mut self, version: u64) {
        self.version = version;
    }

    /// Returns the resolved rectangles in this frame.
    #[must_use]
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Returns the per-entry dirty state, parallel to [`rects`](Self::rects).
    #[must_use]
    pub fn dirty(&self) -> &[bool] {
        &self.dirty
    }

    /// Returns the solid-rectangle instances in this frame.
    #[must_use]
    pub fn instances(&self) -> &[BoxInstance] {
        &self.instances
    }

    /// Returns the decorated-box instances in this frame (M21).
    #[must_use]
    pub fn decorated(&self) -> &[DecoratedBox] {
        &self.decorated
    }

    /// Returns the texture-sampled image instances in this frame (M21).
    #[must_use]
    pub fn textures(&self) -> &[TextureSampler] {
        &self.textures
    }

    /// Returns the per-line wrap widths, parallel to [`texts`](Self::texts).
    #[must_use]
    pub fn text_wraps(&self) -> &[Option<f32>] {
        &self.text_wrap
    }

    /// Returns the text lines pushed to the frame.
    #[must_use]
    pub fn texts(&self) -> &[TextLine] {
        &self.texts
    }

    /// Returns the MSDF vector-glyph instances in this frame (RFC-0009 §1).
    #[must_use]
    pub fn vector_instances(&self) -> &[VectorInstance] {
        &self.vector_instances
    }

    /// Returns the `Canvas` shape primitives in this frame (RFC-0020).
    #[must_use]
    pub fn canvas_shapes(&self) -> &[CanvasShape] {
        &self.canvas_shapes
    }

    /// This frame's shape-record pool, the members every group head indexes
    /// into (RFC-0031 §S4).
    #[must_use]
    pub fn shape_records(&self) -> &[ShapeRecord] {
        &self.shape_records
    }

    /// Returns the live ripple ink reveals in this frame (RFC-0023).
    #[must_use]
    pub fn ripples(&self) -> &[RippleInstance] {
        &self.ripples
    }

    /// Returns the backdrop-blur surfaces in this frame (RFC-0023 §2).
    #[must_use]
    pub fn backdrops(&self) -> &[BackdropInstance] {
        &self.backdrops
    }

    /// Returns the per-backdrop pool-cursor snapshots (parallel to
    /// [`backdrops`](Self::backdrops)): the encoder's pass-split boundaries.
    #[must_use]
    pub fn backdrop_marks(&self) -> &[LayerMark] {
        &self.backdrop_marks
    }

    /// Returns the pending atlas uploads recorded this frame (RFC-0009 §2-C).
    #[must_use]
    pub fn atlas_uploads(&self) -> &[AtlasUpload] {
        &self.atlas_uploads
    }

    // ── Census (RFC-0030 §P6) ──────────────────────────────────────────────
    //
    // Three `len()` reads over data that already crosses the frame boundary.
    // No new field, no new traffic, and nothing retained between frames, the
    // statusline's "382 boxes" is a measurement of the frame in hand, which is
    // the only reading that cannot drift out of date.

    /// How many box-class draw instances this frame carries.
    ///
    /// Every pool the encoder rasterises as a quad: solid rectangles,
    /// decorated boxes, sampled textures, `Canvas` shapes, ripples and
    /// backdrops. Text and vector glyphs are counted separately because they
    /// are a different order of cost, a text line becomes as many quads as it
    /// has glyphs, and conflating the two would make the number unreadable in
    /// the direction that matters.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.instances.len()
            + self.decorated.len()
            + self.textures.len()
            + self.canvas_shapes.len()
            + self.ripples.len()
            + self.backdrops.len()
    }

    /// How many text **lines** this frame carries (not glyphs).
    #[must_use]
    pub fn text_count(&self) -> usize {
        self.texts.len()
    }

    /// How many MSDF vector-glyph instances this frame carries (RFC-0009 §1).
    #[must_use]
    pub fn vector_count(&self) -> usize {
        self.vector_instances.len()
    }

    /// Draw-order depths parallel to [`instances`](Self::instances).
    #[must_use]
    pub fn solid_depths(&self) -> &[f32] {
        &self.solid_depths
    }

    /// Draw-order depths parallel to [`decorated`](Self::decorated).
    #[must_use]
    pub fn decorated_depths(&self) -> &[f32] {
        &self.decorated_depths
    }

    /// Draw-order depths parallel to [`textures`](Self::textures).
    #[must_use]
    pub fn texture_depths(&self) -> &[f32] {
        &self.texture_depths
    }

    /// Draw-order depths parallel to [`texts`](Self::texts).
    #[must_use]
    pub fn text_depths(&self) -> &[f32] {
        &self.text_depths
    }

    /// Draw-order depths parallel to [`canvas_shapes`](Self::canvas_shapes).
    #[must_use]
    pub fn canvas_depths(&self) -> &[f32] {
        &self.canvas_depths
    }

    /// The content-clip table (RFC-0005 `ScrollView`); a primitive's
    /// `*_clips` entry indexes into this.
    #[must_use]
    pub fn clips(&self) -> &[ClipRect] {
        &self.clips
    }
    /// Per-`BoxInstance` clip index (parallel to [`instances`](Self::instances)).
    #[must_use]
    pub fn solid_clips(&self) -> &[Option<u16>] {
        &self.solid_clips
    }
    /// Per-`DecoratedBox` clip index (parallel to [`decorated`](Self::decorated)).
    #[must_use]
    pub fn decorated_clips(&self) -> &[Option<u16>] {
        &self.decorated_clips
    }
    /// Per-`TextureSampler` clip index (parallel to [`textures`](Self::textures)).
    #[must_use]
    pub fn texture_clips(&self) -> &[Option<u16>] {
        &self.texture_clips
    }
    /// Per-`TextLine` clip index (parallel to [`texts`](Self::texts)).
    #[must_use]
    pub fn text_clips(&self) -> &[Option<u16>] {
        &self.text_clips
    }
    /// Per-`VectorInstance` clip index (parallel to [`vector_instances`](Self::vector_instances)).
    #[must_use]
    pub fn vector_clips(&self) -> &[Option<u16>] {
        &self.vector_clips
    }
    /// Per-`CanvasShape` clip index (parallel to [`canvas_shapes`](Self::canvas_shapes)).
    #[must_use]
    pub fn canvas_clips(&self) -> &[Option<u16>] {
        &self.canvas_clips
    }
    /// Per-`RippleInstance` clip index (parallel to [`ripples`](Self::ripples)).
    #[must_use]
    pub fn ripple_clips(&self) -> &[Option<u16>] {
        &self.ripple_clips
    }
    /// Per-`BackdropInstance` clip index (parallel to [`backdrops`](Self::backdrops)).
    #[must_use]
    pub fn backdrop_clips(&self) -> &[Option<u16>] {
        &self.backdrop_clips
    }

    /// Returns the monotonic version counter for this frame.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Pulls the calling thread's CPU scope samples into this frame
    /// (RFC-0013 "Hand-off"), piggybacked on this frame's atomic swap instead
    /// of a dedicated channel.
    ///
    /// Called from the logic thread, once per tick, by
    /// [`crate::relay::Relay::publish`] right before the frame is swapped
    /// in, so every publish path picks up telemetry automatically, with no
    /// per-call-site wiring needed. Reuses this frame's existing
    /// `SampleBlock` allocation (see [`RenderFrame::clear`]) rather than
    /// allocating a fresh one each tick.
    pub fn drain_telemetry(&mut self) {
        crate::telemetry::drain_samples_into(&mut self.telemetry);
    }

    /// Returns this tick's CPU scope samples, if any were captured.
    #[must_use]
    pub fn telemetry(&self) -> &crate::telemetry::SampleBlock {
        &self.telemetry
    }

    /// Folds `previous`'s per-primitive dirty bits into this frame's
    /// (RFC-0032 §R3 step 6 × RFC-0001 §5.2).
    ///
    /// # Why this exists
    ///
    /// The relay is **latest-wins**: the logic thread publishes faster than
    /// the display refreshes, so most published frames are never rendered.
    /// While every primitive was emitted `dirty: true` that cost nothing,
    /// the encoder re-shaped and redrew everything on whichever frame it
    /// happened to see. Now that a frame reports what actually changed, a
    /// skipped frame is a **lost dirty bit**: the frame carrying "this
    /// paragraph's text changed" is dropped, the next one truthfully reports
    /// it clean, and the glyph cache keeps the string from before the edit.
    ///
    /// Merging is the fix, and it is the right one because dirtiness is
    /// monotone: the union of "changed since frame N-1" and "changed since
    /// frame N" *is* "changed since frame N-1". The alternative, detecting
    /// the gap and redrawing everything, is correct too, and is what this
    /// replaced; it just gives the whole win back on any machine whose logic
    /// thread outruns its display, which is every machine.
    ///
    /// A pool whose length differs is marked dirty in full: positions no
    /// longer mean the same thing, so there is nothing to merge index-wise.
    pub fn merge_dirty_from(&mut self, previous: &Self) {
        // A full-redraw request is monotone in exactly the same way a dirty
        // bit is: if the frame that mounted an overlay was published and never
        // rendered, its request is still owed. Dropping it here would make the
        // overlay's correctness depend on the display keeping up with the
        // logic thread, which is the one thing this whole merge exists because
        // it does not.
        self.full_redraw |= previous.full_redraw;
        merge_flags(
            &mut self.instances_dirty,
            previous.instances_dirty.as_slice(),
            |slot, prev| *slot |= prev,
            |slot| *slot = true,
        );
        merge_into(
            &mut self.texts,
            &previous.texts,
            |t| &mut t.dirty,
            |t| t.dirty,
        );
        merge_into(
            &mut self.decorated,
            &previous.decorated,
            |d| &mut d.dirty,
            |d| d.dirty,
        );
        merge_into(
            &mut self.textures,
            &previous.textures,
            |t| &mut t.dirty,
            |t| t.dirty,
        );
        merge_into(
            &mut self.canvas_shapes,
            &previous.canvas_shapes,
            |c| &mut c.dirty,
            |c| c.dirty,
        );
    }

    /// Asks the encoder to redraw the whole surface this frame rather than
    /// only the dirty union (RFC-0006 §3.4).
    ///
    /// See [`Self::full_redraw`] for the two instants that need it. Cleared by
    /// [`clear`](Self::clear), so it never leaks into the next frame, a
    /// sticky full redraw would silently disable the incremental path for the
    /// rest of the session, which is the expensive way to be wrong here.
    pub fn request_full_redraw(&mut self) {
        self.full_redraw = true;
    }

    /// Whether this frame asked for a full, unscissored redraw.
    #[must_use]
    pub const fn wants_full_redraw(&self) -> bool {
        self.full_redraw
    }

    /// Records which layout paths the atlas took while producing this frame
    /// (RFC-0032 §R7), a **delta** for this tick, not a running total.
    pub fn set_atlas_paths(&mut self, counts: crate::atlas::layout::path_counters::Counts) {
        self.atlas_paths = counts;
    }

    /// Which layout paths the atlas took while producing this frame.
    ///
    /// This is the answer to "am I on the fast path?" being *visible* rather
    /// than inferred. A view that rebuilds every frame is now something a
    /// developer can see, and usually fix.
    #[must_use]
    pub const fn atlas_paths(&self) -> crate::atlas::layout::path_counters::Counts {
        self.atlas_paths
    }
}

/// `a |= b` over two parallel flag slices, or "all true" when they disagree
/// on length.
fn merge_flags<T>(
    current: &mut [T],
    previous: &[bool],
    mut merge: impl FnMut(&mut T, bool),
    mut set_all: impl FnMut(&mut T),
) {
    if current.len() != previous.len() {
        for slot in current.iter_mut() {
            set_all(slot);
        }
        return;
    }
    for (slot, prev) in current.iter_mut().zip(previous) {
        merge(slot, *prev);
    }
}

/// [`merge_flags`] for a pool whose dirty bit lives on the primitive itself.
fn merge_into<T>(
    current: &mut [T],
    previous: &[T],
    mut flag: impl FnMut(&mut T) -> &mut bool,
    mut read: impl FnMut(&T) -> bool,
) {
    if current.len() != previous.len() {
        for item in current.iter_mut() {
            *flag(item) = true;
        }
        return;
    }
    for (item, prev) in current.iter_mut().zip(previous) {
        let was = read(prev);
        *flag(item) |= was;
    }
}

/// Per-primitive paint fingerprints, retained across frames so a
/// [`RenderFrame`]'s dirty bits can say what actually changed instead of
/// asserting that everything did (RFC-0032 §R3 step 6).
///
/// # What it compares, and why that is the strong form
///
/// One `u64` per primitive per pool, hashed from the primitive's **resolved
/// values**, the numbers that reached the frame, not the expressions behind
/// them. [`apply`](Self::apply) re-hashes this frame's primitives, marks a
/// primitive dirty exactly when its hash differs from the hash at the same
/// pool position last frame, and keeps the new hashes for next time.
///
/// RFC-0032 §R1 chose value comparison over reactive attribute bindings
/// because a reactive graph can have a *missing edge*, and a missing edge
/// yields a false "clean", an element that renders in its new position and
/// answers taps in its old one. Comparing primitives closes that gap in its
/// strongest form: the thing being compared is the pipeline's own output, so
/// there is no attribute table to keep in sync and no classification to get
/// wrong. An attribute added tomorrow is covered the moment it changes a
/// pixel.
///
/// # Two rules
///
/// - **`f32`s are hashed through [`f32::to_bits`].** `NaN != NaN` would make a
///   primitive permanently dirty (wasteful, visible); `-0.0 == 0.0` would make
///   it permanently clean (silent, wrong). The second is the dangerous one.
/// - **A primitive's own `dirty` bit is never hashed.** It is an output of
///   this comparison; feeding it back in would make the answer depend on last
///   frame's answer.
///
/// # Structural changes
///
/// Pool positions shift when the tree's shape changes, so hashes stop lining
/// up and a wide region reports dirty. That is both the safe direction and the
/// correct one, after a structural change the frame really has changed
/// everywhere.
#[derive(Debug, Default, Clone)]
pub struct PaintDigest {
    solid: Vec<u64>,
    text: Vec<u64>,
    decorated: Vec<u64>,
    textures: Vec<u64>,
    canvas: Vec<u64>,
    /// Whether any frame has been digested yet. Until one has, every
    /// primitive is reported dirty, a first frame in which nothing is dirty
    /// would simply never be drawn.
    primed: bool,
}

impl PaintDigest {
    /// A digest that has seen no frames: the first [`apply`](Self::apply)
    /// reports everything dirty.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets everything, so the next [`apply`](Self::apply) reports every
    /// primitive dirty again.
    ///
    /// Call after anything that invalidates the *meaning* of a pool position,
    /// a hot reload, a new view, rather than letting positional comparison
    /// silently equate two unrelated primitives that happen to hash the same.
    pub fn reset(&mut self) {
        self.solid.clear();
        self.text.clear();
        self.decorated.clear();
        self.textures.clear();
        self.canvas.clear();
        self.primed = false;
    }

    /// Rewrites `frame`'s per-primitive dirty bits from a comparison against
    /// the previous frame's values, and retains this frame's for the next
    /// call.
    ///
    /// Ripples and backdrops are deliberately left alone: a live ripple
    /// re-samples its own radius and alpha every tick and a backdrop pane
    /// re-blurs whatever moved behind it, so both are always dirty by
    /// construction and the encoder already treats them that way. Vector
    /// glyphs are likewise untouched, a placeholder→resident atlas
    /// transition changes their *content* without changing any value here, and
    /// the encoder forces a draw on a fresh atlas upload for exactly that
    /// reason.
    pub fn apply(&mut self, frame: &mut RenderFrame) {
        let primed = self.primed;

        // Solids keep their dirty bit out of band, so they get their own loop
        // rather than a `set` closure over the same slice.
        self.solid
            .resize(frame.instances.len().max(self.solid.len()), 0);
        for (i, instance) in frame.instances.iter().enumerate() {
            let h = paint_hash::box_instance(instance);
            frame.instances_dirty[i] = !primed || self.solid[i] != h;
            self.solid[i] = h;
        }
        self.solid.truncate(frame.instances.len());

        // Text is the one pool whose value is not all inside the primitive: the
        // wrap width lives in a parallel array, and it decides where the lines
        // break, so it decides the pixels. It is hashed here with the line it
        // belongs to rather than left out of the comparison, which is the same
        // rule INV-26 states for a shape group's members.
        self.text.resize(frame.texts.len().max(self.text.len()), 0);
        for (i, line) in frame.texts.iter_mut().enumerate() {
            let h = paint_hash::text_line(line, frame.text_wrap.get(i).copied().flatten());
            line.dirty = !primed || self.text[i] != h;
            self.text[i] = h;
        }
        self.text.truncate(frame.texts.len());
        Self::diff(
            &mut self.decorated,
            &mut frame.decorated,
            primed,
            paint_hash::decorated,
            |d, dirty| d.dirty = dirty,
        );
        Self::diff(
            &mut self.textures,
            &mut frame.textures,
            primed,
            paint_hash::texture,
            // The encoder sets this bit itself the frame after an async decode
            // lands (M29), so a freshly-loaded image paints without a full
            // redraw. Never clear a bit that is already set.
            |t, dirty| t.dirty |= dirty,
        );
        Self::diff(
            &mut self.canvas,
            &mut frame.canvas_shapes,
            primed,
            paint_hash::canvas,
            |s, dirty| s.dirty = dirty,
        );
        self.primed = true;
    }

    /// One pool: hash each primitive, compare against the retained hash at the
    /// same position, write the verdict onto the primitive, and retain the new
    /// hash for next frame.
    fn diff<T>(
        previous: &mut Vec<u64>,
        items: &mut [T],
        primed: bool,
        hash: impl Fn(&T) -> u64,
        set: impl Fn(&mut T, bool),
    ) {
        previous.resize(items.len().max(previous.len()), 0);
        for (i, item) in items.iter_mut().enumerate() {
            let h = hash(item);
            let dirty = !primed || previous[i] != h;
            previous[i] = h;
            set(item, dirty);
        }
        previous.truncate(items.len());
    }
}

/// Hashes a group's member records by their **bits** (RFC-0031, INV-26).
///
/// `f32::to_bits`, not `f32`, for the reason the digest's own header gives and
/// RFC-0032's fingerprints already documented: `NaN != NaN` makes a group
/// permanently dirty, wasteful and visible, while `-0.0 == 0.0` makes it
/// permanently *clean*, which is silent and wrong. The second is the dangerous
/// one, and hashing the record's raw bytes is immune to both.
#[must_use]
pub fn shape_record_hash(members: &[ShapeRecord]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    // `ShapeRecord` is `Pod`, so its bytes *are* its value, there is no
    // padding to leak and no float comparison to get wrong.
    bytemuck::cast_slice::<ShapeRecord, u8>(members).hash(&mut h);
    h.finish()
}

/// Value hashes for the primitive types [`PaintDigest`] compares.
///
/// Written out per field rather than derived, for two reasons that are not
/// stylistic: `f32` has no `Hash`, and each primitive's own `dirty` flag must
/// be excluded because it is the *output* of the comparison these hashes feed.
mod paint_hash {
    use super::{BoxInstance, CanvasShape, DecoratedBox, TextLine, TextureSampler, Transform};
    use std::hash::{Hash, Hasher};

    fn hasher() -> rustc_hash::FxHasher {
        rustc_hash::FxHasher::default()
    }

    fn f32s(h: &mut rustc_hash::FxHasher, values: &[f32]) {
        for v in values {
            v.to_bits().hash(h);
        }
    }

    fn transform(h: &mut rustc_hash::FxHasher, t: &Transform) {
        f32s(h, &t.translate);
        f32s(h, &t.scale);
        f32s(h, &[t.rotate]);
        f32s(h, &t.origin);
        f32s(h, &[t.opacity]);
    }

    fn base(h: &mut rustc_hash::FxHasher, b: &BoxInstance) {
        f32s(h, &b.rect);
        f32s(h, &b.color);
        f32s(h, &b.radii);
        f32s(h, &[b.smooth]);
        transform(h, &b.transform);
    }

    pub(super) fn box_instance(b: &BoxInstance) -> u64 {
        let mut h = hasher();
        base(&mut h, b);
        h.finish()
    }

    pub(super) fn text_line(t: &TextLine, wrap: Option<f32>) -> u64 {
        let mut h = hasher();
        // Content first: it is the field whose change is most expensive to
        // miss, because a stale glyph run is a stale *shape*, not a stale
        // colour.
        t.text.hash(&mut h);
        f32s(&mut h, &[t.x, t.y, t.font_size]);
        f32s(&mut h, &t.color);
        // The wrap width, which is not a field of the line at all (RFC-0005
        // default wrap keeps it in a parallel array). It breaks the lines, so
        // two runs that differ only in it are two different pictures.
        // `to_bits` through `f32s` for the reason every other float here goes
        // through it: `-0.0 == 0.0` would compare two different wrap widths
        // equal, silently.
        match wrap {
            Some(w) => {
                1u8.hash(&mut h);
                f32s(&mut h, &[w]);
            }
            None => 0u8.hash(&mut h),
        }
        h.finish()
    }

    pub(super) fn decorated(d: &DecoratedBox) -> u64 {
        let mut h = hasher();
        base(&mut h, &d.base);
        f32s(
            &mut h,
            &[
                d.border_width,
                d.shadow_dx,
                d.shadow_dy,
                d.shadow_blur,
                d.shadow_spread,
                d.opacity,
            ],
        );
        f32s(&mut h, &d.border_color);
        f32s(&mut h, &d.shadow_color);
        // The gradient, whole. Not a chosen subset of its fields: `axis()` is
        // by construction *the four floats the shader reads*, so a gradient
        // that changed cannot be judged clean, whatever the kind reinterprets
        // those lanes as. Hashing a hand-picked list is how a moving glow
        // centre, or a shifted `mid_pos`, becomes a change nobody repaints.
        match &d.gradient {
            Some(g) => {
                1u8.hash(&mut h);
                (g.kind as u32).hash(&mut h);
                f32s(&mut h, &g.from);
                f32s(&mut h, &g.mid);
                f32s(&mut h, &g.to);
                f32s(&mut h, &g.axis());
            }
            None => 0u8.hash(&mut h),
        }
        h.finish()
    }

    pub(super) fn texture(t: &TextureSampler) -> u64 {
        let mut h = hasher();
        t.src.hash(&mut h);
        (t.fit as u8).hash(&mut h);
        f32s(&mut h, &t.rect);
        f32s(&mut h, &t.radii);
        f32s(&mut h, &[t.smooth, t.opacity]);
        h.finish()
    }

    pub(super) fn canvas(s: &CanvasShape) -> u64 {
        let mut h = hasher();
        s.kind.hash(&mut h);
        s.cap.hash(&mut h);
        f32s(&mut h, &s.params);
        f32s(&mut h, &s.stroke_color);
        f32s(&mut h, &s.fill_color);
        f32s(&mut h, &s.dash);
        f32s(&mut h, &[s.stroke_width, s.dash_offset, s.opacity]);
        transform(&mut h, &s.transform);
        // INV-26 (RFC-0031): the group members live outside this primitive and
        // the shader reads them, so they are part of what decides its pixels.
        // `member_hash` is that data, folded in.
        //
        // `group_first` is deliberately *not* hashed. It is a pool position,
        // not a value: a group whose members shifted to a different offset but
        // hashed the same reads the same records and paints the same pixels, so
        // hashing it would report a false dirty on every frame that added a
        // group earlier in the list.
        s.group_mode.hash(&mut h);
        s.group_count.hash(&mut h);
        s.member_hash.hash(&mut h);
        f32s(&mut h, &[s.group_param]);
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether two 2-vectors agree within a tight float tolerance.
    fn approx(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    #[test]
    fn compose_with_identity_is_a_no_op() {
        let t = Transform {
            translate: [3.0, -4.0],
            scale: [1.5, 2.0],
            rotate: 0.3,
            origin: [10.0, 20.0],
            opacity: 0.8,
        };
        // An identity *outer* preserves the inner transform field-for-field.
        assert_eq!(Transform::IDENTITY.compose(&t), t);
        // An identity *inner* re-anchors the pivot to (0,0) but stays the same
        // mapping, check a couple of sample points rather than the fields.
        let reanchored = t.compose(&Transform::IDENTITY);
        for p in [[0.0, 0.0], [12.0, -7.0], [30.0, 40.0]] {
            let a = t.apply_point(p);
            let b = reanchored.apply_point(p);
            assert!(
                (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4,
                "identity-inner compose maps {p:?} the same: {a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn parent_scale_carries_a_child_point_and_scale() {
        // A parent scaling 2× about the origin (0,0) should map a child point at
        // (10, 5) to (20, 10) and double the child's scale, the group transform.
        let parent = Transform {
            scale: [2.0, 2.0],
            origin: [0.0, 0.0],
            ..Transform::IDENTITY
        };
        // Child is an unscaled box pivoting at its own centre (10, 5).
        let child = Transform {
            origin: [10.0, 5.0],
            ..Transform::IDENTITY
        };
        let composed = parent.compose(&child);
        assert!(approx(composed.scale, [2.0, 2.0]), "scale multiplies");
        // The composed transform, applied to the child pivot, lands where the
        // parent would have put it: 2×(10,5) = (20,10).
        assert!(approx(composed.apply_point([10.0, 5.0]), [20.0, 10.0]));
    }

    #[test]
    fn parent_translate_offsets_children() {
        let parent = Transform {
            translate: [7.0, -3.0],
            ..Transform::IDENTITY
        };
        let child = Transform {
            origin: [4.0, 4.0],
            ..Transform::IDENTITY
        };
        let composed = parent.compose(&child);
        // A pure translate just shifts the child's mapped geometry.
        assert!(approx(composed.apply_point([4.0, 4.0]), [11.0, 1.0]));
        assert!(approx(composed.scale, [1.0, 1.0]));
    }

    #[test]
    fn round_trip_preserves_all_fields() {
        let id = TargetId::new(0x1234_5678, 0xABCD, 0x9F00);
        assert_eq!(id.index(), 0x1234_5678);
        assert_eq!(id.generation(), 0xABCD);
        assert_eq!(id.kind(), 0x9F00);
    }

    #[test]
    fn maximum_values_do_not_overflow_neighbouring_fields() {
        let id = TargetId::new(u32::MAX, u16::MAX, u16::MAX);
        assert_eq!(id.index(), u32::MAX);
        assert_eq!(id.generation(), u16::MAX);
        assert_eq!(id.kind(), u16::MAX);
    }

    #[test]
    fn zero_id_has_all_zero_fields() {
        let id = TargetId::new(0, 0, 0);
        assert_eq!(id.as_raw(), 0);
        assert_eq!(id.index(), 0);
        assert_eq!(id.generation(), 0);
        assert_eq!(id.kind(), 0);
    }

    #[test]
    fn vector_instance_is_pod_and_round_trips_through_the_frame() {
        let v = VectorInstance::new(
            Rect::new(10.0, 20.0, 16.0, 16.0),
            Rect::new(0.0, 0.0, 0.25, 0.25),
            [1.0, 1.0, 1.0, 1.0],
            4.0,
            0,
        );
        // UV rect is stored as [u0, v0, u1, v1].
        let close = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
        assert!(close(v.atlas_uv_rect, [0.0, 0.0, 0.25, 0.25]));
        assert!(close(v.screen_rect, [10.0, 20.0, 16.0, 16.0]));
        // Pod: 15 × 4-byte fields, no padding (so it uploads zero-copy).
        assert_eq!(std::mem::size_of::<VectorInstance>(), 60);
        let _bytes: &[u8] = bytemuck::bytes_of(&v);

        let mut frame = RenderFrame::new();
        frame.push_vector(v);
        frame.push_atlas_upload(AtlasUpload {
            layer: 0,
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            id: 0,
            bytes: vec![0u8; 2 * 2 * 4],
        });
        assert_eq!(frame.vector_instances().len(), 1);
        assert_eq!(frame.atlas_uploads().len(), 1);
        frame.clear();
        assert!(frame.vector_instances().is_empty());
        assert!(frame.atlas_uploads().is_empty());
    }

    #[test]
    fn is_copy_and_cheap_to_clone() {
        const fn assert_copy<T: Copy>() {}
        assert_copy::<TargetId>();
        assert_eq!(std::mem::size_of::<TargetId>(), 8);
    }

    #[test]
    fn rect_contains_point_inside() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(r.contains(50.0, 30.0));
    }

    #[test]
    fn rect_does_not_contain_point_on_right_edge() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(!r.contains(110.0, 30.0), "right edge is exclusive");
    }

    #[test]
    fn rect_does_not_contain_point_outside() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(!r.contains(0.0, 0.0));
    }

    #[test]
    fn rect_union_of_disjoint_rects_covers_both() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(100.0, 200.0, 10.0, 10.0);
        let u = a.union(&b);
        assert_eq!(u, Rect::new(0.0, 0.0, 110.0, 210.0));
    }

    #[test]
    fn rect_union_with_self_is_identity() {
        let a = Rect::new(5.0, 5.0, 20.0, 30.0);
        assert_eq!(a.union(&a), a);
    }

    #[test]
    fn rect_union_is_commutative() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(-5.0, 3.0, 4.0, 50.0);
        assert_eq!(a.union(&b), b.union(&a));
    }

    #[test]
    fn rect_union_where_one_fully_contains_the_other_returns_the_larger() {
        let outer = Rect::new(0.0, 0.0, 100.0, 100.0);
        let inner = Rect::new(10.0, 10.0, 5.0, 5.0);
        assert_eq!(outer.union(&inner), outer);
        assert_eq!(inner.union(&outer), outer);
    }

    #[test]
    fn rect_union_of_overlapping_rects_merges_correctly() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 15.0, 15.0));
    }

    #[test]
    fn rect_union_of_zero_area_rects_covers_both_corners() {
        // A degenerate (zero-size) rect can still arise from a TextLine whose
        // heuristic bounds collapse to a point; union must not panic or
        // silently drop it.
        let a = Rect::new(0.0, 0.0, 0.0, 0.0);
        let b = Rect::new(50.0, 50.0, 0.0, 0.0);
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 50.0, 50.0));
    }

    #[test]
    fn render_frame_starts_empty() {
        let frame = RenderFrame::new();
        assert!(frame.rects().is_empty());
    }

    #[test]
    fn render_frame_clear_empties_rects() {
        let mut frame = RenderFrame::new();
        frame.push_rect(Rect::new(0.0, 0.0, 10.0, 10.0), false);
        frame.push_rect(Rect::new(10.0, 0.0, 10.0, 10.0), true);
        assert_eq!(frame.rects().len(), 2);

        frame.clear();
        assert!(frame.rects().is_empty());
        assert!(frame.dirty().is_empty());
    }

    #[test]
    fn target_kind_round_trips_through_target_id() {
        let id = TargetId::new(7, 3, TargetKind::AtlasNode as u16);
        assert_eq!(id.kind(), TargetKind::AtlasNode as u16);
        assert_eq!(id.index(), 7);
        assert_eq!(id.generation(), 3);
    }

    // ── Rect::contains edge cases ─────────────────────────────────────────────

    #[test]
    fn rect_contains_point_on_left_edge_is_inclusive() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(
            r.contains(10.0, 30.0),
            "left edge (x == rect.x) is inclusive"
        );
    }

    #[test]
    fn rect_contains_point_on_top_edge_is_inclusive() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(
            r.contains(50.0, 20.0),
            "top edge (y == rect.y) is inclusive"
        );
    }

    #[test]
    fn rect_does_not_contain_point_on_bottom_edge() {
        // Half-open: y == rect.y + rect.height is exclusive.
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(
            !r.contains(50.0, 70.0),
            "bottom edge (y == y + height) is exclusive"
        );
    }

    #[test]
    fn zero_size_rect_contains_nothing() {
        // A Rect with width=0 or height=0 has no interior; every point is outside.
        let zero_w = Rect::new(10.0, 10.0, 0.0, 50.0);
        assert!(
            !zero_w.contains(10.0, 20.0),
            "zero-width rect contains nothing"
        );

        let zero_h = Rect::new(10.0, 10.0, 50.0, 0.0);
        assert!(
            !zero_h.contains(20.0, 10.0),
            "zero-height rect contains nothing"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // comparing literal → stored literal, no arithmetic, always bit-exact
    fn rect_default_is_all_zeros() {
        let r = Rect::default();
        assert_eq!(r.x, 0.0);
        assert_eq!(r.y, 0.0);
        assert_eq!(r.width, 0.0);
        assert_eq!(r.height, 0.0);
    }

    // ── Viewport ─────────────────────────────────────────────────────────────

    #[test]
    #[allow(clippy::float_cmp)] // round-trip through Viewport::new: no arithmetic, bit-exact
    fn viewport_new_round_trips() {
        let vp = Viewport::new(1920.0, 1080.0);
        assert_eq!(vp.width, 1920.0);
        assert_eq!(vp.height, 1080.0);
    }

    #[test]
    #[allow(clippy::float_cmp)] // Default-derived zero: no arithmetic, bit-exact
    fn viewport_default_is_zero() {
        let vp = Viewport::default();
        assert_eq!(vp.width, 0.0);
        assert_eq!(vp.height, 0.0);
    }

    #[test]
    fn viewport_is_copy() {
        const fn assert_copy<T: Copy>() {}
        assert_copy::<Viewport>();
        assert_eq!(std::mem::size_of::<Viewport>(), 8);
    }

    // ── RenderFrame ───────────────────────────────────────────────────────────

    #[test]
    fn render_frame_push_rect_preserves_order() {
        let mut frame = RenderFrame::new();
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 20.0, 30.0, 30.0);
        frame.push_rect(a, false);
        frame.push_rect(b, true);
        assert_eq!(frame.rects()[0], a);
        assert_eq!(frame.rects()[1], b);
    }

    #[test]
    fn render_frame_dirty_is_parallel_to_rects() {
        let mut frame = RenderFrame::new();
        frame.push_rect(Rect::new(0.0, 0.0, 10.0, 10.0), false);
        frame.push_rect(Rect::new(10.0, 0.0, 10.0, 10.0), true);
        frame.push_rect(Rect::new(20.0, 0.0, 10.0, 10.0), false);

        assert_eq!(frame.dirty(), &[false, true, false]);
        assert_eq!(frame.dirty().len(), frame.rects().len());
    }

    #[test]
    fn render_frame_starts_with_no_dirty_entries() {
        let frame = RenderFrame::new();
        assert!(frame.dirty().is_empty());
    }

    #[test]
    #[allow(clippy::float_cmp)] // x=99.0 stored from a literal, no arithmetic, bit-exact
    fn render_frame_clear_retains_capacity_for_reuse() {
        // Clearing a frame with N rects and immediately re-populating with N
        // rects should not reallocate. We verify correctness (no stale data),
        // not performance, allocation is observable only via Miri/asan.
        let mut frame = RenderFrame::new();
        for i in 0..10 {
            #[allow(clippy::cast_precision_loss)]
            frame.push_rect(Rect::new(i as f32, 0.0, 10.0, 10.0), false);
        }
        frame.clear();
        assert!(frame.rects().is_empty(), "clear must empty the frame");

        frame.push_rect(Rect::new(99.0, 0.0, 1.0, 1.0), true);
        assert_eq!(frame.rects().len(), 1, "can push after clear");
        assert_eq!(frame.rects()[0].x, 99.0);
        assert_eq!(frame.dirty(), &[true]);
    }
}

#[cfg(test)]
mod motion_tests {
    use super::*;

    fn spring(from: f32, to: f32, start_ms: u32) -> Motion {
        Motion {
            from,
            to,
            start_ms,
            curve: MotionCurve {
                kind: MotionCurve::SPRING,
                // The RFC-0010 A2 default: snappy 210/20, no initial velocity.
                params: [210.0, 20.0, 0.0],
            },
        }
    }

    #[test]
    fn spring_starts_at_from_and_approaches_to() {
        let m = spring(10.0, 3.0, 1_000);
        // At the start instant, the value is exactly `from`.
        assert!((m.sample(1_000) - 10.0).abs() < 1e-4);
        // Far in the future it has settled onto `to`.
        assert!((m.sample(1_000 + 10_000) - 3.0).abs() < Motion::DEFAULT_EPS_POS);
    }

    #[test]
    fn spring_velocity_starts_near_its_initial_velocity() {
        let mut m = spring(0.0, 100.0, 0);
        m.curve.params[2] = 50.0; // initial velocity
        assert!(
            (m.velocity(0) - 50.0).abs() < 2.0,
            "v(0) should be ~50, got {}",
            m.velocity(0)
        );
    }

    #[test]
    fn spring_is_unsettled_in_flight_and_settled_at_rest() {
        let m = spring(0.0, 100.0, 0);
        assert!(!m.is_settled(0), "a just-started spring is moving");
        assert!(
            m.is_settled(10_000),
            "a spring long past its start has settled"
        );
    }

    #[test]
    fn overdamped_and_critically_damped_springs_still_reach_the_target() {
        // Critically damped: c = 2*sqrt(k). k=100 -> c=20.
        let mut m = spring(0.0, 5.0, 0);
        m.curve.params = [100.0, 20.0, 0.0];
        assert!((m.sample(0)).abs() < 1e-4);
        assert!((m.sample(6_000) - 5.0).abs() < Motion::DEFAULT_EPS_POS);
        // Overdamped: c well above 2*sqrt(k).
        m.curve.params = [100.0, 60.0, 0.0];
        assert!((m.sample(0)).abs() < 1e-4);
        assert!((m.sample(6_000) - 5.0).abs() < Motion::DEFAULT_EPS_POS);
    }

    #[test]
    fn linear_curve_interpolates_over_its_duration() {
        let m = Motion {
            from: 0.0,
            to: 200.0,
            start_ms: 0,
            curve: MotionCurve {
                kind: MotionCurve::LINEAR,
                params: [200.0, 0.0, 0.0], // 200 ms
            },
        };
        assert!((m.sample(0) - 0.0).abs() < 1e-4);
        assert!((m.sample(100) - 100.0).abs() < 1e-3, "halfway at 100ms");
        assert!((m.sample(200) - 200.0).abs() < 1e-4, "arrived at 200ms");
        assert!((m.sample(999) - 200.0).abs() < 1e-4, "clamped past the end");
        assert!(m.is_settled(200));
    }

    #[test]
    fn ease_in_out_hits_its_endpoints_and_midpoint() {
        let m = Motion {
            from: 0.0,
            to: 1.0,
            start_ms: 0,
            curve: MotionCurve {
                kind: MotionCurve::EASE_IN_OUT,
                params: [100.0, 0.0, 0.0],
            },
        };
        assert!((m.sample(0) - 0.0).abs() < 1e-4);
        assert!((m.sample(100) - 1.0).abs() < 1e-4);
        // Symmetric ease passes through 0.5 at the temporal midpoint.
        assert!((m.sample(50) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn motion_is_pod_and_resting_is_settled() {
        // A resting motion never moves and reports settled immediately.
        let m = Motion::resting(42.0);
        assert!((m.sample(0) - 42.0).abs() < 1e-6);
        assert!((m.sample(9_999) - 42.0).abs() < 1e-6);
        assert!(m.is_settled(0));
        // POD round-trip (crosses the frame boundary as bytes).
        let bytes = bytemuck::bytes_of(&m);
        let back: Motion = *bytemuck::from_bytes(bytes);
        assert_eq!(back, m);
    }

    #[test]
    fn near_critical_and_negative_damping_stay_finite() {
        // A damping ratio a hair off critical must not divide by a vanishing
        // `wd`/`r1−r2` and blow up, the near-critical band routes it through
        // the division-free form.
        let mut m = spring(0.0, 10.0, 0);
        m.curve.params = [100.0, 20.05, 0.0]; // critical is c = 2√k = 20
        for t_ms in [0_u32, 1, 8, 100, 1_000, 6_000] {
            assert!(m.sample(t_ms).is_finite(), "sample must stay finite");
            assert!(m.velocity(t_ms).is_finite(), "velocity must stay finite");
        }
        assert!((m.sample(6_000) - 10.0).abs() < Motion::DEFAULT_EPS_POS);

        // Negative damping is clamped to zero, so the worst case is an undamped
        // (bounded) oscillation, never unbounded exponential growth.
        m.curve.params = [100.0, -50.0, 0.0];
        for t_ms in [0_u32, 100, 1_000, 5_000] {
            let v = m.sample(t_ms);
            assert!(
                v.is_finite() && v.abs() < 1.0e4,
                "clamped damping stays bounded"
            );
        }
    }

    // ── Looping & keyframes (RFC-0025) ───────────────────────────────────

    fn linear(from: f32, to: f32, ms: f32) -> Motion {
        Motion {
            from,
            to,
            start_ms: 0,
            curve: MotionCurve {
                kind: MotionCurve::LINEAR,
                params: [ms, 0.0, 0.0],
            },
        }
    }

    #[test]
    fn a_fixed_duration_curve_repeats_at_its_own_duration() {
        assert_eq!(linear(0.0, 1.0, 800.0).natural_duration_ms(0.002), 800);
    }

    #[test]
    fn a_springs_period_is_the_time_it_takes_to_settle() {
        let m = spring(0.0, 1.0, 0);
        let period = m.natural_duration_ms(0.002);
        // The period must be exactly where the motion comes to rest: still
        // moving a hair before, at rest a hair after.
        assert!(!m.is_settled_with_eps(period - 60, 0.002, 0.02));
        assert!(m.is_settled_with_eps(period + 60, 0.002, 0.02));
    }

    #[test]
    fn an_undamped_spring_falls_back_to_the_period_cap() {
        // `damping: 0` never settles, so there is no natural period to find.
        let mut m = spring(0.0, 1.0, 0);
        m.curve.params = [210.0, 0.0, 0.0];
        assert_eq!(m.natural_duration_ms(0.002), Motion::MAX_PERIOD_MS);
        // An instant curve still gets a wrappable (non-zero) period.
        assert_eq!(linear(0.0, 1.0, 0.0).natural_duration_ms(0.002), 16);
    }

    #[test]
    fn a_single_play_finishes_at_the_end_of_its_period() {
        let p = loop_phase(1_000, 400, RepeatMode::Once, false);
        assert!(!p.finished && (p.t_secs - 0.4).abs() < 1e-6);
        let p = loop_phase(1_000, 2_500, RepeatMode::Once, false);
        assert!(p.finished, "past its only play");
        assert!((p.t_secs - 1.0).abs() < 1e-6, "holds the final value");
        assert_eq!(p.iteration, 0);
    }

    #[test]
    fn an_infinite_repeat_wraps_forever_and_never_finishes() {
        for (elapsed, want_t, want_i) in [
            (0, 0.0, 0),
            (999, 0.999, 0),
            (1_000, 0.0, 1),
            (2_500, 0.5, 2),
        ] {
            let p = loop_phase(1_000, elapsed, RepeatMode::Infinite, false);
            assert!(!p.finished, "an infinite repeat never finishes");
            assert!((p.t_secs - want_t).abs() < 1e-6, "t at {elapsed}ms");
            assert_eq!(p.iteration, want_i);
        }
        // Hours in, the phase is still exact to the millisecond (integer wrap).
        let p = loop_phase(1_000, 4 * 3_600_000 + 250, RepeatMode::Infinite, false);
        assert!((p.t_secs - 0.25).abs() < 1e-6);
    }

    #[test]
    fn reverse_alternates_the_direction_each_iteration() {
        // Even iterations run 0 → period, odd ones period → 0.
        let fwd = loop_phase(1_000, 250, RepeatMode::Infinite, true);
        assert!((fwd.t_secs - 0.25).abs() < 1e-6);
        let back = loop_phase(1_000, 1_250, RepeatMode::Infinite, true);
        assert!((back.t_secs - 0.75).abs() < 1e-6, "counting back down");
        // The two directions meet exactly at the boundary, no jump.
        let a = loop_phase(1_000, 999, RepeatMode::Infinite, true).t_secs;
        let b = loop_phase(1_000, 1_001, RepeatMode::Infinite, true).t_secs;
        assert!(
            (a - b).abs() < 3e-3,
            "continuous across the turn: {a} vs {b}"
        );
    }

    #[test]
    fn a_counted_repeat_plays_exactly_n_times_then_holds() {
        // Three forward plays: still going inside the third, done after it.
        assert!(!loop_phase(100, 250, RepeatMode::Count(3), false).finished);
        let end = loop_phase(100, 300, RepeatMode::Count(3), false);
        assert!(end.finished && end.iteration == 2);
        assert!((end.t_secs - 0.1).abs() < 1e-6, "holds the far end");
        // Alternating plays end wherever the *last* one pointed: an even count
        // ends back at the start value, an odd count at the far end.
        let end = loop_phase(100, 999, RepeatMode::Count(2), true);
        assert!(end.finished && end.t_secs == 0.0);
        let end = loop_phase(100, 999, RepeatMode::Count(3), true);
        assert!(end.finished && (end.t_secs - 0.1).abs() < 1e-6);
        // `repeat: 0` is meaningless; it degrades to a single play, never to a
        // division by zero or an animation that "already finished at t = 0".
        assert!(!loop_phase(100, 10, RepeatMode::Count(0), false).finished);
    }

    #[test]
    fn a_looped_motion_samples_the_same_curve_at_the_phase_offset() {
        // The whole point of the phase: feeding it to `sample_secs` replays one
        // curve endlessly without ever touching `start_ms`.
        let m = linear(0.0, 100.0, 500.0);
        let at =
            |elapsed| m.sample_secs(loop_phase(500, elapsed, RepeatMode::Infinite, false).t_secs);
        assert!((at(0) - 0.0).abs() < 1e-4);
        assert!((at(250) - 50.0).abs() < 1e-3);
        assert!((at(500) - 0.0).abs() < 1e-4, "wrapped back to the start");
        assert!((at(750) - 50.0).abs() < 1e-3);
    }

    #[test]
    fn a_keyframe_cursor_walks_the_segments_in_order() {
        let percents = [0.0, 0.5, 1.0];
        let easings = [MotionCurve::LINEAR; 3];
        let c = keyframe_cursor(&percents, &easings, 0.25);
        assert_eq!((c.lo, c.hi), (0, 1));
        assert!(
            (c.t - 0.5).abs() < 1e-6,
            "halfway through the first segment"
        );
        let c = keyframe_cursor(&percents, &easings, 0.75);
        assert_eq!((c.lo, c.hi), (1, 2));
        assert!((c.t - 0.5).abs() < 1e-6);
        // Exactly on a step: the next segment has opened but not advanced, so
        // the blend still reads that step's own value.
        let c = keyframe_cursor(&percents, &easings, 0.5);
        assert_eq!((c.lo, c.t), (1, 0.0));
    }

    #[test]
    fn keyframe_progress_outside_the_table_parks_on_an_end_step() {
        // A sequence that starts at 20% holds its first value until then, and
        // holds its last value after the final step.
        let percents = [0.2, 0.8];
        let easings = [MotionCurve::LINEAR; 2];
        assert_eq!(keyframe_cursor(&percents, &easings, 0.0).hi, 0);
        assert_eq!(keyframe_cursor(&percents, &easings, 1.0).lo, 1);
        // Degenerate tables never panic and never divide by a zero span.
        assert_eq!(keyframe_cursor(&[], &[], 0.5).lo, 0);
        let c = keyframe_cursor(&[0.0, 0.5, 0.5, 1.0], &[MotionCurve::LINEAR; 4], 0.5);
        assert!(c.t.is_finite());
    }

    #[test]
    fn a_keyframe_segment_applies_its_own_easing() {
        // `easings[i]` is the easing *into* step `i`: the first segment eases
        // out (fast then slow), the second is linear.
        let percents = [0.0, 0.5, 1.0];
        let easings = [
            MotionCurve::LINEAR,
            MotionCurve::EASE_OUT,
            MotionCurve::LINEAR,
        ];
        let eased = keyframe_cursor(&percents, &easings, 0.25).t;
        assert!(eased > 0.5, "ease-out is ahead of linear at the midpoint");
        let plain = keyframe_cursor(&percents, &easings, 0.75).t;
        assert!(
            (plain - 0.5).abs() < 1e-6,
            "the linear segment is untouched"
        );
        // The shared easing helper agrees with the curve sampler.
        assert!((ease_progress(MotionCurve::LINEAR, 0.3) - 0.3).abs() < 1e-6);
        assert!(ease_progress(MotionCurve::EASE_IN, 0.5) < 0.5);
    }

    // ── ScrollView content clip (RFC-0005) ───────────────────────────────

    fn box_at(x: f32, y: f32) -> BoxInstance {
        BoxInstance {
            rect: [x, y, 10.0, 10.0],
            color: [1.0; 4],
            radii: [0.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        }
    }

    #[test]
    fn drawables_are_stamped_with_the_active_clip_and_none_outside() {
        let mut f = RenderFrame::new();
        f.push_instance(box_at(0.0, 0.0)); // unclipped
        let c = f.begin_clip(Rect::new(20.0, 20.0, 100.0, 80.0));
        f.push_instance(box_at(30.0, 30.0)); // inside the clip
        f.end_clip();
        f.push_instance(box_at(5.0, 5.0)); // unclipped again

        assert_eq!(f.solid_clips(), &[None, Some(c), None]);
        assert_eq!(f.clips().len(), 1);
        assert_eq!(f.clips()[0].rect, Rect::new(20.0, 20.0, 100.0, 80.0));
    }

    #[test]
    fn a_nested_clip_stores_its_intersection_with_the_parent() {
        let mut f = RenderFrame::new();
        let outer = f.begin_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        let inner = f.begin_clip(Rect::new(50.0, 50.0, 100.0, 100.0));
        f.push_instance(box_at(60.0, 60.0));
        f.end_clip();
        f.end_clip();

        // Inner clip is the intersection: (50,50) .. (100,100) → 50×50.
        assert_eq!(
            f.clips()[inner as usize].rect,
            Rect::new(50.0, 50.0, 50.0, 50.0)
        );
        assert_eq!(
            f.clips()[outer as usize].rect,
            Rect::new(0.0, 0.0, 100.0, 100.0)
        );
        assert_eq!(f.solid_clips(), &[Some(inner)]);
    }

    #[test]
    fn disjoint_nested_clips_intersect_to_an_empty_rect() {
        let mut f = RenderFrame::new();
        f.begin_clip(Rect::new(0.0, 0.0, 40.0, 40.0));
        let inner = f.begin_clip(Rect::new(100.0, 100.0, 40.0, 40.0)); // no overlap
        f.end_clip();
        f.end_clip();
        let r = f.clips()[inner as usize].rect;
        assert!(r.width == 0.0 || r.height == 0.0, "empty: {r:?}");
    }

    #[test]
    fn every_pool_carries_a_clip_slice_parallel_to_it() {
        let mut f = RenderFrame::new();
        let c = f.begin_clip(Rect::new(0.0, 0.0, 50.0, 50.0));
        f.push_instance(box_at(1.0, 1.0));
        f.push_vector(VectorInstance::new(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Rect::new(0.0, 0.0, 1.0, 1.0),
            [1.0; 4],
            4.0,
            0,
        ));
        f.end_clip();
        assert_eq!(f.solid_clips(), &[Some(c)]);
        assert_eq!(f.vector_clips(), &[Some(c)]);
    }

    #[test]
    fn clear_resets_the_clip_table_stack_and_slices() {
        let mut f = RenderFrame::new();
        f.begin_clip(Rect::new(0.0, 0.0, 10.0, 10.0));
        f.push_instance(box_at(0.0, 0.0));
        // Deliberately leave the clip open, then clear.
        f.clear();
        assert!(f.clips().is_empty());
        assert!(f.solid_clips().is_empty());
        // The stack is empty, so a fresh push is unclipped.
        f.push_instance(box_at(0.0, 0.0));
        assert_eq!(f.solid_clips(), &[None]);
    }

    // ── Z-layer marks (RFC-0017 layered draw batches) ──────────────────────────

    #[test]
    fn begin_layer_records_every_pool_cursor() {
        let mut f = RenderFrame::new();
        f.push_instance(box_at(0.0, 0.0));
        f.push_instance(box_at(1.0, 1.0));
        f.push_text(TextLine {
            x: 0.0,
            y: 0.0,
            text: "hi".to_string(),
            font_size: 12.0,
            color: [1.0; 4],
            dirty: true,
        });
        f.begin_layer();
        assert_eq!(
            f.layer_marks(),
            &[LayerMark {
                solid: 2,
                decorated: 0,
                texture: 0,
                vector: 0,
                text: 1,
                canvas: 0,
                ripple: 0,
                backdrop: 0,
                native: 0,
            }]
        );
    }

    // ── Canvas shape pool (RFC-0020) ────────────────────────────────────────

    #[test]
    fn push_canvas_shape_stamps_depth_clip_and_layer_cursor() {
        let mut f = RenderFrame::new();
        let c = f.begin_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        f.push_canvas_shape(CanvasShape {
            kind: CANVAS_SHAPE_ARC,
            params: [24.0, 24.0, 20.0, 0.0, std::f32::consts::PI, 0.0, 0.0, 0.0],
            ..CanvasShape::default()
        });
        f.end_clip();
        assert_eq!(f.canvas_shapes().len(), 1);
        assert_eq!(f.canvas_depths().len(), 1);
        assert_eq!(f.canvas_clips(), &[Some(c)]);
        // The shape advanced the global emission counter: its depth is nearer
        // than the cleared far plane.
        assert!(f.canvas_depths()[0] < DRAW_DEPTH_CLEAR);
        f.begin_layer();
        assert_eq!(f.layer_marks()[0].canvas, 1);
        f.clear();
        assert!(f.canvas_shapes().is_empty());
        assert!(f.canvas_depths().is_empty());
        assert!(f.canvas_clips().is_empty());
    }

    // ── Ripple pool (RFC-0023) ──────────────────────────────────────────────

    #[test]
    fn push_ripple_stamps_depth_clip_and_layer_cursor() {
        let mut f = RenderFrame::new();
        // Background first, then the ink: the ripple's stamped depth must be
        // strictly nearer, which is the RFC-0023 "above the background"
        // compositing guarantee.
        f.push_instance(box_at(0.0, 0.0));
        let c = f.begin_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        f.push_ripple(RippleInstance {
            rect: [0.0, 0.0, 40.0, 40.0],
            params: [20.0, 20.0, 10.0, 1.0],
            color: [1.0, 1.0, 1.0, 0.5],
            radii: [8.0; 4],
            t_translate: [0.0, 0.0],
            t_scale: [1.0, 1.0],
            t_rotate: 0.0,
            t_origin: [0.0, 0.0],
            depth: 123.0, // overwritten by the push
            smooth: 0.0,
        });
        f.end_clip();
        assert_eq!(f.ripples().len(), 1);
        assert_eq!(f.ripple_clips(), &[Some(c)]);
        let ripple_depth = f.ripples()[0].depth;
        assert!(ripple_depth < DRAW_DEPTH_CLEAR, "depth was stamped");
        assert!(
            ripple_depth < f.solid_depths()[0],
            "ink sits nearer than the background pushed before it"
        );
        f.begin_layer();
        assert_eq!(f.layer_marks()[0].ripple, 1);
        f.clear();
        assert!(f.ripples().is_empty());
        assert!(f.ripple_clips().is_empty());
    }

    // ── Backdrop pool (RFC-0023 §2) ─────────────────────────────────────────

    #[test]
    fn push_backdrop_stamps_depth_clip_and_records_the_behind_cursor() {
        let mut f = RenderFrame::new();
        // Two solids behind the glass, one after (a child).
        f.push_instance(box_at(0.0, 0.0));
        f.push_instance(box_at(1.0, 1.0));
        let c = f.begin_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        f.push_backdrop(BackdropInstance {
            rect: [0.0, 0.0, 80.0, 40.0],
            radii: [8.0; 4],
            blur: 20.0,
            tint: [1.0, 1.0, 1.0, 0.5],
            saturation: 1.8,
            quality: BLUR_QUALITY_AUTO,
            opacity: 1.0,
            transform: Transform::IDENTITY,
            depth: 123.0, // overwritten by the push
            smooth: 0.0,
        });
        f.end_clip();
        f.push_instance(box_at(2.0, 2.0));

        assert_eq!(f.backdrops().len(), 1);
        assert_eq!(f.backdrop_clips(), &[Some(c)]);
        // The barrier snapshot captures exactly what was emitted behind it.
        let mark = f.backdrop_marks()[0];
        assert_eq!(mark.solid, 2, "two solids sit behind the glass");
        assert_eq!(mark.backdrop, 0, "its own index in the backdrop pool");
        // Depth was stamped between the behind-content and the child.
        let d = f.backdrops()[0].depth;
        assert!(
            d < f.solid_depths()[1],
            "glass sits above the content behind"
        );
        assert!(f.solid_depths()[2] < d, "the child sits above the glass");
        f.begin_layer();
        assert_eq!(f.layer_marks()[0].backdrop, 1);
        f.clear();
        assert!(f.backdrops().is_empty());
        assert!(f.backdrop_marks().is_empty());
        assert!(f.backdrop_clips().is_empty());
    }

    #[test]
    fn canvas_shape_bounds_cover_stroke_and_caps() {
        // Circle r=20 at (24,24), stroke 4 → box must reach at least ±22 from
        // the centre on every side.
        let s = CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [24.0, 24.0, 20.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            stroke_width: 4.0,
            ..CanvasShape::default()
        };
        let b = s.bounds();
        assert!(b.x <= 24.0 - 22.0 && b.y <= 24.0 - 22.0);
        assert!(b.x + b.width >= 24.0 + 22.0 && b.y + b.height >= 24.0 + 22.0);

        // Line with round caps: endpoints overhang by half the stroke width.
        let l = CanvasShape {
            kind: CANVAS_SHAPE_LINE,
            params: [10.0, 10.0, 90.0, 10.0, 0.0, 0.0, 0.0, 0.0],
            stroke_width: 6.0,
            cap: CANVAS_CAP_ROUND,
            ..CanvasShape::default()
        };
        let lb = l.bounds();
        assert!(lb.x <= 7.0 && lb.x + lb.width >= 93.0);
        assert!(lb.y <= 7.0 && lb.y + lb.height >= 13.0);
    }

    // ── Shape groups (RFC-0031 §S4–§S6) ──────────────────────────────────

    /// A circle record at `(cx, cy)` with radius `r`.
    fn circle_record(cx: f32, cy: f32, r: f32) -> ShapeRecord {
        ShapeRecord::from_shape(&CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [cx, cy, r, 0.0, 0.0, 0.0, 0.0, 0.0],
            fill_color: [1.0, 0.0, 0.0, 1.0],
            ..CanvasShape::default()
        })
    }

    /// A group head in `FUSE` mode with smoothing radius `k`.
    fn group_head(k: f32) -> CanvasShape {
        CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            group_mode: GROUP_FUSE,
            group_param: k,
            fill_color: [1.0, 0.0, 0.0, 1.0],
            ..CanvasShape::default()
        }
    }

    #[test]
    fn shape_group_member_ranges_round_trip_and_never_overlap() {
        let mut f = RenderFrame::new();
        f.push_shape_group(group_head(16.0), &[circle_record(10.0, 10.0, 8.0)]);
        f.push_shape_group(
            group_head(4.0),
            &[
                circle_record(40.0, 10.0, 6.0),
                circle_record(52.0, 10.0, 6.0),
                circle_record(64.0, 10.0, 6.0),
            ],
        );

        let heads = f.canvas_shapes();
        assert_eq!(heads.len(), 2, "each group is exactly one instance");
        assert_eq!((heads[0].group_first, heads[0].group_count), (0, 1));
        assert_eq!((heads[1].group_first, heads[1].group_count), (1, 3));
        assert_eq!(f.shape_records().len(), 4);
        // The ranges are contiguous and disjoint, the property the shader's
        // `first + i` indexing depends on.
        assert_eq!(
            f.shape_records()[heads[1].group_first as usize].params0[0].to_bits(),
            40.0_f32.to_bits(),
        );

        f.clear();
        assert!(
            f.shape_records().is_empty(),
            "the record pool is a per-frame vector like every other one"
        );
    }

    #[test]
    fn a_frame_with_no_groups_produces_no_records() {
        // INV-22: nothing about RFC-0031 may cost a frame that does not use it.
        let mut f = RenderFrame::new();
        f.push_canvas_shape(CanvasShape {
            kind: CANVAS_SHAPE_CIRCLE,
            params: [10.0, 10.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ..CanvasShape::default()
        });
        assert!(f.shape_records().is_empty());
        let s = &f.canvas_shapes()[0];
        assert_eq!(s.group_mode, GROUP_NONE);
        assert_eq!((s.group_first, s.group_count, s.member_hash), (0, 0, 0));
    }

    /// **INV-26.** A group's members live outside the primitive `PaintDigest`
    /// compares, and the shader reads them, so they are part of what decides
    /// its pixels, and the head folds a hash of them.
    ///
    /// The failure this prevents is not hypothetical and not loud: a fusion
    /// group with a static `k` whose member circle moves has an *unchanged
    /// head*, would be judged clean, and would render the previous frame's
    /// shape. A `morph` group escapes it only by accident, because its
    /// parameter is the phase and the phase moves.
    #[test]
    fn a_group_whose_only_change_is_a_member_is_dirty() {
        let members_a = [
            circle_record(10.0, 10.0, 8.0),
            circle_record(30.0, 10.0, 8.0),
        ];
        // Only the second member moved. The head, mode, `k`, colours, rect,
        // is identical.
        let members_b = [
            circle_record(10.0, 10.0, 8.0),
            circle_record(24.0, 10.0, 8.0),
        ];

        let mut digest = PaintDigest::new();
        let mut first = RenderFrame::new();
        first.push_shape_group(group_head(16.0), &members_a);
        digest.apply(&mut first);

        let mut second = RenderFrame::new();
        second.push_shape_group(group_head(16.0), &members_b);
        digest.apply(&mut second);
        assert!(
            second.canvas_shapes()[0].dirty,
            "a moved member must repaint its group"
        );

        // Demonstrated red: with the member hash taken back out of the head,
        // the two frames' heads are byte-identical and the digest reports the
        // group clean, the silent stale-shape bug this field exists to stop.
        let mut without = PaintDigest::new();
        let mut a = RenderFrame::new();
        a.push_shape_group(group_head(16.0), &members_a);
        a.canvas_shapes[0].member_hash = 0;
        without.apply(&mut a);
        let mut b = RenderFrame::new();
        b.push_shape_group(group_head(16.0), &members_b);
        b.canvas_shapes[0].member_hash = 0;
        without.apply(&mut b);
        assert!(
            !b.canvas_shapes()[0].dirty,
            "the assertion above must be measuring the member hash and nothing else"
        );
    }

    #[test]
    fn a_group_that_did_not_change_stays_clean() {
        // The other half: the hash must not report dirty every frame, or it
        // would hand back the whole benefit of the digest for any app that
        // draws a group.
        let members = [circle_record(10.0, 10.0, 8.0)];
        let mut digest = PaintDigest::new();
        for frame in 0..3 {
            let mut f = RenderFrame::new();
            f.push_shape_group(group_head(16.0), &members);
            digest.apply(&mut f);
            if frame > 0 {
                assert!(
                    !f.canvas_shapes()[0].dirty,
                    "frame {frame}: nothing moved, so nothing repaints"
                );
            }
        }
    }

    #[test]
    fn the_member_hash_compares_bits_not_floats() {
        // The same trap RFC-0032's fingerprints documented, one construct
        // down. `-0.0 == 0.0` would make a changed group permanently *clean*,
        // which is the silent direction; `NaN != NaN` would make it
        // permanently dirty, which is merely wasteful.
        let positive = [circle_record(0.0, 10.0, 8.0)];
        let negative = [circle_record(-0.0, 10.0, 8.0)];
        assert_ne!(
            shape_record_hash(&positive),
            shape_record_hash(&negative),
            "-0.0 and 0.0 are different bytes and must hash differently"
        );
        let nan = [circle_record(f32::NAN, 10.0, 8.0)];
        assert_eq!(
            shape_record_hash(&nan),
            shape_record_hash(&nan),
            "the same NaN bits must hash the same, or the group is never clean"
        );
        assert_eq!(shape_record_hash(&[]), shape_record_hash(&[]));
    }

    #[test]
    fn a_group_never_carries_more_members_than_the_cap() {
        // The compiler diagnoses the ninth shape (§S5), so reaching here with
        // nine is a bug, and drawing eight of nine would hide it. Release
        // builds clamp rather than index out of range; debug builds assert.
        let members: Vec<ShapeRecord> = (0..MAX_GROUP_MEMBERS)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                circle_record(i as f32 * 10.0, 10.0, 8.0)
            })
            .collect();
        let mut f = RenderFrame::new();
        f.push_shape_group(group_head(16.0), &members);
        assert_eq!(f.canvas_shapes()[0].group_count as usize, MAX_GROUP_MEMBERS);
        assert_eq!(f.shape_records().len(), MAX_GROUP_MEMBERS);
    }

    #[test]
    fn begin_layer_dedups_an_empty_layer() {
        let mut f = RenderFrame::new();
        f.push_instance(box_at(0.0, 0.0));
        f.begin_layer();
        f.begin_layer(); // nothing emitted in between, must not add a mark
        assert_eq!(f.layer_marks().len(), 1);
        f.push_instance(box_at(1.0, 1.0));
        f.begin_layer(); // pool advanced, a real new layer
        assert_eq!(f.layer_marks().len(), 2);
    }

    #[test]
    fn a_frame_with_no_layers_has_no_marks_and_clear_resets_them() {
        let mut f = RenderFrame::new();
        f.push_instance(box_at(0.0, 0.0));
        assert!(f.layer_marks().is_empty());
        f.begin_layer();
        f.push_instance(box_at(1.0, 1.0));
        f.clear();
        assert!(f.layer_marks().is_empty());
    }

    // ── The dev-surface partition (RFC-0030 erratum "self-accounting") ─────

    fn line(text: &str) -> TextLine {
        TextLine {
            x: 0.0,
            y: 0.0,
            text: text.to_string(),
            font_size: 14.0,
            color: [1.0; 4],
            dirty: true,
        }
    }

    #[test]
    fn a_frame_with_no_dev_surfaces_reports_none_and_a_split_past_every_line() {
        // Every frame of a shipped app. "No dev text" has to be the default
        // reading, or the encoder would attribute the app's own shaping to a
        // profiler that is not running.
        let mut f = RenderFrame::new();
        f.push_text(line("the app"));
        assert_eq!(f.dev_base(), None);
        assert_eq!(
            f.dev_text_start(),
            f.text_count(),
            "the dev range is empty, not the whole pool"
        );
    }

    #[test]
    fn the_dev_base_splits_the_text_pool_where_the_overlay_started() {
        let mut f = RenderFrame::new();
        f.push_text(line("the app"));
        f.push_text(line("the app again"));
        let base = f.cursor();
        f.push_text(line("hud fps  60"));
        f.set_dev_base(base);
        assert_eq!(f.dev_text_start(), 2);
        assert_eq!(f.texts()[f.dev_text_start()..].len(), 1);
    }

    #[test]
    fn a_stale_dev_base_can_only_ever_mean_no_dev_text() {
        // The base is taken on the logic thread and read on the render thread
        // one pool-population later. A base past the end must degrade to "the
        // app owns everything", the reading that cannot invent overhead,
        // rather than panicking on an out-of-range split.
        let mut f = RenderFrame::new();
        f.push_text(line("the app"));
        let mut past_the_end = f.cursor();
        past_the_end.text = 99;
        f.set_dev_base(past_the_end);
        assert_eq!(f.dev_text_start(), 1);
    }

    #[test]
    fn clear_forgets_the_dev_base_so_a_recycled_frame_never_inherits_one() {
        // Frames are recycled. A base surviving into a frame the HUD did not
        // draw on would silently bill the app's own tail to the profiler.
        let mut f = RenderFrame::new();
        let base = f.cursor();
        f.push_text(line("hud"));
        f.set_dev_base(base);
        assert_eq!(f.dev_base(), Some(base));
        f.clear();
        assert_eq!(f.dev_base(), None);
    }
}

/// RFC-0032 §R3 step 6: the per-primitive paint comparison, on its own.
#[cfg(test)]
mod paint_digest_tests {
    use super::*;

    fn boxed(x: f32, color: [f32; 4]) -> BoxInstance {
        BoxInstance {
            rect: [x, 0.0, 10.0, 10.0],
            color,
            radii: [0.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        }
    }

    fn line(text: &str) -> TextLine {
        TextLine {
            x: 0.0,
            y: 0.0,
            text: text.to_string(),
            font_size: 12.0,
            color: [1.0; 4],
            dirty: true,
        }
    }

    /// Builds a frame carrying `boxes` and `texts`, and runs it through
    /// `digest`.
    fn digest_frame(
        digest: &mut PaintDigest,
        boxes: &[BoxInstance],
        texts: &[TextLine],
    ) -> RenderFrame {
        let mut f = RenderFrame::new();
        for b in boxes {
            f.push_instance(*b);
        }
        for t in texts {
            f.push_text(t.clone());
        }
        digest.apply(&mut f);
        f
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

    #[test]
    fn the_first_frame_is_entirely_dirty() {
        let mut d = PaintDigest::new();
        let f = digest_frame(&mut d, &[boxed(0.0, RED)], &[line("a")]);
        assert!(f.instances_dirty().iter().all(|x| *x));
        assert!(f.texts().iter().all(|t| t.dirty));
    }

    #[test]
    fn an_unchanged_frame_is_entirely_clean() {
        let mut d = PaintDigest::new();
        let _ = digest_frame(&mut d, &[boxed(0.0, RED)], &[line("a")]);
        let f = digest_frame(&mut d, &[boxed(0.0, RED)], &[line("a")]);
        assert!(f.instances_dirty().iter().all(|x| !x));
        assert!(f.texts().iter().all(|t| !t.dirty));
    }

    #[test]
    fn only_the_changed_primitive_is_dirty() {
        let mut d = PaintDigest::new();
        let before = [boxed(0.0, RED), boxed(20.0, RED)];
        let _ = digest_frame(&mut d, &before, &[line("a")]);
        let after = [boxed(0.0, RED), boxed(20.0, BLUE)];
        let f = digest_frame(&mut d, &after, &[line("a")]);
        assert_eq!(f.instances_dirty(), [false, true]);
        assert!(!f.texts()[0].dirty, "the text did not change");
    }

    #[test]
    fn a_text_edit_marks_only_that_line() {
        let mut d = PaintDigest::new();
        let _ = digest_frame(&mut d, &[], &[line("a"), line("b")]);
        let f = digest_frame(&mut d, &[], &[line("a"), line("changed")]);
        assert_eq!(
            f.texts().iter().map(|t| t.dirty).collect::<Vec<_>>(),
            vec![false, true]
        );
    }

    #[test]
    fn negative_zero_is_a_change() {
        // `-0.0 == 0.0`, so a naive comparison reports this primitive clean,
        // permanently, and with nothing on screen to suggest why.
        let mut d = PaintDigest::new();
        let _ = digest_frame(&mut d, &[boxed(0.0, RED)], &[]);
        let f = digest_frame(&mut d, &[boxed(-0.0, RED)], &[]);
        assert_eq!(f.instances_dirty(), [true]);
    }

    #[test]
    fn a_nan_does_not_make_a_primitive_permanently_dirty() {
        // `NaN != NaN`, so a naive comparison would redraw this one for ever.
        let mut d = PaintDigest::new();
        let nan = boxed(f32::NAN, RED);
        let _ = digest_frame(&mut d, &[nan], &[]);
        let f = digest_frame(&mut d, &[nan], &[]);
        assert_eq!(f.instances_dirty(), [false]);
    }

    #[test]
    fn a_shrinking_pool_does_not_leave_stale_hashes_behind() {
        let mut d = PaintDigest::new();
        let _ = digest_frame(&mut d, &[boxed(0.0, RED), boxed(20.0, RED)], &[]);
        let _ = digest_frame(&mut d, &[boxed(0.0, RED)], &[]);
        // Growing back to two must report the reinstated slot dirty rather
        // than matching it against a hash from two frames ago.
        let f = digest_frame(&mut d, &[boxed(0.0, RED), boxed(20.0, RED)], &[]);
        assert_eq!(f.instances_dirty(), [false, true]);
    }

    /// Builds a frame of wrapped lines and runs it through `digest`.
    fn digest_wrapped(digest: &mut PaintDigest, lines: &[(TextLine, Option<f32>)]) -> RenderFrame {
        let mut f = RenderFrame::new();
        for (t, wrap) in lines {
            f.push_text_wrapped(t.clone(), *wrap);
        }
        digest.apply(&mut f);
        f
    }

    #[test]
    fn a_line_that_only_changed_its_wrap_width_is_dirty() {
        // The wrap width is not a field of `TextLine`, it travels in a
        // parallel array, and it is what decides where the lines break. A
        // paragraph pinned to the top-left of a resized window changes nothing
        // else: same string, same origin, same colour, different picture.
        //
        // Left out of the hash, the line is reported clean, shaped correctly
        // (shaping is content-addressed) and then clipped out of the
        // incremental redraw region, which is a stale rectangle on screen with
        // no stale data anywhere to explain it.
        let mut d = PaintDigest::new();
        let _ = digest_wrapped(&mut d, &[(line("a long paragraph"), Some(200.0))]);
        let f = digest_wrapped(&mut d, &[(line("a long paragraph"), Some(300.0))]);
        assert!(f.texts()[0].dirty);
    }

    #[test]
    fn a_line_whose_wrap_width_is_unchanged_stays_clean() {
        let mut d = PaintDigest::new();
        let _ = digest_wrapped(&mut d, &[(line("a long paragraph"), Some(200.0))]);
        let f = digest_wrapped(&mut d, &[(line("a long paragraph"), Some(200.0))]);
        assert!(!f.texts()[0].dirty);
    }

    #[test]
    fn gaining_or_losing_a_wrap_width_is_a_change() {
        let mut d = PaintDigest::new();
        let _ = digest_wrapped(&mut d, &[(line("hello"), None)]);
        let f = digest_wrapped(&mut d, &[(line("hello"), Some(120.0))]);
        assert!(f.texts()[0].dirty, "unwrapped → wrapped");
        let f = digest_wrapped(&mut d, &[(line("hello"), None)]);
        assert!(f.texts()[0].dirty, "wrapped → unwrapped");
    }

    /// Every field of a gradient decides pixels, so every field has to reach
    /// the digest. Hand-picking a few of them is how a moving glow becomes a
    /// change nobody repaints, which is what this is about: the frame is
    /// published, the app looks frozen, and the next unrelated repaint reveals
    /// the update that had been sitting there.
    #[test]
    fn a_gradient_that_only_moved_its_centre_is_dirty() {
        let mut d = PaintDigest::new();
        let glow = |cx: f32| DecoratedBox {
            base: boxed(0.0, RED),
            gradient: Some(Gradient {
                kind: GradientKind::Radial,
                center: [cx, 0.5],
                radius: 0.8,
                ..Gradient::two_stop(0.0, RED, BLUE)
            }),
            ..Default::default()
        };
        let mut f = RenderFrame::new();
        f.push_decorated(glow(0.2));
        d.apply(&mut f);
        let mut f = RenderFrame::new();
        f.push_decorated(glow(0.8));
        d.apply(&mut f);
        assert!(f.decorated()[0].dirty);
    }

    /// The same claim for the fields a subset-hash left out before there were
    /// kinds at all: a band that slid along its ramp was already invisible.
    #[test]
    fn a_gradient_that_only_moved_its_middle_stop_is_dirty() {
        let mut d = PaintDigest::new();
        let band = |mid_pos: f32| DecoratedBox {
            base: boxed(0.0, RED),
            gradient: Some(Gradient {
                mid_pos,
                ..Gradient::two_stop(0.0, RED, BLUE)
            }),
            ..Default::default()
        };
        let mut f = RenderFrame::new();
        f.push_decorated(band(0.3));
        d.apply(&mut f);
        let mut f = RenderFrame::new();
        f.push_decorated(band(0.7));
        d.apply(&mut f);
        assert!(f.decorated()[0].dirty);
    }

    /// And its opposite: an unchanged gradient is still clean, or the whole
    /// incremental path is handed back for every gradient on screen.
    #[test]
    fn an_unchanged_gradient_stays_clean() {
        let mut d = PaintDigest::new();
        let card = || DecoratedBox {
            base: boxed(0.0, RED),
            gradient: Some(Gradient {
                kind: GradientKind::Conic,
                center: [0.5, 0.5],
                ..Gradient::two_stop(1.0, RED, BLUE)
            }),
            ..Default::default()
        };
        let mut f = RenderFrame::new();
        f.push_decorated(card());
        d.apply(&mut f);
        let mut f = RenderFrame::new();
        f.push_decorated(card());
        d.apply(&mut f);
        assert!(!f.decorated()[0].dirty);
    }

    #[test]
    fn reset_makes_the_next_frame_fully_dirty_again() {
        let mut d = PaintDigest::new();
        let _ = digest_frame(&mut d, &[boxed(0.0, RED)], &[line("a")]);
        d.reset();
        let f = digest_frame(&mut d, &[boxed(0.0, RED)], &[line("a")]);
        assert_eq!(f.instances_dirty(), [true]);
        assert!(f.texts()[0].dirty);
    }
}
