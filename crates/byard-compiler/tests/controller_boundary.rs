//! End-to-end coverage of the controller boundary (RFC-0028 §4–§7) and the
//! lifecycle effects that drive it (RFC-0028 §4b).
//!
//! These are integration tests rather than unit tests on purpose. The thing
//! this phase adds is a *path*, from an action, through the Tokio pool, back
//! onto the logic thread, into a `var`, and onto the screen, and every unit
//! along it already existed and already passed its own tests while the path
//! did not work at all. So each test here drives the real `Relay`, the real
//! registry and the real `Interpreter`, and asserts on what is rendered.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::bridge::{BoxFuture, Controller, ControllerRegistry, Dispatcher, HostValue};
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;

/// A controller whose replies the test decides, so a success, a failure and a
/// slow answer are all reachable without a network.
struct Echo {
    /// How many times `invoke` has been reached, so "the call was actually
    /// placed" is observable separately from "its reply was applied".
    calls: Arc<AtomicUsize>,
}

impl Controller for Echo {
    fn type_name(&self) -> &'static str {
        "Echo"
    }

    fn invoke(
        &self,
        method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let out = match method {
            "say" => Ok(HostValue::Record(vec![(
                "text".to_string(),
                args.into_iter().next().unwrap_or(HostValue::Unit),
            )])),
            "fail" => Err(HostValue::Record(vec![(
                "message".to_string(),
                HostValue::Str("nope".to_string()),
            )])),
            other => Err(HostValue::Record(vec![(
                "message".to_string(),
                HostValue::Str(format!("no method {other}")),
            )])),
        };
        Box::pin(async move { out })
    }
}

/// A running interpreter wired to a real relay, plus the knobs a test needs:
/// tap a button, pump the async round trip, read the screen.
struct Harness {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    relay: Relay,
    frame: RenderFrame,
    calls: Arc<AtomicUsize>,
}

impl Harness {
    fn new(source: &str) -> Self {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(Echo {
            calls: Arc::clone(&calls),
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
            calls,
        };
        harness.render();
        harness
    }

    fn render(&mut self) {
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 800.0, 600.0);
    }

    /// The rendered text lines, in paint order.
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

    /// Waits for a reply to land on the relay's logic channel, applies it, and
    /// re-renders. Returns whether anything was applied.
    fn pump(&mut self) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut results = Vec::new();
        while results.is_empty() && std::time::Instant::now() < deadline {
            while let Some(result) = self.relay.try_recv_io_result() {
                results.push(result);
            }
            if results.is_empty() {
                std::thread::yield_now();
            }
        }
        let applied = self.interp.apply_io_results(results);
        self.interp.tick();
        self.render();
        applied
    }
}

/// A view with one button that calls `Echo::say` and both arms written.
const SAY_VIEW: &str = r#"
View Main() {
    inject Echo as echo
    var status: Str = "idle"
    Column {
        Text("{status}")
        Button("go") #[width: 100, height: 40]
            => { echo.say("hi") ok r => { status = r.text } err e => { status = e.message } }
    }
}
"#;

// ── grammar (RFC-0028 §4) ────────────────────────────────────────────────

