# Layout

```byld
View Main() {
    Column #[bg: 0x14141C, p: 20, gap: 16, grow: 1] {
        Row #[align: center] {
            Text("Inbox") #[color: 0xFFFFFF, size: 22, weight: bold]
            Spacer
            Button("Compose") #[bg: 0x3B82F6, color: 0xFFFFFF, radius: 8]
        }
        Box #[bg: 0x1E1E2A, radius: 12, p: 16, gap: 4] {
            Text("Ada Lovelace") #[color: 0xFFFFFF, size: 16]
            Text("The engine might compose elaborate music.") #[color: 0x9AA0AC, size: 13]
        }
        Spacer
        Text("2 unread") #[color: 0x9AA0AC, size: 12]
    }
}
```

Layout in Byard is flexbox. Every container lays its children out along one axis,
the *main axis*, and you say how they are spaced and aligned. The engine does the
arithmetic, and only containers whose inputs changed are laid out again.

## The containers

| Element | What it does |
|---------|--------------|
| `Column` | Children top to bottom |
| `Row` | Children left to right |
| `Box` | A container that is a column unless you say `direction: row` |
| `Grid` | Children in rows and columns of a template you write |
| `ZStack` | Children on top of each other |
| `ScrollView` | A window onto content that is taller or wider than it |
| `Overlay` | A layer above everything, such as a dialog or a menu |
| `Spacer` | Empty space that grows to fill what is left |

`Box`, `Column` and `Row` take the same attributes. Use `Column` and `Row` when
you mean a stack, and `Box` when the thing is a card or a surface with a
background. The names carry intent for the reader and cost nothing.

## Size

`width` and `height` are in logical pixels, so they mean the same on a dense
screen as on an ordinary one. Without them a container is as big as its content,
and a column fills the width it is given.

`grow` hands out whatever room is left on the main axis, in proportion. Two
children with `grow: 1` and `grow: 2` split it one to two. `Spacer` is a child
that does nothing but `grow`. The root of a screen usually says `grow: 1`, so it
fills the window instead of hugging its content.

## Space

`gap` is the space between children. `p` is padding, inside the container, and
`m` is margin, outside it. Both take one number for all sides, or a pair or quad:

| Written | Means |
|---------|-------|
| `p: 12` | 12 on every side |
| `p: (8, 16)` | 8 above and below, 16 left and right (vertical, horizontal) |
| `p: (4, 8, 12, 16)` | top, right, bottom, left, in CSS order |
| `p: (top: 20, horizontal: 24)` | named sides, in any order |

Named sides are `top`, `right`, `bottom`, `left`, `horizontal` and `vertical`. A
named form cannot mix with a positional one, and naming a side twice, or `left`
beside `horizontal`, is an error. Single sides have short forms too: `pt`, `pr`,
`pb`, `pl`, and for margin `mt`, `mr`, `mb`, `ml`, `mx` and `my`.

## Alignment

Two attributes position children, and they work on different axes.

- `justify` places them along the main axis: `start`, `center`, `end`,
  `between`, `around` or `evenly`.
- `align` places them across it: `start`, `center`, `end`, `stretch` or
  `justify`.

```byld
View Main() {
    Column #[bg: 0x14141C, p: 20, gap: 12, width: 320] {
        Row #[bg: 0x1E1E2A, p: 8, justify: between] {
            Text("left") #[color: 0xFFFFFF]
            Text("middle") #[color: 0xFFFFFF]
            Text("right") #[color: 0xFFFFFF]
        }
        Row #[bg: 0x1E1E2A, p: 8, height: 60, justify: center, align: center] {
            Text("centred both ways") #[color: 0xFFFFFF]
        }
        Row #[bg: 0x1E1E2A, p: 8, gap: 8] {
            Box #[bg: 0x3B82F6, height: 24, grow: 1] {}
            Box #[bg: 0x8FD19E, height: 24, grow: 2] {}
        }
    }
}
```

## Grid

A `Grid` takes a template for its columns, and optionally its rows, as text.

```byld
View Main() {
    Grid #[columns: "1fr 1fr 1fr", gap: 12, width: 400, height: 240, bg: 0x14141C, p: 12] {
        Box #[bg: 0x381E72, radius: 12, col_span: 2, p: 12] { Text("Revenue") #[color: 0xFFFFFF] }
        Box #[bg: 0x4A4458, radius: 12, p: 12] { Text("Users") #[color: 0xFFFFFF] }
        Box #[bg: 0x633B48, radius: 12, p: 12] { Text("Orders") #[color: 0xFFFFFF] }
        Box #[bg: 0x1F3A34, radius: 12, col_span: 2, p: 12] { Text("Latency") #[color: 0xFFFFFF] }
        Box #[bg: 0x2A2A3A, radius: 12, row: 3, col_span: 3, p: 12] {
            Text("Notes, placed in row 3") #[color: 0xFFFFFF]
        }
    }
}
```

In a template, `1fr` is a share of the free space, a bare number is a width in
pixels, and `auto` fits the content. `repeat(3, 1fr)` is three equal columns.
Children fill the cells in order, left to right and then down. `col_span` and
`row_span` make one cover several cells, and `col` and `row` put one in a cell of
your choosing, counting from 1. `gap` spaces both directions, and `col_gap` and
`row_gap` set each on its own. A template that does not parse is an error that
says so.

## ZStack

A `ZStack` puts its children in the same rectangle, the last one on top. A child
smaller than the stack is placed by `alignment`, which defaults to `center`:

```byld
View Main() {
    ZStack #[width: 260, height: 150, alignment: bottom_end] {
        Box #[bg: 0x1F3A34, radius: 16, width: 260, height: 150] {}
        Box #[bg: 0xF2B8B5, radius: 12, width: 44, height: 26, m: 8] {}
    }
}
```

