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
| `bg` | colour | Fill |
| `color` | colour | Text colour, on `Text`, `Button` and `TextField` |
| `radius` | length | Corner radius. A quad rounds each corner on its own |
| `smooth` | `0` to `1` | Corner profile. `0` is a circular arc, about `0.6` a continuous corner like an Apple icon |
| `opacity` | `0` to `1` | Of the whole element and what it holds |
| `border`, `border_width` | colour, number | An outline |
| `shadow` | preset, tuple or list | See below |
| `gradient` | tuple | See below |
| `blur`, `backdrop_tint` | number, colour | Frosted glass, see below |

Text has its own: `size`, `weight`, `font`, `typo`, `align`, `wrap`. They are
under [Text](#text) below. Every attribute is checked. A name the element does not
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

A misspelt name inside the tuple is checked as well, so `gradient: (to: blak)` is
an error and not a gradient that quietly does not draw.

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
| `focused` | It has keyboard focus |
| `disabled` | It has `disabled: true` |
| `checked` | A `Checkbox` or `Toggle` is on |
| `selected` | It has `selected: true`, for a tab, a chip, a list item |
| `invalid` | It has `invalid: true` |
| `indeterminate` | A `Checkbox` has `indeterminate: true` |
| `dragging` | A drag gesture is under way on it |

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

The states come from the engine's own tracking, so you write no `var` and no
handler for any of them. They also animate: put `with anim.spring()` on an
attribute and it eases between its values. [Motion](motion.md) explains how.

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
- Text wraps to the width its parent offers. `wrap: false` keeps it on a line.

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
