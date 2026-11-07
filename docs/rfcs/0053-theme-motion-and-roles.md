# RFC-0053: Theme motion, and the rest of Material's roles

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-06
- **Last updated:** 2026-11-06
- **Depends on:** RFC-0022 (themes, seeds), RFC-0010 and RFC-0025 (the `with`
  curves and modifiers), RFC-0050 (Byard Material, whose prerequisites 3, 4 and
  5 this closes).

> **Placement:** `docs/rfcs/0053-theme-motion-and-roles.md`

---

## Summary

A theme can name its motion. `[theme.motion]` declares springs and durations,
and byld reads them as tokens: `with t.springSpatialFast`, `anim.ease(t.durationMedium2)`.
`byard-base` ships Material 3 Expressive's six springs and its sixteen standard
durations, so a package that extends nothing still has them. A token in `with`
is checked like any other theme token: a misspelt one is `UnknownThemeToken`, one
of the wrong kind says what it is, and a token costs nothing until a `with` uses
it.

The same RFC completes `byard-base`'s colour roles. The base theme gains the
roles Material's scheme has and it lacked (`tertiary*`, `surfaceVariant`,
`errorContainer`, the two extreme surface containers, `surfaceDim`,
`surfaceBright`, the `inverse*` roles, `scrim`, `shadow` and the `*Fixed` roles),
the seed derives every one of them at the tone Material's scheme uses, the
contrast list the seed is tested against grows to match, and the modal scrim an
overlay draws is the theme's `scrim` instead of a constant. The expressive
corners and the `*Emphasized` type styles join the base too.

## Motivation

Byard Material's spike (RFC-0050, gap log entries 5 and 11) found two things a
package cannot fix from outside.

**Motion has no name.** A `[theme.motion]` table is accepted and nothing reads
it. `with` takes a literal curve only, so `anim.spring(stiffness: t.x)`,
a `let` and `with t.springSpatialFast` are all errors. The spike repeated
`stiffness: 800, damping: 0.6` twenty-five times across three files; an app that
wants every spatial spring a little softer cannot say so. Colour, type and shape
are tokens, and motion is the one part of a design system that is not.

**A seed stops short.** `seed = "#1E8E3E"` on top of `extends = "material"`
recolours `primary` and the surfaces and leaves the filled text field's
container, the inverse roles, `surfaceDim` and `scrim` the package's purple
grey, because the seed derives the roles `byard-base` knows and the base lacks
the rest. A package can declare all of them; a base that does not have them
means every package redeclares the same eleven.

Two smaller things ride along because they are the same file. The overlay's
scrim is black at 32% in the source, so a theme with a tinted `scrim` is
ignored by the one thing that draws it. And a sentence in the guide,
`anim.spring(stiffness: 200, damping: 0.6)`, is nearly undamped (see
"Damping", below), which matters the moment a theme ships Material's numbers.

## Guide-level explanation

```toml
[theme.motion]
# A spring: stiffness, and the damping as a ratio (1.0 settles without
# overshoot, less rings).
spatial_fast  = { stiffness = 800, damping_ratio = 0.6 }
spatial_slow  = { stiffness = 200, damping_ratio = 0.8 }
# A duration is a whole number of milliseconds.
short_2       = 100
```

```byld
View Sheet() {
    inject Theme as t
    var open = false
    Box #[
        translate: (0, open ? 0 : 320) with t.springSpatialFast,
        opacity: open ? 1 : 0 with anim.ease(t.durationShort2),
    ] { ... }
}
```

The name a byld program uses is the kind plus the key: a spring declared as
`spatial_fast` is `t.springSpatialFast`, a duration declared as `short_2` is
`t.durationShort2`. Which kind a key is, is read from its value: a table with
`stiffness` is a spring, a whole number is a duration.

`byard-base` ships, with no declaration:

| Token | Stiffness / damping ratio | |
|---|---|---|
| `springSpatialFast`, `Default`, `Slow` | 800 / 0.6, 380 / 0.8, 200 / 0.8 | for movement and size |
| `springEffectsFast`, `Default`, `Slow` | 3800 / 1.0, 1600 / 1.0, 800 / 1.0 | for opacity and colour |

and Material's sixteen durations, `durationShort1` to `durationShort4` (50 to
200 ms in steps of 50), `Medium1` to `Medium4` (250 to 400), `Long1` to `Long4`
(450 to 600) and `ExtraLong1` to `ExtraLong4` (700 to 1000).

A package's `[theme.motion]` is brought in by `extends`, like its colours, and
the project's own entries win. A token a package's views use and the consumer's
theme does not carry is `UnknownThemeToken` with the same "add
`[theme] extends = ...`" hint every other token has.

```
error[UnknownThemeToken]: theme `byard-base` declares no token `springSpatialFat`
error[InvalidAnimation]: `t.primary` is a colour, not a motion token
error[InvalidAnimation]: `t.durationShort2` is a duration, a curve is `anim.ease(t.durationShort2)`
error[InvalidAnimation]: `t.springSpatialFast` is a spring, not a duration
```

