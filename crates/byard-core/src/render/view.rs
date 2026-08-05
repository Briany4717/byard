//! [`NativeView`], the trait a package's custom-drawn widget implements
//! (RFC-0039 §"The `NativeView` trait").
//!
//! # Four responsibilities, and not one subsystem more
//!
//! Layout, draw, events, mount and unmount. Each maps onto machinery the
//! engine already runs for its own intrinsics: `measure` is called where an
//! intrinsic's measurement is called, `render` where an intrinsic's lowering
//! fills a pool, `on_event` under the same hit-testing and z-layer rules
//! (RFC-0003, RFC-0017). Nothing here is a new subsystem, which is why a
//! native view can be as fast as an intrinsic rather than merely close.
//!
//! Async is deliberately not a fifth responsibility. A native view does no
//! I/O: it calls a controller (RFC-0028) and the result comes back through
//! [`NativeView::on_result`] on the logic thread, so no graphics state ever
//! goes near another thread (INV-12).
//!
//! # Where a view's state lives
//!
//! In the view. The engine owns the boxed view for exactly as long as the
//! element that declared it is mounted, and drops it in the same linear pass
//! that releases the rest of that element (INV-31). There is no separate
//! lifetime for extension state, and no way to ask for one: a view that seems
//! to need a resource outliving its own mount is describing a cache the app
//! should own, not a gap in this trait.

use super::ctx::RenderCtx;
use crate::bridge::HostValue;

/// What layout knows about a view's box, and what the view answers with.
///
/// The same type in both directions on purpose. As an input, `Some` is a
/// constraint layout has already decided and `None` an axis still free. As an
/// output, `Some` is the size the view wants and `None` means "whatever the
/// constraint turns out to be", which is what filling means.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measure {
    /// Logical pixels across, or `None` for unconstrained/fill.
    pub width: Option<f32>,
    /// Logical pixels down, or `None` for unconstrained/fill.
    pub height: Option<f32>,
}

impl Measure {
    /// A view that wants exactly this size on both axes.
    #[must_use]
    pub const fn exact(width: f32, height: f32) -> Self {
        Self {
            width: Some(width),
            height: Some(height),
        }
    }

    /// A view that takes whatever it is given on both axes.
    #[must_use]
    pub const fn fill() -> Self {
        Self {
            width: None,
            height: None,
        }
    }

    /// The constraints, unchanged: the answer of a view that is happy with
    /// what layout already decided.
    #[must_use]
    pub const fn keep(self) -> Self {
        self
    }

    /// This measurement with any free axis resolved to `fallback`.
    ///
    /// What the engine calls to turn an answer into a leaf size, so a view
    /// that filled and a view that was exact reach layout as the same kind of
    /// value.
    #[must_use]
    pub fn or(self, fallback: (f32, f32)) -> (f32, f32) {
        (
            self.width.unwrap_or(fallback.0),
            self.height.unwrap_or(fallback.1),
        )
    }
}

/// The box layout gave a view, in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Layout {
    /// `[x, y, width, height]` in the frame's logical-pixel space, which is
    /// the space every other primitive is in. The encoder scales to physical
    /// pixels once, for everything, so a view never multiplies by a DPI
    /// factor and never has to know one.
    pub rect: [f32; 4],
}

impl Layout {
    /// A layout box at `[x, y, w, h]`.
    #[must_use]
    pub const fn new(rect: [f32; 4]) -> Self {
        Self { rect }
    }

    /// Whether `point` (logical pixels, absolute) is inside this box.
    #[must_use]
    pub fn contains(&self, point: (f32, f32)) -> bool {
        let [x, y, w, h] = self.rect;
        point.0 >= x && point.0 < x + w && point.1 >= y && point.1 < y + h
    }

    /// `point` relative to this box's top-left corner.
    #[must_use]
    pub const fn local(&self, point: (f32, f32)) -> (f32, f32) {
        (point.0 - self.rect[0], point.1 - self.rect[1])
    }

    /// Width in logical pixels.
    #[must_use]
    pub const fn width(&self) -> f32 {
        self.rect[2]
    }

    /// Height in logical pixels.
    #[must_use]
    pub const fn height(&self) -> f32 {
        self.rect[3]
    }
}

