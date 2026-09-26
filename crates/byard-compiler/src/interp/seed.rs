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
}
