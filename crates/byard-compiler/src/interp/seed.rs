//! Colour schemes derived from one seed colour (RFC-0022 §5).
//!
//! # What this is, and what it is not
//!
//! The *structure* is Material 3's: five tonal palettes derived from the seed
//! (primary, secondary, tertiary, neutral, neutral-variant, plus a fixed error
//! palette), and every scheme role assigned a *tone* from one of them: primary
//! is tone 40 in light and 80 in dark, its "on" colour 100 and 20, and so on.
//! Those tone pairs are what make the scheme legible: the roles an app puts
//! text on are always far enough apart in lightness to read.
//!
//! The *colour space* is not Google's. Material You builds its palettes in HCT
//! (CAM16 hue and chroma, CIELAB tone). This builds them in OKLCH, the
//! perceptual space the engine already blends colours in, and places each tone
//! by solving for its **CIELAB lightness**, which is exactly what HCT's tone
//! is. So a tone here is the same tone as there, and the contrast guarantees
//! that come from tone differences hold; the hues and chromas are close to
//! Material's, not equal to them. A seed run through Google's tool and through
//! this will produce related but different hex values, and nothing here claims
//! otherwise.
//!
//! Every function is pure and deterministic: the same seed is the same table
//! on every platform, which is what lets a theme be derived at build time and
//! at runtime interchangeably.

/// The roles a derived scheme fills, and the tone each takes, `(role, palette,
/// light tone, dark tone)`. The role names are `byard-base`'s, so a seed
/// replaces the base palette wholesale rather than leaving half of it behind.
const ROLES: &[(&str, Palette, f32, f32)] = &[
    ("primary", Palette::Primary, 40.0, 80.0),
    ("onPrimary", Palette::Primary, 100.0, 20.0),
    ("primaryContainer", Palette::Primary, 90.0, 30.0),
    ("onPrimaryContainer", Palette::Primary, 10.0, 90.0),
    ("secondary", Palette::Secondary, 40.0, 80.0),
    ("onSecondary", Palette::Secondary, 100.0, 20.0),
    ("secondaryContainer", Palette::Secondary, 90.0, 30.0),
    ("onSecondaryContainer", Palette::Secondary, 10.0, 90.0),
    ("tertiary", Palette::Tertiary, 40.0, 80.0),
    ("onTertiary", Palette::Tertiary, 100.0, 20.0),
    ("tertiaryContainer", Palette::Tertiary, 90.0, 30.0),
    ("onTertiaryContainer", Palette::Tertiary, 10.0, 90.0),
    ("surface", Palette::Neutral, 98.0, 6.0),
    ("surfaceContainerLow", Palette::Neutral, 96.0, 10.0),
    ("surfaceContainer", Palette::Neutral, 94.0, 12.0),
    ("surfaceContainerHigh", Palette::Neutral, 92.0, 17.0),
    ("onSurface", Palette::Neutral, 10.0, 90.0),
    ("surfaceVariant", Palette::NeutralVariant, 90.0, 30.0),
    ("onSurfaceVariant", Palette::NeutralVariant, 30.0, 80.0),
    ("outline", Palette::NeutralVariant, 50.0, 60.0),
    ("outlineVariant", Palette::NeutralVariant, 80.0, 30.0),
    ("background", Palette::Neutral, 98.0, 6.0),
    ("onBackground", Palette::Neutral, 10.0, 90.0),
    ("error", Palette::Error, 40.0, 80.0),
    ("onError", Palette::Error, 100.0, 20.0),
    ("errorContainer", Palette::Error, 90.0, 30.0),
    ("onErrorContainer", Palette::Error, 10.0, 90.0),
];

