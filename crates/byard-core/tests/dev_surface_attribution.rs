//! The render thread's half of RFC-0030 §V4's self-accounting, and the
//! content-addressed glyph cache that makes it affordable.
//!
//! Two findings from the self-accounting erratum live here, because both are
//! properties of the *encoder* and neither is visible from the logic thread:
//!
//! - **§A1.** A dev overlay's text is shaped inside `encode.glyphs`, long after
//!   the overlay's own `hud.render` scope has been dropped. So the frame
//!   carries a partition (`RenderFrame::set_dev_base`) and the encoder charges
//!   the dev half to `Owner::DevTools`. Without it, the largest single term in
//!   what the HUD costs is billed to the app the HUD is measuring.
//!
//! - **§A2.** The HUD's numeric fields are formatted to a fixed width so their
//!   strings are byte-identical on five of every six frames. That only saves
//!   anything if the shaper is asked "did this change?" rather than told; the
//!   interpreter sets `dirty: true` on every leaf of every frame, so the old
//!   flag-driven gate re-shaped all of it regardless.
//!
//! The re-shape assertions are **counted, not timed**. `reshaped_lines` is a
//! number the encoder reports about itself, so these tests fail for the reason
//! they name instead of for whatever else the machine was doing.

#![cfg(feature = "telemetry")]

use std::sync::Arc;

use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{BoxInstance, RenderFrame, TextLine, Transform, Viewport};
use byard_core::telemetry::{Owner, SampleBlock, drain_samples, scope_name};

// ── Harness ────────────────────────────────────────────────────────────────

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ByardCore - Dev Surface Attribution Test Device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

/// An encoder and a 256×256 offscreen target, ready to encode frames into.
struct Harness {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    enc: EncoderSubsystem,
    target: wgpu::Texture,
}

impl Harness {
    fn new() -> Option<Self> {
        let (device, queue) = try_device()?;
        let mut enc = pollster::block_on(EncoderSubsystem::init(
            Arc::clone(&device),
            Arc::clone(&queue),
            wgpu::TextureFormat::Rgba8UnormSrgb,
            1.0,
            256,
            256,
        ))
        .expect("encoder init");
        enc.update_viewport(Viewport::new(256.0, 256.0), 256, 256, 1.0);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dev surface attribution target"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
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
        Some(Self {
            device,
            queue,
            enc,
            target,
        })
    }

    /// Encodes `frame` and returns this thread's drained ring.
    fn encode(&mut self, frame: &RenderFrame) -> SampleBlock {
        let _ = drain_samples();
        let cmd = self
            .enc
            .encode_frame_from_relay(&self.target, frame)
            .expect("encode");
        self.queue.submit(std::iter::once(cmd));
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        drain_samples()
    }

    fn reshapes(&self) -> usize {
        self.enc.last_text_reshapes()
    }
}

fn line(y: f32, text: &str) -> TextLine {
    TextLine {
        x: 4.0,
        y,
        text: text.to_string(),
        font_size: 12.0,
        weight: 400,
        color: [1.0, 1.0, 1.0, 1.0],
        // What the interpreter always sends. These tests are only meaningful
        // because it is `true` here: the point is that it is not the signal.
        dirty: true,
    }
}

/// A frame shaped like a real `byard dev` frame: an app tree, then, when
/// `hud_fields` is `Some`, the dev runner's own layer behind a `dev_base`.
fn frame_with(app_lines: &[String], hud_fields: Option<&[String]>) -> RenderFrame {
    let mut f = RenderFrame::new();
    f.push_instance(BoxInstance {
        rect: [0.0, 0.0, 128.0, 128.0],
        color: [0.1, 0.1, 0.12, 1.0],
        radii: [0.0; 4],
        transform: Transform::IDENTITY,
        smooth: 0.0,
    });
    for (i, text) in app_lines.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        f.push_text(line(16.0 + i as f32 * 14.0, text));
    }
    let base = f.cursor();
    if let Some(fields) = hud_fields {
        f.begin_layer();
        for (i, text) in fields.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            f.push_text(line(160.0 + i as f32 * 10.0, text));
        }
    }
    f.set_dev_base(base);
    f
}

fn app_lines() -> Vec<String> {
    (0..8).map(|i| format!("app row {i}")).collect()
}

/// The HUD's field set, formatted the way `hud::fixed_ms` formats it: a
/// value-independent width, so a changing number keeps a stable string
/// *length* and, with the content-addressed cache, an identical string
/// whenever the value has not moved.
fn hud_fields(work_ms: f64) -> Vec<String> {
    let mut v: Vec<String> = (0..20).map(|i| format!("hud label {i:>3}")).collect();
    v.push(format!("{work_ms:>5.1}"));
    v
}

// ── §A1: the encoder charges dev text to the dev runner ────────────────────

#[test]
fn a_frame_with_no_dev_surfaces_produces_no_dev_owned_samples() {
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let f = frame_with(&app_lines(), None);
    let block = h.encode(&f);
    assert!(
        block
            .samples
            .iter()
            .any(|s| scope_name(s.scope) == Some("encode.glyphs")),
        "the app's shaping still happens"
    );
    assert_eq!(
        block.owner_total_ns(Owner::DevTools),
        0,
        "nothing may be charged to a profiler that is not running"
    );
    assert!(
        !block
            .samples
            .iter()
            .any(|s| scope_name(s.scope) == Some("encode.glyphs.dev")),
        "and the scope must not be entered at all"
    );
}

