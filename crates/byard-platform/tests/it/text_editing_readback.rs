//! A text field's selection and caret, in pixels (RFC-0041 §6).
//!
//! The frame says where the selection box and the caret were asked to be; this
//! says what reached the screen. Two claims, the two the RFC names:
//!
//! 1. the selection is painted **behind** the glyphs: inside it the text is
//!    still the colour it was written in, at full strength, not tinted by a
//!    box drawn over it;
//! 2. the caret is drawn at the caret's x, **not at the end** of the text.
//!
//! Skips cleanly when no GPU adapter is available.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::sync::Arc;

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use byard_core::{EventKind, InputEvent, InputPayload, KeyModifiers};

const SIZE: u32 = 256;
/// Large type, so a glyph has pixels it covers completely: the claim is about
/// what colour they are.
const FONT: f32 = 40.0;

const SRC: &str = r#"
View Main() {
    var q: Str = ""
    TextField #[bind: q, bg: 0x20242C, color: 0xFFFFFF, size: 40, width: 240, height: 80]
}
"#;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(byard_core::engine::device_limits)
}

/// The field, driven with the events a window would send.
struct Field {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    frame: RenderFrame,
    t: u64,
}

impl Field {
    fn new() -> Self {
        let parsed = parse(SRC);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        interp.load_views(&parsed.views);
        let tree = interp.lower_view(&parsed.views[0], &["Main"]);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut field = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            t: 0,
        };
        field.send(&[]);
        field
    }

    fn send(&mut self, events: &[InputEvent]) {
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame = RenderFrame::new();
        self.frame.request_full_redraw();
        self.interp
            .render(&self.tree, &mut self.frame, SIZE as f32, SIZE as f32);
    }

    fn event(
        &mut self,
        kind: EventKind,
        payload: Option<InputPayload>,
        mods: KeyModifiers,
    ) -> InputEvent {
        self.t += 50;
        InputEvent {
            kind,
            pos: (120.0, 40.0),
            delta: (0.0, 0.0),
            payload,
            time_ms: self.t,
            mods,
        }
    }

    fn focus(&mut self) {
        let down = self.event(EventKind::PointerDown, None, KeyModifiers::default());
        let up = self.event(EventKind::PointerUp, None, KeyModifiers::default());
        self.send(&[down, up]);
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let e = self.event(
                EventKind::TextInput,
                Some(InputPayload::Key(c.to_string())),
                KeyModifiers::default(),
            );
            self.send(&[e]);
        }
    }

    fn key(&mut self, name: &str, shift: bool, times: usize) {
        for _ in 0..times {
            let e = self.event(
                EventKind::KeyDown,
                Some(InputPayload::Key(name.into())),
                KeyModifiers {
                    shift,
                    ..KeyModifiers::default()
                },
            );
            self.send(&[e]);
        }
    }

    fn window_focus(&mut self, focused: bool) {
        let e = self.event(
            EventKind::WindowFocus,
            Some(InputPayload::Bool(focused)),
            KeyModifiers::default(),
        );
        self.send(&[e]);
    }
}

/// The pixels of `frame`, as RGB rows, drawn at scale 1.
fn draw(
    device: &Arc<wgpu::Device>,
    queue: &Arc<wgpu::Queue>,
    enc: &mut EncoderSubsystem,
    frame: &RenderFrame,
) -> Vec<[u8; 3]> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("text editing target"),
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

    let bpr = 256 * (SIZE * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("text editing readback"),
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
    let mut out = Vec::with_capacity((SIZE * SIZE) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let i = (y * bpr + x * 4) as usize;
            out.push([data[i], data[i + 1], data[i + 2]]);
        }
    }
    out
}

