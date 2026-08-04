//! Timer effects, end to end (RFC-0029 O4/§5).
//!
//! `every 1s => …` and `after 200ms => …` are structural effects: armed when
//! their scope mounts, cancelled when it unmounts, and delivered through the
//! **same** continuation/apply path a controller reply travels. These tests
//! drive the real relay and the real Tokio time driver, because a timer that
//! only fires in a fake clock proves nothing about the reactor.

use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_compiler::symbol::Symbol;
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;

struct Harness {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    relay: Relay,
    frame: RenderFrame,
}

impl Harness {
    fn new(source: &str) -> Self {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let dispatcher = Dispatcher::new(
            ControllerRegistry::new(),
            relay.io_handle(),
            relay.io_result_sender(),
        );
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
        };
        harness.render();
        harness
    }

    fn render(&mut self) {
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 800.0, 600.0);
    }

    fn text(&self) -> String {
        self.frame
            .texts()
            .first()
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    fn var(&self, name: &str) -> Value {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .expect("a declared var");
        self.interp.peek(sig)
    }

    fn set(&mut self, name: &str, value: Value) {
        let sig = self
            .interp
            .var_signal(&Symbol::intern(name))
            .expect("a declared var");
        self.interp.write_var(sig, value);
        self.interp.tick();
        self.render();
    }

    /// Drains and applies whatever the pool has queued, without waiting.
    fn drain(&mut self) {
        let mut results = Vec::new();
        while let Some(result) = self.relay.try_recv_io_result() {
            results.push(result);
        }
        let _ = self.interp.apply_io_results(results);
        self.interp.tick();
        self.render();
    }

    /// Pumps until `counter` reaches `target`, or the deadline passes.
    fn pump_until(&mut self, counter: &str, target: i64, within: std::time::Duration) {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            self.drain();
            if self.var(counter).as_int().unwrap_or(0) >= target {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

// ── grammar (RFC-0029 §5) ────────────────────────────────────────────────

#[test]
fn minutes_seconds_and_milliseconds_all_lower_to_milliseconds() {
    // The lexer knew only `ms` and `s`; a refresh interval is written in the
    // unit a person says it in, and `every 300000ms` hides a mistyped zero.
    let parsed = parse(
        r#"
View Main() {
    var n: Int = 0
    every 5min => { n = n + 1 }
    after 1500ms => { n = n + 1 }
    every 2s => { n = n + 1 }
    Text("{n}")
}
"#,
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let durations: Vec<(bool, u64)> = parsed.views[0]
        .body
        .iter()
        .filter_map(|m| match m {
            byard_compiler::parser::ast::Member::Timer { every, dur_ms, .. } => {
                Some((*every, *dur_ms))
            }
            _ => None,
        })
        .collect();
    assert_eq!(durations, [(true, 300_000), (false, 1_500), (true, 2_000)]);
}

#[test]
fn every_and_after_stay_ordinary_identifiers_without_a_duration() {
    // Contextual, like `on mount`: the keyword is only special when a duration
    // literal follows, so an app may still name a `var` `after`.
    let parsed = parse(
        r#"
View Main() {
    var every: Int = 1
    var after: Int = 2
    Text("{every}{after}")
}
"#,
    );
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
}

// ── firing (RFC-0029 §5) ─────────────────────────────────────────────────

#[test]
fn after_fires_once_and_only_once() {
    let mut harness = Harness::new(
        r#"
View Main() {
    var fired: Int = 0
    after 20ms => { fired = fired + 1 }
    Text("{fired}")
}
"#,
    );
    assert_eq!(harness.text(), "0", "nothing fires before its delay");

    harness.pump_until("fired", 1, std::time::Duration::from_secs(3));
    assert_eq!(harness.text(), "1");

    // Well past a second delay's worth of time: a one-shot must not repeat.
    std::thread::sleep(std::time::Duration::from_millis(120));
    harness.drain();
    assert_eq!(harness.text(), "1");
}

#[test]
fn every_fires_repeatedly() {
    let mut harness = Harness::new(
        r#"
View Main() {
    var ticks: Int = 0
    every 10ms => { ticks = ticks + 1 }
    Text("{ticks}")
}
"#,
    );
    harness.pump_until("ticks", 3, std::time::Duration::from_secs(5));
    assert!(
        harness.var("ticks").as_int().unwrap_or(0) >= 3,
        "a repeating timer must fire more than once, got {}",
        harness.text()
    );
}

#[test]
fn the_first_tick_of_every_waits_out_its_interval() {
    // `every 5min` means "in five minutes", not "now, and then every five
    // minutes". Tokio's `interval` completes its first tick immediately, so
    // this is the assertion that the driver consumes it.
    let mut harness = Harness::new(
        r#"
View Main() {
    var ticks: Int = 0
    every 30s => { ticks = ticks + 1 }
    Text("{ticks}")
}
"#,
    );
    std::thread::sleep(std::time::Duration::from_millis(80));
    harness.drain();
    assert_eq!(harness.text(), "0");
}

// ── scope (INV-10) ───────────────────────────────────────────────────────

#[test]
fn an_unmounted_scope_stops_ticking() {
    // The leak this exists to prevent: a screen that polls, closed, still
    // polling. Nothing about it is visible until it writes a `var` nobody is
    // watching, so it is asserted directly.
    let mut harness = Harness::new(
        r#"
View Main() {
    var open: Bool = true
    var ticks: Int = 0
    Column {
        Text("{ticks}")
        when open {
            every 10ms => { ticks = ticks + 1 }
            Text("panel")
        }
    }
}
"#,
    );
    harness.pump_until("ticks", 2, std::time::Duration::from_secs(5));
    let while_open = harness.var("ticks").as_int().unwrap_or(0);
    assert!(while_open >= 2, "the timer must have been ticking first");

    harness.set("open", Value::Bool(false));
    // Anything already in flight is drained and discarded, then nothing more
    // may arrive.
    harness.drain();
    let at_close = harness.var("ticks").as_int().unwrap_or(0);

    std::thread::sleep(std::time::Duration::from_millis(120));
    harness.drain();
    assert_eq!(
        harness.var("ticks").as_int().unwrap_or(0),
        at_close,
        "a closed scope's timer kept firing"
    );
}

#[test]
fn a_scope_that_comes_back_arms_a_fresh_timer() {
    let mut harness = Harness::new(
        r#"
View Main() {
    var open: Bool = true
    var ticks: Int = 0
    Column {
        Text("{ticks}")
        when open {
            every 10ms => { ticks = ticks + 1 }
            Text("panel")
        }
    }
}
"#,
    );
    harness.pump_until("ticks", 1, std::time::Duration::from_secs(5));
    harness.set("open", Value::Bool(false));
    harness.drain();
    let closed_at = harness.var("ticks").as_int().unwrap_or(0);

    harness.set("open", Value::Bool(true));
    harness.pump_until("ticks", closed_at + 2, std::time::Duration::from_secs(5));
    assert!(
        harness.var("ticks").as_int().unwrap_or(0) > closed_at,
        "a re-mounted scope must tick again"
    );
}

#[test]
fn a_zero_interval_is_refused_rather_than_armed() {
    // A zero-period interval fires as fast as the pool can send, which is a
    // livelock dressed as a timer.
    let mut harness = Harness::new(
        r#"
View Main() {
    var ticks: Int = 0
    every 0ms => { ticks = ticks + 1 }
    Text("{ticks}")
}
"#,
    );
    std::thread::sleep(std::time::Duration::from_millis(60));
    harness.drain();
    assert_eq!(harness.text(), "0");
}

// ── the shipped example ──────────────────────────────────────────────────

#[test]
fn the_timers_example_arms_its_timers_on_mount() {
    const TIMERS: &str = include_str!("../../byard-cli/examples/timers/src/main.byd");
    let mut harness = Harness::new(TIMERS);
    let hard: Vec<&byard_compiler::CompileError> = harness
        .interp
        .errors()
        .iter()
        .filter(|e| !e.is_warning())
        .collect();
    assert!(hard.is_empty(), "the example must render clean: {hard:?}");

    // The clock advances on its own, with no input at all, which is the whole
    // point of the example.
    harness.pump_until("seconds", 1, std::time::Duration::from_secs(4));
    assert!(
        harness.var("seconds").as_int().unwrap_or(0) >= 1,
        "the example's `every 1s` must have ticked"
    );
}

#[test]
fn a_closed_screens_timer_does_not_leave_a_diagnostic_behind() {
    // A controller reply that outlives its scope is worth reporting: someone
    // asked a question and the answer went nowhere. A timer tick is not, its
    // task is cancelled with the scope, and reporting the race would put a
    // diagnostic on every screen that closes.
    let mut harness = Harness::new(
        r#"
View Main() {
    var open: Bool = true
    var ticks: Int = 0
    Column {
        Text("{ticks}")
        when open {
            every 5ms => { ticks = ticks + 1 }
            Text("panel")
        }
    }
}
"#,
    );
    harness.pump_until("ticks", 2, std::time::Duration::from_secs(5));
    harness.set("open", Value::Bool(false));
    harness.drain();
    std::thread::sleep(std::time::Duration::from_millis(60));
    harness.drain();

    assert!(
        !harness
            .interp
            .errors()
            .iter()
            .any(|e| e.kind() == "DiscardedControllerReply"),
        "a cancelled timer is not a discarded reply: {:?}",
        harness.interp.errors()
    );
}
