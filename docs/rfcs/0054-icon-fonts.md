# RFC-0054: Icon fonts — variation axes and subsetting

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0034 (font families and the `weight` axis), RFC-0022
  (assets in packages, `[assets.vectors] include`), RFC-0045 (the bundle),
  `third_party/swash/BYARD.md` (the variation-coordinate leak, #348).

> **Placement:** `docs/rfcs/0054-icon-fonts.md`

---

## Summary

`Text` and `TextField` get `variations: (FILL: 1, wght: 500, GRAD: 0, opsz: 24)`,
the variable-font axes beyond `wght`, drawn by the glyph rasteriser. An axis can
be animated with `with`. `[assets.fonts]` learns a table form,
`symbols = { path = "...", icons = true }`, which declares an icon font;
`byard bundle` then keeps only the glyphs for the ligature names the program
uses (and the `fvar`, `gvar` and `avar` data for those glyphs). A name the check
cannot find statically is a warning that says which text, and the whole font is
shipped unless the manifest lists `icons_include`. A font not declared as an
icon font is never touched.

## Motivation

Material Symbols is one 15 MB variable font whose icons are ligatures (`Text("favorite")`
draws a heart) and whose look is four axes. Byard shapes the ligatures already, but
`variations:` is an `UnknownAttribute`, so the icon is always the outline one, and
`byard bundle` copies the whole file, so an app with four icons ships 15 MB. The
Material package spike measured the font alone at +140 ms for `byard check`. Both
are blockers for Byard Material (RFC-0050, prerequisite 6).

## Guide-level explanation

```byld
Text("favorite") #[font: symbols, size: 24, variations: (FILL: 1, wght: 500)]
Text("favorite") #[font: symbols, variations: (FILL: liked ? 1 : 0 with t.spring)]
```

Axis names are the four-letter tags of the font (`FILL`, `GRAD`, `opsz`, `wght`,
`wdth`, ...) and the values are numbers. An axis the font does not have is ignored
by the font, as in CSS. `wght` in `variations` and `weight:` on the same element is
a check error (`VariationWeightConflict`), because two numbers would fight for one
axis. A `typo:` token's weight yields to an explicit `wght`.

Declaring an icon font:

```toml
[assets.fonts]
symbols = { path = "fonts/MaterialSymbolsRounded.ttf", icons = true }
# names the program builds at run time:
# symbols = { path = "...", icons = true, icons_include = ["favorite", "home"] }
```

`byard bundle` writes a font with the glyphs for the names found in the program's
`Text("literal")` (and `Text` with a literal string through a `let`) whose `font:` is
that family. `byard check` warns (`IconNameNotStatic`) on a `Text(name)` in that
family whose value it cannot resolve, naming the line. With the warning the whole
font is shipped, unless `icons_include` lists the names. `byard run` and `shot` load
the whole font; the saving is the bundle's.

## Reference-level explanation

**Syntax.** `variations` is a record literal whose keys are 4-character tags and
whose values are numbers, ordinary expressions or animated values. New prop type
`PropType::Variations`; a key longer or shorter than four characters is an error.

**Drawing.** Neither glyphon nor cosmic-text can carry axes other than `wght`: the
glyph cache key has no room for them. A vendored cosmic-text would not survive
`cargo publish` (`[patch]` does not reach crates.io users), so the axes go through
glyphon's own extension point, custom glyphs. A line with `variations` is shaped by
cosmic-text as always (layout, ligatures, advances). Its text area then points at an
empty buffer, so glyphon draws no ordinary glyphs for it, and carries one
`CustomGlyph` per laid-out glyph instead, placed from the layout run (cell: a quarter
size of padding either side, 1.25 sizes above the baseline to half a size below).
`prepare_with_depth_and_custom` rasterises them through swash with every axis of the
font set (unnamed ones to their defaults, so nothing leaks between glyphs), and
`(font, weight, glyph, size, axis set)` is interned to the `u16` glyph id, so two
settings never share an atlas slot. The custom glyphs are built when the line is
shaped and cached with it; a steady frame looks nothing up. A line without `variations`
takes the unchanged path at no cost. The id space is 65 535 distinct glyphs; past it a
glyph is not drawn.

**Layout.** Shaping uses the face at the line's `weight`. Axes that move advances
(`wdth`) are drawn but not re-measured; icon fonts keep their advances fixed. The
guide says so.

**`wght` and `weight:`.** If `variations` has `wght` and `weight:` is absent, the
line's weight is that value rounded, so measuring and drawing agree.

**Animation.** The set is part of the line, so a moving axis changes it every frame:
each frame rasterises the glyphs for the new value (a small outline per icon, atlas
slots grow by one per distinct quantised value). Cost is paid only while the value
moves, and a settled spring costs nothing; this is accepted because the alternative
(a rasterised blend) does not exist for outlines.

**Subsetting.** `byard-cli` links `fontcull-klippa` (a fork of the fontations
subsetter that handles `GSUB` closure and `gvar`/`fvar`/`avar`/`HVAR`). The plan
retains the codepoints of the letters in the used names and the default layout
features; closure keeps the ligature glyphs. Names are found by walking the
compiled program for `Text` whose `font:` is the icon family. The result replaces the
font in the bundle. A font without `icons = true` is copied as is.

## Drawbacks

- A custom glyph is a bitmap cell, not hinted outline text, and its subpixel position snaps to a whole pixel; right for icons, not for body text.
- The subset keeps any ligature whose letters are all kept, so a few unused icons
  can come along (`air` with `fair`). Bytes, not behaviour.
- `fontcull-klippa` adds about thirty crates to the CLI's build, none to apps.

## Rationale and alternatives

- **Google's `klippa`** (the fontations subsetter) is not on crates.io; `fontcull-klippa` is its published fork.
- **`subsetter` (typst):** small, but drops `GSUB` and the variation tables.
  **`allsorts`:** subsets static fonts only. Neither can do the job.
- **Vendoring cosmic-text** to add the axes to its cache key: rejected, it cannot be published.
- **Pre-instancing a font per variation set:** needs an instancer; more bytes shipped.
- **An `Icon` view in core:** not needed; `Text` with a font is the primitive.

## Unresolved

- Re-measuring text for advance-changing axes.
- Pruning ligatures that are not used even when their letters are kept.
