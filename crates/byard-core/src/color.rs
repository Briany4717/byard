//! Colour, and the one place its transfer function lives.
//!
//! # Why this is in core
//!
//! Because it is a fact about the engine, not about the language. A colour is
//! *written* the way a designer reads it (`0x5B8DEF` is the number out of the
//! design tool, an sRGB-encoded triple), and everything downstream of this
//! module is linear: `RenderFrame`'s colours are documented as linear, the
//! shaders blend in linear, and the surface is an sRGB format precisely so the
//! GPU encodes once on write.
//!
//! It used to live in the compiler, which was fine while `byld` was the only
//! thing that produced colours. A package's native view produces them too
//! (RFC-0039), and a second implementation of a transfer function is a second
//! chance to get it wrong: the engine already spent a release displaying every
//! colour lighter than it was written, because one path encoded twice.

/// One channel of sRGB gamma → linear.
///
/// The IEC 61966-2-1 piecewise transfer, not a `powf(2.2)` approximation: the
/// approximation is visibly wrong in the darkest few percent, which is exactly
/// where a dark UI spends most of its range.
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// One channel of linear → sRGB gamma, the inverse of [`srgb_to_linear`].
#[must_use]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
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
