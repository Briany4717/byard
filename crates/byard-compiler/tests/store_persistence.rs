//! `Store` reached from `byld`, end to end (RFC-0029 O5).
//!
//! The capability's own tests (in `byard-core`) prove the file survives a
//! restart. These prove the half that only exists once both RFCs are wired:
//! that a view can ask for its saved state on mount, get it back as a reactive
//! `var`, and write it again from an ordinary action, which is the whole
//! sentence "the todo list is still there tomorrow".

use std::sync::Arc;

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::cap::Store;
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;

/// A store file in a fresh temporary directory, removed with the test.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "byard-store-e2e-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        Self(dir)
    }

    fn file(&self) -> std::path::PathBuf {
        self.0.join("store.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An interpreter whose `Store` writes to `path`.
struct Harness {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    relay: Relay,
    frame: RenderFrame,
}

impl Harness {
    fn new(source: &str, path: &std::path::Path) -> Self {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(Store::at(path)));
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

    /// Waits for a reply, applies it, re-renders.
    fn pump(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut results = Vec::new();
        while results.is_empty() && std::time::Instant::now() < deadline {
            while let Some(result) = self.relay.try_recv_io_result() {
                results.push(result);
            }
            if results.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        assert!(!results.is_empty(), "no reply arrived within 5s");
        let _ = self.interp.apply_io_results(results);
        self.interp.tick();
        self.render();
    }
}

const VIEW: &str = r#"
View Main() {
    inject Store as store
    var label: Str = "unloaded"
    on mount => {
        store.get("label")
            ok saved => { label = saved }
            err e => { label = e.kind }
    }
    Column {
        Text("{label}")
        Button("save") #[width: 100, height: 40]
            => { label = "kept"  store.set("label", "kept") }
    }
}
"#;

#[test]
fn a_view_reads_what_a_previous_run_wrote() {
    // A restart, modelled honestly: a whole second interpreter over the same
    // file, with no shared state but the bytes on disk.
    let dir = TempDir::new("restart");

    let mut first = Harness::new(VIEW, &dir.file());
    first.pump(); // the mount `get`, on an empty store
    assert_eq!(
        first.text(),
        "",
        "a missing key comes back as Unit, not an error"
    );
    first.tap(50.0, 40.0);
    assert_eq!(first.text(), "kept", "the action wrote the var immediately");
    first.pump(); // the `set` reply
    drop(first);

    let mut second = Harness::new(VIEW, &dir.file());
    second.pump();
    assert_eq!(
        second.text(),
        "kept",
        "the next run read what the previous one wrote"
    );
}

#[test]
fn a_missing_key_is_not_an_error_arm() {
    // The first run of every app that saves anything. If this took the `err`
    // arm, every app would need to special-case its own first launch.
    let dir = TempDir::new("missing");
    let mut harness = Harness::new(VIEW, &dir.file());
    harness.pump();
    assert_eq!(harness.text(), "");
}

#[test]
fn a_corrupt_store_reaches_the_err_arm_and_the_app_still_runs() {
    // INV-4: a settings file truncated by a crash must not stop the app.
    let dir = TempDir::new("corrupt");
    std::fs::create_dir_all(&dir.0).expect("mkdir");
    std::fs::write(dir.file(), "{ not json").expect("write");

    let mut harness = Harness::new(VIEW, &dir.file());
    harness.pump();
    assert_eq!(harness.text(), "storage", "the err arm named the failure");

    // And the app keeps working: a write after the corruption lands.
    harness.tap(50.0, 40.0);
    harness.pump();
    let mut reopened = Harness::new(VIEW, &dir.file());
    reopened.pump();
    assert_eq!(reopened.text(), "kept");
}

#[test]
fn a_list_of_records_survives_the_round_trip_through_the_view() {
    // The motivating shape: a todo list is a list of records, and the nesting
    // has to come back as records the view can read fields off, not as text.
    const TODOS: &str = r#"
View Main() {
    inject Store as store
    var todos: List = []
    on mount => { store.get("todos", []) ok saved => { todos = saved } }
    Column {
        Text("{todos.len} · {todos.filter(t => !t.done).len} left")
        Button("seed") #[width: 100, height: 40]
            => {
                todos = [{ id: 1, text: "a", done: false }, { id: 2, text: "b", done: true }]
                store.set("todos", todos)
            }
    }
}
"#;
    let dir = TempDir::new("todos");
    let mut first = Harness::new(TODOS, &dir.file());
    first.pump();
    first.tap(50.0, 40.0);
    first.pump();
    assert_eq!(first.text(), "2 · 1 left");
    drop(first);

    let mut second = Harness::new(TODOS, &dir.file());
    second.pump();
    assert_eq!(
        second.text(),
        "2 · 1 left",
        "the records came back as records, with their fields readable"
    );
}

#[test]
fn the_persistent_todo_example_loads_on_mount() {
    const EXAMPLE: &str = include_str!("../../byard-cli/examples/persistent_todo/src/main.byd");
    let dir = TempDir::new("example");
    let mut harness = Harness::new(EXAMPLE, &dir.file());
    let hard: Vec<&byard_compiler::CompileError> = harness
        .interp
        .errors()
        .iter()
        .filter(|e| !e.is_warning())
        .collect();
    assert!(hard.is_empty(), "the example must render clean: {hard:?}");

    // Two outstanding, with no input at all: the `on mount` `get`, and the
    // `after 400ms` timer the example arms to name its own file.
    assert_eq!(harness.interp.outstanding_continuations(), 2);
    harness.pump();
    assert!(
        harness
            .frame
            .texts()
            .iter()
            .any(|t| t.text.contains("0 of 0 left")),
        "an empty store renders an empty list: {:?}",
        harness
            .frame
            .texts()
            .iter()
            .map(|t| &t.text)
            .collect::<Vec<_>>()
    );
}
