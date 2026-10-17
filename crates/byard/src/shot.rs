//! [`App::shot`]: the app, rendered to a PNG without a window.
//!
//! The same program [`App::run`] opens, with the same controllers, native
//! views and shaders, driven for a stretch of real time and drawn into an
//! offscreen texture. Controller calls are really made and their answers
//! really applied, so a screen that loads data shows the data.
//!
//! It exists because looking at the picture is the only check that catches a
//! wrong picture, and an app with a Rust half cannot be drawn by `byard shot`,
//! which knows nothing of that half.

use std::path::Path;
use std::sync::Arc;

use byard_compiler::interp::eval::Interpreter;
use byard_core::ByardError;
use byard_core::bridge::Dispatcher;
use byard_core::encoder::EncoderSubsystem;
use byard_core::frame::{RenderFrame, Viewport};
use byard_core::platform::{EventKind, InputEvent};
use byard_core::relay::Relay;

use crate::app::{App, Program, load_program};

/// One frame's worth of real time between steps.
const STEP: std::time::Duration = std::time::Duration::from_millis(16);

/// What [`App::shot`] does before it takes the picture.
#[derive(Clone, Debug, Default)]
pub struct Shot {
    run_ms: u32,
    /// The gestures, in order: each step is one frame's input.
    gestures: Vec<Vec<InputEvent>>,
    system_dark: Option<bool>,
    trace: Option<std::path::PathBuf>,
}

impl Shot {
    /// Runs the app for `run_ms` milliseconds, then takes the picture.
    #[must_use]
    pub const fn after(run_ms: u32) -> Self {
        Self {
            run_ms,
            gestures: Vec::new(),
            system_dark: None,
            trace: None,
        }
    }

    /// Writes a trace of every frame of the run to `path`, as
    /// [`App::trace`] does for a window, GPU passes included where the GPU
    /// can time them.
    #[must_use]
    pub fn trace(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.trace = Some(path.into());
        self
    }

    /// Lets `ms` milliseconds of the app run before the next gesture, so one
    /// can wait for what an earlier one started: a screen's data, a map's
    /// tiles.
    #[must_use]
    pub fn wait(mut self, ms: u32) -> Self {
        let frames = ms.div_ceil(16);
        self.gestures.extend((0..frames).map(|_| Vec::new()));
        self
    }

    /// Drags from `from` to `to` over `frames` frames, after any gesture
    /// before it: a press, one move per frame, a release. A shot can pan or
    /// scroll to the state it wants, and a trace of it measures the frames
    /// a drag costs.
    #[must_use]
    pub fn drag(mut self, from: (f32, f32), to: (f32, f32), frames: u32) -> Self {
        let event = |kind, pos: (f32, f32), delta| InputEvent {
            kind,
            pos,
            delta,
            payload: None,
            time_ms: 0,
        };
        let steps = frames.max(1);
        #[allow(clippy::cast_precision_loss)]
        let step = (
            (to.0 - from.0) / steps as f32,
            (to.1 - from.1) / steps as f32,
        );
        self.gestures
            .push(vec![event(EventKind::PointerDown, from, (0.0, 0.0))]);
        for i in 1..=steps {
            #[allow(clippy::cast_precision_loss)]
            let at = (from.0 + step.0 * i as f32, from.1 + step.1 * i as f32);
            self.gestures
                .push(vec![event(EventKind::PointerMove, at, step)]);
        }
        self.gestures
            .push(vec![event(EventKind::PointerUp, to, (0.0, 0.0))]);
        self
    }

    /// Reports the system's appearance as dark (`true`) or light, as a
    /// window would when it opens, for an app whose theme follows it
    /// (`scheme = "system"`). Without it a shot reports nothing, as a
    /// platform that cannot say would.
    #[must_use]
    pub const fn system_dark(mut self, dark: bool) -> Self {
        self.system_dark = Some(dark);
        self
    }

    /// Taps a logical point first. Taps happen in order, one per frame, at
    /// the start of the run, so a shot can open the screen it wants.
    #[must_use]
    pub fn tap(mut self, x: f32, y: f32) -> Self {
        let at = |kind| InputEvent {
            kind,
            pos: (x, y),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
        };
        self.gestures
            .push(vec![at(EventKind::PointerDown), at(EventKind::PointerUp)]);
        self
    }
}

fn gpu(what: &str, detail: impl std::fmt::Display) -> ByardError {
    ByardError::Platform(format!("{what}: {detail}"))
}

