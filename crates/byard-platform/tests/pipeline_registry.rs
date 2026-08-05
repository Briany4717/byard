//! The pipeline registry, against a real device (RFC-0039 §"Pipeline
//! registration").
//!
//! Two claims that only a running encoder can settle, because both are about
//! what the *frame* does rather than what the registry contains:
//!
//! 1. **The dynamic dispatch is per-pipeline, not per-instance** (INV-30). This
//!    is the sentence the whole ABI rests on: a package's pipeline is as cheap
//!    as a core one because the only indirect call chooses *which* pipeline
//!    runs. A frame with ten thousand boxes and a frame with ten make the same
//!    number of those calls, or the claim is false.
//! 2. **The order is declared and reproducible** (INV-32), including across
//!    encoders built independently — the property a `HashMap` of pipelines
//!    would not have.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Transform, Viewport};
use std::sync::Arc;

const LOGICAL: f32 = 200.0;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("pipeline registry device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    let phys = LOGICAL as u32;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Bgra8UnormSrgb,
        1.0,
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
        1.0,
    );
    enc
}

/// A frame of `n` overlapping boxes.
fn boxes(n: usize) -> RenderFrame {
    let mut frame = RenderFrame::new();
    for i in 0..n {
        let x = (i % 100) as f32;
        let y = ((i / 100) % 100) as f32;
        frame.push_instance(BoxInstance {
            rect: [x, y, 4.0, 4.0],
            color: [0.2, 0.4, 0.8, 1.0],
            radii: [1.0; 4],
            transform: Transform::IDENTITY,
            smooth: 0.0,
        });
    }
    frame
}

fn encode(enc: &mut EncoderSubsystem, device: &wgpu::Device, queue: &wgpu::Queue, n: usize) -> u32 {
    let phys = LOGICAL as u32;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("registry target"),
        size: wgpu::Extent3d {
            width: phys,
            height: phys,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let frame = boxes(n);
    let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
    queue.submit(std::iter::once(cmd));
    enc.pipeline_dispatches()
}

#[test]
fn dispatch_cost_tracks_the_pipeline_count_and_not_the_instance_count() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping registry test");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let few = encode(&mut enc, &device, &queue, 10);
    let many = encode(&mut enc, &device, &queue, 10_000);
    assert_eq!(
        few, many,
        "a thousand times the instances made {many} erased calls against {few}: \
         the per-instance path is going through the trait object (INV-30)"
    );
    assert_eq!(
        few,
        enc.pipeline_order().len() as u32,
        "one call per registered pipeline per segment, and this frame is one segment"
    );
}

#[test]
fn the_draw_order_is_declared_and_two_encoders_agree_on_it() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping registry test");
        return;
    };
    let first = encoder(&device, &queue);
    let second = encoder(&device, &queue);
    assert_eq!(
        first.pipeline_order(),
        second.pipeline_order(),
        "two encoders built the same way draw in the same order (INV-32)"
    );
    // The historical order, which is what made the registry a parity change:
    // these are the same pipelines, drawn at the same points, as before it
    // existed. `canvas_fill` (RFC-0037) is the first pipeline that is not one
    // of them, and it is *after* them, which is where a registration lands
    // that did not exist when the order was written down (INV-32).
    assert_eq!(
        first.pipeline_order(),
        vec![
            "solid_box",
            "decorated_box",
            "ripple",
            "canvas_shape",
            "texture_sampler",
            "vector_msdf",
            "canvas_fill",
        ]
    );
}
