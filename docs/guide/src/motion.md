# Motion

```byld
View Main() {
    var on = false

    Column #[bg: 0x14141C, p: 40, gap: 24, align: center, grow: 1] {
        Box #[width: 90, height: 90, radius: 20, origin: center,
              bg: on ? 0xEF5350 : 0x3B82F6 with anim.ease_in_out(300ms),
              scale: on ? 1.3 : 1.0 with anim.spring(),
              rotate: on ? 45deg : 0deg with anim.spring(),
              tap => on = !on] {}
        Text("Tap the square") #[color: 0x9AA0AC]
    }
}
```

Add `with anim.something()` after a value, and when that value changes, the property
moves toward it instead of jumping. Nothing is scheduled and nothing is cancelled:
the value is the goal, and the curve is how to get there.

## Where `with` goes

`with` follows any value of a *paint* property: colours, opacity, shadows, blur, and
the transforms `translate`, `scale`, `rotate` and `origin`. Paint properties change
pixels inside a rectangle layout has already decided, so animating them costs no
layout and runs on the GPU.

It does not follow a *layout* property. `width`, `height`, `gap`, padding and the
rest decide where other things go, so animating them would lay the screen out every
frame. The checker refuses, and says what to do:

```byld,fails
View Main() {
    var wide = false
    Box #[width: wide ? 200 : 80 with anim.spring(), height: 20] {}
}
```

```diagnostic
error[LayoutPropNotAnimatable]: `width` is a layout property and cannot be animated
```

To grow or shrink something smoothly, animate its `scale`.

`with` has the lowest precedence of the expression operators short of assignment, so
`on ? a : b with anim.spring()` animates the whole choice.

## Curves

| Curve | Reads as |
|-------|----------|
| `anim.spring()` | Physical: it overshoots and settles. The default for most things |
| `anim.linear(200ms)` | A steady rate for the time given |
| `anim.ease_in(200ms)` | Starts slow |
| `anim.ease_out(200ms)` | Ends slow |
| `anim.ease_in_out(200ms)`, `anim.ease(200ms)` | Slow at both ends |

A spring takes `stiffness`, `damping` and `initial_velocity`, all optional:
`anim.spring(stiffness: 200, damping: 0.6)`. A higher stiffness is quicker, and a
lower damping rings for longer. A spring has no fixed length: it settles when it
settles. A timed curve is for something that must take exactly so long.

A mistake in a curve is reported with the name you probably meant, whether it is the
curve (`anim.sping`), an argument, or a modifier.

## Modifiers

Any curve accepts these after its own arguments:

| Modifier | Does |
|----------|------|
| `from: v` | Starts at `v` instead of the current value |
| `repeat: 3` or `repeat: infinite` | Plays that many times |
| `reverse: true` | Alternate plays run backwards |
| `delay: 200ms` | Waits first |
| `duration: 300ms` | The length, for a curve that takes one |
| `restart: v` | Plays again whenever `v` changes |

```byld,fragment
Box #[width: 56, height: 56, radius: 28, bg: 0x2B2930, origin: center,
      scale: 1.4 with anim.spring(from: 1.0, stiffness: 90, damping: 9,
                                  repeat: infinite, reverse: true)] {}
```

## Looping

An endless animation is `repeat: infinite`. It has no start and stop calls. It runs
as long as its element is on screen, and removing the element stops it:

```byld
View Main() {
    var loading = true

    Column #[bg: 0x141218, p: 28, gap: 20, width: 360] {
        Button(loading ? "Stop" : "Start") #[bg: 0x4A4458, color: 0xE6E1E5, radius: 20, p: (8, 16)]
            => loading = !loading

        when loading {
            Box #[width: 56, height: 56, origin: center,
                  rotate: 360deg with anim.linear(1s, from: 0deg, repeat: infinite)] {
                Canvas #[width: 56, height: 56] {
                    arc(cx: 28, cy: 28, r: 22, stroke: 0x36323C, stroke_width: 5)
                    arc(cx: 28, cy: 28, r: 22, start: -90, sweep: 250,
                        stroke: 0xD0BCFF, stroke_width: 5, cap: round)
                }
            }
        }
    }
}
```

Press Stop, and the spinner leaves the tree, its animation goes with it, and the
app stops drawing frames. That is how an app with a loader still idles at zero cost
when the load is done. A finite `repeat: 3` ends by itself and needs no `when`.

A looping gradient offset makes a shimmer, the skeleton of a loading list. The
ramp is part of each bar, so it is clipped to the bar's rounded corners:

```byld
View Main() {
    let bar = style {
        bg: 0x2B2930, radius: 4, height: 18
        gradient: (angle: 180deg, from: 0x00FFFFFF, mid: 0x26FFFFFF, to: 0x00FFFFFF, mid_pos: 0.5)
        gradient_offset: 1.0 with anim.linear(1.4s, from: 0.0, repeat: infinite)
    }

    Column #[bg: 0x141218, p: 28, gap: 10, width: 360] {
        Box #[..bar, width: 300] {}
        Box #[..bar, width: 220] {}
        Box #[..bar, width: 140] {}
    }
}
```

