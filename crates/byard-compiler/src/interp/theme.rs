//! Design-token theme runtime (RFC-0022, completing RFC-0008 Pillar D and
//! RFC-0005 §6 `Typo` resolution).
//!
//! A [`Theme`] is a package/project-declared, engine-resolved bundle of **color**
//! (per named scheme, `light`/`dark`), **typography**, and **shape** tokens. It
//! serves two roles:
//!
//! 1. **Theme-default layer** (RFC-0005 §6), any intrinsic prop not set by a class or inline
//!    attribute falls back to the theme (the pre-existing `Theme::light()` role).
//! 2. **Reactive token references**, `inject Theme as t` then `t.primary`,
//!    `t.titleLarge`, `t.cornerLg` in `.byd` resolve against the *active* scheme.
//!    Flipping the scheme (`t.dark = true`) is a single reactive write that
//!    marks every token-dependent binding dirty (RFC-0022 §1).
//!
//! Tokens are declared `snake_case` in `byard.toml` (TOML convention) and
//! referenced `camelCase` in `byld` (language convention); [`to_camel`] performs
//! the mapping at parse time so both maps share one canonical `camelCase` key.
//!
//! The engine ships a built-in [`Theme::byard_base`] (RFC-0022 §6): a neutral
//! gray palette, the M3 type scale, and the 0/4/8/12/16/28 shape scale. A
//! manifest theme layers on top, it overrides only the tokens it redefines;
//! everything else falls through to `byard-base`.

use std::collections::BTreeMap;

/// A font weight, resolved from a manifest `weight = "…"` string or the M3
/// defaults. The numeric value is the CSS/OpenType weight axis (100–900).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FontWeight {
    /// 100.
    Thin,
    /// 300.
    Light,
    /// 400, the default.
    #[default]
    Regular,
    /// 500.
    Medium,
    /// 600.
    SemiBold,
    /// 700.
    Bold,
    /// 900.
    Black,
}

impl FontWeight {
    /// Parses a manifest weight string (case-insensitive). Unknown strings map
    /// to [`FontWeight::Regular`], the caller reports the diagnostic.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "thin" | "100" => Self::Thin,
            "light" | "300" => Self::Light,
            "regular" | "normal" | "400" => Self::Regular,
            "medium" | "500" => Self::Medium,
            "semibold" | "semi_bold" | "600" => Self::SemiBold,
            "bold" | "700" => Self::Bold,
            "black" | "900" => Self::Black,
            _ => return None,
        })
    }

    /// The numeric `OpenType` weight axis value (100–900).
    #[must_use]
    pub fn axis(self) -> u16 {
        match self {
            Self::Thin => 100,
            Self::Light => 300,
            Self::Regular => 400,
            Self::Medium => 500,
            Self::SemiBold => 600,
            Self::Bold => 700,
            Self::Black => 900,
        }
    }
}

/// A resolved typography token (RFC-0022 §1, RFC-0005 `Typo`).
#[derive(Clone, Debug, PartialEq)]
pub struct TypoToken {
    /// The font family name (e.g. `"Roboto"`). `None` ⇒ the system font. A
    /// declared-but-unregistered family falls back to the system font with a
    /// `FontNotFound` warning (RFC-0022 §3-§4).
    pub family: Option<String>,
    /// Font size in logical pixels.
    pub size: f32,
    /// Font weight.
    pub weight: FontWeight,
    /// Letter tracking (spacing) in logical pixels.
    pub tracking: f32,
    /// Line height in logical pixels.
    pub line_height: f32,
}

impl TypoToken {
    /// A plain token: a size, regular weight, and a line height ≈ 1.25× size.
    #[must_use]
    pub fn plain(size: f32) -> Self {
        Self {
            family: None,
            size,
            weight: FontWeight::Regular,
            tracking: 0.0,
            line_height: (size * 1.25).round(),
        }
    }
}

