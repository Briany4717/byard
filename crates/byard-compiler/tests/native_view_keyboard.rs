//! RFC-0039: a native view gets pointer events by position, and nothing else.
//!
//! Hosts push keyboard and text events with `pos: (0.0, 0.0)` because a key
//! has no position. A view sitting at the window's top-left corner contains
//! that point, so if dispatch offered it every event by rect, it could swallow
//! the keys and text that belong to the focused `TextField`. Keys reach a view
//! the way they reach an intrinsic, through focus, never through hit testing.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent, InputPayload};
use byard_core::render::{
    Handled, Layout, NativeProps, NativeView, NativeViewInfo, NativeViewMeta, RenderCtx,
};

/// A view that claims every event it is offered.
#[derive(Default)]
struct Greedy;

impl NativeProps for Greedy {
    fn set_prop(&mut self, _name: &str, _value: &byard_core::bridge::HostValue) {}
}

impl NativeView for Greedy {
    fn render(&mut self, _layout: Layout, _cx: &mut RenderCtx<'_>) {}

    fn on_event(&mut self, _event: &byard_core::render::Event, _layout: Layout) -> Handled {
        Handled::Yes
    }
}

impl NativeViewMeta for Greedy {
    const INFO: NativeViewInfo = NativeViewInfo {
        name: "TestGreedy",
        props: &[],
        events: &[],
    };

    fn create() -> Box<dyn NativeView> {
        Box::new(Self)
    }
}

fn ev(kind: EventKind, pos: (f32, f32), payload: Option<InputPayload>, t: u64) -> InputEvent {
    InputEvent {
        kind,
        pos,
        delta: (0.0, 0.0),
        payload,
        time_ms: t,
    }
}

#[test]
fn a_view_at_the_origin_does_not_take_the_focused_fields_keys() {
    byard_core::render::registry::register::<Greedy>();
    let source = "View Main() { var text = \"\" \
        Column { \
            TestGreedy #[width: 100, height: 40] \
            TextField #[bind: text, width: 200, height: 40] \
        } }";
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    let text = interp.var_signal(&Symbol::intern("text")).unwrap();
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Focus the field with a tap below the greedy view.
    let field = (20.0, 60.0);
    interp.dispatch_events(&[
        ev(EventKind::PointerDown, field, None, 0),
        ev(EventKind::PointerUp, field, None, 50),
    ]);
    interp.tick();
    interp.render(&tree, &mut frame, 400.0, 300.0);

    // Type the way the hosts do: at the origin, which is inside the view.
    let key = |s: &str| Some(InputPayload::Key(s.to_string()));
    interp.dispatch_events(&[
        ev(EventKind::TextInput, (0.0, 0.0), key("h"), 100),
        ev(EventKind::TextInput, (0.0, 0.0), key("i"), 110),
    ]);
    interp.tick();
    assert_eq!(
        interp.peek(text),
        Value::Str("hi".to_string()),
        "typed text belongs to the focused field, not the view at the origin"
    );

    // A key press, too: Backspace edits the field.
    interp.render(&tree, &mut frame, 400.0, 300.0);
    interp.dispatch_events(&[ev(EventKind::KeyDown, (0.0, 0.0), key("Backspace"), 200)]);
    interp.tick();
    assert_eq!(interp.peek(text), Value::Str("h".to_string()));
}
