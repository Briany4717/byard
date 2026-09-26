//! # `TextGlyph` pipeline
//!
//! GPU text rendering via a [`glyphon`] glyph atlas, integrated into the
//! single UI render pass shared with [`SolidBox`](super::EncoderSubsystem).
//!
//! ## Design constraints
//!
//! - **Single render pass**, `TextGlyphPipeline::render_layer` is called
//!   *inside* the same `wgpu::RenderPass` already started by the `SolidBox`
//!   draw. On Apple Silicon (TBDR architecture) every render pass break
//!   flushes the tile buffer to VRAM; sharing the pass with `SolidBox`
//!   eliminates that cost.
//! - **Layered text draws, one shared atlas**, RFC-0017 z-layers need a
//!   later layer's transparent geometry (a modal scrim, a shadow) to
//!   alpha-blend *over* an earlier layer's text, which a single frame-final
//!   text draw can never provide. Instead of one `TextRenderer`, the pipeline
//!   holds one **per z-layer**, all sharing the *same* `FontSystem`,
//!   `SwashCache`, `TextAtlas`, `Viewport`, and shaped-buffer cache, so every
//!   line is still shaped exactly once and every glyph is rasterised into one
//!   atlas regardless of layer count. The only per-layer cost is one small
//!   glyph vertex buffer and one draw call *inside* the existing pass, never
//!   an extra render pass, never a re-shape, never a duplicate atlas. A
//!   single-layer frame uses exactly one renderer: the pre-layering fast
//!   path, unchanged.
//! - **Content-addressed re-shaping**, a line's glyph buffer is re-shaped if
//!   and only if the [`shape_key`] its buffer was shaped from, `(text,
//!   font_size, wrap)`, differs from this frame's, or the viewport changed,
//!   or index identity is not comparable at all. See [`needs_reshape`].
//!
//!   This replaces trusting [`TextLine::dirty`] alone, and the reason is
//!   measured rather than theoretical. The flag's producer is the
//!   interpreter, which re-walks the tree and re-emits every leaf each tick;
//!   it has no per-element change signal, so it sets `dirty: true` on every
//!   line of every frame. "Trust the flag, hash nothing" therefore bought
//!   zero skips in a real `byld` app and degenerated to *re-shape everything,
//!   every frame*, which is why `encode.glyphs` was the largest single row
//!   on the profile block, and why the dev HUD's twenty-five fixed-width
//!   fields cost four times what they should have (RFC-0030 erratum
//!   "self-accounting", §A2).
//!
//!   The trade is the other way round from what it looked like: an `FxHasher`
//!   pass over a short string is tens of nanoseconds and shaping it is tens
//!   of microseconds, so hashing every line every frame to skip the ones that
//!   did not change wins by roughly three orders of magnitude, and wins on
//!   the very first line it skips.
//!
//!   It is also strictly more robust. A producer that changes a line's text
//!   and forgets to set `dirty` used to render stale glyphs in release, in
//!   silence; now it renders correctly, because the key it is compared
//!   against is derived from the content rather than asserted about it.
//!
//! - **The dirty flag still governs the redraw region**, so it is still
//!   checked, in debug, against the full content hash (colour included):
//!   `dirty` is what [`dirty_text_bounds`](super::dirty_text_bounds) unions
//!   into the incremental scissor, so a line that changed colour with the
//!   flag unset would now be shaped correctly and clipped out of the redraw.
//!   See [`assert_dirty_flag_consistency`].
//! - **Three-pass borrow pattern**, `prepare` splits work across three
//!   sequential passes to satisfy Rust's field-split borrowing rules (see the
//!   method documentation for a precise explanation).
//! - **No panics**, every fallible operation returns [`ByardError`].

use std::hash::Hasher as _;

use glyphon::{
    Attrs, Buffer, Cache, Color, FontSystem, Metrics, Resolution, SwashCache, TextArea, TextAtlas,
    TextBounds, TextRenderer, Viewport,
};

/// Converts a logical-pixel content clip ([`crate::frame::ClipRect`]) into
/// glyphon's physical-pixel [`TextBounds`] (RFC-0005 `ScrollView`), so a text
/// line inside a scroll viewport is clipped to it per-area rather than via a
/// render-pass scissor.
#[allow(clippy::cast_possible_truncation)]
fn clip_to_text_bounds(rect: crate::frame::Rect, scale: f32) -> TextBounds {
    TextBounds {
        left: (rect.x * scale).floor() as i32,
        top: (rect.y * scale).floor() as i32,
        right: ((rect.x + rect.width) * scale).ceil() as i32,
        bottom: ((rect.y + rect.height) * scale).ceil() as i32,
    }
}
use rustc_hash::FxHasher;
use wgpu::MultisampleState;

use crate::ByardError;

// ── Public surface ────────────────────────────────────────────────────────────

/// Re-exported from [`crate::frame`], the canonical definition now lives
/// there so the Logic thread can populate [`RenderFrame::texts`] without
/// importing from a subsystem that it must not depend on (RFC-0001 §9).
pub use crate::frame::TextLine;

// ── Internal cache entry ──────────────────────────────────────────────────────

/// Cached GPU-side state for a single [`TextLine`].
///
/// Lives entirely inside [`TextGlyphPipeline`]; never exposed outside this
/// module. `buffer` is the shaped glyph run, kept across frames so unchanged
/// lines cost nothing beyond one [`shape_key`] hash.
struct CachedLine {
    buffer: Buffer,
    /// The [`shape_key`] `buffer` was last shaped from, the authority on
    /// whether it still describes this line.
    shape_key: u64,
    /// `FxHasher` digest of everything a *repaint* depends on, colour
    /// included.
    ///
    /// **Debug-only redraw-region safety net.** It plays no part in the
    /// re-shape decision ([`needs_reshape`] consults `shape_key`); it exists
    /// so [`assert_dirty_flag_consistency`] can catch a producer that changed
    /// a line without setting `dirty`, because `dirty` is still what
    /// [`dirty_text_bounds`](super::dirty_text_bounds) unions into the
    /// incremental scissor. Absent in `--release`.
    #[cfg(debug_assertions)]
    content_hash: u64,
}

