//! A screen, a `when` branch or a `for` row that goes away gives its reactive
//! scopes back, and the next one reuses them (RFC-0018, RFC-0026).
//!
//! The unit tests next to the interpreter cover the mechanics; these drive the
//! public surface a host sees (`Interpreter::render`, `live_scope_count`,
//! `scope_slot_count`) over the shapes an app is made of, and the one place
//! where giving scopes back early would be wrong: a controller reply that
//! arrives after the frame that raised the call has been replaced.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::bridge::{BoxFuture, Controller, ControllerRegistry, Dispatcher, HostValue};
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent};
use byard_core::relay::Relay;

fn lower(src: &str, dispatcher: Option<Dispatcher>) -> (Interpreter, Vec<RenderNode>) {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    if let Some(dispatcher) = dispatcher {
        interp.set_dispatcher(dispatcher);
    }
    interp.load_views(&parsed.views);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    let tree = interp.lower_view(&parsed.views[0], &known);
    interp.tick();
    (interp, tree)
}

fn texts(interp: &mut Interpreter, tree: &[RenderNode]) -> Vec<String> {
    let mut frame = RenderFrame::new();
    interp.render(tree, &mut frame, 800.0, 600.0);
    frame.texts().iter().map(|t| t.text.clone()).collect()
}

fn set(interp: &mut Interpreter, name: &str, value: Value) {
    let sig = interp.var_signal(&Symbol::intern(name)).expect("a var");
    interp.write_var(sig, value);
    interp.tick();
}

fn tap(interp: &mut Interpreter, x: f32, y: f32) {
    let event = |kind, time_ms| InputEvent {
        kind,
        pos: (x, y),
        delta: (0.0, 0.0),
        payload: None,
        time_ms,
        mods: byard_core::KeyModifiers::default(),
    };
    interp.dispatch_events(&[
        event(EventKind::PointerDown, 0),
        event(EventKind::PointerUp, 10),
    ]);
    interp.tick();
}

const SHOP: &str = r#"
View Shop() {
    var path = "/"
    var tags = ["new", "sale"]
    NavStack(path: path) {
        route "/" { Text("catalogue") }
        route "/product/:id" {|p| Product(id: p.id) }
    }
}
View Product(id: Str) {
    let title = "product {id}"
    var qty = 1
    when qty > 0 { Text("{title} x{qty}") } else { Text("{title} sold out") }
    for tag in tags { Text("{title} #{tag}") }
}
"#;

#[test]
fn a_screen_pushed_and_popped_ten_times_costs_what_it_cost_once() {
    let (mut interp, tree) = lower(SHOP, None);
    assert_eq!(texts(&mut interp, &tree), ["catalogue"]);
    let home = interp.live_scope_count();

    set(&mut interp, "path", Value::Str("/product/9".into()));
    assert_eq!(
        texts(&mut interp, &tree),
        ["product 9 x1", "product 9 #new", "product 9 #sale"]
    );
    let product = interp.live_scope_count();
    assert!(product > home);
    set(&mut interp, "path", Value::Str("/".into()));
    assert_eq!(texts(&mut interp, &tree), ["catalogue"]);
    assert_eq!(
        interp.live_scope_count(),
        home,
        "the product's scopes are back"
    );
    let slots = interp.scope_slot_count();

    for i in 0..10 {
        set(&mut interp, "path", Value::Str(format!("/product/{i}")));
        let shown = texts(&mut interp, &tree);
        assert_eq!(shown[0], format!("product {i} x1"));
        assert_eq!(interp.live_scope_count(), product);
        set(&mut interp, "path", Value::Str("/".into()));
        assert_eq!(texts(&mut interp, &tree), ["catalogue"]);
        assert_eq!(interp.live_scope_count(), home);
    }
    assert_eq!(
        interp.scope_slot_count(),
        slots,
        "no slot was added after the first visit"
    );
}