A token is the whole curve: `with t.springSpatialFast`. It does not take
modifiers (`repeat`, `delay`, `from`), because it is not a call. A looping or
delayed animation is written out with `anim.spring(...)` as before, and a
duration token can be used inside it: `anim.linear(t.durationLong2, repeat: infinite)`.

### The colour roles

`t.surfaceContainerLowest`, `surfaceContainerHighest`, `surfaceDim`,
`surfaceBright`, `inverseSurface`, `inverseOnSurface`, `inversePrimary`,
`scrim`, `shadow`, and `primaryFixed`, `primaryFixedDim`, `onPrimaryFixed`,
`onPrimaryFixedVariant` with the same four for `secondary` and `tertiary` all
resolve in both schemes in `byard-base` and in anything derived from a seed. A
modal overlay that does not say `scrim:` draws `t.scrim` at 32%.

`t.cornerLgIncreased` (20), `t.cornerXlIncreased` (32) and `t.cornerXxl` (48)
are shape tokens, and every type style has an `...Emphasized` twin
(`t.labelLargeEmphasized`): the same size, tracking and line height at the next
weight up.

## Reference-level explanation

### 1. The manifest

`[theme.motion]` is a known `[theme]` key (it was rejected as unknown). Each
entry is one of:

- a **spring**, an inline table with `stiffness` (a number greater than zero)
  and exactly one of `damping_ratio` (a number greater than zero) or `damping`
  (a number of at least zero). Any other key is an error naming the key and the
  token. `damping` is the engine's own coefficient, the `damping:` of
  `anim.spring`; `damping_ratio` is Material's.
- a **duration**, a whole number of milliseconds, zero to `u32::MAX`.

Anything else (a string, a float duration, a table without `stiffness`, both
damping spellings) is an error naming the entry and what it expected. The token
name is `spring` or `duration` followed by the `camelCase` of the key.

`Theme` gains a `motion` map from token name to a `MotionToken`, a spring
already reduced to the engine's `(stiffness, damping coefficient)` or a
duration in milliseconds. It takes part in `same_tokens` (a reload that changes
only a spring must apply it), in `token_names` (so the "declared by package X"
hint finds motion tokens), and in `extends`: `apply_theme_table` writes the
package's entries first and the project's after, exactly as for shapes.

### 2. Damping

The engine's spring (`byard-core`'s `spring_position`) takes `damping` as the
coefficient `c` of a unit mass, and the damping ratio is `c / (2 * sqrt(k))`.
`anim.spring()`'s default, `210/20`, has a ratio of about 0.69. Material's
`damping` is the ratio. The two are different quantities with one name, so the
theme spells them differently: `damping_ratio` is converted once, when the
theme is read (`c = 2 * ratio * sqrt(stiffness)`), and `damping` is taken as
written. `spatial_fast = { stiffness = 800, damping_ratio = 0.6 }` is therefore
the literal `anim.spring(stiffness: 800, damping: 33.94)`, and writing
`damping = 0.6` in a theme gets what `damping: 0.6` gets in a literal, a spring
that rings for several seconds. The guide's example is corrected.

Byld speaks both spellings too: `anim.spring(stiffness: 800, damping_ratio: 0.6)` is
the same spring as the token, and `damping:` stays the coefficient. A call takes one of
the two (both is `InvalidAnimation`), and a ratio is converted when the curve is
resolved, whatever order the arguments are written in.

### 3. Resolution

The grammar of `with` gains one form: a member of the injected theme, `t.name`,
in curve position, where `anim.` would be. It resolves while the curve is
resolved, to the same `Curve::Spring` a literal produces, so the animation
runtime, the repeat clock and the checks downstream see no difference: the test
that holds it samples a token and its literal at the same instants and compares
them.

A member in curve position whose base is not `anim` is a token reference, and is
looked up in the active theme:

- found, a spring: `Curve::Spring { stiffness, damping, v0: 0 }`.
- found, anything else: `InvalidAnimation` saying what it is (a duration, a
  colour, a typography or a shape token).
- not found: `UnknownThemeToken`, with `declared_by` and `extends` filled from
  the theme as for colours.

