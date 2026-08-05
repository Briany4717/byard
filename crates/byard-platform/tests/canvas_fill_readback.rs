//! GPU readback proofs for Tier-2 filled paths (RFC-0037).
//!
//! Three claims that only pixels can settle:
//!
//! 1. **A tessellated path fills its interior and nothing else.** The mesh is
//!    built on the CPU and drawn by a pipeline nothing else uses, so "the
//!    triangles are right" and "the triangles reached the screen" are separate
//!    questions, and this answers the second.
//! 2. **A path gradient fades across the path's own bounds**, in the direction
//!    the descriptor says. A `uv` measured against the wrong box is a fill
//!    that looks plausible and is wrong everywhere.
//! 3. **A path fill and a box fill with the same descriptor produce the same
//!    colour.** The anti-drift test the shared gradient block exists for: the
//!    two are compared to each other, not to a number somebody typed.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::encoder::canvas_fill::FillVertex;
use byard_core::frame::{
    CanvasFill, DecoratedBox, FillMesh, Gradient, GradientKind, RenderFrame, Transform, Viewport,
};
use std::sync::Arc;

const LOGICAL_W: f32 = 240.0;
const LOGICAL_H: f32 = 120.0;
const SCALE: f32 = 2.0;

/// The probe card: a wide 2:1 rect, so an aspect mistake is a large error
/// rather than a rounding one.
const CARD: [f32; 4] = [20.0, 20.0, 200.0, 80.0];

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("canvas fill readback device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

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

    /// The green channel at a logical point, `0..1`.
    fn green(&self, lx: f32, ly: f32) -> f32 {
        f32::from(self.at(lx, ly).1) / 255.0
    }

    /// The green channel decoded back to linear space, which is the space the
    /// stops were mixed in. Comparing an sRGB-encoded byte against a linear
    /// expectation is how a correct falloff reads as a wrong number.
    fn green_linear(&self, lx: f32, ly: f32) -> f32 {
        let c = self.green(lx, ly);
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
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
        label: Some("canvas fill target"),
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
        label: Some("canvas fill readback"),
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

/// A rectangle as a mesh: two triangles with `uv` spanning `0..1`, which is
/// exactly what the tessellator produces for a rectangular path and what a
/// gradient is measured against.
fn quad_mesh(rect: [f32; 4]) -> FillMesh {
    let [x, y, w, h] = rect;
    FillMesh {
        vertices: vec![
            FillVertex {
                pos: [x, y],
                uv: [0.0, 0.0],
            },
            FillVertex {
                pos: [x + w, y],
                uv: [1.0, 0.0],
            },
            FillVertex {
                pos: [x + w, y + h],
                uv: [1.0, 1.0],
            },
            FillVertex {
                pos: [x, y + h],
                uv: [0.0, 1.0],
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        bounds: rect,
    }
}

fn fill(rect: [f32; 4], color: [f32; 4], gradient: Option<Gradient>) -> CanvasFill {
    CanvasFill {
        mesh: Arc::new(quad_mesh(rect)),
        color,
        gradient,
        transform: Transform::IDENTITY,
        opacity: 1.0,
        dirty: true,
    }
}

/// A vertical ramp, green at the top and black at the bottom, with the mid
/// stop exactly halfway so the profile is linear in `t`.
fn vertical_ramp() -> Gradient {
    Gradient {
        kind: GradientKind::Linear,
        from: [0.0, 1.0, 0.0, 1.0],
        mid: [0.0, 0.5, 0.0, 1.0],
        to: [0.0, 0.0, 0.0, 1.0],
        mid_pos: 0.5,
        ..Gradient::two_stop(std::f32::consts::FRAC_PI_2, [0.0; 4], [0.0; 4])
    }
}

#[test]
fn a_filled_path_paints_its_interior_and_leaves_the_rest_alone() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-fill readback");
        return;
    };
    let mut frame = RenderFrame::new();
    frame.push_fill(fill(CARD, [0.0, 1.0, 0.0, 1.0], None));
    frame.request_full_redraw();
    let rb = render(&device, &queue, &frame);

    let inside = rb.green(CARD[0] + CARD[2] * 0.5, CARD[1] + CARD[3] * 0.5);
    assert!(
        inside > 0.9,
        "the middle of the fill is its colour: {inside}"
    );
    let outside = rb.green(CARD[0] - 8.0, CARD[1] - 8.0);
    assert!(outside < 0.1, "and outside it is untouched: {outside}");
}

#[test]
fn a_path_gradient_runs_across_the_paths_own_bounds() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-fill readback");
        return;
    };
    let mut frame = RenderFrame::new();
    frame.push_fill(fill(CARD, [0.0; 4], Some(vertical_ramp())));
    frame.request_full_redraw();
    let rb = render(&device, &queue, &frame);

    let x = CARD[0] + CARD[2] * 0.5;
    let top = rb.green_linear(x, CARD[1] + CARD[3] * 0.08);
    let middle = rb.green_linear(x, CARD[1] + CARD[3] * 0.5);
    let bottom = rb.green_linear(x, CARD[1] + CARD[3] * 0.92);
    assert!(
        top > middle && middle > bottom,
        "the ramp runs top to bottom across the path: {top} {middle} {bottom}"
    );
    assert!(
        (middle - 0.5).abs() < 0.12,
        "and the mid stop lands at the middle of the path, not of the canvas: {middle}"
    );
}

#[test]
fn a_path_gradient_and_a_box_gradient_agree() {
    // The reason the fragment block is shared rather than copied. Same
    // descriptor, same shape, same size: any difference here is drift, and
    // drift is what a second implementation guarantees eventually.
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-fill readback");
        return;
    };

    let mut box_frame = RenderFrame::new();
    box_frame.push_decorated(DecoratedBox {
        base: BoxInstance {
            rect: CARD,
            color: [0.0, 0.0, 0.0, 1.0],
            radii: [0.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        },
        gradient: Some(vertical_ramp()),
        opacity: 1.0,
        dirty: true,
        ..Default::default()
    });
    box_frame.request_full_redraw();
    let from_box = render(&device, &queue, &box_frame);

    let mut path_frame = RenderFrame::new();
    path_frame.push_fill(fill(CARD, [0.0, 0.0, 0.0, 1.0], Some(vertical_ramp())));
    path_frame.request_full_redraw();
    let from_path = render(&device, &queue, &path_frame);

    let x = CARD[0] + CARD[2] * 0.5;
    for fraction in [0.15_f32, 0.35, 0.5, 0.65, 0.85] {
        let y = CARD[1] + CARD[3] * fraction;
        let (a, b) = (from_box.green_linear(x, y), from_path.green_linear(x, y));
        assert!(
            (a - b).abs() < 0.03,
            "at {fraction} of the way down, a box reads {a} and a path {b}"
        );
    }
}

#[test]
fn a_fill_with_no_alpha_paints_nothing() {
    // The fragment discards rather than blending a transparent colour over the
    // scene, which is what keeps an empty series from costing a full-screen
    // blend.
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping canvas-fill readback");
        return;
    };
    let mut frame = RenderFrame::new();
    frame.push_fill(fill(CARD, [0.0, 1.0, 0.0, 0.0], None));
    frame.request_full_redraw();
    let rb = render(&device, &queue, &frame);
    let inside = rb.green(CARD[0] + CARD[2] * 0.5, CARD[1] + CARD[3] * 0.5);
    assert!(
        inside < 0.05,
        "a zero-alpha fill leaves the frame alone: {inside}"
    );
}
