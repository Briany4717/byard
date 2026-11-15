//! A variable font drawn after another one is drawn as it would be alone.
//!
//! `swash` keeps one buffer of variation coordinates and never clears it
//! (dfrg/swash#149), and `cosmic-text` sets only `wght`, so any variable font
//! scaled through glyphon's own cache after another at a non-default weight
//! came out at the other's coordinates on every axis it has besides `wght`.
//! Byard draws lines with a variable face itself (see `variation_glyphs`), and
//! this renders a heavy Manrope line and then an icon-font line, whose `FILL`
//! axis would catch the leak, through one long-lived encoder, and the icon line
//! again through a fresh one, and compares every byte.
//!
//! Skips when there is no GPU adapter, like the other GPU tests here.

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{FontFace, FontTable, RenderFrame, TextLine, Viewport};

const ICONS: &[u8] = include_bytes!("../../../byard-cli/examples/assets/fonts/ByardIcons.ttf");
const BODY: &[u8] = include_bytes!("../../../byard-cli/examples/assets/fonts/Manrope-Variable.ttf");

const SIZE: u32 = 128;

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
        format: wgpu::TextureFormat::Rgba8Unorm,
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

fn face(bytes: &'static [u8], declared: &str) -> FontFace {
    let bytes: Arc<[u8]> = Arc::from(bytes);
    let resolved = byard_core::text::TextMeasurer::new()
        .register_family(&bytes)
        .expect("the face parses");
    FontFace {
        declared: declared.to_string(),
        resolved: resolved.into(),
        bytes,
    }
}

fn line(y: f32, text: &str, family: &Arc<str>, weight: u16) -> TextLine {
    TextLine {
        align: byard_core::frame::TextAlign::Start,
        max_lines: 0,
        ellipsis: false,
        x: 6.0,
        y,
        text: text.to_string(),
        font_size: 40.0,
        weight,
        family: Some(Arc::clone(family)),
        variations: None,
        color: [1.0, 1.0, 1.0, 1.0],
        dirty: true,
    }
}

#[test]
fn a_variable_font_draws_the_same_after_another_at_a_heavy_weight() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let (icons, body) = (face(ICONS, "Icons"), face(BODY, "Body"));
    let mut table = FontTable::default();
    table.push(icons.clone());
    table.push(body.clone());
    let table = Arc::new(table);

    let frame = |lines: Vec<TextLine>| {
        let mut f = RenderFrame::new();
        f.set_fonts(Arc::clone(&table));
        for l in lines {
            f.push_text(l);
        }
        f
    };
    let heavy = frame(vec![line(4.0, "Rag", &body.resolved, 800)]);
    let icon = frame(vec![line(60.0, "favorite", &icons.resolved, 400)]);

    let mut kept = encoder(&device, &queue);
    let _ = render(&mut kept, &device, &queue, &heavy);
    let after = render(&mut kept, &device, &queue, &icon);
    let mut fresh = encoder(&device, &queue);
    let alone = render(&mut fresh, &device, &queue, &icon);
    assert!(alone.iter().any(|&b| b != 0), "the icon drew something");
    assert!(
        after == alone,
        "the icon line drew differently after a heavy variable line"
    );
}
