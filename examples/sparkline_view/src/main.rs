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

use byard::render::{Handled, Layout, Measure, NativeView, RenderCtx};

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

fn main() -> Result<(), byard::ByardError> {
    byard::App::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.byd"))
        .title("Sparkline, a native view")
        .size(760, 520)
        .native_view::<Sparkline>()
        .run()
}
