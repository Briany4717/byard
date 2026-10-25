# Drawing

```byld
View Main() {
    var percent: Float = 65.0

    Column #[bg: 0x14141C, p: 28, gap: 16, align: center] {
        Canvas #[width: 120, height: 120] {
            arc(cx: 60, cy: 60, r: 48, stroke: 0x49454F, stroke_width: 8)
            arc(cx: 60, cy: 60, r: 48, start: -90, sweep: percent * 3.6,
                stroke: 0xD0BCFF, stroke_width: 8, cap: round)
            text("{percent}%", x: 60, y: 60, color: 0xE6E1E5, size: 18, align: center)
        }
        Slider #[bind: percent, min: 0.0, max: 100.0, step: 1.0, width: 220, height: 26]
    }
}
```

A `Canvas` is a surface you draw on with shapes. Its children are not elements but
*shape commands*, each one a name and named arguments. The shapes are cheap to
change, because they read your `var`s like anything else: drag the slider and only
the arc moves.

A canvas is a fixed-size leaf. It never grows to fit what it draws, so `width` and
`height` are required. Leave one out and the checker says so. It takes `bg` for a
background fill, `grow`, `m` and `opacity`. Coordinates inside it are in its own
pixels, with `(0, 0)` at the top-left.

## The shapes

| Command | Arguments |
|---------|-----------|
| `circle` | `cx`, `cy`, `r` |
| `arc` | `cx`, `cy`, `r`, and optionally `start` and `sweep`, in degrees. Without them it is a full circle |
| `line` | `x1`, `y1`, `x2`, `y2` |
| `rect` | `x`, `y`, `w`, `h`, and optionally `radius` and `smooth` |
| `ngon` | `cx`, `cy`, `r`, `n`, and optionally `corner`, `inner` and `rotate` |
| `bezier` | `x1`, `y1`, `cx1`, `cy1`, `cx2`, `cy2`, `x2`, `y2` |
| `path` | A body of commands, or `d:` with SVG path text |
| `text` | The text, then `x`, `y`, and optionally `color`, `size` and `align` |

Every shape that can be painted also takes these:

