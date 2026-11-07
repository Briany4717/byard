# RFC-0059: Interaction styles

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0016 (`on <state>` blocks and style values), RFC-0024
  (compound states), RFC-0052 (composed controls, `focus_visible` and
  `a11y_role`), RFC-0057 (motion on colours), RFC-0050 (Byard Material, whose
  controls are the reason).

> **Placement:** `docs/rfcs/0059-interaction-styles.md`

---

## Summary

A control built from a `Box` can say how it looks while the pointer is over it,
while it is pressed and while the keyboard has focus without a `var` and
without a handler. Four things are added to make that complete:

- `on hover { … }` and the other state blocks are written **inside the
  element's body**, next to its children, as well as in a `style`.
- `outline`, `outline_width` and `outline_offset` draw a ring **outside** the
  element's shape, following `radius`, taking no layout room.
- `overlay` is a colour or gradient drawn over the element's `bg` and under its
  children, clipped to its shape: the state layer of a Material control.
- `a11y_role` takes a value, so one root serves a toggle and a plain button.

A state block that changes an attribute with `with` also moves it back when the
state ends, a held Space key shows `on pressed`, and five paint bugs found while
building the Material package are fixed: `typo:` chosen by `?:` keeps its family
and weight, a `shadow:` held in a `let`, chosen by `?:` or followed by `with`
draws, a `Box` with a `border` and no `bg` draws the border, and a `ZStack`
child with `grow` or `align_self: stretch` fills the stack.

Nothing here costs anything when unused: an element with no outline, no
overlay and no value-valued role takes the paths it took before.

## Motivation

RFC-0050's spike wrote a Material button and found that every control copies
three `var`s and seven handlers:

```byld
var hov = false
var down = false
var kbd = false
Box #[..chrome, a11y_role: button,
    tap => on_tap(),
    pointer_enter => hov = true,
    pointer_exit => { hov = false  down = false },
    pointer_down => down = true,
    pointer_up => down = false,
    focus => kbd = !down,
    blur => kbd = false] {
    StateLayer(…) { … }
}
```

and wraps itself in a `StateLayer` view (the translucent tint) and a
`FocusRing` view (the ring two pixels outside). The reasons are all gaps in what
a style can say:

1. A child cannot read its parent's states, and a covering child with a handler
   steals hover and press from the parent. So the tint cannot be a child that
   follows the parent's `on hover`.
2. `on focus_visible` can only draw inside the element: `border` is inside the
   shape, so a ring that sits outside it is a sibling that needs the same state
   as a `var`, and `kbd = !down` is an approximation of what the engine already
   knows.
3. The state layer is a colour over the container. There is no attribute for it,
   so it is a wrapper view with its own radius that has to be kept equal to the
   parent's.
4. `a11y_role` is a keyword, so a control with two roles has two copies of its
   root.
5. A held Space key presses a button (its `tap` runs) but `on pressed` does not
   show, so a keyboard user sees nothing the pointer user does.

The same spike found four bugs that the declarative form runs into at once: a
`typo:` chosen with `?:` draws in the default font and weight, a `shadow:` kept
in a `let` or picked with `?:` is not drawn and nothing says so, a `Box` with a
`border` and no `bg` draws nothing, and a `ZStack` child cannot be made to fill
the stack. Each is a silent failure, which the language refuses elsewhere.

## Guide-level explanation

The control Material needs, with no `var` and no wrapper:

```byld
View Pill(label: Str, selected: Bool = false, on_tap: Fn() = {}) {
    inject Theme as t

    Box #[
        bg: selected ? t.secondaryContainer : t.primary,
        radius: 20,
        a11y_role: selected ? toggle_button : button,
        a11y_checked: selected,
        height: 40,
        p: (0, 24),
        align: center,
        justify: center,
        tap => on_tap(),
    ] {
        on hover { overlay: t.onPrimary.alpha(0.08) with t.springEffectsFast }
        on pressed { overlay: t.onPrimary.alpha(0.10), radius: 8 with t.springSpatialFast }
        on focus_visible { outline: t.secondary, outline_width: 3, outline_offset: 2 }
        Text(label) #[typo: t.labelLarge, color: t.onPrimary]
    }
}
```

