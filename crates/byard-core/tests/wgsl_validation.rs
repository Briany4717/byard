//! Headless validation of every WGSL shader module (RFC-0009 §1).
//!
//! `naga` is the same shader front-end/validator `wgpu` uses internally, so a
//! green validation here predicts a clean `Device::create_shader_module` on the
//! GPU, caught in CI without a graphics device.
//!
//! # Why some of these are two files
//!
//! Two blocks are shared textually rather than copied. The gradient ramp
//! (RFC-0037): a pipeline that paints one prepends `gradient.wgsl` to its own
//! source before compiling, so a box gradient and a path gradient are the same
//! instructions rather than two implementations that agree today. And the clip
//! mask (RFC-0037 clip masks): every clippable pipeline prepends `clip.wgsl`,
//! so a clip cuts the same curve in all of them — if each spelled the test
//! itself, a clip would round one pipeline's corners and not another's.
//!
//! What is validated here is therefore the shader **as the pipeline assembles
//! it**, not the fragment on disk. Validating the fragment would be validating
//! something no GPU ever sees, and would fail for a reason that is not a
//! defect: half a shader does not compile.

use naga::valid::{Capabilities, ValidationFlags, Validator};

/// The shared gradient block, prepended by the pipelines that paint one.
const GRADIENT: &str = include_str!("../src/encoder/gradient.wgsl");

/// The shared clip block, prepended by every pipeline that can be clipped.
const CLIP: &str = include_str!("../src/encoder/clip.wgsl");

/// Every pipeline shader, assembled the way its pipeline assembles it.
fn shaders() -> Vec<(&'static str, String)> {
    vec![
        (
            "solid_box",
            format!("{CLIP}\n{}", include_str!("../src/encoder/solid_box.wgsl")),
        ),
        (
            "decorated_box",
            format!(
                "{CLIP}\n{GRADIENT}\n{}",
                include_str!("../src/encoder/decorated_box.wgsl")
            ),
        ),
        (
            "canvas_fill",
            format!(
                "{CLIP}\n{GRADIENT}\n{}",
                include_str!("../src/encoder/canvas_fill.wgsl")
            ),
        ),
        // Neither of these was listed before; both are clippable, so both
        // assemble with the shared block now and both belong here.
        (
            "canvas_shape",
            format!(
                "{CLIP}\n{}",
                include_str!("../src/encoder/canvas_shape.wgsl")
            ),
        ),
        (
            "ripple",
            format!("{CLIP}\n{}", include_str!("../src/encoder/ripple.wgsl")),
        ),
        (
            "texture_sampler",
            format!(
                "{CLIP}\n{}",
                include_str!("../src/encoder/texture_sampler.wgsl")
            ),
        ),
        (
            "vector_msdf",
            format!(
                "{CLIP}\n{}",
                include_str!("../src/encoder/vector_msdf.wgsl")
            ),
        ),
    ]
}

#[test]
fn every_wgsl_module_parses_and_validates() {
    for (name, src) in shaders() {
        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("`{name}.wgsl` failed to parse: {e:?}"));
        let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
        validator
            .validate(&module)
            .unwrap_or_else(|e| panic!("`{name}.wgsl` failed to validate: {e:?}"));
    }
}

#[test]
fn the_gradient_block_is_included_rather_than_copied() {
    // The anti-drift guard, checked in source rather than in pixels: if a
    // second copy of the ramp ever appears in a shader, this is where it is
    // noticed, before the two start disagreeing.
    for (name, src) in [
        (
            "decorated_box",
            include_str!("../src/encoder/decorated_box.wgsl"),
        ),
        (
            "canvas_fill",
            include_str!("../src/encoder/canvas_fill.wgsl"),
        ),
    ] {
        assert!(
            !src.contains("fn gradient_color"),
            "`{name}.wgsl` defines its own gradient ramp; there is one, in gradient.wgsl"
        );
        assert!(
            src.contains("gradient_color("),
            "`{name}.wgsl` should be calling the shared ramp"
        );
    }
}
