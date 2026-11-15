//! How long a page of text takes to draw, cold and scrolling. Prints; run with
//! `cargo test --release -p byard-core --test it scroll_cost -- --ignored --nocapture`.

#![allow(clippy::cast_precision_loss)]

use std::sync::Arc;
use std::time::Instant;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{FontFace, FontTable, RenderFrame, TextLine, Viewport};

const BODY: &[u8] = include_bytes!("../../../byard-cli/examples/assets/fonts/Manrope-Variable.ttf");

const SIZE: u32 = 512;

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Rgba8Unorm,
        1.0,
        SIZE,
        SIZE,
    ))
    .expect("encoder init");
    #[allow(clippy::cast_precision_loss)]
    enc.update_viewport(Viewport::new(SIZE as f32, SIZE as f32), SIZE, SIZE, 1.0);
    enc
}

fn page(table: &Arc<FontTable>, family: Option<&Arc<str>>, offset: f32) -> RenderFrame {
    let mut f = RenderFrame::new();
    f.set_fonts(Arc::clone(table));
    for i in 0..200 {
        f.push_text(TextLine {
            align: byard_core::frame::TextAlign::Start,
            max_lines: 0,
            ellipsis: false,
            x: 8.0,
            y: i as f32 * 20.0 - offset,
            text: format!("Row {i}: the quick brown fox jumps over the lazy dog 0123456789"),
            font_size: 14.0,
            weight: 400,
            family: family.cloned(),
            variations: None,
            color: [1.0, 1.0, 1.0, 1.0],
            dirty: true,
        });
    }
    f
}

fn time(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> f64 {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let t = Instant::now();
    let cmd = enc.encode_frame_from_relay(&target, frame).expect("encode");
    queue.submit(std::iter::once(cmd));
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore = "prints timings"]
fn scroll_cost() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        return;
    };
    let bytes: Arc<[u8]> = Arc::from(BODY);
    let resolved = byard_core::text::TextMeasurer::new()
        .register_family(&bytes)
        .unwrap();
    let mut table = FontTable::default();
    table.push(FontFace {
        declared: "Body".into(),
        resolved: resolved.into(),
        bytes,
    });
    let table = Arc::new(table);
    let body = table.faces()[0].resolved.clone();
    for (name, family) in [
        ("variable (Manrope)", Some(&body)),
        ("system default", None),
    ] {
        let mut cold = Vec::new();
        for _ in 0..5 {
            let mut enc = encoder(&device, &queue);
            cold.push(time(&mut enc, &device, &queue, &page(&table, family, 0.0)));
        }
        let mut enc = encoder(&device, &queue);
        let _ = time(&mut enc, &device, &queue, &page(&table, family, 0.0));
        let still: f64 = (0..50)
            .map(|_| time(&mut enc, &device, &queue, &page(&table, family, 0.0)))
            .sum::<f64>()
            / 50.0;
        for k in 1..=100 {
            let _ = time(
                &mut enc,
                &device,
                &queue,
                &page(&table, family, k as f32 * 0.37),
            );
        }
        let scroll: f64 = (101..=200)
            .map(|k| {
                time(
                    &mut enc,
                    &device,
                    &queue,
                    &page(&table, family, k as f32 * 0.37),
                )
            })
            .sum::<f64>()
            / 100.0;
        cold.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "{name}: cold frame {:.2} ms (median of 5), warm still {still:.2} ms, warm scrolling {scroll:.2} ms",
            cold[2]
        );
    }
}