/// Maps a `snake_case` manifest key to the `camelCase` byld reference form
/// (RFC-0022 unresolved-question resolution: snake in TOML, camel in byld).
///
/// `primary_container` → `primaryContainer`; `title_large` → `titleLarge`. A key
/// already in `camelCase` (no underscores) is returned unchanged.
#[must_use]
pub fn to_camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for ch in snake.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The Material 3 typography scale: `camelCase` token → (size, weight, tracking).
/// Line height is derived (`plain`) unless a manifest overrides the whole token.
/// Used both for `byard-base` typography and as the fallback for a bare
/// `typo: titleLarge` size (RFC-0005 §6).
const M3_TYPO: &[(&str, f32, FontWeight, f32)] = &[
    ("displayLarge", 57.0, FontWeight::Regular, -0.25),
    ("displayMedium", 45.0, FontWeight::Regular, 0.0),
    ("displaySmall", 36.0, FontWeight::Regular, 0.0),
    ("headlineLarge", 32.0, FontWeight::Regular, 0.0),
    ("headlineMedium", 28.0, FontWeight::Regular, 0.0),
    ("headlineSmall", 24.0, FontWeight::Regular, 0.0),
    ("titleLarge", 22.0, FontWeight::Regular, 0.0),
    ("titleMedium", 16.0, FontWeight::Medium, 0.15),
    ("titleSmall", 14.0, FontWeight::Medium, 0.1),
    ("bodyLarge", 16.0, FontWeight::Regular, 0.5),
    ("bodyMedium", 14.0, FontWeight::Regular, 0.25),
    ("bodySmall", 12.0, FontWeight::Regular, 0.4),
    ("labelLarge", 14.0, FontWeight::Medium, 0.1),
    ("labelMedium", 12.0, FontWeight::Medium, 0.5),
    ("labelSmall", 11.0, FontWeight::Medium, 0.5),
    // Short aliases used across byld samples.
    ("display", 36.0, FontWeight::Regular, 0.0),
    ("headline", 24.0, FontWeight::Regular, 0.0),
    ("title", 20.0, FontWeight::Regular, 0.0),
    ("body", 14.0, FontWeight::Regular, 0.0),
    ("caption", 12.0, FontWeight::Regular, 0.0),
    ("label", 11.0, FontWeight::Medium, 0.0),
];

/// How many of the leading [`M3_TYPO`] entries are the fifteen M3 styles (the
/// rest are short aliases); only those have an emphasized twin.
const M3_STYLES: usize = 15;

/// The suffix of an emphasized type style (`labelLargeEmphasized`).
const EMPHASIZED: &str = "Emphasized";

/// The weight an emphasized style carries: regular becomes medium, medium
/// becomes bold, anything else is already heavy and stays.
fn emphasize(weight: FontWeight) -> FontWeight {
    match weight {
        FontWeight::Regular => FontWeight::Medium,
        FontWeight::Medium => FontWeight::Bold,
        other => other,
    }
}

/// The M3 shape scale: `camelCase` corner token → radius (RFC-0022 §6).
const BASE_SHAPES: &[(&str, f32)] = &[
    ("cornerNone", 0.0),
    ("cornerXs", 4.0),
    ("cornerSm", 8.0),
    ("cornerMd", 12.0),
    ("cornerLg", 16.0),
    ("cornerLgIncreased", 20.0),
    ("cornerXl", 28.0),
    ("cornerXlIncreased", 32.0),
    ("cornerXxl", 48.0),
    ("cornerFull", 9999.0),
];

/// Material 3 Expressive's springs: `(token, stiffness, damping ratio)`.
/// The ratio is Material's `damping`; [`spring_damping`] turns it into the
/// engine's coefficient when the theme is built.
const BASE_SPRINGS: &[(&str, f32, f32)] = &[
    ("springSpatialFast", 800.0, 0.6),
    ("springSpatialDefault", 380.0, 0.8),
    ("springSpatialSlow", 200.0, 0.8),
    ("springEffectsFast", 3800.0, 1.0),
    ("springEffectsDefault", 1600.0, 1.0),
    ("springEffectsSlow", 800.0, 1.0),
];

/// Material 3's standard durations in milliseconds.
const BASE_DURATIONS: &[(&str, u32)] = &[
    ("durationShort1", 50),
    ("durationShort2", 100),
    ("durationShort3", 150),
    ("durationShort4", 200),
    ("durationMedium1", 250),
    ("durationMedium2", 300),
    ("durationMedium3", 350),
    ("durationMedium4", 400),
    ("durationLong1", 450),
    ("durationLong2", 500),
    ("durationLong3", 550),
    ("durationLong4", 600),
    ("durationExtraLong1", 700),
    ("durationExtraLong2", 800),
    ("durationExtraLong3", 900),
    ("durationExtraLong4", 1000),
];

/// A named piece of motion a theme declares (RFC-0053).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MotionToken {
    /// A spring, already in the engine's terms: the `stiffness` and the damping
    /// *coefficient* of `anim.spring(stiffness:, damping:)`.
    Spring {
        /// Spring stiffness.
        stiffness: f32,
        /// Damping coefficient (not the ratio).
        damping: f32,
    },
    /// A duration in whole milliseconds.
    Duration(u32),
}

impl MotionToken {
    /// What this token is, for a diagnostic: `a spring` or `a duration`.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Spring { .. } => "a spring",
            Self::Duration(_) => "a duration",
        }
    }
}

