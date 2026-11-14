# RFC-0064: Layout and text for components

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0005 (the element catalogue and its layout attributes),
  RFC-0018 (text wrap, the measured `Text` leaf), RFC-0032 (the layout class of
  an attribute and the retained build), RFC-0041 (text editing in a
  `TextField`), RFC-0059 (interaction styles; `align` on `Text`, the decorated
  pipeline's border).

> **Placement:** `docs/rfcs/0064-layout-and-text-for-components.md`

---

## Summary

Six things a component library kept running into, each small, each one a
sentence the language could not say:

1. `Row #[wrap: true, gap: 8, row_gap: 8]` flows its children onto new lines.
2. `min_width`, `max_width`, `min_height` and `max_height` on every element.
3. `max_lines: 2` on a `Text`, with an ellipsis where it cuts (`overflow:
   ellipsis`, the default, or `clip`).
4. A multiline `TextField`: `#[lines: 3]` for a fixed height, `#[min_lines: 2,
   max_lines: 6]` for one that grows with its content and then scrolls. Enter
   inserts a newline there and does not submit.
5. `border_gap: (start, length)`, a real gap in the top edge of a border.
6. Shadow presets `"xl"` and `"2xl"`, so the presets cover Material's elevation
   levels 1 to 5, and a table of which is which.

All of it is static: the new layout attributes take values, not animations
(another change makes layout properties animatable and will cover these). Every
one of them is free for an element that does not write it.

## Motivation

Byard Material (RFC-0050) builds Material 3 Expressive on the engine and found
these while building its first wave. Each item names the component that needed
it.

- **Wrapping.** A chip set (`FilterChip`, `AssistChip`, `InputChip` groups) is a
  row that must not overflow a narrow window: the chips that do not fit go to
  the next line, 8 dp apart both ways. Today a `Row` never wraps, so the package
  hand-computes breaks from the window width, which is wrong the moment a chip's
  label changes.
- **Min and max sizes.** Today only a `Button` has a minimum (the touch floor)
  and nothing has a maximum. A Material chip is "at least 32 high", a card
  "at most 560 wide", a snackbar "between 344 and 672", a `TextField` "at least
  56 and at most 280 wide inside a narrow column". The spike wrote `width:` and
  accepted that the element then stopped adapting.
- **`max_lines`.** A list item's supporting text is capped at one or two lines
  with an ellipsis, a card's too. Without it the package cuts the string by
  hand, which cannot know where the line ends.
- **Multiline `TextField`.** Material's text area is the same field with more
  lines. The spike found a field that is one line high and turns a pasted
  paragraph into one long line.
- **Border gap.** The outlined `TextField` floats its label into a notch in
  the outline. The spike drew a `bg` patch over the border, which shows as a
  rectangle on every background but the one it was matched to (an outlined field
  on a `surfaceContainer` card has a surface-coloured box on it).
- **Elevation.** Material has elevation levels 0 to 5 and the presets stop at
  `lg`, so a floating action button (level 3) has nothing to rise to on hover
  (level 4).

## Guide-level explanation

### A row that wraps

```byld
Row #[wrap: true, gap: 8, row_gap: 8] {
    for tag in tags { Chip(tag) }
}
```

Children flow along the row and move to a new line when the next one does not
fit. `gap` is the space between neighbours on a line, `row_gap` the space
between lines; `row_gap` falls back to `gap`, and writing it without `wrap:
true` is a check error, since it would mean nothing. `wrap` is on `Row` only.

### Minimum and maximum sizes

```byld
Box #[min_height: 32, max_width: 560, p: (4, 16)] { ... }
```

`min_width`, `max_width`, `min_height`, `max_height` bound an element after
layout has decided its size: a size below the minimum is raised to it, above
the maximum lowered to it, and the minimum wins over the maximum when they
disagree, as in CSS. They take numbers (logical pixels), static in this
version: `min_width: x with anim.spring()` is the `LayoutPropNotAnimatable`
every layout attribute gives. A `Button` keeps its theme minimum unless the
button writes its own.

### Lines of text

```byld
Text(supporting) #[max_lines: 2]                       // "…" where it is cut
Text(supporting) #[max_lines: 2, overflow: clip]       // cut, no mark
```

The text wraps as it always did and stops after `max_lines` lines: the box is at
most that tall, and the last visible line ends in `…` when something was left
out. `overflow` is `ellipsis` (the default once `max_lines` is written) or
`clip`. `overflow` without `max_lines` and `max_lines` on a `wrap: false` text
are check errors.

### A field with more lines

```byld
var notes = ""
TextField #[bind: notes, lines: 3]                     // always 3 lines high
TextField #[bind: notes, min_lines: 2, max_lines: 6]   // 2 to 6, then it scrolls
```

Long lines wrap at the field's width, Enter inserts a newline, Up and Down move
between lines keeping the column, Home and End go to the ends of the line, and
Ctrl+Home and Ctrl+End to the ends of the text (Cmd+Up and Cmd+Down on macOS).
`submit` is not fired by Enter in a multiline field. `lines: 1`, or no line
attribute, is the single-line field it always was, so a component can forward
`lines: n` with a default of 1. `lines` together with `min_lines` or
`max_lines` is a check error.

### A gap in the border

```byld
Box #[border: 0x79747E, border_width: 1, radius: 4, border_gap: (12, label_w + 8)] {
    Text("Label") #[ml: 16]
}
```

`border_gap: (start, length)` leaves the top edge's border out between `start`
and `start + length` pixels from the element's left edge. What was under the
border shows through, so there is no patch to match to a background. It needs a
`border`, and a pair of numbers.

### Elevation

`shadow: "xl"` and `shadow: "2xl"` join `"sm"`, `"md"` and `"lg"`:

| Material level | dp | Preset |
|---|---|---|
| 0 | 0 | `"none"` |
| 1 | 1 | `"sm"` |
| 2 | 3 | `"md"` |
| 3 | 6 | `"lg"` |
| 4 | 8 | `"xl"` |
| 5 | 12 | `"2xl"` |

A FAB that lifts on hover is `shadow: hovered ? "xl" : "lg" with anim.spring()`.

## Reference-level explanation

### 1. Wrapping

`Row` gains `wrap: Bool` and `row_gap: Int`, both layout-class. In the layout
atlas `ContainerStyle` gets `wrap: bool` and `row_gap: Option<f32>`; `to_taffy`
sets `flex_wrap: Wrap` and gives the style's gap `height` (the gap between lines
of a row) `row_gap.unwrap_or(gap)`. Both are in the layout fingerprint. A
`Column`, a `Box` and the rest do not list them, so they are
`UnknownAttribute` with the hint. `row_gap` with no `wrap`, or with a literal
`wrap: false`, is `InvalidAttributeCombination`.