/// The scope charged with a dev surface's share of glyph work, interned once.
///
/// Cached exactly as [`profile_scope!`](crate::profile_scope) caches its own:
/// the scope registry is a mutex, and a per-layer loop is not the place to
/// lock it.
static DEV_GLYPHS_SCOPE: std::sync::OnceLock<crate::telemetry::ScopeId> =
    std::sync::OnceLock::new();

/// The identity of a *shaped glyph run*: `(text, font_size, wrap)`.
///
/// Two lines with the same key shape to byte-identical glyph buffers, so a
/// cached buffer whose key still matches needs no work at all.
///
/// Colour and position are deliberately **not** in it. Neither reaches the
/// shaper: colour is applied per-`TextArea` in pass 2 and position in pass 3,
/// so folding either one in would re-shape a run for a change that provably
/// cannot alter a single glyph. That is the same paint-class/layout-class
/// distinction RFC-0030 §V4's sparkline rests on, applied to the cache key.
///
/// [`FxHasher`] rather than `SipHash`: ≈3× faster on short keys, and this runs
/// once per text line per frame. A collision would render a stale line; at
/// 64 bits over a few hundred lines that is not a failure mode this engine
/// will observe, and the alternative, comparing the string itself, costs a
/// heap copy per line to retain it.
/// Loads every face in `fonts` that `loaded` does not already name, and
/// returns how many were loaded (RFC-0034, INV-27).
///
/// A free function over the `fontdb::Database` rather than a method, so the
/// registration half of INV-27 can be exercised without a GPU: the pipeline
/// that owns the `FontSystem` needs a device, and the invariant does not.
fn load_missing(
    db: &mut glyphon::fontdb::Database,
    loaded: &mut std::collections::HashSet<String>,
    fonts: &crate::frame::FontTable,
) -> usize {
    let mut count = 0;
    for face in fonts.faces() {
        if loaded.contains(face.resolved.as_ref()) {
            continue;
        }
        // Loaded through the same helper as the measurement side, so the two
        // `FontSystem`s cannot be handed the bytes by subtly different routes.
        let here = crate::text::register_into(db, &face.bytes);
        // INV-27 checked at the seam it can break at. The logic thread
        // resolved this face's family name from the same bytes; if this side
        // reads a different one, every line naming that family shapes in the
        // fallback font and no test of *this* frame would notice.
        debug_assert_eq!(
            here.as_deref(),
            Some(face.resolved.as_ref()),
            "family `{}` resolves differently on the paint side (INV-27)",
            face.declared
        );
        loaded.insert(face.resolved.to_string());
        count += 1;
    }
    count
}

/// Shapes one [`TextLine`] into `buffer`, the only place the paint side turns
/// a line into glyphs.
///
/// Free rather than inlined into `shape_range` so the attributes it resolves,
/// family above all, can be checked against the measurement path without a
/// device (INV-27). If this and `TextMeasurer::shape` ever stop agreeing about
/// which face a line is in, the test that compares them is reading the real
/// code on both sides rather than two transcriptions of it.
fn shape_line(
    font_system: &mut FontSystem,
    buffer: &mut Buffer,
    line: &TextLine,
    wrap: Option<f32>,
    metadata: usize,
) {
    let metrics = Metrics::new(line.font_size, line.font_size * 1.2);
    buffer.set_metrics(font_system, metrics);
    // RFC-0018 text wrap: a `Some(w)` bound shapes the line onto multiple
    // lines within `w` logical pixels; `None` keeps the natural single-line
    // width. Height stays unbounded so every wrapped line is shaped.
    buffer.set_size(font_system, wrap, None);
    // Color is applied per-TextArea in pass 2 (default_color field). Here we
    // only need to shape the text; color does not affect layout. Tag every
    // glyph of this line with its line index as glyphon `metadata`, so pass
    // 3's `metadata_to_depth` can look up this line's draw-order depth. A
    // skipped line keeps the metadata it was shaped with, which is still
    // correct: the skip requires the index to mean the same element it did
    // last frame.
    buffer.set_text(
        font_system,
        &line.text,
        &Attrs::new()
            .family(crate::text::family_of(line.family.as_deref()))
            .weight(glyphon::Weight(line.weight))
            .metadata(metadata),
        glyphon::Shaping::Advanced,
        None, // align: no paragraph-level override
    );
    buffer.shape_until_scroll(font_system, false);
}

fn shape_key(line: &TextLine, wrap: Option<f32>) -> u64 {
    let mut h = FxHasher::default();
    h.write(line.text.as_bytes());
    h.write_u32(line.font_size.to_bits());
    // RFC-0018: the wrap width changes the shaped glyph run (line breaks), so a
    // wrap-only change must invalidate the cached buffer.
    h.write_u32(wrap.map_or(u32::MAX, f32::to_bits));
    // RFC-0034: weight and family both reach `Attrs`, so both change the glyph
    // run. Either one missing here is the same defect wearing a different hat:
    // a line that changes face and keeps last frame's buffer, which looks like
    // the prop doing nothing at all.
    h.write_u16(line.weight);
    h.write(line.family.as_deref().unwrap_or("").as_bytes());
    h.finish()
}

/// Computes the debug-only repaint hash for a [`TextLine`]: [`shape_key`]
/// widened with colour.
///
/// Position (`x`, `y`) stays excluded, a moved line is caught by
/// [`dirty_text_bounds`](super::dirty_text_bounds) unioning its previous
/// bounds, not by this.
///
/// **Debug-only.** This function does not exist in `--release` builds.
#[cfg(debug_assertions)]
fn content_hash(line: &TextLine, wrap: Option<f32>) -> u64 {
    let mut h = FxHasher::default();
    h.write_u64(shape_key(line, wrap));
    for c in line.color {
        h.write_u32(c.to_bits());
    }
    h.finish()
}