## Keyframes

When there are several stops, write them in the value itself, with `anim.keyframes`.
Each stop is a percentage of the `duration`, a value, and optionally the easing into
it:

```byld
View Main() {
    Column #[bg: 0x141218, p: 28, width: 360] {
        Box #[bg: 0x36323C, radius: 3, width: 300, height: 6] {
            Box #[bg: 0xD0BCFF, radius: 3, width: 100, height: 6, origin: center,
                  translate: anim.keyframes(
                      0%: (-100, 0),
                      60%: (120, 0) ease_in_out,
                      100%: (300, 0) ease_out,
                      duration: 2s, loop: true),
                  scale: anim.keyframes(
                      0%: (0.3, 1),
                      60%: (1.6, 1) ease_in_out,
                      100%: (0.3, 1) ease_in,
                      duration: 2s, loop: true)] {}
        }
    }
}
```

That is Material's indeterminate progress bar: two tracks of keyframes that loop.
There must be at least two stops, in order, and `duration` is required. `loop: true`
repeats it. Keyframes replace the value, so they are not written after `with`.

## Lists that arrive in turn

Give every row of a list the same animation, and a delay that grows with its
position, and they cascade in. `anim.stagger(curve, step, index)` does it:

```byld
View Main() {
    var entrance = 0
    let rows = ["Inbox", "Starred", "Snoozed", "Sent"]

    Column #[bg: 0x141218, p: 28, gap: 8, width: 360] {
        Button("Replay") => entrance++
        for i, row in rows {
            Row #[bg: 0x211F26, radius: 8, p: (10, 14),
                  opacity: 1.0 with anim.stagger(linear(320ms, from: 0.0), 90ms, i,
                                                 restart: entrance),
                  translate: (0, 0) with anim.stagger(spring(from: (0, 18)), 90ms, i,
                                                      restart: entrance)] {
                Text(row) #[color: 0xE6E1E5, size: 14]
            }
        }
    }
}
```

`for i, row in rows` gives each row its position `i`. Row `i` starts `i * 90ms`
after the first. The rows are written once and each has its own timeline, so they
can run at different points. `restart: entrance` replays the cascade each time the
button changes `entrance`, which is how you run a start-up animation again without
reloading.

## Transforms

`translate`, `scale`, `rotate` and `origin` move how an element is *drawn*, never
where layout put it. A transformed element keeps its place in its parent, takes the
same room, and nothing around it shifts.

| Attribute | Takes | Example |
|-----------|-------|---------|
| `translate` | a pair of pixels | `translate: (40, 0)` |
| `scale` | one number or a pair | `scale: 1.2`, `scale: (1.5, 1)` |
| `rotate` | an angle | `rotate: 45deg` |
| `origin` | the point they pivot on | `origin: center` |

One axis can be set alone: `translate.y: 4`. To pivot on the middle of the element, as a
spin wants, say `origin: center`.

Transforms compose down the tree. A button inside a rotated card turns with it, and
a tap on it lands where it is drawn. They apply to text too, and a rotated label
turns with its card. A `Clip` inside a rotated card is cut along the card's turned
edges.

```byld
View Main() {
    var angle = 0

    let grows = style {
        on hover { scale: 1.08 }
    }

    Column #[bg: 0x0E1116, p: 28, gap: 18, width: 420, height: 360] {
        Button("tilt") #[bg: 0x2A3442, color: 0xE7ECF2, radius: 8] => angle = angle + 30
        Box #[m: (left: 60, top: 20), width: 240, height: 180, bg: 0x1C2430, radius: 16,
              p: 20, rotate: angle * 1deg, origin: center] {
            Box #[..grows, width: 90, height: 44, bg: 0x4A6FD0, radius: 10, origin: center,
                  justify: center, align: center] {
                Text("+1") #[color: 0xFFFFFF, size: 18]
            }
        }
        Text("tilted {angle}°") #[color: 0xF2B705, width: 110, rotate: angle * -0.5deg, origin: center]
    }
}
```

`angle * 1deg` turns a number into an angle, so a `var` of whole degrees can drive a
rotation. Note that the text gets a `width`: its box is as wide as its column
offers, so `origin: center` is only the middle of the words when the box is as wide
as the words.

Style states can animate too. Put the `with` on the attribute in the style's own
attributes, and the `on hover` block only changes the target:

```byld,fragment
let grows = style {
    scale: 1.0 with anim.spring()
    on hover { scale: 1.08 }
}
Box #[..grows, width: 50, height: 50, bg: 0x3B82F6] {}
```

The frosted-glass example does the same with `blur: 0 with anim.spring()` beside
`on hover { blur: 16 }`.

## Why this is cheap

An animation is an ordinary value that changes over time, so it goes through the same
path as any other change to a paint property, with no layout. While at least one animation is
running the app ticks once for each frame the display shows. When none is, the app waits for the next input.
