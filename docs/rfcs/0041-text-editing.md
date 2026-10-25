# RFC-0041: Text editing, caret, selection, clipboard and undo

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-10-24
- **Last updated:** 2026-10-24
- **Depends on:** RFC-0040 (composition: the preedit is drawn at the caret, and
  the keys the IME owns), RFC-0003 (the event router and focus), RFC-0004 (the
  reactive tick), RFC-0001 §6 (`PlatformHost` keeps `winit` out of
  `byard-core`).
- **Enables:** forms that people can actually fix a typo in; search fields that
  take a pasted query; any app that edits text.

> **Placement:** `docs/rfcs/0041-text-editing.md`

---

## Summary

A `TextField` gets a caret that moves, a selection, the clipboard and undo.
The caret moves by grapheme with the arrows, by word with the platform's word
modifier, and to either end with Home and End. Shift extends a selection; a
press places the caret where it lands and a drag selects; a double press
selects a word. Mod+A selects everything, Mod+C, Mod+X and Mod+V copy, cut and
paste, Mod+Z and Mod+Shift+Z (or Mod+Y) undo and redo. Delete removes the
grapheme after the caret, which also fixes a defect: today Delete empties the
whole field.

Nothing changes in `byld`. Every `TextField #[bind: name]` written today
starts editing like a text field everywhere else.

## Motivation

`TextField` appends what is typed and removes the last grapheme on Backspace.
The caret is always drawn at the end (RFC-0040 §8 left caret movement and
selection to "a separate RFC"; this is that RFC). In practice:

- A typo in the middle of a field can only be fixed by deleting everything
  after it.