/// The damping coefficient of a unit-mass spring with this stiffness and
/// damping ratio: `2 * ratio * sqrt(stiffness)`. The engine's `damping:` is the
/// coefficient; Material's `damping` is the ratio (RFC-0053 §2).
#[must_use]
pub fn spring_damping(stiffness: f32, ratio: f32) -> f32 {
    2.0 * ratio * stiffness.max(0.0).sqrt()
}

/// The token name for a manifest key: `prefix` followed by the key in
/// `camelCase` with its first letter raised (`spring` + `spatial_fast` is
/// `springSpatialFast`).
fn motion_token_name(prefix: &str, key: &str) -> String {
    let camel = to_camel(key);
    let mut chars = camel.chars();
    let mut out = String::from(prefix);
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
}

/// The `byard-base` light color scheme (`camelCase` token → 0xRRGGBB).
///
/// The core tokens (`primary`/`surface`/`onSurface`/`secondary`/`error`) keep
/// the historical `Theme::light()` values so the *default* look, every
/// intrinsic that falls back to the theme, and the pixel-coupled render tests,
/// is unchanged; RFC-0022 adds the container/outline tokens and the dark scheme.
const BASE_LIGHT: &[(&str, i64)] = &[
    ("primary", 0x0000_6495),
    ("onPrimary", 0x00FF_FFFF),
    ("primaryContainer", 0x00D6_E3FF),
    ("onPrimaryContainer", 0x0000_1B3E),
    ("secondary", 0x004F_5D75),
    ("onSecondary", 0x00FF_FFFF),
    ("secondaryContainer", 0x00D9_E3F8),
    ("onSecondaryContainer", 0x0012_1C2B),
    ("tertiary", 0x0064_597B),
    ("onTertiary", 0x00FF_FFFF),
    ("tertiaryContainer", 0x00E8_DDFF),
    ("onTertiaryContainer", 0x0021_1632),
    ("surface", 0x00FF_FFFF),
    ("surfaceDim", 0x00DE_D8E1),
    ("surfaceBright", 0x00FF_FFFF),
    ("surfaceContainerLowest", 0x00FF_FFFF),
    ("surfaceContainerLow", 0x00F7_F2FA),
    ("surfaceContainer", 0x00F1_ECF4),
    ("surfaceContainerHigh", 0x00EC_E6EE),
    ("surfaceContainerHighest", 0x00E6_E0E9),
    ("onSurface", 0x001C_1B1F),
    ("surfaceVariant", 0x00E1_E2EA),
    ("onSurfaceVariant", 0x0043_474E),
    ("outline", 0x0073_777F),
    ("outlineVariant", 0x00C3_C7CF),
    ("background", 0x00FF_FFFF),
    ("onBackground", 0x001C_1B1F),
    ("error", 0x00B3_261E),
    ("onError", 0x00FF_FFFF),
    ("errorContainer", 0x00F9_DEDC),
    ("onErrorContainer", 0x0041_0E0B),
    ("inverseSurface", 0x0031_3033),
    ("inverseOnSurface", 0x00F4_EFF4),
    ("inversePrimary", 0x0090_CAF9),
    ("scrim", 0x0000_0000),
    ("shadow", 0x0000_0000),
    ("primaryFixed", 0x00C6_E7FF),
    ("primaryFixedDim", 0x0089_CEFF),
    ("onPrimaryFixed", 0x0000_1E31),
    ("onPrimaryFixedVariant", 0x0000_4B71),
    ("secondaryFixed", 0x00CD_E6F8),
    ("secondaryFixedDim", 0x00B1_CADC),
    ("onSecondaryFixed", 0x0009_1D2B),
    ("onSecondaryFixedVariant", 0x0034_4958),
    ("tertiaryFixed", 0x00E8_DDFF),
    ("tertiaryFixedDim", 0x00CD_C0E7),
    ("onTertiaryFixed", 0x0021_1632),
    ("onTertiaryFixedVariant", 0x004D_4162),
];

