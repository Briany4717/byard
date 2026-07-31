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
        smooth: 0.0,
    });

    let _ = drain_samples();
    let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
    queue.submit(std::iter::once(cmd));
    device.poll(wgpu::PollType::wait_indefinitely()).ok();

    let block = drain_samples();
    assert_entered(&block, "encode.frame");
}

// ── `encode.frame`'s sub-scopes (RFC-0030 §I1, second pass) ─────────────────
//
// `encode.frame` was a single ~6 ms row: the largest term in the frame and the
// least explained one. These assertions pin the breakdown that replaced it —
// uploads, glyphs, passes, buffers — so a sub-scope that stops being entered
// fails here rather than quietly reading `0.000ms` in the terminal, which is
// indistinguishable from "that work got free" (INV-18).
//
// `present.acquire` / `present.submit` are the two scopes this file cannot
// cover: both live in `Engine::render_latest` and need a real window surface,
// which no test in this workspace has. They are verified by running the
// `profiling` example and reading the block it prints — see that example's
// header for the exact command and what to look for.

/// Encodes one frame carrying a solid box and a text line onto a 64×64 target,
/// and returns the render thread's drained ring.
fn encode_one_frame() -> Option<SampleBlock> {
    let (device, queue) = try_device()?;
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
        label: Some("sub-scope target"),
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
        smooth: 0.0,
    });
    frame.push_text(byard_core::frame::TextLine {
        x: 2.0,
        y: 40.0,
        text: "sub-scopes".to_string(),
        font_size: 12.0,
        color: [1.0, 1.0, 1.0, 1.0],
        dirty: true,
    });

    let _ = drain_samples();
    let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
    queue.submit(std::iter::once(cmd));
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    Some(drain_samples())
}

#[test]
fn every_encode_sub_scope_is_entered_on_a_drawn_frame() {
    let Some(block) = encode_one_frame() else {
        eprintln!("no GPU adapter available — skipping");
        return;
    };
    for scope in [
        "encode.frame",
        "encode.uploads",
        "encode.glyphs",
        "encode.passes",
        "encode.buffers",
    ] {
        assert_entered(&block, scope);
    }
}

#[test]
fn the_encode_sub_scopes_nest_inside_encode_frame() {
    let Some(block) = encode_one_frame() else {
        eprintln!("no GPU adapter available — skipping");
        return;
    };
    // `encode.frame` is the only depth-0 encode scope: a sub-scope recorded at
    // depth 0 would be summed into the frame total a *second* time, which is
    // the exact class of defect RFC-0030 §I2 exists to prevent.
    for (i, sample) in block.samples.iter().enumerate() {
        let name = scope_name(sample.scope).unwrap_or("<unknown>");
        if !name.starts_with("encode.") || name == "encode.frame" {
            continue;
        }
        assert!(
            sample.depth() > 0,
            "{name} (sample {i}) was recorded at depth 0 — it must nest inside \
             encode.frame, or the frame total double-counts it"
        );
    }
}

#[test]
fn encode_frame_self_times_sum_to_its_inclusive_time() {
    let Some(block) = encode_one_frame() else {
        eprintln!("no GPU adapter available — skipping");
        return;
    };
    let root = block
        .samples
        .iter()
        .position(|s| scope_name(s.scope) == Some("encode.frame"))
        .expect("encode.frame was entered");

    // Every nanosecond inside `encode.frame` is attributed to exactly one
    // scope in its subtree. This is the property the whole breakdown rests on:
    // if it fails, a sub-scope was mis-nested or synthesised, and the numbers
    // in `support/PERF_encode_baseline.md` cannot be added up by a reader.
    let total = subtree_self_ns(&block, root);
    assert_eq!(
        total,
        block.samples[root].duration_ns(),
        "self-times in the encode.frame subtree must sum to its inclusive time"
    );
}

/// Sum of [`SampleBlock::self_ns`] over the sample at `index` and everything
/// nested inside it.
fn subtree_self_ns(block: &SampleBlock, index: usize) -> u64 {
    let mut total = block.self_ns(index);
    block.for_each_direct_child(index, |child_index, _| {
        total += subtree_self_ns(block, child_index);
    });
    total
}

// ── The GPU instance arena (RFC-0033 §G5) ──────────────────────────────────
//
// The acceptance condition is a counter, not a benchmark: a steady-state frame
// must create **zero** GPU buffers. That is deterministic, so it is one of the
// few performance claims that can be enforced on shared CI hardware, and it is
// the reason this change is defensible even though the sub-scope measurement
// put per-frame buffer creation at 0.3–3.4 % of the encode cost.

#[test]
fn a_steady_state_frame_creates_no_gpu_buffers() {
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
        label: Some("arena counter target"),
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

    // A frame with something in every pool the arena serves.
    let build_frame = |shade: f32| {
        let mut frame = RenderFrame::new();
        for i in 0..8 {
            frame.push_instance(BoxInstance {
                rect: [i as f32 * 4.0, 0.0, 4.0, 4.0],
                color: [shade, 0.0, 0.0, 1.0],
                radii: [0.0; 4],
                transform: Transform::IDENTITY,
                smooth: 0.0,
            });
        }
        frame.push_text(byard_core::frame::TextLine {
            x: 2.0,
            y: 40.0,
            text: "arena".to_string(),
            font_size: 12.0,
            color: [1.0, 1.0, 1.0, 1.0],
            dirty: true,
        });
        frame
    };

    // Warm-up: the first frames may grow the arena to this scene's high-water
    // mark, which is the whole point of a grow-only policy.
    for i in 0..3 {
        let frame = build_frame(i as f32 / 8.0);
        let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
        queue.submit(std::iter::once(cmd));
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
    }

    let creations = enc.arena().buffer_creations();
    let grows = enc.arena().grows_this_session();

    for i in 0..10 {
        let frame = build_frame(i as f32 / 16.0);
        let cmd = enc.encode_frame_from_relay(&target, &frame).unwrap();
        queue.submit(std::iter::once(cmd));
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
    }

    assert_eq!(
        enc.arena().buffer_creations(),
        creations,
        "a steady-state frame created a GPU buffer — every instanced pipeline \
         must draw from the arena, and a new `create_buffer*` on the per-frame \
         path is the regression this counter exists to catch"
    );
    assert_eq!(
        enc.arena().grows_this_session(),
        grows,
        "the arena grew after warm-up on a scene of constant size"
    );
}
