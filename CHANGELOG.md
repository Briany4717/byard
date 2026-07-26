# Changelog

All notable changes to Byard will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Byard uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- When writing entries use these categories:
     Added / Changed / Deprecated / Removed / Fixed / Security -->

## [Unreleased]

### Added

- **Real frame instrumentation (RFC-0030 §I1–§I3).** RFC-0013's zero-allocation
  profiler shipped complete and then went unused: `profile_scope!` had exactly
  one call site in the whole engine, and it was inside a `#[cfg(test)]` block,
  so `byard dev`'s telemetry block printed a header and a GPU row and nothing
  else. Six scopes now cover a frame end to end, each declared inside the
  subsystem it measures so no dependency edge moves:
  `interp.dispatch_events`, `interp.tick`, `interp.render` (`Interpreter`),
  `layout.taffy`, `encode.frame`, `relay.publish` (`Native`).

  - **`Sample` carries a nesting `depth`**, maintained by the RAII guard in the
    low byte of padding the type already reserved — `size_of::<Sample>()` is
    unchanged, so the block still crosses the frame boundary as plain `Pod`
    data (RFC-0001 §5). The guard *restores* the entry depth on drop rather
    than decrementing, so a leaked or unwound-through scope cannot skew the
    counter permanently. GPU samples set depth `0` explicitly: a pass resolves
    two frames later on a different timeline and does not nest in a CPU scope.
  - **New `SampleBlock` accessors:** `total_ns` (depth-0 inclusive — the frame),
    `self_ns` (inclusive minus direct children, recovered from the flat block in
    one reverse pass with no allocation), `sum_self_by_kind`, and
    `for_each_root`/`for_each_direct_child` for consumers rendering the scope
    forest.
  - **The `byard dev` block** now prints the scopes as an indented tree, parents
    first, with self-time as each row's headline number and inclusive time
    beside it where the two differ. `encode.frame` shares the render thread's
    ring with the `gpu.*` rows but is ordinary CPU wall-clock, so it is listed
    plainly, counted in the CPU total, and shown even where `TIMESTAMP_QUERY` is
    unavailable. Durations moved to three decimals: at two, a scope costing a
    few microseconds and a scope that had stopped being entered printed
    identically.
  - **Integration assertions** (`byard-core/tests/instrumentation.rs`,
    `byard-compiler/tests/instrumentation.rs`) fail if production stops entering
    a scope. A benchmark proves a path is fast, not that anyone walks it.
  - **`byard-compiler` gains a `telemetry` feature** (default on) forwarding to
    `byard-core`'s, so `--no-default-features` turns both off together.

  Example: `crates/byard-cli/examples/profiling`.

