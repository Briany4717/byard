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

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

/// An encoder and a 256×256 offscreen target, ready to encode frames into.
struct Harness {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    enc: EncoderSubsystem,
    target: wgpu::Texture,
    /// Held for as long as the harness can draw.
    _turn: byard_test_gpu::Turn,
}

impl Harness {
    fn new() -> Option<Self> {
        let (device, queue, turn) = try_device()?;
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
            _turn: turn,
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
        family: None,
        variations: None,
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
fn a_still_scene_after_an_unmount_re_shapes_nothing() {
    // A panel of four lines unmounts, leaving eight. The frame it unmounts on
    // shifts indices, so reshaping there is right. The frames after it are
    // the same eight lines, and must be compared line by line again: the
    // cache used to keep its high-water length, so every later frame looked
    // "shorter" than the cache and reshaped the whole pool, forever.
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    let mut with_panel = app.clone();
    with_panel.extend((0..4).map(|i| format!("panel row {i}")));

    h.encode(&frame_with(&with_panel, None));
    let without = frame_with(&app, None);
    h.encode(&without);
    for _ in 0..3 {
        h.encode(&without);
        assert_eq!(
            h.reshapes(),
            0,
            "the same eight lines, after the panel is gone, reshape nothing"
        );
    }

    // And growing back to the old length is a length change like any other:
    // the returning panel is shaped, not assumed from what that index held
    // two frames ago.
    let mut other_panel = app.clone();
    other_panel.extend((0..4).map(|i| format!("other row {i}")));
    h.encode(&frame_with(&other_panel, None));
    assert!(
        h.reshapes() >= 4,
        "a panel mounting at the old high-water length is shaped: {}",
        h.reshapes()
    );
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

/// What a change to the pool's length costs, in lines shaped. Each test below
/// states its number; this prints them next to it so a run shows the cost.
fn report(what: &str, reshaped: usize) {
    eprintln!("{what}: {reshaped} re-shaped");
}

#[test]
fn appending_one_line_re_shapes_one_line() {
    // The pool grows by one. The glyph cache used to call a length change
    // "not index-comparable" and shape the whole pool; a line is now compared
    // by its own key, so the eight that did not change are not touched.
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    h.encode(&frame_with(&app, None));
    let mut longer = app.clone();
    longer.push("a ninth row".to_string());
    h.encode(&frame_with(&longer, None));
    report("append one line to eight", h.reshapes());
    assert_eq!(h.reshapes(), 1, "only the new line is shaped");
    h.encode(&frame_with(&longer, None));
    assert_eq!(h.reshapes(), 0, "and the next frame is steady");
}

#[test]
fn removing_the_last_line_re_shapes_nothing() {
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    h.encode(&frame_with(&app, None));
    h.encode(&frame_with(&app[..app.len() - 1], None));
    report("remove the last line of eight", h.reshapes());
    assert_eq!(h.reshapes(), 0, "every line left is where it was");
}

#[test]
fn removing_the_first_line_re_shapes_only_the_lines_whose_key_moved() {
    // Index-addressed, so every line after the removed one now sits on an
    // index that held its neighbour: its key moved, and it is shaped again.
    // That is the floor of the scheme, and it is exact: a line whose
    // neighbour has the same text (here, the two "same" rows) keeps its
    // buffer, because the key says nothing else differs.
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let rows: Vec<String> = ["first", "same", "same", "alpha", "beta", "gamma"]
        .iter()
        .map(ToString::to_string)
        .collect();
    h.encode(&frame_with(&rows, None));
    h.encode(&frame_with(&rows[1..], None));
    report("remove the first of six lines", h.reshapes());
    // ["same","same","alpha","beta","gamma"] against
    // ["first","same","same","alpha","beta"][..5]: index 0 first->same moved,
    // index 1 same->same did not, 2 same->alpha, 3 alpha->beta, 4 beta->gamma.
    assert_eq!(h.reshapes(), 4);

    // A middle line: only what follows it moves.
    let mut h2 = Harness::new().expect("the first harness had a GPU");
    let app = app_lines();
    h2.encode(&frame_with(&app, None));
    let mut gone = app.clone();
    gone.remove(5);
    h2.encode(&frame_with(&gone, None));
    report("remove the sixth of eight lines", h2.reshapes());
    assert_eq!(h2.reshapes(), 2, "rows 5 and 6 slid up; rows 0-4 did not");
}

#[test]
fn the_hud_opening_and_closing_shapes_only_the_hud() {
    // The HUD's lines are the tail of the pool. Opening it appends them,
    // closing it truncates them: the app's eight lines are never touched, and
    // closing costs nothing at all. Reopening reuses the spare buffers the
    // close kept, and shapes the fields again (a buffer's content is not kept).
    let Some(mut h) = Harness::new() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let app = app_lines();
    let fields = hud_fields(3.4);
    h.encode(&frame_with(&app, None));
    for cycle in 0..3 {
        h.encode(&frame_with(&app, Some(&fields)));
        report(&format!("HUD open, cycle {cycle}"), h.reshapes());
        assert_eq!(h.reshapes(), fields.len(), "open: the HUD's lines only");
        h.encode(&frame_with(&app, None));
        report(&format!("HUD close, cycle {cycle}"), h.reshapes());
        assert_eq!(h.reshapes(), 0, "close: nothing left to shape");
    }
}

#[test]
fn losing_the_fixed_width_padding_costs_the_fields_that_moved_and_not_the_apps_lines() {
    // The padding exists so that a changing number keeps its string, and the
    // pool its length. This pins what losing it costs now. A HUD that
    // interpolated widths into `byld` used to change the pool's length,
    // which made every index incomparable and re-shaped the *app's* text for
    // the HUD's formatting (and then forced a full redraw). Its own fields
    // still pay, because their strings do change; the app's lines do not.
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

    // One field longer: the pool's length changes.
    let mut fields = ragged(3.4);
    fields.push("more".to_string());
    h.encode(&frame_with(&app, Some(&fields)));
    report("an unpadded HUD grows a field", h.reshapes());
    assert_eq!(
        h.reshapes(),
        1,
        "the new field is shaped; the app's {} lines pay nothing for the \
         HUD's formatting",
        app.len()
    );

    // And the value crossing a digit changes the fields' own strings.
    h.encode(&frame_with(&app, Some(&ragged(12.75))));
    report("an unpadded value crosses ten", h.reshapes());
    assert_eq!(
        h.reshapes(),
        2,
        "`3`,`4` became `12`,`75`; `more` is the same string at the same index"
    );
}
