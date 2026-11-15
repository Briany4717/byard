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
//!
//! # Variable fonts never reach glyphon's own cache
//!
//! This module is also where Byard works around a bug in swash
//! (dfrg/swash#149, unmerged and still present in 0.2.10). A swash
//! `ScaleContext` keeps one buffer of normalised variation coordinates for
//! every scaler it builds, `ScalerBuilder::new` does not clear it, and
//! `variations()` only writes the axes it is given. `cosmic-text` hands it
//! `wght` and nothing else, so a variable font drawn at a non-default weight
//! leaves its coordinates behind and the next variable font drawn through the
//! same `SwashCache` (whose context is private) picks them up on every other
//! axis. On macOS the system font has several axes, and text came out at stray
//! coordinates; `glyphon` re-rasterised cached glyphs when its atlas grew, and
//! the wider images cut ordinary text into slivers.
//!
//! crates.io ignores a `[patch]`, so Byard does not patch swash. Instead
//! **a line with any glyph from a font that has variation axes is drawn as
//! custom glyphs** ([`VariationGlyphs::is_variable_line`], used by
//! `TextGlyphPipeline::shape_range`) and rasterised by this module's own
//! context, which sets every axis explicitly (the requested `wght`, every other
//! axis at its default or the line's `variations:`) so it cannot leak either.
//! `cosmic-text`'s swash path is left with static fonts, whose coordinates do
//! not matter. The glyph's pixels and position are those `glyphon` would have
//! produced: same hinting, same subpixel bin, same colour sources, same size.
//!
//! The test `swash_still_leaks_variation_coordinates` below fails the day swash
//! stops leaking. Then delete the routing (`is_variable_line`, `build_text`,
//! and their use in `text_glyph.rs`) and that test, and let variable fonts go
//! through glyphon's cache again.

use std::sync::Arc;

use glyphon::cosmic_text::{CacheKeyFlags, Font, SubpixelBin};
use glyphon::{ContentType, CustomGlyph, RasterizeCustomGlyphRequest, RasterizedCustomGlyph};
use rustc_hash::FxHashMap;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform, Vector};
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
    /// Logical font size; for a text glyph, the size in physical pixels.
    size: f32,
    settings: Settings,
    /// Set for a glyph of ordinary text drawn here because its font is
    /// variable ([`VariationGlyphs::build_text`]).
    text: Option<TextCell>,
}

/// Where a text glyph's bitmap sits in the cell it is drawn into, relative to
/// the glyph's origin pixel, in physical pixels. Measured once, when the glyph
/// is first seen, from the glyph itself.
#[derive(Clone, Copy)]
struct TextCell {
    flags: CacheKeyFlags,
    /// Cell left edge, right of the origin.
    x0: i32,
    /// Cell top edge, above the origin.
    top: i32,
    w: u16,
    h: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    font: glyphon::fontdb::ID,
    weight: u16,
    glyph: u16,
    size_bits: u32,
    set: u32,
    /// `CacheKeyFlags` bits of a text glyph; `None` for a glyph with axes.
    text: Option<u32>,
}

/// The interned glyphs of every line with `variations:` and the swash state
/// that draws them.
#[derive(Default)]
pub struct VariationGlyphs {
    sets: Vec<Settings>,
    ids: FxHashMap<Key, u16>,
    entries: Vec<Entry>,
    context: ScaleContext,
    /// Whether each face has variation axes, asked once per face.
    variable: FxHashMap<glyphon::fontdb::ID, bool>,
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
            text: None,
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
            text: None,
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