- **Navigation & routing (RFC-0026).** Two new intrinsics — `NavStack(path: navPath)`
  and `NavHost(active: tab)` — plus a `route "/detail/:id" {|params| … }` /
  `tab "home" { … }` sub-syntax, and the `navigate`/`back`/`replace` actions.
  Navigation state is a reactive `var` and nothing else: setting
  `navPath = "/detail/42"` *is* the push, setting it back *is* the pop. There is
  no navigation controller, no route object and no widget reference (RFC-0003).

  - **Route matching** compiles each pattern once at mount time and matches in
    declaration order, first match wins: literal segments, `:param` segments
    (`Str` in v1, exposed as `route.params.id` and through the optional
    `{|params| … }` binding), and a trailing `*` catch-all. `route.path` carries
    the concrete path. A malformed pattern (`/detail/:`, a non-final `*`, a
    repeated parameter) is a compile error; a path no route matches is a runtime
    warning that leaves the last matched screen up, reported once per path.
  - **State preservation** falls out of the entry model: a screen's View subtree
    is lowered the first time navigation reaches it and kept alive underneath
    whatever covers it, so its `var`s, scroll offsets and controllers are exactly
    where you left them on the way back. A multi-pop discards what it skipped;
    tabs are preserved permanently. Nothing is instantiated up front — a
    ten-route table costs ten compiled patterns, not ten View trees.
  - **Transitions** (`slide`, `slide_up`, `fade`, `none`) run two screens at once
    and place both from a single progress scalar, driven by a fixed-duration
    **monotone** ramp — decelerating into place for the positional ones,
    symmetric for the cross-fade. Deliberately not a spring: RFC-0010's default
    spring is underdamped, and a screen's arrival must not overshoot its own
    edge and wobble back. A duration ramp is bounded to `0..=1`, never reverses,
    and lands on exactly `1.0` at exactly its duration, so the frames stop the
    same instant the pixels do. The screen the navigation
    names stays in the container's normal flow and its transitioning partner is
    laid out absolutely over the same rect, so a transition costs two `f32` and
    an alpha folded into the transform every subtree already inherits — no
    relayout, no extra pass (INV-8), and the frames stop the moment it settles.
  - **`swipe_back: true`** is the Cupertino interactive edge pop: a drag from the
    leading 24 px follows the pointer in real time over the *real* preserved
    screen underneath, and on release the finger's progress hands over to the
    spring — commit past halfway, spring back otherwise, with nothing jumping at
    the hand-off.
  - **`deep_link: true`** accepts OS URL intents through
    `Interpreter::apply_deep_link`, which takes `byard://item/42`,
    `https://app.example/item/42` and a bare `/item/42` alike; `byard dev
    --deep-link <url>` delivers one at startup so the path is verifiable without
    a platform integration. A URL no route matches is rejected rather than
    navigating anything to a blank screen.
  - **Guards.** `max_depth` (default 10, `0` disables) refuses a push past its
    limit with a `PerfWarning::DeepNavStack` and reflects the refusal back into
    the navigation `var`, so app state and screen never diverge — a runaway push
    loop is flagged, not crashed. `route_change(e)` fires once a navigation
    settles. Nested stacks (one per tab) each keep their own independent history.

- **Gradient fills (RFC-0001 §3.1).** Two new paint properties on every box-path
  intrinsic — `gradient: (angle: 90deg, from: <color>, mid: <color>, to: <color>,
  mid_pos: 0.5)` and `gradient_offset: Float` — fulfilling the `DecoratedBox`
  pipeline's declared remit ("rectangles with border-radius, **gradients**,
  box-shadows"). The ramp is three-stop by design: two stops cover the ordinary
  fade (`mid` defaults to their midpoint), and the third is what makes a
  *highlight band* (transparent → bright → transparent) expressible, which is the
  shape a shimmer needs. It composites over the element's own fill with
  straight-alpha src-over — a translucent ramp brightens the surface, an opaque
  one paints it — is clipped by the element's border radius for free, and each
  stop is an ordinary colour value, so `with`/keyframed stops crossfade in OKLab
  like any other animated colour. `gradient_offset` shifts the ramp along its
  axis and **wraps**, so an animated offset (`gradient_offset: 1.0 with
  anim.linear(1.4s, from: 0.0, repeat: infinite)`) is a seamless travelling sweep
  with no extra elements: the RFC-0025 example's shimmer is now a gradient inside
  each skeleton bar rather than a rectangle floating over them. Engine surface:
  `frame::{Gradient, DecoratedBox::gradient}` + four instance slots on the
  `DecoratedBox` pipeline (inactive gradients cost one `misc.w` compare).

- **`restart: <expr>` on any animation (RFC-0025 §5).** A replay trigger: when the
  witness value changes, the animation's timeline starts over and its delays are
  honoured again, so a staggered entrance replays *in item order*. Without it a
  mount-time animation is observable exactly once — its endpoints never change,
  so nothing ever retargets it — which left RFC-0025's own stagger cascade
  unrepeatable. This is the reference-free equivalent of changing a `key`
  (RFC-0003 forbids handles), and it works on curves, keyframe sequences and
  staggers alike (`anim.stagger(spring(), 90ms, i, restart: attempt)`).

