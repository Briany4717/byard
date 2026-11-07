# RFC-0057: Motion on colours and gradients

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0010 (`with`, and the colour channels of A3), RFC-0023
  (translucent colours animate their alpha), RFC-0025 (keyframes and loops, whose
  colour paths share the mixing this changes), RFC-0035 (gradient kinds), RFC-0050
  (Byard Material, whose state layers and colour changes are the reason).

> **Placement:** `docs/rfcs/0057-motion-on-colours-and-gradients.md`

---

## Summary

A `with` after a colour moves it, wherever the language takes a colour: `bg`,
`color`, `border`, `ripple`, `backdrop_tint`, `selection` and `scrim`, a canvas
shape's `fill`, `stroke` and text `color`, and the `color` of a shadow. A `with`
after a whole gradient moves it stop by stop. A fade to or from a translucent
colour no longer passes through a grey, because every colour and every stop is
mixed in OKLab (the perceptual space built on linear-light sRGB) with its alpha
premultiplied. Two gradients that cannot be matched stop for stop, because their
kinds or their number of stops differ, are a check error when the program says
so, and never a silent no-op. Nothing is added to a program that writes no
`with`, and a running colour animation does not allocate.

## Motivation

Byard Material draws a state layer, a translucent overlay on hover, press and
focus, and changes the colour of a selected chip, a switch track and a button
as its state flips. The spike for RFC-0050 found three things in the way
(gap 21 and gap 18 of its log):

1. `gradient: a ? (from: …, to: …) : (from: …, to: …) with anim.spring()` is
   `AttributeTypeMismatch`. So is the same line without the `with`: a gradient
   has to be a tuple literal, and a choice between two is not one. The state
   layer was written as a flat gradient with the alpha animated *inside* it,
   `fg.alpha(layer with anim.spring())`, which works, and is the workaround
   this RFC makes unnecessary.
2. `with` is accepted by the checker on every paint attribute, and understood
   by the evaluator on only some of the colour ones. `bg`, `color`, `border`
   and the others that are read through `eval_color_prop` blend in OKLab. A
   shape's `fill` and `stroke` and a shadow's `color` are read as plain
   integers, so a `with` after them runs the *integer* `0xFF0000` toward
   `0x00FF00` as a scalar: a sweep through colours nobody wrote, on a program
   `check` calls fine. That is the silent failure the language refuses
   elsewhere.
3. The colour blend itself mixes straight alpha. `0x00000000 → 0xFF5B8DEF`
   moves lightness from black to the target while the alpha rises, so halfway
   there the overlay is a darker, greyer colour than either end. A state layer
   is exactly a fade from transparent, and it shows.

```byld
// What a Material chip wants to say, and today cannot (gradient) or gets wrong:
Box #[bg: selected ? t.secondaryContainer : 0x00000000 with anim.spring(),
      gradient: pressed ? (from: t.primary.alpha(0.20), to: t.primary.alpha(0.20))
                        : (from: t.primary.alpha(0.00), to: t.primary.alpha(0.00))
                          with anim.easeInOut(150ms)] {}
```

## Guide-level explanation

Write `with` after any colour, as after any number:

```byld
View Chip(label: Str) {
    var selected = false
    var hover = false

    Box #[radius: 8, p: (6, 14),
          bg: selected ? 0xFFE8DEF8 : 0x00FFFFFF with anim.spring(),
          border: selected ? 0x00000000 : 0xFF79747E with anim.easeInOut(150ms),
          border_width: 1, tap => selected = !selected, hover => hover = $0] {
        Text(label) #[color: selected ? 0xFF1D192B : 0xFF49454F with anim.easeInOut(150ms)]
        // The state layer: a translucent overlay that fades in over the chip.
        Box #[width: 100%, height: 100%, radius: 8,
              bg: hover ? 0x141D192B : 0x001D192B with anim.spring()] {}
    }
}
```

A gradient is moved as a whole, the same way, and a `when`-style choice between
two gradients is allowed:

