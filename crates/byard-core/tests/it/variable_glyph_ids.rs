//! Text whose size keeps changing never runs a variable font out of glyph ids.
//!
//! Glyphs of a variable font are custom glyphs, and a custom glyph is named by
//! a `u16`. An entry is one face, weight, glyph, size and flags, so an app that
//! animates a text's size makes new ones every frame, and after 65 535 the
//! text used to vanish. This draws more than 70 000 distinct sizes (offscreen,
//! 2 000 lines a frame) through one long-lived encoder next to a probe line
//! that never changes, and checks every frame's picture is the probe's alone,
//! as a fresh encoder draws it: the text is still there, and no id reused
//! after the table was flushed drew another glyph's cached image.
//!
//! Skips when there is no GPU adapter, like the other GPU tests here.

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{FontFace, FontTable, RenderFrame, TextLine, Viewport};

const BODY: &[u8] = include_bytes!("../../../byard-cli/examples/assets/fonts/Manrope-Variable.ttf");

const SIZE: u32 = 128;

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        SIZE,
        SIZE,
    ))
    .expect("encoder init");
    #[allow(clippy::cast_precision_loss)]
    enc.update_viewport(Viewport::new(SIZE as f32, SIZE as f32), SIZE, SIZE, 1.0);
    enc
}

/// Encodes `frame` into a fresh target and returns every byte of it.
fn render(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph cache reuse target"),
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
    let cmd = enc.encode_frame_from_relay(&target, frame).expect("encode");
    queue.submit(std::iter::once(cmd));

    let row = SIZE * 4; // 512, already a multiple of 256
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glyph cache reuse readback"),
        size: u64::from(row) * u64::from(SIZE),
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
                bytes_per_row: Some(row),
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
    slice.get_mapped_range().to_vec()
}

fn face() -> FontFace {
    let bytes: Arc<[u8]> = Arc::from(BODY);
    let resolved = byard_core::text::TextMeasurer::new()
        .register_family(&bytes)
        .expect("the face parses");
    FontFace {
        declared: "Body".to_string(),
        resolved: resolved.into(),
        bytes,
    }
}

fn line(x: f32, y: f32, size: f32, family: &Arc<str>) -> TextLine {
    TextLine {
        align: byard_core::frame::TextAlign::Start,
        max_lines: 0,
        ellipsis: false,
        x,
        y,
        text: "Rag".to_string(),
        font_size: size,
        weight: 600,
        family: Some(Arc::clone(family)),
        variations: None,
        color: [1.0, 1.0, 1.0, 1.0],
        dirty: true,
    }
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn a_size_animated_through_seventy_thousand_values_still_draws() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let body = face();
    let mut table = FontTable::default();
    table.push(body.clone());
    let table = Arc::new(table);
    let frame = |sweep: std::ops::Range<u32>, probe: f32| {
        let mut f = RenderFrame::new();
        f.set_fonts(Arc::clone(&table));
        for n in sweep {
            // Three glyphs a line, each a new entry at every size.
            f.push_text(line(0.0, -500.0, 8.0 + n as f32 * 0.0005, &body.resolved));
        }
        f.push_text(line(4.0, 4.0, probe, &body.resolved));
        f
    };

    let mut fresh = encoder(&device, &queue);
    let want = render(&mut fresh, &device, &queue, &frame(0..0, 40.0));
    assert!(want.iter().any(|&b| b != 0), "the probe drew something");

    let mut kept = encoder(&device, &queue);
    for start in (0..72_000).step_by(2_000) {
        let got = render(
            &mut kept,
            &device,
            &queue,
            &frame(start..start + 2_000, 40.0),
        );
        assert!(
            got == want,
            "the probe line drew differently after {} sizes",
            start + 2_000
        );
    }

    // A size nobody has drawn: new entries, after the table has been through
    // everything above.
    let want = render(&mut fresh, &device, &queue, &frame(0..0, 37.3));
    let got = render(&mut kept, &device, &queue, &frame(0..0, 37.3));
    assert!(
        got == want,
        "a new size drew differently after 72 000 sizes"
    );
}
