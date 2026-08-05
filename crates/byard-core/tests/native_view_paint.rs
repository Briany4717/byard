//! RFC-0039: a native view paints, and paints the same pixels an intrinsic
//! would.
//!
//! The claim the extension ABI rests on is not "a package can draw", it is
//! that what a package draws is indistinguishable from what core draws. That
//! is a pixel question, so it is answered here in pixels: the same rectangle
//! is emitted twice, once as the interpreter emits it and once through
//! [`NativeView::render`], and the two frames are compared image against
//! image.
//!
//! GPU-dependent, and skips gracefully with no adapter (headless CI), the same
//! pattern the other readback tests use.
#![allow(clippy::cast_precision_loss)]
// Each test's view type is declared inside the test it belongs to: a view that
// exists to answer one question reads better beside that question than in a
// preamble the reader has to scroll back to.
#![allow(clippy::items_after_statements)]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{BoxInstance, RenderFrame, Transform, Viewport};
use byard_core::render::{Layout, NativeProps, NativeView, RenderCtx};

const SIZE: u32 = 128;
/// The rectangle both paths draw, in logical pixels.
const RECT: [f32; 4] = [24.0, 32.0, 64.0, 48.0];
/// Its fill, in linear space (the frame's colours are linear; the encoder is
/// what turns them into what the screen shows).
const FILL: [f32; 4] = [0.0, 0.6, 0.9, 1.0];

/// The ABI's smallest possible consumer: a view that fills its rect.
///
/// Deliberately not clever. It exists to answer one question, whether the same
/// instance reaching the GPU through the extension path lands in the same
/// pixels, and anything more in it would be a second variable.
struct Quad {
    colour: [f32; 4],
    renders: u32,
}

impl NativeProps for Quad {}

impl NativeView for Quad {
    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
        self.renders += 1;
        let handle = cx.pipeline::<byard_core::encoder::SolidBoxPipeline>();
        cx.emit(
            handle,
            &[BoxInstance {
                rect: layout.rect,
                color: self.colour,
                radii: [0.0; 4],
                transform: Transform::IDENTITY,
                smooth: 0.0,
            }],
        );
    }
}

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ByardCore - Native View Test Device"),
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

/// Renders `frame` and returns the whole image, tightly packed RGBA.
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

/// The frame an intrinsic produces: one solid box, pushed the way the
/// interpreter pushes one.
fn intrinsic_frame() -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.push_instance(BoxInstance {
        rect: RECT,
        color: FILL,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    });
    frame.request_full_redraw();
    frame
}

/// The same frame, drawn by a native view through the extension ABI.
fn native_frame(view: &mut Quad) -> RenderFrame {
    let mut frame = RenderFrame::new();
    frame.render_native(view, Layout::new(RECT));
    frame.request_full_redraw();
    frame
}

#[test]
fn a_native_view_paints_the_pixels_an_intrinsic_would() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping native-view readback test");
        return;
    };

    let mut enc = encoder(&device, &queue);
    let from_core = render_image(&mut enc, &device, &queue, &intrinsic_frame());

    let mut view = Quad {
        colour: FILL,
        renders: 0,
    };
    let mut enc = encoder(&device, &queue);
    let from_view = render_image(&mut enc, &device, &queue, &native_frame(&mut view));

    assert_eq!(view.renders, 1, "the view drew exactly once");
    assert_eq!(
        from_view, from_core,
        "a native view's rectangle must be the intrinsic's rectangle, pixel for pixel"
    );

    // And the image is not two blank frames agreeing with each other.
    let inside = pixel(&from_view, 56, 56);
    assert!(
        inside[2] > 200 && inside[0] < 60,
        "the middle of the rect should be the blue that was emitted, got {inside:?}"
    );
    let outside = pixel(&from_view, 4, 4);
    assert_ne!(inside, outside, "the rect must not cover the whole frame");
}

