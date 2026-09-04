//! Font families, in pixels (RFC-0034).
//!
//! The deterministic tests in `byard-compiler` already say *which* family a
//! line resolved to, and they say it on every machine. What they cannot say is
//! that the resolved name reached the GPU: the family is carried on the frame,
//! registered into a second `FontSystem` on the render thread, and handed to
//! the shaper as an attribute, and every one of those steps can be correct
//! while the glyphs still come out of the system font.
//!
//! So the claim here is deliberately the narrow one a readback can carry: two
//! families produce **different** pixels, and one family produces the same
//! pixels twice. Not which is wider, and not how much ink — that would be a
//! measurement of whichever fonts the machine happens to have.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{FontFace, FontTable, RenderFrame, TextLine, Viewport};

const SIZE: u32 = 256;

const DISPLAY: &[u8] =
    include_bytes!("../../byard-cli/examples/assets/fonts/SpaceGrotesk-Variable.ttf");
const BODY: &[u8] = include_bytes!("../../byard-cli/examples/assets/fonts/Manrope-Variable.ttf");

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("font family device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        SIZE,
        SIZE,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: SIZE as f32,
            height: SIZE as f32,
        },
        SIZE,
        SIZE,
        1.0,
    );
    enc
}

fn render(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("font readback target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
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

    let bpr = 256 * (SIZE * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("font readback"),
        size: u64::from(bpr) * u64::from(SIZE),
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
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let mut out = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        let src = (y * bpr) as usize;
        let dst = (y * SIZE * 4) as usize;
        out[dst..dst + (SIZE * 4) as usize].copy_from_slice(&data[src..src + (SIZE * 4) as usize]);
    }
    out
}

/// The table both faces are registered in, and the resolved names to shape by.
fn table() -> (FontTable, String, String) {
    let display: Arc<[u8]> = Arc::from(DISPLAY);
    let body: Arc<[u8]> = Arc::from(BODY);
    let d = byard_core::text::family_name(&display).expect("display face parses");
    let b = byard_core::text::family_name(&body).expect("body face parses");
    let mut t = FontTable::default();
    t.push(FontFace {
        declared: "display".to_string(),
        resolved: Arc::from(d.as_str()),
        bytes: display,
    });
    t.push(FontFace {
        declared: "body".to_string(),
        resolved: Arc::from(b.as_str()),
        bytes: body,
    });
    (t, d, b)
}

/// One line of text at 40px, in `family`, on a frame carrying `fonts`.
fn frame_in(fonts: &FontTable, family: Option<&str>) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    frame.set_fonts(Arc::new(fonts.clone()));
    frame.push_text(TextLine {
        x: 12.0,
        y: 90.0,
        text: "Handgloves".to_string(),
        font_size: 40.0,
        weight: 400,
        family: family.map(Arc::from),
        color: [1.0, 1.0, 1.0, 1.0],
        dirty: true,
    });
    frame
}

/// How many bytes differ between two readbacks.
fn differing(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).filter(|(x, y)| x != y).count()
}

#[test]
fn two_families_paint_different_pixels() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping font family readback");
        return;
    };
    let (fonts, display, body) = table();
    let mut enc = encoder(&device, &queue);

    let a = render(&mut enc, &device, &queue, &frame_in(&fonts, Some(&display)));
    let b = render(&mut enc, &device, &queue, &frame_in(&fonts, Some(&body)));
    let c = render(&mut enc, &device, &queue, &frame_in(&fonts, Some(&display)));

    // Identity first, and first on purpose: a difference between two
    // renderings means nothing until the same rendering twice is known to be
    // stable. Without this, a flaky encoder would read as a working feature.
    assert_eq!(
        differing(&a, &c),
        0,
        "the same family rendered twice must be identical"
    );
    assert!(
        differing(&a, &b) > 0,
        "`{display}` and `{body}` painted byte-identical pixels: no family \
         reached the shaper"
    );

    if std::env::var("BYARD_DUMP_PNG").is_ok() {
        for (name, img) in [("display", &a), ("body", &b)] {
            let path = format!("/tmp/byard_font_{name}.png");
            image::save_buffer(&path, img, SIZE, SIZE, image::ColorType::Rgba8).expect("write png");
            eprintln!("wrote {path}");
        }
    }
}

/// A family the frame never registered falls back, and that fallback is
/// visibly not the registered face.
///
/// This is the control for the test above: it is what makes "they differ"
/// evidence that the *family* is doing the work rather than some incidental
/// difference between two frames.
#[test]
fn an_unregistered_family_does_not_paint_as_the_registered_one() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping font fallback readback");
        return;
    };
    let (fonts, display, _) = table();
    // A fresh pipeline for each, and that is the point rather than an
    // inconvenience: the paint `FontSystem` keeps every face it has ever been
    // given, and its glyph cache keeps every run it has ever shaped. Reusing
    // one encoder would compare a frame against a font system the *previous*
    // frame had already taught, which is the shape of a test that passes with
    // the feature switched off.
    let mut a = encoder(&device, &queue);
    let with = render(&mut a, &device, &queue, &frame_in(&fonts, Some(&display)));
    let mut b = encoder(&device, &queue);
    let without = render(
        &mut b,
        &device,
        &queue,
        &frame_in(&FontTable::default(), Some(&display)),
    );
    assert!(
        differing(&with, &without) > 0,
        "a family the frame never carried painted identically to one it did, \
         so the table is not what is selecting the face"
    );
}