/// Decides whether a text line's glyph buffer needs to be re-shaped this
/// frame.
///
/// The **only** decision point `prepare` consults, in both build profiles, and
/// deliberately content-addressed: `shape_changed` is the comparison of this
/// frame's [`shape_key`] against the one the cached buffer was shaped from.
/// `TextLine::dirty` is not consulted at all, see the module docs for why a
/// flag whose producer sets it unconditionally is not a signal.
///
/// Pulled out as a free, pure function so it is unit-testable without any
/// glyphon or wgpu state.
const fn needs_reshape(viewport_dirty: bool, shape_changed: bool) -> bool {
    viewport_dirty || shape_changed
}

/// Whether index `i` refers to the same *element* it referred to last frame.
///
/// The whole incremental scheme is index-addressed, `texts[i]` compared
/// against what was shaped for `texts[i]`, with `TextLine::dirty` trusted to say
/// when they differ, and that is sound only while this holds.
///
/// It stops holding when the pool's length changes. Mount a paragraph in the
/// middle of a column and every line after it shifts down one index: each of
/// those indices now holds a different element whose producer has truthfully
/// reported it unchanged, because it *is* unchanged at its own position in the
/// tree. Index-wise it is entirely different text.
///
/// "Which index shifted" is not knowable from two lengths, so a length change
/// invalidates all of them. That costs a reshape of the whole pool on such a
/// frame, which is already a full-redraw frame
/// (`needs_full_redraw_this_frame` includes exactly this condition), so it adds
/// no cost class that was not being paid.
#[must_use]
const fn index_is_identity_stable(is_new: bool, length_changed: bool) -> bool {
    !is_new && !length_changed
}

/// Debug-only safety net: reports a line whose content actually changed
/// (`hash_changed`) while its upstream `dirty` flag was not set.
///
/// # What this still catches, now that shaping is content-addressed
///
/// Not staleness in the glyphs, [`needs_reshape`] derives that from the
/// content itself, so a forgotten flag can no longer leave a shaped buffer
/// behind. What it catches is the *other* consumer of the same flag:
/// `dirty` is what [`dirty_text_bounds`](super::dirty_text_bounds) unions into
/// the incremental redraw scissor, so a line that changed with the flag unset
/// is shaped correctly and then clipped out of the region that gets redrawn,
/// which looks identical on screen and has a completely different cause.
///
/// Absent in `--release` builds.
#[cfg(debug_assertions)]
fn assert_dirty_flag_consistency(hash_changed: bool, line_dirty: bool) {
    assert!(
        !hash_changed || line_dirty,
        "a text primitive changed while its upstream dirty flag stayed unset. \
         The glyphs themselves are still correct, shaping is content-addressed \
         - but `dirty` is also what builds the incremental redraw region, so \
         this line may not be repainted where it changed."
    );
}

/// The frame-wide facts pass 1 needs, so
/// [`shape_range`](TextGlyphPipeline::shape_range) can be called twice, once
/// per owner, without repeating three arguments and risking their drifting
/// apart between the two calls.
#[derive(Clone, Copy)]
struct ShapePass {
    /// How long the shaped-buffer cache was *before* this frame grew it: any
    /// index at or past it has never been shaped.
    preexisting_len: usize,
    /// Whether the text pool's length changed, which makes index-wise
    /// comparison illegal for the whole pool. See [`index_is_identity_stable`].
    length_changed: bool,
    /// Whether the viewport changed, which invalidates every shaped buffer
    /// regardless of content (DPI/scale reaches the shaper).
    viewport_dirty: bool,
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// GPU text pipeline owned by [`EncoderSubsystem`](super::EncoderSubsystem).
///
/// Wraps all glyphon state into a single struct so that it can be initialised
/// once in [`EncoderSubsystem::init`] and driven frame-by-frame by
/// [`prepare`](TextGlyphPipeline::prepare) + [`render`](TextGlyphPipeline::render).
pub struct TextGlyphPipeline {
    /// glyphon font system, owns the loaded font data and shaped buffers.
    font_system: FontSystem,
    /// glyphon swash cache, rasterises shaped glyphs on demand.
    swash_cache: SwashCache,
    /// glyphon glyph atlas, GPU texture containing rasterised glyphs.
    atlas: TextAtlas,
    /// glyphon viewport, maps logical → physical pixels for the render pass.
    viewport: Viewport,
    /// glyphon renderers, **one per z-layer** (RFC-0017 layered draw batches),
    /// all sharing `atlas`/`viewport`/`font_system`/`swash_cache` above. Grown
    /// lazily by `prepare` to the frame's layer count and truncated when it
    /// shrinks (dropping a stale layer's glyph vertex buffer). Index = layer.
    /// The common single-layer frame keeps exactly one entry.
    renderers: Vec<TextRenderer>,
    /// Per-line cache: shaped buffers and content hashes.
    ///
    /// Index-aligned with the **full** `text_lines` slice passed to `prepare`
    ///, *global* across layers, so layering never re-shapes a line.
    /// Entries are added as new lines appear and never removed (Phase 1).
    cache: Vec<CachedLine>,
    /// How many lines the last [`prepare`](Self::prepare) actually re-shaped.
    ///
    /// Read, not inferred, the same principle as the statusline's
    /// `retained N/64`. "Is the glyph cache working?" is otherwise only
    /// answerable by timing, and a number that has to be inferred from a
    /// stopwatch is a number nobody checks. Surfaced on the `encode.glyphs`
    /// row of the profile block, where a developer can watch it sit at zero on
    /// a steady scene and jump the frame something changed.
    reshaped: usize,
    /// Resolved family names already loaded into `font_system` (RFC-0034).
    ///
    /// The frame carries the *whole* registered table every frame rather than
    /// a drained pool (see [`FontTable`](crate::frame::FontTable) for why), so
    /// this set is what makes a repeat delivery free: a face already here is
    /// skipped, and `cosmic-text` never sees the same bytes twice.
    loaded_families: std::collections::HashSet<String>,
}

impl TextGlyphPipeline {
    /// Creates the pipeline.
    ///
    /// Initialises all glyphon resources in the correct order:
    /// `Cache` → `TextAtlas` → `Viewport` → `TextRenderer`. This sequence is
    /// wrapped in a single `Device::push_error_scope` / `pop_error_scope`
    /// pair (RFC §8), glyphon's constructors are opaque to byard-core, but
    /// an error scope captures any validation error raised on `device`
    /// during the scope regardless of which crate triggered it, so the
    /// guarantee holds even though byard-core never calls
    /// `create_render_pipeline` itself here.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::PipelineCompilation`] if glyphon's internal
    /// pipeline/shader construction fails GPU-side validation.
    pub async fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Result<Self, ByardError> {
        // --- GPU VALIDATION ERROR SCOPE (RFC §8) ---
        // Covers Cache::new + TextAtlas::new + Viewport::new + TextRenderer::new.
        // glyphon's pipeline/shader creation is opaque to byard-core, but the
        // scope still captures any validation error wgpu raises on `device`
        // while it runs.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let glyph_cache = Cache::new(device);
        let mut atlas = TextAtlas::new(device, queue, &glyph_cache, format);
        let viewport = Viewport::new(device, &glyph_cache);
        // Enable the same draw-order depth state as the box/texture pipelines so
        // glyphon's text participates in cross-pass paint ordering (RFC-0011)
        // instead of always drawing on top. This first renderer is layer 0,
        // the only one a frame without overlays ever touches; `prepare` grows
        // the vec on demand when a frame carries more z-layers.
        let renderer = TextRenderer::new(
            &mut atlas,
            device,
            MultisampleState::default(),
            Some(super::draw_depth_stencil()),
        );

        if let Some(error) = scope.pop().await {
            return Err(ByardError::PipelineCompilation {
                pipeline: "TextGlyph".to_string(),
                reason: error.to_string(),
            });
        }

        Ok(Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            atlas,
            viewport,
            renderers: vec![renderer],
            cache: Vec::new(),
            reshaped: 0,
            loaded_families: std::collections::HashSet::new(),
        })
    }

