//! Rotated text on the GPU (RFC-0011): the ink turns, not just the boxes.
//!
//! The same word, set upright and at 90°, on a white page. Upright its ink is
//! a wide, short strip; turned it must be a tall, narrow one. The claim is on
//! the ink's extents, not on how much ink there is, so it holds whatever font
//! the machine resolves.
//!
//! `BYARD_DUMP_PNG=1` writes each frame out as well.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};

const W: u32 = 400;
const H: u32 = 400;

/// The ink's bounding box `[min_x, min_y, max_x, max_y]` of `src` rendered on
/// the GPU, or `None` without an adapter.
fn ink_box(src: &str, name: &str) -> Option<[u32; 4]> {
    let (device, queue, _turn) = byard_test_gpu::device(byard_core::engine::device_limits)?;
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
    interp.render(&tree, &mut frame, W as f32, H as f32);

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
    let mut img = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let s = (y * bpr) as usize;
        let d = (y * W * 4) as usize;
        img[d..d + (W * 4) as usize].copy_from_slice(&data[s..s + (W * 4) as usize]);
    }
    if std::env::var("BYARD_DUMP_PNG").is_ok() {
        image::save_buffer(
            format!("/tmp/byard_text_{name}.png"),
            &img,
            W,
            H,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    // Ink: any pixel clearly darker than the white page.
    let mut ink = [u32::MAX, u32::MAX, 0, 0];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if img[i] < 128 {
                ink = [ink[0].min(x), ink[1].min(y), ink[2].max(x), ink[3].max(y)];
            }
        }
    }
    assert!(ink[0] <= ink[2], "{name}: no ink at all");
    Some(ink)
}

const PAGE: &str = "View Main() { Column #[bg: 0xFFFFFF, width: 400, height: 400, \
                    justify: center, align: center] { TEXT } }";

#[test]
fn a_word_turned_ninety_degrees_stands_up() {
    let upright = PAGE.replace("TEXT", "Text(\"WWWWWWWW\") #[color: 0x000000, size: 28]");
    let turned = PAGE.replace(
        "TEXT",
        "Text(\"WWWWWWWW\") #[color: 0x000000, size: 28, rotate: 90deg]",
    );
    let Some(a) = ink_box(&upright, "upright") else {
        eprintln!("no GPU adapter, skipping text transform readback");
        return;
    };
    let b = ink_box(&turned, "turned").unwrap();
    let (aw, ah) = (a[2] - a[0], a[3] - a[1]);
    let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
    assert!(aw > 2 * ah, "upright ink should be wide: {aw}x{ah}");
    assert!(bh > 2 * bw, "turned ink should be tall: {bw}x{bh}");
    // A quarter turn swaps the extents; allow for the resample's soft edge.
    assert!(
        bh.abs_diff(aw) <= 4 && bw.abs_diff(ah) <= 4,
        "{aw}x{ah} vs {bw}x{bh}"
    );
}

#[test]
fn a_rotated_card_turns_the_label_inside_it() {
    let card = PAGE.replace(
        "TEXT",
        "Box #[width: 260, height: 50, rotate: 90deg, origin: center, justify: center, \
         align: center] { Text(\"WWWWWWWW\") #[color: 0x000000, size: 28] }",
    );
    let Some(b) = ink_box(&card, "card") else {
        eprintln!("no GPU adapter, skipping text transform readback");
        return;
    };
    let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
    assert!(bh > 2 * bw, "the label must turn with its card: {bw}x{bh}");
}