```byld
Box #[width: 200, height: 48, radius: 24,
      gradient: on ? (angle: 90deg, from: 0xFF5B8DEF, to: 0xFF9B5BEF)
                   : (angle: 90deg, from: 0xFF2B2930, to: 0xFF49454F)
                     with anim.easeInOut(300ms)]
```

Each stop, and the angle, centre and radius, move between the two. The two
gradients must be the same shape: the same `kind`, and both with a `mid` or
neither. When they are not, and the program can see it, `check` says so:

```byld,fails
Box #[gradient: on ? (from: 0xFF0000, to: 0x0000FF)
                    : (kind: radial, radius: 0.8, from: 0xFF0000, to: 0x0000FF)
                      with anim.spring()]
```

```diagnostic
error[GradientShapeMismatch]: these gradients cannot be animated into each other: one is linear and one is radial
```

Split such a change into two boxes and fade their `opacity`, which is what a
change of shape means.

`fg.alpha(x with anim.spring())` keeps working: the animated number is the
alpha, and the colour is still the theme's.

A fade to or from a transparent colour keeps the other end's hue the whole way.
`0x00000000 → 0xFF5B8DEF` is the blue growing in, not a dark smear that turns
blue.

## Reference-level explanation

### 1. Where a colour moves

One routine, `eval_color_expr`, reads a colour from an expression: a
`with`-animated one drives `eval_animated_color`, an `anim.keyframes(…)` one
`eval_keyframe_color`, anything else is `eval_pure(..).as_int()`. It is what
`eval_color_prop` already did, extracted so every colour consumer asks the same
question:

| Attribute | Read by |
|---|---|
| `bg`, `border`, `color`, `ripple`, `backdrop_tint`, `selection`, `scrim` | `eval_color_prop` (unchanged call sites) |
| a shape's `fill`, `stroke`, a canvas `text`'s `color` | `shape_color` (new) |
| a shadow's `color` field (named or the fifth positional slot) | `eval_shadow_color` (new) |
| the `from`, `mid`, `to` of a gradient | `eval_gradient_stop` (as before) |

A `with` after a colour never reaches the scalar path again.

### 2. The mixing space

Colours blend in **OKLab with premultiplied alpha**. A colour `0xAARRGGBB`
becomes the four channels `(L·α, a·α, b·α, α)`, each driven by one
`byard_core::frame::Motion` as today, and the sampled channels are
un-premultiplied (`L = ΣL·α / α`, and so on) before they are packed back. At
`α = 0` the colour is undefined, so the packed result is `0x00000000`; the hue
the eye sees is whichever end has alpha, from the first frame to the last.

OKLab is a transform of linear-light sRGB, so the blend is in a linear-light
space as RFC-0010 A3 already chose, and what changes is only the alpha. For two
opaque colours `α = 1` throughout and the result is bit-identical to today's.
Keyframed colours (RFC-0025 §3) and the theme's light/dark cross-fade share the
same mixer, so they change in the same way, and for the same reason.

A spring overshoots. The un-premultiplied colour is clamped into gamut and the
alpha into `0..=1` after sampling; a premultiplied channel is never clamped
before it is divided.

### 3. A whole gradient

`gradient:` (and a canvas `stroke_gradient:`, and a path's `gradient:`, which
all read through `resolve_gradient_expr`) takes either

- a tuple, as now; or
- a conditional (`c ? a : b`, `if c { a } else { b }`, nested to any depth)
  whose every leaf is a tuple, followed or not by `with anim…`.

A conditional is resolved by evaluating its condition and parsing the chosen
leaf, so the second form without a `with` is simply the gradient it picks. That
alone is a language change: it turns an `AttributeTypeMismatch` into a value.

With a `with`, the chosen gradient is the *target*. The interpreter keeps, per
`with` node and per instance (the existing `AnimKey`), a fixed-size record: the
gradient the animation left from, the gradient it goes to, and one `Motion`
from `0` to `1` that carries the progress. A change of the target reseeds the
record: `from` becomes the gradient as currently sampled, so a reversal in
flight is continuous, as every other `with` is. A frame samples the progress
once and blends:

