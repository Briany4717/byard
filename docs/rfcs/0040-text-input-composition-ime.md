# RFC-0040: Text input, composition and IME

- **Status:** Accepted, implemented 2026-09-24 (`TextField`, `byard-platform`,
  both hosts; `examples/text_input`). What is verified automatically and what
  still has to be checked by hand is listed in §9.

  **One refinement against the text below.** §3 says a commit "travels the
  existing `TextInput` path". It reaches the field through the same
  `TextInput` handler, but it crosses the platform boundary as its own
  `EventKind::CompositionCommit`, with `EventKind::CompositionEnd` for
  `Disabled`. Without a separate kind, a commit could not be routed to the
  composition's owner (§6) while typed text keeps going to the focused field,
  because the two would be indistinguishable by the time they reached the
  router.
- **Author(s):** Briany4717
- **Created:** 2026-09-24
- **Last updated:** 2026-09-24
- **Depends on:** RFC-0001 (§5 the logic/render thread split and `frame.rs` as
  the only boundary between them; §6 `PlatformHost` keeps `winit` out of
  `byard-core`), RFC-0004 (the reactive tick), the `TextField` intrinsic and
  its focus registry, RFC-0034 (font families, which the preedit is shaped in).
- **Enables:** typing Chinese, Japanese, Korean, Vietnamese and any other
  script entered through an input method; dead-key accents on macOS, which
  only arrive through the IME path.

> **Placement:** `docs/rfcs/0040-text-input-composition-ime.md`

---

## Summary

A `TextField` accepts composed text. While an input method is composing, the
text being composed (the *preedit*) is drawn inline at the caret, underlined,
with the IME's own cursor inside it, and it is **not** the field's value: the
bound `var` does not change until the IME commits. The engine reports the
caret's rectangle outward every frame so the platform can place its candidate
window next to it. A composition that is interrupted by focus moving away is
discarded by the engine, never committed by it.

Nothing changes in `byld`. A `TextField #[bind: name]` that works today works
the same, and starts accepting composed text.

## Motivation

`TextField` today appends each typed character to the bound value and pops the
last `char` on Backspace. Input methods do not work at all, because the window
never asks for them: `winit` delivers no IME events unless the window allows
IME, and Byard never does. The consequences:

- A Japanese, Chinese or Korean user cannot type into any Byard app. The
  keystrokes arrive as Latin letters, the reading the IME would have converted.
- On macOS, dead-key accents (Option-e, e for "é") are combined only on the IME
  path, so they are also lost.
- Backspace removes one code point, which splits a decomposed accent, an emoji
  with a modifier, or a flag in two.

Writing the code first would decide the surface by accident. The single most
common IME defect in every toolkit is treating the preedit as the value: a
validator runs on half a syllable, an `on change` fires for text the user has
not typed, a character counter jumps as the IME rewrites its guess. This RFC
exists to settle that and the questions around it before anything is built.

## Guide-level explanation

For an app author there is nothing to write:

```byld
View Main() {
    var name = ""
    Column #[p: 24, gap: 12] {
        TextField #[bind: name, placeholder: "Your name", width: 320, height: 44]
        Text("Hello, {name}")
    }
}
```

With a Japanese IME active, typing `nihon` shows `にほん` in the field,
underlined, with the IME's candidate window just below the caret. `Text`
below it still says `Hello, `, because `name` has not changed. Pressing Space to convert
shows `日本` underlined, still not the value. Pressing Enter commits: the
underline goes, `name` becomes `日本`, and `Text` updates once.

Clicking elsewhere mid-composition discards the underlined text. The value is
what it was before the composition started.

## Reference-level explanation

### 1. The preedit is not the value

A field's state is two separate things:

| | The value | The composition |
|---|---|---|
| Where it lives | the bound `var` | the engine, per focused element |
| Who writes it | typing, commits, Backspace, the app | the platform IME only |
| Reactive | yes, it is app state | no |
| Visible to `byld` | yes | no |
| Survives focus loss | yes | no |

The composition is `{ text: String, cursor: Option<(usize, usize)> }`: the
preedit string and the IME's cursor, as a byte range inside it (`None` hides
the cursor). It is drawn **at the caret**, inline, after the value's text
before the caret and before the value's text after it. Since the caret is at
the end of the value today (§8), that is after the value.

