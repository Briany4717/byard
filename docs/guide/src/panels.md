# Panels and screens

A `Panel` is a surface that sits against the edges of a screen instead of being a window:
a bar, a dock, a launcher, an on-screen display. It is a Wayland layer surface
(`zwlr_layer_surface_v1`), and like a `Window` it is an element written at the top of the
program, sharing the program's state with every other surface.

The smallest bar is three lines:

```byld
View Main() {
    Panel #[edge: top] {
        Text("Hello from the bar") #[p: 8]
    }
}
```

That is a whole program. The panel sits against the top of the screen, across all of it,
as tall as its text, and windows make room for it. Everything below is something to add
when the default stops being enough.

A panel's background is transparent: only what the program draws is drawn, and the
pointer goes through to whatever is below wherever the program put nothing that takes it.
A bar drawn as a floating island with rounded corners therefore behaves like one, with
no mask to write.

## Where a panel goes

| Write | It is |
|-------|-------|
| `edge: top` (`bottom`, `left`, `right`) | Against that edge, across all of it |
| `anchor: (top, right)` | In that corner, as large as its content or its `width` and `height` |
| `anchor: bottom` | Centred along the bottom edge |
| neither | Centred on the screen: a launcher, a dialog |

`edge: top` is `anchor: (top, left, right)`, and writing both is
`InvalidAttributeCombination`. `m`, `mt`, `mr`, `mb`, `ml`, `mx` and `my` move the panel
off the edges it is anchored to. Screen edges are physical, so `left` and `right` are
written and never `start` and `end`.

## What a panel takes

| Attribute | Type | Default | |
|-----------|------|---------|-|
| `screen` | `Screen` | the compositor's choice | The screen it is made on. Fixed when the panel is made. |
| `edge`, `anchor` | `top`, `bottom`, `left`, `right` | none | See above. Reactive. |
| `layer` | `background`, `bottom`, `top`, `overlay` | `top` | Reactive. |
| `exclusive` | `Bool`, `Float` or `ignore` | `true` with `edge`, else `false` | Whether it reserves space. Reactive. |
| `keyboard` | `none`, `on_demand`, `exclusive` | `none` | Who gets the keys. Reactive. |
| `namespace` | `Str` | the program's app id | The name a compositor's rules match. Fixed when the panel is made. |
| `width`, `height` | `Float` | the content's size | Reactive. |
| `m`, `mt`, `mr`, `mb`, `ml`, `mx`, `my` | `Float` | 0 | Reactive. |
| `input` | `auto`, `all`, `none` | `auto` | Which part of it takes the pointer. |

**Size.** Along an axis the panel is anchored at both ends of, the compositor stretches
it, and the size is the screen's. Along any other it is `width` or `height` when written,
and otherwise the size of its content laid out with that axis unbounded. When the content
changes size (a clock gains a digit), the panel asks for the new size and is drawn at
it, so an island is always as large as what is in it.

A `width` on a panel anchored to left and right, or a `height` on one anchored to top and
bottom, does nothing, and the checker says so (`SizeOnStretchedAxis`).

**`exclusive`.** `true` reserves the panel's thickness along its edge, so maximised
windows start below the bar; a number reserves that many pixels; `false` lets windows go
under it; `ignore` covers what other panels reserved too, as a wallpaper does. A bar on
an `edge` reserves by default, because a bar windows slide under is the surprise nobody
wants on the first run. `exclusive: true` without an `edge` and without three anchors does
nothing, which is `ExclusiveNeedsEdge`.

**`keyboard`.** With `none` the panel never has the keyboard, so a bar does not take the
keys from the window being typed in. A `TextField` or a `key_down` handler in such a panel
could never be used, and the checker warns (`PanelTakesNoKeys`): write
`keyboard: on_demand` for a panel that takes the keys when clicked, or `exclusive` for a
launcher, which has every key while it is shown.

**`namespace` and `screen`** are fixed when the surface is made, so changing them destroys
the panel and makes it again, with its state. The checker warns when they read a `var`
(`RecreatesSurface`). Everything else is sent to the compositor as it changes.

## The input region

With `input: auto`, the part of a panel that takes the pointer is where the program put
something that does: a `tap` or `long_press` handler, a pointer or drag handler, a `hover`
block, a `ScrollView`, a `TextField`, a `Slider`, and a modal overlay's scrim, but not
anything inside an `interactive: false` element. The rest of the panel lets the pointer
through. It is worked out when the panel is drawn and sent to the compositor only when it
changed. `input: all` takes every press on the panel, and `input: none` takes none, for an
on-screen display.

## A bar on every screen

`screens` is the list of the connected screens, each a `Screen` with `name`,
`description`, `x`, `y`, `width`, `height` and `scale`, ordered by `x` and then `y`. It
is a value like `window`: nothing exists for a program that never reads it.