/// The `byard-base` dark color scheme (`camelCase` token → 0xRRGGBB). Core
/// tokens keep the historical `Theme::dark()` values.
const BASE_DARK: &[(&str, i64)] = &[
    ("primary", 0x0090_CAF9),
    ("onPrimary", 0x0000_3258),
    ("primaryContainer", 0x002E_4578),
    ("onPrimaryContainer", 0x00D6_E3FF),
    ("secondary", 0x00B0_BEC5),
    ("onSecondary", 0x0022_333B),
    ("secondaryContainer", 0x003D_4758),
    ("onSecondaryContainer", 0x00D9_E3F8),
    ("tertiary", 0x00CD_C0E7),
    ("onTertiary", 0x0036_2B49),
    ("tertiaryContainer", 0x004D_4162),
    ("onTertiaryContainer", 0x00E8_DDFF),
    ("surface", 0x001C_1B1F),
    ("surfaceDim", 0x0014_1416),
    ("surfaceBright", 0x003A_3B3E),
    ("surfaceContainerLowest", 0x000F_1012),
    ("surfaceContainerLow", 0x001A_1C1E),
    ("surfaceContainer", 0x001E_2022),
    ("surfaceContainerHigh", 0x0029_2A2D),
    ("surfaceContainerHighest", 0x0034_3538),
    ("onSurface", 0x00E6_E1E5),
    ("surfaceVariant", 0x0043_474E),
    ("onSurfaceVariant", 0x00C3_C7CF),
    ("outline", 0x008D_9199),
    ("outlineVariant", 0x0043_474E),
    ("background", 0x001C_1B1F),
    ("onBackground", 0x00E6_E1E5),
    ("error", 0x00F2_B8B5),
    ("onError", 0x0060_1410),
    ("errorContainer", 0x008C_1D18),
    ("onErrorContainer", 0x00F9_DEDC),
    ("inverseSurface", 0x00E6_E1E5),
    ("inverseOnSurface", 0x0031_3033),
    ("inversePrimary", 0x0000_6495),
    ("scrim", 0x0000_0000),
    ("shadow", 0x0000_0000),
    // The fixed roles are the same in both schemes.
    ("primaryFixed", 0x00C6_E7FF),
    ("primaryFixedDim", 0x0089_CEFF),
    ("onPrimaryFixed", 0x0000_1E31),
    ("onPrimaryFixedVariant", 0x0000_4B71),
    ("secondaryFixed", 0x00CD_E6F8),
    ("secondaryFixedDim", 0x00B1_CADC),
    ("onSecondaryFixed", 0x0009_1D2B),
    ("onSecondaryFixedVariant", 0x0034_4958),
    ("tertiaryFixed", 0x00E8_DDFF),
    ("tertiaryFixedDim", 0x00CD_C0E7),
    ("onTertiaryFixed", 0x0021_1632),
    ("onTertiaryFixedVariant", 0x004D_4162),
];

/// The default logical-pixel font size when no `titleLarge`-style token applies.
pub const DEFAULT_FONT_SIZE: f32 = 14.0;

/// The `byard-base` padding a `Button` puts around its label when the author
/// writes no `p:` (vertical, horizontal): the M3 common button's 24 px side
/// inset, and the vertical inset that centres a `labelLarge` line in
/// [`BUTTON_MIN_SIZE`].
pub const BUTTON_PADDING: (f32, f32) = (10.0, 24.0);

/// The `byard-base` minimum `Button` extent, applied to the height unless the
/// author writes `height:` and to the width unless they write `width:`: the M3
/// button container height, so a bare one-line label is still a target a
/// pointer or finger can hit, and a single-glyph button stays a square target
/// instead of a sliver.
pub const BUTTON_MIN_SIZE: f32 = 40.0;

/// The scheme name used for the light color scheme.
pub const SCHEME_LIGHT: &str = "light";
/// The scheme name used for the dark color scheme.
pub const SCHEME_DARK: &str = "dark";

/// A font family declared in `[assets.fonts]`, with its bytes loaded
/// (RFC-0034 §Reference "Asset side").
///
/// Holding the bytes here rather than a path is what makes the theme the
/// single source of truth the font-agreement rule asks for (see
/// `byard_core::frame::FontTable`): the measurement `FontSystem` and
/// the paint one are both fed from this record, so neither can be given a file
/// the other never saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredFont {
    /// The path as written in `byard.toml`, for diagnostics.
    pub path: String,
    /// The family name the face itself carries, which is what shaping matches
    /// on. Resolved once, when the bytes were read.
    pub resolved: std::sync::Arc<str>,
    /// The font file's bytes, shared with every `FontSystem` that loads them.
    pub bytes: std::sync::Arc<[u8]>,
    /// Declared as an icon font (`icons = true`, RFC-0054): its glyphs are
    /// ligature names, and `byard bundle` keeps only the ones the program uses.
    pub icons: bool,
    /// Names the program builds at run time and wants kept in the bundle
    /// (`icons_include`). Listing any silences the not-static warning.
    pub icons_include: Vec<String>,
}

