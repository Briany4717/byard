//! Glyph-accurate text measurement for layout.
//!
//! [`TextMeasurer`] shapes a string with the same `glyphon`/`cosmic-text`
//! engine the renderer uses (`encoder::text_glyph`), so the intrinsic size a
//! `Text`/`Button` reports to Taffy matches what is actually drawn, which is
//! what lets text be aligned and justified correctly within its box (rather
//! than estimated from a character count). The owning `FontSystem` is created
//! once (it loads the system fonts) and reused for every measurement.

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};
use std::collections::HashMap;
use std::sync::Arc;

/// Everything that changes a shaped size, and therefore everything the
/// measurement cache is keyed on: the string, its size and wrap width as bit
/// patterns, its weight, and its family.
///
/// Named rather than written inline because leaving a member out is the whole
/// failure mode: a key that omits one input hands the second caller the first
/// one's answer, silently, for a string that was never measured.
type MeasureKey = (String, u32, u32, u16, Option<Arc<str>>);

/// Measures shaped text sizes, reusing one [`FontSystem`].
///
/// Shaping a string through `cosmic-text` (`Shaping::Advanced`) is expensive,
/// several hundred microseconds for a short label. Because layout re-measures
/// every `Text`/`Button` on **every** render tick (and most strings are
/// unchanged frame to frame), results are memoised in a
/// `(text, font_size, wrap_width, weight)` cache
/// so a steady-state tick re-shapes nothing. This is the single biggest
/// factor in per-tick cost on a continuously-redrawing logic thread.
pub struct TextMeasurer {
    font_system: FontSystem,
    /// `(text, font_size.to_bits(), wrap_width.to_bits(), weight, family)` →
    /// shaped `(width, height)`. The wrap width is part of the key because it
    /// changes the line breaks and thus the measured size (RFC-0018); the
    /// family is, because two faces set the same string to different widths
    /// and a key without it hands the second caller the first one's answer
    /// (RFC-0034 §Engine side).
    cache: HashMap<MeasureKey, (f32, f32)>,
}

impl Default for TextMeasurer {
    fn default() -> Self {
        Self::new()
    }
}

/// A text-measurement sink the layout atlas calls back into during layout so a
/// `Text` leaf can size itself to the width its parent offers (RFC-0005 default
/// wrap): given a candidate `max_width`, it returns the shaped `(width, height)`.
/// Decouples the atlas from the concrete shaper (`TextMeasurer`) and keeps
/// measurement cached and single-sourced.
pub trait TextSizer {
    /// Shaped `(width, height)` of `text` at `font_size`, `weight` and
    /// `family`, wrapped to `max_width` logical px when `Some` (`None` =
    /// natural single line).
    ///
    /// Weight and family are part of the question, not paint details: a bold
    /// run is wider than a regular one at the same size, and two faces set the
    /// same string to different widths, so layout that measured without them
    /// would size every heading to the wrong box.
    fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
        weight: u16,
        family: Option<&str>,
    ) -> (f32, f32);
}

impl TextSizer for TextMeasurer {
    fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
        weight: u16,
        family: Option<&str>,
    ) -> (f32, f32) {
        self.measure_wrapped(text, font_size, max_width, weight, family)
    }
}

