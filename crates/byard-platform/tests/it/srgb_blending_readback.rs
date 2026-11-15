//! RFC-0060: translucent colour blends in sRGB, like the design tools, and
//! opaque colour is not touched by a byte.
//!
//! Three claims, all of them about pixels and so provable only by reading
//! them back:
//!
//! 1. A 10% white layer over Material's `#6750A4` reads `(118, 98, 173)`, the
//!    number Android, a browser and Figma give. Blended in linear light it
//!    read `(130, 116, 176)`, about 19%.
//! 2. An opaque rectangle arrives as exactly the bytes it was written with,
//!    for every byte value, through the solid and the decorated pipeline.
//! 3. An opaque image arrives as exactly its own bytes: it is uploaded as
//!    sRGB, decoded on sampling and encoded again on write.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::sync::Arc;

use byard_core::BoxInstance;
use byard_core::color::{rgba, to_rgba};
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{DecoratedBox, ImageFit, RenderFrame, TextureSampler, Transform, Viewport};

const W: u32 = 256;
const H: u32 = 256;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

/// RGBA rows of a `W` by `H` target at scale 1.
struct Pixels(Vec<u8>);

impl Pixels {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]]
    }
}

fn render(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>, frame: &RenderFrame) -> Pixels {
    let fmt = wgpu::TextureFormat::Rgba8Unorm;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
        1.0,
        W,
        H,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: W as f32,
            height: H as f32,
        },
        W,
        H,
        1.0,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("srgb blending target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
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

    let bpr = W * 4;
    assert_eq!(bpr % 256, 0, "the row is already aligned");
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("srgb blending readback"),
        size: u64::from(bpr) * u64::from(H),
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
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Pixels(data)
}

fn solid(rect: [f32; 4], color: [f32; 4]) -> BoxInstance {
    BoxInstance {
        rect,
        color,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    }
}

/// The state layer of Material's `IconButton`: its content colour at 10% over
/// its container. White at 10% over `#6750A4`.
#[test]
fn a_ten_percent_white_layer_over_material_primary_reads_what_the_design_tools_read() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let mut frame = RenderFrame::new();
    frame.push_instance(solid([0.0, 0.0, 256.0, 16.0], rgba(0x0067_50A4)));
    // 0x1A is 26 of 255, 10.2%: the nearest byte to a tenth.
    frame.push_instance(solid([64.0, 0.0, 128.0, 16.0], to_rgba(0x1AFF_FFFF, true)));
    let px = render(&device, &queue, &frame);

    let under = px.at(16, 8);
    assert_eq!(&under[..3], &[0x67, 0x50, 0xA4], "the container alone");
    let over = px.at(128, 8);
    for (channel, got, want) in [
        ("red", over[0], 118),
        ("green", over[1], 98),
        ("blue", over[2], 173),
    ] {
        assert!(
            got.abs_diff(want) <= 1,
            "{channel}: a 10% white layer over #6750A4 reads (118, 98, 173) like Android, a browser and \
             Figma, got {over:?} (linear light gave (130, 116, 176))"
        );
    }
    assert_eq!(over[3], 255, "the container stays opaque");
}

/// A half-transparent black over white is the half-way grey of a design tool,
/// 127 or 128, and not the 188 linear light gives.
#[test]
fn half_black_over_white_is_the_middle_grey() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let mut frame = RenderFrame::new();
    frame.push_instance(solid([0.0, 0.0, 256.0, 16.0], rgba(0x00FF_FFFF)));
    frame.push_instance(solid([0.0, 0.0, 128.0, 16.0], to_rgba(0x8000_0000, true)));
    let px = render(&device, &queue, &frame);
    let grey = px.at(64, 8)[0];
    assert!(
        grey.abs_diff(127) <= 1,
        "0x80 black over white is {grey}, not 188"
    );
}

/// Every byte value survives the solid pipeline and the decorated one
/// untouched, in each channel. This is what "opaque colours don't change by a
/// single byte" means, tested on all of them rather than on the ones a person
/// thought of.
#[test]
fn an_opaque_rectangle_arrives_as_exactly_its_bytes() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let colour = |v: u32| [v as u8, 255 - v as u8, (v as u8) ^ 0x5A];
    // A 16 by 16 grid of 16 px cells, one per byte value: a cell is wide
    // enough to have a middle the edge's anti-aliasing does not reach.
    let cell = |v: u32| [(v % 16 * 16) as f32, (v / 16 * 16) as f32, 16.0, 16.0];
    let centre = |v: u32| (v % 16 * 16 + 8, v / 16 * 16 + 8);
    let mut frame = RenderFrame::new();
    for v in 0..256_u32 {
        let [r, g, b] = colour(v);
        let linear = rgba(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b));
        if v % 2 == 0 {
            frame.push_instance(solid(cell(v), linear));
        } else {
            frame.push_decorated(DecoratedBox {
                base: solid(cell(v), linear),
                opacity: 1.0,
                dirty: true,
                ..Default::default()
            });
        }
    }
    let px = render(&device, &queue, &frame);
    for v in 0..256_u32 {
        let [r, g, b] = colour(v);
        assert_eq!(
            px.at(centre(v).0, centre(v).1),
            [r, g, b, 255],
            "column {v} ({})",
            if v % 2 == 0 { "solid" } else { "decorated" }
        );
    }
}

/// An opaque image is its own bytes after the sRGB upload, the decode on
/// sampling and the encode on write.
#[test]
fn an_opaque_image_arrives_as_exactly_its_bytes() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    let dir = std::env::temp_dir().join(format!("byard-srgb-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ramp.png");
    let mut img = image::RgbaImage::new(W, 1);
    for (x, _, p) in img.enumerate_pixels_mut() {
        let v = x as u8;
        *p = image::Rgba([v, 255 - v, v ^ 0xA5, 255]);
    }
    img.save(&path).unwrap();

    let mut frame = RenderFrame::new();
    frame.push_texture(TextureSampler {
        rect: [0.0, 0.0, W as f32, H as f32],
        src: path.to_string_lossy().into_owned(),
        fit: ImageFit::Fill,
        radii: [0.0; 4],
        smooth: 0.0,
        opacity: 1.0,
        dirty: true,
    });
    let px = render(&device, &queue, &frame);
    let _ = std::fs::remove_dir_all(&dir);
    for x in 0..W {
        let v = x as u8;
        assert_eq!(px.at(x, 8), [v, 255 - v, v ^ 0xA5, 255], "column {x}");
    }
}