**State blocks in the body.** `on hover { … }` written among an element's
children is exactly the block that `style { on hover { … } }` holds: it belongs
to the element it is written in, the states and their precedence are RFC-0016's
and RFC-0024's, and it can be combined with `..spread` styles. A block written
in the body comes after the ones in `#[ ]`, so on a tie it wins. A state block
that is not inside an element is an error that says where it should go.

**Outline.** `outline` is a colour, `outline_width` its thickness (2 when
omitted) and `outline_offset` the gap between the element's edge and the ring (0
when omitted). The ring is drawn outside the shape and follows `radius`. It adds
nothing to the element's size, moves no neighbour and takes no clicks. A
negative offset draws it inside. All three animate with `with`.

```byld
Box #[bg: 0x6750A4, radius: 20, width: 120, height: 40] {
    on focus_visible { outline: 0xD0BCFF, outline_width: 3, outline_offset: 2 }
}
```

**Overlay.** `overlay` is a colour drawn over the element's `bg` and under its
children, clipped to the shape (so it rounds with `radius`). A colour with alpha
is the tint of a state layer. A gradient, written as `gradient:` is, is a ramp
over the element. It animates with `with` like `bg`.

**Roles from values.** `a11y_role` takes a keyword, as before, or any expression
that comes to one: a `?:` of keywords, a text that holds one, or a view
parameter.

```byld
let role = toggle ? toggle_button : button
Box #[a11y_role: role] { }      // or: a11y_role: toggle ? toggle_button : button
```

**Back to rest.** A state block's `with` moves the attribute in, and the
attribute moves back out on the same curve when the state ends: the written
`radius: 8 with t.springSpatialFast` above returns to `20` as a spring, and
the overlay fades out. Nothing needs a matching `with` on the base.

**The keyboard.** While Space or Enter is held on a focused control, `on pressed`
shows, as it does for a pointer press.

**What was silent.** A `typo:` from a `?:`, a `shadow:` from a `let`, and a
`border` without `bg` all draw now. A `ZStack` child with `grow: 1` or
`align_self: stretch` fills the stack.

## Reference-level explanation

### 1. State blocks in an element's body

