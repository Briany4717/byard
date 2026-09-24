//! Group opacity, in pixels (RFC-0011 T4).
//!
//! The claim is about overlap and nothing else. Two half-opaque boxes drawn
//! one over the other darken where they cross; the same two faded *as one
//! picture* do not, because the fade is applied to an image in which they no
//! longer overlap. So the test reads the overlap and a point covered by only
//! the top box, and asks whether they differ: with no group they must, and in a
//! group they must not. Difference and identity, never an amount.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Transform, Viewport};

const SIZE: u32 = 128;
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.2, 1.0, 1.0];

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("group opacity device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
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
    let cmd = enc.encode_frame_from_relay(&target, frame).unwrap();
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
    out
}

fn at(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * SIZE + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
}

/// A red box at (16..80, 16..80) and a blue one over it at (48..112, 48..112),
/// both at half opacity, either multiplied into each box or faded as a group.
fn two_boxes(grouped: bool) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    let half = |mut c: [f32; 4]| {
        if !grouped {
            c[3] = 0.5;
        }
        c
    };
    let boxed = |rect: [f32; 4], color: [f32; 4]| BoxInstance {
        rect,
        color,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    };
    let opened = grouped && frame.begin_group(0.5);
    frame.push_instance(boxed([16.0, 16.0, 64.0, 64.0], half(RED)));
    frame.push_instance(boxed([48.0, 48.0, 64.0, 64.0], half(BLUE)));
    if opened {
        frame.end_group();
    }
    frame
}

#[test]
fn per_instance_opacity_shows_the_overlap() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping group opacity readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let img = render(&mut enc, &device, &queue, &two_boxes(false));
    let overlap = at(&img, 64, 64);
    let blue_only = at(&img, 100, 100);
    assert!(
        !close(overlap, blue_only),
        "the control: two half-opaque boxes drawn separately must show where \
         they cross, got overlap {overlap:?} vs blue alone {blue_only:?}"
    );
    assert!(
        !enc.has_group_target(),
        "a frame with no group must not allocate the offscreen target"
    );
}

#[test]
fn a_group_fades_as_one_picture_and_hides_the_overlap() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping group opacity readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let img = render(&mut enc, &device, &queue, &two_boxes(true));
    let overlap = at(&img, 64, 64);
    let blue_only = at(&img, 100, 100);
    let red_only = at(&img, 24, 24);
    assert!(
        blue_only[3] > 20 && red_only[3] > 20,
        "the group must draw something at all: blue {blue_only:?} red {red_only:?}"
    );
    assert!(
        close(overlap, blue_only),
        "faded as one picture, the overlap is simply the top box: overlap \
         {overlap:?} vs blue alone {blue_only:?}"
    );
    assert!(
        blue_only[3] < 250,
        "and it is faded, not opaque: {blue_only:?}"
    );
}

/// The composite sits in draw order: a box drawn after the group covers it,
/// and one drawn before is covered by it.
#[test]
fn a_group_composites_in_draw_order() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping group opacity readback");
        return;
    };
    let solid = |rect: [f32; 4], color: [f32; 4]| BoxInstance {
        rect,
        color,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    };
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    // Before: a green band under the group.
    frame.push_instance(solid([0.0, 56.0, 128.0, 16.0], [0.0, 1.0, 0.0, 1.0]));
    assert!(frame.begin_group(0.5));
    frame.push_instance(solid([32.0, 32.0, 64.0, 64.0], RED));
    frame.end_group();
    // After: a white band over it.
    frame.push_instance(solid([56.0, 0.0, 16.0, 128.0], [1.0, 1.0, 1.0, 1.0]));

    let mut enc = encoder(&device, &queue);
    let img = render(&mut enc, &device, &queue, &frame);
    let centre = at(&img, 64, 64);
    assert!(
        centre[0] > 240 && centre[1] > 240 && centre[2] > 240,
        "the box drawn after the group must cover it, got {centre:?}"
    );
    let through = at(&img, 40, 64);
    assert!(
        through[1] > 40 && through[0] > 40,
        "the green drawn before the group must show through the faded red, \
         got {through:?}"
    );
}
