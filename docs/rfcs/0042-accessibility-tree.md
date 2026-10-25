# RFC-0042: The accessibility tree

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-10-24
- **Last updated:** 2026-10-24
- **Depends on:** RFC-0001 (§5 the logic/render split, `frame.rs` as the only
  boundary; §6 `PlatformHost`), RFC-0003 (events, focus, Tab order), RFC-0005
  and RFC-0018 (the intrinsic catalog), RFC-0041 (text field caret and
  selection, which the tree reports).
- **Enables:** screen readers (Orca, NVDA, Narrator, VoiceOver), switch access
  and voice control for every Byard app; UI automation against the same tree.

> **Placement:** `docs/rfcs/0042-accessibility-tree.md`

---

## Summary

A Byard window publishes an accessibility tree through
[AccessKit](https://github.com/AccessKit/accesskit): each control the user can
read or operate is a node with a role, a name, its bounds and its state, and
the assistive technology's actions (press, focus, set value, scroll into view)
come back as the events a pointer or keyboard would have sent. **The tree is
built only while an assistive technology is listening.** With none running, an
app does exactly what it does today and pays nothing.

`byld` gains two optional attributes, `a11y_label:` and `a11y_hidden:`. An app
that uses neither still gets a tree, named from its text.

## Motivation

Byard draws every pixel itself, so the operating system sees one opaque
surface. A blind user hears "window" and nothing else; nobody can use a Byard
app with a screen reader, a switch device or voice control. That rules Byard
out of any public-sector or enterprise app (accessibility is a legal
requirement in the EU from 2025 under the European Accessibility Act, and in
US federal procurement), and it is a regression against every native toolkit
and the web.

It cannot be fixed in `byld` or in a package: only the engine knows what is on
screen and where.

## Guide-level explanation

Nothing changes for an app that is already written with text in its controls:

```
Button("Save") => save()                  // a button named "Save"
Toggle #[bind: wifi]  Text("Wi-Fi")      // a switch; see §3 for its name
TextField #[bind: q, placeholder: "Search"] // a text input named "Search"
```

A control with no text of its own needs a name:

```
Button #[a11y_label: "Close"] { VectorIcon("x") } => close()
```

and decoration can be taken out of the tree:

```
Image("hero.png") #[a11y_hidden: true]
```

To see the tree without a screen reader:

```bash
cargo run -p byard-cli -- shot crates/byard-cli/examples/accessibility --a11y tree.txt
```

writes the tree as indented text (role, name, value, state, bounds), one node
a line, and marks every control a screen reader could not name with
`(unnamed)`. That file is what a test or a person checks.

## Reference-level explanation

### 1. What crosses the frame

`RenderFrame` gains `access: Option<AccessTree>`, filled by the logic thread
**only when** the shared flag `access_active: AtomicBool` is set. The flag is
set by the platform when AccessKit's activation handler is called (an AT
connected) and cleared when its deactivation handler runs.

```rust
pub struct AccessTree {
    pub nodes: Vec<AccessNode>,   // parents before children
    pub focus: Option<AccessId>,
}
pub struct AccessNode {
    pub id: AccessId,             // see §4
    pub parent: Option<AccessId>,
    pub role: AccessRole,         // a Byard enum, mapped to accesskit::Role in the platform
    pub name: Option<String>,
    pub value: Option<AccessValue>,  // text, number (with min/max/step), or toggled
    pub bounds: Rect,             // viewport space, after transforms and scroll
    pub flags: AccessFlags,       // focusable, focused, disabled, checked, selected, expanded
    pub actions: AccessActions,   // press, focus, set_value, increment, decrement, scroll_into_view
}
```

`byard-core` does not depend on AccessKit: the types above are Byard's own,
so the engine stays free of platform crates (RFC-0001 §6), and a headless run
can produce the same tree without any AT.

### 2. Roles

| Intrinsic | Role | Value / state |
|---|---|---|
| `Button` | Button | |
| `TextField` | TextInput | text, caret and selection (RFC-0041), placeholder as description |
| `Toggle` | Switch | toggled |
| `Checkbox` | CheckBox | checked |
| `RadioButton` | RadioButton | checked; its group is the parent `RadioGroup` |
| `Slider` | Slider | number with min, max, step |
| `Text` | Label (StaticText) | |
| `Image`, `VectorIcon` | Image | |
| `ScrollView` | ScrollView | scroll position |
| `NavHost` tabs | TabList / Tab | selected |
| `Overlay` (modal) | Dialog | |
| a user `View`, `Row`, `Column`, `Box`, `Grid`, `ZStack` | none: flattened | |
| a native view (RFC-0039) | what it declares in its catalog entry, default Group | |

Layout containers are flattened: their children are attached to the nearest
ancestor with a role. A screen reader gains nothing from fifty nested `Row`s,
and a smaller tree is cheaper to build and to send.

### 3. Names

In order: `a11y_label:`; for a `TextField`, its placeholder; the text of the
control's own `Text` children, joined with a space; for a `Toggle`,
`Checkbox` or `RadioButton` with no text of its own, the text of the next
sibling `Text` in the same `Row` (the common "control, then its label"
pattern). Otherwise the node has no name and is reported as `(unnamed)` in
the dump. `Text` inside a named control is not repeated as its own node, and
neither is the sibling `Text` that named a control: it is part of that
control's name, and a reader would otherwise say it twice.

`a11y_hidden: true` removes the element and its subtree. Both attributes are
checked by `byard check` like any other attribute: `a11y_label` takes a string
expression (reactive, so `"{count} unread"` updates), `a11y_hidden` a bool.