/// The pairs a scheme puts text on, `(background role, foreground role)`.
/// Exposed so the contrast test checks the same list the scheme is built from.
pub const TEXT_PAIRS: &[(&str, &str)] = &[
    ("primary", "onPrimary"),
    ("primaryContainer", "onPrimaryContainer"),
    ("secondary", "onSecondary"),
    ("secondaryContainer", "onSecondaryContainer"),
    ("tertiary", "onTertiary"),
    ("tertiaryContainer", "onTertiaryContainer"),
    ("surface", "onSurface"),
    ("surfaceContainer", "onSurface"),
    ("surfaceVariant", "onSurfaceVariant"),
    ("background", "onBackground"),
    ("error", "onError"),
    ("errorContainer", "onErrorContainer"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Palette {
    Primary,
    Secondary,
    Tertiary,
    Neutral,
    NeutralVariant,
    Error,
}

/// One derived scheme: `(role, 0xRRGGBB)` in the order of [`ROLES`].
pub type Scheme = Vec<(&'static str, i64)>;

/// Both schemes derived from `seed` (`0xRRGGBB`): `(light, dark)`.
#[must_use]
pub fn derive(seed: i64) -> (Scheme, Scheme) {
    let [_, a, b] = oklab_from_srgb(seed);
    let hue = b.atan2(a);
    let chroma = a.hypot(b);
    let palette = |p: Palette| -> (f32, f32) {
        match p {
            // The seed's own hue, at least moderately saturated: a nearly grey
            // seed still has to produce a primary that reads as a colour.
            Palette::Primary => (hue, chroma.max(0.10)),
            Palette::Secondary => (hue, chroma.max(0.10) / 3.0),
            // Sixty degrees round, as Material's tertiary is.
            Palette::Tertiary => (hue + std::f32::consts::FRAC_PI_3, chroma.max(0.10) / 2.0),
            Palette::Neutral => (hue, 0.010),
            Palette::NeutralVariant => (hue, 0.025),
            // A fixed red, whatever the seed: an error must not turn blue
            // because the brand is.
            Palette::Error => (0.50, 0.19),
        }
    };
    let mut light = Vec::with_capacity(ROLES.len());
    let mut dark = Vec::with_capacity(ROLES.len());
    for &(role, p, lt, dt) in ROLES {
        let (h, c) = palette(p);
        light.push((role, tone(h, c, lt)));
        dark.push((role, tone(h, c, dt)));
    }
    (light, dark)
}

/// The longest side an image is sampled down to before quantising. The
/// dominant colour of a logo or a photo does not live in its fine detail, and
/// this bounds the work at 16,384 pixels whatever the file's size.
const SAMPLE_SIDE: u32 = 128;
/// The least alpha a pixel needs to count as part of the picture.
const HALF_OPAQUE: u8 = 128;
/// How many colours the image is quantised to.
const CLUSTERS: usize = 16;
/// Below this colourfulness (the spread between an sRGB pixel's largest and
/// smallest channel, `0..=255`) a cluster counts as grey and cannot be the
/// seed while a colourful one exists: a photo's large grey sky is not its
/// brand.
const GREY_BELOW: u32 = 24;

/// The seed colour (`0xRRGGBB`) of an image, from its RGBA8 pixels (RFC-0022
/// §5): the cluster that maximises population weighted by colourfulness,
/// near-greys rejected. Only pixels at least half opaque (alpha 128 or more)
/// are the picture: a logo's anti-aliased edge counts, the faint halo round
/// it does not. `None` when no pixel is.
///
/// Median cut to [`CLUSTERS`] colours over at most [`SAMPLE_SIDE`] pixels on
/// the long side, in **integer sRGB** throughout, so the same bytes are the
/// same seed on every platform to the bit. OKLab would be the more perceptual
/// space to cut in, but it needs a cube root, whose last bit is the maths
/// library's choice, and a seed that changed with the machine building it
/// would make a theme change with it. The seed then goes through [`derive`],
/// which works in OKLCH as before.
///
/// This is close to what Material's image scoring picks, not the same
/// algorithm: Material quantises in CAM16 and scores with its own weights.
#[must_use]
pub fn seed_from_rgba(rgba: &[u8], width: u32, height: u32) -> Option<i64> {
    let step = width.max(height).div_ceil(SAMPLE_SIDE).max(1);
    let mut pixels: Vec<[u8; 3]> = Vec::new();
    for y in (0..height).step_by(step as usize) {
        for x in (0..width).step_by(step as usize) {
            let i = ((y as usize) * (width as usize) + x as usize) * 4;
            let Some(px) = rgba.get(i..i + 4) else {
                continue;
            };
            // Under half opaque is not part of the picture.
            if px[3] >= HALF_OPAQUE {
                pixels.push([px[0], px[1], px[2]]);
            }
        }
    }
    if pixels.is_empty() {
        return None;
    }
    let clusters = median_cut(pixels);
    let colourful = |c: &[u8; 3]| {
        u32::from(*c.iter().max().unwrap_or(&0)) - u32::from(*c.iter().min().unwrap_or(&0))
    };
    let pack = |c: [u8; 3]| (i64::from(c[0]) << 16) | (i64::from(c[1]) << 8) | i64::from(c[2]);
    // Highest score first; ties go to the smaller colour, so the choice never
    // depends on the order the clusters came out in.
    let best = |score: &dyn Fn(&Cluster) -> u64| {
        clusters
            .iter()
            .max_by(|a, b| score(a).cmp(&score(b)).then(pack(b.0).cmp(&pack(a.0))))
            .map(|c| pack(c.0))
    };
    if clusters.iter().any(|(c, _)| colourful(c) >= GREY_BELOW) {
        best(&|(c, n)| {
            let k = colourful(c);
            if k >= GREY_BELOW {
                u64::from(*n) * u64::from(k)
            } else {
                0
            }
        })
    } else {
        // Only greys: the most common one, which `derive` floors to a colour.
        best(&|(_, n)| u64::from(*n))
    }
}

/// A quantised colour and how many sampled pixels it stands for.
type Cluster = ([u8; 3], u32);

/// Median cut: splits the pixels into at most [`CLUSTERS`] boxes, each time
/// cutting the box with the widest channel range at the median of that
/// channel, and returns each box's mean colour and population.
fn median_cut(pixels: Vec<[u8; 3]>) -> Vec<Cluster> {
    // A box's widest channel and its range.
    let widest = |b: &[[u8; 3]]| -> (usize, u8) {
        (0..3)
            .map(|ch| {
                let (lo, hi) = b
                    .iter()
                    .fold((u8::MAX, 0), |(lo, hi), p| (lo.min(p[ch]), hi.max(p[ch])));
                (ch, hi.saturating_sub(lo))
            })
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .unwrap_or((0, 0))
    };
    let mut boxes = vec![pixels];
    while boxes.len() < CLUSTERS {
        // The box to cut: widest range, then most pixels, then first.
        let Some((i, (ch, range))) = boxes
            .iter()
            .enumerate()
            .map(|(i, b)| (i, widest(b)))
            .max_by(|a, b| {
                (a.1.1, boxes[a.0].len())
                    .cmp(&(b.1.1, boxes[b.0].len()))
                    .then(b.0.cmp(&a.0))
            })
        else {
            break;
        };
        if range == 0 {
            break;
        }
        let mut b = boxes.swap_remove(i);
        // A total order, so equal channels still sort the same way every run.
        b.sort_unstable_by_key(|p| (p[ch], *p));
        let upper = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(upper);
    }
    boxes
        .into_iter()
        .filter(|b| !b.is_empty())
        .map(|b| {
            let n = u32::try_from(b.len()).unwrap_or(u32::MAX);
            let sum = b.iter().fold([0u64; 3], |mut acc, p| {
                for ch in 0..3 {
                    acc[ch] += u64::from(p[ch]);
                }
                acc
            });
            let len = b.len() as u64;
            let mean = sum.map(|s| u8::try_from((s + len / 2) / len).unwrap_or(u8::MAX));
            (mean, n)
        })
        .collect()
}

/// The sRGB colour at OKLCH hue `h` and (at most) chroma `c` whose CIELAB
/// lightness is `target` (`0..=100`), with the chroma reduced only as far as
/// the sRGB gamut requires.
fn tone(h: f32, c: f32, target: f32) -> i64 {
    if target >= 100.0 {
        return 0x00FF_FFFF;
    }
    if target <= 0.0 {
        return 0x0000_0000;
    }
    // Lightness in OKLab and in CIELAB rise together, so a bisection on the
    // OKLab one finds the CIELAB one. Thirty steps is well past the 1/255 a
    // channel can resolve.
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..30 {
        let mid = f32::midpoint(lo, hi);
        if lstar(in_gamut(mid, h, c)) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    pack(in_gamut(f32::midpoint(lo, hi), h, c))
}

/// Linear sRGB for OKLCH `(l, c, h)`, with `c` reduced until it fits.
fn in_gamut(l: f32, h: f32, c: f32) -> [f32; 3] {
    let at = |c: f32| linear_from_oklab([l, c * h.cos(), c * h.sin()]);
    let fits = |rgb: [f32; 3]| rgb.iter().all(|v| (-1e-4..=1.0 + 1e-4).contains(v));
    let full = at(c);
    if fits(full) {
        return full;
    }
    let (mut lo, mut hi) = (0.0_f32, c);
    for _ in 0..20 {
        let mid = f32::midpoint(lo, hi);
        if fits(at(mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(lo).map(|v| v.clamp(0.0, 1.0))
}

/// CIELAB lightness of a linear-sRGB colour, which is Material's "tone".
fn lstar(rgb: [f32; 3]) -> f32 {
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    let f = if y > 216.0 / 24389.0 {
        y.cbrt()
    } else {
        (24389.0 / 27.0 * y + 16.0) / 116.0
    };
    116.0 * f - 16.0
}

/// WCAG relative luminance of a packed sRGB colour.
#[must_use]
pub fn luminance(hex: i64) -> f32 {
    let rgb = unpack(hex).map(to_linear);
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// WCAG contrast ratio between two packed sRGB colours, `1.0..=21.0`.
#[must_use]
pub fn contrast(a: i64, b: i64) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn oklab_from_srgb(hex: i64) -> [f32; 3] {
    let [red, green, blue] = unpack(hex).map(to_linear);
    let cone = |x: f32, y: f32, z: f32| (x * red + y * green + z * blue).cbrt();
    let lms = [
        cone(0.412_221_46, 0.536_332_55, 0.051_445_995),
        cone(0.211_903_5, 0.680_699_5, 0.107_396_96),
        cone(0.088_302_46, 0.281_718_85, 0.629_978_7),
    ];
    let row = |x: f32, y: f32, z: f32| x * lms[0] + y * lms[1] + z * lms[2];
    [
        row(0.210_454_26, 0.793_617_8, -0.004_072_047),
        row(1.977_998_5, -2.428_592_2, 0.450_593_7),
        row(0.025_904_037, 0.782_771_77, -0.808_675_77),
    ]
}

fn linear_from_oklab(lab: [f32; 3]) -> [f32; 3] {
    let l = (lab[0] + 0.396_337_78 * lab[1] + 0.215_803_76 * lab[2]).powi(3);
    let m = (lab[0] - 0.105_561_346 * lab[1] - 0.063_854_17 * lab[2]).powi(3);
    let s = (lab[0] - 0.089_484_18 * lab[1] - 1.291_485_5 * lab[2]).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[allow(clippy::cast_precision_loss)]
fn unpack(hex: i64) -> [f32; 3] {
    [
        ((hex >> 16) & 0xFF) as f32 / 255.0,
        ((hex >> 8) & 0xFF) as f32 / 255.0,
        (hex & 0xFF) as f32 / 255.0,
    ]
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pack(linear: [f32; 3]) -> i64 {
    let byte = |c: f32| i64::from((to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u8);
    (byte(linear[0]) << 16) | (byte(linear[1]) << 8) | byte(linear[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(table: &[(&str, i64)], name: &str) -> i64 {
        table
            .iter()
            .find(|(r, _)| *r == name)
            .map(|(_, c)| *c)
            .unwrap()
    }

    /// The real assertion about the output: every pair the scheme puts text on
    /// meets WCAG AA for body text (4.5:1), in both schemes, for seeds that
    /// span the hue circle and a nearly grey one. "A scheme was produced" would
    /// pass for a scheme nobody can read.
    #[test]
    fn every_text_pair_meets_aa_contrast_in_both_schemes() {
        for seed in [
            0x67_50A4, 0x0B_57D0, 0x1E_8E3E, 0xF9_AB00, 0xD9_3025, 0x80_8080, 0x00_BCD4,
        ] {
            let (light, dark) = derive(seed);
            for (scheme, table) in [("light", &light), ("dark", &dark)] {
                for (bg, fg) in TEXT_PAIRS {
                    let ratio = contrast(role(table, bg), role(table, fg));
                    assert!(
                        ratio >= 4.5,
                        "seed {seed:06X} {scheme}: {fg} on {bg} is {ratio:.2}:1"
                    );
                }
            }
        }
    }

    /// Tones are CIELAB lightness, which is what makes them comparable to
    /// Material's and what the contrast guarantee rests on.
    #[test]
    fn a_tone_is_the_cielab_lightness_it_names() {
        for t in [10.0, 30.0, 40.0, 50.0, 80.0, 90.0, 98.0] {
            let hex = tone(1.0, 0.12, t);
            let got = lstar(unpack(hex).map(to_linear));
            assert!((got - t).abs() < 1.0, "tone {t} came out as L* {got}");
        }
    }

    /// The same seed is the same table, every time and everywhere.
    #[test]
    fn a_seed_is_deterministic() {
        assert_eq!(derive(0x67_50A4), derive(0x67_50A4));
    }

    /// The seed decides the scheme: two seeds far apart on the hue circle give
    /// two different primaries, and the error palette does not follow them.
    #[test]
    fn the_seed_moves_the_primary_and_not_the_error() {
        let (blue, _) = derive(0x0B_57D0);
        let (green, _) = derive(0x1E_8E3E);
        assert_ne!(role(&blue, "primary"), role(&green, "primary"));
        assert_eq!(role(&blue, "error"), role(&green, "error"));
    }

    /// An RGBA8 image of `w` x `h` filled by `f(x, y)`.
    fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| f(x, y))
            .collect()
    }

    #[test]
    fn a_solid_image_is_its_own_seed() {
        for rgb in [[0x1E, 0x8E, 0x3E], [0xD9, 0x30, 0x25], [0x80, 0x80, 0x80]] {
            let px = image(40, 30, |_, _| [rgb[0], rgb[1], rgb[2], 255]);
            let want = (i64::from(rgb[0]) << 16) | (i64::from(rgb[1]) << 8) | i64::from(rgb[2]);
            assert_eq!(seed_from_rgba(&px, 40, 30), Some(want));
        }
    }

    /// A photo's large grey area is not its colour: a small saturated patch
    /// wins over nine times as many grey pixels.
    #[test]
    fn a_small_saturated_area_beats_a_large_grey_one() {
        let px = image(100, 100, |x, y| {
            if x < 30 && y < 30 {
                [0xE8, 0x54, 0x3F, 255]
            } else {
                // A grey with some noise, as a real sky or wall has.
                let n = u8::try_from((x * 7 + y * 13) % 9).unwrap();
                [120 + n, 122 + n, 125 + n, 255]
            }
        });
        let seed = seed_from_rgba(&px, 100, 100).unwrap();
        assert_eq!(seed, 0xE8_543F, "{seed:06X}");
    }

    /// Of two colours, population weighted by colourfulness decides: the
    /// larger one wins at equal colourfulness, the more colourful one at
    /// equal size.
    #[test]
    fn population_and_colourfulness_both_count() {
        let (blue, teal) = ([0x30, 0x60, 0xD0, 255], [0x40, 0x90, 0x88, 255]);
        let larger_blue = image(10, 10, |x, _| if x < 7 { blue } else { teal });
        assert_eq!(seed_from_rgba(&larger_blue, 10, 10), Some(0x30_60D0));
        let (vivid, dull) = ([0xF0, 0x20, 0x20, 255], [0xA0, 0x60, 0x60, 255]);
        let half = image(10, 10, |x, _| if x < 5 { dull } else { vivid });
        assert_eq!(seed_from_rgba(&half, 10, 10), Some(0xF0_2020));
    }

    /// The cutoff is exactly half opaque: 128 counts, 127 does not.
    #[test]
    fn a_pixel_counts_from_half_opaque() {
        let at = |alpha: u8| image(4, 4, move |_, _| [0xE8, 0x54, 0x3F, alpha]);
        assert_eq!(seed_from_rgba(&at(128), 4, 4), Some(0xE8_543F));
        assert_eq!(seed_from_rgba(&at(127), 4, 4), None);
    }

    #[test]
    fn transparent_pixels_are_not_the_picture() {
        let px = image(20, 20, |x, _| {
            if x < 15 {
                [0, 0, 255, 0]
            } else {
                [0xE8, 0xB5, 0x4A, 255]
            }
        });
        assert_eq!(seed_from_rgba(&px, 20, 20), Some(0xE8_B54A));
        assert_eq!(
            seed_from_rgba(&image(4, 4, |_, _| [9, 9, 9, 0]), 4, 4),
            None
        );
    }

    /// Pinned: this constant is asserted on every platform CI runs, which is
    /// the cross-platform half of "the same bytes are the same seed". A
    /// 600 x 400 gradient is sampled down, cut into sixteen and scored, all in
    /// integers, so no maths library can move it.
    #[test]
    fn the_same_bytes_are_the_same_seed_everywhere() {
        let px = image(600, 400, |x, y| {
            let r = u8::try_from(x * 255 / 599).unwrap();
            let g = u8::try_from(y * 255 / 399).unwrap();
            [r, g, 255 - r / 2, 255]
        });
        let first = seed_from_rgba(&px, 600, 400);
        assert_eq!(first, seed_from_rgba(&px, 600, 400));
        assert_eq!(first, Some(PINNED), "{:06X}", first.unwrap());
    }
    const PINNED: i64 = 0x1E_1EF0;
}