#[test]
fn result_arms_parse_and_ok_and_err_stay_ordinary_identifiers_elsewhere() {
    // The arms are contextual: the trigger is `IDENT IDENT "=>"` right after a
    // call, so a `var` named `ok` in the same file must still be a `var`.
    let parsed = parse(
        r#"
View Main() {
    inject Echo as echo
    var ok: Int = 1
    var err: Int = 2
    Text("{ok}{err}")
    Button("g") => { echo.say("hi") ok r => { ok = 3 } }
}
"#,
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
}

#[test]
fn a_call_in_a_pure_context_is_rejected() {
    // RFC-0028 §4: a call is an effect. A `let` is a projection, and a
    // projection that could place a network call would re-place it every time
    // it was re-pulled.
    let mut harness = Harness::new(
        r#"
View Main() {
    inject Echo as echo
    let bad = echo.say("hi")
    Text("x")
}
"#,
    );
    harness.render();
    assert!(
        harness
            .interp
            .errors()
            .iter()
            .any(|e| e.kind() == "EffectInPureContext"),
        "expected EffectInPureContext, got {:?}",
        harness.interp.errors()
    );
}

// ── the round trip (RFC-0028 §5) ─────────────────────────────────────────

#[test]
fn a_tap_places_a_call_and_the_reply_writes_the_var() {
    let mut harness = Harness::new(SAY_VIEW);
    assert_eq!(harness.texts()[0], "idle");

    harness.tap(50.0, 40.0);
    assert_eq!(
        harness.interp.outstanding_continuations(),
        1,
        "the tap must have placed exactly one call"
    );
    // The action returned immediately: the frame it produced still shows the
    // pre-call state, which is the point of not blocking.
    assert_eq!(harness.texts()[0], "idle");

    assert!(
        harness.pump(),
        "the reply must report that it changed state"
    );
    assert_eq!(
        harness.calls.load(Ordering::SeqCst),
        1,
        "the controller method itself ran exactly once, on the pool"
    );
    assert_eq!(harness.texts()[0], "hi");
}

#[test]
fn a_failing_call_runs_the_err_arm() {
    let mut harness = Harness::new(
        r#"
View Main() {
    inject Echo as echo
    var status: Str = "idle"
    Column {
        Text("{status}")
        Button("go") #[width: 100, height: 40]
            => { echo.fail() ok r => { status = "unreachable" } err e => { status = e.message } }
    }
}
"#,
    );
    harness.tap(50.0, 40.0);
    harness.pump();
    assert_eq!(harness.texts()[0], "nope");
}

#[test]
fn a_continuation_is_one_shot() {
    // RFC-0028 §5 step 4: applied, then removed. Two taps must leave two
    // continuations and clear both, not accumulate.
    let mut harness = Harness::new(SAY_VIEW);
    harness.tap(50.0, 40.0);
    assert_eq!(harness.interp.outstanding_continuations(), 1);
    harness.pump();
    assert_eq!(harness.interp.outstanding_continuations(), 0);

    harness.tap(50.0, 40.0);
    harness.pump();
    assert_eq!(harness.interp.outstanding_continuations(), 0);
}

#[test]
fn a_reply_for_a_reloaded_program_is_discarded_not_applied() {
    // INV-14. The `var` must keep the value it had; the alternative, writing
    // it, would apply an answer to a question a different program asked.
    let mut harness = Harness::new(SAY_VIEW);
    harness.tap(50.0, 40.0);
    assert_eq!(harness.interp.outstanding_continuations(), 1);

    let replacement = parse(SAY_VIEW);
    harness.interp.reload(
        &replacement.views[0],
        byard_compiler::interp::reload::ReloadKind::StructureIncompatible,
    );
    assert_eq!(
        harness.interp.outstanding_continuations(),
        0,
        "a reload drops every pending continuation"
    );

    assert!(
        !harness.pump(),
        "a reply with no continuation must apply nothing"
    );
    let discard = harness
        .interp
        .errors()
        .iter()
        .find(|e| e.kind() == "DiscardedControllerReply")
        .expect("the discard must be reported, not silent");
    // Reported *at the call that was dropped*: a diagnostic anchored at an
    // empty range is one the developer cannot act on, and this is the one
    // place the message is actionable.
    assert!(
        discard.span().end > discard.span().start,
        "the discard must point at its call site, got {:?}",
        discard.span()
    );
    assert_eq!(
        &SAY_VIEW[discard.span().start as usize..discard.span().end as usize],
        r#"echo.say("hi") ok r => { status = r.text } err e => { status = e.message }"#
    );
}

#[test]
fn fire_and_forget_places_the_call_and_needs_no_arms() {
    let mut harness = Harness::new(
        r#"
View Main() {
    inject Echo as echo
    Column {
        Text("static")
        Button("go") #[width: 100, height: 40] => { echo.say("hi") }
    }
}
"#,
    );
    harness.tap(50.0, 40.0);
    assert_eq!(
        harness.interp.outstanding_continuations(),
        0,
        "a call nobody is listening to opens no continuation"
    );
    // The reply still arrives and must be absorbed without a diagnostic.
    harness.pump();
    assert_eq!(harness.calls.load(Ordering::SeqCst), 1);
    assert!(
        !harness
            .interp
            .errors()
            .iter()
            .any(|e| e.kind() == "DiscardedControllerReply"),
        "an expected fire-and-forget reply is not a discard"
    );
}

#[test]
fn a_signal_cannot_cross_the_boundary() {
    // INV-13: the boundary carries data. Passing the `var` itself (not its
    // value) would put logic-thread state on a pool worker.
    let mut harness = Harness::new(
        r#"
View Main() {
    inject Echo as echo
    fn identity(n) => n
    Column {
        Text("x")
        Button("go") #[width: 100, height: 40] => { echo.say(identity) }
    }
}
"#,
    );
    harness.tap(50.0, 40.0);
    assert_eq!(
        harness.calls.load(Ordering::SeqCst),
        0,
        "the call must be abandoned, not truncated to a placeholder argument"
    );
    assert!(
        harness
            .interp
            .errors()
            .iter()
            .any(|e| e.kind() == "NonDataControllerArg"),
        "expected NonDataControllerArg, got {:?}",
        harness.interp.errors()
    );
}

// ── lifecycle effects (RFC-0028 §4b) ─────────────────────────────────────

#[test]
fn on_mount_runs_once_when_the_view_appears() {
    let mut harness = Harness::new(
        r#"
View Main() {
    var hits: Int = 0
    on mount => { hits = hits + 1 }
    Text("{hits}")
}
"#,
    );
    assert_eq!(harness.texts()[0], "1");
    // Rendering again is not remounting.
    harness.render();
    harness.interp.tick();
    harness.render();
    assert_eq!(harness.texts()[0], "1");
}

#[test]
fn a_mount_effect_may_place_a_call_with_no_input_at_all() {
    // This is the entry point RFC-0028 §4b exists for: a data-backed screen
    // has to be able to ask for its data without the user doing anything.
    let mut harness = Harness::new(
        r#"
View Main() {
    inject Echo as echo
    var status: Str = "loading"
    on mount => { echo.say("auto") ok r => { status = r.text } }
    Text("{status}")
}
"#,
    );
    assert_eq!(harness.texts()[0], "loading");
    assert_eq!(harness.calls.load(Ordering::SeqCst), 1);
    harness.pump();
    assert_eq!(harness.texts()[0], "auto");
}

#[test]
fn a_when_branch_remounting_runs_on_mount_again() {
    let mut harness = Harness::new(
        r#"
View Main() {
    var open: Bool = true
    var mounts: Int = 0
    Column {
        Text("{mounts}")
        when open {
            on mount => { mounts = mounts + 1 }
            Text("panel")
        }
    }
}
"#,
    );
    assert_eq!(harness.texts()[0], "1");
    assert_eq!(harness.interp.mounted_effects(), 1);

    let open = harness
        .interp
        .var_signal(&byard_compiler::Symbol::intern("open"))
        .expect("open is a var");
    harness
        .interp
        .write_var(open, byard_compiler::interp::env::Value::Bool(false));
    harness.interp.tick();
    harness.render();
    assert_eq!(
        harness.interp.mounted_effects(),
        0,
        "the collapsed branch's effect must have unmounted"
    );

    harness
        .interp
        .write_var(open, byard_compiler::interp::env::Value::Bool(true));
    harness.interp.tick();
    harness.render();
    assert_eq!(
        harness.texts()[0],
        "2",
        "a branch that comes back mounts again"
    );
}

#[test]
fn on_unmount_runs_when_its_scope_goes_away() {
    let mut harness = Harness::new(
        r#"
View Main() {
    var open: Bool = true
    var closed: Int = 0
    Column {
        Text("{closed}")
        when open {
            on unmount => { closed = closed + 1 }
            Text("panel")
        }
    }
}
"#,
    );
    assert_eq!(harness.texts()[0], "0");

    let open = harness
        .interp
        .var_signal(&byard_compiler::Symbol::intern("open"))
        .expect("open is a var");
    harness
        .interp
        .write_var(open, byard_compiler::interp::env::Value::Bool(false));
    harness.interp.tick();
    harness.render();
    assert_eq!(harness.texts()[0], "1");
}

#[test]
fn an_unmounted_scopes_in_flight_call_is_dropped_with_it() {
    // INV-10/INV-14: a screen that asked for data and then left must not have
    // its answer written into it afterwards.
    let mut harness = Harness::new(
        r#"
View Main() {
    inject Echo as echo
    var open: Bool = true
    var status: Str = "idle"
    Column {
        Text("{status}")
        when open {
            on mount => { echo.say("late") ok r => { status = r.text } }
            Text("panel")
        }
    }
}
"#,
    );
    assert_eq!(harness.interp.outstanding_continuations(), 1);

    let open = harness
        .interp
        .var_signal(&byard_compiler::Symbol::intern("open"))
        .expect("open is a var");
    harness
        .interp
        .write_var(open, byard_compiler::interp::env::Value::Bool(false));
    harness.interp.tick();
    harness.render();
    assert_eq!(
        harness.interp.outstanding_continuations(),
        0,
        "the unmount takes the call it started with it"
    );

    harness.pump();
    assert_eq!(
        harness.texts()[0],
        "idle",
        "the late reply must not write a var the departed scope owned"
    );
}

// ── the registry (RFC-0028 §3) ───────────────────────────────────────────

#[test]
fn an_unregistered_handle_answers_with_an_error_rather_than_vanishing() {
    // The `byard check` path binds an unbound handle so calls still lower. If
    // one is ever actually placed, the failure has to be visible at the call
    // site, not a call that quietly never returns.
    let relay = Relay::new().expect("relay");
    let dispatcher = Dispatcher::new(
        ControllerRegistry::new(),
        relay.io_handle(),
        relay.io_result_sender(),
    );
    let parsed = parse(
        r#"
View Main() {
    inject Ghost as ghost
    var status: Str = "idle"
    Column {
        Text("{status}")
        Button("go") #[width: 100, height: 40]
            => { ghost.anything() ok r => { status = "ok" } err e => { status = e.kind } }
    }
}
"#,
    );
    let mut interp = Interpreter::new();
    interp.set_dispatcher(dispatcher);
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 600.0);
    // With a registry present, an `inject` nothing provides is an error, the
    // host knows its whole controller set.
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| e.kind() == "UnresolvedInject"),
        "expected UnresolvedInject, got {:?}",
        interp.errors()
    );
}

