# RFC-0050: Byard Material, the first official package

- **Status:** Draft
- **Author(s):** Briany4717
- **Created:** 2026-11-06
- **Last updated:** 2026-11-06
- **Depends on:** RFC-0008 (the package ecosystem: `byard get`, `byard
  publish`, registries), RFC-0022 (themes and assets in packages), RFC-0017
  and RFC-0036 (overlays), RFC-0020 and RFC-0031 (shapes and morphing),
  RFC-0025 (motion), RFC-0042 (the accessibility tree).

> **Placement:** `docs/rfcs/0050-material-package.md`

---

## Summary

`material` is a package of Material 3 Expressive components written in Byld:
buttons, button groups, switches, fields, chips, lists, navigation, app bars,
dialogs, snackbars, loading and progress indicators. It is the first package
Byard maintains itself. It lives in the repository as `packages/material`, is
versioned with Byard, and is checked by the same CI. An app adds it and writes:

```byld,ignore
use material as m

View Main() {
    var wifi = true
    var name = ""

    m.Scaffold(title: "Settings") {
        m.TextField(bind: name, label: "Name", supporting: "As it appears on your ID")
        m.ListItem(headline: "Wi-Fi", supporting: wifi ? "On" : "Off") {
            m.Switch(bind: wifi)
        }
        m.Button("Save", icon: "check", size: m.medium) => save()
    }
}
```

It is also a test. Stockroom proved Byard can carry an app. This proves it can
carry a design system that other people build apps from, which is a different
and harder thing: every component is a view someone else calls, with values,
bindings, slots, states, motion and accessibility that have to work from the
outside.

## Motivation

Every app in the repository draws its own buttons out of `Box`, `Text` and a
`style`. Stockroom has a `parts.byd` of cards, tiles, chips, buttons and a
dialog, and Aura Weather has its own. That is the right shape for a first app
and the wrong one for the hundredth. The package system already exists
(`byard add`, `byard get`, `byard publish`, themes with `extends`), and so far
nothing official uses it.

Material 3 Expressive fits what the engine already does well:

- the built-in theme is Material 3's roles, type scale and corner scale, and a
  seed colour derives a whole scheme (`seed.rs`);
- springs are the default curve, and Expressive's motion is springs;
- `ngon`, `morph` and `fuse` draw Expressive's shape library and its morphing
  loading indicator, which most toolkits can't;
- ripple, state layers (`on pressed`, `on hovered`, `on focused`), overlays
  with a scrim, and frosted glass exist.

## Guide-level explanation

### Adding it

```sh
byard add material --registry https://packages.byard.dev
```

or, inside this repository, `material = { path = "../../packages/material" }`.
`[theme] extends = "material"` takes its baseline theme: the full set of colour
roles, the emphasized type scale, the expressive shape scale and its fonts. A
`seed` on top recolours all of it.

### Components (first release)

| Group | Views |
|-------|-------|
| Actions | `Button` (filled, tonal, outlined, elevated, text; sizes xs to xl; round or square; morphs on press), `IconButton`, `ToggleButton`, `ButtonGroup` (standard and connected), `SplitButton`, `Fab`, `ExtendedFab` |
| Selection | `Switch`, `Checkbox`, `Radio`, `Slider`, `FilterChip`, `InputChip`, `AssistChip`, `SuggestionChip` |
| Input | `TextField` (filled and outlined, label, supporting text, error, leading and trailing icon, counter) |
| Containment | `Card` (elevated, filled, outlined), `ListItem` (one to three lines, leading and trailing slots), `Divider`, `Badge` |
| Navigation | `NavigationBar`, `NavigationRail`, `TopAppBar` (small, medium and large, collapsing with a ScrollView) |
| Communication | `Dialog`, `Snackbar`, `Tooltip`, `LoadingIndicator` (the morphing shapes), `LinearProgress` and `CircularProgress` (with the wavy track) |
| Layout | `Scaffold` (app bar, body, navigation, FAB, snackbar host) |
| Icons | `Icon("favorite", fill: 1.0)`, Material Symbols Rounded |

A second release adds the toolbars (docked and floating), the FAB menu, bottom
sheets, search, menus, tabs, the date and time pickers and the carousel.

Every component:

- takes its colours, type, shape and motion from the theme, never a literal;
- has a light and a dark picture in the gallery, and a test;
- is in the Tab order when it can be used, and is announced with its role,
  name, value and state;
- costs nothing until it is used: no timer, no animation and no layer while it
  is still.

### The gallery

`examples/material_gallery` shows every component in every variant, light and
dark, and is how the package is looked at: `byard shot` draws each page. Its
tests are the package's tests.

## Reference-level explanation

### What the engine needs first

Building three components as a spike (`Button`, `Switch`, `TextField`) against
main, the way Stockroom was built, will turn up the real list. What is already
known:

