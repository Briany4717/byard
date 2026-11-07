//! Glyphs drawn with variable-font axes (RFC-0054).
//!
//! glyphon draws a line's glyphs from `cosmic-text`'s layout, and its glyph
//! cache is keyed by font, glyph, size and weight, with no room for any other
//! axis. A `Text` with `variations:` therefore does not go through that path.
//! It is **shaped** by `cosmic-text` exactly as before (layout, ligatures,
//! advances), but its glyphs are handed to glyphon as *custom glyphs*, one
//! quad each, placed from the layout run and rasterised here by swash with the
//! axes set. A line without `variations` never reaches this module.
//!
//! A custom glyph is identified by a `u16`. [`VariationGlyphs`] interns
//! `(font, weight, glyph, size, axis set)` to that id, so two axis settings can
//! never share an atlas slot, and a steady frame looks nothing up (the custom
//! glyphs of a line are built when it is shaped and cached with it).
//!
//! The quad is a cell around the glyph: a quarter of the size of padding on
//! each side, and from 1.25 sizes above the baseline to half a size below it,
//! room for an outline that a `wght` or `FILL` axis grows. Icons are the use;
//! a glyph that reaches past the cell is clipped to it.

use std::sync::Arc;

use glyphon::cosmic_text::Font;
use glyphon::{ContentType, CustomGlyph, RasterizeCustomGlyphRequest, RasterizedCustomGlyph};
use rustc_hash::FxHashMap;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::{Setting, Tag};

/// Axis settings, sorted by tag, values quantised to 1/16.
type Settings = Arc<[([u8; 4], f32)]>;

/// Padding left and right of the glyph, in sizes.
const PAD: f32 = 0.25;
/// How far the cell reaches above the baseline, in sizes.
const ABOVE: f32 = 1.25;
/// The cell's height, in sizes.
const HEIGHT: f32 = 1.75;

struct Entry {
    font: Arc<Font>,
    glyph: u16,
    weight: u16,
    /// Logical font size.
    size: f32,
    settings: Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    font: glyphon::fontdb::ID,
    weight: u16,
    glyph: u16,
    size_bits: u32,
    set: u32,
}

/// The interned glyphs of every line with `variations:` and the swash state
/// that draws them.
#[derive(Default)]
pub struct VariationGlyphs {
    sets: Vec<Settings>,
    ids: FxHashMap<Key, u16>,
    entries: Vec<Entry>,
    context: ScaleContext,
}

impl VariationGlyphs {
    /// The set equal to `axes` once quantised, interned.
    fn set_of(&mut self, axes: &[([u8; 4], f32)]) -> u32 {
        let quantised: Vec<([u8; 4], f32)> = axes
            .iter()
            .map(|&(t, v)| (t, (v * 16.0).round() / 16.0))
            .collect();
        let at = self
            .sets
            .iter()
            .position(|s| s.as_ref() == quantised.as_slice())
            .unwrap_or_else(|| {
                self.sets.push(quantised.into());
                self.sets.len() - 1
            });
        u32::try_from(at).unwrap_or(u32::MAX)
    }

    /// The id for one glyph at one setting, interning it on first sight.
    /// `None` once 65 535 distinct glyphs are interned: that glyph is not drawn.
    fn id_of(
        &mut self,
        font: &Arc<Font>,
        weight: u16,
        glyph: u16,
        size: f32,
        axes: &[([u8; 4], f32)],
    ) -> Option<u16> {
        let set = self.set_of(axes);
        let key = Key {
            font: font.id(),
            weight,
            glyph,
            size_bits: size.to_bits(),
            set,
        };
        if let Some(id) = self.ids.get(&key) {
            return Some(*id);
        }
        let id = u16::try_from(self.entries.len()).ok()?;
        self.entries.push(Entry {
            font: Arc::clone(font),
            glyph,
            weight,
            size,
            settings: Arc::clone(&self.sets[set as usize]),
        });
        self.ids.insert(key, id);
        Some(id)
    }