- **Looping & indefinite animations (RFC-0025).** `with anim.*(…)` grows the
  modifiers that turn a one-shot transition into continuous motion —
  `repeat: N | infinite` (`loop: true` is sugar for the latter),
  `reverse: true` (alternate plays run back-to-front, so one curve becomes an
  oscillation), `delay: <duration>` and `from: <value>` (the explicit second
  endpoint a loop needs) — plus two new curve surfaces:
  - **`anim.keyframes(0%: …, 50%: … ease_out, 100%: …, duration: 2s, loop: true)`**
    — a multi-step sequence *in value position* (it supplies its own values, so
    it is the property value rather than a `with` clause), with per-segment
    easing, capped at 8 steps (`TooManyKeyframes`). Steps may be scalars,
    colours (blended in OKLab), or coordinate pairs (interpolated
    component-wise, so `translate` keyframes work).
  - **`anim.stagger(spring(), 50ms, i)`** — sugar for `delay: i * 50ms` over a
    `for` loop's index, with entrance semantics: a retarget replays the cascade
    in order instead of cancelling the offset, while a plain `delay:` *is*
    cancelled by a retarget so a delayed transition can never overwrite a more
    recent interaction (RFC-0025 §5).

  Every curve family (`spring`, `linear`, `ease`, and now the individually
  addressable `ease_in`/`ease_out`/`ease_in_out`) repeats through one integer-
  millisecond clock: a fixed-duration curve wraps at its duration, and a spring
  wraps at its *analytic* settle time, so "restart when it settles" needs no
  per-frame state. Infinite animations stay in the active set (frames keep
  flowing at the display rate); a finite repeat holds its final value and lets
  the app idle; and an animation that stops being drawn — offscreen, or in a
  collapsed `when` branch — is **paused** and later resumes in phase rather than
  jumping (RFC-0025 §2), at zero cost while it is away. New grammar: percentage
  literals (`50%`), seconds durations (`1.5s`), and the `for i, item in items`
  index binding. Engine surface: `frame::{RepeatMode, LoopPhase, loop_phase,
  KeyframeCursor, keyframe_cursor, ease_progress, MAX_KEYFRAME_STEPS}` and
  `Motion::{sample_secs, natural_duration_ms}`.
  Example: `crates/byard-cli/examples/looping_animations`.

