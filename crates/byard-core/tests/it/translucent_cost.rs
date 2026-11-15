//! What a page of translucent layers costs to draw, still and scrolling:
//! RFC-0060's cost section, measured. Prints; run with
//! `cargo test --release -p byard-core --test it translucent_cost -- --ignored --nocapture`.
//!
//! The page is Material's own: cards with an elevation shadow, a state layer
//! on each at 8% (Material's hover opacity), three chips at the 12%
//! container opacity, a 38% disabled label, and one 32% scrim over all of it,
//! at the sizes of a phone gallery page. Every one of them is a fragment
//! that is blended, which is where the encode costs.

#![allow(clippy::cast_precision_loss)]

use std::sync::Arc;
use std::time::Instant;

use byard_core::BoxInstance;
use byard_core::color::{rgba, to_rgba};
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{DecoratedBox, RenderFrame, Transform, Viewport};

const W: u32 = 1080;
const H: u32 = 1920;

fn bx(rect: [f32; 4], color: [f32; 4], radius: f32) -> BoxInstance {
    BoxInstance {
        rect,
        color,
        radii: [radius; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    }
}

/// 24 cards at the page's scroll `offset`, each with its layers.
fn page(offset: f32) -> RenderFrame {
    let mut f = RenderFrame::new();
    f.push_instance(bx([0.0, 0.0, W as f32, H as f32], rgba(0xFEF7FF), 0.0));
    for i in 0..24_u32 {
        let y = 24.0 + i as f32 * 168.0 - offset;
        let x = 24.0;
        let w = W as f32 - 48.0;
        // The card: surface container with the level-1 shadow.
        f.push_decorated(DecoratedBox {
            base: bx([x, y, w, 144.0], rgba(0xF3EDF7), 12.0),
            shadow_dy: 1.0,
            shadow_blur: 3.0,
            shadow_color: to_rgba(0x4D00_0000, true),
            opacity: 1.0,
            dirty: true,
            ..Default::default()
        });
        // Its state layer, 8% of the content colour.
        f.push_instance(bx([x, y, w, 144.0], to_rgba(0x141D_1B20, true), 12.0));
        // Three chips at the 12% container opacity, and a 38% label plate.
        for c in 0..3 {
            f.push_decorated(DecoratedBox {
                base: bx(
                    [x + 16.0 + c as f32 * 168.0, y + 16.0, 152.0, 32.0],
                    to_rgba(0x1F67_50A4, true),
                    8.0,
                ),
                border_width: 1.0,
                border_color: to_rgba(0x6179_747E, true),
                opacity: 1.0,
                dirty: true,
                ..Default::default()
            });
        }
        f.push_instance(bx(
            [x + 16.0, y + 64.0, w - 32.0, 64.0],
            to_rgba(0x611D_1B20, true),
            8.0,
        ));
    }
    // The scrim of a dialog, 32% black over the lot.
    f.push_instance(bx(
        [0.0, 0.0, W as f32, H as f32],
        to_rgba(0x5200_0000, true),
        0.0,
    ));
    f
}

fn time(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    frame: &RenderFrame,
) -> f64 {
    let t = Instant::now();
    let cmd = enc.encode_frame_from_relay(target, frame).expect("encode");
    queue.submit(std::iter::once(cmd));
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore = "prints timings"]
fn translucent_cost() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        return;
    };
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(&device),
        Arc::clone(&queue),
        wgpu::TextureFormat::Rgba8Unorm,
        1.0,
        W,
        H,
    ))
    .expect("encoder init");
    enc.update_viewport(Viewport::new(W as f32, H as f32), W, H, 1.0);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    for _ in 0..10 {
        let _ = time(&mut enc, &device, &queue, &target, &page(0.0));
    }
    let still = median(
        (0..101)
            .map(|_| time(&mut enc, &device, &queue, &target, &page(0.0)))
            .collect(),
    );
    let scroll = median(
        (0..201)
            .map(|k| time(&mut enc, &device, &queue, &target, &page(k as f32 * 3.7)))
            .collect(),
    );
    println!(
        "translucent page, {W}x{H}, 24 cards, 144 layers (shadows, state layers, chips) and a scrim: \
         still frame {still:.2} ms (median of 101), scrolling {scroll:.2} ms (median of 201)"
    );
}
