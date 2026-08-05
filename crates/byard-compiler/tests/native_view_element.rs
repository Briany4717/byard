//! RFC-0039: a registered native view is an element like any other.
//!
//! The RFC's central claim is that a package widget is indistinguishable from
//! an intrinsic at the call site, and "indistinguishable" is checkable: the
//! same validation rules, the same prop classes, the same diagnostics with the
//! same spans, and props that arrive re-evaluated every tick because they go
//! through the language's own evaluation rather than around it.

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::interp::intrinsics::{AttrClass, PropType, lookup};
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;
use byard_core::render::{
    Handled, Layout, NativeProp, NativePropType, NativeProps, NativeView, NativeViewInfo,
    NativeViewMeta, RenderCtx,
};

/// What the test view was told, so a test can ask what actually arrived.
///
/// A `static`, because the interpreter owns the view instances and hands out
/// no way to reach into one: this is the test's own back channel, not an API.
static LAST: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);

#[derive(Clone, Debug, Default, PartialEq)]
struct Seen {
    data: Vec<f32>,
    bar: u32,
    label: String,
    rect: [f32; 4],
    renders: u32,
    taps: u32,
}

#[derive(Default)]
struct Chart {
    data: Vec<f32>,
    bar: u32,
    label: String,
    renders: u32,
    taps: u32,
}

impl NativeProps for Chart {
    fn set_prop(&mut self, name: &str, value: &byard_core::bridge::HostValue) {
        use byard_core::bridge::FromHostValue;
        match name {
            "data" => self.data = Vec::<f32>::from_host(value.clone()),
            "bar" => self.bar = u32::from_host(value.clone()),
            "label" => self.label = String::from_host(value.clone()),
            _ => {}
        }
    }
}

impl NativeView for Chart {
    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
        self.renders += 1;
        let pipeline = cx.pipeline::<byard_core::encoder::SolidBoxPipeline>();
        cx.emit(
            pipeline,
            &[byard_core::frame::BoxInstance {
                rect: layout.rect,
                color: byard_core::color::rgba(self.bar),
                radii: [0.0; 4],
                transform: byard_core::frame::Transform::IDENTITY,
                smooth: 0.0,
            }],
        );
        *LAST.lock().unwrap() = Some(Seen {
            data: self.data.clone(),
            bar: self.bar,
            label: self.label.clone(),
            rect: layout.rect,
            renders: self.renders,
            taps: self.taps,
        });
    }

    fn on_event(&mut self, event: &byard_core::render::Event, _layout: Layout) -> Handled {
        if event.kind == byard_core::platform::EventKind::Tap {
            self.taps += 1;
            Handled::Yes
        } else {
            Handled::No
        }
    }
}

impl NativeViewMeta for Chart {
    const INFO: NativeViewInfo = NativeViewInfo {
        name: "TestChart",
        props: &[
            NativeProp {
                name: "data",
                ty: NativePropType::Floats,
                layout: false,
            },
            NativeProp {
                name: "bar",
                ty: NativePropType::Color,
                layout: false,
            },
            NativeProp {
                name: "label",
                ty: NativePropType::Str,
                layout: true,
            },
        ],
        events: &["point_hover"],
    };

    fn create() -> Box<dyn NativeView> {
        Box::new(Self::default())
    }
}

/// Serializes the tests that read [`LAST`].
///
/// The view instances belong to the interpreter, so what a test can see of one
/// is what the view chose to record, and one shared record means one test at a
/// time. Cheap: these are microsecond tests.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Registers the test view once, whichever test gets there first.
fn registered() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        byard_core::render::registry::register::<Chart>();
    });
}

/// Lowers and renders one view, returning the interpreter, its tree and the
/// frame, so a test can look at diagnostics and at pixels-to-be.
fn run(source: &str) -> (Interpreter, Vec<RenderNode>, RenderFrame) {
    registered();
    *LAST.lock().unwrap() = None;
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    (interp, tree, frame)
}

fn seen() -> Seen {
    LAST.lock().unwrap().clone().expect("the view rendered")
}

#[test]
fn a_registered_view_is_looked_up_in_the_same_catalog_an_intrinsic_is() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registered();
    let entry = lookup("TestChart").expect("a registered view resolves like an intrinsic");

    // Its own declared props, with the classes the package declared.
    assert_eq!(entry.property_type("data"), Some(PropType::List));
    assert_eq!(entry.property_class("data"), Some(AttrClass::Paint));
    assert_eq!(entry.property_class("label"), Some(AttrClass::Layout));
    assert!(entry.has_event("point_hover"));

    // And everything an element gets for free, so a package widget can be
    // sized and decorated like anything else.
    assert_eq!(entry.property_type("width"), Some(PropType::Int));
    assert_eq!(entry.property_class("width"), Some(AttrClass::Layout));
    assert_eq!(entry.property_class("bg"), Some(AttrClass::Paint));
    assert!(entry.has_event("tap"));
}