- **Backdrop blur & vibrancy (RFC-0023 §2).** Four new paint-time style
  properties — `blur: Float` (frosted-glass backdrop blur in logical px,
  clamped to 40), `backdrop_tint: Color` (blended over the blurred sample; the
  vibrancy pair with `blur`, a plain translucent wash without it),
  `blur_saturation: Float` (vibrancy boost, default 1.8) and `blur_quality:
  auto | high | low` (always the two-pass separable Gaussian — the tiers pick
  the base resolution: 0.75× forced high, 0.25× forced low, GPU-probed 0.5×
  or 0.25× on auto) — on every box-path intrinsic. `blur` is the Gaussian σ,
  the CSS `backdrop-filter: blur(N)` convention. A blurred element samples
  the scene behind it (its own background included), blurs it off-screen at
  adaptively reduced resolution (tap spacing stays gap-free at any radius —
  no ghosting), saturates and tints it, and draws the result as its
  background clipped to its border radius; children render crisply on top
  and overlapping panes stack naturally (painter's-order double blur). Both `blur` and `backdrop_tint` animate through the RFC-0010 `with`
  chokepoints (`blur: 0 with anim.spring()` + `on hover { blur: 16 }`).
  Engine surface: `frame::BackdropInstance` + `RenderFrame::{push_backdrop,
  backdrops, backdrop_marks, backdrop_clips}` + `LayerMark::backdrop`,
  `encoder::backdrop` (blur + composite pipelines), pass segmentation at
  backdrop barriers, `EncoderSubsystem::set_blur_auto_capable`, and a runtime
  `PerfWarning::OverlappingBlurs` diagnostic (≥ 3 stacked panes) surfaced by
  `byard dev`. Example: `crates/byard-cli/examples/frosted_glass`.

- **Material ripple ink (RFC-0023).** Four new paint-time style properties —
  `ripple: Color` (enables the effect and sets the ink colour),
  `ripple_active: Bool` (the trigger, typically `on pressed { ripple_active:
  true }`), `ripple_radius: Float` (max-radius override) and `ripple_duration:
  Int` (fade-out ms, default 300) — on every box-path intrinsic. A press spawns
  an ink circle at the exact tap point that expands (ease-out) to cover the
  element and fades linearly, composited *above* the element's background and
  *below* its children, always clipped to the element's border radius. The ink
  is alpha-composited over the surface — a light ink brightens a dark surface,
  a dark ink darkens a light one — and rapid taps spawn one ripple each,
  pooling where their circles overlap. Backed by a new `Ripple`
  render pipeline (the seventh): `frame::RippleInstance`,
  `RenderFrame::{push_ripple, ripples, ripple_clips}`, `LayerMark::ripple`,
  `encoder::ripple`, and `EventRouter::press_gesture` (the tap-point source).
  Live ripples participate in the RFC-0010 active-animation set (frames keep
  flowing until the ink fades) and in the incremental dirty-scissor union.
  Example: `crates/byard-cli/examples/ripple`.

### Changed

- **`SampleBlock::interpreter_tax_ns` is now self-time, not inclusive time
  (RFC-0030 §I2b).** `layout.taffy` is `Native` and nests strictly inside
  `interp.render`, which is `Interpreter`, so summing the interpreter bucket
  inclusively billed Taffy to the interpreter — and an AOT build still pays for
  layout in full. `project_aot`, which computes
  `total − interpreter + interpreter × ratio`, therefore returned a projection
  optimistic by the entire cost of layout, and would have pushed the RFC-0014
  JIT decision towards "the interpreter is the problem". This is a behavioural
  change to a public accessor; `sum_by_kind` is unchanged and still reports
  inclusive time, which remains correct for the disjoint `Gpu` bucket. Nothing
  could have observed the old value in practice, because nothing was
  instrumented — the fix lands before the bug could ever have been read.
- **`profile_scope!` now works outside `byard-core`.** The macro tested
  `#[cfg(feature = "telemetry")]` inside its own expansion, which is evaluated
  against the *calling* crate's feature set — so it silently compiled to nothing
  in every crate but the one that defined it. The feature is now resolved at the
  definition site by selecting between two macro definitions.

- **Text now wraps to its parent's width by default (RFC-0005).** A `Text` with
  no explicit `width` reflows to the width its container offers — like a block of
  text in a browser — instead of overflowing on a single line. This is done
  properly through Taffy's measure protocol: `Text` becomes a measured leaf that
  the layout atlas sizes via the shared, cached `TextMeasurer` during layout
  (`LayoutAtlas::add_text_leaf` + `compute_with_text`), so it re-wraps when its
  container resizes with no per-`Text` bookkeeping. `wrap: false` opts out to a
  single line; an explicit `width` still pins the wrap width. Previously wrapping
  required both `wrap: true` and an explicit `width`, so unbounded text overflowed
  — the catalog documented `wrap` as defaulting to `true`, but the leaf-measured
  model couldn't honour it. New engine surface: `atlas::TextLeaf`,
  `atlas::LayoutAtlas::{add_text_leaf, compute_with_text}`, and the
  `text::TextSizer` trait.

### Fixed

- **A settled app now genuinely idles at zero frames (RFC-0010 / RFC-0025 §2).**
  `Interpreter::has_active_animations()` existed and **nothing consulted it**:
  `byard dev` set `ControlFlow::Poll` once at start-up and requested a redraw on
  every event-loop iteration forever, so a completely static scene still cost a
  full core — the active-set settling that the whole animation design is built
  around had no consumer. The event loop now asks the host each iteration
  (`PlatformHost::wants_frames`, default `true` so no other host changes
  behaviour) and spins **only while something is in motion**, dropping back to
  `Wait` the moment everything settles. The logic thread publishes the flag
  across the boundary as an `AtomicBool` (INV-2) and wakes the loop on the rising
  edge — plus on a hot reload or a fresh error overlay, which change the frame
  with no input behind them, so live-reload stays immediate. Visible in
  `byard dev`: the once-a-second telemetry line stops printing when the scene
  settles and returns when it moves.

- **An over-large corner radius no longer deforms the box.** The rounded-rect SDF
  is only well-defined for `radius <= min(half_width, half_height)`; past it the
  distance field folds in on itself and the silhouette is pulled *inside* its own
  rect — visible on any pill button (`radius: 20` on a 33 px-tall button, the
  everyday case: its ends curved inward and looked pinched). The radius is now
  reduced to fit at the one place it is consumed, in every pipeline's
  `sd_rounded_box` (solid, decorated, ripple, backdrop, texture, canvas rect), so
  a too-large radius renders as the pill it is asking for — the CSS rule. Proven
  on a real GPU by `an_over_large_radius_is_reduced_to_a_pill_not_a_deformed_box`.

- **`Spacer` actually flexes (RFC-0005).** The catalog specifies `Spacer` as a
  "flexible gap" with `grow: Int` (default 1) and `basis: Int`; the
  implementation ignored both and laid out a fixed 0×12 leaf, so `Row { Text …
  Spacer Text … }` left the trailing item glued to the leading one instead of
  pushing it to the far end. It is now a real flex leaf (`LayoutAtlas::add_flex_leaf`):
  `basis` is its size before growing, `grow` its share of the free space, and
  both are ordinary reactive props.

- **An unmounted `when` branch now drops its animation state (RFC-0025).** "No
  separate stop-animation API — the animation lives and dies with its element"
  now holds literally: collapsing a branch forgets the animations inside it, so a
  spinner that comes back starts its turn again instead of resuming a stale phase.
  §2's offscreen rule is unchanged and now covers only what it was written for —
  an element that is still mounted but not painted pauses and resumes *in phase*.

- **Hit targets now follow the scroll offset (RFC-0005).** Interactive
  elements inside a `ScrollView` registered their hit rects at the laid-out
  position: after scrolling, a button reacted at the stale location and was
  inert at its on-screen one. The scroll displacement (which paints through
  the transform, deliberately excluded from hit-testing by RFC-0011/INV-8)
  now travels separately through the render walk and shifts every hit rect —
  handlers, hover/press regions, focusables — to its on-screen position,
  clipped to the scroll viewport (content scrolled out of view is no longer
  tappable). Ripple tap points and backdrop-blur sample regions map through
  the same displacement.

- **Colour `with` animations now animate the alpha byte.** A translucent
  colour transition (`backdrop_tint: 0x00FFFFFF` → `0x80FFFFFF` on hover)
  collapsed to an instant opaque colour: the OKLab interpolator dropped the
  alpha byte, and `0x00FFFFFF` is numerically identical to opaque `0xFFFFFF`.
  Hex colour literals written with more than six digits (the RFC-0005 §1
  `0xAARRGGBB` form) are now tagged at lex time, every alpha-aware colour
  consumer shares one auto-detect, and colour animations carry a fourth
  alpha channel — so translucent tints, ripple inks, and shape colours fade
  the way they read.

- **Enum keyword props can no longer be shadowed by a same-named `var`.** A
  keyword-valued prop (`snap: page`, `axis: horizontal`, `align: center`,
  `justify: …`, `direction: …`, `fit: …`, `alignment: …`, `anchor: …`) is a
  closed token set the type-checker reads as a bare identifier — but the runtime
  was resolving that identifier through the reactive environment, so a view
  declaring a `var` with the same name as the keyword silently evaluated the
  *variable* instead of the token. Most visibly, RFC-0021's `snap: page` carousel
  reflects its page through a `var page`, and `snap: page` next to `var page`
  read as the page index (`0`), disabling snapping entirely. Enum props are now
  read directly from the AST at the single resolution point, matching the
  checker: they can never be shadowed, and the read skips lowering an expression
  for a value that is always a compile-time keyword. Fixes the `scroll_snap`
  example (`cargo run -p byard-cli -- dev` in
  `crates/byard-cli/examples/scroll_snap`).
- **`DecoratedBox` inner border edge is now anti-aliased.** The rounded-rect
  SDF shader smoothed only the *outer* edge; the transition from the border to
  the (possibly transparent) interior used a hard threshold, leaving the inner
  edge jagged. Most visible on thin rings such as `RadioButton`. The inner edge
  now uses the same screen-space-derivative smoothstep as the outer edge, so
  both edges of any border are crisp at every size and DPI.

### Added

- **RFC-0021 advanced scroll behaviours — page snap, pagination, infinite
  scroll (first slice).** `ScrollView` gains the full RFC-0021 prop/event surface
  (`snap`, `snap_align`, `pull_refresh`, `refreshing`, `collapse_header`, `page`,
  `page_count`, `end_threshold`; events `end_reached`/`page_change`/`scroll_end`/
  `refresh`). Implemented in this slice: **`snap: page`** glides the offset to the
  nearest viewport-sized page with a **spring** (RFC-0010) when scrolling stops —
  on drag release *and* after wheel/trackpad scrolling goes quiet (there is no
  release event for a wheel). The settle is momentum-aware: it waits for the
  fling's shrinking deltas to actually stop before snapping, so the snap animation
  never fights an in-progress scroll, and any fresh scroll/drag cancels the glide
  so the user always takes over cleanly. It reflects the `page:` var **both ways**:
  `page` tracks the current page *continuously* as you scroll (wheel or drag,
  firing `page_change`), and setting `page` scrolls the offset to that page
  (edge-triggered so it never fights a drag). **`on_end_reached`** fires once when
  the visible bottom crosses `end_threshold` (debounced until the offset falls
  back, so appending items re-arms it) — the infinite-scroll trigger. All other
  props parse and validate. `snap: item` boundary snapping, pull-to-refresh
  (overscroll + indicator), and collapsing headers (layout-during-scroll +
  implicit `scroll_fraction`) are follow-up passes — each needs a new
  physics/layout subsystem. See `crates/byard-cli/examples/scroll_snap`.
- **RFC-0021 collapsing header.** `collapse_header: true` on a `ScrollView` pins
  its **first child** (the header) to the viewport top while the rest scrolls
  under it, and exposes an implicit reactive **`scroll_fraction`** binding
  (`0` = expanded, `1` = collapsed) scoped to that header's subtree — its children
  read it to interpolate their own size/opacity (e.g. a subtitle
  `opacity: 1.0 - scroll_fraction`). The fraction runs over the header's
  collapsible range (its natural height minus `collapse_min`, default 56) and
  clamps past it. Per RFC-0021's rationale this avoids sticky-positioning layout
  complexity: the header keeps its laid-out height and the collapse is expressed as
  ordinary reactive interpolation. New example
  `crates/byard-cli/examples/collapse_header`. Parallax
  (`collapse_parallax`) is accepted but not yet applied — a follow-up.
- **RFC-0021 pull-to-refresh.** `pull_refresh: true` makes a downward over-drag
  past the top of a `ScrollView` grow an elastic pull region (a diminishing-returns
  resistance curve); releasing past the threshold fires the `refresh` event and
  rests a default indicator (a ring drawn in the revealed gap that grows and fades
  in with pull progress). With a reflected `refreshing: Bool` the app owns the
  lifecycle — the engine sets it `true` on trigger and holds the indicator until
  the controller clears it, which springs the region away; without the binding the
  pull is a momentary trigger that retracts immediately. Works with or without an
  `offset` var (the pull region is engine state, not the scroll offset), and the
  spring reuses RFC-0010's `Motion`. `refresh` was parsed since the first slice but
  never fired; it fires now. Custom indicator slots (`pull_refresh: { … }`) and the
  platform-specific edge overscroll rubber-band remain follow-ups. See
  `crates/byard-cli/examples/scroll_snap`.
- **RFC-0021 `snap_spring` + fling-velocity projection (snap physics).** A
  `snap_spring: anim.spring(stiffness: …, damping: …)` prop overrides the snap
  glide's spring per `ScrollView` (reusing RFC-0010's curve grammar; a malformed
  curve is diagnosed and falls back to the default). And a settle now projects the
  fling: above 150 dp/s it advances one boundary in the fling direction — clamped
  to ±1 of the nearest, so a fast flick that stops short of the midpoint still
  turns the page while a moderate one never skips — reusing the same boundary
  geometry for `snap: page` and `snap: item`. Velocity is estimated from the
  offset change between scroll inputs over their timestamps, so it needs no extra
  gesture plumbing. This completes RFC-0021's snap-scrolling pillar. See
  `crates/byard-cli/examples/scroll_snap`.