impl App {
    /// Renders the app to a PNG at `out` after the taps and the stretch of
    /// running `plan` describes, at the size given to [`size`](App::size) and
    /// two device pixels per logical pixel.
    ///
    /// # Errors
    ///
    /// Whatever [`run`](App::run) would report before opening its window, and
    /// [`ByardError::Platform`] if there is no GPU to draw with or the file
    /// cannot be written.
    pub fn shot(self, out: impl AsRef<Path>, plan: &Shot) -> Result<(), ByardError> {
        self.check_registrations()?;
        let Program {
            views, theme, http, ..
        } = load_program(&self.entry)?;
        let mut registry = self.registry;
        byard_core::cap::configure_http(&mut registry, http.base_url.as_deref(), &http.hosts);

        let relay = Relay::new()?;
        let dispatcher = Dispatcher::new(registry, relay.io_handle(), relay.io_result_sender());
        let mut interp = Interpreter::new();
        interp.set_dispatcher(dispatcher);
        interp.set_theme(theme);
        interp.load_views(&views);
        let known: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        let root = byard_compiler::parser::ast::root_view(&views)
            .ok_or_else(|| ByardError::Platform("the program declares no `View`".to_string()))?;
        let tree = interp.lower_view(root, &known);
        if !interp.errors().is_empty() {
            let report: Vec<String> = interp
                .errors()
                .iter()
                .map(|e| format!("  {}", e.headline()))
                .collect();
            return Err(ByardError::Platform(format!(
                "`{}` did not compile:\n{}",
                self.entry.display(),
                report.join("\n")
            )));
        }

        let scale = 2.0_f32;
        let (width, height) = self.size;
        #[allow(clippy::cast_precision_loss)]
        let (w, h) = (width as f32, height as f32);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (pw, ph) = ((w * scale).round() as u32, (h * scale).round() as u32);
        let mut target = Target::new(pw, ph, scale, (w, h))?;

        if let Some(dark) = plan.system_dark {
            interp.dispatch_events(&[InputEvent {
                kind: EventKind::SystemScheme,
                pos: (0.0, 0.0),
                delta: (0.0, 0.0),
                payload: Some(byard_core::platform::InputPayload::Bool(dark)),
                time_ms: 0,
            }]);
        }
        let mut trace = plan
            .trace
            .as_deref()
            .map(byard_core::telemetry::trace::TraceWriter::create)
            .transpose()
            .map_err(ByardError::Platform)?;
        let mut now = 0_u32;
        let mut gestures = plan.gestures.iter();
        loop {
            let mut replies = Vec::new();
            while let Some(reply) = relay.try_recv_io_result() {
                replies.push(reply);
            }
            if !replies.is_empty() {
                let _ = interp.apply_io_results(replies);
            }
            interp.set_now_ms(now);
            // The first frame draws the app as it opens; each one after it
            // delivers the next step of a gesture, to whatever that frame put
            // there.
            if now > 0 {
                if let Some(step) = gestures.next() {
                    let events: Vec<InputEvent> = step
                        .iter()
                        .map(|e| InputEvent {
                            time_ms: u64::from(now),
                            ..e.clone()
                        })
                        .collect();
                    interp.dispatch_events(&events);
                }
            }
            interp.tick();
            let mut frame = RenderFrame::new();
            frame.request_full_redraw();
            interp.render(&tree, &mut frame, w, h);
            // Every frame is drawn, not only the last: a layer a view wrote on
            // an earlier frame has to reach the texture it lives in.
            target.draw(&frame)?;
            if let Some(trace) = trace.as_mut() {
                // One thread does the logic and the drawing here, so its
                // scopes are one lane.
                trace.write_frame(None, &byard_core::telemetry::drain_samples());
            }
            if now >= plan.run_ms && gestures.len() == 0 {
                break;
            }
            now += 16;
            std::thread::sleep(STEP);
        }

        let pixels = target.read();
        image::save_buffer(out.as_ref(), &pixels, pw, ph, image::ColorType::Rgba8)
            .map_err(|e| gpu(&out.as_ref().display().to_string(), e))
    }
}

/// The offscreen texture a shot is drawn into, and the encoder that draws it.
struct Target {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    encoder: EncoderSubsystem,
    texture: wgpu::Texture,
    size: (u32, u32),
}

