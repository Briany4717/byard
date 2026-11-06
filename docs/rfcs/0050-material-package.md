# RFC-0050: Byard Material, the first official package

- **Status:** Draft
- **Author(s):** Briany4717
- **Created:** 2026-11-06
- **Last updated:** 2026-11-06 (spike findings)
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
Byard maintains itself, and it lives in a repository of its own,
`Briany4717/byard-material`, the way anyone else's package would. An app adds
it and writes:

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

It is also two tests. Stockroom proved Byard can carry an app. This proves it
can carry a design system that other people build apps from, which is a
different and harder thing: every component is a view someone else calls, with
values, bindings, slots, states, motion and accessibility that have to work
from the outside. And because it lives outside the repository, it is the first
real use of the package system from end to end: `byard add`, `byard get`, the
lockfile, a theme taken with `extends`, fonts named `material/…`, `byard
publish` to a registry, and `byard bundle` carrying the package into a shipped
app. Everything a stranger's package goes through, this one goes through first.

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

or, before the registry exists, from git: `byard add material --git
https://github.com/Briany4717/byard-material --tag v0.1.0`.
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

`gallery/` in the package's repository is an app that depends on the package
by path and shows every component in every variant, light and dark. `byard
shot` draws each page. Its tests are the package's tests.

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

### What the spike found

`Button`, `Switch`, `TextField` and `Icon` were built in the package
repository against every fix pending at the time. The package went through
`byard get` from a git tag, `extends`, `byard publish` to a registry and
`byard bundle`, and every step worked. The content checksum is the same from
path, git and registry. Of the six prerequisites, 3 and 4 turned out not to be
gaps for a package: a package's `[theme]` can declare every colour role and
the expressive corner and type tokens. They are missing only from `byard-base`.
The other four were confirmed.

It also found bugs that are not about Material, and those come first:

- **A view's animations are shared by all its call sites.** Two `T(v: 2)`
  and `T(v: 20)` that animate `radius: v with anim.spring()` both draw the
  last call's value. Only a `for` iteration keeps its own. This makes any view
  with motion unusable more than once on a screen.
- **`size:` on a Text read from a view parameter is ignored.**
- **`Comp(…) => action` on a view call is accepted and dropped.**
- **A package can't name a view like a built-in.** `IntrinsicShadowed` stops
  `Button` and `TextField`, even though callers write `m.Button`.
- `byard shot` draws programs `check` rejects, unknown `[theme.*]` tables are
  accepted silently, and `byard publish` doesn't run `check`.

The icon font is most of a consumer's cost: `check` goes from 52 ms for an
empty app to 210 ms with the package, and to 71 ms without the font. That makes
subsetting part of prerequisite 6, not a later nicety.

### Where it lives and how it ships

- `Briany4717/byard-material`: `byard.toml` (`[package] name = "material"`),
  `src/*.byd`, `fonts/` (Material Symbols Rounded and a text face, Apache-2.0
  or OFL), `gallery/`, README, licence, and its own CI.
- It has its own version. Its README says which Byard versions it supports,
  and its CI installs that Byard (`cargo install byard --version …`) and runs
  `byard check` on the package and the gallery's tests.
- Its CI also runs once a day against Byard's `main`, so an engine change that
  breaks a component is known the next morning, not at Byard's next release.
  Byard's own CI can do the reverse later: fetch the package at its last tag
  and check it, as a canary.
- It is published to Byard's registry, the static layout `byard publish`
  writes, served from GitHub Pages at a domain Byard owns. Until that exists,
  a git dependency with a tag works the same, and is itself part of the test.
- Whatever the package system can't do for a real package (a missing manifest
  key, a confusing error from `get`, a lockfile that changes when it
  shouldn't, a bundle that leaves something out) is a gap in Byard, logged and
  fixed in Byard's repository like Stockroom's were.

### Naming

Views are named as Material names them, without a prefix: `m.Button`, not
`m.MaterialButton`; the `use material as m` alias is the prefix. Variants are
values, not separate views: `m.Button("Send", variant: m.tonal)`. The README
says the package implements the public Material 3 guidelines and is not made by
or affiliated with Google.

## Drawbacks

- It is a large surface to keep correct as the engine changes. The gallery's
  tests are what keep it honest.
- A separate repository can drift from the engine between releases. The daily
  run against `main` is what catches it.
- Material Symbols makes bundles bigger until subsetting exists.

## Alternatives

- **Components in the engine, in Rust**, like `TextField` and `Toggle` are now.
  Faster to draw, but every app gets them whether it wants them, a design
  system becomes part of the language, and nobody else could write one the same
  way. Writing it in Byld is the point.
- **`packages/material` inside this repository.** One CI would catch engine
  breakage at once, but the package would skip what every other package goes
  through (fetching, the lockfile, publishing), and the package system would
  stay untested by any real use.
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

1. Create `Briany4717/byard-material` with the package, a `gallery/` app that
   depends on it by path, and a second app outside it that depends on it by git
   tag. Spike `Button`, `Switch` and `TextField`. Log every gap, in the engine
   and in the package system, the way Stockroom did.
2. Engine branches for the prerequisites (1 to 6 above, plus whatever the spike
   finds), each with tests and its own PR.
3. The first-release components, a group per PR: actions, selection, input,
   containment, navigation, communication, scaffold and icons.
4. The gallery, with a test and a light and a dark shot per component.
5. Publish `material 0.1.0` to the registry once Byard has a release to depend
   on, and bundle the gallery with `byard bundle` to check the package travels
   into a shipped app.
