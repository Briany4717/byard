//! Golden-image parity for the retained layout path (RFC-0032, INV-22).
//!
//! The retained path is not intended to change a single pixel: it reuses the
//! Taffy tree instead of rebuilding it, and recomputes only what a changed
//! input can have moved. So the honest acceptance criterion for it is not "the
//! numbers went down" — it is **byte-identical output**.
//!
//! `byard-compiler`'s `tests/incremental_paths.rs` already compares the emitted
//! primitives, which is exact and cheap. This file closes the remaining gap:
//! primitives are what the interpreter *says*, pixels are what the user
//! *sees*, and the two are separated by the encoder's scissor — which RFC-0032
//! also changed, from "the whole frame, every frame" to a real dirty union. A
//! scissor that is too small produces a frame that is missing an update while
//! every primitive in it is correct, and only a pixel comparison can catch
//! that.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use std::sync::Arc;

const W: u32 = 320;
const H: u32 = 240;

/// A scene with everything the retained path has to get right at once: a
/// value-driven colour (paint-only), a value-driven height (layout-affecting,
/// and it reflows a sibling), and a wrapping paragraph (the measure protocol,
/// which is what un-wraps if the sizer is lost).
const SRC: &str = r#"
View Probe() {
    var hot = false
    Column #[width: 320, height: 240, p: 8, gap: 6, bg: 0x101014] {
        Box #[width: 60, height: hot ? 50 : 24, bg: hot ? 0xFF3366 : 0x3366FF] {}
        Text("A paragraph that wraps across more than one line at this width, so the measure protocol runs inside layout.")
            #[size: 12, color: 0xE0E0E8]
        Box #[width: 60, height: 20, bg: 0x22CC88] {}
    }
}
"#;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("retained parity device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

fn make_encoder(device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> EncoderSubsystem {
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        W,
        H,
    ))
    .expect("encoder init");
    enc.update_viewport(Viewport::new(W as f32, H as f32), W, H, 1.0);
    enc
}

/// Encodes `frame` and reads the whole target back as RGBA8.
fn render_and_read(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("parity target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
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

    let bpr = (4 * W).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("parity readback"),
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
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    rx.recv().unwrap().unwrap();

    // Rows are padded to `bpr`; keep only the meaningful bytes so two reads of
    // the same image compare equal regardless of padding contents.
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((4 * W * H) as usize);
    for row in 0..H {
        let start = (row * bpr) as usize;
        pixels.extend_from_slice(&mapped[start..start + (4 * W) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    pixels
}

fn build() -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(SRC);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    let tree = interp.lower_view(&parsed.views[0], &[]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    (interp, tree)
}

fn flip(interp: &mut Interpreter) {
    let sig = interp.var_signal(&Symbol::intern("hot")).expect("`hot`");
    let current = interp.peek(sig).as_bool().unwrap_or(false);
    interp.write_var(sig, Value::Bool(!current));
}

fn tick_frame(interp: &mut Interpreter, tree: &[RenderNode]) -> RenderFrame {
    interp.tick();
    let mut f = RenderFrame::new();
    interp.render(tree, &mut f, W as f32, H as f32);
    f
}

/// How many pixels differ, and the first offending coordinate.
fn diff(a: &[u8], b: &[u8]) -> (usize, Option<(u32, u32)>) {
    let mut count = 0;
    let mut first = None;
    for i in (0..a.len()).step_by(4) {
        if a[i..i + 4] != b[i..i + 4] {
            count += 1;
            if first.is_none() {
                let px = (i / 4) as u32;
                first = Some((px % W, px / W));
            }
        }
    }
    (count, first)
}

#[test]
fn a_retained_frame_paints_the_same_pixels_as_a_rebuilt_one() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping retained-layout parity");
        return;
    };

    // Two encoders, so each one's `persistent_color` carries its own history:
    // the scissor only ever repaints part of that texture, so comparing the
    // results is comparing accumulated state, which is exactly what a
    // too-small scissor corrupts.
    let mut retained_enc = make_encoder(&device, &queue);
    let mut rebuilt_enc = make_encoder(&device, &queue);

    let (mut retained_interp, retained_tree) = build();
    let (mut rebuilt_interp, rebuilt_tree) = build();
    // The rebuilt side is forced onto the full path on every frame, so it is
    // the pre-RFC-0032 behaviour reproduced exactly.
    rebuilt_interp.invalidate_retained_layout();

    // Warm-up frame: both sides build from scratch and paint everything.
    let f = tick_frame(&mut retained_interp, &retained_tree);
    let warm_retained = render_and_read(&mut retained_enc, &device, &queue, &f);
    let f = tick_frame(&mut rebuilt_interp, &rebuilt_tree);
    let warm_rebuilt = render_and_read(&mut rebuilt_enc, &device, &queue, &f);
    let (n, first) = diff(&warm_retained, &warm_rebuilt);
    assert_eq!(n, 0, "the first frames already differ at {first:?}");

    // Then four alternating frames. Each flip changes a colour *and* a height,
    // so every frame exercises the paint-dirty scissor and a sibling reflow at
    // the same time.
    for i in 0..4 {
        flip(&mut retained_interp);
        let fast_frame = tick_frame(&mut retained_interp, &retained_tree);
        let retained = render_and_read(&mut retained_enc, &device, &queue, &fast_frame);

        flip(&mut rebuilt_interp);
        rebuilt_interp.invalidate_retained_layout();
        let full_frame = tick_frame(&mut rebuilt_interp, &rebuilt_tree);
        let rebuilt = render_and_read(&mut rebuilt_enc, &device, &queue, &full_frame);

        let (n, first) = diff(&retained, &rebuilt);
        assert_eq!(
            n, 0,
            "frame {i}: {n} pixels differ between the retained and rebuilt \
             paths, first at {first:?}. The retained path is a pure refactor \
             of *when* layout runs — any pixel difference is a bug, not a \
             tolerance to widen."
        );
    }
}