/// An input event routed to a view (RFC-0003).
///
/// A narrowed form of the engine's own event, carrying what a leaf widget can
/// act on and nothing it cannot: the position is already in the view's local
/// space, so a view never subtracts its own origin and can never forget to.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// What happened.
    pub kind: crate::platform::EventKind,
    /// Cursor position relative to the view's top-left corner, in logical
    /// pixels.
    pub local: (f32, f32),
    /// Incremental delta, for a scroll or a move.
    pub delta: (f32, f32),
    /// The event's payload, for keys and text.
    pub payload: Option<crate::platform::InputPayload>,
}

/// Whether a view consumed an event, or whether it should keep routing
/// (RFC-0003).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// The view acted on it; routing stops here.
    Yes,
    /// The view did not; the event carries on to whatever is behind.
    No,
}

impl Handled {
    /// Whether this is [`Handled::Yes`].
    #[must_use]
    pub fn is_handled(self) -> bool {
        self == Self::Yes
    }
}

/// The identity of one outstanding controller request a view made.
///
/// A view chooses the key, so it can key by tile coordinate, by row, by
/// anything it will recognise when the answer arrives (RFC-0039 §"Async
/// across the boundary"). The engine only carries it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestKey(pub u64);

/// How a view receives the props its element declared (RFC-0039).
///
/// A trait of its own, and a supertrait of [`NativeView`], because this is the
/// half `#[byard::native_view]` writes and the rest is the half the author
/// writes. Splitting them means the generated code never has to reach into a
/// hand-written `impl` block, which Rust would not allow anyway, and a view
/// with no props says so in one empty line:
///
/// ```
/// # use byard_core::render::{Layout, NativeProps, NativeView, RenderCtx};
/// struct Divider;
/// impl NativeProps for Divider {}
/// impl NativeView for Divider {
///     fn render(&mut self, _layout: Layout, _cx: &mut RenderCtx<'_>) {}
/// }
/// ```
pub trait NativeProps {
    /// Receives one declared prop, re-evaluated this frame.
    ///
    /// Called before [`NativeView::render`], once per prop the element
    /// actually wrote, with the value the expression evaluated to *this tick*.
    /// That is what makes a native view's props reactive and animatable with
    /// no plumbing of its own: they arrive through the same evaluation the
    /// language runs for an intrinsic's, and the view never learns whether the
    /// number behind one was a literal, a signal, or a running animation.
    fn set_prop(&mut self, name: &str, value: &HostValue) {
        let _ = (name, value);
    }
}

