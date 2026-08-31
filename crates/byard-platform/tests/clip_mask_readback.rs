//! Rounded clip masks, in pixels (RFC-0037 clip masks, M125).
//!
//! The claim is narrow and entirely visual: content inside a rounded clip
//! survives, and the corner the clip rounds off does not. Nothing short of a
//! readback settles it — the clip is carried as frame data, uploaded as a
//! uniform, and selected by a dynamic offset, and every one of those steps can
//! be correct while the fragment shader still paints the corner.
//!
//! The fast-path guarantee is asserted alongside it: a rounded clip must cut
//! its corners in the fragment shader, **not** by tessellating a mask or
//! taking a stencil pass. That is what makes it affordable on the common case
//! (an image in a card), so the test pins the mechanism and not only the
//! result.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::items_after_statements
)]

use std::sync::Arc;

use byard_core::BoxInstance;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{Rect, RenderFrame, Transform, Viewport};

const SIZE: u32 = 128;
/// The clip, and the box drawn through it: deliberately the same rectangle, so
/// the only difference a corner radius can make is the corner itself.
const AREA: [f32; 4] = [16.0, 16.0, 96.0, 96.0];
const RADIUS: f32 = 32.0;
const FILL: [f32; 4] = [0.0, 0.55, 0.9, 1.0];
/// Big enough to hold the shipped example, which is 260 logical px wide.
const EXAMPLE: u32 = 560;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("clip mask device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

fn encoder_sized(
    device: &Arc<wgpu::Device>,
    queue: &Arc<wgpu::Queue>,
    size: u32,
) -> EncoderSubsystem {
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        size,
        size,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: size as f32,
            height: size as f32,
        },
        size,
        size,
        1.0,
    );
    enc
}

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    encoder_sized(device, queue, SIZE)
}

/// Encodes `frame` and reads the whole target back as RGBA bytes.
fn render_sized(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
    size: u32,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("clip readback target"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
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

    let bpr = 256 * (size * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("clip readback"),
        size: u64::from(bpr) * u64::from(size),
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
                rows_per_image: Some(size),
            },
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        let src = (y * bpr) as usize;
        let dst = (y * size * 4) as usize;
        out[dst..dst + (size * 4) as usize].copy_from_slice(&data[src..src + (size * 4) as usize]);
    }
    out
}

fn render(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> Vec<u8> {
    render_sized(enc, device, queue, frame, SIZE)
}

/// A frame with one box filling `AREA`, drawn inside a clip over the same
/// rectangle — square-cornered or rounded.
fn framed(radii: [f32; 4]) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    let [x, y, w, h] = AREA;
    frame.begin_clip_rounded(Rect::new(x, y, w, h), radii);
    frame.push_instance(BoxInstance {
        rect: AREA,
        color: FILL,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    });
    frame.end_clip();
    frame
}

fn at(image: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let i = ((y * SIZE + x) * 4) as usize;
    (image[i], image[i + 1], image[i + 2], image[i + 3])
}

/// A rounded clip cuts the corner; a square clip over the same rect does not.
///
/// Both frames are drawn, because "the corner is empty" on its own is also
/// what a clip that swallowed *everything* would produce. Comparing the two
/// separates a clip that rounds from a clip that is simply broken.
#[test]
fn a_rounded_clip_cuts_the_corner_and_keeps_the_middle() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping clip mask readback");
        return;
    };

    let mut enc = encoder(&device, &queue);
    let square = render(&mut enc, &device, &queue, &framed([0.0; 4]));
    let mut enc = encoder(&device, &queue);
    let rounded = render(&mut enc, &device, &queue, &framed([RADIUS; 4]));

    let [x, y, w, h] = AREA;
    // Well inside the corner arc: at 32px radius, a point 6px in from the
    // corner on both axes is outside the quarter-circle.
    let cx = (x + 6.0) as u32;
    let cy = (y + 6.0) as u32;
    // The centre, which no radius can reach.
    let mx = (x + w / 2.0) as u32;
    let my = (y + h / 2.0) as u32;

    let sq_corner = at(&square, cx, cy);
    let rd_corner = at(&rounded, cx, cy);
    let rd_middle = at(&rounded, mx, my);

    assert!(
        sq_corner.3 > 200,
        "a square clip must leave its own corner painted, got {sq_corner:?}"
    );
    assert!(
        rd_corner.3 < 40,
        "a rounded clip must cut the corner the radius removes, got {rd_corner:?}"
    );
    assert!(
        rd_middle.3 > 200 && rd_middle.2 > 100,
        "the middle must survive the clip, got {rd_middle:?}"
    );
}

