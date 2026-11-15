# RFC-0068: Windows, panels and screens

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-15
- **Last updated:** 2026-11-15
- **Depends on:** RFC-0001 (the logic and render threads, the relay), RFC-0004
  (`when` and `for` as structural effects), RFC-0029 (the frame waker, wait mode),
  RFC-0032 (incremental redraw), RFC-0042 (the accessibility tree), RFC-0045 (the
  window's app id), RFC-0060 (premultiplied sRGB output), RFC-0066 (overlays, focus
  scopes, `anchor_to`, `interactive: false`), RFC-0067 (`window`).

> **Placement:** `docs/rfcs/0068-windows-panels-and-screens.md`

---

## Summary

A Byard program draws into exactly one window today. This RFC lets one program
own several surfaces, all of them sharing its state:

1. **`Window`**, an element that is a window of its own. A program can open a
   second window with `when`, or one per item with `for`.
2. **`Panel`**, an element that is a Wayland *layer surface*, the thing bars,
   docks, launchers, notifications and on-screen displays are made of. It is
   placed against the edges of a screen and can reserve space there.
3. **`screens`**, a reactive list of the connected screens, so a program can
   write `for s in screens key s.name { Panel #[screen: s] { … } }` and have a
   bar on every monitor, including one plugged in later.
4. **`for … key`**, so an item whose key is still in the list keeps what it
   made (its state, and its window or panel) when the list changes.
5. **Popups.** In a `Panel`, an `Overlay` anchored to an element becomes a popup
   surface of its own, so a menu can hang below a 40-pixel bar.
6. **Headless screens.** `byard shot` and `byard test` lay out panels and
   popups on a simulated screen by the same rules the compositor uses, with no
   compositor at all.

A program that writes none of these still has its one window, through the same
code it runs through now, and pays nothing for the rest.

The simple path and the deep one are kept apart on purpose. A bar is three
lines of Byld with defaults that do what a first bar should. Below that, Rust
code can reach the Wayland connection, every global the compositor offers and
the SCTK object behind each surface (§8b), for anything the attributes do not
cover.

This is the engine half of **Mantle** (`byard-mantle`), a package for building
Wayland desktop shells in Byld, the way Quickshell builds them in QML. Mantle's
own part, the system services (the window manager, audio, power, notifications,
the tray) and the `mantle` command, will be a separate RFC. This one contains
nothing that only a shell can use. Several windows are for every app, and a
`Panel` is an element like any other, which a launcher or a clipboard manager
can use as much as a shell can.

## Motivation

### A shell is many surfaces with one state

A desktop shell has a bar on each screen, a dock, a notification column, an
on-screen display for the volume, a launcher, and popups hanging from the bar.
These are a dozen surfaces, but they make up one program: the volume the
on-screen display shows is the one the bar's slider sets, and the workspace the
bar highlights is the one the launcher switches to. Quickshell's answer is one
QML engine with many windows, and that is the right shape. In Byard today, each
surface would have to be its own process, with its state synchronised over IPC,
which is exactly what a declarative UI was supposed to spare its author.

nyshell, a real shell of 278 QML files that this RFC uses as its test bench,
uses `PanelWindow` and the layer-shell attributes in 97 places, one surface per
screen through `Variants` in 20, and masks the input region by hand in 43.
Those three things are what this RFC provides.

### Apps want a second window too

A settings window, a detached inspector, a picture-in-picture player, a second
document. Byard apps cannot open any of them today. Designing several surfaces
only for layer shell would mean designing them twice, so this RFC designs them
once and lets the two kinds differ only in how the platform places them.

### What Byard brings that the alternatives do not

This is where the work pays for itself. Quickshell, AGS, EWW and the others give
a shell author none of these:
- A bar tested in CI with no compositor (`byard shot`, `byard test`).
- A checker that knows the attributes and their types.
- Zero frames while nothing changes, built in.
- A binary without Qt or GTK.

Each of those depends on the design here: the headless screen comes from §9, and
the cost of a still bar from §5.

## Guide-level explanation

### A second window

```byld
View Main() {
    var settings = false

    Window #[title: "Notes", width: 720, height: 480] {
        Column #[p: 16, gap: 12] {
            Text("Your notes")
            Button("Settings") => settings = true
        }
    }

    when settings {
        Window #[title: "Settings", width: 360, height: 420, close => settings = false] {
            SettingsPage()
        }
    }
}
```

A `Window` is a window. It is open while its `when` is true and closes when it
becomes false. The close button runs `close =>`. Without a `close =>`, the
close button ends the program, as the one window does today.

