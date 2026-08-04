//! GPU readback proofs for RFC-0025 looping animations.
//!
//! The compiler-side tests read the *frame data* an animation produces; these
//! run the same `.byd` sources through the real encoder on a real device and
//! read the pixels back, so what is asserted is what a user would see: a box
//! that actually moves across the screen, wraps at the end of its period,
//! reverses on alternate plays, and walks its keyframe steps.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use std::sync::Arc;

const LOGICAL_W: f32 = 320.0;
const LOGICAL_H: f32 = 80.0;
const SCALE: f32 = 2.0;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("looping animation readback device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

/// Renders `frame` off-screen and returns the horizontal centre (in logical px)
/// of the painted white marker, "where is the box right now?", measured in
/// pixels rather than inferred from frame data.
fn marker_center_x(
    device: &Arc<wgpu::Device>,
    queue: &Arc<wgpu::Queue>,
    frame: &RenderFrame,
) -> f32 {
    let phys_w = (LOGICAL_W * SCALE) as u32;
    let phys_h = (LOGICAL_H * SCALE) as u32;
    let fmt = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
        SCALE,
        phys_w,
        phys_h,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: LOGICAL_W,
            height: LOGICAL_H,
        },
        phys_w,
        phys_h,
        SCALE,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("looping animation target"),
        size: wgpu::Extent3d {
            width: phys_w,
            height: phys_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: fmt,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let cmd = enc.encode_frame_from_relay(&target, frame).unwrap();
    queue.submit(std::iter::once(cmd));

    let bpr = 256 * (phys_w * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("looping animation readback"),
        size: u64::from(bpr) * u64::from(phys_h),
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
                rows_per_image: Some(phys_h),
            },
        },
        wgpu::Extent3d {
            width: phys_w,
            height: phys_h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();

    // Scan the marker's row for bright pixels and average their positions: the
    // painted centroid, which is exactly what the eye reads as "the position".
    let row = phys_h / 2;
    let mut sum = 0.0_f64;
    let mut count = 0.0_f64;
    for x in 0..phys_w {
        let idx = (row * bpr + x * 4) as usize;
        let (b, g, r) = (data[idx], data[idx + 1], data[idx + 2]);
        if b > 200 && g > 200 && r > 200 {
            sum += f64::from(x);
            count += 1.0;
        }
    }
    assert!(count > 0.0, "the animated marker must paint somewhere");
    (sum / count) as f32 / SCALE
}

/// Renders `src` at each engine time in `times` and returns the on-screen
/// centre of the marker at each.
fn positions_over_time(src: &str, times: &[u32]) -> Vec<f32> {
    let Some((device, queue)) = try_device() else {
        return Vec::new();
    };
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    times
        .iter()
        .map(|ms| {
            interp.set_now_ms(*ms);
            let mut frame = RenderFrame::new();
            interp.render(&tree, &mut frame, LOGICAL_W, LOGICAL_H);
            assert!(
                interp.has_active_animations(),
                "a looping animation must keep the frames coming"
            );
            marker_center_x(&device, &queue, &frame)
        })
        .collect()
}

/// A 20×20 white marker inside a dark row, translated by `anim`.
fn marker_source(anim: &str) -> String {
    format!(
        "View V() {{ \
         Row #[bg: 0x101014, width: 320, height: 80, p: (30, 10)] {{ \
             Box #[bg: 0xFFFFFF, width: 20, height: 20, {anim}] {{}} \
         }} }}"
    )
}

#[test]
fn an_infinite_loop_moves_the_pixels_and_wraps_at_its_period() {
    // 400 ms linear sweep of 200 px, forever.
    let src =
        marker_source("translate: (200, 0) with anim.linear(400ms, from: 0, repeat: infinite)");
    let seen = positions_over_time(&src, &[0, 200, 400, 600]);
    if seen.is_empty() {
        eprintln!("no GPU adapter, skipping readback");
        return;
    }
    assert!(
        (seen[1] - seen[0] - 100.0).abs() < 6.0,
        "halfway through the period the marker has moved ~100 px: {seen:?}"
    );
    assert!(
        (seen[2] - seen[0]).abs() < 6.0,
        "at the period it is back where it started: {seen:?}"
    );
    assert!(
        (seen[3] - seen[1]).abs() < 6.0,
        "and the second play repeats the first: {seen:?}"
    );
}

#[test]
fn reverse_plays_the_second_iteration_backwards_on_screen() {
    let src = marker_source(
        "translate: (200, 0) with anim.linear(400ms, from: 0, repeat: infinite, reverse: true)",
    );
    let seen = positions_over_time(&src, &[0, 200, 400, 600, 800]);
    if seen.is_empty() {
        eprintln!("no GPU adapter, skipping readback");
        return;
    }
    assert!(seen[1] > seen[0] + 90.0, "out: {seen:?}");
    assert!(seen[2] > seen[1] + 90.0, "at the far end: {seen:?}");
    assert!(
        (seen[3] - seen[1]).abs() < 6.0,
        "coming back through the midpoint: {seen:?}"
    );
    assert!(
        (seen[4] - seen[0]).abs() < 6.0,
        "home again after two plays: {seen:?}"
    );
}

#[test]
fn keyframes_walk_their_steps_on_screen() {
    // Out to +160 px at the 50 % step, back to the start at 100 %.
    let src = marker_source(
        "translate: anim.keyframes(0%: (0, 0), 50%: (160, 0), 100%: (0, 0), \
         duration: 400ms, loop: true)",
    );
    let seen = positions_over_time(&src, &[0, 100, 200, 300, 400]);
    if seen.is_empty() {
        eprintln!("no GPU adapter, skipping readback");
        return;
    }
    assert!(
        (seen[1] - seen[0] - 80.0).abs() < 6.0,
        "midway to the 50 % step: {seen:?}"
    );
    assert!(
        (seen[2] - seen[0] - 160.0).abs() < 6.0,
        "at the 50 % step: {seen:?}"
    );
    assert!(
        (seen[3] - seen[1]).abs() < 6.0,
        "symmetric on the way back: {seen:?}"
    );
    assert!(
        (seen[4] - seen[0]).abs() < 6.0,
        "wrapped to the 0 % step: {seen:?}"
    );
}