The value's `var` is written exactly as it is now, by committed text and by
editing keys. It is never written with preedit text, so every reader of the
`var` (a `Text`, a validator, an `on change`, a `Store`) sees only text the
user committed.

### 2. Where the composition lives

In the interpreter, owned by the element that has focus, as engine state:
there is at most one composition, because there is at most one focused
element. It is not a `var`, it has no name in `byld`, and it cannot be bound,
read or written from an app. An implementation may keep it in an internal
signal so that a preedit change schedules a repaint of the field through the
ordinary tick; what matters is that nothing an app can name round-trips
through it.

**Why not a `var`.** A preedit changes on every keystroke of a composition,
and it is routinely rewritten wholesale (a reading becomes a conversion, a
conversion becomes another candidate). If it were app state, every one of
those rewrites would be an app-visible change, and the defect this RFC exists
to prevent would be the default.

### 3. The platform boundary

`byard-core` stays free of `winit` (RFC-0001 §6). It gains a platform-neutral
event:

```rust
pub enum ImeEvent {
    /// The platform turned the IME on for this window.
    Enabled,
    /// The preedit is now `text`, with the IME cursor at `cursor` (bytes).
    /// An empty `text` clears it.
    Preedit { text: String, cursor: Option<(usize, usize)> },
    /// Insert `text` into the value at the caret.
    Commit(String),
    /// The platform turned the IME off; any preedit is gone.
    Disabled,
}
```

`PlatformHost` gains `on_ime(&mut self, event: ImeEvent)`, defaulting to a
no-op so a host that does not opt in behaves exactly as before. `byard-platform`
maps `WindowEvent::Ime` onto it with a pure function, which is what makes the
mapping testable without a window.

The router carries the event to the focused `TextField` as a new
`EventKind::Composition` whose payload is the preedit and cursor, and a
`Commit` travels the existing `TextInput` path. **Committed text and typed
text are the same thing** once they reach the field, so they share one code
path: a commit clears the preedit and inserts its text exactly as typing
would.

