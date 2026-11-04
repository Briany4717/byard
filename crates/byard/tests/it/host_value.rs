//! A struct crossing the controller boundary and back, through the whole path
//! an app takes: a `byld` action builds a record, the interpreter converts it
//! to a `HostValue`, the registry finds the controller the macro generated,
//! the pool runs the method, and the reply is applied on the logic thread and
//! shows on the screen. The derive and the dispatch each have unit tests; this
//! is the one that checks they agree with the interpreter.

// The controller's methods are `async` because the boundary is, and take their
// arguments by value because the conversion hands them over.
#![allow(clippy::unused_async, clippy::needless_pass_by_value)]

use std::sync::{Arc, Mutex};

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_compiler::parser::parse;
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;

#[derive(Debug, Clone, PartialEq, byard::FromHostValue, byard::IntoHostValue)]
struct Address {
    street: String,
    zip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, byard::HostValue)]
struct Customer {
    name: String,
    age: i64,
    address: Address,
    tags: Vec<String>,
    #[host(rename = "isVip")]
    vip: bool,
}

#[byard::byard_controller]
#[derive(Clone)]
struct Crm {
    seen: Arc<Mutex<Vec<Customer>>>,
}

#[byard::byard_controller]
impl Crm {
    /// Asynchronous, and answers with the same struct it was given, changed.
    async fn register(&self, customer: Customer) -> Customer {
        self.seen.lock().unwrap().push(customer.clone());
        let mut customer = customer;
        customer.tags.push("welcome".to_string());
        customer
    }

    /// Synchronous, with a reply that is a struct and an error that is text.
    fn find(&self, name: String) -> Result<Customer, String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.name == name)
            .cloned()
            .ok_or_else(|| format!("no customer called {name}"))
    }
}

struct Harness {
    interp: Interpreter,
    tree: Vec<RenderNode>,
    relay: Relay,
    frame: RenderFrame,
    seen: Arc<Mutex<Vec<Customer>>>,
}

const VIEW: &str = r#"
View Main() {
    inject Crm as crm
    var summary: Str = "idle"
    var city: Str = "idle"
    Column {
        Text("{summary}") #[height: 20]
        Text("{city}") #[height: 20]
        Button("register") #[width: 120, height: 40]
            => {
                crm.register({ name: "Ada", age: 36, address: { street: "Main St" }, tags: ["x"], isVip: true })
                    ok c => { summary = "{c.name} {c.age} {c.address.street} {c.tags.len} {c.isVip}" }
                    err e => { summary = e.message }
            }
        Button("find") #[width: 120, height: 40]
            => {
                crm.find("Ada") ok c => { city = "found {c.address.street}" } err e => { city = e }
            }
        Button("missing") #[width: 120, height: 40]
            => {
                crm.find("Nobody") ok c => { city = "unexpected" } err e => { city = e }
            }
    }
}
"#;

impl Harness {
    fn new() -> Self {
        let parsed = parse(VIEW);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(Crm {
            seen: Arc::clone(&seen),
        }));
        let dispatcher = Dispatcher::new(registry, relay.io_handle(), relay.io_result_sender());
        let mut interp = Interpreter::new();
        interp.set_dispatcher(dispatcher);
        interp.load_views(&parsed.views);
        let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
        let tree = interp.lower_view(&parsed.views[0], &known);
        interp.tick();
        let mut harness = Self {
            interp,
            tree,
            relay,
            frame: RenderFrame::new(),
            seen,
        };
        harness.render();
        harness
    }

    fn render(&mut self) {
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 800.0, 600.0);
    }

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn tap(&mut self, x: f32, y: f32) {
        use byard_core::platform::{EventKind, InputEvent};
        let down = InputEvent {
            kind: EventKind::PointerDown,
            pos: (x, y),
            delta: (0.0, 0.0),
            payload: None,
            time_ms: 0,
            mods: byard_core::KeyModifiers::default(),
        };
        let up = InputEvent {
            kind: EventKind::PointerUp,
            time_ms: 10,
            ..down.clone()
        };
        self.interp.dispatch_events(&[down, up]);
        self.interp.tick();
        self.render();
    }

    /// Applies every reply that is owed.
    fn answer_all(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.interp.outstanding_continuations() > 0 {
            assert!(std::time::Instant::now() < deadline, "no reply in 5s");
            let mut results = Vec::new();
            while let Some(r) = self.relay.try_recv_io_result() {
                results.push(r);
            }
            if results.is_empty() {
                std::thread::yield_now();
                continue;
            }
            let _ = self.interp.apply_io_results(results);
            self.interp.tick();
        }
        self.render();
    }
}

#[test]
fn a_record_from_a_view_is_a_struct_and_the_struct_comes_back_as_a_record() {
    let mut app = Harness::new();
    app.tap(60.0, 60.0);
    app.answer_all();

    // What the controller received, as the Rust struct it expected.
    assert_eq!(
        *app.seen.lock().unwrap(),
        vec![Customer {
            name: "Ada".into(),
            age: 36,
            // `zip` was left out of the record: `None`, not an empty string.
            address: Address {
                street: "Main St".into(),
                zip: None
            },
            tags: vec!["x".into()],
            vip: true,
        }]
    );
    // What the view read of the reply: the nested field, the list the
    // controller grew, and the field that was renamed in the record.
    assert!(
        app.texts().contains(&"Ada 36 Main St 2 true".to_string()),
        "{:?}",
        app.texts()
    );
}

#[test]
fn a_synchronous_method_answers_with_a_struct_or_with_text() {
    let mut app = Harness::new();
    app.tap(60.0, 60.0);
    app.answer_all();

    app.tap(60.0, 100.0);
    app.answer_all();
    assert_eq!(app.texts()[1], "found Main St", "{:?}", app.texts());

    app.tap(60.0, 140.0);
    app.answer_all();
    assert_eq!(
        app.texts()[1],
        "no customer called Nobody",
        "{:?}",
        app.texts()
    );
}
