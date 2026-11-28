//! `store Name persist "key" { … }`, end to end: what one run of a program
//! writes into a store is what the next run, a new interpreter over the same
//! file, starts with.
//!
//! The conformance suite runs one program once, with no `Store` behind it, so
//! keeping a store between runs is checked here, with the app's real `Store`
//! capability writing a file in a directory of the test's own.

use std::sync::Arc;

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::resolve::{PackageProvider, SourceFile};
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::cap::Store;
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(&mut self, _: &str, _: &str) -> Result<Vec<SourceFile>, String> {
        Err("no packages".to_string())
    }
}

struct Dir(std::path::PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "byard-store-persist-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        )))
    }

    fn file(&self) -> std::path::PathBuf {
        self.0.join("store.json")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn resolve(source: &str) -> Vec<byard_compiler::parser::ast::ViewDecl> {
    let program = byard_compiler::resolve::resolve_program(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: source.to_string(),
        }],
        &mut NoPackages,
    );
    assert!(program.errors.is_empty(), "{:?}", program.errors);
    program.views
}

/// One run of a program: mounted, its replies applied, its frame drawn.
struct Run {
    views: Vec<byard_compiler::parser::ast::ViewDecl>,
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    relay: Relay,
    frame: RenderFrame,
}

impl Run {
    fn start(source: &str, path: &std::path::Path) -> Self {
        let views = resolve(source);
        let relay = Relay::new().expect("relay");
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(Store::at(path)));
        let dispatcher = Dispatcher::new(registry, relay.io_handle(), relay.io_result_sender());
        let mut interp = Interpreter::new();
        interp.set_dispatcher(dispatcher);
        interp.load_views(&views);
        let known: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        let main = views
            .iter()
            .find(|v| v.name.as_str() == "Main")
            .expect("Main");
        let tree = interp.lower_view(main, &known);
        interp.tick();
        let mut run = Self {
            views: views.clone(),
            interp,
            tree,
            relay,
            frame: RenderFrame::new(),
        };
        run.render();
        run.settle();
        run
    }

    fn render(&mut self) {
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 800.0, 600.0);
    }

    /// Applies replies until every call the program made has its answer.
    fn settle(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let mut results = Vec::new();
            while let Some(result) = self.relay.try_recv_io_result() {
                results.push(result);
            }
            if !results.is_empty() {
                let _ = self.interp.apply_io_results(results);
                self.interp.tick();
                self.render();
            } else if self.interp.outstanding_continuations() == 0 {
                return;
            } else {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        panic!("the program's calls were not all answered within 5s");
    }

    /// What `byard dev` does when the file is saved again as `source`.
    fn reload(&mut self, source: &str) {
        let new = resolve(source);
        if let Some(tree) =
            byard_compiler::interp::reload::apply_program(&mut self.interp, &self.views, &new)
        {
            self.tree = tree;
        }
        self.views = new;
        self.interp.tick();
        self.render();
        self.settle();
    }

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    fn tap(&mut self, x: f32, y: f32) {
        use byard_core::platform::{EventKind, InputEvent};
        let down = InputEvent {
            surface: byard_core::SurfaceId::MAIN,
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
        self.settle();
    }
}

const APP: &str = r#"
type User = { name: Str }
store Settings persist "settings" { dark: Bool = false, size: Int = 14, user: User? = none }

View Main() {
    inject Settings as s
    Column {
        Button("toggle") #[width: 200, height: 40] => {
            s.dark = !s.dark
            s.size += 2
            s.user = { name: "Ada" }
        }
        Text("{s.dark} {s.size} {s.user?.name ?? "nobody"}")
    }
}
"#;

#[test]
fn what_a_run_writes_is_what_the_next_run_starts_with() {
    let dir = Dir::new("round-trip");
    let mut first = Run::start(APP, &dir.file());
    assert!(
        first.texts().contains(&"false 14 nobody".to_string()),
        "{:?}",
        first.texts()
    );
    first.tap(20.0, 20.0);
    assert!(
        first.texts().contains(&"true 16 Ada".to_string()),
        "{:?}",
        first.texts()
    );
    drop(first);

    let second = Run::start(APP, &dir.file());
    assert!(
        second.texts().contains(&"true 16 Ada".to_string()),
        "the second run reads what the first wrote: {:?}",
        second.texts()
    );
}

#[test]
fn a_field_the_saved_store_lacks_or_holds_as_something_else_keeps_its_default() {
    let dir = Dir::new("older");
    // What an older version of the app saved: no `user`, and a `size` that is
    // text.
    let writer = r#"
View Main() {
    inject Store as store
    on mount => { store.set("settings", { dark: true, size: "big" }) }
    Text("x")
}
"#;
    drop(Run::start(writer, &dir.file()));
    let run = Run::start(APP, &dir.file());
    assert!(
        run.texts().contains(&"true 14 nobody".to_string()),
        "`dark` is read, and the others keep their defaults: {:?}",
        run.texts()
    );
}

const COUNTER: &str = r#"
store Count { n: Int = 0 }

View Main() {
    inject Count as c
    Column {
        Button("more") #[width: 200, height: 40] => { c.n += 1 }
        Text("n is {c.n}")
    }
}
"#;

#[test]
fn a_store_keeps_what_it_holds_across_a_hot_reload() {
    let dir = Dir::new("reload");
    let mut run = Run::start(COUNTER, &dir.file());
    run.tap(20.0, 20.0);
    run.tap(20.0, 20.0);
    assert!(
        run.texts().contains(&"n is 2".to_string()),
        "{:?}",
        run.texts()
    );
    // A save that changes the text and not the store keeps what it holds.
    run.reload(&COUNTER.replace("n is", "count:"));
    assert!(
        run.texts().contains(&"count: 2".to_string()),
        "{:?}",
        run.texts()
    );
    // A save that changes the store's fields starts it again.
    run.reload(&COUNTER.replace("{ n: Int = 0 }", "{ n: Int = 0, m: Int = 7 }"));
    assert!(
        run.texts().contains(&"n is 0".to_string()),
        "{:?}",
        run.texts()
    );
}