impl TextMeasurer {
    /// Creates a measurer with a fresh font system (loads system fonts once).
    #[must_use]
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            cache: HashMap::new(),
        }
    }

    /// Returns the shaped `(width, height)` of `text` at `font_size` logical
    /// pixels, using a `1.2×` line height. Width is the widest laid-out line;
    /// height is `lines × line_height`. Empty text still reports one line's
    /// height so an empty label keeps its baseline.
    #[must_use]
    pub fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        weight: u16,
        family: Option<&str>,
    ) -> (f32, f32) {
        self.measure_wrapped(text, font_size, None, weight, family)
    }

    /// Like [`measure`](Self::measure), but bounds shaping to `max_width` logical
    /// pixels when `Some`, so the text wraps onto multiple lines and the returned
    /// height reflects the wrapped line count (RFC-0018 text wrap). `None` is the
    /// natural single-line measurement.
    #[must_use]
    pub fn measure_wrapped(
        &mut self,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
        weight: u16,
        family: Option<&str>,
    ) -> (f32, f32) {
        // Weight and family are part of the key, not afterthoughts: a bold run
        // is wider than a regular one at the same size, and two faces set the
        // same string to different widths, so a key without them would hand
        // the second caller the first one's width and lay the text out to a
        // measurement of a different face.
        let key = (
            text.to_string(),
            font_size.to_bits(),
            max_width.map_or(u32::MAX, f32::to_bits),
            weight,
            family.map(Arc::from),
        );
        if let Some(&hit) = self.cache.get(&key) {
            return hit;
        }
        let measured = self.shape(text, font_size, max_width, weight, family);
        self.cache.insert(key, measured);
        measured
    }

    /// Registers a font family's bytes with this measurer's `FontSystem` and
    /// returns the family name the face carries (RFC-0034 §Reference).
    ///
    /// The returned name, not the name the manifest declared, is what shaping
    /// must be given: `cosmic-text` matches `Family::Name` against the face's
    /// own family record. The caller carries it to the paint side so both
    /// `FontSystem`s resolve the identical string (INV-27).
    ///
    /// Returns `None` if the bytes are not a font this build can parse; the
    /// caller turns that into a diagnostic naming the file (INV-4).
    pub fn register_family(&mut self, bytes: &Arc<[u8]>) -> Option<String> {
        register_into(self.font_system.db_mut(), bytes)
    }

    /// Shapes `text` through `cosmic-text` and returns its `(width, height)`,
    /// optionally bounded to `max_width` so it wraps.
    fn shape(
        &mut self,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
        weight: u16,
        family: Option<&str>,
    ) -> (f32, f32) {
        let line_height = font_size * 1.2;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, line_height));
        // `None` measures the natural single-line width; `Some(w)` wraps to `w`.
        buffer.set_size(&mut self.font_system, max_width, None);
        buffer.set_text(
            &mut self.font_system,
            text,
            &Attrs::new()
                .family(family_of(family))
                .weight(glyphon::Weight(weight)),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let (width, lines) = buffer
            .layout_runs()
            .fold((0.0_f32, 0u32), |(w, n), run| (w.max(run.line_w), n + 1));
        #[allow(clippy::cast_precision_loss)]
        let height = lines.max(1) as f32 * line_height;
        (width, height)
    }
}

/// The `cosmic-text` family selector for an optional resolved family name.
///
/// One function rather than two call sites, because the measurement and the
/// paint path must make this choice identically or INV-27 is violated by
/// construction. `None` is the system sans-serif, which is the documented
/// fallback when nothing declares a family.
#[must_use]
pub fn family_of(family: Option<&str>) -> Family<'_> {
    family.map_or(Family::SansSerif, Family::Name)
}

/// The family name a font file carries, without registering it anywhere
/// (RFC-0034).
///
/// Used by the manifest loader to answer two questions in one place: are these
/// bytes a font this build can parse, and what will both `FontSystem`s call
/// it? `None` means unparsable, which the caller reports as a diagnostic
/// naming the file (INV-4) rather than letting it surface later as text in the
/// wrong face.
#[must_use]
pub fn family_name(bytes: &Arc<[u8]>) -> Option<String> {
    register_into(&mut glyphon::fontdb::Database::new(), bytes)
}

/// Loads `bytes` into `db` and returns the face's own family name.
///
/// Shared by the measurement `FontSystem` here and the paint one in
/// `encoder::text_glyph`, so a family cannot be loaded into one of them by a
/// slightly different route than the other (INV-27).
///
/// # The weight axis has to be declared, not just supported
///
/// A variable font arrives as **one** face whose recorded weight is its
/// default instance, 300 for Space Grotesk. `cosmic-text` picks a family by
/// filtering candidates down to those whose recorded weight is *exactly* the
/// weight asked for (`font_weight_diff == 0`) before it looks at family names
/// at all, so `#[font: display, weight: bold]` finds no candidate, silently
/// falls back to the system font, and the family looks like it does nothing.
/// That is the failure this project has already paid for twice under other
/// names, and it is invisible: the text still renders.
///
/// So every weight the axis actually covers is registered as its own face
/// record, all sharing this one blob. `Font::new` then sets the `wght` axis to
/// the weight it was asked for, which is the whole point of a variable font.
/// A static font declares nothing extra and keeps exactly the one weight it
/// really has, so asking a Regular file for bold still misses rather than
/// lying about it.
#[must_use]
pub fn register_into(db: &mut glyphon::fontdb::Database, bytes: &Arc<[u8]>) -> Option<String> {
    let ids = db.load_font_source(glyphon::fontdb::Source::Binary(Arc::new(bytes.clone())));
    // A collection can carry several faces; they share a family, so the first
    // one names the family for all of them.
    let id = *ids.first()?;
    let info = db.face(id)?.clone();
    let family = info.families.first().map(|(name, _)| name.clone())?;
    for weight in axis_weights(bytes) {
        if weight == info.weight {
            continue; // the face already stands at this weight
        }
        db.push_face_info(glyphon::fontdb::FaceInfo {
            // `push_face_info` assigns the real id; this one is a placeholder
            // it overwrites.
            id,
            weight,
            ..info.clone()
        });
    }
    Some(family)
}