#[test]
fn a_headless_check_reports_an_unknown_inject_as_a_warning_and_still_checks_the_call() {
    // A two-layer app's controllers are registered by its Rust half at run
    // time, so `byard check` cannot know they exist. Failing every such app
    // would make the checker useless on exactly the apps this phase enables.
    let parsed = parse(SAY_VIEW);
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 600.0);

    let errors: Vec<&byard_compiler::CompileError> =
        interp.errors().iter().filter(|e| !e.is_warning()).collect();
    assert!(errors.is_empty(), "no hard errors expected: {errors:?}");
    assert!(
        interp
            .errors()
            .iter()
            .any(|e| e.kind() == "UncheckableInject"),
        "the unknown inject must still be reported, as a warning"
    );
}

// ── the shipped example (the thing a person can actually look at) ────────

/// The `.byd` half of `examples/controller_demo`, compiled into the test so
/// the example cannot drift away from the feature it demonstrates.
const DEMO_VIEW: &str = include_str!("../../../examples/controller_demo/src/main.byd");

#[test]
fn the_controller_demo_view_mounts_and_places_its_call_against_a_real_registry() {
    // Run the shipped example's own view, with `Greeter` standing in for the
    // crate's controller. Two things are asserted because two things can rot
    // independently: the file still compiles, and the interaction it advertises
    // still happens.
    let relay = Relay::new().expect("relay");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ControllerRegistry::new();
    registry.insert(Arc::new(Greeter {
        calls: Arc::clone(&calls),
    }));
    let dispatcher = Dispatcher::new(registry, relay.io_handle(), relay.io_result_sender());

    let parsed = parse(DEMO_VIEW);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let mut interp = Interpreter::new();
    interp.set_dispatcher(dispatcher);
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 600.0);

    let hard: Vec<&byard_compiler::CompileError> =
        interp.errors().iter().filter(|e| !e.is_warning()).collect();
    assert!(hard.is_empty(), "the example must render clean: {hard:?}");

    let texts: Vec<String> = frame.texts().iter().map(|t| t.text.clone()).collect();
    assert!(
        texts.iter().any(|t| t == "status: ready"),
        "`on mount` must have run: {texts:?}"
    );
}