    /// Whether any glyph of `buffer` is from a font with variation axes. Such a
    /// line is drawn by [`build_text`](Self::build_text), never by glyphon's
    /// own cache (see the module docs). A line of static fonts costs one map
    /// lookup per change of face, when it is shaped.
    pub fn is_variable_line(
        &mut self,
        font_system: &mut glyphon::FontSystem,
        buffer: &glyphon::Buffer,
    ) -> bool {
        let mut last = None;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                if last == Some(glyph.font_id) {
                    continue;
                }
                last = Some(glyph.font_id);
                let variable = if let Some(v) = self.variable.get(&glyph.font_id) {
                    *v
                } else {
                    let v = font_system
                        .get_font(glyph.font_id, glyph.font_weight)
                        .is_some_and(|f| f.as_swash().variations().len() > 0);
                    self.variable.insert(glyph.font_id, v);
                    v
                };
                if variable {
                    return true;
                }
            }
        }
        false
    }

    /// Appends one custom glyph per visible glyph of `buffer` to `out`, placed
    /// and drawn as glyphon would have drawn the buffer itself: `origin` is the
    /// text area's physical `(left, top)` and `scale` its scale. The glyph's
    /// pixel position, subpixel bin, hinting and size are cosmic-text's, and
    /// only the rasteriser differs. A glyph is placed from `origin`, so the
    /// result has to be built again when the line moves.
    pub fn build_text(
        &mut self,
        font_system: &mut glyphon::FontSystem,
        buffer: &glyphon::Buffer,
        origin: (f32, f32),
        scale: f32,
        metadata: usize,
        out: &mut Vec<CustomGlyph>,
    ) {
        for run in buffer.layout_runs() {
            // `glyphon` adds this to every glyph's own `y`.
            #[allow(clippy::cast_possible_truncation)]
            let line_y = (run.line_y * scale).round() as i32;
            for glyph in run.glyphs {
                let physical = glyph.physical(origin, scale);
                let key = physical.cache_key;
                let Some(id) = self.text_id_of(font_system, glyph, key.flags, key.font_size_bits)
                else {
                    continue;
                };
                let Some(cell) = self.entries[usize::from(id)].text else {
                    continue;
                };
                if cell.w == 0 || cell.h == 0 {
                    continue; // nothing to draw: a space
                }
                #[allow(clippy::cast_precision_loss)]
                let (cx, cy) = (
                    (physical.x + cell.x0) as f32 + key.x_bin.as_float(),
                    (line_y + physical.y - cell.top) as f32,
                );
                out.push(CustomGlyph {
                    id,
                    left: (cx - origin.0) / scale,
                    top: (cy - origin.1) / scale,
                    width: f32::from(cell.w) / scale,
                    height: f32::from(cell.h) / scale,
                    color: None,
                    snap_to_physical_pixel: false,
                    metadata,
                });
            }
        }
    }

    /// The id for one glyph of ordinary text, measured and interned on first
    /// sight. `None` once 65 535 distinct glyphs are interned.
    fn text_id_of(
        &mut self,
        font_system: &mut glyphon::FontSystem,
        glyph: &glyphon::cosmic_text::LayoutGlyph,
        flags: CacheKeyFlags,
        size_bits: u32,
    ) -> Option<u16> {
        let key = Key {
            font: glyph.font_id,
            weight: glyph.font_weight.0,
            glyph: glyph.glyph_id,
            size_bits,
            set: 0,
            text: Some(flags.bits()),
        };
        if let Some(id) = self.ids.get(&key) {
            return Some(*id);
        }
        let font = font_system.get_font(glyph.font_id, glyph.font_weight)?;
        let id = u16::try_from(self.entries.len()).ok()?;
        let mut entry = Entry {
            font,
            glyph: glyph.glyph_id,
            weight: glyph.font_weight.0,
            size: f32::from_bits(size_bits),
            settings: Arc::from([]),
            text: None,
        };
        // The glyph at its origin, to see how much room it needs. A subpixel
        // bin moves it right by less than a pixel, and nothing moves it up.
        let cell = match render_text(
            &mut self.context,
            &entry,
            flags,
            SubpixelBin::Zero,
            SubpixelBin::Zero,
        ) {
            Some(image) if image.placement.width > 0 && image.placement.height > 0 => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let (w, h) = (image.placement.width as i32, image.placement.height as i32);
                let (left, top) = (image.placement.left, image.placement.top);
                TextCell {
                    flags,
                    x0: left - 1,
                    top: top + 1,
                    w: u16::try_from(w + 3).ok()?,
                    h: u16::try_from(h + 2).ok()?,
                }
            }
            _ => TextCell {
                flags,
                x0: 0,
                top: 0,
                w: 0,
                h: 0,
            },
        };
        entry.text = Some(cell);
        self.entries.push(entry);
        self.ids.insert(key, id);
        Some(id)
    }

    /// Draws one custom glyph: the outline at the entry's axes, placed in a
    /// bitmap the size of the cell glyphon asked for.
    pub fn rasterize(&mut self, req: RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph> {
        let entry = self.entries.get(usize::from(req.id))?;
        if let Some(cell) = entry.text {
            let image = render_text(&mut self.context, entry, cell.flags, req.x_bin, req.y_bin)?;
            return Some(blit(&image, &req, -cell.x0, cell.top));
        }
        let font = entry.font.as_swash();
        // Every axis is set, the unnamed ones to their defaults, so nothing a
        // previous glyph left in the scale context can reach this one.
        let settings = all_axes(font, &entry.settings, entry.weight);
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
        // Where the glyph's origin sits in the cell, in physical pixels.
        #[allow(clippy::cast_possible_truncation)]
        let (ox, oy) = (
            (PAD * entry.size * req.scale).round() as i32,
            (ABOVE * entry.size * req.scale).round() as i32,
        );
        Some(blit(&image, &req, ox, oy))
    }
}

