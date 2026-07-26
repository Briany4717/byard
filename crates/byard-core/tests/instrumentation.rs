//! The engine-side half of the instrumentation floor (RFC-0030 §I1).
//!
//! `byard-compiler`'s `tests/instrumentation.rs` covers the logic thread's
//! four scopes; this covers the two that live in `byard-core` — `layout.taffy`
//! (entered by both the full and the incremental layout path) and
//! `encode.frame` — plus `relay.publish`, which is the scope that carries every
//! other one across the frame boundary.
//!
//! Same rationale as the compiler-side file: a benchmark proves a path is
//! fast, not that anyone walks it. These assertions fail if production stops
//! entering a scope, which is the only way a starved profiler gets noticed
//! before it has quietly mis-reported for a phase or two.

#![cfg(feature = "telemetry")]
#![allow(clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_core::atlas::{ContainerStyle, LayoutAtlas, LeafSize};
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{BoxInstance, RenderFrame, Transform, Viewport};
use byard_core::relay::Relay;
use byard_core::telemetry::{SampleBlock, ScopeKind, drain_samples, scope_kind, scope_name};

fn names(block: &SampleBlock) -> Vec<&'static str> {
    block
        .samples
        .iter()
        .map(|s| scope_name(s.scope).unwrap_or("<unknown>"))
        .collect()
}

fn assert_entered(block: &SampleBlock, name: &str) {
    assert!(
        names(block).contains(&name),
        "scope {name:?} was never entered — production has stopped taking the \
         instrumented path. Scopes seen: {:?}",
        names(block)
    );
}

/// Builds a small tree: a root container over `leaves` fixed-size leaves.
fn build_tree(atlas: &mut LayoutAtlas, leaves: usize) {
    let children: Vec<_> = (0..leaves)
        .map(|_| atlas.add_leaf(LeafSize::new(40.0, 20.0)).unwrap())
        .collect();
    let root = atlas
        .add_container(ContainerStyle::new(Some(400.0), Some(300.0)), &children)
        .unwrap();
    atlas.set_root(root).unwrap();
}

#[test]
fn the_full_layout_path_enters_layout_taffy() {
    let _ = drain_samples();
    let mut atlas = LayoutAtlas::new();
    build_tree(&mut atlas, 8);
    atlas.compute(Viewport::new(400.0, 300.0)).unwrap();

    let block = drain_samples();
    assert_entered(&block, "layout.taffy");
    assert_eq!(
        scope_kind(block.samples[0].scope),
        Some(ScopeKind::Native),
        "layout is native work: an AOT build pays for it in full"
    );
}

#[test]
fn the_incremental_layout_path_enters_the_same_scope_as_the_full_one() {
    // Both paths carry the same label deliberately: the point of measuring
    // them is to compare them, and two differently-named scopes could not be
    // read against each other in the overlay without arithmetic the reader has
    // to do in their head.
    let mut atlas = LayoutAtlas::new();
    build_tree(&mut atlas, 8);
    atlas.compute(Viewport::new(400.0, 300.0)).unwrap();

    let _ = drain_samples();
    atlas.recompute_dirty(Viewport::new(400.0, 300.0)).unwrap();
    let block = drain_samples();
    assert_entered(&block, "layout.taffy");
}

#[test]
fn publish_enters_relay_publish() {
    let _ = drain_samples();
    let relay = Relay::new().expect("relay");

    // The scope's sample is written when its guard drops — after the drain
    // inside `publish` — so it rides along with the *next* publish. Two
    // publishes, then read what the second one drained.
    relay.publish(RenderFrame::new());
    relay.publish(RenderFrame::new());

    let published = relay.current().expect("a frame was published");
    assert_entered(published.telemetry(), "relay.publish");
    assert_eq!(
        published
            .telemetry()
            .samples
            .iter()
            .find(|s| scope_name(s.scope) == Some("relay.publish"))
            .map(byard_core::telemetry::Sample::depth),
        Some(0),
        "the hand-off is a top-level scope, not nested in anything"
    );
}

// ── `encode.frame` (needs a real adapter; skips cleanly without one) ────────

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ByardCore - Instrumentation Test Device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

#[test]
fn encoding_a_frame_enters_encode_frame() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter available — skipping");
        return;
    };
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(&device),
        Arc::clone(&queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        64,
        64,
    ))
    .expect("encoder init");
    enc.update_viewport(Viewport::new(64.0, 64.0), 64, 64, 1.0);

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("instrumentation target"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
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

    let mut frame = RenderFrame::new();
    frame.push_instance(BoxInstance {
        rect: [0.0, 0.0, 32.0, 32.0],
        color: [1.0, 0.0, 0.0, 1.0],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
    });

    let _ = drain_samples();
    let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
    queue.submit(std::iter::once(cmd));
    device.poll(wgpu::PollType::wait_indefinitely()).ok();

    let block = drain_samples();
    assert_entered(&block, "encode.frame");
}
