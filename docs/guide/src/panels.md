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

## Drawing and testing without a compositor

`byard shot` has no compositor, so it places the panels itself, by layer shell's rules
(`byard_core::surface::placement`), on one simulated screen of the shot's size. Name the
panel to draw: its `as` name, else its `namespace`.

```sh
byard shot crates/byard-cli/examples/simple_bar --surface bar -o bar.png
byard shot crates/byard-cli/examples/simple_bar --surface clock --at 12000 -o clock.png
```

The picture is the panel alone, at the size it was given, and its pixels are transparent
where nothing was drawn. `byard test` sees a panel like a window: its dump has a
`Panel "namespace" @ screen-1` line with the tree indented below, and a step that names
an element finds it on whichever surface it is on.

A program that has a view called `Panel` must call it something else, as `Panel` is the
element.

## Running a panel

```sh
byard dev crates/byard-cli/examples/simple_bar
```

A program with a `Panel` runs on the Wayland shell backend, in `byard dev` with hot reload
and in a built app (`byard::App::run`) the same way. The backend is chosen when the program
starts, from whether it has a `Panel` anywhere: an edit under `byard dev` that adds the first
panel (or takes the last away) says it needs a restart and keeps the program that is running.
A program without panels runs as it always did, on every platform.

**Which builds include the backend.** `byard dev`, `byard shot` and `byard test` from this
repository (`byard-cli`) include it on Linux. An app of your own does not, because a program
with no panel would carry it for nothing: add the feature in its `Cargo.toml`,

```toml
byard = { version = "0.1", features = ["wayland-shell"] }
```

and without it `App::run` on a program with a `Panel` stops with `PanelsUnsupported`, saying
so. `byard build` only bakes assets and does not compile the app, so it prints this line
when the program has a `Panel`. `cargo install byard --features wayland-shell` gives a
`byard` that runs panels.

It works on compositors that offer `zwlr_layer_shell_v1`: Hyprland, Sway, river, niri,
labwc, Wayfire, KDE Plasma and the other wlroots-style ones. It does not work on GNOME,
whose compositor has no layer shell, nor on X11, Windows or macOS. A panel never falls back
to an ordinary window, since a bar that opens as a window is broken in a confusing way, so
there the program stops at startup with `PanelsUnsupported` and says what is missing: the
Wayland session, or the `zwlr_layer_shell_v1` the compositor does not offer.

What it does for you:

- Each panel draws only when its own frame callback fires and it has something new, so a bar
  on a screen that is off, or behind a full-screen window, costs nothing, and a still bar
  commits nothing: with `WAYLAND_DEBUG=1` the terminal stays quiet while nothing changes.
- Each screen is read from `wl_output` (its connector name, and `xdg_output`'s position and
  size) and a fractional scale is used where the compositor offers it, an integer one where
  not. `screens` follows a screen being connected, unplugged or re-scaled while the program
  runs, and a panel of a screen that goes is taken away by the compositor.
- A panel's input region is sent only when it changed. The pointer, the scroll wheel and
  touch reach the panel under them in its own coordinates.
- A `Window` in a program with panels is an ordinary window on the same connection, so a
  settings window opens next to the bar.
- With the `a11y` feature, each panel and window is a window under the program's AT-SPI
  root.

What it does not do yet: **a text field in a panel takes the keys the keymap gives it, but not
an input method** (no IME, no dead-key composition), so typing Japanese or an accent by
compose key does not work there; a text field in a `Window` of the same program has the same
limit on this backend. Popups from a panel are a later step: an anchored `Overlay` is drawn
inside the panel for now. The app's appearance does not follow the desktop's light or dark
setting on this backend (reading it needs a D-Bus client Byard does not carry): `scheme =
"system"` uses the theme's own scheme, and the terminal says so once at startup.