- the three stops, in OKLab with premultiplied alpha as in §2, from the linear
  RGBA the gradient already stores;
- `angle`, `center`, `radius`, `mid_pos` as plain numbers. `angle` is blended
  as the number written, not along the shortest turn, so a gradient turned
  from `0deg` to `270deg` sweeps the long way, as a rotation does;
- `offset` as the target's, because `gradient_offset` has its own `with`
  (RFC-0025's shimmer) and two animations of one number would fight.

The result is the one `Gradient` the box was going to draw anyway; the frame,
the encoder and the shader see no new type.

A gradient has `from` and `to`, and optionally `mid`, so it has **two stops or
three**. Two gradients can be blended when they have the same `kind` and the
same number of stops. `mid_pos` is a position, not a stop, and does not count.

### 4. When they cannot be blended

The checker collects the leaves of a `with`-animated gradient conditional. All
leaves are tuple literals, so their kinds (an absent `kind:` is `linear`) and
numbers of stops are known at check time, and if two differ the checker reports
`GradientShapeMismatch` at the `with`, naming what differs. There is no
`GradientShapeMismatch` without a `with`: the same two gradients as a plain
conditional are fine, they just jump, like any other value that is not animated.

A leaf that is not a tuple is `AttributeTypeMismatch`, as it is today: a
gradient is written out, not looked up (a `let` or a view parameter holding one
is the Future-possibilities entry on first-class gradients). So the
check-time test covers every program the evaluator accepts, and the evaluator
still guards the case in its own right: if the target ever has a different kind
or number of stops than the one it left from (a hot reload that edited the
program under a running animation is the one way), it *jumps* to the target
without animating, and the record is reseeded. It never draws a half-blended
gradient of two shapes.

### 5. `alpha`

`c.alpha(x)` is a number turned into a colour's alpha, and `x with anim…`
inside it is a number animation, which works today and stays. It is the
cheapest way to animate a state layer when the colour itself is fixed: one
`Motion` instead of four.

### 6. Cost

- **No `with`, no cost.** The new code runs behind `Expr::Animated` and
  `is_keyframes_call` tests the colour readers already made. A program that
  writes neither does one more `match` arm miss per colour read in a shape or a
  shadow, and nothing else. The map that holds a gradient's record is empty
  until the first animated gradient and allocates nothing before.
- **No allocation per frame.** A record is `Copy` and fixed in size, inserted
  into the map once, at the first frame its `with` is drawn, and removed when
  its element or branch goes, as the colour animations are. Sampling is
  arithmetic on arrays. The premultiply and un-premultiply are four multiplies
  and a divide.
- **The same frame discipline.** A running animation sets the interpreter's
  active flag until every channel settles, and the app asks for frames until
  then and not after.

## Drawbacks

- **Straight-alpha blends change.** Fading from transparent *black* to a colour
  now looks like the colour growing in; before it looked like a darker colour
  turning into it. The first is what was meant by every use the team found, but
  it is a different picture, and a hand-tuned `0x00000000` start someone made to
  get the dark look will not have it.
- **A gradient animates as a unit with one curve.** Per-stop curves are written
  as `from: a with …, to: b with …` inside the tuple, as before; mixing both on
  one gradient makes the two animations compose, and nothing stops a program
  from writing it.
- **One more error name.** `GradientShapeMismatch` is not a general
  mismatch, and exists because the message has to name what differs.
- **The record is larger than a colour's four `Motion`s.** Two gradients are
  about a hundred and fifty bytes, per animated gradient on screen.

## Rationale and alternatives

- **Straight alpha, with a fix-up for black.** Mixing in straight alpha and
  picking the opaque end's hue when the other end is fully transparent fixes
  the common case and breaks `0x01000000 → …`. Premultiplying is the
  definition of the correct blend, costs the same, and has no special cases.
- **Premultiplied linear RGB instead of OKLab.** It also has no fringe, and is
  what compositors do. OKLab is what every colour in this engine already blends
  in (RFC-0010 A3, keyframes, the theme), and two colours animated in two spaces
  would differ at mid-flight between an `Icon` and the `Box` behind it.
- **Animate a gradient's stops as separate `with`s (as today) and allow a
  conditional.** It works for `from:`/`to:` written as conditionals, and is what
  a state layer can do now. It does not move a `kind`-compatible *choice*
  between two gradients, which is what a theme token that is a gradient is, and
  needs a curve written three times.
- **Interpolate any two gradients by cross-fading their rendering.** It would
  need two draws and an offscreen pass per animated gradient. The principle is
  the lowest resource use, and a change of shape is rare and is well served by
  an opacity fade.
- **A check error for every conditional of gradients whose shapes differ.**
  Without a `with` they only jump, which is legitimate, and the error would
  forbid a program that is fine.
- **Make a gradient a first-class value now** (`let g = (…)`, view parameters).
  It is the right next step, it is a language change with its own questions,
  and the state layer does not need it.

## Prior art

- **CSS** interpolates `background-color` in sRGB with premultiplied alpha by
  default, for the reason in the Motivation, and interpolates gradients only
  when they have the same type and the same number of stops, otherwise it
  flips at 50%. CSS Color 4 lets an author choose OKLab, which is the space
  chosen here.
- **Flutter's `Color.lerp`** blends straight alpha in sRGB and is a known source
  of grey fringes on a fade from `Colors.transparent`, which is why the
  documentation for `Colors.transparent` warns about it.
- **Jetpack Compose** animates `Color` in OKLab with the alpha handled
  separately and offers `Brush` animation only by hand.
- **SwiftUI** animates a gradient's stops and colours when the two have the same
  number of stops, and jumps otherwise.

## Resolved questions

### Q1, which space and how is alpha handled?

**Question.** Straight or premultiplied alpha, in OKLab or linear RGB.

**Options.** (a) straight alpha in OKLab, as now; (b) premultiplied in OKLab;
(c) premultiplied in linear RGB.

**Resolution: (b).** The engine's colours already blend in OKLab; premultiplying
removes the fringe. (c) is as correct for alpha and would put two spaces in one
frame.

### Q2, what is the unit a gradient animates in?

**Question.** Each stop on its own `Motion`, or one progress for the whole.

**Options.** (a) a `Motion` per scalar (about twenty); (b) one progress and two
stored gradients.

**Resolution: (b).** The stops and the geometry arrive together, as one
gradient does, and a reversal needs one number to reseed rather than twenty.
The record is larger than a colour's, and is paid by a program that animates a
gradient.

### Q3, which parts of a gradient move?

**Question.** Only the colours, or also its geometry.

**Resolution.** The three stops, `angle`, `center`, `radius` and `mid_pos` move.
`offset` does not, because it has an animation of its own. The kind does not,
and is the thing that must match.

### Q4, what is a stop?

**Question.** A gradient always has three colours internally.

**Resolution.** The number written: `from` and `to`, with `mid` if there is one.
A written `mid` is a stop and its absence means there is none, even though the
engine stores the midpoint for the shader.

### Q5, is a difference in shape an error?

**Question.** Two gradients that cannot be blended under a `with`.

**Resolution.** An error where the program shows both (`GradientShapeMismatch`),
a jump where it cannot. Today every gradient is written out, so the program
always shows both; the jump is the evaluator's own guard, and is what the
language promises about a shape it cannot match.

## Unresolved questions

- Whether a `with` on a colour should be rejected on an attribute that is not a
  colour at all (`width: 0xFF0000 with …` is already a layout error, and
  `opacity: 0xFF0000 with …` is a number). Left as it is.
- Whether the keyframed gradient (`anim.keyframes` over gradients) should
  exist. It is not asked for.

## Future possibilities

- Gradients as first-class values (`let`, view parameters, theme tokens), after
  which a `with` on one needs no new rule: the check-time test above becomes a
  run-time one for a non-literal leaf, which the jump already covers.
- A gradient's `mid` appearing or disappearing animated as a stop sliding in
  from an end, instead of a jump.
- Mixing in other spaces (`anim.spring(space: srgb)`), if a design wants the old
  look.
