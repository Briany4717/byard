//! Colour, and the one place its transfer function lives.
//!
//! # Why this is in core
//!
//! Because it is a fact about the engine, not about the language. A colour is
//! *written* the way a designer reads it (`0x5B8DEF` is the number out of the
//! design tool, an sRGB-encoded triple), and everything downstream of this
//! module is linear: `RenderFrame`'s colours are documented as linear, and each
//! fragment shader encodes its colour once, on write, into a `Unorm` target
//! (RFC-0060), so the blend mixes sRGB bytes like the tools a design comes
//! from. [`wgsl_transfer`] is that encode, generated from the same constants.
//!
//! It used to live in the compiler, which was fine while `byld` was the only
//! thing that produced colours. A package's native view produces them too
//! (RFC-0039), and a second implementation of a transfer function is a second
//! chance to get it wrong: the engine already spent a release displaying every
//! colour lighter than it was written, because one path encoded twice.

/// Where the sRGB → linear curve leaves its straight segment.
const DECODE_KNEE: f32 = 0.040_45;
/// Where the linear → sRGB curve leaves its straight segment.
const ENCODE_KNEE: f32 = 0.003_130_8;
/// The slope of both straight segments' encoded side.
const LINEAR_SLOPE: f32 = 12.92;
/// The curve's offset.
const OFFSET: f32 = 0.055;
/// The curve's scale, `1 + OFFSET`.
const SCALE: f32 = 1.055;
/// The curve's exponent.
const GAMMA: f32 = 2.4;

/// One channel of sRGB gamma → linear.
///
/// The IEC 61966-2-1 piecewise transfer, not a `powf(2.2)` approximation: the
/// approximation is visibly wrong in the darkest few percent, which is exactly
/// where a dark UI spends most of its range.
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= DECODE_KNEE {
        c / LINEAR_SLOPE
    } else {
        ((c + OFFSET) / SCALE).powf(GAMMA)
    }
}

/// One channel of linear → sRGB gamma, the inverse of [`srgb_to_linear`].
#[must_use]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= ENCODE_KNEE {
        c * LINEAR_SLOPE
    } else {
        SCALE * c.powf(1.0 / GAMMA) - OFFSET
    }
}

/// The WGSL every colour-writing fragment shader ends with (RFC-0060),
/// generated from the constants above so the curve is written once.
///
/// It defines `byard_encode(c: f32) -> f32`, `byard_out(c: vec4<f32>)` (a
/// straight-alpha colour in, the encoded one out) and `byard_out_premul`
/// (premultiplied in, premultiplied encoded out, zero alpha kept as zero).
/// The render targets are `Unorm`, so what a shader writes is the sRGB byte,
/// and the fixed-function blend mixes encoded values like a design tool does.
#[must_use]
pub fn wgsl_transfer() -> &'static str {
    static SOURCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SOURCE.get_or_init(|| {
        format!(
            "// Linear to sRGB, generated from byard_core::color (RFC-0060).
fn byard_encode(c: f32) -> f32 {{
    let v = clamp(c, 0.0, 1.0);
    if (v <= {ENCODE_KNEE:?}) {{
        return v * {LINEAR_SLOPE:?};
    }}
    return {SCALE:?} * pow(v, {inv:?}) - {OFFSET:?};
}}

fn byard_out(c: vec4<f32>) -> vec4<f32> {{
    return vec4<f32>(byard_encode(c.r), byard_encode(c.g), byard_encode(c.b), c.a);
}}

fn byard_out_premul(c: vec4<f32>) -> vec4<f32> {{
    if (!(c.a > 0.0)) {{
        return vec4<f32>(0.0);
    }}
    let straight = c.rgb / c.a;
    return vec4<f32>(
        byard_encode(straight.r) * c.a,
        byard_encode(straight.g) * c.a,
        byard_encode(straight.b) * c.a,
        c.a,
    );
}}
",
            inv = 1.0 / GAMMA
        )
    })
}

/// Parses a colour integer into **linear-space** RGBA (6-digit ⇒ opaque,
/// 8-digit ⇒ alpha-first `0xAARRGGBB`), RFC-0005 §1.
///
/// Alpha is **not** transferred: it is a coverage fraction, not a colour, and
/// has never been gamma-encoded.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "a masked byte is 0..=255, which every f32 represents exactly"
)]
pub fn to_rgba(hex: i64, alpha_byte: bool) -> [f32; 4] {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = hex as u32;
    let f = |b: u32| srgb_to_linear((b & 0xFF) as f32 / 255.0);
    let a = |b: u32| (b & 0xFF) as f32 / 255.0;
    if alpha_byte {
        [f(v >> 16), f(v >> 8), f(v), a(v >> 24)]
    } else {
        [f(v >> 16), f(v >> 8), f(v), 1.0]
    }
}

/// The colour a package widget writes: `0xRRGGBB` opaque, `0xAARRGGBB` with an
/// alpha byte, decoded to the linear RGBA the engine paints in (RFC-0039).
///
/// The magnitude is what distinguishes the two forms here, because a value
/// reaching Rust has no lexer tag on it: above `0x00FF_FFFF` is alpha-first.
/// A widget wanting a fully transparent colour writes `0x00_00_00_00`, which
/// is opaque black at zero alpha either way.
#[must_use]
pub fn rgba(hex: u32) -> [f32; 4] {
    to_rgba(i64::from(hex), hex > 0x00FF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_colour_survives_the_round_trip() {
        // The defect this module exists to make impossible: mid grey written
        // as 0x808080 reached the screen as 0xBC, because it was encoded
        // twice. Decoding and re-encoding must land back on the byte that was
        // written.
        for byte in [0u32, 1, 64, 128, 200, 255] {
            let linear = rgba(byte << 16 | byte << 8 | byte);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let back = (linear_to_srgb(linear[0]) * 255.0).round() as u32;
            assert_eq!(back, byte, "{byte} did not survive the round trip");
        }
    }

    #[test]
    fn an_alpha_byte_is_a_coverage_fraction_not_a_colour() {
        let half = rgba(0x8000_0000);
        assert!(
            (half[3] - 128.0 / 255.0).abs() < 1e-6,
            "alpha is linear coverage, never gamma-encoded: {half:?}"
        );
    }

    #[test]
    fn six_digits_are_opaque() {
        assert!((rgba(0x00FF_FFFF)[3] - 1.0).abs() < f32::EPSILON);
        assert!((rgba(0x0000_0000)[3] - 1.0).abs() < f32::EPSILON);
    }
}
