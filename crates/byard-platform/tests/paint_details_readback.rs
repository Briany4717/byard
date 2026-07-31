//! GPU readback proofs for two `DecoratedBox` contracts (RFC-0001 §3.1):
//! an over-large corner radius is reduced to fit instead of deforming the box,
//! and a linear gradient paints a real ramp over the fill that a phase offset
//! travels along.
//!
//! Both are things only pixels can prove: the frame data is identical either
//! way, and it is the shader that gets them right or wrong.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{DecoratedBox, Gradient, RenderFrame, Transform, Viewport};
use std::sync::Arc;

const LOGICAL_W: f32 = 240.0;
const LOGICAL_H: f32 = 60.0;
const SCALE: f32 = 2.0;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("paint details readback device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

/// A read-back framebuffer, sampled in logical coordinates.
struct Readback {
    data: Vec<u8>,
    bpr: u32,
}

impl Readback {
    /// `(b, g, r, a)` at a logical point.
    fn at(&self, lx: f32, ly: f32) -> (u8, u8, u8, u8) {
        let px = (lx * SCALE) as u32;
        let py = (ly * SCALE) as u32;
        let idx = (py * self.bpr + px * 4) as usize;
        (
            self.data[idx],
            self.data[idx + 1],
            self.data[idx + 2],
            self.data[idx + 3],
        )
    }

    /// Perceived brightness at a logical point (the green channel is a fine
    /// stand-in for these greyscale/white-ramp cases).
    fn luma(&self, lx: f32, ly: f32) -> f32 {
        f32::from(self.at(lx, ly).1) / 255.0
    }
}

fn render(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, frame: &RenderFrame) -> Readback {
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
        label: Some("paint details target"),
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
        label: Some("paint details readback"),
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
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Readback { data, bpr }
}

#[test]
fn an_over_large_radius_is_reduced_to_a_pill_not_a_deformed_box() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping readback");
        return;
    };
    // The everyday case: a `radius: 20` pill on a 33 px-tall button. 20 exceeds
    // half the height (16.5), which the rounded-rect SDF cannot represent — it
    // used to pull the whole silhouette inward, most visibly at the left and
    // right ends.
    let (x, y, w, h) = (20.0_f32, 12.0_f32, 120.0_f32, 33.0_f32);
    let mut frame = RenderFrame::new();
    frame.push_instance(BoxInstance {
        rect: [x, y, w, h],
        color: [1.0, 1.0, 1.0, 1.0],
        radii: [20.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    });
    let rb = render(&device, &queue, &frame);

    // The vertical middle of each end must be painted: a stadium reaches its
    // full width at half height.
    let mid_y = y + h / 2.0;
    assert!(
        rb.at(x + 1.0, mid_y).3 > 200,
        "the left end reaches the rect edge, got {:?}",
        rb.at(x + 1.0, mid_y)
    );
    assert!(
        rb.at(x + w - 1.5, mid_y).3 > 200,
        "the right end reaches the rect edge, got {:?}",
        rb.at(x + w - 1.5, mid_y)
    );
    // …and the box is still *rounded*: its corners stay empty.
    assert!(
        rb.at(x + 1.0, y + 1.0).3 < 40,
        "the corner is still cut, got {:?}",
        rb.at(x + 1.0, y + 1.0)
    );

    // The one point that actually separates the two shapes: the *shoulder*,
    // where the unclamped field erodes the silhouette by ~3 px. Measured from
    // the box centre (80, 28.5) with half-extents (60, 16.5):
    //   • the true stadium (r reduced to 16.5) reaches |dx| = 43.5 + √(16.5² −
    //     15²) ≈ 50.4 at |dy| = 15;
    //   • the unclamped field (r = 20) reaches only |dx| = 40 + √(20² − 18.5²)
    //     ≈ 47.6 there — its boundary is an arc centred *outside* the box.
    // So (129, 43.5) is ~1.4 px inside the correct shape and ~1.4 px outside the
    // deformed one: painted now, empty before. Point-sampled rather than
    // row-scanned, so the claim is about the silhouette and not about how a
    // given rasteriser fills the interior.
    let shoulder = rb.at(x + 109.0, y + 31.5);
    assert!(
        shoulder.3 > 200,
        "the shoulder is filled, so the ends are not pinched inward, got {shoulder:?}"
    );
}

/// A dark bar with a transparent → white → transparent ramp across it, at the
/// given phase offset. `None` paints the same bar with no ramp at all — the
/// baseline every brightness claim below is measured against, so the assertions
/// never have to guess at sRGB encoding.
fn shimmer_frame(offset: Option<f32>) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.push_decorated(DecoratedBox {
        base: BoxInstance {
            rect: [20.0, 20.0, 200.0, 20.0],
            color: [0.05, 0.05, 0.05, 1.0],
            radii: [4.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        },
        opacity: 1.0,
        gradient: offset.map(|offset| Gradient {
            angle: 0.0, // left → right
            from: [1.0, 1.0, 1.0, 0.0],
            mid: [1.0, 1.0, 1.0, 0.9],
            to: [1.0, 1.0, 1.0, 0.0],
            mid_pos: 0.5,
            offset,
        }),
        dirty: true,
        ..Default::default()
    });
    frame
}

#[test]
fn a_gradient_paints_a_ramp_over_the_fill_and_its_offset_travels() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping readback");
        return;
    };
    let mid_y = 30.0;
    let flat = render(&device, &queue, &shimmer_frame(None));
    let base = flat.luma(120.0, mid_y);
    let rb = render(&device, &queue, &shimmer_frame(Some(0.0)));
    let (left, centre, right) = (
        rb.luma(24.0, mid_y),
        rb.luma(120.0, mid_y),
        rb.luma(215.0, mid_y),
    );
    assert!(
        centre > base + 0.3,
        "the band peaks in the middle, well above the bare fill ({base}): {centre}"
    );
    assert!(
        (left - base).abs() < 0.12 && (right - base).abs() < 0.12,
        "…and fades back to the bare fill ({base}) at both ends: {left} / {right}"
    );

    // A quarter-phase shift moves the band a quarter of the way along the ramp,
    // wrapping — which is what makes an animated offset a seamless sweep.
    let shifted = render(&device, &queue, &shimmer_frame(Some(0.25)));
    assert!(
        shifted.luma(70.0, mid_y) > shifted.luma(120.0, mid_y) + 0.2,
        "the band moved: 70px {} vs 120px {}",
        shifted.luma(70.0, mid_y),
        shifted.luma(120.0, mid_y)
    );
    // Wrapping is seamless: half a period later the band is at the other end,
    // and the ramp is still continuous (no black seam anywhere on the bar).
    let wrapped = render(&device, &queue, &shimmer_frame(Some(0.5)));
    for lx in 22..218 {
        let l = wrapped.luma(lx as f32 + 0.5, mid_y);
        let next = wrapped.luma(lx as f32 + 1.5, mid_y);
        assert!(
            (l - next).abs() < 0.12,
            "the wrapped ramp stays continuous at x={lx}: {l} → {next}"
        );
    }
}