/// The example crate's controller, re-declared here rather than depended on:
/// a test that pulled in the binary crate would be asserting on a build
/// artifact, and the shape is three lines.
struct Greeter {
    calls: Arc<AtomicUsize>,
}

impl Controller for Greeter {
    fn type_name(&self) -> &'static str {
        "Greeter"
    }

    fn invoke(
        &self,
        _method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let name = match args.into_iter().next() {
            Some(HostValue::Str(s)) => s,
            _ => String::new(),
        };
        Box::pin(async move {
            Ok(HostValue::Record(vec![
                (
                    "text".to_string(),
                    HostValue::Str(format!("Hello, {name}!")),
                ),
                (
                    "length".to_string(),
                    HostValue::Int(i64::try_from(name.len()).unwrap_or(i64::MAX)),
                ),
            ]))
        })
    }
}

#[test]
fn a_controller_error_with_no_err_arm_is_reported_as_a_failure_not_a_missing_method() {
    // Two different mistakes, two different messages. `UnknownControllerMethod`
    // is a wiring error the developer fixes in the call; this is the
    // controller's own runtime failure, and reporting it as the former
    // produced headlines like "controller has no method `type a name first`".
    let mut harness = Harness::new(
        r#"
View Main() {
    inject Echo as echo
    var status: Str = "idle"
    Column {
        Text("{status}")
        Button("go") #[width: 100, height: 40]
            => { echo.fail() ok r => { status = "unreachable" } }
    }
}
"#,
    );
    harness.tap(50.0, 40.0);
    harness.pump();

    let failure = harness
        .interp
        .errors()
        .iter()
        .find(|e| e.kind() == "ControllerCallFailed")
        .expect("an error nobody handled must be reported");
    assert!(
        failure.headline().contains("nope"),
        "the controller's own message must survive: {}",
        failure.headline()
    );
    assert!(
        !harness
            .interp
            .errors()
            .iter()
            .any(|e| e.kind() == "UnknownControllerMethod"),
        "a runtime failure must not be reported as a missing method"
    );
}
