//! RFC-0068 §3: a surface that blends with the desktop (a `Panel`) keeps its
//! alpha. Anything drawn over an opaque pixel leaves it opaque, including the
//! anti-aliased edge of a rounded shape: with `ALPHA_BLENDING`'s alpha
//! component the edge pixels came out at `a * a + (1 - a)`, 75% at half
//! coverage, and the desktop showed through them as a light hairline.
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

fn dot_alpha_edges(px: &Pixels, inside: [u32; 4]) -> Vec<(u32, u32, u8)> {
    let mut bad = Vec::new();
    for y in inside[1]..inside[1] + inside[3] {
        for x in inside[0]..inside[0] + inside[2] {
            let a = px.at(x, y)[3];
            if a != 255 {
                bad.push((x, y, a));
            }
        }
    }
    bad
}

/// An opaque card with a rounded box and a rounded, bordered one over it: every
/// pixel of the card stays opaque, edges included, through both pipelines.
#[test]
fn a_shape_over_an_opaque_background_leaves_the_alpha_at_255_at_its_edges() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let mut frame = RenderFrame::new();
    frame.push_instance(solid([16.0, 16.0, 224.0, 224.0], rgba(0x0020_2030), 0.0));
    // Translucent, rounded, at fractional-looking coverage on its curve.
    frame.push_instance(solid(
        [40.0, 40.0, 90.0, 90.0],
        to_rgba(0xCC33_99FF, true),
        45.0,
    ));
    frame.push_decorated(DecoratedBox {
        base: solid([140.0, 60.0, 80.0, 120.0], to_rgba(0xFF22_BB66, true), 30.0),
        opacity: 1.0,
        dirty: true,
        ..Default::default()
    });
    let px = render(&device, &queue, &frame);
    let bad = dot_alpha_edges(&px, [16, 16, 224, 224]);
    assert!(
        bad.is_empty(),
        "{} pixels of an opaque card are not opaque, first {:?}",
        bad.len(),
        &bad[..bad.len().min(5)]
    );
    // And outside the card nothing was drawn: the surface stays transparent.
    assert_eq!(px.at(4, 4)[3], 0);
}
