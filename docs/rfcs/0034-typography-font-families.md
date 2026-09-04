# RFC-0034: Typography — custom font families

- **Status:** Active, the weight axis implemented 2026-09-03; font-family
  registration still open.

  **The RFC's premise was wrong about the starting point.** It describes
  `weight` as "a four-value enum" to be widened. `weight` was not a four-value
  enum — it was registered in the intrinsic catalogue and read by nobody. Both
  glyphon paths, measurement (`text.rs`) and rasterisation (`text_glyph.rs`),
  built `Attrs::new().family(Family::SansSerif)` with no weight on it. Two
  frames differing only in `weight` came back byte-for-byte identical, so every
  `weight: bold` in every example rendered regular. The work was implementing
  the property, not widening it.

  What landed: `weight` accepts the four keywords **and** any integer
  `100..=900`, and reaches the glyphs. It is threaded through the measurement
  cache key, the `TextSizer` layout callback and the RFC-0032 layout
  fingerprint, because a bold run is wider than a regular one — measurement
  that ignored it would size every heading to a box measured for a lighter
  face, and a fingerprint that ignored it would keep last frame's line breaks
  at the new weight.

  A `typo:` token's weight now applies too. The theme has carried a numeric
  weight axis since RFC-0022 and the projection dropped it, keeping only the
  size — `eval.rs` said so in a comment deferring it to "font byte-loading".
  A theme that called its headline semibold got a headline that was not.

  **Two deltas against this document:**

  - `weight` is its own `PropType::WeightAxis`, not `Int`. `Int` accepts a bare
    identifier, so `weight: chunky` type-checked and was silently ignored —
    which is the exact failure being fixed, reintroduced one layer up. The new
    type takes the keywords or `100..=900` and rejects the rest with a hint.
  - A weight cannot be a variable (`weight: w` for a view parameter). It is
    checked like the enum properties it replaces, which is consistent but
    narrower than a numeric axis suggests. Worth revisiting.

  **Still open: font families.** `font <name> = asset(path)`, the `font` prop,
  `fontdb` registration and the family half of the glyph cache key. The family
  remains hardcoded `Family::SansSerif` in both paths.
- **Author(s):** Briany4717
- **Created:** 2026-08-04
- **Last updated:** 2026-09-03

> **Placement:** `docs/rfcs/0034-typography-font-families.md` (replaces the earlier
> draft that still carried text roles).

---

## Summary

Byard's `Text` today exposes `weight` (a four-value enum: `thin`, `regular`,
`medium`, `bold`) and no way to pick a typeface. Every glyph rasterizes through
the one font glyphon was handed at startup. This RFC adds exactly two things, and
deliberately no more: (1) font-family registration through the asset/theme layer,
and (2) a `font` property on `Text` (and text-bearing widgets) that selects a
registered family, with `weight` widened to a numeric axis. Both are unavoidable
in core because font registration touches glyphon's `FontSystem` and the engine's
glyph cache key — a package cannot reach either. **Named text roles**
(`role: display`) are explicitly *not* core: they are compile-time sugar over
these props and belong to a theme convention or a package (see Resolved
questions). What stays in core is the minimal primitive that makes pairing a
rounded display face with a neutral UI face possible — which every screen in the
Aura Weather reference needs.

## Motivation

The Aura Weather screens use two distinct families: a geometric rounded display
face for the big titles (`10-Day Outlook`, `Air Quality`, the `72°`) and a
narrower UI sans for labels and body. Byard cannot express this. There is no
`font` prop on `Text` (`crates/byard-compiler/src/interp/intrinsics.rs`, the
`Text` arm carries only `TEXT_PROPS`), and no font-loading entry point exists in
`byard-core` — a search for `load_font` / `register_font` / `FontFace` returns
nothing. The result is that a pixel-exact rebuild of the reference is impossible
regardless of how good layout and colour are: the type is wrong on every screen.

A secondary problem: `weight` is a closed four-value enum. Rounded display faces
routinely ship `SemiBold` (600) and `ExtraBold` (800); the reference headings sit
around 700–800. Clamping to `bold` loses the design's intended weight.

## Guide-level explanation

**Registering families.** Fonts are assets, so they register the same way icons
and images do (RFC-0022 asset distribution). A theme declares them:

```
theme {
    font display  = asset("fonts/Quicksand[wght].ttf")   // variable font
    font body     = asset("fonts/Inter-Roman.ttf")
    font mono     = asset("fonts/JetBrainsMono.ttf")
}
```

**Using them.** `Text` gains a `font` prop that selects a registered family:

```
Text("10-Day Outlook") #[font: display, size: 56, weight: 700]
Text("Extended forecast for New York, NY") #[font: body, size: 13, weight: 500]
Text("72") #[font: display, size: 56, weight: 800]
```

`weight` accepts a numeric value (`100`–`900`) in addition to the existing four
keywords, which map to `100/400/500/700`. A numeric weight on a variable font
sets the `wght` axis directly; on a static family it selects the nearest shipped
face and, failing an exact match, synthesizes nothing (we do **not** fake-bold).