### 2. Size limits

The four attributes are `Int` and layout-class, listed on every element that
lists `width` and `height`. They ride on `FlexItem` (the call that already lays
`shrink` and `align_self` over a placed node), so they work on a container, a
leaf and a `Text` alike, with the retained build's rule unchanged: an item that
says nothing costs nothing, a changed one marks the node, and a dropped one puts
the built values back. A limit that is written replaces what the node was built
with (a `Button`'s touch floor included). Native views take them too.

### 3. `max_lines` and `overflow`

`Text` gains `max_lines: Int` (layout-class) and `overflow:
ellipsis|clip` (paint-class). A literal `max_lines` below 1 is a check error.

*Layout.* `TextLeaf` carries `max_lines: Option<u32>`; the measure callback
caps the height at `max_lines` lines of the text's line height (1.2 times its
size) after shaping. The width is the shaped width. The count is in the
fingerprint.

*Paint.* `TextLine` carries `max_lines: u16` (0 for none) and `ellipsis: bool`.
The glyph pipeline shapes a capped line with `Ellipsize::End(Lines(n))` when
`ellipsis` and with the buffer's height set to `n` lines otherwise, so the lines
past the cap are not laid out. Both are part of the shape key. A line with
`max_lines: 0` is shaped as before, byte for byte.

### 4. The multiline field

`TextField` gains `lines`, `min_lines` and `max_lines` (layout-class, `Int`). A
field is multiline when any of them is above 1. The checks: `lines` with either
of the others, `min_lines` above `max_lines` (both literals), and a literal
below 1 are `InvalidAttributeCombination` or `AttributeTypeMismatch`.