impl Target {
    fn new(pw: u32, ph: u32, scale: f32, logical: (f32, f32)) -> Result<Self, ByardError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .map_err(|e| gpu("no GPU adapter", e))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("byard shot"),
            // Asked for where the GPU has it, so a traced shot times its
            // passes; nothing else depends on it.
            required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
            required_limits: byard_core::engine::device_limits(&adapter),
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        }))
        .map_err(|e| gpu("no GPU device", e))?;
        let (device, queue) = (Arc::new(device), Arc::new(queue));
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut encoder = pollster::block_on(EncoderSubsystem::init(
            Arc::clone(&device),
            Arc::clone(&queue),
            format,
            scale,
            pw,
            ph,
        ))?;
        encoder.update_viewport(
            Viewport {
                width: logical.0,
                height: logical.1,
            },
            pw,
            ph,
            scale,
        );
        let texture = device.create_texture(&wgpu::TextureDescriptor {
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
        Ok(Self {
            device,
            queue,
            encoder,
            texture,
            size: (pw, ph),
        })
    }

    fn draw(&mut self, frame: &RenderFrame) -> Result<(), ByardError> {
        let commands = self.encoder.encode_frame_from_relay(&self.texture, frame)?;
        // Through the encoder, which also asks for the frame's GPU timings.
        self.encoder.submit(commands);
        Ok(())
    }

    /// The picture, as tightly packed opaque RGBA8 rows.
    fn read(&self) -> Vec<u8> {
        let (pw, ph) = self.size;
        let bpr = 256 * (pw * 4).div_ceil(256);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("byard shot readback"),
            size: u64::from(bpr) * u64::from(ph),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut copy = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        copy.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
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
        self.queue.submit(std::iter::once(copy.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range();
        let row = (pw * 4) as usize;
        let mut out = vec![0u8; row * ph as usize];
        for y in 0..ph as usize {
            let src = y * bpr as usize;
            out[y * row..(y + 1) * row].copy_from_slice(&data[src..src + row]);
        }
        // A window's surface is opaque: where the app drew nothing it shows
        // black. The colour is premultiplied, so only the alpha changes.
        for alpha in out.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drag is a press, one move a frame, a release, ending where it was
    /// told to; a wait is that many frames of nothing.
    #[test]
    fn a_drag_and_a_wait_are_frames_of_input() {
        let plan = Shot::after(0).wait(40).drag((0.0, 0.0), (30.0, -12.0), 3);
        let kinds: Vec<Vec<EventKind>> = plan
            .gestures
            .iter()
            .map(|step| step.iter().map(|e| e.kind).collect())
            .collect();
        assert_eq!(
            kinds,
            [
                vec![],
                vec![],
                vec![],
                vec![EventKind::PointerDown],
                vec![EventKind::PointerMove],
                vec![EventKind::PointerMove],
                vec![EventKind::PointerMove],
                vec![EventKind::PointerUp],
            ]
        );
        let moves: Vec<((f32, f32), (f32, f32))> = plan.gestures[4..7]
            .iter()
            .map(|step| (step[0].pos, step[0].delta))
            .collect();
        assert_eq!(
            moves,
            [
                ((10.0, -4.0), (10.0, -4.0)),
                ((20.0, -8.0), (10.0, -4.0)),
                ((30.0, -12.0), (10.0, -4.0)),
            ]
        );
        assert_eq!(plan.gestures[7][0].pos, (30.0, -12.0));
    }

    /// A traced shot writes a frame's scopes for every frame it draws. Skipped
    /// where there is no GPU to draw with.
    #[test]
    fn a_traced_shot_writes_every_frame() {
        let dir = std::env::temp_dir().join(format!("byard-shot-trace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.byd"), "View Main() { Text(\"traced\") }\n").unwrap();
        let (png, json) = (dir.join("shot.png"), dir.join("trace.json"));

        let result = App::new(dir.join("main.byd"))
            .size(120, 80)
            .shot(&png, &Shot::after(64).trace(&json));
        if let Err(ByardError::Platform(message)) = &result {
            if message.starts_with("no GPU") {
                return;
            }
        }
        result.unwrap();
        let text = std::fs::read_to_string(&json).unwrap();
        assert!(
            text.starts_with('[') && text.trim_end().ends_with(']'),
            "{text}"
        );
        // Frames at 0, 16, 32, 48 and 64 ms.
        assert_eq!(text.matches("\"encode.frame\"").count(), 5, "{text}");
        assert_eq!(text.matches("\"interp.render\"").count(), 5, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