/// Color, typography, and shape design tokens for a view tree (RFC-0022 §1).
///
/// The runtime *active scheme* is not stored here, it lives in a reactive
/// signal owned by the interpreter, so a scheme flip participates in
/// Mark-and-Pull. [`active_dark`](Self::active_dark) mirrors it for the
/// non-reactive theme-default accessors ([`primary`](Self::primary) etc.).
#[derive(Clone, Debug)]
pub struct Theme {
    /// Theme name (`[theme].name`), for diagnostics.
    pub name: String,
    /// `scheme name → (camelCase token → 0xRRGGBB)`.
    schemes: BTreeMap<String, BTreeMap<String, i64>>,
    /// `camelCase token → typography`.
    typography: BTreeMap<String, TypoToken>,
    /// `camelCase token → corner radius`.
    shapes: BTreeMap<String, f32>,
    /// `camelCase token → spring or duration` (RFC-0053).
    motion: BTreeMap<String, MotionToken>,
    /// Declared font families (RFC-0022 §3, RFC-0034): the name written in
    /// `[assets.fonts]` → the loaded face.
    ///
    /// The bytes are loaded when the manifest is read, not deferred: a family
    /// that resolves to nothing is a compile diagnostic, and a diagnostic that
    /// arrives at paint time is a square box nobody can act on.
    fonts: BTreeMap<String, DeclaredFont>,
    /// Named viewport widths for responsive style variants (RFC-0016):
    /// `camelCase` name → logical pixels. A design system's breakpoints are
    /// part of the design system, so they are declared here rather than baked
    /// into the language as a fixed `sm`/`md`/`lg`.
    breakpoints: BTreeMap<String, f32>,
    /// The active scheme mirror for default resolution (`true` ⇒ `dark`).
    pub active_dark: bool,
    /// Whether the scheme follows the system's appearance (`scheme =
    /// "system"`): the host reports it at start and whenever it changes.
    pub follows_system: bool,
    /// Default font size in logical pixels (the theme-default layer).
    pub font_size: f32,
    /// Padding a `Button` gets when no `p:`/side is written, as
    /// `(vertical, horizontal)` (the theme-default layer).
    pub button_padding: (f32, f32),
    /// Minimum `Button` width and height, each applied unless the matching
    /// `width:`/`height:` is written (the theme-default layer).
    pub button_min_size: f32,
    /// How long a scheme flip cross-fades the colour tokens, in milliseconds
    /// (RFC-0016 animated token transitions). `0` is the cut, which is what
    /// every theme did before this existed and what a theme that says nothing
    /// still gets.
    pub transition_ms: u32,
    /// `camelCase token → dependency` for tokens a dependency's `[theme]`
    /// declares that this theme did not take (it does not extend that
    /// dependency). Never read for a value; it lets `UnknownThemeToken` say
    /// where the token lives.
    unapplied: BTreeMap<String, String>,
    /// The dependency this theme extends, if any.
    extended: Option<String>,
}

impl Theme {
    /// Whether `other` declares the same tokens, fonts and defaults as this
    /// theme. Floats compare by bits: both sides were read from a manifest,
    /// so "the same" means the same number, not a nearby one. `active_dark` is left out: it is the scheme the app is showing,
    /// which the running app flips, not something a manifest edit changes.
    ///
    /// What a dev reload asks before replacing the running theme: replacing
    /// it re-registers every font and rebuilds the layout, which a save that
    /// touched only a view should not pay for.
    #[must_use]
    pub fn same_tokens(&self, other: &Self) -> bool {
        self.name == other.name
            && self.schemes == other.schemes
            && self.typography == other.typography
            && self.shapes == other.shapes
            && self.motion == other.motion
            && self.fonts == other.fonts
            && self.breakpoints == other.breakpoints
            && self.font_size.to_bits() == other.font_size.to_bits()
            && self.button_padding.0.to_bits() == other.button_padding.0.to_bits()
            && self.button_padding.1.to_bits() == other.button_padding.1.to_bits()
            && self.button_min_size.to_bits() == other.button_min_size.to_bits()
            && self.transition_ms == other.transition_ms
    }