fn at(px: &[[u8; 3]], x: u32, y: u32) -> [u8; 3] {
    px[(y * SIZE + x) as usize]
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

fn width_of(text: &str) -> f32 {
    byard_core::text::TextMeasurer::new()
        .measure(text, FONT, 400, None)
        .0
}

fn brightness(p: [u8; 3]) -> u32 {
    p.iter().map(|&c| u32::from(c)).sum()
}

#[test]
fn a_selection_is_painted_behind_the_glyphs() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping the text editing readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let mut field = Field::new();
    field.focus();
    field.type_text("hello wrold");
    // Select `wr`: three back from the end, then two more with Shift.
    field.key("ArrowLeft", false, 3);
    field.key("ArrowLeft", true, 2);
    assert_eq!(field.interp.focused_selection(), Some((8, 6)));

    let boxes: Vec<_> = field.frame.decorated().to_vec();
    assert_eq!(boxes.len(), 1, "one selection box");
    let [bx, by, bw, bh] = boxes[0].base.rect;
    let px = draw(&device, &queue, &mut enc, &field.frame);

    // The box is where the selected text is: from after `hello ` to after `hello wr`.
    let (x0, x1) = (10.0 + width_of("hello "), 10.0 + width_of("hello wr"));
    assert!(
        (bx - x0).abs() < 0.5 && (bx + bw - x1).abs() < 0.5,
        "{bx} {bw}"
    );

    // A glyph-free row inside the box (its first) is tinted, and the same row
    // outside it is the field's own colour.
    let row = by.ceil() as u32 + 1;
    let inside = at(&px, (bx + bw / 2.0) as u32, row);
    let outside = at(&px, (bx - 8.0) as u32, row);
    assert!(
        inside.iter().zip(outside).any(|(a, b)| a.abs_diff(b) > 8),
        "the selection tints the field: {inside:?} inside, {outside:?} outside"
    );

    // And the text inside it is the colour it was written in, at full strength.
    // A box drawn over the glyphs would tint them: the theme's primary at 30 %
    // over white leaves nothing brighter than about 600 of 765.
    let mut brightest = 0;
    for y in by as u32..(by + bh) as u32 {
        for x in (bx + 1.0) as u32..(bx + bw - 1.0) as u32 {
            brightest = brightest.max(brightness(at(&px, x, y)));
        }
    }
    assert!(
        brightest >= 750,
        "inside the selection the brightest pixel is {brightest} of 765: the box was \
         drawn over the glyphs instead of behind them"
    );
}

#[test]
fn the_caret_is_drawn_at_the_carets_x_and_not_at_the_end() {
    let Some((device, queue, _turn)) = try_device() else {
        eprintln!("no GPU adapter, skipping the text editing readback");
        return;
    };
    let mut enc = encoder(&device, &queue);
    let mut field = Field::new();
    field.focus();
    field.type_text("hello wrold");
    field.key("ArrowLeft", false, 3);

    // With the caret, and the same frame without it (a window nobody is in
    // does not show one). What differs between them is the caret, and only it.
    let with = draw(&device, &queue, &mut enc, &field.frame);
    field.window_focus(false);
    let without = draw(&device, &queue, &mut enc, &field.frame);

    let mut columns: Vec<u32> = Vec::new();
    let mut rows: Vec<u32> = Vec::new();
    for y in 0..SIZE {
        for x in 0..SIZE {
            if at(&with, x, y) != at(&without, x, y) {
                columns.push(x);
                rows.push(y);
            }
        }
    }
    assert!(!columns.is_empty(), "the caret was not drawn at all");
    let (first, last) = (
        *columns.iter().min().unwrap(),
        *columns.iter().max().unwrap(),
    );
    let caret_x = 10.0 + width_of("hello wr") - 0.5;
    assert!(
        first as f32 >= caret_x - 1.5 && last as f32 <= caret_x + 3.0,
        "the caret is in columns {first}..={last}, expected about {caret_x}"
    );
    let end_x = 10.0 + width_of("hello wrold");
    assert!(
        (last as f32) < end_x - 10.0,
        "nothing is drawn at the end of the text ({end_x}): the caret reached column {last}"
    );
    let height = rows.iter().max().unwrap() - rows.iter().min().unwrap() + 1;
    assert!(
        height as f32 >= FONT - 4.0,
        "the caret is as tall as the text, not {height} rows"
    );
}