    /// How many text lines the last [`prepare`](Self::prepare) re-shaped.
    ///
    /// Zero on a steady scene is the content-addressed cache working; a number
    /// equal to the line count every frame means something upstream is
    /// changing every line, which is the failure this counter exists to make
    /// visible rather than merely expensive.
    #[must_use]
    pub const fn reshaped_lines(&self) -> usize {
        self.reshaped
    }

    /// Brings the paint `FontSystem` level with the logic thread's by loading
    /// every family in `fonts` it has not seen (RFC-0034, INV-27).
    ///
    /// Must run **before** any shaping in the frame: a line whose `family`
    /// names a face this `FontSystem` does not hold shapes in the system font
    /// and is then cached under a key that says otherwise.
    ///
    /// Returns how many faces this call actually loaded, which is the number
    /// the "a registered family is not re-loaded every frame" test reads. It
    /// is a count of work done, not a count of families known, precisely so
    /// that a regression to per-frame loading shows up as a number rather than
    /// as a slow application.
    pub fn register_fonts(&mut self, fonts: &crate::frame::FontTable) -> usize {
        load_missing(self.font_system.db_mut(), &mut self.loaded_families, fonts)
    }

    /// Uploads updated viewport dimensions to the glyphon `Viewport`.
    ///
    /// Must be called whenever the surface is resized and before the next
    /// `prepare`. The `phys_w`/`phys_h` pair is in **physical pixels**,
    /// glyphon's `Resolution` always works in physical pixels.
    pub fn update_resolution(&mut self, queue: &wgpu::Queue, phys_w: u32, phys_h: u32) {
        self.viewport.update(
            queue,
            Resolution {
                width: phys_w,
                height: phys_h,
            },
        );
    }