1. **Bindable view parameters.** A control inside a view can't write its
   caller's var: `bind:` on a parameter is `BindNotAVar` (#392). Without this,
   `m.Switch(bind: on)` has to be `m.Switch(value: on, change: {|v| on = v })`,
   for every control in the package. That is the one thing the package can't be
   good without. It needs an RFC of its own: a parameter declared as a
   binding (`View Switch(bind value: Bool)`), which a caller passes a var to
   and the view reads and writes.
2. **Roles and keyboard activation for composed controls.** A package's
   `Switch` is a `Box` with a `tap`, and today a Box has a label but no role,
   value or state in the accessibility tree, and Space doesn't press it. It
   needs `a11y_role` (`button`, `switch`, `checkbox`, `radio`, `slider`, `tab`,
   `link`), `a11y_value`, `a11y_checked`, and Space/Enter activating a
   focusable element's `tap`.
3. **The rest of the colour roles.** Material's scheme has roles the theme
   lacks: `surfaceContainerLowest`, `surfaceContainerHighest`, `surfaceDim`,
   `surfaceBright`, `inverseSurface`, `inverseOnSurface`, `inversePrimary`,
   `scrim`, `shadow`, and the fixed roles. `seed.rs` derives them from the
   same palettes.
4. **Expressive shape and type tokens.** The corner scale gains
   `cornerLgIncreased` (20), `cornerXlIncreased` (32) and `cornerXxl` (48). The
   type scale gains the emphasized styles (`displayLargeEmphasized`, …), which
   are the same sizes at a heavier weight.
5. **Motion tokens.** Expressive names six springs: spatial fast, default and
   slow (stiffness 800, 380 and 200; damping 0.6, 0.8 and 0.8) and effects
   fast, default and slow (stiffness 3800, 1600 and 800; damping 1.0). A theme should be able to
   name them, so a component says `with t.springSpatialFast` and an app can
   change them all at once.
6. **Icons.** Material Symbols is a variable font that turns names into icons
   with ligatures and has `FILL`, `wght`, `GRAD` and `opsz` axes. Text needs
   ligatures, which the shaper already does, and a way to set variation axes
   (`variations: (FILL: 1, wght: 500)`). The whole font is several megabytes.
   An app that uses six icons should not ship all of them, so `byard bundle`
   subsets an icon font to the names the app uses. Without subsetting, the
   alternative is one `VectorIcon` SVG per icon.

Each of these is its own branch, with its own RFC where it changes the
language (1, 5, 6), in the order above.

### Where it lives and how it ships

- `packages/material/` in this repository: `byard.toml` (`[package] name =
  "material"`), `src/*.byd`, `fonts/` (Material Symbols Rounded and a text
  face, both Apache-2.0 or OFL), README, licence.
- The version follows Byard's. `material 0.1.x` needs `byard 0.1.x`.
- It is published to Byard's own registry, the static layout `byard publish`
  already writes, served from GitHub Pages at a domain Byard owns. Until that
  exists, a git dependency with a tag works the same.
- CI runs `byard check` on the package and the gallery's tests on every PR, so
  an engine change that breaks a component fails where it is made.

### Naming

Views are named as Material names them, without a prefix: `m.Button`, not
`m.MaterialButton`; the `use material as m` alias is the prefix. Variants are
values, not separate views: `m.Button("Send", variant: m.tonal)`. The README
says the package implements the public Material 3 guidelines and is not made by
or affiliated with Google.

## Drawbacks

- It is a large surface to keep correct as the engine changes. The gallery's
  tests are what keep it honest.
- Shipping it from the main repository ties its releases to Byard's. A fix to
  one component waits for a Byard release, unless the package gets patch
  releases of its own, which the registry allows.
- Material Symbols makes bundles bigger until subsetting exists.

## Alternatives

- **Components in the engine, in Rust**, like `TextField` and `Toggle` are now.
  Faster to draw, but every app gets them whether it wants them, a design
  system becomes part of the language, and nobody else could write one the same
  way. Writing it in Byld is the point.
- **A separate repository.** Cleaner releases, but an engine change could break
  it with no CI to say so. That can come later, once the language is stable.
- **Material 3 without Expressive.** Expressive is the current Material, and
  the parts it adds (shape morphing, springs, connected button groups, the
  loading indicator) are the ones Byard draws better than other toolkits.

## Unresolved questions

- The registry's domain and who can publish to it.
- Whether the package's theme should replace `byard-base` as the default
  theme when no `[theme]` is declared.
- Right-to-left layout for navigation and lists, which the engine doesn't do
  yet.
- Adaptive layouts (the rail on wide windows, the bar on narrow ones) depend on
  responsive styles, which exist, and on window size classes, which don't.

## Action plan

1. Spike: `Button`, `Switch` and `TextField` in `packages/material` on main.
   Log every gap the way Stockroom did.
2. Engine branches for the prerequisites (1 to 6 above, plus whatever the spike
   finds), each with tests and its own PR.
3. The first-release components, a group per PR: actions, selection, input,
   containment, navigation, communication, scaffold and icons.
4. The gallery, with a test and a light and a dark shot per component.
5. Publish `material 0.1.0` once Byard has a release to depend on.