The values are `center`, `top`, `bottom`, `start`, `end`, and the corners
`top_start`, `top_end`, `bottom_start` and `bottom_end`.

## ScrollView

A `ScrollView` holds content larger than itself and scrolls it with the wheel, a
trackpad or a drag. Give it a size, and its `offset` a `var` if you want to read
or set where it is:

```byld
View Main() {
    var y = 0.0

    Column #[bg: 0x14141C, p: 20, gap: 12, width: 340] {
        ScrollView #[width: 300, height: 180, offset: (0, y), bg: 0x1E1E2A, radius: 12] {
            Column #[p: 12, gap: 8] {
                for i in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
                    Text("Row {i}") #[color: 0xFFFFFF, size: 16]
                }
            }
        }
        Slider #[bind: y, min: 0.0, max: 300.0, step: 1.0, width: 300]
    }
}
```

`offset: (x, y)` is two-way. Dragging writes the `var`, and writing the `var`
moves the content. `axis` is `vertical` (the default), `horizontal` or `both`.

For a long list, add `windowed: true` and `row_height`. Only the rows in view are
built. Every row must then be exactly `row_height` tall, margins included, because
the window works out which rows are visible by multiplying. Put spacing in the
row's own height or margin, not the container's `gap`.

A `ScrollView` also snaps to pages or items (`snap: page`, `snap: item`, with
`snap_align`), reports `page` and `page_count`, calls `end_reached =>` near the
end for infinite lists, and can pull to refresh (`pull_refresh: true` with
`refresh =>`). A header that shrinks as you scroll is `collapse_header: true`
with `collapse_min`; the children of that header read the engine's
`scroll_fraction`, from 0 to 1. The `scroll_snap` and `collapse_header` examples
show each one working.

## Overlay

An `Overlay` is drawn above everything else, in the window's own space and not in
its parent's flow. It takes no room where it is written. Use it for dialogs,
sheets, menus and tooltips.

```byld
View Main() {
    var open = true

    Column #[bg: 0x14141C, p: 20, gap: 12, grow: 1] {
        Text("The page") #[color: 0xFFFFFF]
        Button("Open the sheet") => open = true
    }

    when open {
        Overlay #[dismiss => open = false] {
            Box #[anchor: bottom, bg: 0x333344, radius: 16, p: 20, gap: 8] {
                Text("A sheet") #[color: 0xFFFFFF, size: 18]
                Button("Close") => open = false
            }
        }
    }
}
```

By default an overlay is *modal*: a scrim covers the page and takes its input.
`dismiss =>` runs when the scrim is tapped or Escape is pressed. A child places
itself with `anchor`: `center`, `top`, `bottom`, `start` or `end`.
`at: (x, y)` puts a child at an exact offset from the window's top-left instead.

### Anchored to an element

A dropdown or a tooltip belongs next to the thing that opened it. Name that
element with `as name` after its attributes, and point the overlay's child at it
with `anchor_to`:

```byld
View Main() {
    var query = "bu"

    Column #[bg: 0x0E1116, p: 24, gap: 16, grow: 1] {
        TextField #[bind: query, placeholder: "Search locations", width: 240] as searchField
    }

    when query != "" {
        Overlay #[modal: false] {
            Box #[anchor_to: "searchField", anchor_edge: below, anchor_align: start,
                  anchor_gap: 6, width: match(searchField), bg: 0x1C2430, radius: 10, p: 10,
                  dismiss => query = ""] {
                Text("Bucharest") #[color: 0xE7ECF2]
                Text("Budapest") #[color: 0xE7ECF2]
            }
        }
    }
}
```

`modal: false` keeps the page live under the overlay. The panel hangs below the
field (`anchor_edge` is `above`, `below`, `before` or `after`), lines up with its
start (`anchor_align` is `start`, `center` or `end`), sits 6 pixels off
(`anchor_gap`) and is as wide as the field (`width: match(searchField)`). If there
is no room below, it flips above. `dismiss =>` here runs on a press outside both
the panel and its anchor, and it needs an `anchor_to` on the same element.

## Images and icons

```byld,fragment
Image("assets/photo.png") #[width: 120, height: 80, fit: cover, radius: 12]
```

`Image` shows a picture from a path. `fit` is `fill`, `contain` (the default),
`cover` or `none`. It takes `radius`, `smooth` and `opacity`, and the layout
attributes. A changed file reloads under `byard dev`. `VectorIcon("path.svg")` shows
a single-colour vector icon, tinted with `color` and sized with `size`.

## Knowing your own size

Sometimes a child needs to know how wide its parent turned out. `on measure` runs
an action whenever the element it is written in has been laid out, with the
result as `it`:

```byld
View Main() {
    var w: Float = 0.0

    Column #[bg: 0x14141C, p: 20, grow: 1] {
        on measure => { w = it.w }

        Text("This card is {w:.0} wide") #[color: 0xFFFFFF]
        Canvas #[width: w - 40.0, height: 40, bg: 0x1E1E2A] {
            line(x1: 8, y1: 20, x2: w - 48.0, y2: 20, stroke: 0x3B82F6, stroke_width: 3)
        }
    }
}
```

`it.w` and `it.h` are the element's width and height. The line is drawn to the
measured width and not to a number written in the file, so it stays right as the
window resizes. Use the measurement downward: a parent measures, its children
read the number. The other way round never settles.

## Text

`Text` is the one element that shows words. Its content may be any text, with
[interpolation](expressions.md#text-and-interpolation), and it wraps to the width
its parent offers. `wrap: false` keeps it on one line. The look of text is covered under [styling](styling.md).

Next, [styling](styling.md) makes all of this look like something.