A duration token is also accepted wherever a duration literal is: the positional
duration of `anim.linear`, `anim.ease`, `anim.easeIn`, `anim.easeOut`,
`anim.easeInOut`, the `duration:` modifier (a keyframe track's included), and the step of `anim.stagger`. A
spring there, or any other token, is `InvalidAnimation`. A spring token is also
accepted as the base curve of `anim.stagger`.

The checks run where they ran: `validate_element` (so `on hover { ... }` state
blocks and `style` rules are covered), canvas shapes, and `snap_spring:`. They
take the theme. Without one a token is the old "expected an animation curve".
The base ident is not checked to be the injected `Theme` (the validator has no
scope); a record field in that position is reported as an unknown token, which
is the honest reading since a `with` takes no data.

Cost: the six springs and sixteen durations are a map of twenty-two small
entries per theme, built with the theme. A token is looked up when the curve is
resolved, which is where a literal is parsed, so a program that uses none pays
nothing per frame, and one that uses them pays one map lookup in place of
walking the argument list.

### 4. The roles

`ROLES` in the seed gains, `(role, palette, light tone, dark tone)`, Material's
tones:

| Role | Palette | Light | Dark |
|---|---|---|---|
| `surfaceContainerLowest` | neutral | 100 | 4 |
| `surfaceContainerHighest` | neutral | 90 | 22 |
| `surfaceDim` | neutral | 87 | 6 |
| `surfaceBright` | neutral | 98 | 24 |
| `inverseSurface` | neutral | 20 | 90 |
| `inverseOnSurface` | neutral | 95 | 20 |
| `inversePrimary` | primary | 80 | 40 |
| `scrim`, `shadow` | neutral | 0 | 0 |
| `{p}Fixed` | primary, secondary, tertiary | 90 | 90 |
| `{p}FixedDim` | the same | 80 | 80 |
| `on{P}Fixed` | the same | 10 | 10 |
| `on{P}FixedVariant` | the same | 30 | 30 |

The fixed roles are the same in both schemes, which is what fixed means. The
tertiary and error container roles the seed already derived are also added to
the base, where they were missing, with `surfaceVariant`.

`TEXT_PAIRS`, the list the seed's contrast test is held to (4.5:1 in both
schemes for seven seeds including a grey one), gains every new pairing that
carries text: the extreme and dim/bright surfaces with `onSurface`,
`inverseSurface` with `inverseOnSurface` and with `inversePrimary`, and each
fixed role with its `on` role and `onFixedVariant`. `byard-base`'s own values
are held to the same list by a test.

`byard-base`'s hand-written values for the new roles follow the neighbouring
ones in each scheme. The three families of fixed roles are derived from the
base's own `primary`, `secondary` and a tertiary at a sixty degree turn.

The modal scrim is `scrim` of the active scheme at 32% alpha, read when the
overlay is drawn, so a scheme flip changes it. `scrim:` on the overlay, and a
first child that fills the viewport, still replace it.

### 5. Shapes and type

`cornerLgIncreased` 20, `cornerXlIncreased` 32, `cornerXxl` 48 join the shape
scale. For each of the fifteen M3 type styles an `...Emphasized` twin is added:
same size, tracking and line height, with `Regular` raised to `Medium` and
`Medium` to `Bold`. `typo: titleLargeEmphasized` resolves the way `typo:
titleLarge` does, including when written bare.

## Drawbacks

- **Two spellings of damping.** `damping_ratio` and `damping` differ by a factor
  and a square root, and an author has to know which they hold. The alternative,
  one spelling, is worse: Material's numbers (ratios) or the engine's
  (coefficients) would be wrong for the other camp's tooling.
- **The kind is read from the value.** A key says nothing about being a spring or
  a duration until you see its value; the prefix of the byld name (`spring`,
  `duration`) is added, not written. A subtable per kind would be explicit and
  longer, and two `[theme.motion.*]` tables is what Material's own token
  documents avoid.
- **A token takes no modifiers.** `with t.springSpatialFast` cannot loop.
- **The base grows.** Twenty-two entries are in every theme, used or not.

## Rationale and alternatives

- **Rewriting the token to its literal expression** in the tree, before
  checking, would make every check and the runtime see an ordinary call. It
  allocates an expression per token use, or a pass over every view at load, and
  it hides what the author wrote from the diagnostics. Resolving to the curve
  does the same without either.
- **Letting `anim.spring(stiffness: t.x)` take theme numbers** was the other
  reading of the gap. It makes every argument a reactive expression, which a
  curve deliberately is not (a curve is a constant; RFC-0010). A token names the
  whole curve instead.
- **Easings.** Material's easing tokens are cubic beziers, and the engine's
  timed curves are three named families. A bezier curve is its own change in
  `byard-core`, with its own sampling; a token for "ease with 300 ms" is
  `anim.ease(t.durationMedium2)` today.

## Prior art

Material 3 Expressive's motion tokens (`md.sys.motion.spring.*`,
`md.sys.motion.duration.*`), Jetpack Compose's `MotionScheme`, which is the same
idea: a scheme owns the specs and components ask for `fastSpatialSpec()`.

## Resolved questions

### Q1, does a token refine at the use site?

**Question.** `t.springSpatialFast.with(damping: 1.0)`.

**Resolution: not now.** It needs a method on a token, a second kind of call in
`with`, and a decision about what `with` of a duration means. The two things the
spike needed (one place to retune a spring, no copies) do not. An author who
wants a variant declares it in the theme: `spatial_fast_settled = { ... }`.

### Q2, one `[theme.motion]` table or one per kind?

**Resolution: one**, as the Byard Material spike wrote it, with the kind read from
the value and the prefix added to the name.

## Future possibilities

- A `cubic_bezier` curve in `byard-core` and easing tokens on top of it.
- Refinement of a token at the use site (Q1).
- Motion tokens for `snap_spring` defaults per component class, once Material
  has the components that need them.