*Size.* The line height is 1.2 times the text size, as for `Text`. The field's
vertical padding is the padding it writes (`p`, `pt`, `pb`) or 8, its horizontal
padding as in the single-line field (10, or `p`/`pl`). `lines: n` is a leaf of
`n` lines plus padding high (an explicit `height` still wins). `min_lines` and
`max_lines` build a measured text leaf over the value, wrapped to the content
width, with the padding of the field and a minimum and maximum height of the two
counts, so it grows with its content and stops. A width is as for the
single-line field: its `width`, its `grow`, or 200.

*Text.* The value may contain `\n`; every line break typed or pasted is
normalised to it, other control characters are dropped (a tab becomes a space).
The text is drawn as one wrapped `TextLine` at the content width, moved up by
the vertical scroll, inside a clip of the content box.

*Lines.* The editing model learns the field's wrapped lines. They are computed
by the shared text measurer (`TextMeasurer::wrap_lines`: the byte range of each
visual line, from the same shaping `Text` is measured with, so they agree with
what is drawn) and cached on the field's edit state against the value, the width
and the font. A caret is `(line, x)`: the line is the last one that starts at or
before its offset, `x` is the width of the line's text before it. Up and Down
keep a goal column (the x the caret had when the run of vertical moves began);
any other move or edit forgets it. A press maps `y` to a line and `x` to the
nearest boundary on it; a drag does the same with the anchor kept. A selection
is one rectangle per line it touches. The vertical scroll follows the caret as
the horizontal one does (RFC-0041 §6), and is clamped to the text.

*Wheel and preedit.* A multiline field that holds more lines than it shows
scrolls with the wheel (a line is 40 px) and the trackpad while its text can
move that way, and at either end the event goes on to the `ScrollView` around it
(and from one scroll view to the next outside it); the scroll follows
the caret only when the caret or the text moved. An input method's preedit is
put into the text at the caret for drawing and wrapping, so what follows it
moves aside and the caret and the underlines sit where the preedit wraps to. The
accessibility node carries a `multiline` flag, AccessKit's
`MultilineTextInput`.

*Keys.* In a multiline field `Enter` inserts `\n` and does not fire `submit`;
`Mod+Enter` fires `submit` and inserts nothing; `PageUp`/`PageDown` move a page of lines (the lines in view) keeping the goal column; `Home`/`End` go to the start/end of the visual line, with `Ctrl` (`Cmd+Up`/`Down`
on macOS) to the ends of the text; `ArrowUp`/`ArrowDown` as above, and on the
first or last line they go to the start or the end of the text, as a browser's
text area does; `Shift` extends the selection with any of them. Everything else
is RFC-0041's table.

### 5. `border_gap`

