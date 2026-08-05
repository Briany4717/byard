//! Headless validation of every WGSL shader module (RFC-0009 §1).
//!
//! `naga` is the same shader front-end/validator `wgpu` uses internally, so a
//! green validation here predicts a clean `Device::create_shader_module` on the
//! GPU, caught in CI without a graphics device.
//!
//! # Why some of these are two files
//!
//! The gradient ramp is shared textually rather than copied (RFC-0037): a
//! pipeline that paints one prepends `gradient.wgsl` to its own source before
//! compiling, so a box gradient and a path gradient are the same instructions
//! rather than two implementations that agree today.
//!
//! What is validated here is therefore the shader **as the pipeline assembles
//! it**, not the fragment on disk. Validating the fragment would be validating
//! something no GPU ever sees, and would fail for a reason that is not a
//! defect: half a shader does not compile.

use naga::valid::{Capabilities, ValidationFlags, Validator};

/// The shared gradient block, prepended by the pipelines that paint one.
const GRADIENT: &str = include_str!("../src/encoder/gradient.wgsl");

/// Every pipeline shader, assembled the way its pipeline assembles it.
fn shaders() -> Vec<(&'static str, String)> {
    vec![
        (
            "solid_box",
            include_str!("../src/encoder/solid_box.wgsl").to_string(),
        ),
        (
            "decorated_box",
            format!(
                "{GRADIENT}\n{}",
                include_str!("../src/encoder/decorated_box.wgsl")
            ),
        ),
        (
            "canvas_fill",
            format!(
                "{GRADIENT}\n{}",
                include_str!("../src/encoder/canvas_fill.wgsl")
            ),
        ),
        (
            "texture_sampler",
            include_str!("../src/encoder/texture_sampler.wgsl").to_string(),
        ),
        (
            "vector_msdf",
            include_str!("../src/encoder/vector_msdf.wgsl").to_string(),
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