- Delete clears the field (`"Delete" => write_signal(sig, "")` in the
  field's key handler), which loses the user's work with one key.
- Nothing can be pasted, so a search box cannot take a copied address, and a
  login form cannot take a password manager's paste.
- There is no undo, so a mistaken Delete is final.

Every one of these is a defect a user meets in the first minute. For a
framework whose first promise is the developer's experience, the developer
cannot work around any of them in `byld`: the field owns its text.

## Guide-level explanation

Nothing to write. A field like

```
TextField #[bind: query, placeholder: "Search"]
```

now behaves as follows while it has focus:

| Input | Effect |
|---|---|
| Left / Right | caret one grapheme back / forward; with a selection, collapse it to its start / end |
| Mod-word + Left / Right (Ctrl on Linux and Windows, Option on macOS) | caret to the previous / next word boundary |
| Home / End (and Cmd+Left / Cmd+Right on macOS) | caret to the start / end |
| Shift + any of the above | extend the selection instead of moving |
| Backspace / Delete | remove the selection, or the grapheme before / after the caret |
| Mod-word + Backspace / Delete | remove to the previous / next word boundary |
| Mod+A | select all |
| Mod+C / Mod+X | copy / cut the selection (nothing selected: nothing happens) |
| Mod+V | replace the selection with the clipboard's text (newlines become spaces: the field is one line) |
| Mod+Z / Mod+Shift+Z or Mod+Y | undo / redo |
| press | caret to the nearest grapheme boundary under the pointer |
| press and drag | select from the press to the pointer |
| double press | select the word under the pointer |

"Mod" is Cmd on macOS and Ctrl elsewhere, as for the dev chords
(`KeyModifiers::mod_held`). Typed text replaces the selection. The selection is
painted behind the text in the theme's `primary` at 30 % opacity, or a field's
`selection:` colour when it sets one (the only new attribute, optional).

When the bound `var` is written from somewhere else (an action, a reply), the
caret moves to the end and the selection collapses, because the text the caret
pointed into is gone.

`on input`, `on change`, `on text_input`, `on key_down` and `on submit` fire as
they do today. An edit that changes nothing (Backspace at the start, a paste of
nothing) fires nothing, as Backspace in an empty field already does.

## Reference-level explanation

### 1. Edit state lives in the interpreter, per field

```rust
struct EditState {
    caret: usize,          // byte offset, always on a grapheme boundary
    anchor: usize,         // == caret when nothing is selected
    undo: Vec<Snapshot>,   // bounded, see §5
    redo: Vec<Snapshot>,
    last_seen: Arc<str>,   // the value this state was computed against
    coalesce: Coalesce,    // what the last edit was, for §5
}
```

held in the interpreter beside `composition`, keyed by the field's element
index, created on first focus and dropped when the element is torn down. A
field never focused has none. When `last_seen` differs from the bound value at
the start of a handler, the value was written from outside: caret and anchor
go to the end, undo and redo are cleared.

Every offset is a byte offset kept on a grapheme boundary
(`unicode-segmentation`, already a dependency). Words are
`unicode_word_indices` boundaries, so "naïve café" moves by word the way a
reader expects.

The IME keeps priority: while composing (RFC-0040 `composing()`), the keys the
IME owns stay consumed and the preedit is drawn at `caret`, not at the end. A
commit inserts at the caret, replacing the selection.

### 2. Modifiers reach the field

`InputEvent` gains `mods: KeyModifiers`. The platform already tracks them
(`WindowEvent::ModifiersChanged`) for the dev chords; it now copies them onto
every key and pointer event. `KeyModifiers` stays where it is; its doc comment
changes, because it no longer has one consumer.

The `key_down` payload a `byld` handler sees is unchanged (the key name), so
this is not a language-visible change. That was the only reason RFC-0030 kept
modifiers off the event, and it still holds.

### 3. The clipboard is a capability the host provides

```rust
// byard-core
pub trait Clipboard: Send {
    fn read_text(&mut self) -> Option<String>;
    fn write_text(&mut self, text: &str);
}
pub struct MemoryClipboard { /* a String */ }
```

The interpreter holds a `Box<dyn Clipboard>`. `byard-platform` provides
`SystemClipboard`, backed by `arboard` (with its `wayland-data-control`
feature, so it works under Hyprland, GNOME and KDE on Wayland as well as X11),
created **lazily on the first copy, cut or paste** and kept after that.
Headless runs (`Shot`, `byard shot`, every test) get a `MemoryClipboard`, so a
test can paste without touching the machine's clipboard.

A clipboard that fails (no display server, a sandbox) reads as empty and
writes as nothing. It never fails the edit or the frame.

### 4. Pointer to caret

A press on a focused field (or the press that focuses it) maps its x to a
caret by measuring prefix widths at grapheme boundaries with the field's font
and size, through the interpreter's existing `measure_text`. The widths are
cached for the current value, font and size, so a drag measures nothing new;
the cache is dropped when any of the three change. The nearest boundary wins
(a press past the middle of a grapheme lands after it).

Drag: `PointerMove` with the primary button down, after a press in the field,
moves `caret` and keeps `anchor`. A drag past either edge scrolls the field's
text (see §6) one grapheme per frame.

Double press (`EventKind::DoubleTap`) selects the word under the pointer.

### 5. Undo

A snapshot is `(value, caret, anchor)`. An edit pushes the state before it and
clears redo. Consecutive typed characters coalesce into one undo step until the
caret moves, a non-character key is pressed, a word boundary is typed (a space
or punctuation after a letter) or 1 s passes, which is what users of every
native field expect. Paste, cut, and a deletion of a selection are always their
own step. The stack is bounded at 100 steps per field; the oldest drops.

Undo and redo write the bound `var` like any edit, so `on input` and
`on change` fire for them.

### 6. Painting

`render` of a `TextField` (the function that draws its background, text and
caret today) draws, in order: background, selection box(es), text, caret. The
caret is placed at the measured x of `caret` instead of the end. When the
text is wider than the field, the visible window scrolls so the caret is
always inside the field with a 4 px margin; the offset is part of the edit
state and the text is clipped to the field's content box as it is today.
`TextInputState.caret` (the IME's candidate-window anchor) follows the new
caret position.

The caret blinks (530 ms on, 530 ms off, reset by every edit and caret move)
only while the window has focus. **The blink is the one animation a focused
field runs**, and it must not keep the logic thread awake at frame rate: it
schedules exactly one wake per phase through the existing timer path, so a
focused, idle field costs two frames a second and nothing in between.

### 7. Headless scripting, so a person can see it

`Shot` gains `key(name)` (with an optional `KeyModifiers`) and
`type_text(text)`, and `byard shot` gains repeatable `--key` (`--key
shift+Left`, `--key mod+a`) and `--type "text"`, applied in order with the
existing `--tap`. With them a selection can be drawn to a PNG without a window:

```bash
cargo run -p byard-cli -- shot crates/byard-cli/examples/text_editing \
  --tap 200,120 --type "hello wrold" --key Left --key Left --key Left \
  --key shift+Left --key shift+Left -o editing.png
```

## Drawbacks

- One more dependency (`arboard`) in `byard-platform`, with its X11 and
  Wayland backends. It is only linked into the windowed host, never into
  `byard-core` or the compiler.
- The field is single-line; this RFC does not make it multi-line, and
  everything here (Home and End, newlines in a paste) is written for one line.

## Rationale and alternatives

- **Clipboard through the frame** (logic thread asks, the platform answers a
  tick later) keeps every OS call on the main thread, but makes paste a
  two-tick round trip with a pending state the field must remember. `arboard`
  is usable off the main thread on all three desktop platforms, so the direct
  call is simpler and synchronous. If a future host (mobile) needs the main
  thread, its `Clipboard` implementation can marshal internally; the trait
  does not change.
- **Exposing caret and selection to `byld`** (`#[caret: c]`) was weighed and
  left out: nothing in the motivating apps needs it, and a surface added now is
  a surface kept forever. It is a Future possibility.
- **Doing nothing** keeps a field that loses work on Delete.

## Prior art

Every native toolkit's single-line field (NSTextField, GtkEntry, Win32 EDIT,
the web's `<input>`) has this exact key table; the coalescing rule follows
GtkEntry and the browsers. `egui`'s `TextEdit` and `iced`'s `text_input` hold
the same caret-and-anchor pair in widget state.

## Resolved questions

### Q1, byte offsets or grapheme indices

**Options.** (a) byte offsets on grapheme boundaries; (b) grapheme indices.
**Resolution: (a).** Slicing, inserting and measuring a prefix all take byte
offsets, so (b) would convert on every operation. The boundary invariant is
checked in debug builds after every edit.

### Q2, what a paste with newlines does

**Options.** (a) refuse it; (b) keep the first line; (c) replace newlines with
spaces. **Resolution: (c).** A pasted address or a copied sentence that wrapped
is the common case and (c) keeps all of it; (a) and (b) silently lose text.

### Q3, what Delete with no selection at the end does

**Resolution:** nothing, like Backspace at the start. No event fires.

### Q4, whether the caret blink is opt-out

**Resolution:** it is not configurable in this RFC. It costs two wakes a
second only while a field is focused and the window is focused, which is
within the "costs nothing when unused" rule. A `caret_blink: false` attribute
can be added later without a breaking change.

### Q5, where `Mod` is Cmd and where word-movement is Option

**Resolution:** macOS uses Cmd for Mod and Option for word movement; Linux and
Windows use Ctrl for both. That is the platform convention, and it is the same
split `KeyModifiers::mod_held` already makes.

## Acceptance

- Interpreter tests for every row of the key table, with graphemes that are
  more than one code point (`é` decomposed, a flag, an emoji with a skin
  tone), including the Delete fix.
- Undo coalescing: typing "hello world" then Mod+Z leaves "hello "; a paste is
  one step; redo after a new edit is empty.
- A pasted value with newlines arrives as one line.
- A `var` written from an action moves the caret to the end.
- A readback test (`byard-platform/tests`) that a selection is painted behind
  the glyphs and the caret is drawn at the caret's x, not the end.
- Cost: a frame-budget assertion that a focused idle field wakes the logic
  thread at most 2 times per simulated second, and an unfocused field 0.
- **For a person to check:** a new example `crates/byard-cli/examples/text_editing`
  whose `main.byd` says what to try, and the `byard shot` command above writing
  a PNG with a visible selection.

## Future possibilities

Multi-line `TextArea`; exposing the selection to `byld`; a context menu
(cut/copy/paste) on secondary press; IME reconversion of a selection.
