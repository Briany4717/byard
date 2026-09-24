//! Path clip masks, in pixels (RFC-0037 `clip(path)`).
//!
//! The claim is the one a readback alone can settle: content inside an
//! arbitrary outline survives, content outside it does not, and the edge the
//! outline draws is antialiased like every other edge in the frame.
//!
//! Each assertion carries its control. A mask that swallowed everything would
//! also leave the cut corner empty, so the kept side is sampled too; and a
//! plain rectangle clip over the same box is drawn to prove it is the *path*
//! doing the cutting rather than the rectangle it is bounded by.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::many_single_char_names
)]

use std::sync::Arc;

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::encoder::canvas_fill::FillVertex;
use byard_core::frame::{ClipMask, FillMesh, Rect, RenderFrame, Transform, Viewport};

const SIZE: u32 = 128;
const AREA: [f32; 4] = [16.0, 16.0, 96.0, 96.0];
const FILL: [f32; 4] = [0.0, 0.55, 0.9, 1.0];

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("clip mask device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

fn encoder_sized(
    device: &Arc<wgpu::Device>,
    queue: &Arc<wgpu::Queue>,
    size: u32,
) -> EncoderSubsystem {
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        size,
        size,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: size as f32,
            height: size as f32,
        },
        size,
        size,
        1.0,
    );
    enc
}

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    encoder_sized(device, queue, SIZE)
}

/// Encodes `frame` and reads the whole target back as RGBA bytes.
fn render_sized(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
    size: u32,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("clip readback target"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
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

    let bpr = 256 * (size * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("clip readback"),
        size: u64::from(bpr) * u64::from(size),
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
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        let src = (y * bpr) as usize;
        let dst = (y * size * 4) as usize;
        out[dst..dst + (size * 4) as usize].copy_from_slice(&data[src..src + (size * 4) as usize]);
    }
    out
}

fn render(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> Vec<u8> {
    render_sized(enc, device, queue, frame, SIZE)
}

fn at(image: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let i = ((y * SIZE + x) * 4) as usize;
    (image[i], image[i + 1], image[i + 2], image[i + 3])
}

/// The frame's alpha as a coarse character map, attached to every failure.
///
/// Instrumentation rather than decoration: this path's shader work differs
/// between backends, and the one CI machine with D3D12 is the only place some
/// failures exist. A map of where coverage landed says which stage broke — a
/// uniformly empty frame, a mask sampled from the wrong place, a flipped axis —
/// in one CI round rather than three rounds of hypotheses drawn from re-reading
/// the diff.
fn alpha_map(image: &[u8]) -> String {
    const STEP: u32 = 4;
    let mut out = String::from("\nalpha map (4px cells, ' ' = 0, '#' = opaque):\n");
    for y in (0..SIZE).step_by(STEP as usize) {
        for x in (0..SIZE).step_by(STEP as usize) {
            let a = at(image, x, y).3;
            out.push(match a {
                0 => ' ',
                1..=63 => '.',
                64..=191 => '+',
                _ => '#',
            });
        }
        out.push('\n');
    }
    out
}

fn solid() -> BoxInstance {
    BoxInstance {
        rect: AREA,
        color: FILL,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    }
}

/// A triangle over `AREA`: top-left, top-right, bottom-left. Its hypotenuse is
/// the anti-diagonal, so it keeps the upper-left half of the box and cuts the
/// lower-right one along a line that is neither horizontal nor vertical, which
/// is the only kind of edge a scissor cannot fake.
fn triangle() -> ClipMask {
    let [x, y, w, h] = AREA;
    let v = |px: f32, py: f32| FillVertex {
        pos: [px, py],
        uv: [(px - x) / w, (py - y) / h],
    };
    ClipMask {
        mesh: Arc::new(FillMesh {
            vertices: vec![v(x, y), v(x + w, y), v(x, y + h)],
            indices: vec![0, 1, 2],
            bounds: AREA,
        }),
        bounds: Rect::new(x, y, w, h),
    }
}

fn path_clipped() -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    frame.begin_clip_path(triangle());
    frame.push_instance(solid());
    frame.end_clip();
    frame
}

fn rect_clipped() -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    let [x, y, w, h] = AREA;
    frame.begin_clip(Rect::new(x, y, w, h));
    frame.push_instance(solid());
    frame.end_clip();
    frame
}

/// The triangle keeps its side and cuts the other; the rectangle over the same
/// box keeps both.
#[test]
fn a_path_clip_keeps_its_inside_and_cuts_its_outside() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping path clip readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let masked = render(&mut enc, &device, &queue, &path_clipped());
    let mut enc = encoder(&device, &queue);
    let plain = render(&mut enc, &device, &queue, &rect_clipped());

    // Well inside the triangle, near the top-left corner.
    let kept = at(&masked, 30, 30);
    assert!(
        kept.3 > 200,
        "inside the path must be painted, got {kept:?}"
    );
    // Well past the hypotenuse, near the bottom-right corner.
    let cut = at(&masked, 100, 100);
    assert!(cut.3 < 20, "outside the path must be clipped, got {cut:?}");
    // The control: the rectangle clip paints that same corner, so the cut
    // above is the path's doing and not the bounding box's.
    let control = at(&plain, 100, 100);
    assert!(
        control.3 > 200,
        "a rectangle clip over the same box keeps the corner, got {control:?}"
    );
}