/// A package-authored widget that lays out, draws, and handles events like an
/// intrinsic (RFC-0039).
///
/// Every method has a default except [`render`](NativeView::render), because
/// drawing is the one thing a view exists to do and the rest are things it may
/// have an opinion about.
pub trait NativeView: NativeProps + 'static {
    /// The view's own size, given what layout has already decided.
    ///
    /// Called where an intrinsic's measurement is called, in the same layout
    /// pass, so a native view participates in flex and grid exactly as any
    /// other leaf does. The default fills its constraints, which is what a
    /// chart or a map wants.
    fn measure(&self, known: Measure) -> Measure {
        known.keep()
    }

    /// Draws the view into the frame, given the box layout resolved for it.
    ///
    /// The only required method. Everything emitted here reaches the GPU by
    /// the same path a core intrinsic's instances do (INV-30).
    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>);

    /// Handles one routed input event.
    ///
    /// The default declines everything, so a purely visual view is not
    /// obliged to say so. Declining routes the event on to whatever is behind
    /// the view, exactly as an intrinsic without a listener does.
    fn on_event(&mut self, event: &Event, layout: Layout) -> Handled {
        let _ = (event, layout);
        Handled::No
    }

    /// Called once when the element that declares this view is mounted.
    fn on_mount(&mut self) {}

    /// Called once when that element is unmounted, before the view is
    /// dropped.
    ///
    /// For symmetry and for a view that wants to notice; it is **not** where
    /// memory is released. The view is dropped either way, in the single
    /// linear pass that releases the element around it, and a view that
    /// forgets to implement this leaks nothing (INV-31).
    fn on_unmount(&mut self) {}

    /// Delivers the result of a controller request this view issued.
    ///
    /// On the logic thread, keyed by whatever the view asked with. A view that
    /// has already forgotten the key ignores it; a view that unmounted before
    /// the answer arrived never sees it at all, because the engine drops a
    /// result whose owner is gone rather than looking for somewhere to put it.
    fn on_result(&mut self, key: RequestKey, value: &HostValue) {
        let _ = (key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::EventKind;
    use crate::render::batch::NativeBatches;

    struct Quad {
        colour: [f32; 4],
        taps: u32,
        mounted: bool,
    }

    impl NativeProps for Quad {}

    impl NativeView for Quad {
        fn measure(&self, known: Measure) -> Measure {
            known.keep()
        }

        fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
            let handle = cx.pipeline::<crate::encoder::SolidBoxPipeline>();
            cx.emit(
                handle,
                &[crate::frame::BoxInstance {
                    rect: layout.rect,
                    color: self.colour,
                    radii: [0.0; 4],
                    transform: crate::frame::Transform::IDENTITY,
                    smooth: 0.0,
                }],
            );
        }

        fn on_event(&mut self, event: &Event, layout: Layout) -> Handled {
            if event.kind == EventKind::Tap
                && layout.contains((
                    layout.rect[0] + event.local.0,
                    layout.rect[1] + event.local.1,
                ))
            {
                self.taps += 1;
                Handled::Yes
            } else {
                Handled::No
            }
        }

        fn on_mount(&mut self) {
            self.mounted = true;
        }

        fn on_unmount(&mut self) {
            self.mounted = false;
        }
    }

    fn quad() -> Quad {
        Quad {
            colour: [0.0, 1.0, 0.0, 1.0],
            taps: 0,
            mounted: false,
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "exact geometry: these are the numbers that were written, not a computation"
    )]
    fn a_view_fills_the_rect_it_was_laid_out_in() {
        let mut view = quad();
        let mut pool = NativeBatches::new();
        let mut textures = Vec::new();
        let mut cx = RenderCtx::new(&mut pool, &mut textures, 0.25);
        let layout = Layout::new([10.0, 20.0, 100.0, 50.0]);
        view.render(layout, &mut cx);

        let batch = &pool.batches()[0];
        let instances: &[crate::frame::BoxInstance] = bytemuck::cast_slice(&batch.bytes);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].rect, [10.0, 20.0, 100.0, 50.0]);
    }

    #[test]
    fn a_measurement_that_fills_takes_the_constraint_it_was_given() {
        let view = quad();
        let known = Measure {
            width: Some(320.0),
            height: Some(120.0),
        };
        assert_eq!(view.measure(known), known);
        assert_eq!(Measure::fill().or((320.0, 120.0)), (320.0, 120.0));
        assert_eq!(Measure::exact(40.0, 40.0).or((320.0, 120.0)), (40.0, 40.0));
    }

    #[test]
    fn a_hit_is_handled_and_a_miss_routes_on() {
        let mut view = quad();
        let layout = Layout::new([0.0, 0.0, 50.0, 50.0]);
        let hit = Event {
            kind: EventKind::Tap,
            local: (25.0, 25.0),
            delta: (0.0, 0.0),
            payload: None,
        };
        assert_eq!(view.on_event(&hit, layout), Handled::Yes);
        assert_eq!(view.taps, 1);

        let miss = Event {
            local: (75.0, 25.0),
            ..hit
        };
        assert_eq!(
            view.on_event(&miss, layout),
            Handled::No,
            "a miss must route on, or everything behind a view becomes dead"
        );
        assert_eq!(view.taps, 1);
    }

    #[test]
    fn mount_and_unmount_bracket_a_views_life() {
        let mut view = quad();
        assert!(!view.mounted);
        view.on_mount();
        assert!(view.mounted);
        view.on_unmount();
        assert!(!view.mounted);
    }

    #[test]
    fn the_default_methods_are_the_ones_a_drawing_only_view_wants() {
        struct JustDraws;
        impl NativeProps for JustDraws {}
        impl NativeView for JustDraws {
            fn render(&mut self, _layout: Layout, _cx: &mut RenderCtx<'_>) {}
        }
        let mut view = JustDraws;
        let layout = Layout::new([0.0, 0.0, 10.0, 10.0]);
        assert_eq!(
            view.on_event(
                &Event {
                    kind: EventKind::Tap,
                    local: (1.0, 1.0),
                    delta: (0.0, 0.0),
                    payload: None,
                },
                layout
            ),
            Handled::No
        );
        assert_eq!(
            view.measure(Measure::exact(4.0, 4.0)),
            Measure::exact(4.0, 4.0)
        );
        view.on_result(RequestKey(7), &HostValue::Unit);
    }
}
