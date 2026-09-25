//! Every colour path paints the colour it was given.
//!
//! Text painted every mid-tone far too dark for months (its linear colour was
//! converted to sRGB twice), and no test noticed, because the pixel tests
//! asserted that ink arrived rather than which colour it was. This paints one
//! mid-grey, `0x69737F`, through every way a colour reaches the screen, and
//! requires the exact pixel back from each: a double or a missing conversion
//! moves a mid-tone by tens of levels, so the tolerance can be tight.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::items_after_statements
)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};

const W: u32 = 640;
const H: u32 = 100;

/// Eight 60px cells in a row, each painting 0x69737F a different way.
const SRC: &str = r"
View Main() {
    Row #[bg: 0x000000, p: 10, gap: 10, width: 640, height: 100] {
        Box #[bg: 0x69737F, width: 60, height: 60] {}
        Box #[bg: 0x69737F, radius: 12, width: 60, height: 60] {}
        Box #[bg: 0x000000, border: 0x69737F, border_width: 20, width: 60, height: 60] {}
        Box #[gradient: (angle: 90deg, from: 0xFF69737F, to: 0xFF69737F),
              width: 60, height: 60] {}
        Canvas #[width: 60, height: 60] { rect(x: 0, y: 0, w: 60, h: 60, fill: 0x69737F) }
        Canvas #[width: 60, height: 60] { circle(cx: 30, cy: 30, r: 28, fill: 0x69737F) }
        Canvas #[width: 60, height: 60] {
            arc(cx: 30, cy: 30, r: 18, stroke: 0x69737F, stroke_width: 20)
        }
        Canvas #[width: 60, height: 60] {
            path(fill: 0x69737F) { move(0, 0) line(60, 0) line(60, 60) line(0, 60) close() }
        }
    }
}
";

#[test]
fn every_colour_path_paints_the_colour_it_was_given() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        eprintln!("no GPU adapter, skipping colour truth readback");
        return;
    };

    let parsed = byard_compiler::parser::parse(SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = byard_compiler::interp::eval::Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    // Tessellated paths land a frame late (the mesh is built on the logic
    // side and cached); two ticks is the steady state an app shows.
    let mut frame = RenderFrame::new();
    for _ in 0..2 {
        interp.tick();
        frame = RenderFrame::new();
        frame.request_full_redraw();
        interp.render(&tree, &mut frame, W as f32, H as f32);
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
    let mut img = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        let s = (y * bpr) as usize;
        let d = (y * W * 4) as usize;
        img[d..d + (W * 4) as usize].copy_from_slice(&data[s..s + (W * 4) as usize]);
    }

    let px = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    };
    let want = [105u8, 115, 127];
    let cells = [
        ("box", 40),
        ("rounded box", 110),
        ("border", 155),
        ("gradient", 250),
        ("canvas rect", 320),
        ("canvas circle", 390),
        ("canvas arc stroke", 478),
        ("filled path", 530),
    ];
    let wrong: Vec<String> = cells
        .iter()
        .map(|(name, x)| (name, px(*x, 40)))
        .filter(|(_, got)| got.iter().zip(want).any(|(a, b)| a.abs_diff(b) > 3))
        .map(|(name, got)| format!("{name}: {got:?}"))
        .collect();
    assert!(
        wrong.is_empty(),
        "expected {want:?} everywhere, got {wrong:?}"
    );
}
