//! A package-authored widget that draws itself (RFC-0039).
//!
//! Run it from the repository root:
//!
//! ```text
//! cargo run -p sparkline-view
//! ```
//!
//! The bars are not `Box` elements. They are one native view emitting its own
//! instances into the frame, through the same pipeline and the same arena the
//! interpreter's own boxes go through, which is the whole claim of the
//! extension ABI: a widget too specific for core does not have to be slower
//! than one inside it.
//!
//! What to look at:
//!
//! - The chart is written in `byld` as `Sparkline(data: …, bar: …)`. Nothing at
//!   that call site says "package": it takes `width`/`height`/`bg` like any
//!   element, its props are checked with spans, and a typo in one is a compile
//!   error rather than a silently ignored attribute.
//! - Press **add** and the series grows. The prop is re-evaluated and handed to
//!   the view every tick, so the widget reacts without owning a single signal.
//! - Hover a bar: the view hit-tests the pointer itself, in its own local
//!   coordinates, and lights that bar. The event never becomes a `byld`
//!   handler; the widget handles it.
//! - The dashed line is a second native view, and it does not compute the
//!   average itself: it asks a Rust controller and draws the answer when it
//!   arrives (RFC-0039 §"Async across the boundary"). Press **add** and watch
//!   it catch up a moment later. Nothing blocked while it did.

use byard::bridge::HostValue;
use byard::render::{Handled, Layout, Measure, NativeView, RenderCtx, RequestKey};

/// Bars drawn from a series of numbers, the smallest widget worth writing in
/// Rust rather than composing.
///
/// It is deliberately something you *could* build out of `Box` elements, so
/// that the interesting difference is visible: this one is a single element to
/// the language, a single batch to the encoder, and it grows by one instance
/// per data point rather than by one element per data point.
#[byard::native_view(name = "Sparkline")]
#[derive(Default)]
struct Sparkline {
    /// The series, in whatever units the caller likes; the view scales to fit.
    #[prop]
    data: Vec<f32>,
    /// Bar colour.
    #[prop]
    bar: u32,
    /// Colour of the bar under the pointer.
    #[prop]
    highlight: u32,
    /// Gap between bars, in logical pixels. Marked `layout` because it is the
    /// one prop that could change how much room the widget wants.
    #[prop(layout)]
    gap: f32,
    /// Which bar the pointer is over, if any. Not a prop: it is the view's own
    /// state, which is exactly the kind of thing a native view exists to keep.
    hovered: Option<usize>,
    /// Whether the hover moved since the last frame, so `render` can ask for
    /// the frame that shows it.
    repaint: bool,
}

impl Sparkline {
    /// The rectangle of bar `i`, in absolute logical pixels.
    fn bar_rect(&self, layout: Layout, i: usize, peak: f32) -> [f32; 4] {
        let count = self.data.len().max(1);
        #[allow(clippy::cast_precision_loss)]
        let slot = layout.width() / count as f32;
        let gap = self.gap.max(0.0).min(slot * 0.5);
        let height = (self.data[i] / peak) * layout.height();
        #[allow(clippy::cast_precision_loss)]
        let x = layout.rect[0] + slot * i as f32;
        [
            x + gap * 0.5,
            layout.rect[1] + layout.height() - height,
            (slot - gap).max(1.0),
            height.max(1.0),
        ]
    }

    /// The largest value, which is what the bars are scaled against. Never
    /// zero, so a series of zeroes draws flat rather than dividing by nothing.
    fn peak(&self) -> f32 {
        self.data
            .iter()
            .copied()
            .fold(1.0_f32, |peak, v| peak.max(v.abs()))
    }
}