- **RFC-0021 `snap: item` + `snap_align`.** `snap: item` settles the scroll to
  the nearest **direct-child boundary** instead of a fixed page, so a carousel of
  unequal-width cards snaps each card to the viewport edge. `snap_align` places
  the snapped item at the viewport's `start` (default), `center`, or `end`. The
  item boundaries are read from the laid-out child rects each render (offset is a
  paint-time translate, so layout positions are the natural content coordinates),
  aligned, and clamped to the scroll extent — the settle then picks the boundary
  nearest the current offset and reuses the same spring glide and momentum-aware
  quiet detection as `snap: page`. When the content is wrapped in a single
  `Row`/`Column` (the usual scroll layout), the items are that container's
  children. New engine surface: `LayoutAtlas::children`. `snap_spring` overrides
  and fling-velocity projection remain follow-ups. See
  `crates/byard-cli/examples/scroll_snap`.
- **RFC-0024 extended style states + combined selectors.** The style-state
  system (RFC-0012/0016) gains five engine-managed pseudo-states — `checked`
  (a value-widget's value is true), `selected` (the `selected:` prop, or a
  `RadioButton` whose `bind == value`), `invalid` (the `invalid:` prop),
  `indeterminate` (a `Checkbox`'s mixed prop), and `dragging` (the element being
  dragged past an 8px threshold) — plus **combined selectors**: `on focused+hover
  { … }` applies only when *all* its states are active. `selected`/`invalid` are
  universal opt-in props on any element; `checked`/`indeterminate` are mutually
  exclusive. Resolution is by specificity (a combined selector beats a
  single-state one) then declaration order. This completes RFC-0012's remaining
  states and lets `Checkbox`/`RadioButton`/`TextField` theme their states through
  `on <state>` blocks instead of duplicating the element tree with `when/else`.
  See `crates/byard-cli/examples/style_states`.
