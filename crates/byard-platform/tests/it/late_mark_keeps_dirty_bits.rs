//! A frame the render thread finishes late does not take another frame's dirty
//! bits with it.
//!
//! The relay is latest-wins, and a frame the render thread never drew hands its
//! dirty bits to the frame that replaces it. The render thread says which
//! frames it drew by marking them after it has encoded them, and encoding takes
//! milliseconds, so by then the logic thread has usually published newer
//! frames. If that mark is read as "everything in the slot has been drawn",
//! the next publish drops the newer frames' dirty bits, and a line that changed
//! in one of them is reported clean by every frame after it. Shaping is
//! content-addressed, so the glyphs are right; `dirty` is what builds the
//! incremental redraw region, so the line is clipped out of the area that is
//! repainted, and in a debug build the glyph pipeline stops the process with
//! "a text primitive changed while its upstream dirty flag stayed unset".
//!
//! It showed up as a crash in `byard dev`, roughly one run in three, with the
//! window idle: a test that renders one frame at a time cannot see it, because
//! the bug is the interleaving. So the interleaving is written out here, one
//! step at a time, on the real interpreter, relay and encoder, and the frame
//! is checked two ways: against the invariant itself (a line whose content
//! changed since the frame the encoder last saw is dirty), which holds in a
//! release build too, and by letting the encoder's own assertion look at it.
//!
//! The encoder half skips cleanly when no GPU adapter is available.
#![allow(clippy::cast_precision_loss)]

use std::sync::Arc;

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use byard_core::platform::{EventKind, InputEvent};
use byard_core::relay::Relay;

const W: u32 = 400;
const H: u32 = 300;

/// Two counters that change independently, each on its own line: the failure
/// needs a line that changed in a dropped frame and a *different* line that is
/// dirty in the frame the encoder sees next.
const SRC: &str = r#"
View Main() {
    var a: Int = 0
    var b: Int = 0
    Column #[bg: 0x14141C, p: 32, gap: 20, width: 360] {
        Row #[gap: 10] {
            Button("AAA") #[bg: 0x262636, radius: 10, color: 0xFFFFFF] => { a++ }
            Button("BBB") #[bg: 0x262636, radius: 10, color: 0xFFFFFF] => { b++ }
        }
        Text("a {a}") #[color: 0x9AA0AC, size: 14]
        Text("b {b}") #[color: 0x6A7080, size: 12]
    }
}
"#;

/// The logic thread, run by hand: every tick publishes, exactly as the
/// relay's logic loop does, so the test controls the interleaving instead of
/// hoping a scheduler produces it.
struct Logic {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    relay: Arc<Relay>,
    now: u32,
}

impl Logic {
    fn start(relay: &Arc<Relay>) -> Self {
        let program = byard_compiler::parser::parse(SRC);
        assert!(program.errors.is_empty(), "{:?}", program.errors);
        let mut interp = Interpreter::new();
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        interp.load_views(&program.views);
        let tree = interp.lower_view(&program.views[0], &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut logic = Self {
            interp,
            tree,
            relay: Arc::clone(relay),
            now: 0,
        };
        logic.tick(&[]);
        logic
    }

    /// One logic tick, published. Reuses a recycled frame, as the dev runner
    /// does.
    fn tick(&mut self, events: &[InputEvent]) {
        self.now += 16;
        self.interp.set_now_ms(self.now);
        self.interp.dispatch_events(events);
        self.interp.tick();
        let mut frame = self.relay.acquire_recycled();
        self.interp
            .render(&self.tree, &mut frame, W as f32, H as f32);
        self.relay.publish(frame);
    }

    /// Presses and releases the button labelled `label` over two ticks, then
    /// ticks once more with nothing happening. Each of the three frames is
    /// published.
    fn tap(&mut self, label: &str) {
        let pos = {
            let frame = self.relay.current().expect("a published frame");
            let line = frame
                .texts()
                .iter()
                .find(|t| t.text == label)
                .unwrap_or_else(|| panic!("a text {label}"));
            (line.x + 4.0, line.y + 4.0)
        };
        let event = |kind, dt| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: u64::from(self.now) + dt,
        };
        let down = event(EventKind::PointerDown, 0);
        let up = event(EventKind::PointerUp, 8);
        self.tick(&[down]);
        self.tick(&[up]);
        self.tick(&[]);
    }
}