#[test]
fn a_view_that_emits_nothing_leaves_the_frame_alone() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping native-view readback test");
        return;
    };

    struct Silent;
    impl NativeProps for Silent {}
    impl NativeView for Silent {
        fn render(&mut self, _layout: Layout, _cx: &mut RenderCtx<'_>) {}
    }

    let mut empty = RenderFrame::new();
    empty.request_full_redraw();
    let mut enc = encoder(&device, &queue);
    let blank = render_image(&mut enc, &device, &queue, &empty);

    let mut frame = RenderFrame::new();
    frame.render_native(&mut Silent, Layout::new(RECT));
    frame.request_full_redraw();
    let mut enc = encoder(&device, &queue);
    let silent = render_image(&mut enc, &device, &queue, &frame);

    assert_eq!(
        blank, silent,
        "a view with nothing to draw must cost the frame nothing, including pixels"
    );
}

#[test]
fn a_views_batch_is_ordered_against_core_primitives_by_depth_not_by_kind() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping native-view readback test");
        return;
    };

    // A core box, then a view's box over it: the later emission wins, which is
    // the same rule two core boxes follow. If native batches were drawn as a
    // separate pass rather than in emission order, the answer would depend on
    // which pass ran last instead of on which was emitted last.
    let mut over = RenderFrame::new();
    over.push_instance(BoxInstance {
        rect: RECT,
        color: [0.9, 0.1, 0.1, 1.0],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    });
    over.render_native(
        &mut Quad {
            colour: FILL,
            renders: 0,
        },
        Layout::new(RECT),
    );
    over.request_full_redraw();
    let mut enc = encoder(&device, &queue);
    let image = render_image(&mut enc, &device, &queue, &over);
    let px = pixel(&image, 56, 56);
    assert!(
        px[2] > 200 && px[0] < 60,
        "the view emitted last, so its blue is what is on top, got {px:?}"
    );

    // And the other way round: a core box emitted after the view covers it.
    let mut under = RenderFrame::new();
    under.render_native(
        &mut Quad {
            colour: FILL,
            renders: 0,
        },
        Layout::new(RECT),
    );
    under.push_instance(BoxInstance {
        rect: RECT,
        color: [0.9, 0.1, 0.1, 1.0],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    });
    under.request_full_redraw();
    let mut enc = encoder(&device, &queue);
    let image = render_image(&mut enc, &device, &queue, &under);
    let px = pixel(&image, 56, 56);
    assert!(
        px[0] > 200 && px[1] < 140 && px[2] < 140,
        "the core box emitted last, so its red is what is on top, got {px:?}"
    );
}

#[test]
fn dispatch_stays_per_pipeline_however_many_instances_a_view_emits() {
    // INV-30, as a number rather than as a claim. Ten instances and ten
    // thousand go through the same registry call, because the erased call
    // chooses a pipeline and everything after it is the concrete type. If
    // `emit` or the draw loop ever routed instances through the trait object,
    // this is where it would show up: as a count in the thousands.
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping dispatch-count test");
        return;
    };

    struct Many(usize);
    impl NativeProps for Many {}
    impl NativeView for Many {
        fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
            let handle = cx.pipeline::<byard_core::encoder::SolidBoxPipeline>();
            let instances: Vec<BoxInstance> = (0..self.0)
                .map(|i| BoxInstance {
                    rect: [(i % 100) as f32, (i / 100) as f32, 1.0, 1.0],
                    color: FILL,
                    radii: [0.0; 4],
                    transform: Transform::IDENTITY,
                    smooth: 0.0,
                })
                .collect();
            let _ = layout;
            cx.emit(handle, &instances);
        }
    }

    let dispatches_for = |count: usize| {
        let mut frame = RenderFrame::new();
        frame.render_native(&mut Many(count), Layout::new(RECT));
        frame.request_full_redraw();
        let mut enc = encoder(&device, &queue);
        let _ = render_image(&mut enc, &device, &queue, &frame);
        enc.pipeline_dispatches()
    };

    let few = dispatches_for(10);
    let many = dispatches_for(10_000);
    assert_eq!(
        few, many,
        "a thousand times the instances must not be a thousand times the dispatches"
    );
    assert!(
        many < 32,
        "dispatch is per pipeline per segment, so this is a single-digit-ish \
         number, got {many}"
    );
}