- **RFC-0018 `ZStack` intrinsic.** Overlapping children within the layout tree:
  all children occupy the same rect (painted in declaration order, last on top),
  the stack sizes to its largest child (the SwiftUI model), and
  `alignment: Align2D` (`center` default, plus the eight edge/corner tokens)
  positions children smaller than the stack. Implemented as a single-cell CSS
  grid, so it composes with the rest of the layout system; unblocks badges on
  avatars, a play button over a thumbnail, and floating action buttons over
  content. See `crates/byard-cli/examples/zstack`.
- **RFC-0018 `Grid` intrinsic.** A CSS-grid container backed by Taffy's grid
  mode. `columns`/`rows` take a template string (`"1fr 2fr 100"`,
  `"repeat(3, 1fr)"`, `auto`) parsed into engine tracks — a malformed template
  is a `CompileError::InvalidGridTemplate`. `gap`, plus per-axis `col_gap`/
  `row_gap`, space the cells. Children auto-place left-to-right, top-to-bottom by
  default, or place explicitly with the child props `col`/`row` (1-based grid
  lines) and `col_span`/`row_span`. Replaces the nested-`Row`/`Column` "wrapper
  hell" for two-dimensional layouts (dashboards, galleries, label+field forms);
  see `crates/byard-cli/examples/grid`.
