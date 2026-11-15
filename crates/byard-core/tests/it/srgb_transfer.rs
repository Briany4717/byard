//! RFC-0060: the sRGB write the shaders end with is `byard_core::color`'s
//! transfer function, not a second copy of it.
//!
//! The WGSL is generated from the Rust constants, so the two agree by
//! construction. This test is what notices the day someone writes the curve
//! into a shader by hand: it runs `byard_out` and `byard_out_premul` on the
//! GPU and compares them with `linear_to_srgb` at 1024 points. The shader
//! writes each result's bits into an integer target, so nothing rounds and no
//! float target is needed: GL renders to `Rgba32Float` only with an extension
//! Mesa does not always offer.
#![allow(clippy::cast_precision_loss)]

use byard_core::color::{linear_to_srgb, wgsl_transfer};

const STEPS: u32 = 1024;

const MAIN: &str = "
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(vi & 1u) * 4 - 1);
    let y = f32(i32(vi >> 1u) * 4 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<u32> {
    let l = floor(p.x) / 1023.0;
    if (p.y < 1.0) {
        // Row 0: straight alpha, the colour encoded and the alpha left alone.
        return bitcast<vec4<u32>>(byard_out(vec4<f32>(l, l * 0.5, l * 0.25, 0.75)));
    }
    // Row 1: premultiplied at alpha 0.5, so the straight colour is 2 * c.
    return bitcast<vec4<u32>>(byard_out_premul(vec4<f32>(l * 0.5, l * 0.25, l * 0.125, 0.5)));
}
";

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one GPU round trip, read top to bottom"
)]
fn the_wgsl_transfer_is_the_rust_one() {
    let Some((device, queue, _turn)) = byard_test_gpu::device(byard_core::engine::device_limits)
    else {
        eprintln!("no GPU adapter, skipping transfer comparison");
        return;
    };
    let source = format!("{}\n{MAIN}", wgsl_transfer());
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("srgb transfer"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("srgb transfer"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba32Uint,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("srgb transfer target"),
        size: wgpu::Extent3d {
            width: STEPS,
            height: 2,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Uint,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut ce = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = ce.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    let bpr = STEPS * 16;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bpr) * 2,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
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
                rows_per_image: Some(2),
            },
        },
        wgpu::Extent3d {
            width: STEPS,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let floats: Vec<f32> = data
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let at = |row: u32, x: u32, channel: usize| floats[((row * STEPS + x) * 4) as usize + channel];

    // Half a byte is 0.002; the two curves have to agree far inside that.
    let tolerance = 2e-4;
    for x in 0..STEPS {
        let l = x as f32 / 1023.0;
        for (channel, scale) in [(0, 1.0), (1, 0.5), (2, 0.25)] {
            let want = linear_to_srgb(l * scale);
            let got = at(0, x, channel);
            assert!(
                (got - want).abs() < tolerance,
                "byard_out channel {channel} at linear {}: wgsl {got}, rust {want}",
                l * scale
            );
        }
        assert!((at(0, x, 3) - 0.75).abs() < 1e-6, "alpha is not encoded");
        for (channel, scale) in [(0, 1.0), (1, 0.5), (2, 0.25)] {
            // Premultiplied at alpha 0.5: straight is `l * scale`, so the
            // premultiplied encoded colour is half its encoding.
            let want = linear_to_srgb(l * scale) * 0.5;
            let got = at(1, x, channel);
            assert!(
                (got - want).abs() < tolerance,
                "byard_out_premul channel {channel} at linear {}: wgsl {got}, rust {want}",
                l * scale
            );
        }
        assert!((at(1, x, 3) - 0.5).abs() < 1e-6);
    }
}

/// A premultiplied colour of zero alpha stays zero, with no division by it.
#[test]
fn the_generated_wgsl_guards_zero_alpha() {
    let wgsl = wgsl_transfer();
    assert!(wgsl.contains("fn byard_out("));
    assert!(wgsl.contains("fn byard_out_premul("));
    assert!(wgsl.contains("!(c.a > 0.0)"), "zero alpha is kept as zero");
}
