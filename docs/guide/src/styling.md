# Styling

```byld
View Main() {
    let card = style {
        bg: 0x1E1E2A, radius: 14, p: 16, gap: 6
        shadow: "md"
        on hover { bg: 0x2A2A3A }
        on pressed { scale: 0.97 }
    }

    Column #[bg: 0x0E1116, p: 24, gap: 12, grow: 1] {
        Box #[..card] {
            Text("Hover me, then press me") #[color: 0xFFFFFF, size: 16]
        }
        Box #[..card, bg: 0x381E72] {
            Text("Same card, another colour") #[color: 0xE8DEF8, size: 16]
        }
    }
}
```

Appearance is set by attributes, written in `#[ ]` after an element. Attributes you
repeat are collected into a `style`, a value you name once and spread where you
need it. Styles also carry the element's reactions: how it looks when the pointer
is over it, when it is pressed, or when the window is wide.

## Attributes

The ones that paint:

| Attribute | Takes | Notes |
|-----------|-------|-------|
| `bg` | colour | Fill. On a `Text` it fills the text's box, behind the words |
| `color` | colour | Text colour, on `Text`, `Button` and `TextField` |
| `radius` | length | Corner radius. A quad rounds each corner on its own |
| `smooth` | `0` to `1` | Corner profile. `0` is a circular arc, about `0.6` a continuous corner like an Apple icon |
| `opacity` | `0` to `1` | Of the whole element and what it holds |
| `border`, `border_width` | colour, number | A line inside the edge. A box with a `border` and no `bg` draws just the line |
| `outline`, `outline_width`, `outline_offset` | colour, number, number | A ring outside the shape, see [below](#outline-and-overlay) |
| `overlay` | colour or gradient | A tint over `bg`, under the children, see [below](#outline-and-overlay) |
| `shadow` | preset, tuple or list | See below |
| `gradient` | tuple | See below |
| `blur`, `backdrop_tint` | number, colour | Frosted glass, see below |

Text has its own: `size`, `weight`, `font`, `typo`, `align`, `wrap`. They are
under [Text](#text) below. Its size and spacing are [layout](layout.md#text). Every attribute is checked. A name the element does not
take is an error, with the nearest one suggested:

```byld,fails
View Main() {
    Box #[bg: 0x3B82F6, radus: 8, width: 80, height: 80] {}
}
```

```diagnostic
error[UnknownAttribute]: unknown attribute `radus` (did you mean `radius`?)
```

An attribute that is a number, colour, length, angle or list must be one: `bg:
prmary` is reported, not drawn as nothing, and text given to a number is a type
mismatch.

### Colours

A colour is an `Int`. Write `0xRRGGBB`, or `0xAARRGGBB` for alpha. A colour from the
theme is a name, and `.alpha()` makes it translucent: `t.primary.alpha(0.16)`.
[Theming](theming.md) covers both.

### Shadows

```byld
View Main() {
    Column #[bg: 0xF2F3F5, p: 40, gap: 28, width: 400] {
        // A preset: "sm", "md" or "lg".
        Box #[bg: 0xFFFFFF, radius: 12, width: 320, height: 56, shadow: "md"] {}

        // Named fields: x, y, blur, spread and color, in any order.
        Box #[bg: 0xFFFFFF, radius: 12, width: 320, height: 56,
              shadow: (y: 10, blur: 24, spread: -2, color: 0x33000000)] {}

        // Layers: a tight contact shadow and a soft ambient one.
        Box #[bg: 0xFFFFFF, radius: 12, width: 320, height: 56,
              shadow: [(y: 1, blur: 2, color: 0x30000000),
                       (y: 12, blur: 28, spread: -4, color: 0x28000000)]] {}
    }
}
```

A plain tuple is positional: `(x, y, blur, spread, color)`, and any slot may be
left out from the end. A list paints its first shadow on top. A shadow follows
the element's corner radius and smoothing.

A `shadow` can be any expression that comes to a shadow: a `let` holding a list, a
conditional choosing between them, `()` for none. A `with` after it moves every offset, blur
and colour, and a list that gains a shadow fades it in:

```byld
View Main() {
    var lifted = false
    let rest = [(y: 1, blur: 3, color: 0x33000000)]
    let raised = [(y: 8, blur: 20, color: 0x40000000), (y: 2, blur: 4, color: 0x30000000)]
    Column #[bg: 0xF2F3F5, p: 40] {
        Box #[bg: 0xFFFFFF, radius: 12, width: 160, height: 56,
              shadow: lifted ? raised : rest with anim.spring(),
              tap => lifted = !lifted]
    }
}
```

A value that is not a shadow is an error that says so.

### Gradients

```byld
View Main() {
    Row #[bg: 0x0B0D12, p: 20, gap: 16] {
        Box #[width: 120, height: 80, radius: 14,
              gradient: (angle: 60deg, from: 0xFF5B8DEF, to: 0xFF9B5BEF)] {}
        Box #[width: 120, height: 80, radius: 14,
              gradient: (kind: radial, center: (0.5, 0.5), radius: 0.9,
                         from: 0xFF34D399, mid: 0xCC0E3D2F, to: 0x000E3D2F)] {}
        Box #[width: 120, height: 80, radius: 14,
              gradient: (kind: conic, center: (0.5, 0.5), start: -90deg,
                         from: 0xFFEF4444, mid: 0xFFF59E0B, to: 0xFFEF4444)] {}
    }
}
```

`kind` is `linear` (the default), `radial` or `conic`. Colours are `from`, an
optional `mid` and `to`, with `mid_pos` to move the middle stop. A linear gradient
takes an `angle`. A radial or conic one takes a `center`, as fractions of the box,
so `(1, 0)` is its top-right corner whatever its size. `radius` scales a radial,
and `start` turns a conic. Gradients are drawn as part of the box, so they follow
its rounded corners, and `gradient_offset` slides the ramp, which is how a
[shimmer](motion.md#looping) works. The same descriptor is used on canvas strokes
and filled paths, see [drawing](drawing.md).

A gradient moves with [`with`](motion.md#colours-and-gradients), and a conditional of
gradients is the one its condition picks.

A misspelt name inside the tuple is checked as well, so `gradient: (to: blak)` is
an error and not a gradient that quietly does not draw.

### Outline and overlay

`outline` draws a ring **outside** the element, which is what a focus ring is. `outline_width`
is its thickness (2 when left out) and `outline_offset` the gap between the edge and the ring
(0). The ring follows `radius`, adds nothing to the element's size, moves no neighbour and
takes no clicks. A negative offset draws it inside the edge. All three move with
[`with`](motion.md), and a colour whose alpha is zero draws nothing.

`overlay` is a colour drawn over the element's `bg` and under what it holds, clipped to its
shape, so it rounds with `radius`: the state layer of a Material control. A colour with alpha
tints; a gradient, written as `gradient` is, ramps over it. It moves with `with` like `bg`.

```byld
View Main() {
    Row #[bg: 0xFEF7FF, p: 32, gap: 28] {
        Box #[bg: 0x6750A4, radius: 20, width: 120, height: 40,
              overlay: 0x29FFFFFF,
              outline: 0x6750A4, outline_width: 3, outline_offset: 3] {}
        Box #[bg: 0x6750A4, radius: 8, width: 120, height: 40,
              overlay: (angle: 90deg, from: 0x00FFFFFF, to: 0x55FFFFFF)] {}
    }
}
```

An outline is painted where the element is, so an ancestor's `Clip` or a `ScrollView` cuts it
at their edge, and a later sibling paints over it where they meet. Leave a gap for it, as
you would for a shadow.

### Aligning text

`align` on a `Text` sits its lines inside the text's width: `start`, `center`, `end` or `justify`
(spaces stretched to both edges, the last line of a paragraph left at the start). It is not what
`align` means on a `Row` or `Column`, where it places the children across the axis; on a `Text` there
are no children, so it is the text's own. It needs a width to align within, which a `Text` has unless it
says `wrap: false`, and it can be chosen with `?:`.

```byld
View Main() {
    Column #[width: 300, p: 10, gap: 8] {
        Text("short\nthe longer second line") #[width: 280, align: center, bg: 0xEEEEEE]
        Text("the quick brown fox jumps over the lazy dog") #[width: 200, align: justify]
    }
}
```

### Keywords chosen by a condition

An attribute that takes a word (`align`, `justify`, `direction`, `align_self`, `alignment`, `fit`,
`axis`, `snap`, `transition`, `blur_quality`, `a11y_role`) can be chosen with `?:`, nested as deep as
you like. The word the condition picks is the one used, and it follows the condition as it changes.
Every word in the conditional is checked, so a misspelt one is an error naming it.

```byld
View Main() {
    var centred = true
    Row #[width: 240, height: 60, bg: 0xE8DEF8, align: centred ? center : end,
          tap => centred = !centred] {
        Box #[bg: 0x6750A4, width: 40, height: 20] {}
    }
}
```

### Frosted glass and ripple

```byld
View Main() {
    ZStack #[width: 320, height: 200, bg: 0x101014] {
        Row #[gap: 0] {
            Box #[bg: 0xE91E63, width: 107, height: 200] {}
            Box #[bg: 0x2196F3, width: 107, height: 200] {}
            Box #[bg: 0xFFC107, width: 106, height: 200] {}
        }
        Column #[p: 24, width: 320, height: 200, justify: center] {
            Box #[blur: 18, backdrop_tint: 0x66FFFFFF, radius: 16, p: (12, 18)] {
                Text("Frosted glass") #[color: 0x1D1B20, size: 16, weight: medium]
            }
        }
    }
}
```

`blur` blurs what is behind the element, up to 40 pixels, and `backdrop_tint`
washes it with a colour. Together they make glass. A tint alone is a plain
translucent wash, which is cheaper. `blur_saturation` boosts colour, and
`blur_quality` is `auto`, `high` or `low`.

`ripple` is Material's ink. Give an element a `ripple` colour, and turn
`ripple_active` on in `on pressed`, and ink spreads from where it was tapped:

```byld
View Main() {
    let ink = style {
        bg: 0x6750A4, radius: 24, p: (14, 28)
        ripple: 0x66FFFFFF
        on pressed { ripple_active: true }
    }

    Box #[..ink, width: 300, m: 20] {
        Text("Tap anywhere on me") #[color: 0xFFFFFF, size: 14]
    }
}
```

`ripple_radius` and `ripple_duration` (milliseconds) tune it.

## `style`: a name for a set of attributes

```byld,fragment
let heading = style { size: 22, weight: bold, color: 0xFFFFFF }
Text("Forecast") #[..heading]
Text("Tonight") #[..heading, size: 16]
```

A `style { … }` is a value. `let` gives it a name, and `..name` inside `#[ ]` spreads
its attributes into the element. An attribute written on the element itself wins over
the style, wherever in the list it sits, so `size: 16` above overrides the
style's `22`. Inside a `style`, attributes may be separated by
commas or by line breaks.

Combine two styles with `merge`. The right one wins:

```byld,fragment
let card = style { bg: 0x1E1E2A, radius: 12, p: 16 }
let loud = card merge style { bg: 0x381E72, radius: 24 }
Box #[..loud] { Text("loud") }
```

A style declared in a view is local to it. To share one, make a small view that
applies it, as the examples do with `ActionButton`.

## States

An element is in states the engine tracks, such as hovered and pressed. A style
says how to look in each with an `on` block:

```byld
View Main() {
    var accepted = true
    var note = "Hi"

    let box = style {
        bg: 0xFEF7FF, border: 0x234135, border_width: 2
        on checked { bg: 0x6750A4, border: 0x6750A4 }
        on hover { border: 0x6750A4 }
    }

    let field = style {
        bg: 0x1E1E2A, radius: 10, p: 14, border: 0x49454F, border_width: 1, color: 0xE6E1E5
        on hover { border: 0x8A80B0 }
        on focused { border: 0x6750A4, border_width: 2 }
        on focused+hover { border: 0xD0BCFF, border_width: 2 }
        on invalid { border: 0xB3261E, color: 0xF2B8B5 }
    }

    Column #[bg: 0x14141C, p: 32, gap: 18, width: 380] {
        Checkbox #[..box, bind: accepted, width: 22, height: 22]
        TextField #[..field, bind: note, width: 316]
        TextField #[..field, placeholder: "this one is invalid", invalid: true, width: 316]
    }
}
```

| State | When |
|-------|------|
| `hover` | The pointer is over the element |
| `pressed` | A press is on it |
| `focused` | It has keyboard focus, however it got there |
| `focus_visible` | It has focus, and the keyboard put it there (Tab, an arrow key in a tab list or radio group), or it is a `TextField`. A mouse press does not set it |
| `disabled` | It has `disabled: true`. A disabled element is never hovered, pressed, focused or dragged, so only `on disabled` applies to it |
| `checked` | A `Checkbox` or `Toggle` is on |
| `selected` | It has `selected: true`, for a tab, a chip, a list item |
| `invalid` | It has `invalid: true` |
| `indeterminate` | A `Checkbox` has `indeterminate: true` |
| `dragging` | A drag gesture is under way on it |
| `focus_within` | It, or an element inside it, has keyboard focus |

`selected` and `invalid` are plain attributes on any element. You decide when
they are true, and the style reacts: `selected: tab == "home"` turns a tab
button's `on selected` block on. `on a+b` applies only while both are true, and
the more states a block names, the more it outranks one that names fewer. A state
that does not exist is an error:

```byld,fails
View Main() {
    let s = style { bg: 0xFF0000, on hovr { bg: 0x00FF00 } }
    Box #[..s] {}
}
```

```diagnostic
error[UnknownStyleState]: unknown interaction state `hovr`
```

A disabled element is only `disabled`: it stops its handlers, Tab skips it, and
no `on hover` or `on pressed` tints it. So a block that asks for both can never
apply, and says so:

```byld,fails
View Main() {
    Box #[disabled: true] {
        on disabled+hover { overlay: 0x14FFFFFF }
    }
}
```

```diagnostic
error[DisabledStateNeverMet]: `on disabled+hover` never applies: a disabled element is never hovered; write `on disabled` for how it looks
```

`focused` is true after a click as well as after Tab. A ring that should show for the
keyboard only is written `on focus_visible`, which is what the browsers' `:focus-visible`
does and what Material asks for. A press on a button focuses it without the ring, and the
next Tab draws it on the next control. A `TextField` is the exception: you are typing in
it, so it is focus-visible however you got there. Keep `on focused` for anything that is
true of every kind of focus, such as the tint of the field you are in.

```byld
View Main() {
    var n = 0
    let ring = style {
        border: 0x2F2F40, border_width: 2
        on focus_visible { border: 0xB69DF8, border_width: 3 }
    }
    Button("Save") #[..ring, bg: 0x3B82F6, radius: 8, width: 120, height: 40] => n += 1
}
```

The states come from the engine's own tracking, so you write no `var` and no
handler for any of them. They also animate: put `with anim.spring()` on an
attribute and it eases between its values. [Motion](motion.md) explains how.

### States written in the element

An `on` block can be written inside the element it styles, among its children, and needs
no `style` at all. It is the same block: the same states, the same precedence, and it can
sit next to a `..style` spread. Where both give an attribute for the same state, the one in
the body wins. This is a whole control with no `var` and no handler for its states:

```byld
View Main() {
    var taps = 0
    Box #[bg: 0x6750A4, radius: 20, width: 140, height: 40, align: center, justify: center,
          a11y_role: button, tap => taps += 1] {
        on hover { overlay: 0x14FFFFFF }
        on pressed { overlay: 0x1AFFFFFF, radius: 8 with anim.spring() }
        on focus_visible { outline: 0xD0BCFF, outline_width: 3, outline_offset: 2 }
        Text("Taps: {taps}") #[color: 0xFFFFFF, size: 14]
    }
}
```

An `on` block that is not inside an element has nothing to style, and is an error that
says where it goes.

A state that moves an attribute with `with` moves it back on the same curve when the state
ends: the pressed corners above spring open again on release, with no `with` on the
resting `radius`. Where there is no resting value to move back to, as for `overlay` and
`outline`, the element rests transparent. `on pressed` also shows while Space or Enter is
held on a focused control, as it does for a press of the pointer.

### States of another element

An element named with `as name` publishes its states, and a state block on a descendant, or on an
element after it in the same view, can name them: `on name.hover`, `on name.focused`,
`on name.pressed`, any state in the table, joined with `+` to each other and to the element's own.
A floating label and an indicator follow a field this way, with no `var` and no handler:

```byld
View LabelledField(label: Str, bind value: Str) {
    ZStack #[width: 300, height: 64, alignment: top_start] {
        Text(label) #[color: 0x79747E, ml: 16, mt: 18] {
            on field.focused { color: 0x6750A4, translate.y: -18, scale: 0.75 }
        }
        TextField #[bind: value, width: 300, height: 56, indicator: none] as field
        Box #[width: 300, height: 1, mt: 56, bg: 0x79747E] {
            on field.focused + hover { bg: 0x6750A4 }
        }
    }
}
```

A name belongs to the whole view, so the label may be written before its field. A name no element is
tagged with is `UnknownStateTarget`, with the closest name. A block on another element's states, or on
`focus_within`, changes what is painted and never the layout, so a layout attribute in one
(`width`, `height`, `p`, `m`, `grow`, ...) is `LayoutInStateBlock`: a label that floats moves with
`translate` and `scale`. An element written before the named one follows its pointer and focus states;
`checked`, `selected`, `invalid` and `disabled` reach the elements written after it. Two calls of one view keep their own
names. See `examples/composing_components`.

## Responsive styles

A block can depend on the window's width instead of a state:

```byld
View Main() {
    let page = style {
        p: 12, gap: 12
        on width >= 640 { p: 28, gap: 20 }
        on width >= 1024 { p: 40, gap: 24 }
    }
    let cards = style {
        columns: "1fr"
        gap: 12
        on width >= 640 { columns: "1fr 1fr" }
        on width >= 1024 { columns: "1fr 1fr 1fr" }
    }

    Column #[..page, bg: 0x0E1116, grow: 1] {
        Text("Forecast") #[color: 0xE7ECF2, size: 22]
        Grid #[..cards] {
            Box #[bg: 0x1C2430, radius: 12, p: 16, height: 80] { Text("Morning") #[color: 0xE7ECF2] }
            Box #[bg: 0x1C2430, radius: 12, p: 16, height: 80] { Text("Afternoon") #[color: 0xE7ECF2] }
            Box #[bg: 0x1C2430, radius: 12, p: 16, height: 80] { Text("Evening") #[color: 0xE7ECF2] }
        }
    }
}
```

`on width >= 640 { … }` applies while the window is at least 640 logical pixels
wide, and `on width < 640` while it is narrower. `height` works the same. Unlike
`on hover`, a responsive block can change *layout*: padding, gap, columns, size,
direction. Resize the window and it switches at exactly the right width.

Rather than scatter numbers, name them once in `byard.toml`:

```toml
[theme.breakpoints]
md = 640
lg = 1024
```

and write `on width >= md`, which means the same as `on width >= 640`. The name is
checked. One that is not declared is an error, which says where to declare it:

```byld,fails
View Main() {
    let s = style { on width >= md { p: 24 } }
    Box #[..s] {}
}
```

```diagnostic
error[UnknownBreakpoint]: no breakpoint `md` is declared
```

The names are yours: Byard does not fix a `sm`, `md` and `lg`.

## Text

```byld
View Main() {
    Column #[bg: 0x0E1116, p: 24, gap: 10, width: 420] {
        Text("Thin and big") #[color: 0xE7ECF2, size: 30, weight: thin]
        Text("Bold") #[color: 0xE7ECF2, size: 30, weight: bold]
        Text("Weight 600 on the axis") #[color: 0xE7ECF2, size: 20, weight: 600]
        Text("This paragraph has no width and no wrap flag, yet it wraps to fill the column, and wraps again if you resize the window.") #[color: 0x9AA3AE, size: 14]
        Text("This line opts out of wrapping.") #[color: 0x69737F, size: 14, wrap: false]
    }
}
```

- `size` is in logical pixels. Text at a size of 0, or below it, takes no room and draws nothing.
- `weight` is `thin`, `regular`, `medium` or `bold`, or a number from 100 to 900. A
  variable font follows the number exactly. `bold` and `700` are the same weight.
- `font` picks a family you declared in `byard.toml`. [Theming](theming.md) shows
  how.
- `typo` takes a named text style from the theme, which sets size, weight and
  family together. This is the better way to style text, because a screen then
  says what a piece of text *is*, not three loose numbers.
- `align` places the text within its width.
- Any of these can read a view's parameter, a `let` or a `var`: `size: dim`,
  `weight: w`, `color: c`, `wrap: on`, or `typo: name` with a `Str` holding a token name.
  A `Float` size such as `12.5` works too. A name the view binds wins over a keyword of the
  same spelling, so a parameter called `bold` is the parameter.
- Text wraps to the width its parent offers. `wrap: false` keeps it on a line, and with a `width` clips it to that width.
- `bg` and `radius` give a `Text` a background. It is drawn behind the words and fills
  the text's whole box, `p` included, so `Text("Email") #[bg: 0x14141C, p: (horizontal: 4)]`
  is a label that hides the border it sits over. A `Text` that writes neither draws no
  fill and pays nothing for one.

```byld,fragment
Box #[bg: 0x14141C, border: 0x938F99, border_width: 1, radius: 4, height: 56] {}
Text("Email") #[bg: 0x14141C, color: 0xCAC4D0, size: 12, p: (horizontal: 4), ml: 12]
```

Text, like boxes, can be moved and turned. See [motion](motion.md).

## Corners, borders and clipping

`radius` rounds the corners and a border follows them. To cut children to a shape,
use `Clip`, which cuts what is inside it to its own box, with `rrect` for rounded
corners:

```byld
View Main() {
    Clip #[rrect: 28, width: 180, height: 120] {
        Column {
            Box #[bg: 0xE8543F, width: 180, height: 60] {}
            Box #[bg: 0x4A6FD0, width: 180, height: 60] {}
        }
    }
}
```

`rrect: (28, 28, 0, 0)` rounds only the top corners. A `Clip` can also take a
path as its mask. [Drawing](drawing.md) shows that.