    /// The engine's built-in `byard-base` theme (RFC-0022 §6): a neutral gray
    /// palette, the M3 type scale, and the 0/4/8/12/16/28 shape scale.
    #[must_use]
    pub fn byard_base() -> Self {
        let mut schemes = BTreeMap::new();
        schemes.insert(
            SCHEME_LIGHT.to_string(),
            BASE_LIGHT
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
        );
        schemes.insert(
            SCHEME_DARK.to_string(),
            BASE_DARK
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
        );
        // The M3 scale, and an `...Emphasized` twin of each of its fifteen
        // styles at the next weight up (RFC-0053 §5).
        let typography = M3_TYPO
            .iter()
            .map(|&(name, size, weight, tracking)| (name.to_string(), weight, size, tracking))
            .chain(
                M3_TYPO
                    .iter()
                    .take(M3_STYLES)
                    .map(|&(name, size, weight, tracking)| {
                        (
                            format!("{name}{EMPHASIZED}"),
                            emphasize(weight),
                            size,
                            tracking,
                        )
                    }),
            )
            .map(|(name, weight, size, tracking)| {
                (
                    name,
                    TypoToken {
                        family: None,
                        size,
                        weight,
                        tracking,
                        line_height: (size * 1.25).round(),
                    },
                )
            })
            .collect();
        let motion = BASE_SPRINGS
            .iter()
            .map(|&(name, stiffness, ratio)| {
                (
                    name.to_string(),
                    MotionToken::Spring {
                        stiffness,
                        damping: spring_damping(stiffness, ratio),
                    },
                )
            })
            .chain(
                BASE_DURATIONS
                    .iter()
                    .map(|&(name, ms)| (name.to_string(), MotionToken::Duration(ms))),
            )
            .collect();
        let shapes = BASE_SHAPES
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect();
        Self {
            name: "byard-base".to_string(),
            schemes,
            typography,
            shapes,
            motion,
            fonts: BTreeMap::new(),
            breakpoints: BTreeMap::new(),
            active_dark: false,
            follows_system: false,
            font_size: DEFAULT_FONT_SIZE,
            button_padding: BUTTON_PADDING,
            button_min_size: BUTTON_MIN_SIZE,
            transition_ms: 0,
            unapplied: BTreeMap::new(),
            extended: None,
        }
    }

    /// A Material 3-inspired light-first theme, `byard-base` is close enough to
    /// serve as the historical `Theme::light()` default. Kept for the interpreter
    /// default and existing call sites.
    #[must_use]
    pub fn light() -> Self {
        Self::byard_base()
    }

    /// `byard-base` with the dark scheme active.
    #[must_use]
    pub fn dark() -> Self {
        let mut t = Self::byard_base();
        t.active_dark = true;
        t
    }

    /// Whether this theme declares a `dark` color scheme with any tokens.
    #[must_use]
    pub fn has_dark(&self) -> bool {
        self.schemes.get(SCHEME_DARK).is_some_and(|m| !m.is_empty())
    }

    /// The scheme name for the given dark flag, falling back to `light` when the
    /// theme has no dark scheme.
    #[must_use]
    pub fn scheme_name(&self, dark: bool) -> &str {
        if dark && self.has_dark() {
            SCHEME_DARK
        } else {
            SCHEME_LIGHT
        }
    }

    /// Resolves a `camelCase` color token in the given scheme, falling back to
    /// the light scheme, then to `byard-base`. Returns `None` for an unknown
    /// token.
    #[must_use]
    pub fn color(&self, token: &str, dark: bool) -> Option<i64> {
        let scheme = self.scheme_name(dark);
        self.schemes
            .get(scheme)
            .and_then(|m| m.get(token))
            .or_else(|| self.schemes.get(SCHEME_LIGHT).and_then(|m| m.get(token)))
            .copied()
    }

    /// Resolves a `camelCase` typography token (RFC-0005 `typo:`).
    #[must_use]
    pub fn typo(&self, token: &str) -> Option<&TypoToken> {
        let key = token.strip_prefix("m3.").unwrap_or(token);
        self.typography.get(key)
    }

    /// The size of a typography token, for the `typo:` size-only fast path.
    #[must_use]
    pub fn typo_size(&self, token: &str) -> Option<f32> {
        self.typo(token).map(|t| t.size)
    }

    /// The weight a typography token carries, on the CSS axis (RFC-0034).
    ///
    /// The `typo:`/`size:` pipeline projected only the size until now, so a
    /// theme that said its headline was semibold got a headline that was not.
    #[must_use]
    pub fn typo_weight(&self, token: &str) -> Option<u16> {
        self.typo(token).map(|t| t.weight.axis())
    }

    /// Resolves a `camelCase` motion token (RFC-0053): `springSpatialFast`,
    /// `durationShort2`.
    #[must_use]
    pub fn motion(&self, token: &str) -> Option<MotionToken> {
        self.motion.get(token).copied()
    }

    /// Declares a spring from a manifest key (`spatial_fast`), with the
    /// damping coefficient the engine's `anim.spring(damping:)` takes. The
    /// token is `springSpatialFast`.
    pub fn set_spring(&mut self, key: &str, stiffness: f32, damping: f32) {
        self.motion.insert(
            motion_token_name("spring", key),
            MotionToken::Spring { stiffness, damping },
        );
    }

    /// Declares a spring from a damping *ratio*, Material's `damping`
    /// (1.0 settles without overshoot).
    pub fn set_spring_ratio(&mut self, key: &str, stiffness: f32, ratio: f32) {
        self.set_spring(key, stiffness, spring_damping(stiffness, ratio));
    }