/// Scales a glyph of ordinary text the way `cosmic-text`'s `swash_image`
/// does, with every axis set so nothing can leak in.
fn render_text(
    context: &mut ScaleContext,
    entry: &Entry,
    flags: CacheKeyFlags,
    x_bin: SubpixelBin,
    y_bin: SubpixelBin,
) -> Option<swash::scale::image::Image> {
    let font = entry.font.as_swash();
    let settings = all_axes(font, &[], entry.weight);
    let mut scaler = context
        .builder(font)
        .size(entry.size)
        .hint(!flags.contains(CacheKeyFlags::DISABLE_HINTING))
        .variations(settings)
        .build();
    let offset = if flags.contains(CacheKeyFlags::PIXEL_FONT) {
        Vector::new(x_bin.as_float().round() + 1.0, y_bin.as_float().round())
    } else {
        Vector::new(x_bin.as_float(), y_bin.as_float())
    };
    Render::new(&[
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
    ])
    .format(Format::Alpha)
    .offset(offset)
    .transform(
        flags
            .contains(CacheKeyFlags::FAKE_ITALIC)
            .then(|| Transform::skew(Angle::from_degrees(14.0), Angle::from_degrees(0.0))),
    )
    .render(&mut scaler, entry.glyph)
}

/// Every axis of `font` as a setting: the ones in `named` at their value, the
/// weight at `weight`, the rest at their defaults, all clamped to the axis.
/// Setting all of them is what keeps a scale context's leftovers out.
fn all_axes(font: swash::FontRef<'_>, named: &[([u8; 4], f32)], weight: u16) -> Vec<Setting<f32>> {
    font.variations()
        .map(|axis| {
            let tag = axis.tag();
            let value = named
                .iter()
                .find(|(t, _)| Tag::from_be_bytes(*t) == tag)
                .map(|(_, v)| *v)
                .or_else(|| (tag == Tag::from_be_bytes(*b"wght")).then(|| f32::from(weight)))
                .unwrap_or_else(|| axis.default_value());
            Setting {
                tag,
                value: value.clamp(axis.min_value(), axis.max_value()),
            }
        })
        .collect()
}

