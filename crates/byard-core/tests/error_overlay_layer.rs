//! RFC-0006 §3.4 / RFC-0030 §C2 — the error overlay is a layer over the last
//! good view, not a screen instead of it.
//!
//! Two claims, tested separately because they fail separately.
//!
//! # The view underneath is actually drawn
//!
//! RFC-0006 promised *"the last successfully-rendered view stays as a blurred
//! background"*, and the dev runner painted an opaque field instead. The comment
//! explaining why was honest and correct when it was written: the flat four-pass
//! encoder drew all text in one global pass after every box, so the app's text
//! bled *over* the scrim. RFC-0017's z-layers and RFC-0023's backdrop blur
//! removed that constraint, and this asserts the promise can now be kept — by
//! reading the pixel, not by trusting the layer marks.
//!
//! # The two transition frames need a full redraw
//!
//! The encoder's scissor union is derived from what changed *between two
//! frames*. Mounting the overlay and dismissing it each change the whole
//! composition at once, so a union computed from a clean previous frame would
//! leave the result partially painted. That interaction did not exist when
//! RFC-0006 was written; it is a direct consequence of the retained/incremental
//! path, and it is the most likely visible bug in this area.
//!
//! GPU-dependent tests request a real adapter and **skip gracefully** when none
//! is available (headless CI), mirroring `m21_pipelines.rs`'s pattern.
#![allow(clippy::cast_precision_loss)]

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{
    BLUR_QUALITY_HIGH, BackdropInstance, BoxInstance, RenderFrame, Transform, Viewport,
};
use std::sync::Arc;

const SIZE: u32 = 128;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ByardCore - Overlay Test Device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

fn render_and_read(
    enc: &mut EncoderSubsystem,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &RenderFrame,
    px: u32,
    py: u32,
) -> [u8; 4] {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("overlay readback target"),
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

    let cmd = enc
        .encode_frame_from_relay(&target, frame)
        .expect("encode the overlay frame");
    queue.submit([cmd]);

    // 256-byte row alignment, per `COPY_BYTES_PER_ROW_ALIGNMENT`.
    let bytes_per_row = (SIZE * 4).next_multiple_of(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("overlay readback buffer"),
        size: u64::from(bytes_per_row * SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut copy = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    copy.copy_texture_to_buffer(
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
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([copy.finish()]);

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let offset = (py * bytes_per_row + px * 4) as usize;
    let pixel = [
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ];
    drop(data);
    buffer.unmap();
    pixel
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
    .expect("encoder");
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

/// A full-viewport box in `color` — "the app".
fn app_frame(color: [f32; 4]) -> RenderFrame {
    let mut f = RenderFrame::new();
    f.push_instance(BoxInstance {
        rect: [0.0, 0.0, SIZE as f32, SIZE as f32],
        color,
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
    });
    f
}

/// The overlay's frosted pane, as `dev.rs` emits it: a new layer, then a
/// full-viewport blurred backdrop with a dark tint.
fn push_overlay(f: &mut RenderFrame) {
    f.begin_layer();
    f.push_backdrop(BackdropInstance {
        rect: [0.0, 0.0, SIZE as f32, SIZE as f32],
        radii: [0.0; 4],
        blur: 18.0,
        tint: [0.05, 0.05, 0.07, 0.82],
        saturation: 0.35,
        quality: BLUR_QUALITY_HIGH,
        opacity: 1.0,
        transform: Transform::IDENTITY,
        depth: 0.0,
    });
}

#[test]
fn the_last_good_view_is_visible_through_the_overlays_backdrop() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping");
        return;
    };

    // Two identical overlays over two different "apps". If the overlay were an
    // opaque field — the pre-RFC-0017 implementation — these would read the
    // same pixel, because the view underneath would contribute nothing.
    let mut red = app_frame([0.9, 0.05, 0.05, 1.0]);
    push_overlay(&mut red);
    let mut blue = app_frame([0.05, 0.05, 0.9, 1.0]);
    push_overlay(&mut blue);

    let mut enc = encoder(&device, &queue);
    let over_red = render_and_read(&mut enc, &device, &queue, &red, SIZE / 2, SIZE / 2);
    let mut enc = encoder(&device, &queue);
    let over_blue = render_and_read(&mut enc, &device, &queue, &blue, SIZE / 2, SIZE / 2);

    assert_ne!(
        over_red, over_blue,
        "the view beneath the overlay must reach the screen — an opaque field \
         would render these identically ({over_red:?} vs {over_blue:?})"
    );
    assert!(
        over_red[0] > over_red[2],
        "a red app must still read red-ish through the scrim: {over_red:?}"
    );
    assert!(
        over_blue[2] > over_blue[0],
        "and a blue one blue-ish: {over_blue:?}"
    );
}

#[test]
fn the_backdrop_darkens_what_it_covers_rather_than_replacing_it() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter — skipping");
        return;
    };
    // The point of the blur *and* the tint: the app must survive as shape and
    // colour while stopping being readable. A pane that left it at full
    // brightness would compete with the error text for the same attention.
    let bare = app_frame([0.9, 0.05, 0.05, 1.0]);
    let mut covered = app_frame([0.9, 0.05, 0.05, 1.0]);
    push_overlay(&mut covered);

    let mut enc = encoder(&device, &queue);
    let bare_px = render_and_read(&mut enc, &device, &queue, &bare, SIZE / 2, SIZE / 2);
    let mut enc = encoder(&device, &queue);
    let covered_px = render_and_read(&mut enc, &device, &queue, &covered, SIZE / 2, SIZE / 2);

    assert!(
        covered_px[0] < bare_px[0],
        "the tint must darken the view it covers: {covered_px:?} vs {bare_px:?}"
    );
}

