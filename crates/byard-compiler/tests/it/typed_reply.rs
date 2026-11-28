//! `value as Type` on what a controller replies, end to end.
//!
//! The conformance suite cannot run a call that waits for a reply, so this is
//! where the half that needs one is checked: a reply of the shape the type
//! says is the value, and one that is not stops the `ok` arm and goes to the
//! call's `err` arm, with the path of the first field that does not fit.

use std::sync::Arc;

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::resolve::{PackageProvider, SourceFile};
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::cap::Store;
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "byard-typed-reply-{name}-{}-{}",
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

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(&mut self, _: &str, _: &str) -> Result<Vec<SourceFile>, String> {
        Err("no packages".to_string())
    }
}

struct Harness {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    relay: Relay,
    frame: RenderFrame,
}

impl Harness {
    fn new(source: &str, path: &std::path::Path) -> Self {
        // Resolved, as an app's program is: a type with parameters is made
        // into the types its uses name there.
        let parsed = byard_compiler::resolve::resolve_program(
            vec![SourceFile {
                name: "main.byd".to_string(),
                source: source.to_string(),
            }],
            &mut NoPackages,
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(Store::at(path)));
        registry.insert(Arc::new(byard_core::cap::Json));
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

    fn tap(&mut self) {
        use byard_core::platform::{EventKind, InputEvent};
        let down = InputEvent {
            surface: byard_core::SurfaceId::MAIN,
            kind: EventKind::PointerDown,
            pos: (50.0, 40.0),
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

/// Saves two todos, the second with a text where its `id` should be a number.
const SEED: &str = r#"
View Main() {
    inject Store as store
    Button("seed") #[width: 100, height: 40]
        => { store.set("todos", [{ id: 1, text: "a" }, { id: "two", text: "b" }]) }
}
"#;

fn seeded(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    let mut seed = Harness::new(SEED, &dir.file());
    seed.tap();
    seed.pump();
    dir
}

#[test]
fn a_reply_of_the_shape_the_type_says_is_the_value() {
    const VIEW: &str = r#"
type Todo = { id: Int, text: Str }
View Main() {
    inject Store as store
    var n = 0
    on mount => {
        store.get("todos", [])
            ok saved => { n = (saved as List<Todo>).len }
            err e => { n = -1 }
    }
    Text("{n}")
}
"#;
    let dir = TempDir::new("fits");
    let mut seed = Harness::new(
        r#"
View Main() {
    inject Store as store
    Button("seed") #[width: 100, height: 40]
        => { store.set("todos", [{ id: 1, text: "a" }, { id: 2, text: "b" }]) }
}
"#,
        &dir.file(),
    );
    seed.tap();
    seed.pump();
    drop(seed);
    let mut harness = Harness::new(VIEW, &dir.file());
    harness.pump();
    assert_eq!(harness.text(), "2");
}

#[test]
fn a_reply_of_another_shape_goes_to_the_err_arm_with_the_path() {
    const VIEW: &str = r#"
type Todo = { id: Int, text: Str }
View Main() {
    inject Store as store
    var what = "waiting"
    on mount => {
        store.get("todos", [])
            ok saved => { let todos = saved as List<Todo>  what = "ok" }
            err e => { what = "{e.kind} {e.path}" }
    }
    Text("{what}")
}
"#;
    let dir = seeded("path");
    let mut harness = Harness::new(VIEW, &dir.file());
    harness.pump();
    assert_eq!(
        harness.text(),
        "shape [1].id",
        "the arm stopped at the cast, and the err arm named where"
    );
}

#[test]
fn the_rest_of_the_ok_arm_does_not_run() {
    const VIEW: &str = r#"
type Todo = { id: Int, text: Str }
View Main() {
    inject Store as store
    var what = "start"
    on mount => {
        store.get("todos", [])
            ok saved => { let todos = saved as List<Todo>  what = "reached" }
            err e => { what = e.message }
    }
    Text("{what}")
}
"#;
    let dir = seeded("stops");
    let mut harness = Harness::new(VIEW, &dir.file());
    harness.pump();
    let said = harness.text();
    assert_ne!(said, "reached");
    assert!(said.contains("[1].id"), "{said}");
}

#[test]
fn a_call_with_no_err_arm_says_so() {
    const VIEW: &str = r#"
type Todo = { id: Int, text: Str }
View Main() {
    inject Store as store
    var n = 0
    on mount => {
        store.get("todos", [])
            ok saved => { n = (saved as List<Todo>).len }
    }
    Text("{n}")
}
"#;
    let dir = seeded("silent");
    let mut harness = Harness::new(VIEW, &dir.file());
    harness.pump();
    assert_eq!(harness.text(), "0");
    assert!(
        harness
            .interp
            .errors()
            .iter()
            .any(|e| matches!(e, byard_compiler::CompileError::ControllerCallFailed { .. })),
        "a reply of the wrong shape with nobody to tell is not silent: {:?}",
        harness.interp.errors()
    );
}

#[test]
fn a_reply_cast_to_a_type_with_a_parameter_is_compared_with_the_fields_it_gives() {
    const VIEW: &str = r#"
type Pair<T> = { id: T, text: Str }
View Main() {
    inject Store as store
    var what = "waiting"
    on mount => {
        store.get("todos", [])
            ok saved => { let todos = saved as List<Pair<Int>>  what = "ok {todos.len}" }
            err e => { what = "{e.kind} {e.path}" }
    }
    Text("{what}")
}
"#;
    let dir = seeded("generic");
    let mut harness = Harness::new(VIEW, &dir.file());
    harness.pump();
    assert_eq!(
        harness.text(),
        "shape [1].id",
        "`Pair<Int>` has an `id` that is an `Int`, and the second todo's is not"
    );
}

/// One run of `view`, its reply applied: what it shows.
fn shown_by(view: &str, name: &str) -> String {
    let dir = TempDir::new(name);
    let mut harness = Harness::new(view, &dir.file());
    harness.pump();
    harness.text()
}

#[test]
fn a_reply_cast_to_a_map_or_a_set_is_compared_and_becomes_one() {
    // JSON has records and lists; a `Map` and a `Set` are what the type says.
    let view = |json: &str, cast: &str, shown: &str| {
        format!(
            r#"
View Main() {{
    inject Json as json
    var what = "waiting"
    on mount => {{
        json.parse("{json}")
            ok v => {{
                let x = v as {cast}
                what = "{shown}"
            }}
            err e => {{ what = "{{e.kind}} {{e.path}}" }}
    }}
    Text("{{what}}")
}}
"#
        )
    };
    let map = view(
        r#"\{\"a\": 1, \"b\": 2\}"#,
        "Map<Str, Int>",
        r#"{x.len} {x["b"] ?? 0}"#,
    );
    assert_eq!(shown_by(&map, "map"), "2 2");
    let bad = view(r#"\{\"a\": 1, \"b\": \"two\"\}"#, "Map<Str, Int>", "ok");
    assert_eq!(shown_by(&bad, "bad-map"), "shape b");
    let set = view("[1, 2, 2, 3]", "Set<Int>", "{x.len} {x.contains(2)}");
    assert_eq!(shown_by(&set, "set"), "3 true");
}