The parser accepts `on <state> ("+" <state>)* { attr* }` and `on width >= N {
… }` as a member of an element's `{ … }`, where it already accepts the
child list. The shapes `on mount =>`, `on unmount =>`, `on measure =>` and
`on change(…) =>` are recognised first and are not affected, and neither is any
other word after `on` that is not followed by a block. The blocks are
gathered and become one `..style { on … }` spread appended to the element's
attribute list, so every later stage (resolution, inference, the checker, the
lowering, the formatter's safety check, the language server) sees the form it
already knows. Precedence is the existing rule: more states first, then
declaration order, and the spread comes last in the attribute list.

`on <state> { }` as a member of anything that is not an element (a view body,
a `when`, a `for`) is a parse error: "a state block styles the element it is
written in; put it inside the element". The unknown-state error
(`UnknownStyleState`) is the existing one.

### 2. `outline`, `outline_width`, `outline_offset`

Three paint-class attributes on every element that takes `border`
(`outline: Color`, `outline_width: Int`, `outline_offset: Int`).

- Drawn only when `outline` resolves to a colour whose alpha is not zero and
  the width is above zero. `outline_width` is 2 when omitted, as `border_width`
  is, and `outline_offset` is 0.
- The ring is the box's rect grown by `offset + width` on every side, stroked
  `width` thick on its inside edge, with the corner radii `r + offset + width`
  where the element's corner `r` is above zero and `0` where it is square
  (a square element has a square ring). A negative `offset` shrinks the ring
  into the element; a ring that would have no area is not drawn.
- It uses the element's rect, `smooth`, paint transform and effective opacity, so
  it rotates, scales and fades with it, and follows `radius` as it animates.
- It is emitted after the element's fill, border and overlay and before its
  children in the element's own paint order. It is not part of layout: no
  node, no hit region, no change to the size of any parent. An ancestor's
  `Clip` or `ScrollView` cuts it, as it cuts anything outside its edge.
- All three go through the ordinary property readers, so `with` works on each.

### 3. `overlay`

A paint-class attribute on the same elements, typed as a colour; a tuple is read
as a gradient with the grammar and checks of `gradient:` (RFC-0035), including
a conditional of gradients and a `with` on a whole gradient (RFC-0057). A plain
colour is read through the colour reader, so `with`, `anim.keyframes` and
`.alpha(x with …)` work, mixed in premultiplied OKLab (RFC-0057).

- One extra decorated quad, with the element's rect, radii, `smooth`, transform
  and opacity, emitted after the element's fill, border and backdrop pane (RFC-0023)
  and before its ripple and its children. The state layer therefore tints the
  container and the frosted pane under it, and lies under the ripple ink and the
  content.
- Not drawn when its alpha is zero, so a fade that has finished leaves nothing
  behind it.
- On `Toggle`, `Slider`, `Checkbox` and `RadioButton`, which paint their own
  visuals and treat `bg` as an accent, the overlay covers the element's rect.

### 4. `a11y_role` from a value

The keyword form is unchanged. The attribute's value is read, in order:

1. a bare word in the keyword set, or a string literal holding one, is the role
   (the word wins over a same-named binding, as every keyword attribute does);
2. a conditional: its condition is evaluated and the chosen branch is read by
   these same rules, to any depth;
3. anything else is evaluated, and must come to a `Str` holding a keyword.

The checker accepts a bare word that the view binds (a parameter, a `let`, a
`var`), as it already does for `weight:`, and still reports a bare word that
nothing binds and no role is called, with the closest keyword. The keywords of
a conditional's branches are checked one by one. A value that comes to text that
is not a role at run time is an `AttributeTypeMismatch` with the list, never an
element silently without a role. The fast path is the first rule: an element that
writes a keyword evaluates nothing.

`a11y_role` is the only a11y attribute that was keyword-only. The others
(`a11y_label`, `a11y_value`, `a11y_hidden`, `a11y_checked`, `a11y_mixed`,
`a11y_selected`, `a11y_expanded`, `a11y_disabled`) already take expressions and
need nothing.

### 5. A state returns by the curve it left on

`override_state_attr` already keeps the base's `with` when a state block gives a
bare value. This RFC closes the other direction. When lowering an element, for
every attribute that a state block writes with a `with`:

- if the base writes the attribute without a `with`, the base value gets the
  state's curve, so leaving the state animates;
- if the base does not write it and it is a colour that can be absent
  (`overlay`, `outline`), the base value is a fully transparent colour; for
  `outline_width` and `outline_offset` it is `0`, with that curve;
- every block's animated value for the same attribute shares one animation
  identity (the base's if the base has one, else the first block's) with its own
  curve, so `hover → pressed` retargets one animation instead of starting a new
  one.

This is decided once when the element is lowered, so a frame costs exactly what
it did. An element with no state block, or none with a `with`, is not touched.

### 6. A held key shows `on pressed`

The router remembers which element a held activation key (Space, Enter) pressed
and reports `PRESSED` for it, instead of a bare "held" flag. The key's `KeyUp`
clears it. Nothing is registered or allocated; the field changes from `bool` to
`Option<u32>`.

### 7. Bugs

- **`typo:` from a conditional.** The theme token a `typo:` names is found by
  reading the expression. A conditional now has its condition evaluated and its
  chosen branch read by the same rule, so `typo: big ? t.titleLarge : t.labelLarge`
  gets the token's family and weight as well as its size.
- **`shadow:` as a value.** A literal list or tuple is read as before, field by
  field. Everything else is evaluated: a conditional picks its branch, a name
  bound in a `let` or a parameter is read as its value (a list of tuples, one
  tuple, a preset word, or `()` for none). A `with` after a shadow animates it as
  a whole: every offset, blur and spread as a number, every colour in OKLab, and
  a list that grows or shrinks fades its extra shadows in place. A value that is
  not a shadow is `AttributeTypeMismatch`, never nothing.
- **`border` without `bg`.** A box with a `border` and no `bg` and no gradient
  draws its border over a transparent interior, on the decorated pipeline, as a
  translucent fill does. It already could not hide behind its children; it still
  cannot.
- **`ZStack` children.** layout.md says a smaller child is placed by
  `alignment`, and `align_self` is documented as taken by every child. Its
  block-axis half worked and its inline half did not, so a child with
  `align_self: stretch` came out one pixel wide. In a `ZStack`, `align_self:
  stretch` and `grow` above zero both make the child fill the stack on both axes
  (a child with its own `width` or `height` keeps that one). A stack with such a
  child has one cell as large as the stack; a stack without one keeps the layout
  it always had, its column as wide as the widest child, so that no program that
  works today moves. The other `align_self` words are unchanged, and `alignment`
  stays the way to place a child that does not fill.

## Drawbacks

- **A parser form that is sugar.** A block in a body is one more place for the
  same thing a `style` holds. It is the form every other UI toolkit's
  declarative states take, and it is implemented as a spread so nothing
  downstream learns a new node.
- **An outline can be clipped.** An ancestor's `Clip` or `ScrollView` cuts a ring
  drawn outside its child. That is the same as a shadow, and a design that wants
  the ring visible leaves room.
- **Two draw calls for a pressed, focused control.** An overlay and an outline are
  two decorated quads. An element that has neither pays nothing; a state layer
  was a whole wrapper view, with its own layout node, before.
- **`overlay` on controls that own their visuals** covers a rectangle that is
  not their drawn shape. Those controls are not what the attribute is for.

## Rationale and alternatives

- **Readable parent state for children** (an `on hover` that a child could
  condition on, or a `hovered` value an element can read). It is the general
  answer and a much bigger one: a dependency from a child's paint to a parent's
  interaction state, with the cost of re-lowering the child. The declarative
  attributes cover Material; a general form can be argued separately.
- **`ring`/`focus_ring` instead of `outline`.** Outline is the CSS word and the
  shape designers know: colour, width, offset. A focus ring is one use of it.
- **A `state_layer` view in the package.** It exists and is what this removes: a
  layout node, a second radius to keep in step, and a size the content has to
  give it.
- **A keyword-or-expression `a11y_role` through a new `Role` enum.** A closed
  built-in enum cannot be written as `.button` unless the language gains
  enums that a built-in can own, and the keyword spelling is what the guide
  and every example already use. Bare words in a conditional read the same.
- **Making the shadow tuple form check-only** (reject a `let`). Working is
  simpler for the author than a rule about where a list may be written.

## Prior art

- **CSS** has `outline`, `outline-offset` and the `:hover`/`:active`/
  `:focus-visible` states a rule styles in place; `outline` is outside the box and
  takes no space there as here. A state layer is a `::before` in CSS, which is
  a node; here it is a property.
- **Material 3** defines the state layer as an overlay of the content colour at
  8, 10 and 10 percent, and the focus indicator as a 3 dp ring offset 2 dp.
- **SwiftUI** and **Compose** (`indication`, `Modifier.border` with offsets) draw
  their focus and press indication from the interaction source rather than from
  copied state, which is the shape this gives Byld.

## Resolved questions

### Q1, where does `on hover { … }` go?

**Options.** (a) only in a `style`, which a control spreads; (b) in the
element's body; (c) in `#[ ]`.

**Resolution: (b), in addition to (a).** The body is where an element's
children are and where a reader looks for what it does; `#[ ]` is a list of
`name: value` pairs and a block does not belong in it. It is a spread
underneath, so (a) keeps working and the two mix.

### Q2, does the outline take layout room?

**Resolution: no.** A ring that moved its neighbours when focus arrived would
make the page jump on every Tab, which is what a focus ring must not do. It
paints outside the rect and is cut by an ancestor's clip, as a shadow is.

### Q3, what is an outline's default width?

**Options.** 1, 2 or 3.

**Resolution: 2**, as `border_width` is, so that swapping a `border` for an
`outline` changes where it is and not how thick. Material writes its 3 dp.

### Q4, how does a state animate back with no `with` on the base?

**Options.** (a) require the author to repeat the curve on the base; (b) take
the curve from the state block.

**Resolution: (b).** Writing `radius: 20 with t.springSpatialFast` and
`on pressed { radius: 8 with t.springSpatialFast }` is the same fact twice, and
forgetting one is a control that snaps back. The base keeps the last word: a
`with` on the base is the curve the state's bare values use, as RFC-0016 says.

### Q5, a keyboard press shows `on pressed`: is that cheap?

**Resolution: yes.** The router already knew a key was held, to fire the tap
once; it now also knows which element it was.

## Future possibilities

- A readable `hovered`/`pressed`/`focus_visible` for an element's children, for
  the controls whose label must change with the state.
- `outline_style` (dashed) and a double ring, if a design wants them.
- A `ripple` that reads the overlay colour.
- `overlay` painted on a `Text`, which takes no `border` today.