`border_gap: (start, length)` is a paint-class `Vec2` on everything that takes
`border`. It needs a `border` (on the element or in one of its state blocks) and
a literal pair of two numbers or two expressions; a scalar or a triple is
`AttributeTypeMismatch`, a missing border `InvalidAttributeCombination`.
`DecoratedBox` carries `border_gap: [f32; 2]` (length 0 is none) and the
`DecoratedBox` instance a new `Float32x2` attribute at the last free vertex
location, 15. The fragment shader multiplies the border's coverage by `1 - gap`
where `gap` is the product of an anti-aliased interval along x
(`[start, start + length]` from the element's left edge) and the top band of the
border (`y` within `border_width` of the top edge). The gap is meant for the
straight part of the edge; a gap that reaches into a rounded corner removes the
border only in the top band. What the border would have covered shows what is
under it: the element's own fill, then the page. Nothing changes for an element
without it: the field is zero and the shader's gap term is a constant `1`.

### 6. Elevation presets

`"xl"` is `(y: 8, blur: 20)` and `"2xl"` `(y: 12, blur: 28)`, with the preset
alpha rule (`0x44` rising with the blur to `0x66`). The error for a word that is
not a preset lists the five. The table above is the mapping; it is in
`styling.md`.

### 7. Diagnostics

One new error, `InvalidAttributeCombination { span, message }`, for an attribute
that means nothing without, or with, another: `row_gap` without `wrap`,
`overflow` without `max_lines`, `max_lines` on a `wrap: false` text, `lines` with
`min_lines`/`max_lines`, `min_lines` above `max_lines`, `border_gap` without a
`border`. The message says what to write.

## Drawbacks

- **Two more `TextLine` fields and a vertex attribute.** The text line is a
  struct built in many places (a dozen tests among them), and the decorated
  pipeline now uses its last free vertex location. Neither costs anything per
  frame for an element that does not use it, but the next thing the pipeline
  needs will have to pack a lane.
- **The multiline field draws its text as one wrapped run and works out the
  caret from a second shaping.** The two agree because they are the same
  engine, the same font, size, weight and width; if they ever stop, the caret
  is off by a glyph. A test shapes both and compares.
- **A limit replaces a built value.** `min_height: 0` on a `Button` removes its
  touch floor, which is the point, and a way to build a button nobody can hit.
- **Nested scrolling is the usual rule, and it changed `ScrollView` too.** A
  wheel goes to the innermost thing under the pointer that can still move that
  way: a multiline field, then the scroll views around it from the inside out.
  A `ScrollView` inside a `ScrollView` used to keep every wheel event even at its
  end; it now passes it out.

## Rationale and alternatives

- **`wrap` on `Row` only.** A wrapping `Column` has no use in the library and
  `row_gap` would be misnamed there; `Box #[direction: row]` can say it with a
  `Row`.
- **Limits in `ContainerStyle` only.** Cheaper to write, and a `Text`, a
  `TextField` and an `Image` would then each need their own copy. `FlexItem` is
  where the node-level overrides already live.
- **A `Text` ellipsis by cutting the string on the logic side.** The logic side
  cannot know where the line ends without shaping the text it is about to hand
  the encoder to shape again.
- **A `TextField` that wraps by inserting breaks into the value.** The value is
  the app's data; the wrapping is how it looks.
- **A `rows` name for `lines`.** HTML's `rows` is the precedent, but Byard calls
  the thing `Row`, and a `Text` has `max_lines`.
- **`border_gap` as a second border mesh or a clip.** A clip would cut the
  fill too, and a mesh is a second draw for a one-pixel edge. The border is
  already coverage in the shader; subtracting an interval from it is the
  smallest change.
- **Elevation as a number (`shadow: 4`).** It would be the first attribute that
  takes either a word or a number, and a number says nothing about which
  elevation scale it is on. The words keep the presets' shape.

## Prior art

- **CSS** for `flex-wrap`, `row-gap`, `min-*`/`max-*` (and the rule that the
  maximum wins), and `-webkit-line-clamp`/`text-overflow`.
- **Flutter** for `maxLines`, `minLines` and `TextOverflow.ellipsis`; a
  `TextField` there is multiline when `maxLines` is above 1.
- **Android's `TextView`** for `maxLines` and `ellipsize`.
- **Material 3** for the elevation levels, the text area and the notched
  outline.
- **Compose's `OutlinedTextField`**, which cuts the notch with a clip on the
  label's rectangle; this is the same effect in the border instead.

## Resolved questions

### Q1, does `row_gap` default to `gap`?

**Question.** A wrapped row with `gap: 8` and no `row_gap`.

**Options.** (a) 0 between lines; (b) `gap`.

**Resolution: (b).** A chip set is spaced the same both ways, and (a) is the
spacing nobody wants. `row_gap: 0` still says it.

### Q2, which wins, a minimum or a maximum?

**Resolution.** The minimum, as CSS and as Taffy do it: an element that is
told it is at least 200 wide and at most 150 is 200 wide.

### Q3, what is `max_lines` on a `wrap: false` text?

**Question.** One line is what it already is.

**Options.** (a) ignore it; (b) a check error.

**Resolution: (b).** The author wrote two attributes that disagree about
whether the text wraps, and `max_lines: 1` is the natural way to say "one line
with an ellipsis", which `wrap: false` does not draw. The error says to drop
`wrap: false`.

### Q4, is `lines: 1` multiline?

**Resolution: no.** `lines: 1`, `max_lines: 1` and no attribute are the single-line
field. A component then forwards `lines: lines` with a default of 1 and does not
need two spellings.

### Q5, does Enter submit in a multiline field?

**Options.** (a) never; (b) `Enter` inserts and `Cmd/Ctrl+Enter` submits.

**Resolution: (b).** A chat box or a comment form wants a keyboard submit, and
no `Ctrl+Enter` did anything in a field before.

## Future possibilities

- Layout attributes under `with` (limits, `wrap`), in the change that makes
  layout animatable.
- `border_gap` animated, for a label that moves into the notch.
- `overflow: fade` for a text, and `overflow: ellipsis` on a single line with no
  `max_lines`.
- A `Column` that wraps.