/// What the paint side keys a line's shaped glyphs on, plus its colour: the
/// same fields, so "the content changed" means what the encoder means by it.
fn signature(frame: &RenderFrame, i: usize) -> String {
    let line = &frame.texts()[i];
    format!(
        "{}|{}|{:?}|{}|{:?}|{:?}",
        line.text,
        line.font_size,
        line.color,
        line.weight,
        line.family,
        frame.text_wraps().get(i)
    )
}

/// Lines whose content differs from the frame the encoder last saw and that
/// are not marked dirty, which is the assertion the glyph pipeline makes.
fn changed_but_clean(seen: &RenderFrame, next: &RenderFrame) -> Vec<String> {
    if seen.texts().len() != next.texts().len() {
        return Vec::new();
    }
    (0..next.texts().len())
        .filter(|&i| signature(seen, i) != signature(next, i) && !next.texts()[i].dirty)
        .map(|i| next.texts()[i].text.clone())
        .collect()
}

/// A device and target to encode onto, or `None` when there is no GPU.
struct Gpu {
    enc: EncoderSubsystem,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    target: wgpu::Texture,
    _turn: byard_test_gpu::Turn,
}

impl Gpu {
    fn open() -> Option<Self> {
        let (device, queue, turn) = byard_test_gpu::device(byard_core::engine::device_limits)?;
        let mut enc = pollster::block_on(EncoderSubsystem::init(
            Arc::clone(&device),
            Arc::clone(&queue),
            wgpu::TextureFormat::Rgba8UnormSrgb,
            1.0,
            W,
            H,
        ))
        .expect("encoder init");
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
            label: Some("late mark target"),
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
        Some(Self {
            enc,
            device,
            queue,
            target,
            _turn: turn,
        })
    }

    /// Encodes `frame` the way the render thread does. In a debug build the
    /// glyph pipeline asserts the dirty-flag invariant in here.
    fn encode(&mut self, frame: &RenderFrame) {
        let cmd = self
            .enc
            .encode_frame_from_relay(&self.target, frame)
            .expect("encode");
        self.queue.submit(std::iter::once(cmd));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

#[test]
fn a_late_mark_does_not_drop_the_dirty_bits_of_frames_it_never_saw() {
    let relay = Arc::new(Relay::new().expect("relay"));
    let mut logic = Logic::start(&relay);
    let mut gpu = Gpu::open();

    // The render thread takes the current frame and starts encoding it. The
    // encoder now holds this frame's text.
    let first = relay.current().expect("a published frame");
    if let Some(gpu) = gpu.as_mut() {
        gpu.encode(&first);
    }

    // While it encodes, the logic thread keeps publishing. `a` changes, then a
    // frame with no change follows it, so the change is in a frame that is
    // neither the one being drawn nor the newest one.
    logic.tap("AAA");
    assert!(
        relay
            .current()
            .unwrap()
            .texts()
            .iter()
            .any(|t| t.text == "a 1"),
        "the tap must have reached the frame, or this test proves nothing"
    );

    // The encode finishes and marks the frame it drew, which is not the
    // frame in the slot.
    relay.mark_rendered(first.version());

    // Now `b` changes. The frame the render thread takes next must say that
    // `a` changed too, because the encoder has never seen `a 1`.
    logic.tap("BBB");
    let next = relay.current().expect("a published frame");
    assert!(
        next.texts().iter().any(|t| t.text == "b 1"),
        "the second tap must have reached the frame"
    );

    let lost = changed_but_clean(&first, &next);
    assert!(
        lost.is_empty(),
        "lines that changed since the frame last drawn, reported clean: {lost:?}"
    );
    if let Some(gpu) = gpu.as_mut() {
        gpu.encode(&next);
    }
}
