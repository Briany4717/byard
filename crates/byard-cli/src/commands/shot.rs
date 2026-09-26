//! `byard shot`: render a project to a PNG, headlessly.
//!
//! The same program `byard dev` and a shipped `App` run (the manifest, the
//! theme and its fonts, every `.byd`, the packages), through the same
//! interpreter and the same GPU encoder, drawn into an offscreen texture
//! instead of a window. Nothing appears on screen, so nothing else on the
//! screen can cover it, and no stray click can change it.
//!
//! This exists because looking at the result is the only check that catches
//! a wrong picture: every text-rendering defect fixed alongside it passed its
//! tests and was obvious in a screenshot. A screenshot of a live window is
//! the right tool when a person is watching; this is the one that works when
//! the machine is busy, and the one a script can repeat exactly.

use std::path::Path;
use std::sync::Arc;

use byard_compiler::interp::eval::Interpreter;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use byard_core::platform::{EventKind, InputEvent};

use crate::deps::resolve_project;
use crate::manifest::Manifest;
use crate::style;

/// What `byard shot` was asked to render.
pub struct ShotArgs<'a> {
    /// The project directory, `byard.toml` or `.byd`; the current project
    /// when absent.
    pub path: Option<&'a Path>,
    /// Where the PNG goes.
    pub out: &'a Path,
    /// Logical size of the viewport.
    pub size: (u32, u32),
    /// Device pixels per logical pixel.
    pub scale: f32,
    /// Milliseconds of app time to run before the picture is taken, so
    /// entrance animations and transitions have settled.
    pub at_ms: u32,
    /// Start in the dark scheme (`Some(true)`), the light one, or whichever
    /// the manifest says (`None`).
    pub dark: Option<bool>,
    /// Logical points to tap, in order, before the picture is taken.
    pub taps: &'a [(f32, f32)],
}

pub fn run(args: &ShotArgs<'_>) -> Result<(), String> {
    let started = std::time::Instant::now();
    let manifest = Manifest::discover(args.path)?;
    let (program, _) = resolve_project(&manifest)?;
    if !program.errors.is_empty() {
        let report: Vec<String> = program
            .errors
            .iter()
            .map(|e| format!("  {}", e.headline()))
            .collect();
        return Err(format!(
            "the program does not compile:\n{}",
            report.join("\n")
        ));
    }
    let root = byard_compiler::parser::ast::root_view(&program.views)
        .ok_or("the program declares no `View`")?;

    let mut interp = Interpreter::new();
    // As `check` does: the framework's own capabilities resolve, so a view
    // that injects `Http` renders its not-yet-loaded state instead of failing.
    interp.declare_controllers(&byard_core::cap::provided_names());
    interp.set_theme(manifest.theme.clone());
    if let Some(dark) = args.dark {
        interp.set_theme_dark(dark);
    }
    interp.load_views(&program.views);
    let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(root, &known);
    let errors: Vec<String> = interp
        .errors()
        .iter()
        .map(|e| format!("  {}", e.headline()))
        .collect();
    if !errors.is_empty() {
        return Err(format!(
            "the program does not compile:\n{}",
            errors.join("\n")
        ));
    }

    #[allow(clippy::cast_precision_loss)]
    let (w, h) = (args.size.0 as f32, args.size.1 as f32);
    let mut frame = RenderFrame::new();
    let mut now = 0_u32;
    let step = |interp: &mut Interpreter,
                frame: &mut RenderFrame,
                now: &mut u32,
                events: &[InputEvent]| {
        interp.set_now_ms(*now);
        interp.dispatch_events(events);
        interp.tick();
        *frame = RenderFrame::new();
        frame.request_full_redraw();
        interp.render(&tree, frame, w, h);
        *now += 16;
    };
    step(&mut interp, &mut frame, &mut now, &[]);
    for &(x, y) in args.taps {
        let at = |kind| InputEvent {
            kind,
            pos: (x, y),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: u64::from(now),
        };
        let tap = [at(EventKind::PointerDown), at(EventKind::PointerUp)];
        step(&mut interp, &mut frame, &mut now, &tap);
    }
    while now < args.at_ms {
        step(&mut interp, &mut frame, &mut now, &[]);
    }
    // Once more at rest, so a mesh or an image that landed a frame late is in.
    step(&mut interp, &mut frame, &mut now, &[]);

    let pixels = paint(&frame, args.size, args.scale)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (pw, ph) = (
        (w * args.scale).round() as u32,
        (h * args.scale).round() as u32,
    );
    image::save_buffer(args.out, &pixels, pw, ph, image::ColorType::Rgba8)
        .map_err(|e| format!("{}: {e}", args.out.display()))?;
    style::ok(
        &format!("{} ({pw}x{ph})", args.out.display()),
        Some(started.elapsed()),
    );
    Ok(())
}

/// Draws `frame` offscreen and reads it back as RGBA8 (sRGB) rows.
fn paint(frame: &RenderFrame, size: (u32, u32), scale: f32) -> Result<Vec<u8>, String> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|e| format!("no GPU adapter: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("byard shot"),
        required_features: wgpu::Features::empty(),
        required_limits: byard_core::engine::device_limits(&adapter),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .map_err(|e| format!("no GPU device: {e}"))?;
    let (device, queue) = (Arc::new(device), Arc::new(queue));
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let (pw, ph) = (
        (size.0 as f32 * scale).round() as u32,
        (size.1 as f32 * scale).round() as u32,
    );
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        Arc::clone(&device),
        Arc::clone(&queue),
        format,
        scale,
        pw,
        ph,
    ))
    .map_err(|e| e.to_string())?;
    #[allow(clippy::cast_precision_loss)]
    enc.update_viewport(
        Viewport {
            width: size.0 as f32,
            height: size.1 as f32,
        },
        pw,
        ph,
        scale,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("byard shot target"),
        size: wgpu::Extent3d {
            width: pw,
            height: ph,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let cmd = enc
        .encode_frame_from_relay(&target, frame)
        .map_err(|e| e.to_string())?;
    queue.submit(std::iter::once(cmd));

    Ok(read_back(&device, &queue, &target, pw, ph))
}

/// Copies `target` out of the GPU as tightly packed RGBA8 rows.
fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    pw: u32,
    ph: u32,
) -> Vec<u8> {
    let bpr = 256 * (pw * 4).div_ceil(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("byard shot readback"),
        size: u64::from(bpr) * u64::from(ph),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut ce = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    ce.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(ph),
            },
        },
        wgpu::Extent3d {
            width: pw,
            height: ph,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(ce.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let row = (pw * 4) as usize;
    let mut out = vec![0u8; row * ph as usize];
    for y in 0..ph as usize {
        let src = y * bpr as usize;
        out[y * row..(y + 1) * row].copy_from_slice(&data[src..src + row]);
    }
    out
}
