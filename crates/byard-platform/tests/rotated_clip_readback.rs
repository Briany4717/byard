//! A `Clip` under a rotated ancestor cuts along the edges it is drawn with
//! (RFC-0011 × RFC-0037).
//!
//! Before this, the clip under a rotated card was the *axis-aligned bounding
//! box* of where the card went: the scissor and the clip table can only hold
//! axis-aligned rectangles. Content overflowing a rotated card therefore
//! showed in the four triangles between the card's edges and that box, which
//! is the defect a readback is the only honest way to see.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};

const SIZE: u32 = 300;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

fn render(src: &str) -> Option<Vec<u8>> {
    let (device, queue, _turn) = try_device()?;
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
        Arc::clone(&device),
        Arc::clone(&queue),
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
        label: None,
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
    let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
    queue.submit(std::iter::once(cmd));
    let bpr = 256 * (SIZE * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
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
        let s = (y * bpr) as usize;
        let d = (y * SIZE * 4) as usize;
        out[d..d + (SIZE * 4) as usize].copy_from_slice(&data[s..s + (SIZE * 4) as usize]);
    }
    if std::env::var("BYARD_DUMP_PNG").is_ok() {
        image::save_buffer(
            "/tmp/byard_rotated_clip.png",
            &out,
            SIZE,
            SIZE,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    Some(out)
}

fn red_at(img: &[u8], x: u32, y: u32) -> u8 {
    img[((y * SIZE + x) * 4) as usize]
}

/// A 100×100 clip centred at (150, 150), turned 45° by its parent, holding a
/// red box twice its size. The rotated square is a diamond whose corners
/// point up, down, left and right, 70.7px from the centre.
fn source(rotate: &str) -> String {
    format!(
        "View Main() {{
    Column #[width: 300, height: 300, bg: 0x000000] {{
        Box #[m: (left: 100, top: 100), width: 100, height: 100,
              rotate: {rotate}, origin: center] {{
            Clip #[width: 100, height: 100] {{
                Box #[width: 200, height: 200, bg: 0xFF0000] {{}}
            }}
        }}
    }}
}}"
    )
}

#[test]
fn a_clip_under_a_rotated_parent_cuts_along_the_rotated_edges() {
    let Some(img) = render(&source("45deg")) else {
        eprintln!("no GPU adapter, skipping rotated clip readback");
        return;
    };
    // The centre is inside the rotated square.
    assert!(
        red_at(&img, 150, 150) > 200,
        "the clip's inside must be painted"
    );
    // Two points that separate the rotated outline from what an axis-aligned
    // clip does here. The axis-aligned version was not even the bounding box:
    // it put a 100×100 box at the *transformed* top-left corner, so it kept a
    // triangle to the right of the centre and lost the left half entirely.
    //
    // (110, 150) is 40px left of the centre, inside the diamond
    // (|dx| + |dy| = 40 < 70.7) and outside that shifted box.
    assert!(
        red_at(&img, 110, 150) > 200,
        "the rotated square's left half must be painted, got red {}",
        red_at(&img, 110, 150)
    );
    // (235, 170) is outside the diamond (|dx| + |dy| = 105) but inside the
    // shifted box and under the content, so an axis-aligned clip paints it.
    assert!(
        red_at(&img, 235, 170) < 20,
        "a point outside the rotated square must be clipped, got red {}",
        red_at(&img, 235, 170)
    );
}

/// The control: unrotated, the same corner of the same region is inside the
/// clip and is painted, so the assertion above is about the rotation and not
/// about a clip that swallowed that corner for some other reason.
#[test]
fn the_same_point_is_inside_the_clip_when_nothing_rotates() {
    let Some(img) = render(&source("0deg")) else {
        eprintln!("no GPU adapter, skipping rotated clip readback");
        return;
    };
    assert!(
        red_at(&img, 195, 105) > 200,
        "unrotated, the clip is the 100..200 square and this point is inside it"
    );
}