/// The hypotenuse is antialiased.
///
/// Scanned strictly inside the box's own edges, the lesson the rounded clip's
/// test already paid for: the box fills `AREA` exactly, so its left and top
/// edges are antialiased by the box itself, and a scan that reached them would
/// pass on a frame whose mask was a one-bit stencil.
#[test]
fn the_path_edge_is_antialiased() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping path clip readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let image = render(&mut enc, &device, &queue, &path_clipped());
    let [x, y, w, h] = AREA;
    let (x0, y0, x1, y1) = (
        x as u32 + 4,
        y as u32 + 4,
        (x + w) as u32 - 4,
        (y + h) as u32 - 4,
    );
    let partial = (y0..y1).any(|py| (x0..x1).any(|px| (20..=235).contains(&at(&image, px, py).3)));
    assert!(
        partial,
        "the path's edge must be antialiased: no partially covered pixel along it"
    );
}

/// A path clip nested in a rounded one is cut by both.
///
/// The coverages multiply, so neither edge is allowed to erase the other's
/// softness or its cut. Sampled at the rounded corner, which the triangle
/// keeps: if the path replaced the parent's corner instead of joining it, this
/// pixel would be painted.
#[test]
fn a_path_inside_a_rounded_clip_is_cut_by_both() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping nested clip readback");
        return;
    };
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    let [x, y, w, h] = AREA;
    frame.begin_clip_rounded(Rect::new(x, y, w, h), [32.0; 4]);
    frame.begin_clip_path(triangle());
    frame.push_instance(solid());
    frame.end_clip();
    frame.end_clip();
    let mut enc = encoder(&device, &queue);
    let image = render(&mut enc, &device, &queue, &frame);

    let corner = at(&image, 18, 18);
    assert!(
        corner.3 < 20,
        "the rounded parent's corner must still be cut, got {corner:?}"
    );
    let kept = at(&image, 40, 40);
    assert!(
        kept.3 > 200,
        "the shared inside must survive, got {kept:?}{}",
        alpha_map(&image)
    );
    let cut = at(&image, 100, 100);
    assert!(
        cut.3 < 20,
        "the path's outside must still be cut, got {cut:?}"
    );
}

/// A mask that is exactly its own bounds keeps the whole box.
///
/// A test in its own right (a mask with nothing to cut must cut nothing), and
/// also the half of a bisection: if this frame is empty on some backend while
/// the geometry is trivially a rectangle, the fault is in how the coverage is
/// *sampled*, not in how the path was tessellated or rasterised.
#[test]
fn a_mask_equal_to_its_bounds_keeps_everything() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping path clip readback");
        return;
    };
    let [x, y, w, h] = AREA;
    let v = |px: f32, py: f32| FillVertex {
        pos: [px, py],
        uv: [(px - x) / w, (py - y) / h],
    };
    let full = ClipMask {
        mesh: Arc::new(FillMesh {
            vertices: vec![v(x, y), v(x + w, y), v(x + w, y + h), v(x, y + h)],
            indices: vec![0, 1, 2, 0, 2, 3],
            bounds: AREA,
        }),
        bounds: Rect::new(x, y, w, h),
    };
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    frame.begin_clip_path(full);
    frame.push_instance(solid());
    frame.end_clip();
    let mut enc = encoder(&device, &queue);
    let image = render(&mut enc, &device, &queue, &frame);
    for (px, py) in [(30, 30), (100, 30), (30, 100), (100, 100), (64, 64)] {
        let p = at(&image, px, py);
        assert!(
            p.3 > 200,
            "a mask covering its whole bounds must keep ({px}, {py}), got {p:?}{}",
            alpha_map(&image)
        );
    }
}

/// A frame whose masks have not changed rasterises nothing, and is still
/// clipped correctly from the strip the previous frame left.
///
/// Both halves matter. The count on its own would pass on a frame that skipped
/// the pass and then sampled a stale or empty strip; the pixels on their own
/// would pass on a frame that paid for the pass every time.
#[test]
fn an_unchanged_mask_is_not_rasterised_again() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping path clip readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    // One mask, kept alive across frames the way the interpreter's mesh cache
    // keeps it: the same `Arc` every frame the outline does not move.
    let mask = triangle();
    let frame_with = |m: &ClipMask| {
        let mut frame = RenderFrame::new();
        frame.request_full_redraw();
        frame.begin_clip_path(m.clone());
        frame.push_instance(solid());
        frame.end_clip();
        frame
    };
    let _first = render(&mut enc, &device, &queue, &frame_with(&mask));
    let after_first = enc.clip_mask_rasterisations();
    let second = render(&mut enc, &device, &queue, &frame_with(&mask));
    assert_eq!(
        enc.clip_mask_rasterisations(),
        after_first,
        "a frame with the same mask must not rasterise it again"
    );
    assert!(
        at(&second, 30, 30).3 > 200 && at(&second, 100, 100).3 < 20,
        "and must still be clipped by it, from the strip it kept{}",
        alpha_map(&second)
    );
    // A different outline is a different mesh, and is drawn once.
    let _third = render(&mut enc, &device, &queue, &frame_with(&triangle()));
    assert_eq!(enc.clip_mask_rasterisations(), after_first + 1);
}