#[test]
fn the_dev_half_of_glyph_shaping_is_charged_to_the_dev_runner() {
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    // Fresh strings each frame on both sides, so shaping actually happens and
    // there is something to attribute.
    let f = frame_with(&app_lines(), Some(&hud_fields(3.4)));
    let block = h.encode(&f);

    let dev: Vec<_> = block
        .samples
        .iter()
        .filter(|s| s.owner() == Owner::DevTools)
        .collect();
    assert!(
        !dev.is_empty(),
        "the HUD's twenty-one lines were shaped by somebody, and it was not \
         the app"
    );
    assert!(
        dev.iter()
            .any(|s| scope_name(s.scope) == Some("encode.glyphs.dev")),
        "scopes seen: {:?}",
        dev.iter().map(|s| scope_name(s.scope)).collect::<Vec<_>>()
    );
    for s in &dev {
        assert!(
            s.depth() > 0,
            "a dev scope recorded at depth 0 would be summed into the frame \
             total a second time (RFC-0030 §I2)"
        );
    }
}

#[test]
fn the_apps_glyph_row_excludes_what_the_dev_runner_shaped() {
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let f = frame_with(&app_lines(), Some(&hud_fields(3.4)));
    let block = h.encode(&f);

    let glyphs = block
        .samples
        .iter()
        .position(|s| scope_name(s.scope) == Some("encode.glyphs"))
        .expect("encode.glyphs was entered");
    let dev_ns: u64 = (0..block.samples.len())
        .filter(|&i| {
            block.samples[i].owner() == Owner::DevTools
                && scope_name(block.samples[i].scope) == Some("encode.glyphs.dev")
        })
        .map(|i| block.samples[i].duration_ns())
        .sum();

    // Self-time is inclusive minus direct children, and the dev scope is one.
    assert!(
        block.self_ns(glyphs) + dev_ns <= block.samples[glyphs].duration_ns(),
        "the app's row must not contain the dev runner's shaping"
    );

    // And the two owner buckets still reconstruct the frame rather than
    // exceeding it, the §I2b property, one level out.
    assert_eq!(
        block.owner_total_ns(Owner::App) + block.owner_total_ns(Owner::DevTools),
        block.total_ns(),
        "every nanosecond is attributed to exactly one owner"
    );
}

// ── §A2: an unchanged line is not re-shaped ────────────────────────────────

#[test]
fn a_steady_scene_re_shapes_nothing_after_the_first_frame() {
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    let f = frame_with(&app, None);

    h.encode(&f);
    assert_eq!(
        h.reshapes(),
        app.len(),
        "every line is new on the first frame"
    );

    // Same content, `dirty: true` throughout, exactly what the interpreter
    // emits. The old gate re-shaped all of it, every frame, forever.
    for _ in 0..3 {
        h.encode(&f);
        assert_eq!(
            h.reshapes(),
            0,
            "byte-identical text must not be re-shaped just because its \
             producer cannot say so"
        );
    }
}

#[test]
fn opening_the_hud_on_a_steady_scene_re_shapes_nothing() {
    // §A2's acceptance, counted rather than timed. `encode.glyphs` roughly
    // quadrupled when the HUD opened because its twenty-odd fixed-width fields
    // were re-shaped sixty times a second. The fields have always been
    // fixed-width; what was missing was a shaper that looked.
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    let fields = hud_fields(3.4);
    let f = frame_with(&app, Some(&fields));

    h.encode(&f);
    assert_eq!(h.reshapes(), app.len() + fields.len());
    for _ in 0..5 {
        h.encode(&f);
        assert_eq!(h.reshapes(), 0, "five of six HUD frames cost nothing");
    }
}

#[test]
fn only_the_field_that_moved_is_re_shaped_when_the_hud_updates() {
    // The tenth-of-a-second update. One number changes; one line re-shapes.
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    h.encode(&frame_with(&app, Some(&hud_fields(3.4))));
    h.encode(&frame_with(&app, Some(&hud_fields(3.4))));
    assert_eq!(h.reshapes(), 0);

    h.encode(&frame_with(&app, Some(&hud_fields(12.7))));
    assert_eq!(
        h.reshapes(),
        1,
        "` 3.4` → `12.7` is one line's worth of work, not twenty-one's"
    );
}

#[test]
fn losing_the_fixed_width_padding_re_shapes_the_whole_pool() {
    // "Demonstrated red by removing the padding", as an assertion rather than
    // a procedure. Without a value-independent width the string's *length*
    // changes as a number crosses ten, the text pool's length is unaffected
    // but every field's own key moves, and worse, a HUD that interpolated
    // widths into `byld` would change the pool length itself, which makes
    // every index in the frame incomparable and re-shapes the app's text too.
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    // The unpadded form: one field per value, split into two lines once the
    // number needs an extra column, the shape an interpolated `"{t.work}ms"`
    // produces.
    let ragged = |v: f64| {
        let mut fields: Vec<String> = (0..20).map(|i| format!("hud label {i:>3}")).collect();
        for part in format!("{v}").split('.') {
            fields.push(part.to_string());
        }
        fields
    };
    h.encode(&frame_with(&app, Some(&ragged(3.4))));
    h.encode(&frame_with(&app, Some(&ragged(3.4))));
    assert_eq!(h.reshapes(), 0, "a steady value is still steady");

    // 12.75 is one field longer than 3.4, so the pool's length changes and
    // index identity is lost for the whole frame.
    let mut fields = ragged(3.4);
    fields.push("more".to_string());
    let f = frame_with(&app, Some(&fields));
    h.encode(&f);
    assert!(
        h.reshapes() >= app.len(),
        "a pool whose length changed is not index-comparable, so the app's \
         lines pay for the HUD's formatting: {} re-shaped",
        h.reshapes()
    );
}