`window.width` inside a window is that window's size, so `when window.width >=
md` works per window.

A program with no `Window` is a single window, as now: the entry view is its
content, and `[project]`'s title and size are its own. A program that writes
`Window` says where each window is, and then everything at the top of the entry
view has to be a `Window` or a `Panel`: a `Text` loose between two windows
would have no window to be drawn in (`ContentOutsideSurface`).

### The smallest bar

```byld
View Main() {
    Panel #[edge: top] {
        Text("Hello from the bar") #[p: 8]
    }
}
```

That is a whole program. `byard dev` runs it on the desktop with hot reload:
the bar sits at the top of the screen the compositor picks, as tall as its
text, and windows make room for it. Every attribute below is something to add
when the default stops being enough. None of them is needed to start.

### A bar on every screen

```byld
View Main() {
    for s in screens key s.name {
        Panel #[screen: s, edge: top, height: 40, namespace: "bar"] {
            Row #[px: 12, gap: 8, align: center, bg: 0xE01C1B22, radius: 12, mx: 8, my: 4] {
                Text(s.name)
                Spacer
                Clock()
            }
        }
    }
}
```

`edge: top` puts the panel against the top of the screen, across its width.
`height: 40` is its thickness, and without it the panel takes the height of
its content. A panel on an `edge` reserves its thickness, so maximised windows
start below the bar (`exclusive: false` lets them go under it). `namespace` is
the name the compositor's rules match (Hyprland's `layerrule = blur, bar`).

A panel's background is transparent: only what the program draws is drawn. The
pointer goes through to whatever is below wherever the program put nothing that
takes it. A bar drawn as a floating island with rounded corners therefore
behaves like one, with no mask to write.

When a screen is connected, `screens` grows and the `for` gives it its panel;
when one is unplugged, its panel goes. `key s.name` is what keeps the other
bars as they are: without it, a `for` rebuilds every item when its list
changes, and every bar would close and reopen.

### Where a panel goes

| Write | It is |
|---|---|
| `edge: top` (`bottom`, `left`, `right`) | Against that edge, across all of it |
| `anchor: (top, right)` | In that corner, as large as its content or its `width`/`height` |
| `anchor: bottom` | Centred along the bottom edge |
| neither | Centred on the screen: a launcher, a dialog |

`m`, `mt`, `mr`, `mb` and `ml` move it off the edges it is anchored to.
`layer` is `background`, `bottom`, `top` (the default) or `overlay`.
`keyboard` is `none` (the default), `on_demand` (it takes the keys when clicked)
or `exclusive` (it takes every key while it is shown, as a launcher or a lock
prompt does).

### Popups from a panel

```byld
Panel #[edge: top, height: 40] {
    Row #[px: 12] {
        Button("Notifications") as bell => open = !open
    }
    when open {
        Overlay #[modal: false] {
            Box #[anchor_to: bell, anchor_edge: below, anchor_gap: 8, dismiss => open = false] {
                NotificationCenter()
            }
        }
    }
}
```

The overlay is written the way it is in any app (RFC-0066). In a panel, an
`Overlay` whose child is anchored to an element becomes a popup: a surface of
its own, placed by the compositor below the bell, flipped above if there is no
room, and allowed to be larger than the bar. `dismiss` runs when the user
clicks anywhere outside it, on any surface or on another app's window. A
popup that is a menu or a dialog takes the keyboard; a tooltip does not.

### On a screen that is not there

```sh
byard shot shell/ --screens "DP-1:2560x1440@1.5, HDMI-A-1:1920x1080" -o screens.png
byard shot shell/ --surface bar --screen DP-1 -o bar.png
byard shot shell/ --screens "DP-1:1920x1080" --tap 1880,20 -o popup.png
```

With no compositor, `shot` lays the panels out on the simulated screens by
layer shell's rules: anchors, margins and exclusive zones in the order
the compositor applies them, and popups positioned and flipped as it would. It
draws them over a checkerboard where nothing is. `--surface` picks one surface,
by its `as` name or its `namespace`, and `--screen` picks which screen's
instance. `byard test` sees every surface: the accessibility dump has a `Window`
or a `Panel` node at the top of each surface, and a step that taps `Button
"Settings"` finds the button on whichever surface it is drawn on.

## Reference-level explanation

### 1. The surface level

**Where a surface may be written.** `Window` and `Panel` are elements that may
only appear at the *surface level*:
- the entry view's body;
- the bodies of `when`, `else when`, `else` and `for` blocks at the surface
  level;
- the root element of a view called at the surface level, and, recursively, the
  surface level of that view's body when its root is itself a `when` or `for`.

A `Window` or `Panel` anywhere else is `MisplacedSurface`, at the element, with
the hint "a window or a panel goes at the top of the program; to show a panel
from inside one, use an `Overlay`". The checker enforces this, since the rule is
structural and every view it involves is visible at check time.

**A program is single- or multi-surface, statically.** If the program's surface
level contains no `Window` and no `Panel` anywhere, it is *single-surface*: the
entry view's content is the content of the one implicit window, exactly as
today. Nothing in this RFC's runtime is constructed for it (§5). Otherwise it is
*multi-surface*, and every element at the surface level must be a `Window`, a
`Panel`, or a `when`/`for`/view call that leads to them. Any other element is
`ContentOutsideSurface`, at the element. A `var`, a `provide`, an `inject`, a
`const` and a `fn` are not elements and stay where they are: state declared in
the entry view is shared by every surface, which is the point of having one
program.

**Shared state.** All surfaces belong to one interpreter. A `var` written by a
handler on one surface is read by an element on another in the same tick, by
the same reactive graph. Contexts (RFC-0065) provided above a surface reach
inside it. Nothing about state changes.

### 2. `Window`

| Attribute | Type | Default | Meaning |
|---|---|---|---|
| `title` | `Str` | the title the one window has now | The title bar and the task list |
| `width`, `height` | `Float` | the size the one window opens at now (1280 × 720) | The size it opens at, in logical pixels |
| `min_width`, `min_height`, `max_width`, `max_height` | `Float` | none | The bounds a user can resize it within |
| `resizable` | `Bool` | `true` | |
| `decorations` | `Bool` | `true` | The platform's title bar and borders |
| `transparent` | `Bool` | `false` | Undrawn pixels show what is behind |
| `close =>` | action | ends the program | The close button, or the platform's close |

`title`, `min_*`, `max_*`, `resizable` and `decorations` are reactive: changing
one updates the open window. `width` and `height` are only the opening size,
because after that the size is the user's. Writing them later does not resize
the window, and the checker says so if they read a reactive value
(`OpeningSizeIsStatic`, a warning). The content of a `Window` is laid out as the
implicit window's content is today. Its own background is the theme's `surface`,
as now.

**Closing.** The platform's close request (the button, Alt+F4, Cmd+W) runs the
window's `close =>` if it has one, which typically sets the `when` false.
Without one, the program ends, as a single-window app does today. A window
whose `when` becomes false is destroyed after its `on leave` motions settle,
as an element's are (RFC-0066). The program also ends when its last `Window`
closes and it has no `Panel`. A program with panels runs until it is stopped,
because a shell has no last window.

**Identity.** A surface's identity is its element's position in the program
plus, under a keyed `for` (§4b), the item's key. A keyed `for` that reorders
its items moves no window, and a hot reload that keeps the element keeps the
window open, at its position and size, with its state.

### 3. `Panel`

`Panel` is a `zwlr_layer_surface_v1` (wlr-layer-shell, version 4 when the
compositor offers it, else 3).

| Attribute | Type | Default | Protocol |
|---|---|---|---|
| `screen` | `Screen` | the compositor's choice | the `wl_output` the surface is created on |
| `edge` | `top \| bottom \| left \| right` | none | anchor that edge and both perpendicular ones |
| `anchor` | edge or tuple of edges | none | `set_anchor` |
| `layer` | `background \| bottom \| top \| overlay` | `top` | the layer at creation, `set_layer` after |
| `exclusive` | `Bool`, `Float` or `ignore` | `true` with `edge`, else `false` | `set_exclusive_zone` |
| `keyboard` | `none \| on_demand \| exclusive` | `none` | `set_keyboard_interactivity` |
| `namespace` | `Str` | the program's app id | the namespace at creation |
| `width`, `height` | `Float` | the content's size | `set_size` |
| `m`, `mt`, `mr`, `mb`, `ml`, `mx`, `my` | `Float` | 0 | `set_margin` |
| `input` | `auto \| all \| none` | `auto` | the input region (§6) |

**Placement.** `edge: top` is `anchor: (top, left, right)`, and likewise for
the other three. `edge` and `anchor` together are `InvalidAttributeCombination`.
Screen edges are physical, so `left` and `right` are written, not `start` and
`end`: a bar on the left of the screen stays there in a right-to-left language.

**Size.** Along an axis anchored at both ends the size sent is 0, so the
compositor stretches the panel. Along any other axis the size is `width` or
`height` if written, else the content's measured size, laid out with that axis
unbounded. When the content's measure changes, the panel sends the new size and
commits, and draws at the size the compositor configures. A `height` that reads
a reactive value resizes the panel, which, unlike a window, is the program's to
size.

**`exclusive`.** `true` reserves the panel's thickness along its edge: its
height for `top` and `bottom`, its width for `left` and `right`, plus the
margin on that edge. A number reserves that many logical pixels. `false` sends
0, so the panel respects the space others reserved. `ignore` sends -1, so it
covers them too, as a wallpaper or a full-screen overlay should. The default
is `true` for a panel on an `edge`, because a bar that windows slide under is
the surprise nobody wants on the first run, and `false` for anything else.
`exclusive: true` on a panel with no `edge` and fewer than three anchors is
`ExclusiveNeedsEdge`, because the protocol ignores it there.

**`keyboard`.** With `none`, the panel never has keyboard focus. Its text
fields cannot be typed in, so the checker warns (`PanelTakesNoKeys`) at a
`TextField` or `key_down` handler inside a `keyboard: none` panel, with the hint
to write `keyboard: on_demand`. `exclusive` is what a launcher wants: the keys go
to it from the moment it appears until it goes. `on_demand` needs layer shell version 4. A compositor at
version 3 gets `exclusive` while the panel has a focused element and `none`
otherwise, which behaves the same from the user's side.

**`screen`.** A `Screen` record from `screens`. Panels are created on that
output and destroyed if it goes away, before the `for` that made them sees the
shorter list (the compositor sends `closed`). The interpreter keeps the element
until the reconcile, but it no longer has a surface.

**`layer`, `exclusive`, `keyboard`, `anchor`, `edge`, margins and size are
reactive**, sent with the next commit. `namespace` and `screen` are fixed when
the surface is created, so the protocol cannot change them: a change destroys
and recreates the surface, which is what Quickshell does too. The checker warns
if they read a reactive value (`RecreatesSurface`), since that is rarely meant.

**Transparency.** Every panel is created with a premultiplied alpha surface
(`CompositeAlphaMode::PreMultiplied`, which RFC-0060's `byard_out_premul` already
writes). There is no implicit background. A panel's undrawn pixels are fully
transparent.

**Not supported.** On a platform with no wlr-layer-shell (Windows, macOS, X11,
GNOME's Mutter), a multi-surface program with a `Panel` stops at startup with
`PanelsUnsupported`. The message names the platform or the missing protocol
("this compositor does not offer zwlr_layer_shell_v1"). Panels do not fall back
to windows: a bar that opens as a normal window is broken in a confusing way,
and a clear stop is better.

### 4. `screens`

`screens` is a built-in reactive list of `Screen` records:

```text
Screen(name: Str, description: Str, x: Float, y: Float, width: Float, height: Float, scale: Float)
```

`name` is the connector (`DP-1`), from `wl_output.name` (version 4), else from
`xdg_output`, else `"screen-N"` in connection order. `x`, `y`, `width` and
`height` are the logical geometry, from `xdg_output` where offered. `scale` is
the fractional scale where `wp_fractional_scale_v1` is offered, else the
integer `wl_output.scale`. The list is ordered by `x`, then `y`, so it is stable
across restarts and matches how the screens sit on the desk.

It is implemented like RFC-0067's `window`: no signal exists until a program
lowers a read of `screens`, and then one signal holds the list. The host
writes it when an output is added, removed or changes. With `for s in screens
key s.name`, a re-scaled screen keeps its panels and their state, and its `s`
reads the new scale. Under winit, it is `available_monitors()`, with `name` from the
monitor's name and the position and scale it reports. A program that never
writes `screens` pays nothing.

`window` (RFC-0067) is per surface. A `window.width` read inside a `Window` or
`Panel` is bound, when lowered, to that surface's size pair, so the same view
used in two panels reads each panel's own width. In a single-surface program
nothing changes.

### 4b. `for … key`

Today a `for` whose list changes unmounts every item and mounts the new list
(RFC-0004 deferred keyed reconciliation). For elements that is a cost; for a
surface it is a window closing and reopening. So:

```byld
for s in screens key s.name { … }
for i, t in tabs key t.id { … }
```

`key` is read after the list expression, as a contextual word, so a binding
called `key` still works everywhere else. It takes an expression over the loop variable, of any type with equality
(`Str`, `Int`, `Bool`, an enum, a record of those). When the list changes:
- an item whose key was in the old list keeps its mount: its `var`s, its
  memos, its animations, its surface. Its loop variable (and index) are
  rebound to the new item, and what reads them updates as any read does;
- an item with a new key is mounted; an old key that is gone is unmounted,
  through `on leave` as any element;
- the mounts are put in the new list's order.

Two items with the same key are `DuplicateForKey`, a runtime error reported
like a failed reply, and the second one is not drawn, since there is no way to tell
which one is which. In a keyed `for` the loop variable and the index are
signals, one per mount. That costs one signal per item, which only a keyed
`for` pays. A `for` without `key` behaves exactly as it does now.

The checker warns `SurfaceForNeedsKey` at a `for` without `key` whose body
makes a `Window` or a `Panel`: every change to its list would close and reopen
every one of them.

### 5. The runtime

**One program, one logic thread.** The interpreter, its reactive graph, its
capabilities, timers and async pool stay single, on the one logic thread they
run on now. What becomes plural is everything that belongs to a surface:

| Per program | Per surface |
|---|---|
| Interpreter, reactive graph, `var`s, contexts | Layout tree (`LayoutAtlas`) |
| wgpu `Instance`, `Adapter`, `Device`, `Queue` | `wgpu::Surface`, its configuration |
| Pipelines, shader modules, samplers | The persistent target of incremental redraw (RFC-0032) |
| Glyph atlas, image cache, MSDF atlas | Its `RenderFrame` slot in the relay, its frame version |
| Focus owner, focus scopes (RFC-0066) | Hit-test data, pointer capture, hover |
| Controllers, the async pool | Its AccessKit tree (one window node in the platform's tree) |

`Engine` is split into a shared `Gpu` and a `SurfaceTarget` per surface. Today's
single window is one `SurfaceTarget`, created exactly as it is now. The glyph
atlas's `trim()` (the swash workaround's per-frame call) runs once per *round*,
after every surface that drew in it, never between two surfaces of the same round,
so a glyph one surface just uploaded is not evicted before the next surface uses it.

**Painting.** `Interpreter::render` keeps its signature for the single-surface
case. Multi-surface programs go through `render_surfaces`, which paints each
surface's subtree into that surface's `RenderFrame`, at that surface's viewport.
**A surface none of whose elements changed, and whose viewport did not change,
is not painted, not published, and not presented.** Each surface's layout tree
already knows its dirty targets; a surface with none, no motion in flight and
no pending reconcile is skipped. A clock ticking in one bar repaints that bar
only. The test for this counts frames per surface (§10).

**Surfaces come and go from the logic thread.** When a reconcile creates a
`Window` or `Panel`, the logic thread does not create the platform surface,
because only the main thread may. It appends a `SurfaceChange::Create { id, spec }`
to the relay. A `Destroy { id }` and a `Configure { id, spec }` (reactive
attributes) are appended the same way, and the frame waker wakes the main
thread to apply them in order. The surface draws its first frame when the
platform has given it a size: on Wayland, the first `configure`. Until then its
frames are kept, not dropped, so it opens with its content and never empty.

**Frame pacing.** Each Wayland surface draws only on its own `wl_surface.frame`
callback, and only if it has a newer frame than the one it presented. A panel
on a screen that is off, or a window that is fully covered, is throttled by the
compositor and costs nothing. Winit windows use `request_redraw` per window, as
the single window does now.

**Wait mode is unchanged.** With nothing in flight, the logic thread sleeps,
no surface is committed, and `WAYLAND_DEBUG=1` shows no traffic. That is the
acceptance test for a still bar.

### 6. Input

**Events carry their surface.** `InputEvent` gains `surface: SurfaceId`.
Positional events (pointer, wheel, touch) are routed with that surface's hit-test
data, in its coordinates. A pointer press captures to its surface until release,
as it captures to an element now.

**Keyboard focus is one per program.** The focused element can be on any
surface. When the platform gives keyboard focus to a surface (`wl_keyboard.enter`,
winit's `Focused(true)`), focus goes to that surface's last focused element, or
nowhere. Tab and Shift+Tab move within the focused surface and never cross to
another, the way two apps' windows behave. Focus scopes (RFC-0066) work inside
a surface as they do now. A popup that is a scope takes focus from its panel and
gives it back.

**The input region (`Panel` only).** With `input: auto`, a panel's input region
is the union of the bounds, in surface coordinates, of:
- every element that takes the pointer: one with a `tap`, `long_press`,
  pointer or drag handler, a `hover` state block, a role that takes a press, a
  `ScrollView`, a `TextField`, a `Slider`;
- every modal overlay's scrim.
`interactive: false` (RFC-0066) removes an element and its subtree. Everything
else lets the pointer through to what is below. The region is recomputed when a
frame's interactive bounds change, a comparison of rectangles the hit-test data
already has, and sent with `wl_surface.set_input_region` only when it differs.
`input: all` takes every press on the panel, and `input: none` takes none (an
on-screen display). A `Window` always takes input over its whole area.

### 7. Popups

In a `Panel`, an `Overlay` whose first child has `anchor_to` is a popup:

- **Surface.** An `xdg_popup` whose parent is the panel's layer surface
  (`zwlr_layer_surface_v1.get_popup`). The popup's content is the overlay's
  content, laid out unbounded, at its measured size.
- **Position.** An `xdg_positioner` built from the RFC-0066 attributes: the
  anchor rectangle is `anchor_to`'s element's bounds in the panel. `anchor_edge`
  maps to the positioner's anchor and gravity (`below` is anchor `bottom`, gravity
  `bottom`). `anchor_align` maps to the other axis. `anchor_gap` is the offset.
  RFC-0066's flip when there is no room becomes `constraint_adjustment` `flip_y`
  (or `flip_x` for `before`/`after`) plus `slide`, so the compositor, which knows
  the screen, does the flipping.
- **Grab.** A popup that is a focus scope (it holds a `menu` or a `dialog`, or is
  modal) is created with `xdg_popup.grab`, so it takes the keyboard and the
  compositor sends `popup_done` on a click outside it. `popup_done` runs
  `dismiss`, as a press outside does today. A popup that is not a scope (a
  tooltip) has no grab and never takes the keyboard.
- **Nesting.** A popup's own anchored overlay (a submenu) is a popup whose parent
  is that popup, so submenus open and close as RFC-0066 says.
- **Motion.** `on enter` and `on leave` play in the popup's surface. A leaving
  popup is destroyed when its motion settles.

An `Overlay` in a panel without `anchor_to` is drawn inside the panel, as an
overlay is in a window today. In a `Window`, anchored overlays stay inside the
window, as now. Making them popups too would let a menu leave its window, but it
would change every existing app's behaviour, so it waits for its own RFC.

### 8. The backends

**winit, as now.** A program with no `Panel` runs on winit on every platform.
A single-surface program runs exactly the code path it runs today. A
multi-surface one opens a winit window per `Window` from the same event loop.
winit 0.30 supports several windows, and each gets its own `accesskit_winit`
adapter.

**The Wayland shell backend.** A program with a `Panel` anywhere in its surface
level runs on a new backend in `byard-platform`, behind the `wayland-shell`
feature (on by default on Linux, absent elsewhere). It is built on
smithay-client-toolkit, the same 0.19 that winit already links for its own
Wayland support, with `wayland-protocols-wlr` for layer shell. Both are already
in every Linux build's dependency tree, so the backend adds its own code and no
new dependency. It handles `Window` as `xdg_toplevel` too, so a shell program's
settings window opens in the same connection as its panels. The wgpu surface
comes from the `wl_surface` and `wl_display` raw handles, as SCTK's own wgpu
example does.

The choice is made at startup, from the program: the checker records whether
the surface level contains a `Panel`, and `App::run`, `byard dev` and a built
app pick the backend from that. A hot reload that adds the first `Panel` to a running
winit program says it needs a restart and keeps the running program, the same
way an edit that does not compile keeps it.

**Accessibility on the shell backend.** `accesskit_unix` directly, one
`Adapter` per surface, each a window under the application's AT-SPI root, which
is what `accesskit_winit` does on Linux underneath.

### 8b. Going below: the Wayland API

The attributes cover what most shells need. For everything else, a program can
reach the Wayland connection, the compositor's globals and every surface
Byard made, from Rust, on the shell backend. The rule is that nothing a
person writes in Byld depends on this. It is a door for the one panel that needs
`ext-background-effect`, a KDE blur, `set_exclusive_edge` or a protocol Byard
has never heard of, and nobody prototyping a bar has to know it exists.

**The crates, at Byard's versions.** `byard::wayland` re-exports
`smithay_client_toolkit` (as `sctk`), `wayland_client`, `wayland_protocols`,
`wayland_protocols_wlr` and `calloop`, so an extension compiles against exactly the
objects Byard holds and never against a second copy of them.

**An extension.**

```rust,ignore
use byard::wayland::{SurfaceHandle, SurfaceKind, WaylandContext, WaylandExtension};