impl NativeView for Sparkline {
    /// Fills whatever the layout gives it, which is what a chart wants: the
    /// card decides how much room the data gets, not the data.
    fn measure(&self, known: Measure) -> Measure {
        known.keep()
    }

    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
        if std::mem::take(&mut self.repaint) {
            cx.request_repaint();
        }
        if self.data.is_empty() {
            return;
        }
        let peak = self.peak();
        let pipeline = cx.pipeline::<byard::render::SolidBoxPipeline>();
        // One batch for the whole series. A hundred bars is a hundred
        // instances and one draw, not a hundred elements.
        let bars: Vec<byard::render::BoxInstance> = (0..self.data.len())
            .map(|i| byard::render::BoxInstance {
                rect: self.bar_rect(layout, i, peak),
                color: byard::render::rgba(if self.hovered == Some(i) {
                    self.highlight
                } else {
                    self.bar
                }),
                radii: [2.0; 4],
                transform: byard::render::Transform::IDENTITY,
                smooth: 0.0,
            })
            .collect();
        cx.emit(pipeline, &bars);
    }

    /// Hit-tests the pointer against the bars, in the view's own coordinates.
    ///
    /// Always returns [`Handled::No`]: lighting a bar is a hover effect, not a
    /// claim on the event, and consuming it would stop anything behind the
    /// chart from seeing the pointer at all. A widget that *acts* on a click
    /// says `Yes` and the event stops there.
    fn on_event(&mut self, event: &byard::render::Event, layout: Layout) -> Handled {
        use byard::render::EventKind;
        let was = self.hovered;
        self.hovered = match event.kind {
            EventKind::PointerMove | EventKind::Hover => {
                let count = self.data.len().max(1);
                #[allow(clippy::cast_precision_loss)]
                let slot = layout.width() / count as f32;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                let index = (event.local.0 / slot.max(1.0)) as usize;
                (index < self.data.len()).then_some(index)
            }
            EventKind::PointerExit => None,
            _ => self.hovered,
        };
        // The hover lives in this view, where the engine cannot see it, which
        // is exactly the case `request_repaint` exists for: the next `render`
        // asks for the frame that shows it.
        self.repaint = was != self.hovered;
        Handled::No
    }
}

/// The app: its views, its shader and its controller.
fn app() -> byard::App {
    byard::App::new(env!("CARGO_MANIFEST_DIR"))
        .title("Sparkline, a native view")
        .size(760, 560)
        .shader::<GlowShader>()
        .native_view::<Glow>()
        .native_view::<Sparkline>()
        .native_view::<MeanLine>()
        .provide(Stats)
}

fn main() -> Result<(), byard::ByardError> {
    // `cargo run -p sparkline-view -- --shot out.png` draws the app to a PNG
    // instead of opening it, after a second of running.
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("--shot"), Some(out)) => app().shot(out, &byard::Shot::after(1000)),
        _ => app().run(),
    }
}

/// The Rust half of the average: an ordinary controller (RFC-0028), which is
/// the *only* way a native view reaches anything asynchronous.
///
/// It sleeps for a moment on purpose. The widget stays interactive while it
/// waits, because waiting is not something the widget does: it asked, it
/// returned, and the frame shipped.
#[byard::byard_controller]
#[derive(Clone)]
struct Stats;

#[byard::byard_controller]
impl Stats {
    /// The mean of a series, computed somewhere that is not the logic thread.
    ///
    /// The sleep is a stand-in for the network round trip a real one would be,
    /// and it is a `tokio` sleep because this runs on the pool: blocking the
    /// pool's worker would be a smaller version of the same mistake blocking
    /// the logic thread is.
    async fn mean(&self, values: Vec<f64>) -> HostValue {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let sum: f64 = values.iter().sum();
        #[allow(clippy::cast_precision_loss)]
        let mean = if values.is_empty() {
            0.0
        } else {
            sum / values.len() as f64
        };
        HostValue::Float(mean)
    }
}