    /// Appends one custom glyph per laid-out glyph of `buffer` to `out`:
    /// the line's glyphs, drawn with `axes` (RFC-0054). `metadata` is the
    /// line's draw-order index.
    pub fn build(
        &mut self,
        font_system: &mut glyphon::FontSystem,
        buffer: &glyphon::Buffer,
        axes: &[([u8; 4], f32)],
        metadata: usize,
        out: &mut Vec<CustomGlyph>,
    ) {
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let Some(font) = font_system.get_font(glyph.font_id, glyph.font_weight) else {
                    continue;
                };
                let size = glyph.font_size;
                let Some(id) = self.id_of(&font, glyph.font_weight.0, glyph.glyph_id, size, axes)
                else {
                    continue;
                };
                let origin_x = glyph.x + glyph.x_offset * size;
                let baseline = run.line_y + glyph.y - glyph.y_offset * size;
                out.push(CustomGlyph {
                    id,
                    left: origin_x - PAD * size,
                    top: baseline - ABOVE * size,
                    width: glyph.w + 2.0 * PAD * size,
                    height: HEIGHT * size,
                    color: None,
                    snap_to_physical_pixel: true,
                    metadata,
                });
            }
        }
    }

    /// Draws one custom glyph: the outline at the entry's axes, placed in a
    /// bitmap the size of the cell glyphon asked for.
    pub fn rasterize(&mut self, req: RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph> {
        let entry = self.entries.get(usize::from(req.id))?;
        let font = entry.font.as_swash();
        // Every axis is set, the unnamed ones to their defaults, so nothing a
        // previous glyph left in the scale context can reach this one.
        let settings: Vec<Setting<f32>> = font
            .variations()
            .map(|axis| {
                let tag = axis.tag();
                let value = entry
                    .settings
                    .iter()
                    .find(|(t, _)| Tag::from_be_bytes(*t) == tag)
                    .map(|(_, v)| *v)
                    .or_else(|| {
                        (tag == Tag::from_be_bytes(*b"wght")).then(|| f32::from(entry.weight))
                    })
                    .unwrap_or_else(|| axis.default_value());
                Setting {
                    tag,
                    value: value.clamp(axis.min_value(), axis.max_value()),
                }
            })
            .collect();
        let mut scaler = self
            .context
            .builder(font)
            .size(entry.size * req.scale)
            .hint(false)
            .variations(settings)
            .build();
        let image = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ])
        .format(Format::Alpha)
        .render(&mut scaler, entry.glyph)?;

        let (content_type, bpp) = match image.content {
            Content::Color => (ContentType::Color, 4),
            Content::Mask | Content::SubpixelMask => (ContentType::Mask, 1),
        };
        let (width, height) = (usize::from(req.width), usize::from(req.height));
        let mut data = vec![0u8; width * height * bpp];
        // Where the glyph's origin sits in the cell, in physical pixels.
        #[allow(clippy::cast_possible_truncation)]
        let (ox, oy) = (
            (PAD * entry.size * req.scale).round() as i32,
            (ABOVE * entry.size * req.scale).round() as i32,
        );
        let (iw, ih) = (
            image.placement.width as usize,
            image.placement.height as usize,
        );
        for row in 0..ih {
            #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
            let y = oy - image.placement.top + row as i32;
            let Ok(y) = usize::try_from(y) else { continue };
            if y >= height {
                continue;
            }
            for col in 0..iw {
                #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
                let x = ox + image.placement.left + col as i32;
                let Ok(x) = usize::try_from(x) else { continue };
                if x >= width {
                    continue;
                }
                let src = (row * iw + col) * bpp;
                let dst = (y * width + x) * bpp;
                data[dst..dst + bpp].copy_from_slice(&image.data[src..src + bpp]);
            }
        }
        Some(RasterizedCustomGlyph { data, content_type })
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
mod tests {
    use super::*;
    use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

    const ICONS: &[u8] = include_bytes!("../../../byard-cli/examples/assets/fonts/ByardIcons.ttf");

