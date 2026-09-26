//! The shipped RFC-0011 T4 example, rendered through the interpreter.
//!
//! Two cards with the same two crossing circles: the left one faded as a card
//! (one picture), the right one with the fade written on every piece (what the
//! left one used to be). The claim is read where the circles cross: on the
//! left the crossing looks exactly like the top circle alone, on the right it
//! does not. The sample points are taken from the frame's own circles rather
//! than from coordinates worked out by hand.
//!
//! `BYARD_DUMP_PNG=1` writes the frame out as well.
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

const W: u32 = 520;
const H: u32 = 400;

#[test]
fn the_faded_card_hides_its_overlap_and_the_per_piece_card_does_not() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        eprintln!("no GPU adapter, skipping group opacity example readback");
        return;
    };

    const SRC: &str = include_str!("../../byard-cli/examples/group_opacity/src/main.byd");
    let parsed = byard_compiler::parser::parse(SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = byard_compiler::interp::eval::Interpreter::new();
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
    assert_eq!(frame.groups().len(), 1, "exactly the left card is a group");

    // The four 80×80 circles, left pair first.
    let mut circles: Vec<[f32; 4]> = frame
        .instances()
        .iter()
        .map(|b| b.rect)
        .chain(frame.decorated().iter().map(|d| d.base.rect))
        .filter(|r| (r[2] - 80.0).abs() < 0.5 && (r[3] - 80.0).abs() < 0.5)
        .collect();
    circles.sort_by(|a, b| a[0].total_cmp(&b[0]));
    assert_eq!(circles.len(), 4, "{circles:?}");

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
            "/tmp/byard_group_opacity.png",
            &img,
            W,
            H,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    let px = |x: f32, y: f32| {
        let i = ((y as u32 * W + x as u32) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    };
    let close = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 4);

    // For each pair: the crossing is the midpoint between the two centres; the
    // top (blue) circle alone is its centre pushed 25px further right.
    let sample = |red: [f32; 4], blue: [f32; 4]| {
        let cy = red[1] + 40.0;
        let crossing = px((red[0] + 40.0 + blue[0] + 40.0) / 2.0, cy);
        let blue_only = px(blue[0] + 65.0, cy);
        (crossing, blue_only)
    };
    let (left_cross, left_blue) = sample(circles[0], circles[1]);
    let (right_cross, right_blue) = sample(circles[2], circles[3]);
    assert!(
        close(left_cross, left_blue),
        "the card faded as one picture must look like the top circle where they \
         cross: crossing {left_cross:?} vs top alone {left_blue:?}"
    );
    assert!(
        !close(right_cross, right_blue),
        "the control: faded piece by piece, the crossing must show both circles, \
         got crossing {right_cross:?} vs top alone {right_blue:?}"
    );
}