| Argument | Does |
|----------|------|
| `fill` | A colour, to fill it |
| `stroke`, `stroke_width` | An outline, and how thick it is |
| `cap` | The ends of a stroke: `butt`, `round` or `square` |
| `dash`, `dash_offset` | A dashed stroke: `dash: (12, 8)` is 12 on and 8 off, and the offset slides it |
| `stroke_gradient` | A ramp along the stroke. Same tuple as [a box's gradient](styling.md#gradients) |
| `opacity` | Of that shape |

An argument is any expression. It follows your `var`s, and it can animate with `with`:
`cx: 250 with anim.ease_in_out(1800ms, from: 130, repeat: infinite, reverse: true)`
moves a circle back and forth.

```byld
View Main() {
    Canvas #[width: 320, height: 120, bg: 0x1C1B22] {
        line(x1: 20, y1: 24, x2: 200, y2: 24, stroke: 0xCAC4D0, stroke_width: 6,
             cap: round, dash: (12, 8))
        rect(x: 20, y: 40, w: 72, h: 60, radius: 12, fill: 0x381E72,
             stroke: 0xD0BCFF, stroke_width: 2)
        circle(cx: 130, cy: 70, r: 20, fill: 0xF2B8B5)
        ngon(cx: 190, cy: 70, r: 24, n: 6, corner: 4, fill: 0x4CC38A)
        bezier(x1: 230, y1: 100, cx1: 250, cy1: 10, cx2: 280, cy2: 150, x2: 300, y2: 30,
               stroke: 0x7DD3A8, stroke_width: 3)
    }
}
```

A shape written outside a canvas is an error that says so, and a misspelt argument
is reported with the nearest real one.

## Charts from data

`for` and `when` work inside a canvas, so the number of shapes comes from the data and
not from the source:

```byld
View Main() {
    var percent: Float = 65.0
    let samples = [40.0, 62.0, 55.0, 78.0, 91.0, 70.0, 48.0, 83.0]

    Column #[bg: 0x14141C, p: 24, gap: 12, width: 380] {
        Canvas #[width: 340, height: 96, bg: 0x1C1B22] {
            for i, s in samples {
                rect(x: 16 + i * 40, y: 88 - s * percent * 0.008 * 88,
                     w: 24, h: s * percent * 0.008 * 88, radius: 3, fill: 0x6750A4)
            }
            when percent > 80 {
                line(x1: 8, y1: 8, x2: 330, y2: 8, stroke: 0xF2B8B5, stroke_width: 2, dash: (6, 4))
            } else {
                line(x1: 8, y1: 88, x2: 330, y2: 88, stroke: 0x49454F)
            }
        }
        Slider #[bind: percent, min: 0.0, max: 100.0, step: 1.0, width: 340, height: 26]
    }
}
```

Every bar's height comes from the slider, so the whole chart follows it. There is
no reconciliation step and no node per bar. The same goes for a `var` filled from a
server: draw what the list holds.

## Paths

A `path` draws any shape out of straight and curved segments. Written as a body, its
commands are `move`, `line`, `quad`, `cubic` and `close`, each with plain
coordinates:

```byld
View Main() {
    Canvas #[width: 300, height: 140, bg: 0x1C1B22] {
        path(fill: 0xFF5B8DEF) {
            move(20, 120)
            line(20, 60)
            cubic(60, 10, 120, 110, 180, 50)
            quad(220, 20, 280, 60)
            line(280, 120)
            close()
        }
    }
}
```

- `move(x, y)` lifts the pen to a start point, and every path begins with one.
- `line(x, y)` draws a straight line to a point.
- `quad(cx, cy, x, y)` is a curve with one control point.
- `cubic(c1x, c1y, c2x, c2y, x, y)` is a curve with two.
- `close()` joins the end back to the start.

A body path is turned into a mesh, remembered, and not worked out again while its
numbers stay the same. A path whose data moved is redone. A path that does not
start with `move` is an error.

### Fills and fades

`fill` is a colour. `gradient` takes the same descriptor a box does, so an area
chart can fade from its curve to its baseline:

```byld
View Main() {
    Canvas #[width: 300, height: 140, bg: 0x1C1B22] {
        path(fill: 0x00000000,
             gradient: (kind: linear, angle: 90deg,
                        from: 0xFFF9A8A8, mid: 0x66F9A8A8, to: 0x00F9A8A8)) {
            move(0, 140)
            line(0, 90)
            cubic(70, 20, 150, 110, 220, 60)
            cubic(250, 40, 280, 50, 300, 50)
            line(300, 140)
            close()
        }
        bezier(x1: 0, y1: 90, cx1: 70, cy1: 20, cx2: 150, cy2: 110, x2: 220, y2: 60,
               stroke: 0xFFF9A8A8, stroke_width: 3, cap: round)
    }
}
```

Several closed outlines in one path are filled by a rule. With the default,
`nonzero`, an inner square drawn the same way round as the outer one is filled
solid. `winding: even_odd` leaves it out, which makes a ring:

```byld
View Main() {
    Canvas #[width: 160, height: 120, bg: 0x1C1B22] {
        path(fill: 0xFF5B8DEF, winding: even_odd) {
            move(40, 10) line(120, 10) line(120, 110) line(40, 110) close()
            move(60, 35) line(100, 35) line(100, 85) line(60, 85) close()
        }
    }
}
```

### A curve from data

A `for` or `when` inside a path body writes its commands from the data. One command
per sample draws a chart straight from a list:

```byld
View Main() {
    var week: List<Float> = [14.0, 17.5, 21.0, 19.0, 24.5, 22.0, 16.0]

    Canvas #[width: 540, height: 160, bg: 0x1C1B22] {
        path(fill: 0x00000000,
             gradient: (kind: linear, angle: 90deg,
                        from: 0xFF7CC4FF, mid: 0x667CC4FF, to: 0x007CC4FF)) {
            move(0, 160)
            for i, t in week { line(i * 90, 160 - t * 5) }
            line(540, 160)
            close()
        }
    }
}
```

### SVG text

`path(d: "M60 90 C30 70 …")` takes SVG path text instead of a body. It is drawn
from a baked picture, which suits static art, such as a logo or a heart. Use a body
when the geometry follows data.

## Clip masks

A `Clip` cuts what it holds to a shape. Its own box is the default. `rrect` rounds
the corners. A `path` child makes the mask any shape at all, and the path can read
state, so the mask can move:

```byld
View Main() {
    var notch = 18

    Clip #[width: 180, height: 120, tap => notch = notch == 18 ? 40 : 18] {
        path {
            move(0, 0)
            line(180, 0)
            line(180, 60 - notch)
            cubic(180 - notch, 60 - notch, 180 - notch, 60 + notch, 180, 60 + notch)
            line(180, 120)
            line(0, 120)
            close()
        }
        Column {
            Row {
                Box #[bg: 0xE8543F, width: 90, height: 60] {}
                Box #[bg: 0xF2B705, width: 90, height: 60] {}
            }
            Row {
                Box #[bg: 0x2D9B8A, width: 90, height: 60] {}
                Box #[bg: 0x4A6FD0, width: 90, height: 60] {}
            }
        }
    }
}
```

Tap the clip and the notch changes.

## Shapes that change into each other

`ngon` is one shape that covers polygons, rounded squares, scallops and bursts:
`n` is its number of sides, `corner` rounds them, and `inner` pulls alternate points
in to make a star (`inner: 0.42` with `n: 5` is a burst).

A canvas can turn one shape into the next. Give it `morph: <number>`, and its shapes
become a sequence the number indexes, wrapping at the end. Animate the number and
the picture changes shape, with the colour blended along the way:

```byld
View Main() {
    var selected = false

    Canvas #[width: 120, height: 120, bg: 0x1C1F27,
             morph: selected ? 1.0 : 0.0 with anim.spring(stiffness: 200, damping: 0.6),
             tap => selected = !selected] {
        ngon(cx: 60, cy: 60, r: 44, n: 4, corner: 8, fill: 0xE8B54A)
        circle(cx: 60, cy: 60, r: 44, fill: 0xE8B54A)
    }
}
```

The same scalar can run forever, `morph: 7.0 with anim.linear(4550ms, from: 0.0,
repeat: infinite)` over seven shapes is a loading indicator that cycles through
them. Two *paths* morph too. They need the same commands in the same order, and a
mismatch names the first command that differs. Add `morph_mode: resample` to the
canvas and paths of different structure can morph, a heart of curves into a star of
lines, as long as they have the same number of subpaths.

`fuse: 20` on a canvas merges its shapes where they come close, so circles bridge
into one blob, and their colours blend across the bridge. The number is the
distance, in pixels, over which two shapes join. `fuse` and `morph` cannot be used
on the same canvas, and a fused group has one outline, taken from its first shape.

```byld
View Main() {
    Canvas #[width: 340, height: 110, bg: 0x1C1F27, fuse: 22] {
        circle(cx: 70, cy: 55, r: 26, fill: 0x5B8DEF)
        circle(cx: 250 with anim.ease_in_out(1800ms, from: 130, repeat: infinite, reverse: true),
               cy: 55, r: 20, fill: 0xE8734A)
    }
}
```

## Drawing to the size you have

A canvas cannot size itself to its content, but it can be told the size of what
holds it. Measure the parent with [`on measure`](layout.md#knowing-your-own-size)
and use the number in the canvas's own `width` and in its coordinates. The chart
then fills its card at any window size.

When a drawing needs behaviour a canvas cannot express, such as a map, you can write
it in Rust. See [native views](rust-native-views.md).
