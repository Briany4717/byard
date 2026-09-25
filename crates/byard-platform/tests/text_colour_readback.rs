//! Text colour, in pixels.
//!
//! A `TextLine` carries its colour as linear floats, like every primitive on
//! the frame, and `glyphon` expects sRGB bytes, which it linearises in its
//! shader. Handing it bytes built from the linear floats linearised every text
//! colour twice: white and black survive, and every mid-tone came out far too
//! dark (a caption grey written as `0x69737F` painted at about a third of its
//! lightness). The claim is the exact one: where a glyph fully covers a pixel,
//! that pixel is the colour a box of the same hex paints.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, TextLine, Viewport};

const SIZE: u32 = 256;

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
        label: Some("text colour target"),
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
        label: Some("text colour readback"),
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

/// The colour at `(x, y)` of a readback.
fn pixel(px: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * SIZE + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2]]
}

#[test]
fn a_glyph_paints_the_colour_a_box_of_the_same_hex_paints() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping text colour readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    // A mid-tone, where a double conversion is largest.
    let colour = byard_core::color::to_rgba(0x0069_737F, false);
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    frame.push_instance(byard_core::BoxInstance {
        rect: [0.0, 0.0, SIZE as f32, SIZE as f32],
        color: [0.0, 0.0, 0.0, 1.0],
        radii: [0.0; 4],
        transform: byard_core::frame::Transform::IDENTITY,
        smooth: 0.0,
    });
    frame.push_instance(byard_core::BoxInstance {
        rect: [200.0, 20.0, 40.0, 40.0],
        color: colour,
        radii: [0.0; 4],
        transform: byard_core::frame::Transform::IDENTITY,
        smooth: 0.0,
    });
    frame.push_text(TextLine {
        x: 10.0,
        y: 10.0,
        text: "I".to_string(),
        font_size: 160.0,
        weight: 900,
        family: None,
        color: colour,
        dirty: true,
    });
    let px = render(&mut enc, &device, &queue, &frame);

    let reference = pixel(&px, 220, 40);
    // The brightest pixel in the glyph's region is one it covers fully.
    let mut brightest = [0u8; 3];
    for y in 0..SIZE {
        for x in 0..180 {
            let p = pixel(&px, x, y);
            if u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])
                > u32::from(brightest[0]) + u32::from(brightest[1]) + u32::from(brightest[2])
            {
                brightest = p;
            }
        }
    }
    let close = brightest
        .iter()
        .zip(reference)
        .all(|(a, b)| a.abs_diff(b) <= 3);
    assert!(
        close,
        "a fully covered glyph pixel is {brightest:?}, a box of the same hex is {reference:?}"
    );
}
