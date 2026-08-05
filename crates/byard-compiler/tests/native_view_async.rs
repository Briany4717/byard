//! RFC-0039 §"Async across the boundary": a native view asks a controller for
//! something and is answered on the logic thread.
//!
//! An integration test rather than a unit test, for the same reason the
//! controller-boundary tests are: what this adds is a *path*, from a widget,
//! through the pool, back into the widget, and every piece along it already
//! worked before the path did. So this drives the real relay, the real
//! registry and the real interpreter.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::bridge::{BoxFuture, Controller, ControllerRegistry, Dispatcher, HostValue};
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;
use byard_core::render::{
    Layout, NativeProp, NativePropType, NativeProps, NativeView, NativeViewInfo, NativeViewMeta,
    RenderCtx, RequestKey,
};

/// A tile server that answers instantly, so the test's only wait is the round
/// trip itself.
struct Tiles {
    calls: Arc<AtomicUsize>,
}

impl Controller for Tiles {
    fn type_name(&self) -> &'static str {
        "Tiles"
    }

    fn invoke(
        &self,
        method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let out = match method {
            "fetch" => Ok(HostValue::Record(vec![(
                "tile".to_string(),
                args.into_iter().next().unwrap_or(HostValue::Unit),
            )])),
            other => Err(HostValue::Str(format!("no method {other}"))),
        };
        Box::pin(async move { out })
    }
}

/// What the view has been told, as a test can see it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Seen {
    /// Requests the view issued, by key.
    asked: Vec<u64>,
    /// Answers it received, as `(key, the record's `tile` field)`.
    answers: Vec<(u64, i64)>,
    /// Whether anything the view saw arrived off the logic thread. Set by
    /// comparing thread ids, because "only `Send` handles cross" is a claim
    /// about *where* code runs, not only about what it holds.
    off_thread: bool,
}

static SEEN: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A widget that asks for a tile the first time it draws, and remembers what
/// came back.
#[derive(Default)]
struct Map {
    zoom: i64,
    asked: bool,
    logic_thread: Option<std::thread::ThreadId>,
}

impl NativeProps for Map {
    fn set_prop(&mut self, name: &str, value: &HostValue) {
        if name == "zoom" {
            self.zoom = byard_core::bridge::FromHostValue::from_host(value.clone());
        }
    }
}

impl NativeView for Map {
    fn render(&mut self, _layout: Layout, cx: &mut RenderCtx<'_>) {
        self.logic_thread = Some(std::thread::current().id());
        if self.asked {
            return;
        }
        self.asked = true;
        let key = RequestKey(u64::try_from(self.zoom).unwrap_or(0));
        cx.call(
            key,
            "Tiles",
            "fetch",
            vec![HostValue::Int(i64::from(
                u32::try_from(self.zoom).unwrap_or(0),
            ))],
        );
        let mut seen = SEEN.lock().unwrap();
        seen.get_or_insert_with(Seen::default).asked.push(key.0);
    }

    fn on_result(&mut self, key: RequestKey, value: &HostValue) {
        let tile = match value {
            HostValue::Record(fields) => {
                fields
                    .iter()
                    .find(|(k, _)| k == "tile")
                    .map_or(-1, |(_, v)| match v {
                        HostValue::Int(n) => *n,
                        _ => -1,
                    })
            }
            _ => -1,
        };
        let mut seen = SEEN.lock().unwrap();
        let seen = seen.get_or_insert_with(Seen::default);
        seen.answers.push((key.0, tile));
        // The delivery must happen on the thread the view renders on: that is
        // what makes it safe for a view to touch its own graphics-adjacent
        // state here at all (INV-12, INV-2).
        if self.logic_thread != Some(std::thread::current().id()) {
            seen.off_thread = true;
        }
    }
}

impl NativeViewMeta for Map {
    const INFO: NativeViewInfo = NativeViewInfo {
        name: "TileMap",
        props: &[NativeProp {
            name: "zoom",
            ty: NativePropType::Int,
            layout: false,
        }],
        events: &[],
    };

    fn create() -> Box<dyn NativeView> {
        Box::new(Self::default())
    }
}

