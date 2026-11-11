# RFC-0061: Layout motion

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0010 (implicit animations, whose "layout properties
  cannot animate" this reverses for a closed set), RFC-0016 (`on <state>`
  blocks), RFC-0032 (the retained layout tree, which is what makes this cheap),
  RFC-0059 (interaction styles, whose `on pressed` this lets move a box),
  RFC-0050 (Byard Material, whose controls are the reason).

> **Placement:** `docs/rfcs/0061-layout-motion.md`

---

## Summary

`with` works on `width`, `height`, `gap`, the padding family (`p`, `pt`, `pr`,
`pb`, `pl`) and the margin family (`m`, `mt`, `mr`, `mb`, `ml`, `mx`, `my`), in
an element's attributes and inside its `on <state>` blocks. A box can widen
while it is pressed and its neighbours narrow; a slot can open; a button can
collapse to an icon. Layout is redone only on the frames where such a motion is
in flight, and only for the part of the tree it can reach; a screen at rest
pays nothing.

A corner radius, a padding and a margin written as a tuple take an expression in
every slot, take a `?:` between a tuple and a number (or between two tuples),
and take `with`, each slot moving on the curve by itself.

A `let` inside a `for` body feeds a length again.

Every other layout property is still `LayoutPropNotAnimatable`, and the error
now says which property it is and why it cannot move.

## Motivation

Byard Material's wave 1 (RFC-0050) found three things its components need and
the language refused:

- **A FilterChip** opens a slot for its check mark: the label slides right as
  the slot grows from 0 to 18 px. That is `width` on an empty `Box`, or
  `pl`/`gap` on the chip. The workaround was a `scale` on the check, which does
  not move the label.
- **An ExtendedFab** collapses to a round button: its width, and the gap
  between the icon and the label, go to zero on the spring that Material names
  `springSpatialFast`.
- **A ButtonGroup** widens the pressed button and narrows its neighbours
  (Material 3 Expressive's "connected" groups). That is `on pressed { width:
  120 with … }` on one child and an effect on the others. The workaround drove
  `width` from `pointer_down`/`pointer_up`, which a held Space does not reach.
- **A Switch** moves its handle and grows it while pressed.

RFC-0010 refused all of it, on the grounds that a layout property cannot be
animated on the GPU and relayout every frame is what Byard exists to avoid. The
second half of that is no longer true: since RFC-0032 a frame re-walks the tree
and Taffy recomputes only the nodes whose style actually changed. An animation
whose value does not change this frame changes nothing, and one that does
change costs the nodes it can reach. What is true is that a moving size costs a
layout pass **for as long as it moves**, so the cost has to be visible in the
rules, not hidden.

Corner radius has the same story on the paint side. A pressed Material button
morphs its corners; a button in a group has a square inner corner and a round
outer one; `radius: (a ? 4 : 8, 4, 4, 4) with spring` is how a developer
writes that, and it is an `AttributeTypeMismatch` today, as is
`radius: c ? (4, 4, 0, 0) : 4`, because the radius resolver reads the syntax of
a tuple and not the value of an expression. The gaps list (gaps 8, 17-20)
also found that `let q = 5` inside a `for` body cannot feed `radius: q`.

## Guide-level explanation

```byld
Box #[
    width: 80 with t.springSpatialFast,
    height: 40,
    radius: (selected ? 4 : 20, 20, 20, selected ? 4 : 20) with t.springSpatialFast,
] {
    on pressed { width: 120 }
}
```

Pressing the box (a pointer, or a held Space or Enter) takes `width` to 120 on
the spring, and letting go brings it back on the same spring. Anything laid out
beside it moves as the width does, because the width is a real width and not a
transform: the siblings narrow, the row stays as wide as it was. A `with` on the
base value or on the block's value is enough; the block's curve is also the way
back, as for every other property (RFC-0059 §3).

Padding and margin tuples move slot by slot:

```byld
Row #[p: (8, open ? 16 : 8) with t.springSpatialFast, gap: open ? 8 : 0 with t.springSpatialFast] { … }
```

The properties that cannot move say why:

```
error[LayoutPropNotAnimatable]: `align` is a layout property and cannot be animated
    it is a keyword, and there is nothing between two keywords to move through;
    animate `width`, `height`, `gap` or a padding or margin instead
