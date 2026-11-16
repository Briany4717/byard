//! The opaque/transparent split at the edge of a shape (RFC-0017).
//!
//! The opaque pass draws, and writes depth for, only the fragments a solid box
//! fully covers. Its partially covered fringe (the anti-aliased edge, and the
//! body of a translucent box) is drawn with the transparent geometry, in paint
//! order against it. Before, the fringe wrote depth like the interior did, so a
//! translucent background drawn later in the pass was culled along it and the
//! fringe blended over nothing: on a transparent surface a 1 px row above and
//! below an opaque line carried the line's colour and its coverage as alpha,
//! with no background under it.
//!
//! Each case is checked against a reference built from the same primitives all
//! drawn as decorated boxes, which are transparent geometry drawn strictly in
//! paint order and never test each other's depth: what the split must
//! reproduce.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::sync::Arc;

use byard_core::BoxInstance;
use byard_core::color::{rgba, to_rgba};
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{DecoratedBox, RenderFrame, Transform, Viewport};

const W: u32 = 256;
const H: u32 = 256;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

/// RGBA rows of a `W` by `H` target at scale 1.
struct Pixels(Vec<u8>);

impl Pixels {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]]
    }
}

fn render(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, frame: &RenderFrame) -> Pixels {
    let fmt = wgpu::TextureFormat::Rgba8Unorm;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
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
        label: Some("srgb blending target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
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

    let bpr = W * 4;
    assert_eq!(bpr % 256, 0, "the row is already aligned");
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("srgb blending readback"),
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
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Pixels(data)
}

fn solid(rect: [f32; 4], color: [f32; 4], radius: f32) -> BoxInstance {
    BoxInstance {
        rect,
        color,
        radii: [radius; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    }
}

fn decorated(rect: [f32; 4], color: [f32; 4], radius: f32) -> DecoratedBox {
    DecoratedBox {
        base: solid(rect, color, radius),
        opacity: 1.0,
        dirty: true,
        ..Default::default()
    }
}

/// How a primitive of the scene is pushed: through the solid pool (as the
/// scene under test does) or as a decorated box (the reference).
#[derive(Clone, Copy, PartialEq)]
enum Pool {
    Solid,
    Decorated,
}

fn push(frame: &mut RenderFrame, pool: Pool, rect: [f32; 4], color: [f32; 4], radius: f32) {
    match pool {
        Pool::Solid => frame.push_instance(solid(rect, color, radius)),
        Pool::Decorated => frame.push_decorated(decorated(rect, color, radius)),
    }
}

/// The launcher's background colour, `0xF01C1B22`.
fn panel_bg() -> [f32; 4] {
    to_rgba(0xF01C_1B22, true)
}

/// The underline's colour, opaque.
fn line_colour() -> [f32; 4] {
    rgba(0x0000_6694)
}

/// A translucent rounded background with an opaque 2 px line at a fractional y
/// over it. The line goes through `line_pool`, the rest as given.
fn scene(opaque_window: bool, line_pool: Pool, scrim: Option<Pool>) -> RenderFrame {
    let mut frame = RenderFrame::new();
    if opaque_window {
        push(
            &mut frame,
            Pool::Solid,
            [0.0, 0.0, W as f32, H as f32],
            rgba(0x0040_5060),
            0.0,
        );
    }
    push(
        &mut frame,
        Pool::Decorated,
        [20.0, 20.0, 216.0, 216.0],
        panel_bg(),
        16.0,
    );
    push(
        &mut frame,
        line_pool,
        [40.0, 100.4, 120.3, 2.0],
        line_colour(),
        0.0,
    );
    if let Some(pool) = scrim {
        push(
            &mut frame,
            pool,
            [0.0, 0.0, W as f32, H as f32],
            to_rgba(0x8000_0000, true),
            0.0,
        );
    }
    frame
}

/// The rows around the line, over the columns it spans (with a little margin).
fn line_area() -> impl Iterator<Item = (u32, u32)> {
    (97..106).flat_map(|y| (36..166).map(move |x| (x, y)))
}

fn assert_close(got: &Pixels, want: &Pixels, tolerance: u8, what: &str) {
    let mut bad = Vec::new();
    for (x, y) in line_area() {
        let (g, w) = (got.at(x, y), want.at(x, y));
        if g.iter().zip(&w).any(|(a, b)| a.abs_diff(*b) > tolerance) {
            bad.push((x, y, g, w));
        }
    }
    assert!(
        bad.is_empty(),
        "{what}: {} pixels differ from the paint-order reference by more than {tolerance}, \
         first (x, y, got, want) {:?}",
        bad.len(),
        &bad[..bad.len().min(4)]
    );
}

/// The reported bug: on a transparent surface the line's antialiased rows keep
/// the background under them, alpha included, and carry the blend of the two.
#[test]
fn a_lines_fringe_on_a_translucent_background_keeps_the_background_under_it() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let got = render(&device, &queue, &scene(false, Pool::Solid, None));
    let want = render(&device, &queue, &scene(false, Pool::Decorated, None));
    let bg_alpha = got.at(30, 30)[3];
    assert!(bg_alpha > 200, "the background is translucent but strong");
    for (x, y) in line_area() {
        assert!(
            got.at(x, y)[3] >= bg_alpha,
            "({x}, {y}) has alpha {} under the background's {bg_alpha}: no background under it",
            got.at(x, y)[3]
        );
    }
    assert_close(&got, &want, 1, "transparent surface");
}

/// The same on an opaque window, where the missing background showed as a
/// darker edge: the picture is the one a straight paint-order draw gives.
#[test]
fn an_opaque_windows_picture_is_the_paint_order_reference_at_a_lines_fringe() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let got = render(&device, &queue, &scene(true, Pool::Solid, None));
    let want = render(&device, &queue, &scene(true, Pool::Decorated, None));
    assert_close(&got, &want, 1, "opaque window");
}

/// A scrim painted after the line darkens the line's fringe too: the fringe is
/// drawn before it, not over it.
#[test]
fn a_scrim_over_a_line_darkens_its_fringe() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    for scrim in [Pool::Solid, Pool::Decorated] {
        let with = render(&device, &queue, &scene(true, Pool::Solid, Some(scrim)));
        let want = render(
            &device,
            &queue,
            &scene(true, Pool::Decorated, Some(Pool::Decorated)),
        );
        assert_close(&with, &want, 1, "scrim over the line");
        let without = render(&device, &queue, &scene(true, Pool::Solid, None));
        // Row 99 is the line's top fringe row (the line starts at y = 100.4).
        let (w, wo) = (with.at(80, 99), without.at(80, 99));
        assert!(
            (0..3).all(|c| w[c] <= wo[c]) && (0..3).any(|c| w[c] < wo[c]),
            "the scrim did not darken the fringe: {w:?} vs {wo:?}"
        );
    }
}