/// The CSS-axis weights a variable font's `wght` axis covers, in 100 steps.
///
/// Empty for a static font, which is what keeps a Regular file from claiming
/// to be a Bold one. Clamped to the axis's own range, so a face that stops at
/// 700 does not advertise 800.
fn axis_weights(bytes: &[u8]) -> Vec<glyphon::fontdb::Weight> {
    let Ok(face) = ttf_parser::Face::parse(bytes, 0) else {
        return Vec::new();
    };
    let Some(axis) = face
        .variation_axes()
        .into_iter()
        .find(|a| a.tag == ttf_parser::Tag::from_bytes(b"wght"))
    else {
        return Vec::new();
    };
    (1..=9_u16)
        .map(|step| step * 100)
        .filter(|w| {
            let w = f32::from(*w);
            w >= axis.min_value && w <= axis.max_value
        })
        .map(glyphon::fontdb::Weight)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two shipped example faces, read from the tree so the suite and the
    /// examples are proving the same files work.
    const DISPLAY: &[u8] =
        include_bytes!("../../byard-cli/examples/assets/fonts/SpaceGrotesk-Variable.ttf");
    const BODY: &[u8] =
        include_bytes!("../../byard-cli/examples/assets/fonts/Manrope-Variable.ttf");

    /// A variable font must answer to every weight on its axis, not only to
    /// its default instance.
    ///
    /// This is the shape of a defect that renders perfectly: `cosmic-text`
    /// discards candidate faces whose recorded weight differs from the one
    /// asked for *before* it compares family names, so a display face whose
    /// default instance is 300 is simply not there at `weight: bold`, and the
    /// text appears in the system font with nothing to say it went wrong.
    #[test]
    fn a_variable_family_answers_at_every_weight_on_its_axis() {
        let mut m = TextMeasurer::new();
        let family = m
            .register_family(&Arc::from(DISPLAY))
            .expect("the shipped display face parses");
        // The fallback the family is being distinguished from. Measured with
        // no family at all, which is the exact thing a failed match produces.
        let (fallback, _) = m.measure("Handgloves", 40.0, 700, None);
        let (bold, _) = m.measure("Handgloves", 40.0, 700, Some(&family));
        assert!(
            (bold - fallback).abs() > 0.01,
            "`{family}` at weight 700 measured {bold}, the same as no family at \
             all: the axis weight never matched and the face was skipped"
        );
    }

    /// A static font keeps the one weight it has. Declaring nine would make a
    /// Regular file answer to `bold` and quietly draw regular, which is a
    /// worse failure than not matching at all.
    #[test]
    fn a_font_with_no_weight_axis_declares_no_extra_weights() {
        // A file that is not a font at all stands in for "no `fvar`": the
        // question `axis_weights` answers is whether the bytes declare an
        // axis, and bytes that declare nothing must yield nothing.
        assert!(axis_weights(b"not a font at all").is_empty());
    }

    /// The axis is reported from the font, not assumed. Both shipped faces
    /// cover the four keyword weights the language exposes.
    #[test]
    fn the_declared_weights_come_from_the_font_and_cover_the_keywords() {
        for bytes in [DISPLAY, BODY] {
            let weights: Vec<u16> = axis_weights(bytes).into_iter().map(|w| w.0).collect();
            for kw in [300_u16, 400, 500, 700] {
                assert!(
                    weights.contains(&kw),
                    "a shipped variable face must cover {kw}, got {weights:?}"
                );
            }
            assert!(
                !weights.contains(&100),
                "no shipped face reaches 100, so none may claim it: {weights:?}"
            );
        }
    }

    /// Two different families measure differently. Difference, not
    /// magnitude: which one is wider is a fact about two typefaces, not about
    /// the engine.
    #[test]
    fn two_registered_families_measure_differently() {
        let mut m = TextMeasurer::new();
        let display = m.register_family(&Arc::from(DISPLAY)).expect("display");
        let body = m.register_family(&Arc::from(BODY)).expect("body");
        assert_ne!(display, body, "two files, two family names");
        let a = m.measure("Aura Weather", 28.0, 400, Some(&display));
        let b = m.measure("Aura Weather", 28.0, 400, Some(&body));
        assert_ne!(a, b, "`{display}` and `{body}` set the same string alike");
    }

    /// The family is part of the cache key. Two strings alike but for their
    /// family must not collide.
    #[test]
    fn the_family_is_part_of_the_measurement_cache_key() {
        let mut m = TextMeasurer::new();
        let display = m.register_family(&Arc::from(DISPLAY)).expect("display");
        let body = m.register_family(&Arc::from(BODY)).expect("body");
        let a = m.measure("collide?", 20.0, 400, Some(&display));
        assert_eq!(m.cache.len(), 1);
        let b = m.measure("collide?", 20.0, 400, Some(&body));
        assert_eq!(m.cache.len(), 2, "the second family got its own entry");
        assert_ne!(a, b, "and its own answer, not the first one's");
    }

    /// Bytes that are not a font resolve to no family, which is what lets the
    /// manifest turn the file into a diagnostic instead of a square box.
    #[test]
    fn bytes_that_are_not_a_font_resolve_to_nothing() {
        assert!(family_name(&Arc::from(&b"this is not a font"[..])).is_none());
    }

    #[test]
    fn wider_text_measures_wider() {
        let mut m = TextMeasurer::new();
        let (w_short, h) = m.measure("i", 16.0, 400, None);
        let (w_long, _) = m.measure("wwwwwwwwww", 16.0, 400, None);
        assert!(
            w_long > w_short,
            "more glyphs ⇒ wider: {w_short} vs {w_long}"
        );
        assert!(w_short > 0.0 && h > 0.0);
    }

    #[test]
    fn wrapping_to_a_width_makes_text_taller_and_narrower() {
        // RFC-0018: bounding the shaping width wraps a long line onto several,
        // so it measures taller and no wider than the bound.
        let mut m = TextMeasurer::new();
        let long = "the quick brown fox jumps over the lazy dog again and again";
        let (nat_w, nat_h) = m.measure(long, 16.0, 400, None);
        let (wrap_w, wrap_h) = m.measure_wrapped(long, 16.0, Some(120.0), 400, None);
        assert!(wrap_h > nat_h, "wrapped text is taller: {nat_h} → {wrap_h}");
        assert!(
            wrap_w <= 120.5 && wrap_w < nat_w,
            "wrapped width is bounded and narrower: nat {nat_w}, wrapped {wrap_w}"
        );
    }

    #[test]
    fn larger_font_is_taller() {
        let mut m = TextMeasurer::new();
        let (_, h_small) = m.measure("Ag", 12.0, 400, None);
        let (_, h_big) = m.measure("Ag", 48.0, 400, None);
        assert!(h_big > h_small);
    }

    #[test]
    fn repeated_measures_are_cached() {
        // Re-measuring an unchanged (text, size), which layout does on every
        // render tick, must hit the cache and return an identical result, so a
        // steady-state tick re-shapes nothing (the per-tick perf fix).
        let mut m = TextMeasurer::new();
        let first = m.measure("the counter is 0", 16.0, 400, None);
        assert_eq!(m.cache.len(), 1, "the result was memoised");
        let second = m.measure("the counter is 0", 16.0, 400, None);
        assert_eq!(first, second, "cache returns the same measurement");
        assert_eq!(m.cache.len(), 1, "no second shaping was performed");
        // A distinct size is a distinct key.
        let _ = m.measure("the counter is 0", 20.0, 400, None);
        assert_eq!(m.cache.len(), 2);
    }

    #[test]
    fn empty_text_keeps_one_line_height() {
        let mut m = TextMeasurer::new();
        let (w, h) = m.measure("", 16.0, 400, None);
        assert!(w.abs() < 1e-6, "empty text has zero width, got {w}");
        assert!(h > 0.0);
    }
}
