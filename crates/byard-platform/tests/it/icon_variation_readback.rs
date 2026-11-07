//! Variation axes, in pixels (RFC-0054).
//!
//! The compiler tests say the axes reach the `TextLine`; these say they reach
//! the screen, through the whole encoder: shaped by `cosmic-text`, drawn by
//! glyphon as custom glyphs, rasterised by swash with the axes set. The claim
//! is the narrow one a readback can carry: `FILL: 1` paints more ink than
//! `FILL: 0`, the same setting twice paints the same pixels, and a line with
//! no axes is not touched. With the custom path switched off, both lines are
//! drawn by the ordinary path and the first assertion fails.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{FontFace, FontTable, RenderFrame, TextLine, Viewport};

const SIZE: u32 = 256;

const ICONS: &[u8] = include_bytes!("../../../byard-cli/examples/assets/fonts/ByardIcons.ttf");

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
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
    enc
}

fn render(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("font readback target"),
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
    let cmd = enc.encode_frame_from_relay(&target, frame).unwrap();
    queue.submit(std::iter::once(cmd));

    let bpr = 256 * (SIZE * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("font readback"),
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
        let src = (y * bpr) as usize;
        let dst = (y * SIZE * 4) as usize;
        out[dst..dst + (SIZE * 4) as usize].copy_from_slice(&data[src..src + (SIZE * 4) as usize]);
    }
    out
}

fn frame(fill: Option<f32>) -> RenderFrame {
    let bytes: Arc<[u8]> = Arc::from(ICONS);
    let family = byard_core::text::family_name(&bytes).expect("the icon font parses");
    let mut fonts = FontTable::default();
    fonts.push(FontFace {
        declared: "symbols".to_string(),
        resolved: Arc::from(family.as_str()),
        bytes,
    });
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    frame.set_fonts(Arc::new(fonts));
    frame.push_text(TextLine {
        x: 40.0,
        y: 40.0,
        text: "favorite".to_string(),
        font_size: 128.0,
        weight: 400,
        family: Some(Arc::from(family.as_str())),
        variations: fill.map(|v| -> byard_core::frame::Variations { Arc::from([(*b"FILL", v)]) }),
        color: [1.0, 1.0, 1.0, 1.0],
        dirty: true,
    });
    frame
}

/// How many pixels differ from the top-left one, the background.
fn ink(img: &[u8]) -> usize {
    let bg = &img[..4];
    img.chunks(4).filter(|p| *p != bg).count()
}

fn differing(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).filter(|(x, y)| x != y).count()
}

#[test]
fn fill_one_paints_more_ink_than_fill_zero_and_never_leaks() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping variation readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let open = render(&mut enc, &device, &queue, &frame(Some(0.0)));
    let filled = render(&mut enc, &device, &queue, &frame(Some(1.0)));
    let open_again = render(&mut enc, &device, &queue, &frame(Some(0.0)));
    assert!(ink(&open) > 500, "the icon is drawn at all: {}", ink(&open));
    assert!(
        ink(&filled) > ink(&open) * 12 / 10,
        "FILL 1 painted {} ink pixels against {} at FILL 0",
        ink(&filled),
        ink(&open)
    );
    assert_eq!(
        differing(&open, &open_again),
        0,
        "A, B, A: the first setting leaked"
    );
    if std::env::var("BYARD_DUMP_PNG").is_ok() {
        for (name, img) in [("open", &open), ("filled", &filled)] {
            let path = format!("/tmp/byard_fill_{name}.png");
            image::save_buffer(&path, img, SIZE, SIZE, image::ColorType::Rgba8).expect("png");
        }
    }
}

/// A line with no `variations` is drawn by the ordinary path, which does not
/// know about axes: it paints the same pixels whether or not a variation line
/// was drawn in the frame before.
#[test]
fn a_line_without_variations_is_untouched() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping variation readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let plain = render(&mut enc, &device, &queue, &frame(None));
    let _ = render(&mut enc, &device, &queue, &frame(Some(1.0)));
    let plain_again = render(&mut enc, &device, &queue, &frame(None));
    assert!(ink(&plain) > 500);
    assert_eq!(differing(&plain, &plain_again), 0);
}