Explicit props win over the theme default family. Missing a family falls back to
the theme's `body` family and emits one compile-time diagnostic, never a silent
square-box render.

**Roles are out of core scope.** Bundling family + size + weight under a name
(`role: display`) is pure compile-time sugar over the props above and adds no
capability the engine must own. It is left to a theme convention or a small
package (e.g. a `type` package exporting `Display`, `Title`, `Label` Views), so
the core carries only the irreducible font primitive.

## Reference-level explanation

**Asset side.** `font <name> = asset(path)` adds a `FontAsset { name, bytes,
variable_axes }` to the theme's asset table. At app start the engine hands every
registered `bytes` blob to glyphon's `FontSystem` (`fontdb`), recording the
returned family id in a `FamilyTable: HashMap<InternedStr, fontdb::ID>`. Variable
fonts keep their `fvar` axis ranges so a numeric `weight` can be validated at
compile time against the family's real `wght` range (out-of-range clamps with a
diagnostic).

**Compiler side.** `Text`'s intrinsic gains one prop: `font`
(`PropType::Enum` resolved against the registered family names, so a typo is a
compile error). `weight` changes from `PropType::Enum(WEIGHT)` to a small custom
type accepting either a keyword or an integer literal `100..=900`. No role
machinery enters the compiler; a package that offers roles does so by exporting
Views that set `font`/`size`/`weight`, which is ordinary RFC-0007 view
instantiation with nothing new underneath.

**Engine side.** The glyph cache key (`text_glyph.rs`) extends from
`(text, size, weight)` to `(text, size, weight, family_id)`. This is the only
runtime cost and it is a wider key, not a new pass. Font blobs live in the
theme's arena and share its lifetime; unmounting the last view that used a
theme-scoped font is already covered by the arena's linear release.

**Fallback chain.** Per-family fallback lists (for glyphs a family lacks, e.g. a
Latin display face asked to render `°` or a CJK label) resolve through glyphon's
existing system-font fallback. Byard adds no fallback policy of its own beyond
"missing registered family → theme `body` family".

## Drawbacks

- Bundling font files grows binary/asset size. Mitigated by the asset system
  already supporting external (non-embedded) assets, and by variable fonts
  collapsing many weights into one file.
- A numeric `weight` on a static family can land between shipped faces; we round
  to nearest and never synthesize, which can surprise a user who expected 600
  from a family that ships only 400/700. The diagnostic names the substituted
  face, so the surprise is visible at build time, not runtime.

## Rationale and alternatives

- **Why keep roles out of core?** The reference repeats three or four type
  treatments across screens, which is a real ergonomic need — but it is met
  entirely by userspace Views over the `font`/`size`/`weight` props, so paying
  for it in core surface would violate the minimal-core principle. Roles cost
  nothing to add later as a package and nothing is lost by deferring them.
- **Why extend `weight` to numeric rather than add `w100..w900` keywords?**
  Numeric maps directly onto the variable-font `wght` axis and reads the way
  designers already think. The four keywords stay as aliases for continuity.
- **Rejected: a global font swap without families.** Would let one screen change
  its font but not mix two faces on the same screen, which the reference
  requires on every screen.

## Prior art

SwiftUI `Font.custom(_:size:)` plus `.fontWeight`. Flutter's `TextTheme` with
`GoogleFonts`. CSS `@font-face` + `font-weight` numeric axis. glyphon/cosmic-text
already expose `fontdb` family registration and variable-axis selection, so the
engine-side lift is wiring, not new rasterization.

## Resolved questions

**Do named roles belong in core?** Options: a core `role` prop + theme role
table, vs leaving roles to userspace. Resolved: out of core. Reasoning: a role is
compile-time sugar over `font`/`size`/`weight` and adds no capability the engine
alone can provide, so putting it in core would violate the minimal-core principle
this restructure is built on; a theme convention or a `type` package delivers
identical ergonomics as plain RFC-0007 Views, and keeps the core surface to the
one primitive (font registration) that genuinely cannot live in a package.

**Keyword `weight` or numeric `weight`, not both?** Resolved: both, with the four
keywords as fixed aliases (`thin=100`, `regular=400`, `medium=500`, `bold=700`).
Reasoning: dropping keywords breaks every existing `.byd`; dropping numeric makes
variable fonts unusable. Aliasing is zero-cost and back-compatible.

**Fake-bold / fake-italic synthesis when a face is missing?** Resolved: never
synthesize. Reasoning: synthesized weights look wrong at exactly the large sizes
a display face is used for, and silently degrade pixel-exactness; a build-time
diagnostic naming the substitution is more honest than a smeared glyph.

**Compile error or runtime fallback for an unknown family name?** Resolved:
compile error on an unknown name (the `Enum` is closed over registered families),
runtime fallback only for a *registered* family that fails to load at startup.
Reasoning: a typo'd family name is a bug the author should see immediately;
matches how `inject` names and icon assets already fail at compile time.