/// Copies `image` into a bitmap the size of the requested cell, its origin at
/// `(ox, oy)` from the cell's top left.
fn blit(
    image: &swash::scale::image::Image,
    req: &RasterizeCustomGlyphRequest,
    ox: i32,
    oy: i32,
) -> RasterizedCustomGlyph {
    let (content_type, bpp) = match image.content {
        Content::Color => (ContentType::Color, 4),
        Content::Mask | Content::SubpixelMask => (ContentType::Mask, 1),
    };
    let (width, height) = (usize::from(req.width), usize::from(req.height));
    let mut data = vec![0u8; width * height * bpp];
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
    RasterizedCustomGlyph { data, content_type }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
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

    const BODY: &[u8] =
        include_bytes!("../../../byard-cli/examples/assets/fonts/Manrope-Variable.ttf");

    /// A line of ordinary text drawn as custom glyphs lands on the same pixels
    /// glyphon's own cache would have drawn: same bitmap, same place, at every
    /// subpixel bin, size and scale, so routing a variable font here changes
    /// nothing a reader can see.
    #[test]
    fn text_drawn_as_custom_glyphs_matches_glyphons_pixels() {
        use glyphon::SwashCache;
        const W: usize = 1400;
        const H: usize = 140;

        fn over(canvas: &mut [u8], x: i32, y: i32, w: usize, h: usize, data: &[u8]) {
            for r in 0..h {
                for c in 0..w {
                    let (px, py) = (x + c as i32, y + r as i32);
                    if px < 0 || py < 0 || px as usize >= W || py as usize >= H {
                        assert_eq!(data[r * w + c], 0, "ink outside the canvas at {px},{py}");
                        continue;
                    }
                    let at = py as usize * W + px as usize;
                    canvas[at] = canvas[at].saturating_add(data[r * w + c]);
                }
            }
        }

        let mut fs = crate::text::new_font_system();
        let family = crate::text::register_into(fs.db_mut(), &Arc::from(BODY)).expect("parses");
        let mut glyphs = VariationGlyphs::default();
        let mut cache = SwashCache::new();
        for (size, scale, weight, origin) in [
            (13.0_f32, 1.0_f32, 400_u16, (3.0_f32, 14.0_f32)),
            (15.5, 2.0, 700, (7.3, 15.6)),
            (22.0, 1.5, 300, (2.9, 19.1)),
            (11.0, 2.0, 800, (1.41, 12.77)),
        ] {
            let mut b = Buffer::new(&mut fs, Metrics::new(size, size * 1.2));
            b.set_text(
                &mut fs,
                "Hamburgefonstiv fi AVATAR gjpqy Åé 0123",
                &Attrs::new()
                    .family(Family::Name(&family))
                    .weight(glyphon::Weight(weight)),
                Shaping::Advanced,
                None,
            );
            b.shape_until_scroll(&mut fs, false);
            assert!(
                glyphs.is_variable_line(&mut fs, &b),
                "a variable face routes"
            );

            // What glyphon's cache draws.
            let mut want = vec![0u8; W * H];
            for run in b.layout_runs() {
                for g in run.glyphs {
                    let p = g.physical(origin, scale);
                    let image = cache
                        .get_image_uncached(&mut fs, p.cache_key)
                        .expect("rasterizes");
                    let y = (run.line_y * scale).round() as i32 + p.y - image.placement.top;
                    over(
                        &mut want,
                        p.x + image.placement.left,
                        y,
                        image.placement.width as usize,
                        image.placement.height as usize,
                        &image.data,
                    );
                }
            }

            // What the custom glyphs draw, placed as glyphon places them.
            let mut custom = Vec::new();
            glyphs.build_text(&mut fs, &b, origin, scale, 0, &mut custom);
            let mut got = vec![0u8; W * H];
            for c in &custom {
                let (x, x_bin) = glyphon::cosmic_text::SubpixelBin::new(origin.0 + c.left * scale);
                let (y, y_bin) = glyphon::cosmic_text::SubpixelBin::new(origin.1 + c.top * scale);
                let (w, h) = (
                    (c.width * scale).round() as u16,
                    (c.height * scale).round() as u16,
                );
                let image = glyphs
                    .rasterize(RasterizeCustomGlyphRequest {
                        id: c.id,
                        width: w,
                        height: h,
                        x_bin,
                        y_bin,
                        scale,
                    })
                    .expect("rasterizes");
                over(&mut got, x, y, usize::from(w), usize::from(h), &image.data);
            }
            assert!(want.iter().any(|&v| v > 0), "the reference drew something");
            assert!(
                want == got,
                "size {size} scale {scale} weight {weight}: custom glyphs differ from glyphon's"
            );
        }
    }

    /// The canary for the workaround in this module: swash still leaks.
    ///
    /// Two variable fonts through one `ScaleContext`, the way `cosmic-text`
    /// scales them (the `wght` axis and nothing else). Manrope is scaled at
    /// its heaviest first, which leaves a normalised 1.0 at the index of its
    /// `wght` axis, 0; the icon font's axis 0 is `FILL`, which `cosmic-text`
    /// does not set, so the icon glyphs come out filled. A fresh context draws
    /// them open. When this stops being true, swash has fixed it.
    #[test]
    fn swash_still_leaks_variation_coordinates() {
        use swash::FontRef;

        fn wght_only(
            context: &mut ScaleContext,
            font: FontRef<'_>,
            weight: f32,
            glyph: u16,
        ) -> Vec<u8> {
            let wght = Tag::from_be_bytes(*b"wght");
            let axis = font.variations().find_by_tag(wght).expect("a wght axis");
            let mut scaler = context
                .builder(font)
                .size(48.0)
                .variations(std::iter::once(Setting {
                    tag: wght,
                    value: weight.clamp(axis.min_value(), axis.max_value()),
                }))
                .build();
            Render::new(&[Source::Outline])
                .format(Format::Alpha)
                .render(&mut scaler, glyph)
                .map(|image| image.data)
                .unwrap_or_default()
        }

        let body = FontRef::from_index(BODY, 0).expect("Manrope parses");
        let icons = FontRef::from_index(ICONS, 0).expect("the icon font parses");
        let first = |f: FontRef<'_>| f.variations().next().map(|a| a.tag());
        assert_eq!(first(body), Some(Tag::from_be_bytes(*b"wght")));
        assert_ne!(
            first(icons),
            Some(Tag::from_be_bytes(*b"wght")),
            "the premise: the icon font's axis 0 is not wght"
        );

        let glyphs = icons.metrics(&[]).glyph_count;
        let mut fresh = ScaleContext::new();
        let mut used = ScaleContext::new();
        let _ = wght_only(&mut used, body, 800.0, 40);
        let leaked = (1..glyphs).any(|g| {
            wght_only(&mut used, icons, 400.0, g) != wght_only(&mut fresh, icons, 400.0, g)
        });
        assert!(
            leaked,
            "swash has fixed the variation-coordinate leak (dfrg/swash#149); variable fonts \
             can go back through glyphon's cache, so remove the routing in \
             crates/byard-core/src/encoder/variation_glyphs.rs (`is_variable_line`, \
             `build_text`) and crates/byard-core/src/encoder/text_glyph.rs (`text_custom`), \
             and this canary"
        );
    }
}