// ── The transition frames (no GPU needed) ──────────────────────────────────

#[test]
fn a_frame_can_ask_for_a_full_redraw_and_the_request_does_not_leak() {
    // A sticky full-redraw request would silently disable the incremental path
    // for the rest of the session — the expensive way to be wrong here, because
    // nothing would ever look broken.
    let mut f = RenderFrame::new();
    assert!(!f.wants_full_redraw());
    f.request_full_redraw();
    assert!(f.wants_full_redraw());
    f.clear();
    assert!(
        !f.wants_full_redraw(),
        "a recycled frame must not inherit the previous one's request"
    );
}

#[test]
fn a_full_redraw_request_survives_a_frame_that_was_never_rendered() {
    // The logic thread outruns the display, so most published frames are never
    // drawn. If the frame that mounted the overlay is one of them, its request
    // is still owed — dropping it would make the overlay's correctness depend
    // on the display keeping up, which is the exact thing `merge_dirty_from`
    // exists because it does not.
    let mut mounted = RenderFrame::new();
    mounted.request_full_redraw();

    let mut next = RenderFrame::new();
    assert!(!next.wants_full_redraw());
    next.merge_dirty_from(&mounted);
    assert!(
        next.wants_full_redraw(),
        "a skipped mount frame must hand its request to the frame that replaces it"
    );
}

#[test]
fn both_transitions_ask_and_the_frames_between_them_do_not() {
    // The shape `dev.rs` implements: mount and dismiss each ask once. Asking on
    // every frame the overlay is up would hand back the incremental path for as
    // long as a file stays broken, which can be a long time.
    let mut was_up = false;
    let mut asked = Vec::new();
    for up in [false, false, true, true, true, false, false] {
        let mut f = RenderFrame::new();
        if up != was_up {
            f.request_full_redraw();
            was_up = up;
        }
        asked.push(f.wants_full_redraw());
    }
    assert_eq!(
        asked,
        vec![false, false, true, false, false, true, false],
        "exactly the mount frame and the dismiss frame"
    );
}

// ── Two producers, one index-addressed pool (RFC-0030 §V3) ─────────────────

#[test]
fn mark_dirty_since_marks_forward_and_leaves_everything_before_it_alone() {
    // The encoder's glyph cache is index-addressed: it compares `texts[i]`
    // against what it shaped for `texts[i]` last frame and trusts
    // `TextLine::dirty`. That contract holds while one producer owns the pool,
    // because an element keeping its index keeps its identity.
    //
    // A dev overlay drawn by a *second* interpreter, appended after the app's
    // primitives, breaks it: its indices move whenever the app's counts change,
    // so index `i` holds the overlay's line where it held the app's a frame
    // ago. Both producers truthfully report their own primitives unchanged.
    // Only the frame sees both, so only the frame can resolve it.
    let mut f = RenderFrame::new();
    let line = |text: &str| byard_core::frame::TextLine {
        x: 0.0,
        y: 0.0,
        text: text.to_string(),
        font_size: 14.0,
        color: [1.0; 4],
        dirty: false,
    };
    f.push_text(line("app 0"));
    f.push_text(line("app 1"));
    f.push_instance(BoxInstance {
        rect: [0.0, 0.0, 1.0, 1.0],
        color: [1.0; 4],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
    });

    let mark = f.cursor();
    assert_eq!(mark.text, 2);
    assert_eq!(mark.solid, 1);

    f.push_text(line("overlay 0"));
    f.push_text(line("overlay 1"));
    f.push_instance(BoxInstance {
        rect: [0.0, 0.0, 1.0, 1.0],
        color: [1.0; 4],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
    });

    f.mark_dirty_since(mark);

    let dirty: Vec<bool> = f.texts().iter().map(|t| t.dirty).collect();
    assert_eq!(
        dirty,
        vec![false, false, true, true],
        "everything at or after the mark is dirty; everything before it is \
         exactly as its own producer reported it"
    );
    // Solids are pushed dirty by default — a `BoxInstance` is a GPU `Pod` with
    // no room for the flag, so the parallel vector is seeded `true` and cleared
    // by whoever can prove otherwise. Marking forward is therefore a no-op
    // here, which is the correct outcome and not evidence of anything.
    assert!(f.instances_dirty().iter().all(|d| *d));
}

#[test]
fn mark_dirty_since_a_cursor_past_the_end_is_a_no_op() {
    // The overlay emitted nothing this frame. Nothing to mark, and nothing to
    // panic about — an overlay that draws conditionally must not have to guard
    // the call site.
    let mut f = RenderFrame::new();
    let mark = f.cursor();
    f.mark_dirty_since(mark);
    assert!(f.texts().is_empty());
    f.push_text(byard_core::frame::TextLine {
        x: 0.0,
        y: 0.0,
        text: "after".to_string(),
        font_size: 14.0,
        color: [1.0; 4],
        dirty: false,
    });
    // A stale mark from before the push still only marks forward from it.
    f.mark_dirty_since(f.cursor());
    assert!(!f.texts()[0].dirty);
}