    struct Rig {
        fs: FontSystem,
        family: String,
        glyphs: VariationGlyphs,
    }

    fn rig() -> Rig {
        let mut fs = crate::text::new_font_system();
        let family =
            crate::text::register_into(fs.db_mut(), &Arc::from(ICONS)).expect("the font parses");
        Rig {
            fs,
            family,
            glyphs: VariationGlyphs::default(),
        }
    }

    impl Rig {
        /// Shapes `favorite` at `weight`, builds its custom glyph with `axes`,
        /// and rasterises it at scale 1: `(glyph count, id, coverage)`.
        fn draw(&mut self, weight: u16, axes: &[([u8; 4], f32)]) -> (usize, u16, u64) {
            let mut b = Buffer::new(&mut self.fs, Metrics::new(48.0, 58.0));
            b.set_text(
                &mut self.fs,
                "favorite",
                &Attrs::new()
                    .family(Family::Name(&self.family))
                    .weight(glyphon::Weight(weight)),
                Shaping::Advanced,
                None,
            );
            b.shape_until_scroll(&mut self.fs, false);
            let mut out = Vec::new();
            self.glyphs.build(&mut self.fs, &b, axes, 7, &mut out);
            let g = out[0];
            let image = self
                .glyphs
                .rasterize(RasterizeCustomGlyphRequest {
                    id: g.id,
                    width: g.width.round() as u16,
                    height: g.height.round() as u16,
                    x_bin: glyphon::cosmic_text::SubpixelBin::Zero,
                    y_bin: glyphon::cosmic_text::SubpixelBin::Zero,
                    scale: 1.0,
                })
                .expect("rasterizes");
            let sum = image.data.iter().map(|&b| u64::from(b)).sum();
            (out.len(), g.id, sum)
        }
    }

    /// `FILL` reaches the glyphs, and two settings in one frame do not leak
    /// into each other: A, B, A rasterizes A the same both times.
    #[test]
    fn two_variation_sets_rasterize_apart_without_leaking() {
        let mut r = rig();
        let (n, open_id, a) = r.draw(400, &[(*b"FILL", 0.0)]);
        assert_eq!(n, 1, "`favorite` is one ligature glyph, one custom glyph");
        let (_, filled_id, b) = r.draw(400, &[(*b"FILL", 1.0)]);
        let (_, again_id, a_again) = r.draw(400, &[(*b"FILL", 0.0)]);
        assert_ne!(open_id, filled_id, "two sets never share an atlas slot");
        assert_eq!(open_id, again_id, "the same set is the same glyph");
        assert!(
            b > a * 12 / 10,
            "FILL 1 covers more than FILL 0: {a} vs {b}"
        );
        assert_eq!(a, a_again, "the second setting leaked into the first");
        let (_, _, h) = r.draw(400, &[(*b"FILL", 0.5)]);
        assert!(a < h && h < b, "FILL 0.5 is between: {a} {h} {b}");
    }

    /// Settings 1/16 apart share a glyph, so a moving axis makes a bounded
    /// number of them.
    #[test]
    fn axis_values_are_quantised_to_sixteenths() {
        let mut r = rig();
        let (_, a, _) = r.draw(400, &[(*b"FILL", 0.50)]);
        let (_, b, _) = r.draw(400, &[(*b"FILL", 0.51)]);
        let (_, c, _) = r.draw(400, &[(*b"FILL", 0.57)]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    /// `weight:` and a `wght` axis are one axis: the axis wins over the weight,
    /// and the same number either way draws the same glyph.
    #[test]
    fn a_wght_axis_and_the_weight_are_one_axis() {
        let mut r = rig();
        let (_, _, by_weight) = r.draw(700, &[]);
        let (_, _, by_axis) = r.draw(400, &[(*b"wght", 700.0)]);
        let (_, _, regular) = r.draw(400, &[]);
        assert_eq!(by_weight, by_axis);
        assert!(by_weight > regular, "{by_weight} vs {regular}");
    }
}