### 4. Identity

`AccessId` is the element's index in the lowered tree, the same index the
event router and RFC-0040's composition owner use. AccessKit wants ids stable
across updates so that focus and the reader's position survive; an index is
stable while the tree's shape does not change. When a list inserts above a
node its id changes; the reader then re-reads that node, which is what a
browser does when a DOM node is replaced. Resolved in Q2.

### 5. The platform side

`byard-platform` creates an `accesskit_winit::Adapter` (accesskit_winit 0.34,
winit 0.30) **before the window is shown**, as AccessKit requires, with the
window hidden at creation and shown after. Its handlers:

- activation: set `access_active`, request a frame, return a tree with just the
  window root until the first real tree arrives;
- action: translate to an `InputEvent` aimed at the node: `Click` is a `Tap`
  at the node's centre (so every existing handler runs unchanged), `Focus`
  focuses it through the router, `SetValue`, `Increment` and `Decrement` are a
  `Change` with the new value, `ScrollIntoView` scrolls its `ScrollView`;
- deactivation: clear `access_active`.

On each frame that carries a tree whose content differs from the last one sent
(compared by a hash of the nodes), the platform maps it to an
`accesskit::TreeUpdate` and sends it. An unchanged tree sends nothing.

### 6. Headless

`Shot` gains `access(path)` and `byard shot` gains `--a11y <file>`: the run
sets the flag, takes the last frame's tree and writes it as text. Tests use
the same path, so the tree is checked without any AT, any D-Bus or any window.

## Cost

- **No AT running** (the common case): one relaxed atomic load per frame on the
  logic thread. No tree is built, nothing is allocated, nothing crosses the
  frame. A frame-budget assertion checks that a frame with the flag off
  allocates nothing more than it did before this RFC.
- **AT running:** one pass over the frame's elements, which the paint walk is
  already doing; a hash compare on the platform side; a `TreeUpdate` only when
  something changed. Flattening keeps the tree to the controls.
- **Binary:** `accesskit_winit` pulls in AccessKit's platform adapters (AT-SPI
  over `zbus` on Linux), measured at about a fifth of a stripped release
  binary. It is the largest cost, and it is why the bridge is opt-in (Q4).

## Drawbacks

- The Linux adapter adds an async D-Bus stack to the dependency tree of every
  windowed app.
- Index-based ids churn on list insertion (Q2).
- The name heuristics (§3) will be wrong for some layouts; `a11y_label` is the
  fix, and the dump shows where it is needed.

## Rationale and alternatives

- **AccessKit** is the one cross-platform accessibility layer in Rust, used by
  egui, Bevy, Xilem and Slint; writing three platform adapters by hand is out
  of proportion.
- **Building the tree always** would be simpler, and costs every app on every
  frame for the minority of sessions with an AT. The activation flag is what
  AccessKit's lazy design is for.
- **Ids from a hash of the element's path** would survive list insertion but
  cost a hash per element per frame and collide on duplicates; Q2.

## Prior art

egui's AccessKit integration (lazy activation, flattened layout), Flutter's
semantics tree (merging a control with its label), the web's accessible-name
computation (label, then content, then nearby label), which §3 follows in
simplified form.

## Resolved questions

### Q1, whether `byard-core` depends on AccessKit

**Resolution: no.** Byard's own types cross the frame and the platform maps
them. The engine stays platform-free and headless runs need nothing.

### Q2, index ids or path-hash ids

**Options.** (a) element index; (b) hash of the element's path; (c) a `key:`
the author writes. **Resolution: (a).** It costs nothing and is already the
router's identity. (b) is the per-element hash (a) avoids. (c) needs keyed
`for` loops, which `byld` does not have; when it gets them, a keyed item's id
comes from its key, a change that needs no new design here.

### Q3, what `Click` sends

**Resolution:** a synthesized `Tap` at the node's centre. Every control
already handles `Tap`; a dedicated activate event would need every handler to
learn it.

### Q4, a cargo feature to leave AccessKit out

**Options.** (a) on by default, a `no-a11y` opt-out; (b) off by default, an
`a11y` opt-in. **Resolution: (b), an `a11y` feature on `byard`,
`byard-platform` and `byard-cli`.** This was first written as (a), on the
argument that accessibility which is opt-in is missing from most apps.
Measured, the bridge adds 19 % to a stripped release binary (Aura Weather,
21.4 MB to 25.4 MB), most of it the Linux D-Bus stack, and Byard's first
promise is the smallest footprint for what an app uses. So an app opts in.
What is opt-in is only the hand-off to the platform: the tree itself, the
`a11y_label` and `a11y_hidden` attributes and `byard shot --a11y` work in
every build, so an app can be checked for accessibility before it ships the
bridge.

## Acceptance

- Unit tests for the role table, the name rules (each of the four sources),
  `a11y_hidden`, flattening, and value/state for every value control.
- The flag off: a frame builds no tree (a counter in the build path stays 0)
  and allocates nothing extra (frame-budget assertion).
- Actions: a `Click` on a button's node runs its handler; `SetValue` on a
  slider writes its `var`; `Focus` moves router focus.
- An unchanged frame sends no `TreeUpdate`.
- **For a person to check:** `crates/byard-cli/examples/accessibility` and the
  `byard shot --a11y` dump of it (checked in as a golden file, compared by a
  test), plus, by hand, the example under Orca on Linux (`orca` reads each
  control as Tab moves through them).

## Future possibilities

Live regions (`a11y_live: polite`), headings and landmarks, custom actions,
a `byard check --a11y` pass that runs the dump and fails on unnamed controls.