```byld
View Main() {
    for s in screens key s.name {
        Panel #[screen: s, edge: top, height: 40, namespace: "bar"] {
            Row #[p: 8, gap: 8, align: center, bg: 0xE01C1B22, radius: 12, mx: 8, my: 4] {
                Text(s.name)
                Spacer
                Text("{s.width}x{s.height}")
            }
        }
    }
}
```

When a screen is connected `screens` grows and the `for` gives it its panel; when one is
unplugged its panel goes. `key s.name` is what keeps the other bars as they are: without
it a `for` matches its rows by position, and a screen connected in the middle of the list
would hand each bar the next screen's place, with its state. A `for` that makes panels
without a `key` is `SurfaceForNeedsKey`. A screen that is re-scaled keeps its panels, and
what reads `s.scale` follows.

## Popups

A bar is 40 pixels high, so a menu that hangs from it cannot be drawn inside it. In a
`Panel`, an `Overlay` whose first child has `anchor_to` is written the way it is in any
app (see [Layout](layout.md)), and becomes a popup: a surface of its own, placed by the
compositor against the element it names, allowed to be larger than the bar, and flipped to
the other side of the element when there is no room.

```byld
View Main() {
    var open = false
    Panel #[edge: top, height: 40, namespace: "bar"] {
        Row #[pl: 12, align: center] {
            Button("Menu") #[width: 80, height: 30] as menu => open = !open
        }
        when open {
            Overlay #[modal: false] {
                Column #[anchor_to: menu, anchor_edge: below, anchor_gap: 4, width: 180,
                         bg: 0x2B2930, radius: 12, p: 8, a11y_role: menu,
                         dismiss => open = false] {
                    Button("Settings") #[height: 32]
                    Button("Quit") #[height: 32]
                }
            }
        }
    }
}
```

`anchor_edge`, `anchor_align` and `anchor_gap` say where the popup goes, as they do in a
window; `anchor_flip: false` keeps it from flipping. Its size is what its content measures,
laid out with no limit, and it follows the content when it changes. It is created when the
overlay mounts, and destroyed after its `on leave` motion has settled, so a popup that
fades out stays until it has.

A popup that is a focus scope (the overlay is modal, or its first element with a role is a
`menu` or a `dialog`) has the grab: it takes the keyboard, Escape and a click outside it
run its `dismiss`, as a press outside does in a window, and the keyboard goes back to the
panel when it closes. A popup that is not one, a tooltip, never takes the keyboard. A
`dismiss` that does not close the overlay leaves it open, although the compositor has
already dismissed it.

An overlay in a panel with no `anchor_to` is drawn inside the panel, as in a window, and
so is every overlay in a `Window`: only a panel's popups are surfaces. An anchored overlay
inside a popup, a submenu, is a popup whose parent is the popup. A popup is drawn only
when something it shows changed, like any surface.

## Drawing and testing without a compositor

`byard shot` has no compositor, so it is one. `--screens` describes the screens, and the
picture is all of them side by side with every surface on them, over a checkerboard where
the panels are transparent. Panels are placed by layer shell's rules (anchors, margins and
exclusive zones, in the order the compositor applies them), windows are centred in the area
the panels left, and popups are positioned and flipped by the same rules a compositor uses
(`byard_core::surface::placement` and `positioner`).

```sh
byard shot crates/byard-cli/examples/shell_demo --screens "DP-1:1600x900, HDMI-A-1:1280x720" --scale 1 -o screens.png
byard shot crates/byard-cli/examples/shell_demo --screens "DP-1:1600x900, HDMI-A-1:1280x720" --surface bar --screen DP-1 -o bar.png
byard shot crates/byard-cli/examples/shell_demo --screens "DP-1:1600x900, HDMI-A-1:1280x720" --scale 1 --tap 40,30 -o menu.png
```

`--surface` draws one surface alone, by its `as` name, else its `namespace`, at the size it
was given, with transparent pixels where nothing was drawn; `--screen` says which screen's
instance of a bar made once per screen. Without `--surface`, `--tap` and the others take
points of the screens and go to the topmost surface whose input region holds the point, so
a tap on a panel's transparent part reaches the window below it; a tap outside the popup
that has the grab closes it, as the compositor would. See [the CLI reference](cli-reference.md#screens).

`byard test` sees every surface: its dump has a `Panel "namespace" @ DP-1` line (a `Popup
"name" @ DP-1` line for a popup) with the tree indented below, a step that names an element
finds it on whichever surface it is on, and `screens = "DP-1:1600x900, HDMI-A-1:1280x720"`
at the top of a scenario file, or in one test, says the screens.

Running panels on the desktop needs the Wayland shell backend, which comes after the
language and the interpreter (RFC-0068 step 3b). `byard dev` and a built app say so and
stop for now. A program that has a view called `Panel` must call it something else, as
`Panel` is the element.