    /// Declares a duration in milliseconds from a manifest key (`short_2`).
    /// The token is `durationShort2`.
    pub fn set_duration(&mut self, key: &str, ms: u32) {
        self.motion.insert(
            motion_token_name("duration", key),
            MotionToken::Duration(ms),
        );
    }

    /// Resolves a `camelCase` shape (corner-radius) token.
    #[must_use]
    pub fn shape(&self, token: &str) -> Option<f32> {
        self.shapes.get(token).copied()
    }

    /// Whether a font family has been declared in `[assets.fonts]` (RFC-0022 §3).
    #[must_use]
    pub fn has_font(&self, family: &str) -> bool {
        self.font(family).is_some()
    }

    /// The declared face for `family`, under either the manifest's spelling or
    /// its `camelCase` byld reference form (RFC-0034 `font:` resolution).
    #[must_use]
    pub fn font(&self, family: &str) -> Option<&DeclaredFont> {
        self.fonts.get(family).or_else(|| {
            self.fonts
                .iter()
                .find(|(k, _)| to_camel(k) == family)
                .map(|(_, v)| v)
        })
    }

    /// The width in logical pixels a named breakpoint stands for, under
    /// either its manifest spelling or its `camelCase` form (RFC-0016).
    #[must_use]
    pub fn breakpoint(&self, name: &str) -> Option<f32> {
        self.breakpoints.get(name).copied().or_else(|| {
            self.breakpoints
                .iter()
                .find(|(k, _)| to_camel(k) == name)
                .map(|(_, v)| *v)
        })
    }

    /// Every declared breakpoint name, for diagnostics.
    #[must_use]
    pub fn breakpoint_names(&self) -> Vec<String> {
        self.breakpoints.keys().cloned().collect()
    }

    /// Declares a breakpoint (RFC-0016).
    pub fn set_breakpoint(&mut self, name: &str, px: f32) {
        self.breakpoints.insert(to_camel(name), px);
    }

    /// Declared font families (`declared name → loaded face`).
    #[must_use]
    pub fn fonts(&self) -> &BTreeMap<String, DeclaredFont> {
        &self.fonts
    }

    /// Every colour, typography, shape and motion token name, `camelCase`, in no
    /// particular order and possibly repeated across schemes.
    pub fn token_names(&self) -> impl Iterator<Item = &str> {
        self.schemes
            .values()
            .flat_map(|tokens| tokens.keys())
            .chain(self.typography.keys())
            .chain(self.shapes.keys())
            .chain(self.motion.keys())
            .map(String::as_str)
    }

    /// Records that dependency `package`'s theme declares `token` (any
    /// spelling) though this theme did not take it. The first dependency to
    /// declare a token is the one named.
    pub fn note_unapplied_token(&mut self, token: &str, package: &str) {
        self.unapplied
            .entry(to_camel(token))
            .or_insert_with(|| package.to_string());
    }

    /// Records the dependency this theme extends.
    pub fn set_extended(&mut self, package: &str) {
        self.extended = Some(package.to_string());
    }

    /// The dependency this theme extends, if any.
    #[must_use]
    pub fn extended(&self) -> Option<&str> {
        self.extended.as_deref()
    }

    /// The dependency whose theme declares `token` without this theme having
    /// taken it, if any.
    #[must_use]
    pub fn unapplied_token_owner(&self, token: &str) -> Option<&str> {
        self.unapplied.get(token).map(String::as_str)
    }

    /// Registers a font family declared in `[assets.fonts]`, already loaded.
    pub fn add_font(&mut self, name: impl Into<String>, font: DeclaredFont) {
        self.fonts.insert(name.into(), font);
    }

    /// Replaces both schemes' colour roles with the ones derived from `seed`
    /// (`0xRRGGBB`, RFC-0022 §5, see [`crate::interp::seed`]). Only the roles
    /// a derivation fills are touched, so a token the base has and the seed
    /// does not keeps its value, and any `set_color` made *after* this wins,
    /// which is how an explicit `[theme.color.*]` entry overrides a seed.
    pub fn apply_seed(&mut self, seed: i64) {
        let (light, dark) = crate::interp::seed::derive(seed);
        for (scheme, table) in [("light", light), ("dark", dark)] {
            for (role, rgb) in table {
                self.set_color(scheme, role, rgb);
            }
        }
    }

    /// Sets (or overrides) a color token in a scheme. Keys are canonicalized to
    /// `camelCase`. Creates the scheme if absent.
    pub fn set_color(&mut self, scheme: &str, token: &str, rgb: i64) {
        self.schemes
            .entry(scheme.to_string())
            .or_default()
            .insert(to_camel(token), rgb);
    }