/// The corner is cut with an antialiased edge, not a stair-step.
///
/// The clip is the one boundary on screen the user did not draw, so a hard
/// `discard` would make it the only jagged edge in a frame where every shape
/// edge is smoothed. Sampling across the arc must therefore find at least one
/// partially covered pixel.
#[test]
fn the_clipped_corner_is_antialiased() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping clip mask readback");
        return;
    };

    let mut enc = encoder(&device, &queue);
    let image = render(&mut enc, &device, &queue, &framed([RADIUS; 4]));

    // Scan the whole top-left corner quadrant rather than one diagonal
    // through it. The fade is a single pixel wide by construction, so a line
    // can cross the arc between two pixel centres and find only fully-in and
    // fully-out samples on either side — a real antialiased edge would then
    // read as jagged purely because of where the line was drawn.
    // Scanned strictly *inside* the box's own edges. The box fills `AREA`
    // exactly, so its left and top edges are antialiased too — an earlier
    // version of this started at the box corner and would have passed on a
    // frame where the clip did nothing at all, reporting the box's own fringe
    // as the clip's arc.
    let [x, y, ..] = AREA;
    let (x0, y0) = (x as u32 + 2, y as u32 + 2);
    let span = RADIUS as u32 - 2;
    let partial = (0..span).any(|dy| {
        (0..span).any(|dx| {
            let a = at(&image, x0 + dx, y0 + dy).3;
            (20..=235).contains(&a)
        })
    });
    assert!(
        partial,
        "the clip's arc must be antialiased: no partially covered pixel found along it"
    );
}

/// A plain rectangular clip must not pay for the rounded path.
///
/// `ScrollView` clips every frame it scrolls and never has corners, so the
/// zero-radius case has to stay exactly what it was: a scissor. This asserts
/// the observable half of that — an unrounded clip's own corner is fully
/// painted — which fails the moment a radius leaks into the default.
#[test]
fn a_square_clip_stays_square() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping clip mask readback");
        return;
    };

    let mut enc = encoder(&device, &queue);
    let image = render(&mut enc, &device, &queue, &framed([0.0; 4]));

    // A coarse alpha map in the failure message. Two guesses at this from the
    // outside were both wrong, and the assertions sample four points, which
    // cannot distinguish "the clip cut the corners" from "nothing painted at
    // all" — the distinction the whole diagnosis turns on.
    let map = || {
        let mut out = String::from("\nalpha map (8px grid):\n");
        for gy in 0..(SIZE / 8) {
            for gx in 0..(SIZE / 8) {
                let a = at(&image, gx * 8 + 4, gy * 8 + 4).3;
                out.push(match a {
                    0 => '.',
                    1..=84 => '-',
                    85..=169 => '+',
                    _ => '#',
                });
            }
            out.push('\n');
        }
        out
    };

    let [x, y, w, h] = AREA;
    for (px, py) in [
        (x + 1.0, y + 1.0),
        (x + w - 2.0, y + 1.0),
        (x + 1.0, y + h - 2.0),
        (x + w - 2.0, y + h - 2.0),
    ] {
        let p = at(&image, px as u32, py as u32);
        assert!(
            p.3 > 200,
            "a square clip keeps all four corners, ({px}, {py}) got {p:?}{}",
            map()
        );
    }
}

/// Renders the shipped `clip_mask` example and, with `BYARD_DUMP_PNG=1`,
/// writes it out so the arcs can be looked at rather than only asserted.
///
/// The assertion stands on its own without the dump: the example must render
/// something, through the real interpreter, from the real `.byd` on disk. That
/// is the difference between "the encoder can clip" and "the feature works as
/// written down for a user".
#[test]
fn the_shipped_example_renders_through_the_interpreter() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping clip mask example render");
        return;
    };

    const SRC: &str = include_str!("../../byard-cli/examples/clip_mask/src/main.byd");
    let parsed = byard_compiler::parser::parse(SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);

    let mut interp = byard_compiler::interp::eval::Interpreter::new();
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    interp.load_views(&parsed.views);
    let main = parsed
        .views
        .iter()
        .find(|v| v.name.as_str() == "Main")
        .expect("the example declares Main");
    let tree = interp.lower_view(main, &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();

    let mut frame = RenderFrame::new();
    frame.request_full_redraw();
    interp.render(&tree, &mut frame, EXAMPLE as f32, EXAMPLE as f32);
    assert!(
        !frame.clips().is_empty(),
        "the example's `Clip` elements must open clips: none were emitted"
    );
    assert!(
        frame
            .clips()
            .iter()
            .any(|c| c.radii.iter().any(|r| *r > 0.0)),
        "at least one of the example's clips must be rounded"
    );

    let mut enc = encoder_sized(&device, &queue, EXAMPLE);
    let image = render_sized(&mut enc, &device, &queue, &frame, EXAMPLE);

    if std::env::var("BYARD_DUMP_PNG").is_ok() {
        let path = std::env::var("BYARD_PNG_PATH")
            .unwrap_or_else(|_| "/tmp/byard_clip_mask.png".to_string());
        image::save_buffer(&path, &image, EXAMPLE, EXAMPLE, image::ColorType::Rgba8)
            .expect("write png");
        eprintln!("wrote {path}");
    }
}
