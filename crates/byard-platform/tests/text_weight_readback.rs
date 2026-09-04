//! `weight:` reaches the glyphs (RFC-0034).
//!
//! This is a pixel question and only a pixel answers it. The property was
//! registered in the intrinsic catalogue and read by nobody: `weight: bold`
//! type-checked, and then every path — measurement, shaping, rasterisation —
//! built `Attrs::new().family(Family::SansSerif)` with no weight on it. Two
//! frames differing only in `weight` came back byte-for-byte identical, and
//! nothing in the suite could see it, because nothing looked at ink.
//!
//! So the assertion is about ink: a heavier axis value must put more of it on
//! screen than a lighter one.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};

const SIZE: u32 = 220;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("text weight device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

/// Renders one `.byd` source and returns the frame as RGBA.
fn render(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, src: &str) -> Vec<u8> {
    let parsed = byard_compiler::parser::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = byard_compiler::interp::eval::Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    interp.render(&tree, &mut frame, SIZE as f32, SIZE as f32);

    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        SIZE,
        SIZE,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: SIZE as f32,
            height: SIZE as f32,
        },
        SIZE,
        SIZE,
        1.0,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("weight readback"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.submit(std::iter::once(
        enc.encode_frame_from_relay(&target, &frame).unwrap(),
    ));

    let bpr = 256 * (SIZE * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("weight readback buffer"),
        size: u64::from(bpr) * u64::from(SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut ce = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    ce.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let mut out = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        let (s, d) = ((y * bpr) as usize, (y * SIZE * 4) as usize);
        out[d..d + (SIZE * 4) as usize].copy_from_slice(&data[s..s + (SIZE * 4) as usize]);
    }
    out
}

/// White text on black, so "ink" is simply how much light the glyphs put down.
fn ink(image: &[u8]) -> u64 {
    image.chunks_exact(4).map(|p| u64::from(p[0])).sum()
}

fn source(weight: &str) -> String {
    format!(
        "View Main() {{
    Column #[bg: 0x000000, p: 20, width: 220, height: 220] {{
        Text(\"Weight\") #[color: 0xFFFFFF, size: 44, weight: {weight}]
    }}
}}"
    )
}

/// A weight change reaches the glyphs at all.
///
/// Deliberately `regular` against `bold` and nothing finer. An earlier version
/// asserted that ink increases monotonically along the axis, and that is not a
/// fact about this engine — it is a fact about the fonts a machine happens to
/// have. Linux CI renders `900` with *less* ink than `700`, because `fontdb`
/// substitutes a narrower face for a weight the family does not ship; Windows
/// renders `thin` and `regular` almost identically for the same reason. Both
/// were the test measuring the environment instead of the feature.
///
/// Regular against bold is the one pairing every usable font family
/// distinguishes, and it is the whole claim: the number written in the source
/// changed what was rasterised.
#[test]
fn a_weight_change_reaches_the_glyphs() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping text weight readback");
        return;
    };

    let regular = render(&device, &queue, &source("regular"));
    let bold = render(&device, &queue, &source("bold"));

    assert_ne!(
        ink(&regular),
        ink(&bold),
        "regular and bold must not rasterise to the same ink; \
         the weight is not reaching the shaper"
    );
}

/// A numeric weight reaches them too, and is not silently rounded to a keyword.
#[test]
fn a_numeric_weight_reaches_the_glyphs() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping text weight readback");
        return;
    };

    assert_ne!(
        ink(&render(&device, &queue, &source("400"))),
        ink(&render(&device, &queue, &source("800"))),
        "400 and 800 must not rasterise to the same ink"
    );
}

/// A keyword and its axis value are the same weight, not two notions of one.
///
/// The strongest claim in this file, and the most portable: it needs no font to
/// ship any particular face, only for both spellings to ask for the same one.
#[test]
fn a_keyword_and_its_axis_value_agree() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping text weight readback");
        return;
    };

    assert_eq!(
        render(&device, &queue, &source("bold")),
        render(&device, &queue, &source("700")),
        "`bold` and `700` must render identically; they are one axis, not two"
    );
}