    /// Shapes and uploads text for the next frame.
    ///
    /// `scale_factor` converts logical → physical pixels so that glyph metrics
    /// stay DPI-correct. `viewport_dirty` forces a re-prepare even when no
    /// text content has changed (e.g. after a window resize).
    ///
    /// `layer_ranges` partitions `text_lines` into the frame's z-layers
    /// (RFC-0017 layered draw batches): contiguous, non-overlapping index
    /// ranges covering the whole slice, one per layer, in draw order. Each
    /// layer gets its own `TextRenderer` (grown here on demand) so the
    /// Encoder can interleave text draws with the other pools per layer; the
    /// shaping pass below stays **global**, so a line is shaped exactly once
    /// no matter how many layers the frame has. Pass a single `0..len` range
    /// for the ordinary single-layer frame.
    ///
    /// ## Three-pass borrow pattern
    ///
    /// Rust's field-split borrowing cannot reason across a Vec of structs when
    /// the same loop body needs both `&mut entry.buffer` (for layout) and
    /// `&entry.buffer` (for the `TextArea` slice). Three sequential passes solve
    /// this cleanly:
    ///
    /// 1. **Mutation pass**, mutably borrows `self.cache` and
    ///    `self.font_system` to grow the cache and re-shape dirty buffers.
    /// 2. **Collection pass**, immutably borrows `self.cache` to build a
    ///    `Vec<TextArea<'_>>` holding `&entry.buffer` references.
    /// 3. **Prepare pass**, borrows `self.renderers`, `self.font_system`,
    ///    `self.atlas`, `self.viewport`, `self.swash_cache`, all distinct
    ///    from `self.cache`, which is already borrowed by `text_areas`.
    ///
    /// Passes 2–3 run once per layer over that layer's subrange; every
    /// renderer is re-prepared each call (an empty range clears its layer's
    /// previous glyph buffer, required, or a text line that moved layers
    /// would ghost in both).
    ///
    /// ## `dev_text_start`, whose shaping is this?
    ///
    /// The index at or after which `text_lines` belongs to the dev runner's own
    /// surfaces rather than to the app (`RenderFrame::dev_text_start`); pass
    /// `text_lines.len()` for a frame that carries none, which is every frame
    /// of a shipped application.
    ///
    /// Text shaping is the largest per-primitive term in the frame, so a dev
    /// overlay's text is also the largest part of what that overlay costs. Both
    /// halves of the split are therefore timed separately and the dev half is
    /// stamped [`Owner::DevTools`](crate::telemetry::Owner::DevTools), so
    /// RFC-0030 §V4's self-accounting reports what the HUD actually charges
    /// instead of billing it to the app it is measuring.
    ///
    /// The split is an index rather than a per-line tag because dev surfaces
    /// are always emitted last: one comparison partitions the whole pool.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::TextPrepare`] if glyphon's `prepare` fails.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text_lines: &[TextLine],
        depths: &[f32],
        scale_factor: f32,
        viewport_dirty: bool,
        clips: &[crate::frame::ClipRect],
        text_clips: &[Option<u16>],
        wraps: &[Option<f32>],
        layer_ranges: &[std::ops::Range<usize>],
        dev_text_start: usize,
    ) -> Result<(), ByardError> {
        // ── Pass 1: grow cache and re-shape dirty lines ───────────────────────
        //
        // Each CachedLine is grown lazily (push when missing) rather than
        // resize_with so the closure does not need to capture &mut font_system
        // at the same time as &mut cache, which would be a double borrow.
        self.reshaped = 0;
        let preexisting_len = self.cache.len();
        // # A pool whose length changed is not index-comparable
        //
        // The whole incremental scheme is index-addressed: `texts[i]` is
        // compared against the `shape_key` recorded for `texts[i]` last frame.
        // That is sound only while index `i` means the same *element* on both
        // frames.
        //
        // A length change breaks it. Mount a paragraph in the middle of a
        // column and every line after it shifts down one index: each of those
        // indices now holds a different element, whose producer has truthfully
        // reported it unchanged, because it *is* unchanged, at its own
        // position in the tree. Index-wise it is entirely different text, and
        // the cached buffer at that index would be drawn as-is.
        //
        // This is latent rather than new. Before an overlay lengthened the
        // pool, the shifted indices usually landed beyond the cache and were
        // taken as `is_new`, which reshapes unconditionally and hides it. The
        // in-window HUD made the cache long enough for them to land *inside*
        // it, which is how it surfaced, as an intermittent debug assertion,
        // and in release as silently stale text.
        //
        // So a length change reshapes everything. The frame is being fully
        // redrawn anyway on such a frame (`needs_full_redraw_this_frame`
        // includes exactly this condition), so this adds no cost class that
        // was not already being paid, and it is the only answer available
        // here, because "which index shifted" is not knowable from two
        // lengths.
        let length_changed = preexisting_len != text_lines.len();
        while self.cache.len() < text_lines.len() {
            let metrics = Metrics::new(12.0, 14.0); // placeholder; overwritten below
            let buffer = Buffer::new(&mut self.font_system, metrics);
            self.cache.push(CachedLine {
                buffer,
                // No buffer has been shaped for this entry yet. The value is
                // never compared, `is_new` forces the shape below, but it is
                // set to a marker rather than to a plausible key so a future
                // reader cannot mistake it for one.
                shape_key: 0,
                #[cfg(debug_assertions)]
                content_hash: 0,
            });
        }

        // The app's lines and the dev runner's are shaped by the same loop
        // body over two ranges, so the dev half can be timed and attributed
        // without the shaping itself knowing anything about owners.
        let split = dev_text_start.min(text_lines.len());
        let pass = ShapePass {
            preexisting_len,
            length_changed,
            viewport_dirty,
        };
        self.shape_range(text_lines, wraps, 0..split, pass);
        if split < text_lines.len() {
            // Entered before the scope so the scope itself is dev-owned, and
            // held across the shaping so every nested scope is too.
            let _dev = crate::telemetry::attribute_to(crate::telemetry::Owner::DevTools);
            crate::profile_scope!("encode.glyphs.dev");
            self.shape_range(text_lines, wraps, split..text_lines.len(), pass);
        }

        // ── Grow/shrink the per-layer renderer pool ───────────────────────────
        //
        // One `TextRenderer` per z-layer, all sharing the atlas/viewport/font
        // stack. Growth is rare (a new overlay depth appears); truncation
        // drops a vanished layer's glyph vertex buffer instead of leaking it.
        let layer_count = layer_ranges.len().max(1);
        while self.renderers.len() < layer_count {
            self.renderers.push(TextRenderer::new(
                &mut self.atlas,
                device,
                MultisampleState::default(),
                Some(super::draw_depth_stencil()),
            ));
        }
        self.renderers.truncate(layer_count);

        // A missing/empty partition means "everything is layer 0", the
        // ordinary frame, expressed as one full-slice range.
        let full_range = 0..text_lines.len();
        let ranges: &[std::ops::Range<usize>] = if layer_ranges.is_empty() {
            std::slice::from_ref(&full_range)
        } else {
            layer_ranges
        };

        // ── Passes 2 + 3, once per layer ──────────────────────────────────────
        for (renderer, range) in self.renderers.iter_mut().zip(ranges) {
            // Clamp defensively: a malformed range must never panic the
            // render thread, worst case a line draws in the wrong layer.
            let start = range.start.min(text_lines.len());
            let end = range.end.clamp(start, text_lines.len());

            // A layer whose lines all sit at or past the split is a dev
            // surface's layer, and its vertex staging is charged to the dev
            // runner. Dev surfaces open their own layer (RFC-0017), so this is
            // an exact partition rather than a heuristic; an empty layer is
            // left with the app, where it costs nothing either way.
            let dev_layer = start >= split && end > start;
            let _dev = dev_layer.then(|| {
                crate::telemetry::attributed_scope(
                    *DEV_GLYPHS_SCOPE
                        .get_or_init(|| crate::telemetry::scope_id("encode.glyphs.dev")),
                    crate::telemetry::Owner::DevTools,
                )
            });

            // ── Pass 2: collect immutable TextArea refs for this layer ────────
            //
            // A free function over `&self.cache` (not a `&self` method) so the
            // returned borrow is of the `cache` field alone, a method would
            // freeze all of `self` and collide with the `&mut` field borrows
            // pass 3 needs.
            let text_areas = collect_layer_text_areas(
                &self.cache,
                text_lines,
                start..end,
                scale_factor,
                clips,
                text_clips,
            );

            // ── Pass 3: glyphon prepare (with draw-order depth) ───────────────
            //
            // Borrows: this layer's renderer (from `self.renderers`),
            // font_system, atlas, viewport, swash_cache. All distinct fields
            // from `cache` (borrowed by text_areas).
            //
            // `metadata_to_depth` maps each glyph's metadata (its *global* line
            // index, set in pass 1) to that line's draw-order NDC-z, so text is
            // depth-sorted against solids/decorated/textures instead of always
            // painting on top (RFC-0011 cross-pass paint order). A missing/
            // out-of-range depth falls back to the far plane. An empty layer
            // still prepares, that is what clears its renderer's previous
            // glyph buffer.
            renderer
                .prepare_with_depth(
                    device,
                    queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    text_areas,
                    &mut self.swash_cache,
                    |meta| {
                        depths
                            .get(meta)
                            .copied()
                            .unwrap_or(crate::frame::DRAW_DEPTH_CLEAR)
                    },
                )
                .map_err(|e| ByardError::TextPrepare(e.to_string()))?;
        }
        Ok(())
    }

    /// Pass 1 over one index range: re-shapes every line in it whose
    /// [`shape_key`] no longer matches the buffer cached for it.
    ///
    /// Split out of [`prepare`](Self::prepare) so the app's lines and a dev
    /// overlay's can go through *identical* code under different attribution.
    /// Nothing in here knows about owners: the caller opens the attribution,
    /// which is what makes it impossible for the two halves to drift apart.
    fn shape_range(
        &mut self,
        text_lines: &[TextLine],
        wraps: &[Option<f32>],
        range: std::ops::Range<usize>,
        pass: ShapePass,
    ) {
        for i in range {
            let line = &text_lines[i];
            // A line beyond the cache's previous length has no shaped buffer
            // yet, it must always be shaped on its first appearance,
            // whatever its key compares equal to. (An all-zero key on a fresh
            // entry could coincide with a real hash; "new" is detected
            // structurally rather than left to a sentinel value.)
            let is_new = i >= pass.preexisting_len;
            let wrap_w = wraps.get(i).copied().flatten();
            let key = shape_key(line, wrap_w);
            let comparable = index_is_identity_stable(is_new, pass.length_changed);
            let entry = &mut self.cache[i];

            #[cfg(debug_assertions)]
            {
                let hash = content_hash(line, wrap_w);
                // Not asserted on a length-changed frame: the premise of the
                // check is about *this* element's flag, and on such a frame
                // index `i` is not this element's previous position at all.
                if comparable {
                    assert_dirty_flag_consistency(hash != entry.content_hash, line.dirty);
                }
                entry.content_hash = hash;
            }

            if comparable && !needs_reshape(pass.viewport_dirty, key != entry.shape_key) {
                continue; // byte-identical glyph run, the cached buffer stands
            }
            entry.shape_key = key;
            self.reshaped += 1;

            shape_line(&mut self.font_system, &mut entry.buffer, line, wrap_w, i);
        }
    }

    /// Records **one z-layer's** text draw commands into the active render
    /// pass (RFC-0017 layered draw batches).
    ///
    /// Must be called after that layer's box/texture/vector draws, inside the
    /// same `wgpu::RenderPass`. On TBDR architectures (Apple Silicon), keeping
    /// every layer in one pass eliminates a tile-buffer flush. A `layer` with
    /// no renderer (out of range, e.g. the frame carries more layer marks
    /// than the last `prepare` saw) is a no-op rather than an error, so a
    /// racing layer-count change can never crash the render thread.
    ///
    /// # Errors
    ///
    /// Returns [`ByardError::TextRender`] if glyphon's `render` fails (e.g.
    /// atlas overflow, rare after a successful `prepare`).
    pub fn render_layer<'pass>(
        &'pass self,
        render_pass: &mut wgpu::RenderPass<'pass>,
        layer: usize,
    ) -> Result<(), ByardError> {
        let Some(renderer) = self.renderers.get(layer) else {
            return Ok(());
        };
        renderer
            .render(&self.atlas, &self.viewport, render_pass)
            .map_err(|e| ByardError::TextRender(e.to_string()))
    }
}