**Text arrives through exactly one door.** `winit` guarantees that during a
preedit the window receives no `KeyboardInput`, and that text an IME commits
arrives as `Commit` and not also as a key event (on macOS this is enforced in
`winit`'s own `insertText`/`keyDown` handling). Typed text outside a
composition keeps arriving as `KeyboardInput`. So the two sources are
disjoint, and neither is deduplicated against the other. Typed text is read
from the key event's `text` rather than its `logical_key`, which is what
carries a Windows dead-key combination.

As a second line, the field ignores editing keys (Backspace, Delete, Enter)
while its preedit is non-empty: platforms differ in what they forward, and a
Backspace meant for the IME must never also delete committed text.

### 4. Turning the IME on, and placing the candidate window

The candidate window is the platform's, never the engine's. The engine's
whole job is to say where the caret is.

Each frame, the focused `TextField` records its caret rectangle on the
`RenderFrame` as `text_input: Option<TextInputState { caret: Rect }>`, in
**viewport space**: logical pixels, after the element's transform and its
scroll containers' offsets, which is what the platform's
`set_ime_cursor_area` wants once it is scaled to physical pixels. `None` means
no field has focus.

The platform thread reads it from the frame it is about to present, and:

- calls `set_ime_allowed(true)` when it becomes `Some`, `false` when it becomes
  `None`. On iOS and Android this is also what shows and hides the soft
  keyboard, so the same line will serve a mobile host when there is one;
- calls `set_ime_cursor_area` when the rectangle changes, and not otherwise.

Reading it from the published frame keeps the logic thread from ever touching
the window, which is the RFC-0001 §5 rule, and means the rectangle is always
that of the frame on screen.

### 5. Clauses

A Japanese conversion is several clauses, one of them active. `winit` 0.30
does not expose clause boundaries or attributes: it gives one preedit string
and one cursor range. So the field draws:

- the whole preedit with a thin underline, in the field's text colour;
- the cursor range, when it is a non-empty range, with a thicker underline,
  which is how IMEs that use it mark the active clause;
- a caret at the range's end, when there is a range.

Richer clause rendering waits for the platform layer to expose it (listed
under unresolved questions).

### 6. Focus loss

When focus leaves a field mid-composition, **the engine discards the preedit;
it never invents a commit.** The engine does not know the IME's conversion
state, so a commit it synthesised from the preedit might be an unconverted
reading, and a platform that also commits on blur would then insert the text
twice. The platform, which does know, is asked to end the composition
(`set_ime_allowed(false)` follows from §4 when no field is focused).

If that platform's convention is to commit on blur and it sends a `Commit`,
the commit is routed to the element that **owned the composition**, not to
whatever has focus now. The owner is recorded when a preedit starts and cleared
by a commit or by `Disabled`. So a platform that commits on blur still gets
its text into the right field, and one that does not leaves the value
untouched. Neither path loses text into the wrong field.

### 7. Editing the committed value

Backspace removes the last **grapheme cluster**, not the last `char`, using
the same segmentation the text stack already depends on
(`unicode-segmentation`). A decomposed accent, an emoji with a skin-tone
modifier and a regional-indicator flag each go in one press.

### 8. What this RFC does not add

The caret stays at the end of the value, as it is today. Moving it (arrows,
Home/End, clicking into the text) and selecting are text *editing*, a surface
with its own questions (word boundaries, bidirectional text, selection
painting, clipboard), and they deserve their own RFC. Everything here is
written so that a movable caret slots in: the preedit is drawn "at the caret"
and inserted "at the caret", which today is the end.

### 9. Testing without a platform IME

- **The mapping** from `winit`'s `Ime` to `ImeEvent` is a pure function,
  unit-tested per variant.
- **The field** is tested through the interpreter with synthetic events, the
  way every other input is: a sequence of `Composition` and `TextInput`
  events drives it, and the assertions are on the bound `var`, on the frame's
  text lines (the preedit span is drawn, underlined, at the caret), and on
  `RenderFrame::text_input` (the caret rectangle, in viewport space,
  transformed and scrolled).
- **The routing** of a late commit to the composition's owner is tested by
  moving focus between a preedit and its commit.
- **What cannot be tested in CI** is a real IME. The example states what to
  check by hand on each platform (a Japanese IME, a Korean IME, a macOS dead
  key), and that list is part of the milestone's gate.

## Drawbacks

- A second piece of per-field state that is deliberately not reactive app
  state. It is the right model, but it is a new kind of thing in an engine
  where state is otherwise `var`s.
- The candidate window's position depends on the caret rectangle being right
  in viewport space. A field inside a transformed or scrolled container is
  exactly where that has gone wrong before, which is why §9 asserts it there.

## Rationale and alternatives

- **Write the preedit into the value and mark it.** Every toolkit that did
  this has spent years patching validators and counters. Rejected: §1.
- **Commit on focus loss, as browsers do.** The engine cannot commit
  correctly (it does not know the conversion state), and a platform that also
  commits would double the text. Routing the platform's own commit to the
  owner gets the browser behaviour where the platform wants it without the
  engine guessing: §6.
- **Draw the candidate window in the engine.** It would have to reimplement
  every IME's candidate UI and would still be wrong. The platform draws it;
  the engine reports a rectangle.
- **Keep the IME always allowed.** It changes key-event delivery for the
  whole window (no key events during preedit), which a game-style view does
  not want, and on mobile it would keep the soft keyboard up. It is allowed
  only while a field has focus.

## Prior art

- `winit`'s `Ime` events and `set_ime_allowed` / `set_ime_cursor_area`, which
  this RFC maps onto without adding to.
- The DOM's composition events (`compositionstart`/`update`/`end`), which also
  keep the composition out of the input's value until it ends, the separation
  §1 adopts.
- AppKit's `NSTextInputClient` marked text, and Windows TSF composition
  strings, both of which model the preedit as a range distinct from the
  document.

## Unresolved questions

- **Clause boundaries and attributes.** Not exposed by `winit` 0.30. When
  they are, §5's rendering grows to match; until then only the cursor range
  is emphasised.
- **Surrounding text and reconversion.** An IME that reconverts already
  committed text needs the text around the caret; `winit` has no surface for
  it (see the `TODO` on `insertText:replacementRange:` in its macOS view).
- **Whether each platform commits on blur.** §6 is correct either way, but
  which way each platform goes is observed by hand, not assumed, and recorded
  when the implementation lands.
- **Caret movement and selection.** A separate RFC (§8).
- **Secure fields.** A password field should not allow IME (the composition
  would be visible in the candidate window). That needs a `secure:` attribute
  on `TextField`, which does not exist yet.
- **Mobile hosts.** §4's `set_ime_allowed` is what shows the soft keyboard on
  iOS and Android, but there is no mobile host to verify it on.