```

What is the cost? Nothing, at rest. While a size is moving, each frame lays out
the box that moves and what depends on it: its parent's other children move,
its parent may resize, and so on up to the first ancestor with a size of its
own. Text inside a box whose width changes is re-wrapped, so animating the width
of a paragraph is the expensive case; animating an icon slot or a button is not.

## Reference-level explanation

### 1. Which properties

`LayoutPropNotAnimatable` is decided by a closed list of names, in the checker:

| Animatable with `with` | Still refused |
|---|---|
| `width`, `height`, `gap`, `p`, `pt`, `pr`, `pb`, `pl`, `m`, `mt`, `mr`, `mb`, `ml`, `mx`, `my` | `grow`, `shrink`, `basis`, `align`, `justify`, `align_self`, `direction`, `size`, `typo`, `weight`, `font`, `wrap`, the grid placements (`col`, `row`, `col_span`, `row_span`, `col_gap`, `row_gap`, `columns`, `rows`), `anchor*`, `at`, `max_depth` and the rest of the layout class |

The list is the class table (RFC-0032 §R2) plus a second column, not a second
table: a property that is added to the class table is refused until someone adds
it to the animatable column. The rule that a `with` anywhere inside the
expression counts (`width: (x with …) + 0`) stays, and now simply means the
property has to be on the animatable side.

`px` and `py` do not exist (RFC-0005 erratum, `px: 5` is `UnknownAttribute`);
this RFC does not bring them back. `p: (v, h)` is the pair form.

The refusal's message depends on the property:

- a keyword (`align`, `justify`, `direction`, `align_self`, `alignment`): there is
  nothing between two keywords to move through;
- text shaping (`size`, `typo`, `weight`, `font`, `wrap`): it changes how the
  text is shaped, so every frame would shape it again; animate `scale`;
- `grow`, `shrink`, `basis`: a share of the free space among siblings; animate
  the sizes it divides;
- a grid or anchor placement: a cell or an edge, not a length;
- anything else: it feeds layout and has no motion defined.

Keyframes (`anim.keyframes`) follow `with`: allowed where `with` is.

### 2. How a size moves, and what it costs

Nothing in the animation machinery changes. A `with` is sampled where the value
is read, which for these properties is the layout walk (`eval_container_style`
and the leaf readers), and the sample marks the frame active
(`has_active_animations`) until the motion settles, exactly as a paint
property's sample does. So the runner keeps asking for frames while a size moves
and stops when it has settled.

The retained layout tree (RFC-0032) does the rest. Each frame the walk rebuilds
the node styles; a style equal to last frame's marks nothing, and a style that
differs marks that node, which Taffy recomputes with its ancestors and the
siblings whose position depends on it. A box whose `width` is at its target does
not differ from last frame, so a settled motion costs one map lookup per read
and no layout. The counters of `byard dev` (`atlas retained · N node(s)
marked`) show N for the box that moves and the nodes it can reach, and zero when
it stops.

Per-component motions are keyed by the `with` node, the element instance and the
component, so a tuple's four slots retarget independently and an interrupted
motion continues from where it is (RFC-0010's interruptible spring).

### 3. `on <state>` blocks reach layout

Until now layout read an element's attributes with no interaction state, and an
`on hover`/`on pressed` block that wrote a layout property did nothing: a silent
no-op that this RFC removes. A container's layout style is now resolved against
the same state mask paint uses (the router's hover, pressed, focus, drag and
disabled bits, plus the prop-driven `checked`, `selected`, `invalid`), read at
the point the container is added to the layout tree, where its place in the
build order, which is what the router names it by, is known. A container with no
state block pays nothing; one whose blocks write no layout property pays one
scan of the block's attribute names.

A state that moves its own box moves what the pointer is over. `on hover
{ width: 60 }` on a box 80 wide under a pointer at its right edge ends the hover
and starts it again. That is the author's to design; the guide says so, and the
Material controls that widen on press do so on `pressed`, which a pointer keeps
until released.

The state applies to every element that lays out as a node of its own: `Row`,
`Column`, `Box`, `Button`, `Grid`, `ZStack` and `Text`. A `Text` is a leaf, so its
place in the build order is known before it is measured; a `Grid` and a `ZStack`
are read like a container, after their children.

### 4. Tuples

`radius`, `p` and `m` accept a tuple whose slots are any expressions, a `?:`
whose branches are tuples or numbers, and `with` after either.

- The checker only has the syntax; the resolvers get the value. A `radius` (or
  `p`, `m`) that is not a literal tuple is evaluated, and the value is read as
  a number (all slots) or a tuple (per slot), with the arity rules of the
  syntactic forms: four slots for `radius`; one, two (`vertical, horizontal`) or
  four (`top, right, bottom, left`) for `p` and `m`; the named form
  (`top: 4, horizontal: 8`) too. A number or tuple of any other arity is the
  same `ArityMismatch`/`AttributeTypeMismatch` the literal gets, at the span of
  the value.
- `value with curve`, where the value is any of the above, resolves the
  target to its four slots and gives each slot a motion of its own. A `?:` that
  flips between `(4, 4, 0, 0)` and `4` retargets the slots that differ.
- `with` inside a slot (`(a with c, 4, 4, 4)`) still animates that slot, as it
  did; `with` on the whole tuple and on a slot at once animate twice, and the
  checker does not forbid it, it is the author's curve on the author's number.

The motions are four `f32` Motions keyed by the `with` node and the slot index
(`AnimKey` gains a `part`), and they live and die with the element like any other.

### 5. A `let` in a `for` body

`for i, x in xs { let q = 5  Box #[radius: q] {} }` reported "expected a
length". The cause is not the length's typing: a `for` (or `when`) body is
lowered by `lower_members`, which knows elements, `when`, `for`, effects and
timers, and drops every other member, so the `let` was never defined and `q`
fell through to a bare word (the string `"q"`, the way a keyword like `center`
is read). `radius: q` and `p: q` rejected the string; `width: q` and `gap: q`
read it as "no number" and silently gave the element its default size, which is
the same bug without the message.

A `let` in a `for` or `when` body is defined where it is written, as in a view
body: a memo in the body's own scope, which is retired with the body, and which
the body's elements capture with the rest of their environment. A `let` of a
style (`let s = style { … }`) works there too.

### 6. Tests

Each rule has a test that fails before: the checker accepting each property of
§1 and refusing the others with their reason; a width sampled at 0, half and
full time of a linear motion, a settled motion that marks no layout node, a
neighbour that moves with it; `on pressed` for a pointer and for a held Space;
padding, margin and gap; every radius form; the `for`/`let` repro.

## Drawbacks

- A moving size costs layout. It is bounded to the frames it moves and to the
  nodes it reaches, but a developer who animates the width of a long paragraph
  pays for re-wrapping it every frame. The guide says so; there is no warning
  in the checker, because the same `width` on a box is free of it.
- `on hover` can move its own box from under the pointer. That is a design
  hazard that previously did not exist (the block was ignored). It is the same
  one CSS has.
- Two readers of one `with` (a leaf reads `width` in two places in a frame)
  sample it twice. Sampling is idempotent for a fixed clock, so this costs a
  lookup, not correctness.

## Rationale and alternatives

- **A `scale` transform.** Does not move siblings, distorts content and hit
  areas, and is the workaround that sent Material here.
- **Animating only in the layout engine (Taffy interpolation).** Larger, and it
  would put a second animation system beside the one every other property uses.
  Sampling at the read, as paint properties do, reuses the clock, the retarget
  rule, the settle rule and the cleanup on unmount, and the retained tree
  already makes "style unchanged" free.
- **Allowing every layout property.** `grow`, `basis` and the keywords have no
  meaningful in-between or a cost with no use. Refused with a reason is better
  than animated badly.
- **State blocks for all elements, not only containers.** Wanted, and cheap for
  a `Text`'s padding, but a leaf's identity is chosen before its text is
  measured; left for a use that needs it.

## Prior art

- CSS transitions animate `width`, `padding`, `margin` and `gap` and relayout on
  every frame; browsers keep the cost down by not laying out what a change can't
  reach.
- Jetpack Compose's `animateDpAsState` on `Modifier.width` does the same, with
  Material's spatial springs.
- SwiftUI's `withAnimation` interpolates frames of a layout-affecting change.

## Unresolved questions

- `anim.keyframes` on a padding tuple is allowed because the tuple resolver
  reads values; its interpolation between tuple steps is per component, which
  has not been needed yet.

## Future possibilities

- `grow`, once a use shows what its in-between means.