- **RFC-0018 `RadioButton` intrinsic.** Single-selection within a group: each
  button carries a `value: Str` identity and a `bind: Str` to the shared group
  `var`, and is selected when `bind == value`. Tapping a button writes its
  `value` to the group var, so the previously selected sibling deselects
  reactively — automatic mutual exclusion, no explicit coordination (the
  standard group-var model). Focusable by default; arrow keys move selection
  within the group (Down/Right next, Up/Left previous, wrapping at both ends).
  Visual is an engine-owned outer ring plus an inner accent dot when selected;
  `bg` is the selected accent. Fires `change` with `bind:` write-back
  (RFC-0003 E1). See `crates/byard-cli/examples/radio_button`.
- **RFC-0018 `Checkbox` intrinsic.** A first-class boolean control with a
  distinct square identity from `Toggle`: reflected two-way `value`/`bind: Bool`
  (`true` = checked), an `indeterminate` mixed state, focusable by default
  (Space toggles), and a `change` event with `bind:` write-back (RFC-0003 E1).
  It owns its visuals — a rounded square that fills with the `bg` accent and
  shows an engine-drawn checkmark when checked, an outlined box (or a muted
  filled slot) when unchecked, and a horizontal dash when indeterminate. The
  container is a `DecoratedBox`, so a style can give it a `border`/`on checked
  { border }` (RFC-0024); `bg` is the checked accent, not a background slab
  (parity with `Toggle`/`Slider`). Replaces the Box+Text approximation design
  systems used for selection controls; see `crates/byard-cli/examples/checkbox`.
