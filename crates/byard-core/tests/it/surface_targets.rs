//! One `Gpu`, several surfaces (RFC-0068 §5).
//!
//! The engine used to be one window's worth of state. These draw two (and
//! three) targets from a single `Gpu`, the way two windows or two bars of one
//! program will: what each target remembers between frames is its own, what is
//! shared is shared, and dropping one leaves the others drawing. They run on the
//! shared test device and skip gracefully when there is no adapter.
#![allow(clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_core::engine::Gpu;
use byard_core::frame::{BoxInstance, RenderFrame, TextLine, Transform};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

fn gpu(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, size: u32) -> Gpu {
    pollster::block_on(Gpu::new(
        Arc::clone(device),
        Arc::clone(queue),
        FORMAT,
        (size, size, 1.0),
        false,
    ))
    .expect("gpu")
}

/// What a surface's swapchain texture is: one per target, drawn into every frame.
fn texture(device: &wgpu::Device, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("surface target test texture"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// The RGBA8 pixel at `(x, y)` of a `size`-square texture.
fn pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: u32,
    (x, y): (u32, u32),
) -> [u8; 4] {
    let bpr = 256u32 * (size * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("surface target readback"),
        size: u64::from(bpr) * u64::from(size),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut ce = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    ce.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(size),
            },
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let i = (y * bpr + x * 4) as usize;
    [data[i], data[i + 1], data[i + 2], data[i + 3]]
}

fn solid(rect: [f32; 4], color: [f32; 4]) -> BoxInstance {
    BoxInstance {
        rect,
        color,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    }
}

fn frame(boxes: &[BoxInstance]) -> RenderFrame {
    let mut f = RenderFrame::new();
    for &b in boxes {
        f.push_instance(b);
    }
    f
}

fn text_frame(text: &str) -> RenderFrame {
    let mut f = RenderFrame::new();
    f.push_text(TextLine {
        align: byard_core::frame::TextAlign::Start,
        max_lines: 0,
        ellipsis: false,
        x: 2.0,
        y: 2.0,
        text: text.to_string(),
        font_size: 12.0,
        weight: 400,
        family: None,
        variations: None,
        color: [1.0; 4],
        dirty: true,
    });
    f
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn is(p: [u8; 4], c: [f32; 4]) -> bool {
    (0..3).all(|i| (f32::from(p[i]) / 255.0 - c[i]).abs() < 0.1) && p[3] > 240
}

#[test]
fn two_targets_draw_their_own_content_from_one_gpu() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(32, 32, 1.0);
    assert_eq!(gpu.target_count(), 2);
    let (tex_a, tex_b) = (texture(&device, 64), texture(&device, 32));

    gpu.render_target(a, &tex_a, &frame(&[solid([0.0, 0.0, 64.0, 64.0], RED)]))
        .unwrap();
    gpu.render_target(b, &tex_b, &frame(&[solid([0.0, 0.0, 32.0, 32.0], GREEN)]))
        .unwrap();

    assert!(is(pixel(&device, &queue, &tex_a, 64, (50, 50)), RED));
    assert!(is(pixel(&device, &queue, &tex_b, 32, (16, 16)), GREEN));
}

/// What a target drew last is its own: drawing another in between does not make
/// the first forget it, so it is still incremental and not a full redraw. A
/// state shared between the two would see a different size and start over.
#[test]
fn a_target_keeps_its_incremental_state_while_another_is_drawn() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(32, 32, 1.0);
    let (tex_a, tex_b) = (texture(&device, 64), texture(&device, 32));
    let scene_a = frame(&[solid([0.0, 0.0, 64.0, 64.0], RED)]);
    let scene_b = frame(&[solid([0.0, 0.0, 32.0, 32.0], GREEN)]);

    gpu.render_target(a, &tex_a, &scene_a).unwrap();
    assert!(
        gpu.encoder().last_frame_was_full_redraw(),
        "the first frame draws everything"
    );
    gpu.render_target(b, &tex_b, &scene_b).unwrap();
    assert!(
        gpu.encoder().last_frame_was_full_redraw(),
        "so does a new target's"
    );

    gpu.render_target(a, &tex_a, &scene_a).unwrap();
    assert!(
        !gpu.encoder().last_frame_was_full_redraw(),
        "a's second frame is incremental although b was drawn in between"
    );
    gpu.render_target(b, &tex_b, &scene_b).unwrap();
    assert!(!gpu.encoder().last_frame_was_full_redraw());
}