/// A dashed line at the series' mean, drawn where a controller says it is.
///
/// The interesting part is what it does *not* do: it never computes the mean,
/// never touches a thread, and never waits. It notices its data changed, asks,
/// and draws the last answer it was given until a better one arrives.
#[byard::native_view(name = "MeanLine")]
#[derive(Default)]
struct MeanLine {
    /// The series to average. The same list the chart draws, so the two are
    /// looking at the same numbers by construction.
    #[prop]
    of: Vec<f32>,
    /// Line colour.
    #[prop]
    stroke: u32,
    /// The mean the controller last answered with, and the request that is in
    /// flight, if any.
    mean: Option<f32>,
    /// A fingerprint of the series the last request was for. The length alone
    /// is not enough: a live series keeps its length while every value moves.
    asked_for: Option<u64>,
    requests: u64,
}

impl MeanLine {
    /// Whether the series changed since the last request, recording it as asked
    /// for when it did.
    fn should_ask(&mut self) -> bool {
        use std::hash::{Hash, Hasher};
        if self.of.is_empty() {
            return false;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for v in &self.of {
            v.to_bits().hash(&mut hasher);
        }
        let fingerprint = hasher.finish();
        if self.asked_for == Some(fingerprint) {
            return false;
        }
        self.asked_for = Some(fingerprint);
        true
    }
}

impl NativeView for MeanLine {
    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
        // Ask when the data changed, and only then: a request per frame would
        // be a widget hammering a controller for an answer it already has.
        if self.should_ask() {
            self.requests += 1;
            cx.call(
                RequestKey(self.requests),
                "Stats",
                "mean",
                vec![HostValue::List(
                    self.of
                        .iter()
                        .map(|v| HostValue::Float(f64::from(*v)))
                        .collect(),
                )],
            );
        }

        let Some(mean) = self.mean else { return };
        let peak = self.of.iter().copied().fold(1.0_f32, |p, v| p.max(v.abs()));
        let y = layout.rect[1] + layout.height() * (1.0 - mean / peak);

        // A dashed line, as a run of short quads: one batch, however many
        // dashes it takes.
        let pipeline = cx.pipeline::<byard::render::SolidBoxPipeline>();
        let dash = 10.0_f32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = (layout.width() / (dash * 2.0)) as usize;
        let dashes: Vec<byard::render::BoxInstance> = (0..count)
            .map(|i| byard::render::BoxInstance {
                #[allow(clippy::cast_precision_loss)]
                rect: [layout.rect[0] + i as f32 * dash * 2.0, y - 1.0, dash, 2.0],
                color: byard::render::rgba(self.stroke),
                radii: [1.0; 4],
                transform: byard::render::Transform::IDENTITY,
                smooth: 0.0,
            })
            .collect();
        cx.emit(pipeline, &dashes);
    }

    /// The answer, on the logic thread, keyed by the request that asked.
    fn on_result(&mut self, _key: RequestKey, value: &HostValue) {
        if let HostValue::Float(mean) = value {
            #[allow(clippy::cast_possible_truncation)]
            {
                self.mean = Some(*mean as f32);
            }
        }
    }
}

/// What the glow's shader draws per instance: where, in which colour, and how
/// far along its drift it is.
#[repr(C)]
#[derive(Clone, Copy, byard::render::bytemuck::Pod, byard::render::bytemuck::Zeroable)]
#[bytemuck(crate = "byard::render::bytemuck")]
struct GlowInstance {
    rect: [f32; 4],
    tint: [f32; 4],
    phase: f32,
}

/// A shader of the app's own: three soft bands of light drifting across a
/// rectangle.
///
/// This is the other half of the extension ABI. A view that only needs boxes
/// emits into a core pipeline, as the two above do; one that needs pixels no
/// core pipeline draws describes its own. It names the instance record, lists
/// its fields, and writes the WGSL. The engine builds the pipeline, and the
/// app never holds a GPU object.
struct GlowShader;

impl byard::render::Shader for GlowShader {
    const NAME: &'static str = "sparkline_glow";
    type Instance = GlowInstance;

    fn lanes() -> &'static [byard::render::Lane] {
        use byard::render::Lane;
        &[Lane::Vec4, Lane::Vec4, Lane::F32]
    }

    fn wgsl() -> &'static str {
        "
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
    @location(2) phase: f32,
};

