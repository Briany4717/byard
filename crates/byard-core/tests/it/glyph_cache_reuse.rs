//! A glyph cache that survives the text pool changing length still draws what a
//! fresh one would.
//!
//! The pipeline keeps a shaped buffer per index of the frame's text pool and
//! keeps it when the line's key (text, size, wrap, weight, family) still
//! matches, even on a frame where the pool grew or shrank. It also keeps the
//! buffers a shrinking pool sheds, and hands them out again when it grows. Both
//! are only worth anything if the pixels come out the same, so this renders a
//! run of frames through one long-lived encoder, and each frame again through
//! an encoder that has never seen anything else, and compares every byte.
//!
//! The scene is built so a stale buffer would show: rows of text, some with an
//! opaque box drawn over their left end, so each row's draw-order depth (the
//! glyph metadata the pipeline maps to one) decides how much of it is visible,
//! and which rows are covered changes when a row is added or removed.
//!
//! Skips when there is no GPU adapter, like the other GPU tests here.

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{BoxInstance, RenderFrame, TextLine, Transform, Viewport};

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

fn box_at(rect: [f32; 4], color: [f32; 4]) -> BoxInstance {
    BoxInstance {
        rect,
        color,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    }
}

fn text(x: f32, y: f32, s: &str, color: [f32; 4]) -> TextLine {
    TextLine {
        align: byard_core::frame::TextAlign::Start,
        max_lines: 0,
        ellipsis: false,
        x,
        y,
        text: s.to_string(),
        font_size: 12.0,
        weight: 400,
        family: None,
        variations: None,
        color,
        dirty: true,
    }
}

/// Rows of text down the left, every third one half-hidden by a box drawn
/// after it, then (optionally) a second layer of lines on the right, the shape
/// of a HUD.
fn scene(rows: &[&str], hud: Option<&[&str]>) -> RenderFrame {
    let mut f = RenderFrame::new();
    f.push_instance(box_at([0.0, 0.0, 128.0, 128.0], [0.1, 0.1, 0.3, 1.0]));
    for (i, row) in rows.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let y = 4.0 + i as f32 * 13.0;
        f.push_text(text(4.0, y, row, [1.0, 0.9, 0.4, 1.0]));
        if i % 3 == 1 {
            f.push_instance(box_at([4.0, y, 22.0, 13.0], [0.8, 0.1, 0.1, 1.0]));
        }
    }
    if let Some(hud) = hud {
        f.begin_layer();
        f.push_instance(box_at([70.0, 0.0, 58.0, 128.0], [0.0, 0.0, 0.0, 0.85]));
        for (i, line) in hud.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let y = 4.0 + i as f32 * 11.0;
            f.push_text(text(72.0, y, line, [0.6, 1.0, 0.6, 1.0]));
        }
    }
    f
}

#[test]
fn a_cache_that_saw_the_pool_change_draws_what_a_fresh_one_does() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let base = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
    ];
    let hud_a = ["fps 60", "work 3.4", "wait 0.2", "text 31", "boxes 12"];
    let hud_b = ["fps 59", "work 12.7", "wait 1.2"];

    let mut appended = base.to_vec();
    appended.push("hotel");
    let mut middle_gone = appended.clone();
    middle_gone.remove(3);
    let mut first_gone = middle_gone.clone();
    first_gone.remove(0);

    let frames = [
        ("base", scene(&base, None)),
        ("a line appended", scene(&appended, None)),
        ("a line removed from the middle", scene(&middle_gone, None)),
        ("the first line removed", scene(&first_gone, None)),
        ("the HUD opens", scene(&first_gone, Some(&hud_a))),
        ("the HUD closes", scene(&first_gone, None)),
        (
            "the HUD opens with other text",
            scene(&first_gone, Some(&hud_b)),
        ),
        (
            "a line appended under the HUD",
            scene(&appended, Some(&hud_a)),
        ),
        ("everything gone but one line", scene(&base[..1], None)),
        ("back to the base", scene(&base, None)),
    ];

    let mut kept = encoder(&device, &queue);
    let mut previous: Option<Vec<u8>> = None;
    for (what, frame) in &frames {
        let got = render(&mut kept, &device, &queue, frame);
        let mut fresh = encoder(&device, &queue);
        let want = render(&mut fresh, &device, &queue, frame);
        assert!(
            got == want,
            "{what}: the pixels of a frame drawn after others differ from the \
             same frame drawn by a pipeline that has seen nothing else"
        );
        // Not vacuous: there are glyph pixels (not just the two flat fills),
        // and the frames are not all the same picture.
        assert!(
            got.chunks_exact(4).any(|p| p[0] > 200 && p[1] > 150),
            "{what}: no text was drawn"
        );
        if let Some(prev) = &previous {
            assert!(
                prev != &got,
                "{what}: drew the same picture as the frame before"
            );
        }
        previous = Some(got);
    }
}