/// The viewport uniform is shared, so it is written again when the target being
/// drawn changes: a box half the size of its surface covers half of it, whichever
/// of two differently sized surfaces was drawn just before.
#[test]
fn each_target_is_drawn_at_its_own_size() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(48, 48, 1.0);
    let (tex_a, tex_b) = (texture(&device, 64), texture(&device, 48));
    let quarter = |size: f32, color| frame(&[solid([0.0, 0.0, size / 2.0, size / 2.0], color)]);

    gpu.render_target(a, &tex_a, &quarter(64.0, BLUE)).unwrap();
    gpu.render_target(b, &tex_b, &quarter(48.0, BLUE)).unwrap();
    // b's box ends at 24; the 64-pixel uniform would end it at 18.
    assert!(is(pixel(&device, &queue, &tex_b, 48, (21, 21)), BLUE));
    assert!(pixel(&device, &queue, &tex_b, 48, (30, 30))[3] < 10);

    // Back to a, with something to draw: its box ends at 32, not at the 42 the
    // 48-pixel uniform b left behind would give.
    gpu.render_target(a, &tex_a, &quarter(64.0, RED)).unwrap();
    assert!(is(pixel(&device, &queue, &tex_a, 64, (20, 20)), RED));
    let outside = pixel(&device, &queue, &tex_a, 64, (38, 38));
    assert!(outside[3] < 10, "a's box ends at 32: {outside:?}");
}

#[test]
fn resizing_one_target_leaves_the_other_alone() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(32, 32, 1.0);
    let (tex_a, tex_b) = (texture(&device, 64), texture(&device, 48));
    let scene = |size: f32| frame(&[solid([0.0, 0.0, size, size], RED)]);

    gpu.render_target(a, &tex_a, &scene(64.0)).unwrap();
    gpu.render_target(b, &texture(&device, 32), &scene(32.0))
        .unwrap();
    assert!(gpu.resize_target(b, 48, 48, 1.0));
    // Zero is what a minimised window reports: ignored, not an error.
    assert!(gpu.resize_target(b, 0, 0, 1.0));

    gpu.render_target(b, &tex_b, &scene(48.0)).unwrap();
    assert!(
        gpu.encoder().last_frame_was_full_redraw(),
        "the resized target starts over"
    );
    gpu.render_target(a, &tex_a, &scene(64.0)).unwrap();
    assert!(
        !gpu.encoder().last_frame_was_full_redraw(),
        "the other did not"
    );
    assert!(is(pixel(&device, &queue, &tex_b, 48, (47, 47)), RED));
}

#[test]
fn a_dropped_target_leaves_the_others_drawing() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(32, 32, 1.0);
    let c = gpu.add_target(16, 16, 1.0);
    let (tex_a, tex_b, tex_c) = (
        texture(&device, 64),
        texture(&device, 32),
        texture(&device, 16),
    );
    gpu.render_target(a, &tex_a, &frame(&[solid([0.0, 0.0, 64.0, 64.0], RED)]))
        .unwrap();
    gpu.render_target(b, &tex_b, &frame(&[solid([0.0, 0.0, 32.0, 32.0], GREEN)]))
        .unwrap();
    gpu.render_target(c, &tex_c, &frame(&[solid([0.0, 0.0, 16.0, 16.0], BLUE)]))
        .unwrap();

    assert!(gpu.drop_target(b));
    assert_eq!(gpu.target_count(), 2);
    assert!(!gpu.drop_target(b), "already gone");
    let err = gpu.render_target(b, &tex_b, &frame(&[])).unwrap_err();
    assert!(err.to_string().contains("dropped"), "{err}");

    // The first keeps drawing, a change reaches its pixels, and so does the third's.
    gpu.render_target(a, &tex_a, &frame(&[solid([0.0, 0.0, 64.0, 64.0], GREEN)]))
        .unwrap();
    assert!(is(pixel(&device, &queue, &tex_a, 64, (32, 32)), GREEN));
    gpu.render_target(c, &tex_c, &frame(&[solid([0.0, 0.0, 16.0, 16.0], RED)]))
        .unwrap();
    assert!(is(pixel(&device, &queue, &tex_c, 16, (8, 8)), RED));
}