#[test]
fn a_list_that_shrinks_from_a_thousand_to_one_gives_its_scopes_back() {
    let (mut interp, tree) = lower(
        r#"
View List() {
    var items = []
    Column { for it in items { Text("row {it}") } }
}
"#,
        None,
    );
    texts(&mut interp, &tree);
    let empty = interp.live_scope_count();
    let rows = |n: usize, tag: &str| {
        Value::List((0..n).map(|i| Value::Str(format!("{tag}{i}"))).collect())
    };
    set(&mut interp, "items", rows(1000, "a"));
    assert_eq!(texts(&mut interp, &tree).len(), 1000);
    let peak = interp.live_scope_count();
    assert!(peak >= empty + 1000, "a row opens at least a scope");

    set(&mut interp, "items", rows(1, "b"));
    assert_eq!(texts(&mut interp, &tree), ["row b0"]);
    let shrunk = interp.live_scope_count();
    assert!(
        shrunk < empty + 64,
        "a list of one keeps {shrunk} scopes live, was {peak} at a thousand"
    );

    set(&mut interp, "items", rows(1000, "c"));
    let shown = texts(&mut interp, &tree);
    assert_eq!((shown.len(), shown[999].as_str()), (1000, "row c999"));
    assert_eq!(
        interp.live_scope_count(),
        peak,
        "growing back costs what it did"
    );
}

#[test]
fn a_handler_that_calls_a_fn_does_not_grow_the_arena_frame_after_frame() {
    let (mut interp, tree) = lower(
        r#"
View Counter() {
    var n = 0
    fn plus(k) => n + k
    Column {
        Button("go") #[width: 100, height: 40] => { n = plus(1) }
        Text("{n}")
    }
}
"#,
        None,
    );
    texts(&mut interp, &tree);
    texts(&mut interp, &tree);
    let (slots, live) = (interp.scope_slot_count(), interp.live_scope_count());
    for _ in 0..500 {
        texts(&mut interp, &tree);
    }
    assert_eq!(interp.scope_slot_count(), slots);
    assert_eq!(interp.live_scope_count(), live);
    tap(&mut interp, 50.0, 20.0);
    assert_eq!(texts(&mut interp, &tree)[1], "1");
}

// ── a reply that outlives the frame that raised the call ─────────────────────

struct Echo(Arc<AtomicUsize>);

impl Controller for Echo {
    fn type_name(&self) -> &'static str {
        "Echo"
    }

    fn invoke(
        &self,
        _method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let out = Ok(HostValue::Record(vec![(
            "text".to_string(),
            args.into_iter().next().unwrap_or(HostValue::Unit),
        )]));
        Box::pin(async move { out })
    }
}

#[test]
fn a_reply_that_arrives_frames_later_still_finds_the_scopes_its_arms_read() {
    // The arms of a call are lowered with the handler that raised it, and here
    // they call `label("bob")`, which opens a scope for its argument. The reply
    // lands after several frames have replaced that handler: if its scopes had
    // been given back with it, the arm would read nothing.
    let relay = Relay::new().expect("relay");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ControllerRegistry::new();
    registry.insert(Arc::new(Echo(Arc::clone(&calls))));
    let dispatcher = Dispatcher::new(registry, relay.io_handle(), relay.io_result_sender());

    let (mut interp, tree) = lower(
        r#"
View Main() {
    inject Echo as echo
    var status: Str = "idle"
    fn label(who) => "sent {who}"
    Column {
        Text("{status}")
        Button("go") #[width: 100, height: 40]
            => { echo.say("hi") ok r => { status = label("bob") } }
    }
}
"#,
        Some(dispatcher),
    );
    assert_eq!(texts(&mut interp, &tree)[0], "idle");
    let idle = interp.live_scope_count();

    tap(&mut interp, 50.0, 40.0);
    assert_eq!(interp.outstanding_continuations(), 1);
    for _ in 0..5 {
        texts(&mut interp, &tree);
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut results = Vec::new();
    while results.is_empty() && std::time::Instant::now() < deadline {
        while let Some(result) = relay.try_recv_io_result() {
            results.push(result);
        }
        std::thread::yield_now();
    }
    assert!(interp.apply_io_results(results));
    interp.tick();
    assert_eq!(texts(&mut interp, &tree)[0], "sent bob");

    // With nothing in flight any more, the frames' scopes are given back.
    for _ in 0..3 {
        texts(&mut interp, &tree);
    }
    assert_eq!(interp.live_scope_count(), idle);
}