#[test]
fn a_batch_for_a_pipeline_nobody_registered_does_not_take_the_frame_down() {
    // A view drawing through an unregistered pipeline is an app-assembly
    // mistake. It must be survivable and it must be said out loud (INV-4): the
    // frame that follows it still renders, and the rest of the scene is intact.
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping unregistered-pipeline test");
        return;
    };

    struct Ghost;
    /// A pipeline that exists as a type and was never registered.
    struct Unregistered;
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct GhostInstance {
        _rect: [f32; 4],
    }
    impl byard_core::encoder::pipeline::RenderPipeline for Unregistered {
        const NAME: &'static str = "ghost";
        type Instance = GhostInstance;
        fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
            const ATTRS: &[wgpu::VertexAttribute] = &wgpu::vertex_attr_array![1 => Float32x4];
            wgpu::VertexBufferLayout {
                array_stride: 16,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: ATTRS,
            }
        }
        fn draw(
            &self,
            _pass: &mut wgpu::RenderPass<'_>,
            _cx: &byard_core::encoder::pipeline::SegmentDraw<'_>,
        ) {
        }
        fn draw_batch(
            &self,
            _pass: &mut wgpu::RenderPass<'_>,
            _cx: &byard_core::encoder::pipeline::BatchDraw<'_>,
        ) {
        }
    }
    impl NativeProps for Ghost {}
    impl NativeView for Ghost {
        fn render(&mut self, _layout: Layout, cx: &mut RenderCtx<'_>) {
            let handle = cx.pipeline::<Unregistered>();
            cx.emit(handle, &[GhostInstance { _rect: [0.0; 4] }]);
        }
    }

    let mut frame = RenderFrame::new();
    frame.push_instance(BoxInstance {
        rect: RECT,
        color: FILL,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    });
    frame.render_native(&mut Ghost, Layout::new(RECT));
    frame.request_full_redraw();

    let mut enc = encoder(&device, &queue);
    let image = render_image(&mut enc, &device, &queue, &frame);
    let px = pixel(&image, 56, 56);
    assert!(
        px[2] > 200,
        "the rest of the frame renders around the batch that could not be drawn, got {px:?}"
    );
}

#[test]
fn a_view_whose_output_changed_repaints_even_though_nothing_else_did() {
    // INV-26 for a pool whose instances are opaque bytes. A native batch has
    // no dirty bit to read, so the bytes are the dirty bit: two frames whose
    // scene is otherwise identical must still repaint when the widget's own
    // output moved, or a chart animating from its own state freezes on screen
    // while the app looks perfectly clean.
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter, skipping native-view invalidation test");
        return;
    };

    struct Moving(f32);
    impl NativeProps for Moving {}
    impl NativeView for Moving {
        fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
            let handle = cx.pipeline::<byard_core::encoder::SolidBoxPipeline>();
            cx.emit(
                handle,
                &[BoxInstance {
                    rect: [self.0, layout.rect[1], 32.0, 32.0],
                    color: FILL,
                    radii: [0.0; 4],
                    transform: Transform::IDENTITY,
                    smooth: 0.0,
                }],
            );
        }
    }

    let mut enc = encoder(&device, &queue);
    let mut view = Moving(8.0);

    // The first frame asks for a full redraw the way a first frame does.
    let mut first = RenderFrame::new();
    first.render_native(&mut view, Layout::new(RECT));
    first.request_full_redraw();
    let _ = render_image(&mut enc, &device, &queue, &first);

    // The second frame asks for nothing: no dirty instance, no full redraw.
    // The only thing that changed is inside the view.
    view.0 = 72.0;
    let mut second = RenderFrame::new();
    second.render_native(&mut view, Layout::new(RECT));
    let _ = render_image(&mut enc, &device, &queue, &second);
    assert!(
        enc.last_frame_was_full_redraw(),
        "the widget moved, so the frame it moved on has to be painted"
    );

    // And a frame that changed nothing must not force a repaint, or the
    // guarantee above would just be "always redraw" wearing a hat.
    let mut third = RenderFrame::new();
    third.render_native(&mut view, Layout::new(RECT));
    let _ = render_image(&mut enc, &device, &queue, &third);
    assert!(
        !enc.last_frame_was_full_redraw() && !enc.last_frame_scissored(),
        "an unchanged batch is not a reason to redraw anything at all"
    );
}