struct Blur { manager: Option<OrgKdeKwinBlurManager>, queue: Option<QueueHandle<BlurState>> }

impl WaylandExtension for Blur {
    fn connected(&mut self, cx: &mut WaylandContext<'_>) {
        // Every global the compositor advertises, with its version.
        // An extension binds what it wants on its own event queue, with its own
        // `Dispatch` impls, inserted into Byard's event loop.
        let (globals, queue) = cx.own_queue::<BlurState>();
        self.manager = globals.bind(&queue.handle(), 1..=1, ()).ok();
        self.queue = Some(queue.handle());
        cx.insert_queue(queue, BlurState::default());
    }

    fn surface_configured(&mut self, cx: &mut WaylandContext<'_>, surface: &SurfaceHandle<'_>) {
        if surface.name() == Some("bar") {
            if let (Some(m), Some(q)) = (&self.manager, &self.queue) {
                let blur = m.create(surface.wl_surface(), q, ());
                blur.commit();
                cx.request_commit(surface.id());
            }
        }
    }
}

byard::App::new("shell/main.byd").wayland(Blur { manager: None, queue: None }).run()
```

- **`WaylandContext`** gives the `Connection`, the `GlobalList` (all globals
  and their versions, not only the ones Byard uses), the calloop `LoopHandle`,
  `own_queue()` (a new `EventQueue` on Byard's connection, with the globals
  listed on it) and `insert_queue()` (dispatched on the main thread with
  Byard's own). Two queues on one connection is how `wayland-client` keeps one
  library's `Dispatch` state from another's, so an extension never touches
  Byard's state type, and Byard never needs to know the extension's protocols.
- **`SurfaceHandle`** is one of Byard's surfaces: `id()`, `name()` (the `as`
  name, else the `namespace`, else the title), `screen()`, `size()`, `scale()`,
  `wl_surface()`, and `kind()`, which is `SurfaceKind::Layer(&sctk::shell::wlr_layer::LayerSurface)`,
  `Window(&sctk::shell::xdg::window::Window)` or `Popup(&sctk::shell::xdg::popup::Popup)`.
  Anything those types offer can be called on them.
- **The hooks** are `connected`, `surface_created` (before the first commit,
  for state that must be set before it, like a `wp_fractional_scale` or an
  alpha modifier), `surface_configured`, `before_commit` (each frame that is about
  to be committed, only for surfaces that draw one), `surface_destroyed`,
  and `screens_changed`. All of them run on the main thread, between frames, and
  have empty default bodies.
- **Who owns what.** Byard owns the roles it created (`xdg_toplevel`, the layer
  surface, `xdg_popup`) and the commits that carry its buffers. An extension may
  call requests on them and attach its own objects to a `wl_surface`, and asks for
  a commit with `request_commit` if its change has no frame behind it. It must not
  destroy Byard's objects or attach buffers to its surfaces; the docs say so, and
  a debug build checks the second with an assertion in `before_commit`.
- **Talking to Byld.** An extension that is also a controller (`App::provide`)
  can be called from a view, and shares its state with the extension half through
  an `Arc`. Values that arrive on their own, a compositor's event, reach a view
  through a controller method that resolves on the next event, which the view
  calls again from its `ok` arm. A proper stream from Rust into a signal is the
  first thing Mantle's RFC needs, and it will propose one. This RFC does not
  invent it on the side.
- **On winit** there is no Wayland connection to share, and `App::wayland` is a
  startup error that says the program has no `Panel` and so runs on winit. Raw
  window handles for winit windows stay out of this RFC, as they are today.

The cost is zero for a program that registers no extension: the module is types
and re-exports, and the hooks are a `Vec` that is empty.

### 9. Headless

`byard shot` and `byard test` gain a simulated screen set and draw every surface.

- **`--screens "NAME:WxH[@SCALE][+X+Y], …"`** describes the screens. The default
  is one `screen-1` of `--size` (or 1280 × 800) at scale 1, so a program with one
  `Window` shoots as it does now.
- **Placement.** Panels are placed on their screen by layer shell's rules, as
  wlroots implements them, with the same arithmetic as `wlr_layer_shell`'s
  arrange pass (sway's `arrange_layers`): layers from `overlay` down to
  `background`, every panel that reserves space before any that does not,
  creation order within that, each reservation shrinking the area left for the
  next. A panel that reserves nothing therefore avoids a bar created after it,
  as it does on the desktop. Windows are placed in the area left,
  centred at their opening size. Popups are positioned by an `xdg_positioner`
  solver with the same flip and slide rules.
- **The image.** Without `--surface`, the image is the screens laid out side by
  side at their positions, with every surface composited in layer order
  (`background`, `bottom`, windows, `top`, `overlay`, popups) over a checkerboard,
  so transparency shows. `--surface NAME` (an `as` name, else a `namespace`, else a
  window title) is one surface alone, at its own size. `--screen` disambiguates a
  surface made per screen.
- **Input.** `--tap X,Y`, `--hover`, `--drag` and the rest take screen
  coordinates and are delivered to the topmost surface whose input region
  contains the point, so a tap on a panel's transparent part reaches the window
  below it, as it would on the desktop. `--key` and `--type` go to the focused
  surface.
- **The accessibility dump** starts each surface with a `Window "title"` or
  `Panel "namespace" @ DP-1` line, its tree indented below. Test steps that name
  an element find it on any surface, and `expect`/`not` match across them all.

The placement solver and the positioner solver are plain functions with their
own tests against the protocol's documented examples. They are the only part of
the shell backend that runs in CI on every platform.

### 10. Cost

- **A single-surface program** constructs nothing new: no `SurfaceId` map, no
  `screens` signal, the same `Engine` built through `Gpu` and one `SurfaceTarget`,
  and the same frame path. Its binary grows by the new code only. Budgets CI
  measures it, and the ceiling is not raised for this RFC.
- **A still multi-surface program** commits nothing and wakes no thread. The
  test runs a two-panel program headless for 5 seconds of virtual time with
  nothing changing and asserts zero frames painted after the first, per surface.
- **A changing surface** costs only itself. The same test changes a `var` read by
  one panel only and asserts the other painted nothing.
- **GPU memory** per surface is its swapchain and its incremental-redraw target.
  Pipelines and atlases are shared, so a second bar does not load a second copy of
  the glyph atlas.

## Drawbacks

- **The engine was built around one window.** `Engine`, the relay, the router
  and the accessibility host each assume it. Splitting them touches a great deal
  of the runtime. The split is mechanical (one becomes a map keyed by surface),
  but it is the most expensive engine change since the relay.
- **A second platform backend** is code to keep working: Wayland protocol
  handling, frame callbacks, popups. Its scope is limited to what winit cannot
  do, but it is real maintenance.
- **Layer shell is not universal.** GNOME does not offer it, so Byard shells will
  not run there. That is the state of every Wayland shell toolkit, and the error
  is explicit.
- **Two ways an `Overlay` behaves** (popup in a panel, in-surface in a window).
  The rule is one sentence, but it is a rule to learn.

## Rationale and alternatives

- **One process per surface, synced over IPC.** No engine change, and every
  shell author pays for it in every feature. Rejected: shared state is the
  reason to write a shell in a declarative language at all.
- **Surfaces as a Rust API (`App::window(...)`) instead of elements.** Windows
  would then not be reactive, `for` could not make one per screen, and `shot`
  could not see them. Rejected: a surface's existence is state, and state
  belongs in Byld.
- **A separate layer-shell crate on top of winit (a fork, or
  `winit`+`smithay` glue).** The forks that add layer shell to winit are
  unmaintained, and mixing two Wayland connections in one process is not
  possible. SCTK is what winit itself uses underneath.
- **Only layer surfaces (shell-only multi-surface).** Smaller now, and the
  second window for apps would need the same split later, with a second design.
  The decision (2026-11-15) was one design for both.
- **Popups decided by overflow** (an overlay becomes a popup only if it does not
  fit its surface). It would flip a surface's kind at runtime, as content
  changes. Rejected for the static rule in §7.
- **Panels falling back to windows where layer shell is missing.** Rejected in
  §3: a bar that opens as a normal window is broken in a confusing way.

## Prior art

- **Quickshell:** `PanelWindow`, `Variants` over `Quickshell.screens`,
  `PopupWindow`, `mask: Region`. The model this RFC follows: one engine, many
  surfaces. Byard derives the mask instead of asking for it.
- **gtk-layer-shell / gtk4-layer-shell:** the layer-shell attributes as window
  properties. It is where `keyboard`'s three modes and `exclusive: ignore` come
  from.
- **layer-shika (Slint):** layer surfaces and session lock for Slint, on SCTK.
- **iced's `iced_layershell`, rbshell:** iced on layer shell, as a separate
  backend chosen at startup, as here.
- **EWW, AGS/Astal:** windows declared in config (`defwindow`) with geometry and
  `exclusive`, the declarative precedent. EWW's re-creation of widgets is what
  §5's per-surface skip avoids.
- **Flutter's multi-view embedding** and **Electron's BrowserWindow:** one
  engine, several native surfaces, shared state, with per-view rendering
  targets. Same split as §5's table.

## Resolved questions

### Q1, what the two elements are called

`Window` and `Panel`. `Window` because it is one, and because `window`
(RFC-0067) is already the name of the record of its size, and a `window.width`
inside a `Window` reads that window. `Panel` because that is what a bar, a dock
or a launcher is called on every desktop, and because Quickshell's `PanelWindow`
made the word familiar to shell authors. `LayerSurface` is the protocol's name,
and a page of Byld should not read like a protocol. `Surface` was rejected as
too vague to say which kind.

### Q2, may a surface be written anywhere

No: only at the surface level (§1). A `Window` written inside a `Row` would
leave the reader wondering what the row lays out. The checker can verify the
rule entirely, and the error points at the `Overlay` for the case people mean.

### Q3, what happens to existing programs

Nothing. A program with no surface elements is single-surface, statically, and
runs the code it runs now. The cost section's budget check makes "nothing" a
measured claim.

### Q4, can a multi-surface program mix loose content and surfaces

No (`ContentOutsideSurface`). Loose content would have to be given an implicit
window, which makes the program's shape depend on whether some unrelated
`when` is true. A program that writes one `Window` writes them all.

### Q5, which backend runs a program, and can it change

It is chosen at startup, from whether the program has a `Panel`. Switching
backends at runtime would mean moving every surface to another connection, and
the case (adding the first panel during a hot reload) only comes up while a
program is being written. A reload that needs it says so and keeps running.

### Q6, why `keyboard: none` is the default

A bar that takes keyboard focus when clicked steals it from the window the user
was typing in, which is the most common complaint about badly configured
panels. Panels that need keys say so, and the checker points at the ones that
forgot.

### Q7, why the input region is derived

nyshell writes 43 masks by hand, and each one is a bug waiting to happen when
the content moves. The elements that take the pointer are already known to the
hit tester. `input: all` and `input: none` cover the rest.

### Q8, can Tab move between surfaces

No. Two surfaces are two windows to the platform, and the compositor decides
which has the keyboard. A panel that wants Tab to reach a popup writes the popup
as a focus scope, which takes focus when it opens (RFC-0066).

### Q9, how a popup is chosen

Statically, as written: an anchored `Overlay` in a `Panel` (§7). Deciding by
overflow was rejected in the alternatives.

### Q10, why panels have no fallback

See §3 and the alternatives. A panel that silently becomes a window would be
broken in ways that are harder to understand than a startup message.

### Q11, what ends a program

Its last `Window` closing when it has no `Panel`, a `close =>`-less window's
close button, or being stopped. A shell (any `Panel`) runs until stopped.

### Q12, what a hot reload does to open surfaces

It keeps every surface whose element it keeps (§2's identity), at its size,
position and state, and creates or destroys the others. Reactive attributes that
changed are sent as a `Configure`. A changed `namespace` or `screen`
recreates its panel, as the protocol requires.

### Q13, whether `screens` should exist under winit

Yes. A presentation app wants a window on the second screen, and a program
written against `screens` should check and run the same on both backends.
winit's monitor list provides it.

### Q14, why `key` is written and not inferred

Matching old and new items by equality would need no syntax, but a record whose
one field changed (a screen's scale, a task's title) would no longer be equal
and would be rebuilt, losing exactly the state the key was for. Matching by an
`id` field when there is one would make a field's name change behaviour. The
key is the program's to say, as in React, SwiftUI's `ForEach(id:)` and
Svelte's keyed `each`. It is an addition to `for`, not a requirement: lists of
plain elements keep working unkeyed, and only surfaces get the warning, since for them
an unkeyed rebuild is visible.

### Q15, how much of Wayland is exposed

All of it, below a floor the simple path never sees (§8b). Byld gets the attributes
nearly every shell uses, with defaults that make a three-line bar work, and
Rust gets the connection, every global, Byard's own surfaces and their SCTK
types. Exposing SCTK's own types is a promise to follow its versions, and Byard
already follows them through winit. The alternative, a curated Rust API of
Byard's own, would always lag behind the protocols people actually want,
and the point of the door is that it does not need Byard to have thought of the
case first.

### Q16, whether `shot` should simulate the compositor

Yes, and this is what makes the RFC worth doing in Byard instead of another
toolkit. A bar's layout depends on its anchors, margins and the exclusive zones
of the panels before it. A test that shot the panel alone, at a size the test
picked, would test a layout the desktop never produces. The arithmetic is
small, documented by the protocol, and testable on its own.

## Not in this RFC

- **Session lock** (`ext-session-lock-v1`) and its PAM prompt. A bug there is a
  security bug, so it gets its own RFC and its own review.
- **Blur behind a panel** (`ext-background-effect-v1`). Compositors without it
  blur by namespace (`layerrule = blur, bar`), which `namespace` already serves.
- **User shaders** for effects like nyshell's metaballs. They get a separate RFC
  (decided 2026-11-15).
- **Foreign toplevels, screencopy, idle, data control, shortcuts.** These are
  services, not surfaces, and they belong to Mantle's RFC. Screencopy's *view*
  (a live thumbnail) will need an engine hook, which that RFC will propose.
- **Popups from a `Window`**, and a menu that leaves its window. §7 explains why
  this waits.

## Implementation plan

Each step is a branch and a PR, and each ends in something testable.

1. **The split.** `Gpu` and `SurfaceTarget`, the relay's per-surface slots and
   `SurfaceChange` queue, `SurfaceId` on input, and the per-surface router and
   accessibility tree. No language change yet. Every existing test passes, and
   Budgets passes without raising its ceiling.
2. **`for … key` and `Window`.** Keyed `for` with its tests (state kept across
   insertions, removals and reorders). Then the element, the surface level and
   its checks, `close =>`, the per-surface `window`, `screens` under winit, and
   multi-window on winit. Tests:
   two windows sharing a `var`, closing one, the per-surface paint count, and
   headless shots of each window.
3. **The shell backend and `Panel`.** SCTK, layer surfaces, the attributes,
   `screens` from `wl_output`, the input region, transparency, `PanelsUnsupported`.
   Checked by hand on Hyprland: a bar on two screens, hotplug, the exclusive
   zone, and `WAYLAND_DEBUG=1` silent while still.
4. **Popups and the Wayland API.** The positioner, grabs, nesting, `popup_done`.
   Then `byard::wayland` (§8b): the re-exports, `WaylandExtension`, the hooks,
   and an example extension in `examples/` that binds a protocol Byard does not
   use and attaches it to a panel.
5. **Headless screens.** The placement and positioner solvers, `--screens`,
   `--surface`, composited shots, cross-surface tests. A gallery page in
   `examples/` with a bar, a dock and a popup, shot in CI.