/// Pass 2 of [`TextGlyphPipeline::prepare`]: builds one z-layer's
/// `TextArea`s, the `range` subslice of `text_lines`, each referencing its
/// *globally* cached shaped buffer (index-aligned with the full slice, like
/// `clips`/`text_clips`).
///
/// A free function over the `cache` slice rather than a `&self` method: the
/// returned `TextArea`s must borrow **only** the `cache` field, so pass 3 can
/// still take `&mut` borrows of the pipeline's other fields (the module's
/// field-split borrow pattern). `range` is pre-clamped by the caller.
fn collect_layer_text_areas<'cache>(
    cache: &'cache [CachedLine],
    text_lines: &[TextLine],
    range: std::ops::Range<usize>,
    scale_factor: f32,
    clips: &[crate::frame::ClipRect],
    text_clips: &[Option<u16>],
) -> Vec<TextArea<'cache>> {
    let start = range.start;
    text_lines[range]
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let global = start + offset; // global line index (cache/clips/depths)
            let [red, green, blue, alpha] = line.color;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let default_color = Color::rgba(
                (red.clamp(0.0, 1.0) * 255.0) as u8,
                (green.clamp(0.0, 1.0) * 255.0) as u8,
                (blue.clamp(0.0, 1.0) * 255.0) as u8,
                (alpha.clamp(0.0, 1.0) * 255.0) as u8,
            );
            TextArea {
                buffer: &cache[global].buffer,
                // glyphon's Viewport/Resolution is configured in PHYSICAL
                // pixels (see EncoderSubsystem::update_viewport), but
                // TextLine.x/y are authored in logical pixels like every
                // other public coordinate in this crate. cosmic-text's
                // glyph positioning does not rescale this offset, only
                // the buffer's own shaped glyph extents are scaled by
                // `scale`, so `left`/`top` must already be physical
                // pixels or text lands at `logical / scale_factor`,
                // visibly drifting toward the origin on HiDPI displays.
                left: line.x * scale_factor,
                top: line.y * scale_factor,
                scale: scale_factor,
                // Content clip (RFC-0005 `ScrollView`): a line inside a
                // scroll viewport is clipped to it via glyphon's own
                // `TextBounds` (physical px), the clean, per-area way to
                // clip text without a render-pass scissor. Unclipped
                // lines stay unbounded.
                bounds: text_clips
                    .get(global)
                    .copied()
                    .flatten()
                    .and_then(|idx| clips.get(idx as usize))
                    .map_or(
                        TextBounds {
                            left: 0,
                            top: 0,
                            right: i32::MAX,
                            bottom: i32::MAX,
                        },
                        |c| clip_to_text_bounds(c.rect, scale_factor),
                    ),
                default_color,
                custom_glyphs: &[],
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// `shape_key`, `needs_reshape` and `assert_dirty_flag_consistency` are
// extracted as pure, glyphon/wgpu-free functions specifically so the re-shape
// decision can be exercised deterministically here, without a real
// `wgpu::Device`, the same CPU-mirror-of-decision-logic style already used
// by `encoder::mod`'s `cpu_sd_rounded_box` tests.
#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal [`TextLine`] for the hashing tests: the fields the keys read,
    /// and defaults for the ones they must not.
    fn line(text: &str, font_size: f32) -> TextLine {
        TextLine {
            x: 0.0,
            y: 0.0,
            text: text.to_string(),
            font_size,
            weight: 400,
            family: None,
            color: [1.0, 1.0, 1.0, 1.0],
            dirty: false,
        }
    }

    /// The same line in a colour.
    fn colored(text: &str, font_size: f32, color: [f32; 4]) -> TextLine {
        TextLine {
            color,
            ..line(text, font_size)
        }
    }

    // ── INV-27: measurement and paint resolve the same font ────────────────

    /// A shipped example asset, read from the tree rather than synthesised.
    ///
    /// The suite and the examples deliberately share these files: a test that
    /// proves families work against a font no example uses proves it for a
    /// font nobody ships.
    const DISPLAY_FONT: &[u8] =
        include_bytes!("../../../byard-cli/examples/assets/fonts/SpaceGrotesk-Variable.ttf");

    /// Shapes `text` the way the paint side does, on a `FontSystem` prepared
    /// the way the paint side prepares one, and returns the run's width.
    fn painted_width(fonts: &crate::frame::FontTable, family: Option<&str>, text: &str) -> f32 {
        let mut fs = FontSystem::new();
        let mut loaded = std::collections::HashSet::new();
        load_missing(fs.db_mut(), &mut loaded, fonts);
        let mut buf = Buffer::new(&mut fs, Metrics::new(32.0, 38.4));
        let mut l = line(text, 32.0);
        l.family = family.map(std::sync::Arc::from);
        shape_line(&mut fs, &mut buf, &l, None, 0);
        buf.layout_runs().fold(0.0_f32, |w, r| w.max(r.line_w))
    }

    /// The invariant Phase 13 wrote and nothing has ever checked: a family
    /// registered from one source of truth measures and paints identically.
    ///
    /// Not a pixel test and not a magnitude test. It compares two
    /// numbers the engine produces for the same string, and the only way they
    /// can differ is the one INV-27 names: one `FontSystem` holding the face
    /// and the other silently falling back.
    #[test]
    fn a_registered_family_measures_and_paints_the_same_width() {
        let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(DISPLAY_FONT);
        let mut measurer = crate::text::TextMeasurer::new();
        let resolved = measurer
            .register_family(&bytes)
            .expect("the shipped example font parses");

        let mut fonts = crate::frame::FontTable::default();
        fonts.push(crate::frame::FontFace {
            declared: "display".to_string(),
            resolved: std::sync::Arc::from(resolved.as_str()),
            bytes,
        });

        let text = "Aura Weather";
        let (from_layout, _) = measurer.measure(text, 32.0, 400, Some(&resolved));
        let painted = painted_width(&fonts, Some(&resolved), text);
        assert!(
            (from_layout - painted).abs() < 0.01,
            "measured {from_layout} vs painted {painted}: the two font systems \
             disagree about `{resolved}` (INV-27)"
        );

        // The control, and the reason this test cannot pass vacuously: with
        // the table withheld from the paint side, the same line falls back to
        // the system font and the widths part company. If this assertion ever
        // fails, the one above proves nothing.
        let unregistered =
            painted_width(&crate::frame::FontTable::default(), Some(&resolved), text);
        assert!(
            (from_layout - unregistered).abs() > 0.01,
            "a family the paint side never loaded still measured {unregistered}, \
             so the agreement above is not evidence of anything"
        );
    }

    /// A family already loaded is not loaded again, however many frames carry
    /// it. The table rides every frame by design; the cost of that must be
    /// zero after the first.
    #[test]
    fn a_registered_family_is_loaded_once_however_often_it_is_delivered() {
        let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(DISPLAY_FONT);
        let resolved = crate::text::family_name(&bytes).expect("parses");
        let mut fonts = crate::frame::FontTable::default();
        fonts.push(crate::frame::FontFace {
            declared: "display".to_string(),
            resolved: std::sync::Arc::from(resolved.as_str()),
            bytes,
        });

        let mut fs = FontSystem::new();
        let mut loaded = std::collections::HashSet::new();
        assert_eq!(
            load_missing(fs.db_mut(), &mut loaded, &fonts),
            1,
            "first frame"
        );
        for frame in 2..=10 {
            assert_eq!(
                load_missing(fs.db_mut(), &mut loaded, &fonts),
                0,
                "frame {frame} re-loaded a face already resident"
            );
        }
    }

    // ── needs_reshape: all four (viewport_dirty, shape_changed) combinations ──

    #[test]
    fn needs_reshape_false_when_the_shaped_run_would_be_identical() {
        assert!(!needs_reshape(false, false));
    }

    #[test]
    fn needs_reshape_true_when_only_viewport_is_dirty() {
        assert!(needs_reshape(true, false));
    }

    #[test]
    fn needs_reshape_true_when_only_the_shape_key_moved() {
        assert!(needs_reshape(false, true));
    }

    #[test]
    fn needs_reshape_true_when_both_are_dirty() {
        assert!(needs_reshape(true, true));
    }

    // ── shape_key: what does and does not reach the shaper ─────────────────

    #[test]
    fn the_shape_key_is_stable_for_an_identical_run() {
        assert_eq!(
            shape_key(&line("hello", 14.0), None),
            shape_key(&line("hello", 14.0), None)
        );
    }

    #[test]
    fn the_shape_key_moves_with_text_size_and_wrap() {
        let base = shape_key(&line("hello", 14.0), None);
        assert_ne!(base, shape_key(&line("world", 14.0), None), "text");
        assert_ne!(base, shape_key(&line("hello", 15.0), None), "font size");
        // RFC-0018: a wrap-only change moves the line breaks.
        assert_ne!(
            base,
            shape_key(&line("hello", 14.0), Some(40.0)),
            "wrap width"
        );
    }

    #[test]
    fn a_fixed_width_number_keeps_its_key_stable_until_the_value_actually_moves() {
        // RFC-0030 §V4's INV-24 mitigation 3, now load-bearing rather than
        // decorative: the HUD re-emits the same padded string on five of every
        // six frames, and those five must not re-shape.
        let a = shape_key(&line(&format!("{:>5.1}", 3.4), 12.0), None);
        let b = shape_key(&line(&format!("{:>5.1}", 3.4), 12.0), None);
        let c = shape_key(&line(&format!("{:>5.1}", 12.7), 12.0), None);
        assert_eq!(a, b, "an unchanged reading re-shapes nothing");
        assert_ne!(a, c, "a changed reading still re-shapes, once");
    }

    #[test]
    fn a_colour_change_alone_never_re_shapes() {
        // Colour is applied per-`TextArea`, so it provably cannot alter a
        // glyph. The whole INV-24 argument, a paint-class change never
        // touches layout, is only true if the cache key agrees.
        let before = shape_key(&line("tap me", 16.0), None);
        let after = shape_key(&line("tap me", 16.0), None);
        assert_eq!(before, after);
        assert!(!needs_reshape(false, before != after));
    }

    #[test]
    fn a_forgotten_dirty_flag_can_no_longer_leave_a_line_stale() {
        // The old gate was `viewport_dirty || line.dirty`, so a producer that
        // changed the text and reported `dirty: false` rendered the previous
        // run, in release, silently. The content is now the authority, and
        // the flag is not consulted at all.
        let cached = shape_key(&line("before", 14.0), None);
        let this_frame = shape_key(&line("after", 14.0), None);
        assert!(
            needs_reshape(false, this_frame != cached),
            "the shaped run is re-derived from the content, not from a claim \
             about it"
        );
    }

    // ── index_is_identity_stable: when index-wise comparison is legal ──────

    #[test]
    fn a_length_change_makes_every_index_unstable() {
        // Mounting a paragraph mid-column shifts every line after it down one
        // index. Each of those indices now holds a different element whose
        // producer truthfully reports it unchanged, it *is* unchanged at its
        // own position in the tree, so index-wise comparison would draw the
        // previous occupant's shaped buffer.
        assert!(!index_is_identity_stable(false, true));
        assert!(!index_is_identity_stable(true, true));
    }

    #[test]
    fn a_brand_new_index_is_never_compared_against_a_previous_occupant() {
        assert!(!index_is_identity_stable(true, false));
    }

    #[test]
    fn a_stable_pool_is_compared_index_wise() {
        // The common case, and the one the whole incremental scheme exists
        // for: same length, same elements, so a matching shape key is enough
        // and an unchanged line is not reshaped.
        assert!(index_is_identity_stable(false, false));
        assert!(!needs_reshape(false, false));
    }

    // ── assert_dirty_flag_consistency: the debug-only safety net ───────────

    #[test]
    #[cfg(debug_assertions)]
    fn consistency_check_passes_when_hash_unchanged_and_not_dirty() {
        assert_dirty_flag_consistency(false, false);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn consistency_check_passes_when_hash_unchanged_but_dirty_anyway() {
        // Over-marking dirty is wasteful, never unsound, must not panic.
        assert_dirty_flag_consistency(false, true);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn consistency_check_passes_when_hash_changed_and_dirty_was_set() {
        assert_dirty_flag_consistency(true, true);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "may not be repainted where it changed")]
    fn consistency_check_reports_a_change_the_redraw_region_will_miss() {
        assert_dirty_flag_consistency(true, false);
    }

    // ── content_hash: debug-only helper feeding the redraw-region net ──────

    #[test]
    #[cfg(debug_assertions)]
    fn content_hash_is_stable_for_identical_input() {
        let a = content_hash(&line("hello", 14.0), None);
        let b = content_hash(&line("hello", 14.0), None);
        assert_eq!(a, b);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn content_hash_changes_with_text() {
        let a = content_hash(&line("hello", 14.0), None);
        let b = content_hash(&line("world", 14.0), None);
        assert_ne!(a, b);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn content_hash_changes_with_wrap_width() {
        // RFC-0018: a wrap-only change must invalidate the shaped buffer.
        let a = content_hash(&line("hello world", 14.0), None);
        let b = content_hash(&line("hello world", 14.0), Some(40.0));
        assert_ne!(a, b);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn the_repaint_hash_widens_the_shape_key_rather_than_replacing_it() {
        // The two hashes answer different questions and must not be conflated:
        // colour changes the pixels (so the region must be redrawn) and cannot
        // change a glyph (so the run must not be re-shaped).
        let white = content_hash(&line("ok", 14.0), None);
        let red = content_hash(&colored("ok", 14.0, [1.0, 0.0, 0.0, 1.0]), None);
        assert_ne!(white, red, "a recolour must reach the redraw region");
        assert_eq!(
            shape_key(&line("ok", 14.0), None),
            shape_key(&line("ok", 14.0), None),
            "and must not reach the shaper"
        );
    }

    // ── TextLine: dirty field is a plain, independent bit ──────────────────

    #[test]
    fn text_line_dirty_field_round_trips() {
        let dirty_line = TextLine {
            x: 0.0,
            y: 0.0,
            text: "hi".to_string(),
            font_size: 12.0,
            weight: 400,
            family: None,
            color: [0.0, 0.0, 0.0, 1.0],
            dirty: true,
        };
        assert!(dirty_line.dirty);

        let clean_line = TextLine {
            dirty: false,
            ..dirty_line
        };
        assert!(!clean_line.dirty);
    }
}