#[test]
fn the_bound_target_can_be_dropped_and_the_last_one_cannot() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(32, 32, 1.0);
    let (tex_a, tex_b) = (texture(&device, 64), texture(&device, 32));
    // `a` is the one bound; dropping it hands the encoder to `b`.
    gpu.render_target(a, &tex_a, &frame(&[solid([0.0, 0.0, 64.0, 64.0], RED)]))
        .unwrap();
    assert!(gpu.drop_target(a));
    gpu.render_target(b, &tex_b, &frame(&[solid([0.0, 0.0, 32.0, 32.0], GREEN)]))
        .unwrap();
    assert!(is(pixel(&device, &queue, &tex_b, 32, (16, 16)), GREEN));
    assert!(!gpu.drop_target(b), "the last target stays");
    assert_eq!(gpu.target_count(), 1);
}

/// RFC-0068 §5: the glyph atlas is trimmed once per round, after every surface
/// that drew in it, and not between two of them. Outside a round every frame
/// trims, as it always did.
#[test]
fn the_glyph_atlas_is_trimmed_once_per_round() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(64, 64, 1.0);
    let (tex_a, tex_b) = (texture(&device, 64), texture(&device, 64));
    let before = gpu.encoder().glyph_atlas_trims();

    gpu.render_target(a, &tex_a, &text_frame("one")).unwrap();
    gpu.render_target(b, &tex_b, &text_frame("two")).unwrap();
    assert_eq!(
        gpu.encoder().glyph_atlas_trims() - before,
        2,
        "with no round open each frame trims"
    );

    let before = gpu.encoder().glyph_atlas_trims();
    gpu.begin_round();
    gpu.render_target(a, &tex_a, &text_frame("one!")).unwrap();
    gpu.render_target(b, &tex_b, &text_frame("two!")).unwrap();
    assert_eq!(
        gpu.encoder().glyph_atlas_trims() - before,
        0,
        "nothing is trimmed between two surfaces of a round"
    );
    gpu.end_round();
    assert_eq!(
        gpu.encoder().glyph_atlas_trims() - before,
        1,
        "one trim ends the round"
    );
    gpu.end_round();
    assert_eq!(
        gpu.encoder().glyph_atlas_trims() - before,
        1,
        "ending a round that is not open does nothing"
    );
}

/// Both surfaces' text is on screen after a round: the glyphs one uploaded were
/// not evicted before the other drew.
#[test]
fn both_surfaces_of_a_round_show_their_text() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping");
        return;
    };
    let mut gpu = gpu(&device, &queue, 64);
    let a = gpu.first_target();
    let b = gpu.add_target(64, 64, 1.0);
    let (tex_a, tex_b) = (texture(&device, 64), texture(&device, 64));
    gpu.begin_round();
    gpu.render_target(a, &tex_a, &text_frame("MMMM")).unwrap();
    gpu.render_target(b, &tex_b, &text_frame("MMMM")).unwrap();
    gpu.end_round();
    for (tex, who) in [(&tex_a, "first"), (&tex_b, "second")] {
        let lit = (0..16)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .any(|p| pixel(&device, &queue, tex, 64, p)[3] > 128);
        assert!(lit, "the {who} surface drew its text");
    }
}
