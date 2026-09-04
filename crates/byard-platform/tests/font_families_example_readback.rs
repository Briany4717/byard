//! The shipped RFC-0034 example, rendered through the real interpreter from
//! the real `.byd` on disk.
//!
//! `byard check` proves the example parses and that its manifest names font
//! files that exist; the compiler suite proves `font:` resolves. Neither of
//! them renders it, and this project has shipped screens that passed every
//! test and were visibly wrong. So this one draws the example and reads back
//! which family each line ended up in.
//!
//! The theme here mirrors the example's `byard.toml` rather than parsing it:
//! the manifest reader lives in the CLI, which is a binary and cannot be
//! imported. The `check` gate covers the manifest; this covers the screen.
//!
//! `BYARD_DUMP_PNG=1` also writes the frame out, which is how the two
//! typefaces get *looked* at rather than only counted.
#![allow(
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    clippy::items_after_statements
)]
use byard_compiler::interp::theme::{DeclaredFont, Theme, TypoToken};
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use std::sync::Arc;

const W: u32 = 620;
const H: u32 = 720;

fn face(file: &str) -> DeclaredFont {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../byard-cli/examples/assets/fonts")
        .join(file);
    let bytes: Arc<[u8]> = Arc::from(std::fs::read(p).unwrap());
    let resolved = byard_core::text::family_name(&bytes).unwrap();
    DeclaredFont {
        path: file.into(),
        resolved: Arc::from(resolved),
        bytes,
    }
}

#[test]
fn the_shipped_example_renders_two_families_through_the_interpreter() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: None,
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .unwrap();
    let (device, queue) = (Arc::new(device), Arc::new(queue));

    let mut theme = Theme::byard_base();
    theme.add_font("display", face("SpaceGrotesk-Variable.ttf"));
    theme.add_font("body", face("Manrope-Variable.ttf"));
    theme.set_typo(
        "hero",
        TypoToken {
            family: Some("display".into()),
            ..TypoToken::plain(44.0)
        },
    );
    theme.set_typo(
        "label",
        TypoToken {
            family: Some("body".into()),
            ..TypoToken::plain(12.0)
        },
    );
    theme.set_typo(
        "body",
        TypoToken {
            family: Some("body".into()),
            ..TypoToken::plain(16.0)
        },
    );

    const SRC: &str = include_str!("../../byard-cli/examples/font_families/src/main.byd");
    let parsed = byard_compiler::parser::parse(SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = byard_compiler::interp::eval::Interpreter::new();
    interp.set_theme(theme);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let main = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == "Main")
        .unwrap();
    let tree = interp.lower_view(main, &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    interp.render(&tree, &mut frame, W as f32, H as f32);
    // Every line landed in a declared family, and both families are on the
    // screen. A screen where one of them silently went missing is exactly the
    // failure an example with no test cannot report.
    let families: Vec<&str> = frame
        .texts()
        .iter()
        .map(|t| t.family.as_deref().unwrap_or("<system>"))
        .collect();
    assert!(
        families.contains(&"Space Grotesk"),
        "no line reached the display face: {families:?}"
    );
    assert!(
        families.contains(&"Manrope"),
        "no line reached the body face: {families:?}"
    );
    assert!(
        !families.contains(&"<system>"),
        "a line fell through to the system font: {families:?}"
    );
    // And the weight axis still moves inside one family, which is the block
    // the example asks a reader to look at.
    let display_weights: Vec<u16> = frame
        .texts()
        .iter()
        .filter(|t| t.family.as_deref() == Some("Space Grotesk"))
        .map(|t| t.weight)
        .collect();
    for w in [300, 400, 700] {
        assert!(
            display_weights.contains(&w),
            "the display column must span the axis, got {display_weights:?}"
        );
    }

    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(&device),
        Arc::clone(&queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        W,
        H,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: W as f32,
            height: H as f32,
        },
        W,
        H,
        1.0,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: H,
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
    let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
    queue.submit(std::iter::once(cmd));
    let bpr = 256 * (W * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bpr) * u64::from(H),
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
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let mut out = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let src = (y * bpr) as usize;
        let dst = (y * W * 4) as usize;
        out[dst..dst + (W * 4) as usize].copy_from_slice(&data[src..src + (W * 4) as usize]);
    }
    // The readback itself asserts only that something arrived: a frame that
    // encoded to nothing is a frame the assertions above could still have
    // passed on.
    assert!(
        out.iter().any(|b| *b != 0),
        "the example encoded to an empty image"
    );
    if std::env::var("BYARD_DUMP_PNG").is_ok() {
        let path = std::env::var("BYARD_PNG_PATH")
            .unwrap_or_else(|_| "/tmp/byard_font_families.png".to_string());
        image::save_buffer(&path, &out, W, H, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote {path}");
    }
}
