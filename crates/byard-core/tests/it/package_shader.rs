//! RFC-0039: a package's own shader draws, and samples the layers a view
//! wrote.
//!
//! The package describes a pipeline and the engine builds it, so the claims
//! are the engine's to keep: the WGSL is compiled against the prelude it was
//! promised, a batch of instances lands where its rects say, each instance
//! samples the layer it names, and a layer written on a frame nobody rendered
//! is still written.
//!
//! The readbacks need a GPU and skip without one, as the other readback tests
//! do. The shader's validity does not, and is checked with `naga` everywhere.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::encoder::package_shader::assembled_wgsl;
use byard_core::frame::{RenderFrame, Viewport};
use byard_core::render::{
    Lane, Layers, Layout, NativeProps, NativeView, RenderCtx, Shader, register_shader,
};

const SIZE: u32 = 64;

/// One textured quad: where it goes, which layer it shows, how opaque it is.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Swatch {
    rect: [f32; 4],
    layer: u32,
    fade: f32,
}

/// A shader that draws each instance's layer over its rect.
struct Swatches;

impl Shader for Swatches {
    const NAME: &'static str = "test_swatches";
    const LAYERS: Option<Layers> = Some(Layers {
        width: 2,
        height: 2,
        count: 2,
    });
    type Instance = Swatch;

    fn lanes() -> &'static [Lane] {
        &[Lane::Vec4, Lane::U32, Lane::F32]
    }

    fn wgsl() -> &'static str {
        "
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
    @location(2) fade: f32,
};

@vertex
fn vs_main(
    @location(0) quad: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(2) layer: u32,
    @location(3) fade: f32,
    @location(15) depth: f32,
) -> Out {
    var out: Out;
    out.position = to_clip(rect.xy + quad * rect.zw, depth);
    out.uv = quad;
    out.layer = layer;
    out.fade = fade;
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let colour = textureSample(layers, layer_sampler, in.uv, in.layer);
    return vec4<f32>(colour.rgb, colour.a * in.fade);
}
"
    }
}

/// A view that writes its two layers once and draws one swatch of each.
struct TwoSwatches {
    written: bool,
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

fn layer_of(colour: [u8; 4]) -> Vec<u8> {
    colour.repeat(4)
}

impl NativeProps for TwoSwatches {}

impl NativeView for TwoSwatches {
    fn render(&mut self, _layout: Layout, cx: &mut RenderCtx<'_>) {
        if !self.written {
            self.written = true;
            cx.write_layer::<Swatches>(0, &layer_of(RED));
            cx.write_layer::<Swatches>(1, &layer_of(GREEN));
        }
        let handle = cx.shader::<Swatches>();
        cx.emit(
            handle,
            &[
                Swatch {
                    rect: [0.0, 0.0, 32.0, 64.0],
                    layer: 0,
                    fade: 1.0,
                },
                Swatch {
                    rect: [32.0, 0.0, 32.0, 64.0],
                    layer: 1,
                    fade: 1.0,
                },
            ],
        );
    }
}

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

fn encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    register_shader::<Swatches>();
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

/// Renders `frame` and returns the image, tightly packed RGBA.
fn render_image(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("readback target"),
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

    let bpr = 256u32 * SIZE.div_ceil(64);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback buffer"),
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
    let mut image = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        let row = (y * bpr) as usize;
        image.extend_from_slice(&data[row..row + (SIZE * 4) as usize]);
    }
    image
}

fn pixel(image: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * SIZE + x) * 4) as usize;
    [image[i], image[i + 1], image[i + 2], image[i + 3]]
}

/// The package's WGSL, with the prelude the engine gives it, is a valid
/// module. No GPU needed, so this runs on every machine.
#[test]
fn a_package_shader_validates_against_the_prelude_it_is_given() {
    let source = assembled_wgsl::<Swatches>();
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{e:?}"));
}

#[test]
fn each_instance_draws_the_layer_it_names() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping package-shader readback test");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let mut frame = RenderFrame::new();
    frame.render_native(
        &mut TwoSwatches { written: false },
        Layout::new([0.0, 0.0, 64.0, 64.0]),
    );
    frame.request_full_redraw();
    let image = render_image(&mut enc, &device, &queue, &frame);

    let left = pixel(&image, 16, 32);
    let right = pixel(&image, 48, 32);
    assert!(
        left[0] > 240 && left[1] < 15 && left[2] < 15,
        "layer 0 is red: {left:?}"
    );
    assert!(
        right[1] > 240 && right[0] < 15 && right[2] < 15,
        "layer 1 is green: {right:?}"
    );
}

/// The relay keeps only the latest frame. A layer is written once, so the
/// frame that replaces an unrendered one inherits its writes.
#[test]
fn a_layer_written_on_a_frame_nobody_rendered_is_still_written() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping package-shader readback test");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let mut view = TwoSwatches { written: false };
    let layout = Layout::new([0.0, 0.0, 64.0, 64.0]);

    // The frame that carries the writes, published and never rendered.
    let mut skipped = RenderFrame::new();
    skipped.render_native(&mut view, layout);
    // The next frame: the view has already written, so it only draws.
    let mut next = RenderFrame::new();
    next.render_native(&mut view, layout);
    assert!(
        next.native_textures().is_empty(),
        "it writes its layers once"
    );
    next.merge_dirty_from(&skipped);
    next.request_full_redraw();

    let image = render_image(&mut enc, &device, &queue, &next);
    let left = pixel(&image, 16, 32);
    let right = pixel(&image, 48, 32);
    assert!(left[0] > 240 && left[1] < 15, "red arrived: {left:?}");
    assert!(right[1] > 240 && right[0] < 15, "green arrived: {right:?}");
}

/// A shader whose lanes are not its instance record is refused by name when
/// the pipeline is built, not drawn as garbage.
#[test]
fn lanes_that_are_not_the_record_are_refused_by_name() {
    struct Wrong;
    impl Shader for Wrong {
        const NAME: &'static str = "test_wrong_lanes";
        type Instance = Swatch;
        fn lanes() -> &'static [Lane] {
            &[Lane::Vec4]
        }
        fn wgsl() -> &'static str {
            ""
        }
    }
    let Some((device, _queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping package-shader build test");
        return;
    };
    let viewport_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[],
    });
    let built = pollster::block_on(byard_core::render::ShaderPipeline::<Wrong>::build(
        &byard_core::encoder::pipeline::PipelineCtx {
            device: &device,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            viewport_layout: &viewport_layout,
        },
    ));
    let Err(error) = built else {
        panic!("a 16-byte lane list for a 24-byte record must not build");
    };
    let message = error.to_string();
    assert!(message.contains("test_wrong_lanes"), "{message}");
    assert!(
        message.contains("16") && message.contains("24"),
        "{message}"
    );
}
