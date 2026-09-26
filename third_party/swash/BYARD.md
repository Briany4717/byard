# swash 0.2.9, vendored with one fix

Upstream: https://github.com/dfrg/swash, version 0.2.9 from crates.io (the
version `glyphon`/`cosmic-text` resolve to here), unmodified except for one
line in `src/scale/mod.rs`: `ScalerBuilder::new` clears `context.coords`.
Licence unchanged: MIT OR Apache-2.0, see the LICENSE files beside this one.
The same code is still present in 0.2.10 and on upstream `main` as of
2026-09-24.

## Why

`ScaleContext` keeps a single buffer of normalized variation coordinates for
every scaler it builds. `ScalerBuilder::new` does not clear it, and
`ScalerBuilder::variations` only resizes it before writing the axes the
caller names. So a variable font scaled away from its default leaves its
coordinates behind, and the next variable font scaled through the same
context inherits them on every axis it does not set.

`cosmic-text` sets only `wght`. On macOS the system font has further axes,
so after any other variable font was drawn at a non-default weight, system
glyphs came out at stray coordinates (in the regression test, an `a` 27px
wide became 41px). `glyphon` then re-rasterized cached glyphs when its atlas
grew and wrote the wider images into slots sized for the narrow ones, which
cut ordinary text into overlapping slivers.

The regression test is
`byard_core::text::tests::a_glyph_rasterizes_the_same_after_another_variable_font`.

## Removing this

When a swash release clears the coordinates itself, delete this directory
and the `[patch.crates-io]` entry in the workspace `Cargo.toml`, and run the
test above.
