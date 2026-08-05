//! GPU readback proofs for RFC-0026 route transitions.
//!
//! The compiler-side tests read the *frame data* a navigation produces; these
//! run the same `.byd` sources through the real encoder on a real device and
//! read the pixels back, so what is asserted is what a user would see: a screen
//! that genuinely slides in from the right edge, a covered screen that drifts
//! the other way underneath it, a pop that reverses the whole thing, and a
//! cross-fade that really blends two screens together.
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

const LOGICAL_W: f32 = 240.0;
const LOGICAL_H: f32 = 120.0;
const SCALE: f32 = 2.0;

/// The two screens' fills, chosen so a single channel identifies each.
const HOME_HEX: &str = "0xFF2020";
const DETAIL_HEX: &str = "0x2020FF";

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("nav transition readback device"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

/// One scanline of the rendered frame, as `(b, g, r)` triples in logical-x
/// order, the raw evidence every assertion below is read from.
fn scanline(
    device: &Arc<wgpu::Device>,
    queue: &Arc<wgpu::Queue>,
    frame: &RenderFrame,
) -> Vec<(u8, u8, u8)> {
    let phys_w = (LOGICAL_W * SCALE) as u32;
    let phys_h = (LOGICAL_H * SCALE) as u32;
    let fmt = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(device),
        Arc::clone(queue),
        fmt,
        SCALE,
        phys_w,
        phys_h,
    ))
    .unwrap();
    enc.update_viewport(
        Viewport {
            width: LOGICAL_W,
            height: LOGICAL_H,
        },
        phys_w,
        phys_h,
        SCALE,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("nav transition target"),
        size: wgpu::Extent3d {
            width: phys_w,
            height: phys_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: fmt,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let cmd = enc.encode_frame_from_relay(&target, frame).unwrap();
    queue.submit(std::iter::once(cmd));

    let bpr = 256 * (phys_w * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("nav transition readback"),
        size: u64::from(bpr) * u64::from(phys_h),
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
                rows_per_image: Some(phys_h),
            },
        },
        wgpu::Extent3d {
            width: phys_w,
            height: phys_h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();

    let row = phys_h / 2;
    (0..phys_w)
        .map(|x| {
            let idx = (row * bpr + x * 4) as usize;
            (data[idx], data[idx + 1], data[idx + 2])
        })
        .collect()
}

/// A live navigation app being driven frame by frame against a real device.
struct Driver {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    interp: Interpreter,
    tree: Vec<RenderNode>,
}

impl Driver {
    /// Lowers `src`, or returns `None` when there is no GPU to render it with.
    fn new(src: &str) -> Option<Self> {
        let (device, queue) = try_device()?;
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let mut interp = Interpreter::new();
        let tree = interp.lower_view(&parsed.views[0], &[]);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        interp.tick();
        Some(Self {
            device,
            queue,
            interp,
            tree,
        })
    }

    fn set_path(&mut self, path: &str) {
        let sig = self.interp.var_signal(&Symbol::intern("navPath")).unwrap();
        self.interp.write_var(sig, Value::Str(path.to_string()));
        self.interp.tick();
    }

    /// Renders at engine time `ms` and reads the middle scanline back.
    fn at(&mut self, ms: u32) -> Vec<(u8, u8, u8)> {
        self.interp.set_now_ms(ms);
        let mut frame = RenderFrame::new();
        self.interp
            .render(&self.tree, &mut frame, LOGICAL_W, LOGICAL_H);
        scanline(&self.device, &self.queue, &frame)
    }
}

/// The leading edge, in logical px, of the first run of pixels satisfying
/// `is_match`, "where does that screen start on screen?", measured in pixels
/// rather than inferred from frame data.
fn edge_of(line: &[(u8, u8, u8)], is_match: impl Fn((u8, u8, u8)) -> bool) -> Option<f32> {
    line.iter().position(|px| is_match(*px)).map(|x| {
        #[allow(clippy::cast_precision_loss)]
        let x = x as f32;
        x / SCALE
    })
}

// The two fills read back through an sRGB surface, so the *written* `0x20`
// channels land near 99 rather than 32. What identifies a screen is which
// channel is saturated, not the exact byte.

/// Predominantly blue, the detail screen.
fn is_detail(px: (u8, u8, u8)) -> bool {
    px.0 > 200 && px.2 < 160
}

/// Predominantly red, the home screen.
fn is_home(px: (u8, u8, u8)) -> bool {
    px.2 > 200 && px.0 < 160
}

/// A two-screen stack with `transition` driving the swap.
fn nav_source(transition: &str) -> String {
    format!(
        "View V() {{ \
         var navPath = \"/\" \
         NavStack(path: navPath) #[transition: {transition}, grow: 1] {{ \
             route \"/\" {{ Box #[bg: {HOME_HEX}, grow: 1] {{}} }} \
             route \"/detail\" {{ Box #[bg: {DETAIL_HEX}, grow: 1] {{}} }} \
         }} }}"
    )
}

#[test]
fn a_push_slides_the_incoming_screen_in_from_the_right_edge() {
    let Some(mut app) = Driver::new(&nav_source("slide")) else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    // At rest the home screen owns every pixel.
    let line = app.at(0);
    assert!(is_home(line[line.len() / 2]), "the home screen is up");
    assert!(
        edge_of(&line, is_detail).is_none(),
        "and the detail screen is nowhere on screen"
    );

    app.set_path("/detail");
    app.at(1); // the push starts on the next render

    // The detail screen's left edge marches in from the right.
    let mut edges = Vec::new();
    for ms in [10, 60, 120, 200] {
        let line = app.at(ms);
        edges.push(edge_of(&line, is_detail).expect("the arriving screen is on screen"));
    }
    assert!(
        edges[0] > LOGICAL_W * 0.5,
        "it starts off to the right: {edges:?}"
    );
    assert!(
        edges.windows(2).all(|w| w[1] <= w[0] + 1.0),
        "and only ever moves left: {edges:?}"
    );
    assert!(
        *edges.last().unwrap() < LOGICAL_W * 0.35,
        "getting most of the way home: {edges:?}"
    );

    // Settled: the detail screen fills the viewport and the home screen is gone.
    let line = app.at(4_000);
    assert!(
        edge_of(&line, is_detail).unwrap() < 1.0,
        "the arrived screen is flush to the left edge"
    );
    assert!(
        edge_of(&line, is_home).is_none(),
        "and the screen it covered has stopped painting"
    );
}

#[test]
fn the_covered_screen_drifts_the_other_way_under_the_incoming_one() {
    let Some(mut app) = Driver::new(&nav_source("slide")) else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    app.at(0);
    app.set_path("/detail");
    app.at(1);
    let line = app.at(60);
    // Both screens are on screen at once, home to the left of detail, the
    // parallax pair the RFC describes, in actual pixels.
    let home = edge_of(&line, is_home).expect("the covered screen still paints");
    let detail = edge_of(&line, is_detail).expect("the arriving screen paints");
    assert!(
        home < detail,
        "home sits under and left of detail: {home} < {detail}"
    );
    // The covered screen has been pushed partly off the leading edge, so what
    // remains of it starts at 0 and ends before the detail screen begins.
    assert!(
        home < 1.0,
        "the covered screen is clipped at the container edge"
    );
    assert!(
        detail < LOGICAL_W,
        "and the incoming screen has entered the viewport: {detail}"
    );
}

#[test]
fn a_pop_reverses_the_slide_on_screen() {
    let Some(mut app) = Driver::new(&nav_source("slide")) else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    app.at(0);
    app.set_path("/detail");
    app.at(1);
    app.at(4_000);

    app.set_path("/");
    app.at(4_001);
    // Popping walks the detail screen back out to the right.
    let mut edges = Vec::new();
    for ms in [4_010, 4_060, 4_120, 4_200] {
        let line = app.at(ms);
        edges.push(edge_of(&line, is_detail).expect("the leaving screen is still on screen"));
    }
    assert!(
        edges.windows(2).all(|w| w[1] >= w[0] - 1.0),
        "the popped screen only ever moves right: {edges:?}"
    );
    let line = app.at(8_000);
    assert!(
        edge_of(&line, is_detail).is_none(),
        "and is gone once the pop settles"
    );
    assert!(is_home(line[line.len() / 2]), "revealing the home screen");
}

#[test]
fn a_fade_blends_the_two_screens_instead_of_moving_them() {
    let Some(mut app) = Driver::new(&nav_source("fade")) else {
        eprintln!("no GPU adapter, skipping readback");
        return;
    };
    app.at(0);
    app.set_path("/detail");
    app.at(1);

    let line = app.at(90);
    let px = line[line.len() / 2];
    // Mid-fade the centre pixel is neither screen's pure fill: the incoming
    // blue is compositing over the outgoing red, so *both* channels are lifted
    // off their resting value and neither is saturated.
    assert!(
        !is_home(px) && !is_detail(px) && px.0 > 140 && px.2 > 140,
        "both screens contribute to the same pixel mid-fade: {px:?}"
    );
    // Nothing moved: the blend covers the whole width, edge to edge.
    let (first, last) = (line[0], line[line.len() - 1]);
    assert!(
        !is_home(first) && !is_home(last),
        "a fade never displaces a screen: {first:?} … {last:?}"
    );

    let line = app.at(4_000);
    assert!(
        is_detail(line[line.len() / 2]),
        "and it settles on the incoming screen alone"
    );
}