#[test]
fn a_view_is_laid_out_and_drawn_where_the_element_sits() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (interp, _, frame) = run("View Main() { Column #[p: 10] { \
            TestChart #[data: [1.0, 2.0], bar: 0x5B8DEF, width: 120, height: 40] \
        } }");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());

    let seen = seen();
    assert_eq!(seen.renders, 1, "the view drew once for one element");
    assert_eq!(seen.data, vec![1.0, 2.0], "the list prop arrived as data");
    assert_eq!(seen.bar, 0x005B_8DEF);
    assert!(
        (seen.rect[2] - 120.0).abs() < 0.5 && (seen.rect[3] - 40.0).abs() < 0.5,
        "the view was given the box layout resolved for it: {:?}",
        seen.rect
    );
    assert!(
        (seen.rect[0] - 10.0).abs() < 0.5,
        "and it is positioned by its parent's padding, like any element: {:?}",
        seen.rect
    );

    // What it emitted reached the frame's native pool, not a core pool: a
    // native view is drawn by its own batch.
    assert_eq!(frame.native_batches().len(), 1);
    assert_eq!(frame.native_batches()[0].count, 1);
    assert!(
        frame.instances().is_empty(),
        "the view's own instances must not be filed as the interpreter's"
    );
}

#[test]
fn an_unknown_prop_is_a_compile_error_with_a_span_like_an_intrinsics() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (interp, _, _) =
        run("View Main() { TestChart #[data: [1.0], colour: 0xFF0000, width: 10, height: 10] {} }");
    let errors = interp.errors();
    assert!(
        errors
            .iter()
            .any(|e| e.kind() == "UnknownAttribute" || e.kind() == "UnexpectedChildren"),
        "a misspelled prop must be rejected by name: {errors:?}"
    );
    assert!(
        errors.iter().all(|e| e.span().start > 0),
        "every diagnostic points at source: {errors:?}"
    );
}

#[test]
fn a_prop_of_the_wrong_type_is_rejected_before_it_reaches_the_view() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (interp, _, _) = run(
        "View Main() { TestChart #[data: \"not a list\", bar: 0x5B8DEF, width: 10, height: 10] }",
    );
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| e.kind() == "AttributeTypeMismatch"),
        "a string is not a series: {:?}",
        interp.errors()
    );
}

#[test]
fn an_unknown_event_is_rejected_like_an_intrinsics_would_be() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (interp, _, _) =
        run("View Main() { TestChart #[data: [1.0], width: 10, height: 10, point_hovr => {}] }");
    assert!(
        !interp.errors().is_empty(),
        "a misspelled event name is a compile error, not a listener nobody calls"
    );
}

#[test]
fn a_prop_bound_to_a_var_is_re_evaluated_every_tick() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The property that makes a native view reactive without owning a signal:
    // its props go through the same evaluation an intrinsic's do, so a `var`
    // write reaches the widget on the next tick with no plumbing of its own.
    registered();
    *LAST.lock().unwrap() = None;
    let source = "View Main() { var series = [1.0, 2.0] \
        Column { TestChart #[data: series, width: 40, height: 20] \
        Button(\"grow\") => series = series + [9.0] } }";
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(seen().data, vec![1.0, 2.0]);

    // Tap the button, which writes the `var`, and render again.
    interp.dispatch_events(&[byard_core::InputEvent {
        kind: byard_core::platform::EventKind::Tap,
        pos: (60.0, 30.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 1,
    }]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    let after = seen();
    assert_eq!(
        after.data,
        vec![1.0, 2.0, 9.0],
        "a view sees the value the language evaluated this tick"
    );
    assert_eq!(after.renders, 2, "one render per frame, not per prop");
}

#[test]
fn a_pointer_event_over_the_view_reaches_it_in_its_own_coordinates() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registered();
    *LAST.lock().unwrap() = None;
    let source = "View Main() { Column #[p: 20] { \
        TestChart #[data: [1.0], width: 100, height: 60] } }";
    let parsed = parse(source);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // A tap inside the view's rect.
    interp.dispatch_events(&[byard_core::InputEvent {
        kind: byard_core::platform::EventKind::Tap,
        pos: (60.0, 40.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 1,
    }]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(seen().taps, 1, "the view handled the tap itself");

    // And a tap far outside it does not.
    interp.dispatch_events(&[byard_core::InputEvent {
        kind: byard_core::platform::EventKind::Tap,
        pos: (390.0, 290.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 2,
    }]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);
    assert_eq!(seen().taps, 1, "a miss is not the view's event");
}

#[test]
fn what_reaches_a_view_is_data_and_only_data() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // INV-13, the same rule the controller boundary follows. A prop value is
    // evaluated first, so what a view can be handed is whatever the evaluator
    // produces: numbers, strings, lists, records. There is deliberately no
    // spelling that hands over the signal itself, and the conversion refuses
    // one anyway rather than trusting that (`NonDataViewProp`).
    let (interp, _, _) = run("View Main() { var series = [1.0, 2.0] \
         TestChart #[data: series, width: 10, height: 10] }");
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    assert_eq!(
        seen().data,
        vec![1.0, 2.0],
        "the view was handed the signal's value, not the signal"
    );
}