    /// Sets (or overrides) a typography token. The key is canonicalized to
    /// `camelCase`.
    pub fn set_typo(&mut self, token: &str, tok: TypoToken) {
        self.typography.insert(to_camel(token), tok);
    }

    /// Sets (or overrides) a shape token. The key is canonicalized to
    /// `camelCase`.
    pub fn set_shape(&mut self, token: &str, radius: f32) {
        self.shapes.insert(to_camel(token), radius);
    }

    // ── Theme-default accessors (non-reactive; read `active_dark`) ──────────

    /// Primary brand color for the active scheme.
    #[must_use]
    pub fn primary(&self) -> i64 {
        self.color("primary", self.active_dark)
            .unwrap_or(0x0000_6495)
    }

    /// On-surface (default text/icon) color for the active scheme.
    #[must_use]
    pub fn on_surface(&self) -> i64 {
        self.color("onSurface", self.active_dark)
            .unwrap_or(0x001C_1B1F)
    }

    /// Surface (default background) color for the active scheme.
    #[must_use]
    pub fn surface(&self) -> i64 {
        self.color("surface", self.active_dark)
            .unwrap_or(0x00FF_FFFF)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::byard_base()
    }
}

/// Resolves a typography token name to a font size using the built-in M3 scale
/// (no theme override). Retained for callers without a [`Theme`] in hand.
#[must_use]
pub fn resolve_typo(token: &str) -> Option<f32> {
    let key = token.strip_prefix("m3.").unwrap_or(token);
    // An emphasized style is the same size as the style it emphasizes.
    let key = key
        .strip_suffix(EMPHASIZED)
        .filter(|plain| M3_TYPO.iter().take(M3_STYLES).any(|(n, ..)| n == plain))
        .unwrap_or(key);
    M3_TYPO
        .iter()
        .find(|(name, ..)| *name == key)
        .map(|(_, size, ..)| *size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_camel_maps_snake_and_leaves_camel() {
        assert_eq!(to_camel("primary_container"), "primaryContainer");
        assert_eq!(to_camel("title_large"), "titleLarge");
        assert_eq!(to_camel("primary"), "primary");
        assert_eq!(to_camel("onSurface"), "onSurface");
    }

    #[test]
    fn resolve_typo_known_tokens() {
        assert_eq!(resolve_typo("titleLarge"), Some(22.0));
        assert_eq!(resolve_typo("bodyMedium"), Some(14.0));
        assert_eq!(resolve_typo("display"), Some(36.0));
    }

    #[test]
    fn resolve_typo_m3_prefix_stripped() {
        assert_eq!(resolve_typo("m3.titleLarge"), Some(22.0));
        assert_eq!(resolve_typo("m3.headlineLarge"), Some(32.0));
    }

    #[test]
    fn resolve_typo_unknown_returns_none() {
        assert_eq!(resolve_typo("notAToken"), None);
    }

    #[test]
    fn base_light_and_dark_differ_on_surface() {
        let t = Theme::byard_base();
        assert_ne!(t.color("onSurface", false), t.color("onSurface", true));
        assert_ne!(t.color("surface", false), t.color("surface", true));
    }

    #[test]
    fn color_falls_back_to_light_when_dark_token_missing() {
        let mut t = Theme::byard_base();
        // A custom token defined only in light resolves in dark via fallback.
        t.set_color("light", "brand", 0x0012_3456);
        assert_eq!(t.color("brand", true), Some(0x0012_3456));
    }

    #[test]
    fn shape_and_typo_tokens_resolve() {
        let t = Theme::byard_base();
        assert_eq!(t.shape("cornerLg"), Some(16.0));
        assert_eq!(t.typo_size("titleLarge"), Some(22.0));
        assert!(t.shape("cornerNope").is_none());
    }

    #[test]
    fn set_color_canonicalizes_snake_keys() {
        let mut t = Theme::byard_base();
        t.set_color("light", "primary_container", 0x00AB_CDEF);
        assert_eq!(t.color("primaryContainer", false), Some(0x00AB_CDEF));
    }

    #[test]
    fn default_is_base_light() {
        let t = Theme::default();
        assert!(!t.active_dark);
        assert_eq!(
            t.on_surface(),
            Theme::byard_base().color("onSurface", false).unwrap()
        );
    }

    #[test]
    fn font_weight_parse_and_axis() {
        assert_eq!(FontWeight::parse("medium"), Some(FontWeight::Medium));
        assert_eq!(FontWeight::parse("700"), Some(FontWeight::Bold));
        assert_eq!(FontWeight::parse("nope"), None);
        assert_eq!(FontWeight::Medium.axis(), 500);
    }
}