- **Binary arithmetic in `byld` (`+ - * /`).** Expressions can now compute:
  `width: base * 2 + 10`, `sweep: percent * 3.6 with anim.spring()`. Standard
  precedence, left-associative, Int/Float promotion; required by RFC-0020's
  reactive shape parameters and useful everywhere a prop is derived.
- **RFC-0020 `Canvas` intrinsic & path/shape primitives.** A fixed-size
  drawing surface whose children are declarative shape commands — `arc`,
  `circle`, `line`, `rect`, `bezier`, `path(d: …)`, and `text` — rendered by
  a new analytic-SDF GPU pipeline: resolution-independent anti-aliasing,
  stroke caps (`butt`/`round`/`square`), dash patterns with an animatable
  `dash_offset`, fills (including arc sectors), and per-parameter reactivity
  (`sweep: percent * 3.6` animates with no re-tessellation, no atlas churn).
  Complex SVG `path` data rasterizes through the existing MSDF pipeline at
  icon quality. Unblocks circular progress indicators, spinners, gauges, and
  custom decorations; see `crates/byard-cli/examples/canvas_shapes`.
- **RFC-0009 `VectorIcon` renders live in `byard dev`.** The `VectorMSDF`
  pipeline is now actually wired into the render loop (atlas + pipeline built
  at startup, drawn every frame) and participates in cross-pipeline paint
  order like every other primitive. A background dev-mode dispatcher generates
  each icon's field on its own worker thread the first time it's referenced;
  the call site paints a zero-opacity placeholder until the field lands, then
  the icon appears — no stall, no re-render trigger needed from the caller.
- **RFC-0009 vector/icon MSDF generator.** `byard_compiler::vector` turns an
  SVG icon into a multi-channel signed distance field: a structural complexity
  guardrail (rejects gradients, patterns, filters, and oversized path sets),
  and a generator that parses/normalizes with `usvg` and produces the field
  with a pure-Rust generator, deterministically and with sharp corners
  preserved at any scale.
- **RFC-0008 package ecosystem.** The `use` import surface with explicit
  namespacing (`use material as m` → `m.Card`, `use material.{Card}`); a
  module resolver in `byard_compiler::resolve` with package-cycle detection
  and a program-wide span `SourceMap`; strict `[dependencies]` parsing;
  `byard add`/`byard install`/`byard get` with a content-hashed `byard.lock`
  and a global `~/.byard/cache`; multi-file + `path`-dependency hot-reload; and
  package-aware LSP completions (`use <TAB>`, `m.<TAB>`, package-view params).
- Repository scaffolding: README, licenses, contributing guide, CI workflow.
- `docs/rfcs/0001-core-architecture.md` — consolidated design document covering
  the memory model, multi-pipeline renderer, spatial hit-testing grid, threading
  model, and the `byld` compiler pipeline.
- RFC template at `docs/rfcs/0000-template.md`.

---

<!-- New versions go above this line, oldest at the bottom. -->
<!-- Example entry:
## [0.1.0] - 2026-MM-DD
### Added
- First working renderer prototype.
-->
