//! GPU readback proof for a gap in a border (RFC-0064): the border's top edge
//! is missing between `start` and `start + length`, and what is under it shows;
//! everywhere else the border is where it was. The frame data is identical
//! whether the shader reads the gap or not, so only pixels settle it.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{DecoratedBox, RenderFrame, Transform, Viewport};
use std::sync::Arc;

const LOGICAL: f32 = 160.0;
const SCALE: f32 = 4.0;
const X: f32 = 30.0;
const Y: f32 = 30.0;
const W: f32 = 100.0;
const H: f32 = 80.0;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

struct Readback {
    data: Vec<u8>,
    bpr: u32,
}

impl Readback {
    /// Coverage `0..=1` at a logical point, read from the alpha channel.
    fn alpha(&self, lx: f32, ly: f32) -> f32 {
        let px = (lx * SCALE) as u32;
        let py = (ly * SCALE) as u32;
        let idx = (py * self.bpr + px * 4) as usize;
        f32::from(self.data[idx + 3]) / 255.0
    }
}

fn render(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, frame: &RenderFrame) -> Readback {
    let phys = (LOGICAL * SCALE) as u32;
    let fmt = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
        SCALE,
        phys,
        phys,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: LOGICAL,
            height: LOGICAL,
        },
        phys,
        phys,
        SCALE,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("border gap target"),
        size: wgpu::Extent3d {
            width: phys,
            height: phys,
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

    let bpr = 256 * (phys * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("border gap readback"),
        size: u64::from(bpr) * u64::from(phys),
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
                rows_per_image: Some(phys),
            },
        },
        wgpu::Extent3d {
            width: phys,
            height: phys,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Readback { data, bpr }
}

/// A 100 by 80 box with a 2 px border, a gap in its top edge from 20 to 60.
/// The interior is transparent, as an opaque fill's border overlay is.
fn frame(gap: [f32; 2]) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.push_decorated(DecoratedBox {
        base: BoxInstance {
            rect: [X, Y, W, H],
            color: [0.0; 4],
            radii: [6.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        },
        border_width: 2.0,
        border_color: [1.0, 1.0, 1.0, 1.0],
        border_gap: gap,
        opacity: 1.0,
        dirty: true,
        ..Default::default()
    });
    frame
}

#[test]
fn the_top_edge_is_missing_inside_the_gap_and_present_outside_it() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let with = render(&device, &queue, &frame([20.0, 40.0]));
    let without = render(&device, &queue, &frame([0.0, 0.0]));
    // One pixel inside the top edge.
    let top = Y + 1.0;
    // Inside the gap: no border with it, a border without.
    assert!(
        with.alpha(X + 40.0, top) < 0.05,
        "{}",
        with.alpha(X + 40.0, top)
    );
    assert!(without.alpha(X + 40.0, top) > 0.95);
    // Either side of the gap the border is untouched.
    assert!(with.alpha(X + 12.0, top) > 0.95, "left of it");
    assert!(with.alpha(X + 70.0, top) > 0.95, "right of it");
    // The other edges are never touched.
    assert!(with.alpha(X + 40.0, Y + H - 1.0) > 0.95, "the bottom edge");
    assert!(with.alpha(X + 1.0, Y + 40.0) > 0.95, "the left edge");
    assert!(with.alpha(X + W - 1.0, Y + 40.0) > 0.95, "the right edge");
    // And the gap does not eat into the inside, which was empty anyway.
    assert!(with.alpha(X + 40.0, Y + 10.0) < 0.05);
}

#[test]
fn the_edges_of_the_gap_are_anti_aliased_not_cut() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let with = render(&device, &queue, &frame([20.0, 40.0]));
    let top = Y + 1.0;
    // Half a pixel either way of the start: coverage is part-way.
    let a: Vec<f32> = (0..8)
        .map(|i| with.alpha(X + 19.0 + i as f32 * 0.25, top))
        .collect();
    assert!(a.windows(2).all(|w| w[1] <= w[0] + 0.02), "falls: {a:?}");
    assert!(a[0] > 0.9 && a[7] < 0.1, "{a:?}");
}