fn registered() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        byard_core::render::registry::register::<Map>();
    });
}

/// A running interpreter wired to a real relay, with the tile controller
/// provided unless the test asks for it to be missing.
struct Harness {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    relay: Relay,
    frame: RenderFrame,
    calls: Arc<AtomicUsize>,
}

impl Harness {
    fn new(source: &str, provide_tiles: bool) -> Self {
        registered();
        *SEEN.lock().unwrap() = Some(Seen::default());
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ControllerRegistry::new();
        if provide_tiles {
            registry.insert(Arc::new(Tiles {
                calls: Arc::clone(&calls),
            }));
        }
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

    /// Waits for a reply, applies it, and re-renders.
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

fn seen() -> Seen {
    SEEN.lock().unwrap().clone().unwrap_or_default()
}

const MAP: &str = "View Main() { TileMap #[zoom: 7, width: 100, height: 100] }";

#[test]
fn a_view_asks_a_controller_and_is_answered_on_the_logic_thread() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::new(MAP, true);

    assert_eq!(seen().asked, vec![7], "the view issued its request");

    assert!(harness.pump(), "the reply was applied");
    assert_eq!(
        harness.calls.load(Ordering::SeqCst),
        1,
        "and it reached the controller, once"
    );
    let seen = seen();
    assert_eq!(
        seen.answers,
        vec![(7, 7)],
        "the answer came back under the key the view chose"
    );
    assert!(
        !seen.off_thread,
        "a result must be delivered on the thread the view renders on (INV-12)"
    );
}

#[test]
fn a_request_is_placed_once_and_not_once_per_frame() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::new(MAP, true);
    for _ in 0..5 {
        harness.render();
    }
    harness.pump();
    for _ in 0..5 {
        harness.render();
    }
    assert_eq!(
        harness.calls.load(Ordering::SeqCst),
        1,
        "the view asked once; the frame must not re-place what it already sent"
    );
}

#[test]
fn a_view_that_unmounts_before_its_answer_drops_it_cleanly() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::new(MAP, true);
    assert_eq!(seen().asked, vec![7]);

    // Lower the tree again, which is what a hot reload or a structural change
    // does: every view is unmounted and a fresh one takes its place. The reply
    // still in flight belongs to a widget that no longer exists.
    let parsed = parse(MAP);
    let known: Vec<&str> = parsed.views.iter().map(|v| v.name.as_str()).collect();
    harness.tree = harness.interp.lower_view(&parsed.views[0], &known);
    harness.interp.tick();
    harness.render();

    // The fresh view asks for itself; drain both replies.
    for _ in 0..2 {
        harness.pump();
    }
    let seen = seen();
    assert_eq!(
        seen.answers.len(),
        1,
        "exactly one answer was delivered: the dead view's was discarded, not \
         handed to whoever took its slot ({seen:?})"
    );
    assert!(
        harness.interp.errors().is_empty(),
        "and quietly, because a \
         request outliving its view is the expected end of one"
    );
}

#[test]
fn a_controller_nobody_provided_is_reported_rather_than_silently_unanswered() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let harness = Harness::new(MAP, false);
    assert_eq!(
        harness.calls.load(Ordering::SeqCst),
        0,
        "there was nothing to call"
    );
    assert!(
        harness.interp.perf_warnings().iter().any(|w| matches!(
            w,
            byard_compiler::interp::eval::PerfWarning::UnprovidedNativeCall { controller, .. }
                if controller == "Tiles"
        )),
        "a widget waiting forever for an answer nobody will send has to be said out loud"
    );
    assert!(seen().answers.is_empty());
}

#[test]
fn the_view_never_blocks_waiting_for_its_answer() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The frame that issues the request completes without the answer: the
    // widget drew, the frame shipped, and the reply landed later. If `call`
    // ever awaited, this render would not return until the pool did.
    let mut harness = Harness::new(MAP, true);
    assert!(
        seen().answers.is_empty(),
        "the answer cannot already be here; the frame did not wait for it"
    );
    assert!(harness.pump());
    assert_eq!(seen().answers.len(), 1);
}
