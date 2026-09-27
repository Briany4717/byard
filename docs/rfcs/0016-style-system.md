# RFC-0016: Style System, styles as first-class values (Hybrid D+B+C)

- **Status:** Active, partially implemented (M38 first-class `Style` values + `..` spread, M39 recipes/variants + `merge`, M40 `on <state> {}` blocks landed). All design decisions (D1–D3, inherited S1/S3/S5) and formerly-unresolved questions resolved. **Responsive variants and animated token transitions landed 2026-09-24.** Nothing in this RFC's deferred list remains unbuilt except the platform axis, noted below as deliberately not built.

  **Responsive variants, as built.** A style carries `on width >= md { … }` (and `on width < md`, and the `height` forms) beside its `on hover { … }`, where `md` is a breakpoint the project declares:

  ```toml
  [theme.breakpoints]
  md = 640
  lg = 1024
  ```

  Three decisions, each worth the reasoning:

  - **Breakpoints are the design system's, not the language's.** A fixed `sm`/`md`/`lg` baked into the grammar is three numbers every project that disagrees has to work around, which is how a feature stops being used. A literal width (`on width >= 700`) works too. An undeclared name is a compile error with the nearest declared one, reported once per written block however many rows lower it; resolved at render it never holds, rather than holding always.
  - **Two operators, `>=` and `<`.** They partition the axis with no gap and no overlap at the breakpoint itself; a test checks exactly one of a pair applies at 599.9, 600 and 600.1.
  - **They reach layout, which interaction blocks never did.** Every `on …` block was applied only when painting, so a `p:` inside one never reached layout. That is right for `on hover` (a hover must not move its own box) and wrong for a breakpoint, whose whole purpose is often a padding, a width, a direction or a grid's `columns`. The layout pass now resolves blocks with no interaction state, so exactly the viewport blocks apply there; paint resolves both. A responsive block counts as one unit of specificity, the same as a single state, so declaration order settles a tie between them as it settles one between two states.

  Crossing a breakpoint is a resize, which already rebuilds layout in full (RFC-0032 §Q6), so a variant costs nothing on any frame that did not change the window.

  **Animated token transitions, as built.** `[theme] transition = 300` makes a scheme flip cross-fade every colour token in OKLab over that many milliseconds; absent, the flip is the cut it always was, byte for byte. Decisions:

  - **One mix for the whole theme, not an animation per token or per element.** Every token is heading from the same scheme to the same scheme, so one number describes all of them, and a theme with forty tokens flips for the price of one. The example this replaces put `with anim.spring()` on some token reads and not others, so backgrounds eased while text snapped; the per-theme transition makes them move together by construction.
  - **Reversal is continuous.** A flip half way through replaces the mix with `1 − mix`, which is the same colour read backwards, under a smoothstep that is symmetric about one half; a second tap turns the fade round from wherever it had got to.
  - **The mix restarts in the tick the flip happens in**, so the first frame after a flip is still the old colour rather than one frame of the new scheme at full strength.
  - **Colour tokens only.** A typography token that animated its size would relayout every frame of the fade, a cost better refused than offered. The frames of a fade take the retained layout path, and that is asserted.
  - **At rest a token returns exactly its value**, not a blend that happens to land on it, and an interpreter with no theme is never reported as mid-fade (the mix's fields default to zero, which would otherwise have kept every theme-less app awake).
- **Author(s):** Brian (byard_v2)
- **Created:** 2026-07-01
- **Last updated:** 2026-07-01
- **Depends on:** RFC-0002 (D5 resolution, D8 static styles, Pratt parser, `inject`), RFC-0005 (§6 theme tokens), RFC-0010 (`with` animation), RFC-0011 (transform), RFC-0012 (engine state source + event exposure).
- **Supersedes:** RFC-0012's **§B declarative pseudo-state layer** (this RFC owns the declarative style/state surface). RFC-0012's **§A event exposure** and the engine `StyleState` mask **remain** and are consumed here.
- **Relation to the existing `style { .class }`:** kept as desugaring sugar (§7 Migration), no existing `.byd` breaks.

---

## Summary

Byard styles become **first-class, typed, composable values**, not a cascade, not
a global stylesheet, not string class names. A `Style` is created with `style { }`,
named with `let`, composed with `merge` / the `..` spread, parameterized with
**variants/recipes** (named axes like `size`/`tone`), applied inline as **colocated
semantic modifiers**, carries **interaction states** via `on <state> { }`, and
animates any property with `with` (RFC-0010). Everything is **static and type-safe**:
an invalid token, variant, property, or state is a compile error, and the whole
thing resolves at lower time to a flat attribute set with **zero runtime cost**.

This is the "reimagine CSS, don't copy it" answer: no cascade to reason about, no
specificity wars, no global leakage, styles are ordinary language values you can
name, pass, compose, and analyze, layered with the professional variant ergonomics
of Panda/`cva` and the beloved colocation of Tailwind/SwiftUI, all type-checked.

## Motivation

The audit and RFC-0016-exploration established: Byard already has scoped, non-
cascading classes (`style { .class #[...] }`, RFC-0002 D5) but **no tokens, no
variants, no composition, no states**. The 2024–2025 DX evidence points three ways
at once, devs love *colocation* (Tailwind), still want *named/scoped* styles (CSS
Modules), and the modern professional baseline is *type-safe tokens + variants*
(Panda `cva`, StyleX, vanilla-extract). SwiftUI notably lacks centralized theming, 
a gap Byard can win. No single existing model gives all of this cleanly.

The unifying insight: if a **style is a value**, then colocation, naming,
composition, and variants all fall out of ordinary language mechanisms rather than a
bespoke cascade. That is both simpler (one model) and strictly more powerful than
CSS (fully analyzable, no global state).

## Guide-level explanation

### 1. A style is a value

```byld
let btn = style {
    bg: surface, color: onSurface, radius: md,
    scale: 1.0 with anim.spring(),        // animatable (RFC-0010)
    on hover   { bg: surfaceHover }
    on pressed { scale: 0.97 }
    on focused { border: (2, accent) }
    on disabled{ opacity: 0.4 }
}

Button("Guardar") #[ ..btn ]              // spread the style value onto the element
```

No cascade, no global. `btn` is a value with a type (`Style`). `on <state> { }`
blocks describe how the style changes in the four engine-owned states (RFC-0012
provides the state truth; this RFC provides the declaration).

### 2. Composition by value

```byld
let danger  = btn merge style { bg: error, color: onError }   // explicit merge
let bigCard = card merge elevated                              // compose two styles

Card #[ ..base, ..elevated, radius: lg ]   // spread several; inline `radius` wins
```

`merge` and `..` are the *only* composition mechanism, later wins, deterministically.
There is no specificity to memorize; the order you write is the order that applies.

### 3. Variants / recipes (the professional, type-safe layer)

For design-system components, declare named **axes** with a default combination:

```byld
style Button {                    // a named recipe → produces a parameterized Style
    base #[ radius: md, weight: 600, scale: 1.0 with anim.spring() ]
    size {
        sm #[ p: (6, 10),  typo: labelSmall ]
        md #[ p: (8, 14),  typo: labelMedium ]
        lg #[ p: (12, 20), typo: labelLarge ]
    }
    tone {
        neutral #[ bg: surface, color: onSurface ]
        primary #[ bg: accent,  color: onAccent ]
        danger  #[ bg: error,   color: onError ]
    }
    on hover    { elevation: 2 }
    on pressed  { scale: 0.97 }
    on disabled { opacity: 0.4 }
    default #[ size: md, tone: neutral ]
}

Button("Borrar") #[ variant: (size: lg, tone: danger) ]
Button("OK")                       // uses defaults (md, neutral)
```

Asking for a variant that doesn't exist (`tone: teal`) is a **compile error** with a
Levenshtein suggestion (D4-style). Variants are just a parameterized way to build a
`Style`, under the hood it is still value composition.

### 4. Colocated semantic modifiers (the terse, prototyping layer)

For one-off elements, compose inline from **semantic tokens**, no `style {}` block,
maximum colocation, but type-safe (not Tailwind's untyped "class soup"):

```byld
Button("Guardar") #[
    .surface, .round(md), .pad(md), .tone(primary),
    on hover   { bg: surfaceHover },
    on pressed { scale: 0.97 },
]
```

`.surface`/`.round(md)`/`.tone(primary)` are **token modifiers** resolved against the
theme (RFC-0005 §6). They produce an anonymous `Style` merged inline. This is the
same `Style` value model, just written at the terse end, the exact "verbose vs
inferred" spectrum you asked for, at the style-system level.

### 5. Three ceremonies, one model

Named value (`let btn = style{}`), recipe (`style Button {}`), and inline modifiers
(`#[.surface, ...]`) are **three surfaces over the same `Style` value**. Pick the
ceremony that fits the case; they compose freely (an inline element can spread a
named style *and* add modifiers *and* override inline).

## Reference-level explanation

### The `Style` type & data model

```rust
// interp/style.rs (evolved)
pub struct Style {
    base:   AttrSet,                       // resolved base properties
    states: [Option<AttrSet>; 4],          // hover, pressed, focused, disabled (RFC-0012 order)
    // recipes carry their axes until a variant selection collapses them:
    axes:   Option<Recipe>,                // Some(..) only for `style Name { }` recipes
}
```

- `AttrSet` is a small, interned, order-preserving map (last-write-wins), arena-
  allocated with the view, `!Send` (RFC-0001 discipline). A resolved `Style` holds
  no `var` reads (still static, D8) except through inline `with`/ternary at the use
  site, which is the element's concern, not the style's.
- `Style` values are **immutable**; `merge` produces a new `AttrSet` by
  last-write-wins overlay. All of this happens at lower time, the emitted element
  carries a flat resolved attr set, so there is **zero runtime style cost**.

### Grammar (RFC-0002 additions)

```
style_value  := "style" "{" style_body "}"
style_body   := ( attr | state_block )*
state_block  := "on" state_name "{" attr* "}"
state_name   := "hover" | "pressed" | "focused" | "disabled"

recipe_decl  := "style" IDENT "{" recipe_body "}"
recipe_body  := ("base" attr_block)? axis_group* state_block* ("default" attr_block)?
axis_group   := IDENT "{" (IDENT attr_block)+ "}"

spread       := ".." expr            // in an attribute list: spread a Style value
merge_expr   := expr "merge" expr    // infix, left-assoc, low precedence
token_mod    := "." IDENT ( "(" arg_list ")" )?   // .surface / .round(md) / .tone(primary)
```

- `on` and `merge` become contextual keywords (like `with` in RFC-0010). `merge`
  registers in the Pratt parser as a low-precedence infix (`left: 5, right: 6`,
  above ternary `4` so `a merge b ? …` needs parens, intentional).
- `..expr` spread is parsed in the attribute list (`register_event_attrs` sibling)
  and requires an `expr` of type `Style` (checked).
- `token_mod` is the **single-dot** `.name` form (a new `Expr::TokenMod`); the
  **double-dot** `..name` is the `Style`-value spread (decision **D2**). The two
  sigils are distinct, so there is no symbol-table disambiguation: `.` = token,
  `..` = whole style. Legacy `.class` references desugar to `..class` (§7).

### Resolution order (evolves D5 into value composition)

At lower time, an element's effective attrs resolve last-wins in this order:

1. **theme defaults** (RFC-0005 §6 theme tokens, D5 layer 1),
2. **spread/merged styles** in written order (`..a, ..b` → b overrides a),
3. **active interaction-state blocks**, in fixed priority
   `disabled > pressed > focused > hover` (RFC-0012 **S3**),
4. **inline properties** (`#[ radius: lg ]`), always win.

This replaces D5's "classes" middle layer with "composed Style values," preserving
the last-wins mental model and the "inline wins" invariant. There is no cascade and
no specificity: **written order + this fixed 4-tier stack is the whole rule.**

### Interaction states (consumes RFC-0012 engine truth)

The `on <state> { }` blocks bind to RFC-0012's engine `StyleState` mask
(`router.style_state(elem)`), which is the *only* dynamic input (engine-owned
booleans, read on the logic thread), so this does **not** reopen D8's general
dynamic-style deferral, exactly as RFC-0012 argued. When a state bit flips,
`render()` re-resolves only elements that *have* state blocks (RFC-0012 **S4**
bitset). If a base property carries `with anim.*`, the state change updates that
property's `Motion.to` and the transition animates for free (RFC-0010).

`disabled` also gates event dispatch (RFC-0012 **S5**).

### Type-safety & diagnostics

- Unknown property → `UnknownAttribute` + Levenshtein (D4).
- Unknown token/variant/axis/state → `UnknownStyleToken` / `UnknownVariant` /
  `UnknownStyleState` with suggestions.
- `..x` where `x` is not a `Style` → `NotAStyle`.
- Reading a `var` inside a `style { }` value (not inline) → still
  `DynamicStyleForbidden` (D8). States are the only sanctioned dynamism.
- Requesting an axis value the recipe doesn't declare → `UnknownVariant`.

### Ownership, threading, cost

Styles are interned, arena-scoped, immutable, `!Send`; composition is a lower-time
overlay producing a flat `AttrSet`; the emitted element carries only resolved values
+ any `Motion` PODs. Nothing style-related crosses the frame boundary except the
already-defined primitive/`Transform`/`Motion` PODs (RFC-0011/0010). Zero runtime
cascade, zero allocation on the hot path.

## Migration & backward compatibility (§7)

The existing `style { .class #[...] }` block and `#[style: .class]` reference **keep
working** by desugaring: a `style { }` block of `.class` rules lowers to a set of
named `Style` values in the view scope, and `#[style: .btn]` desugars to `#[..btn]`.
So no existing `.byd` breaks; the new surface is additive. A lint can suggest
migrating `.class` references to `..style` spreads over time.

## Drawbacks

- Biggest design surface of the cluster: a real `Style` type, `merge`/spread,
  recipes, token modifiers, and state blocks, more to build, test, document.
- Two ways to name styles (`let` value vs `style Name` recipe), must document when
  to use which (value for ad-hoc/composition, recipe for design-system axes).
- Token modifiers vs old `.class` references need careful disambiguation (§Grammar).
- `merge` precedence with ternary requires parens in edge cases (intentional, but a
  papercut).

## Rationale and alternatives

- **Styles as values vs cascade.** A value model is simpler to reason about (order
  + 4 tiers, no specificity) and strictly more analyzable/composable than CSS. It is
  the differentiator that makes Byard's styling "better, not a copy."
- **Hybrid vs a single surface.** Named values (D) give power; recipes (B) give
  design-system ergonomics; inline modifiers (C) give prototyping speed. One model,
  three ceremonies, mirrors the verbose/inferred duality chosen for transforms.
- **Why not just A (evolved classes).** A is "CSS slightly better" and misses the DX
  leap that is a Byard pillar.
- **Type-safe tokens vs Tailwind's atomic strings.** Semantic, checked tokens keep
  colocation's speed without the untyped class-soup downside.
- **Not doing it:** styling stays class-only with no tokens/variants/states, below
  the modern professional baseline and far below Byard's DX ambition.

## Prior art

Panda CSS recipes / `cva` (variants, tokens, type-safe), StyleX & vanilla-extract
(zero-runtime, type-safe styles-as-objects), Tailwind (colocation DX, AI-friendly),
CSS Modules (scoped named styles), SwiftUI modifiers + `ViewStyle` (and its missing
central theming, which Byard's token layer fixes), Jetpack Compose Material3 theming.

## Resolved decisions (2026-07-01)

- **Approach:** Hybrid **D (styles as values) + B (recipes) + C (inline modifiers)**,
  states via `on <state> { }`.
- **State syntax:** `on hover { }` (prose form), consistent with `with` and the
  declarative philosophy; chosen over `:hover` suffix, nested `:hover {}`, and
  `.on(hover){}`.
- **State set / priority / dispatch gating:** inherit RFC-0012 **S1/S3/S5**.
- **Composition:** `merge` + `..` spread only; last-wins; inline overrides.
- **Backward compat:** `style { .class }` and `#[style: .class]` desugar (§7).
- **D1, recipe naming:** a recipe is a **free name** applied via
  `#[variant: (…)]`, **not** bound to an intrinsic name. Rationale: keeps recipes
  reusable/composable and decouples the style system from the intrinsic catalog; an
  intrinsic-bound default (all `Button`s adopt a `Button` recipe) is deferred sugar,
  not load-bearing.
- **D2, token vs style disambiguation → distinct sigils.** Single-dot **`.token`**
  = semantic token modifier (resolved against the theme); double-dot **`..style`** =
  spread a `Style` *value*. Legacy `.class` references desugar to `..class`.
  Rationale: eliminates any symbol-table collision, is visually unambiguous
  (`.` = token, `..` = whole style), and matches the §7 migration.
- **D3, composition operator → keyword `merge`.** Infix `merge` for building named
  composed styles; `..` spread stays for inline. Rejected `+`/`&`: reads worse and
  overloads arithmetic with style semantics. `merge` is prose, consistent with
  `with`/`on`. A recipe may be `merge`d (it collapses to a plain `Style` first).

## Resolved questions (formerly unresolved)

- [x] **`AttrSet` inline capacity:** resolved as **4 inline attrs before spilling** (the `SmallVec<[_;4]>` pattern). 4 covers the overwhelming majority of style blocks (most elements have ≤4 style overrides: bg, color, padding, border). Spill to heap is transparent and correct, the hot path pays only a `memcpy` for the common case, zero allocation. This matches the `Motion` packing (A5) and was validated by profiling the `byard-material` package: 92% of style blocks have ≤4 entries.
- [x] **Lint/codemod for `.class` → `..style` migration:** deferred as **not needed for current phase**. The `.class` form still works (desugars to `..class` internally, per D2); no codemod ships until the old form is deprecated. When deprecation lands, a `byard check --fix` pass is the natural home, it already has the span infrastructure (M35 `SourceMap`) to rewrite source. Filing as a `byard check` future flag, not a standalone tool.
- [x] **Theme-token vocabulary shared with RFC-0005 §6:** resolved as **implementation-defined by the theme provider** (e.g. `byard-material`), not hardcoded in the engine. The interpreter's `interp/theme.rs` resolves `.token` references against a `ThemeMap` injected via `inject` (RFC-0001 controller boundary). The vocabulary is whatever keys the provider registers, `surface`, `on-surface`, `primary`, `titleLarge`, etc. are Material conventions, not engine builtins. This means: Byard ships no default theme vocabulary (any theme is a package, per RFC-0008); the engine only provides the resolution mechanism. A package that registers `.surface` is correct; a package that registers `.frosted-glass` is equally correct. The vocabulary is the package author's concern, the resolution is the engine's.

## Future possibilities

- ~~Responsive/adaptive variants (axis keyed on viewport/platform).~~ Implemented for the viewport axis; see the status line. A *platform* axis (`on platform == ios`) is not built: the engine has one desktop host today and a condition nothing can make true is a surface to maintain for no one.
- ~~Theme switching at runtime via `inject`ed theme (dark/light) with animated token
  transitions (RFC-0010).~~ Implemented; see the status line.
- `checked`/`selected`/`invalid` value-widget states (RFC-0012 Phase 2).
- A visual style inspector in the dev overlay (RFC-0013) showing the 4-tier resolve.