@vertex
fn vs_main(
    @location(0) quad: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(2) tint: vec4<f32>,
    @location(3) phase: f32,
    @location(15) depth: f32,
) -> Out {
    var out: Out;
    out.position = to_clip(rect.xy + quad * rect.zw, depth);
    out.uv = quad;
    out.tint = tint;
    out.phase = phase;
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    // Three bands, each a sine across the width at its own pace, brightest
    // along its own wavering line and fading towards the edges of the card.
    var light = 0.0;
    for (var i = 0; i < 3; i++) {
        let k = f32(i);
        let line = 0.5 + 0.22 * sin(in.uv.x * (3.0 + k * 1.7) + in.phase * (0.6 + k * 0.35) + k * 2.1);
        let d = abs(in.uv.y - line);
        light += 0.012 / (d * d + 0.012);
    }
    let edge = smoothstep(0.0, 0.25, in.uv.x) * smoothstep(1.0, 0.75, in.uv.x);
    let alpha = clamp(light * 0.16, 0.0, 0.6) * edge;
    return vec4<f32>(in.tint.rgb, alpha * in.tint.a);
}
"
    }
}

/// The glow behind the chart: one instance of [`GlowShader`], redrawn every
/// frame with the time.
#[byard::native_view(name = "Glow")]
#[derive(Default)]
struct Glow {
    /// The colour of the light.
    #[prop]
    tint: u32,
    /// When the view first drew, which is where its drift starts.
    started: Option<std::time::Instant>,
}

impl NativeView for Glow {
    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
        let started = *self.started.get_or_insert_with(std::time::Instant::now);
        let shader = cx.shader::<GlowShader>();
        cx.emit(
            shader,
            &[GlowInstance {
                rect: layout.rect,
                tint: byard::render::rgba(self.tint),
                phase: started.elapsed().as_secs_f32(),
            }],
        );
        // It moves, so it asks for the next frame.
        cx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::MeanLine;

    #[test]
    fn the_mean_is_asked_for_again_when_a_value_moves() {
        let mut line = MeanLine {
            of: vec![1.0, 2.0, 3.0],
            ..MeanLine::default()
        };
        assert!(line.should_ask(), "the first series is asked for");
        assert!(!line.should_ask(), "the same series is not asked twice");
        line.of[1] = 5.0;
        assert!(line.should_ask(), "same length, new values: ask again");
        line.of.clear();
        assert!(!line.should_ask(), "nothing to average");
    }

    /// The whole app, Rust half included, drawn without a window: the glow's
    /// shader lights the card behind the bars, and the mean line is there,
    /// which means the controller was really asked and really answered.
    #[test]
    fn the_app_draws_its_shader_and_its_answer() {
        let out = std::env::temp_dir().join(format!("sparkline-{}.png", std::process::id()));
        match super::app().shot(&out, &byard::Shot::after(600)) {
            Ok(()) => {}
            Err(e) if e.to_string().contains("no GPU") => {
                eprintln!("no GPU adapter, skipping: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        }
        let image = image::open(&out).expect("the PNG").to_rgba8();
        let _ = std::fs::remove_file(&out);
        assert_eq!(
            image.dimensions(),
            (1520, 1120),
            "760x560 at two pixels each"
        );

        // Between two bars, inside the card: the card's own colour is
        // (28, 28, 38), and the glow adds blue to it.
        let lit = image.get_pixel(590, 400).0;
        assert!(
            lit[2] > 60 && lit[2] > lit[0] + 20,
            "the glow lights the card: {lit:?}"
        );
        // The dashed mean line is the one yellow thing on screen.
        let yellow = image
            .pixels()
            .filter(|p| p.0[0] > 220 && p.0[1] > 180 && p.0[2] < 160)
            .count();
        assert!(
            yellow > 500,
            "the mean line arrived: {yellow} yellow pixels"
        );
    }
}
