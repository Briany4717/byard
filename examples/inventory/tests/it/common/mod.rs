//! The app, started the way it runs, with a store and a data folder of its own.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent, InputPayload};
use byard_core::relay::Relay;
use stockroom::Stockroom;

pub const W: f32 = 1100.0;
pub const H: f32 = 720.0;
/// What the clock says in every test.
pub const NOW: &str = "2026-11-05T10:30:00";

/// One integer of `ci/budgets.toml`. A missing file, section or key fails the
/// test and says which: a budget that is not read is not a budget.
pub fn ceiling(section: &str, key: &str) -> u64 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/budgets.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the budgets file {}: {e}", path.display()));
    let budgets: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("{} is not valid TOML: {e}", path.display()));
    budgets
        .get(section)
        .and_then(|s| s.get(key))
        .and_then(toml::Value::as_integer)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or_else(|| {
            panic!(
                "{} has no non-negative integer `{key}` under [{section}]",
                path.display()
            )
        })
}

/// A folder of its own for one test: the store file and the export go in it.
pub struct Sandbox {
    pub dir: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("stockroom-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary folder");
        Self { dir }
    }

    pub fn store(&self) -> PathBuf {
        self.dir.join("store.json")
    }

    /// The text of the store file, for a test that wants to see what was saved.
    pub fn saved(&self) -> String {
        std::fs::read_to_string(self.store()).unwrap_or_default()
    }
}

pub struct App {
    pub interp: Interpreter,
    tree: Vec<RenderNode>,
    pub frame: RenderFrame,
    relay: Relay,
    now: u32,
}

impl App {
    /// Starts the app on the data in `sandbox`, and lets its first answers
    /// arrive.
    pub fn start(sandbox: &Sandbox) -> Self {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = byard_project::manifest::Manifest::discover(Some(dir)).unwrap();
        let (program, _) = byard_project::deps::resolve_project(&manifest).unwrap();
        assert!(program.errors.is_empty(), "{:?}", program.errors);

        let relay = Relay::new().expect("relay");
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(byard_core::cap::Store::at(sandbox.store())));
        registry.insert(Arc::new(Stockroom::at(&sandbox.dir)));
        let dispatcher = Dispatcher::new(registry, relay.io_handle(), relay.io_result_sender());

        let mut interp = Interpreter::new();
        interp.set_theme(manifest.theme);
        interp.set_dispatcher(dispatcher);
        interp.set_wall_clock(NOW).expect("a fixed clock");
        interp.load_views(&program.views);
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        let root = byard_compiler::parser::ast::root_view(&program.views).unwrap();
        let tree = interp.lower_view(root, &known);
        assert!(interp.errors().is_empty(), "{:?}", interp.errors());
        let mut app = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            relay,
            now: 0,
        };
        app.step(&[]);
        app.answer_all();
        app
    }

    pub fn step(&mut self, events: &[InputEvent]) {
        self.interp.set_now_ms(self.now);
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame.clear();
        self.interp.render(&self.tree, &mut self.frame, W, H);
        self.now += 16;
    }

    /// Runs long enough for a transition to finish.
    pub fn settle(&mut self) {
        for _ in 0..40 {
            self.step(&[]);
        }
    }

    fn key_event(&self, kind: EventKind, payload: &str) -> InputEvent {
        InputEvent {
            kind,
            pos: (0.0, 0.0),
            delta: (0.0, 0.0),
            payload: Some(InputPayload::Key(payload.to_string())),
            time_ms: u64::from(self.now),
            mods: byard_core::KeyModifiers::default(),
        }
    }

    /// Types `text` into whatever has the focus.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let e = self.key_event(EventKind::TextInput, &c.to_string());
            self.step(&[e]);
        }
        self.answer_all();
        self.settle();
    }

    /// Presses and releases a key by name: `Tab`, `Enter`, `Escape`.
    pub fn key(&mut self, name: &str) {
        let down = self.key_event(EventKind::KeyDown, name);
        let up = self.key_event(EventKind::KeyUp, name);
        self.step(&[down, up]);
        self.answer_all();
        self.settle();
    }

    /// Selects everything in the focused field and types `text` over it.
    pub fn replace_text(&mut self, text: &str) {
        let mods = byard_core::KeyModifiers {
            ctrl: true,
            ..byard_core::KeyModifiers::default()
        };
        let mut e = self.key_event(EventKind::KeyDown, "a");
        e.mods = mods;
        let mut up = self.key_event(EventKind::KeyUp, "a");
        up.mods = mods;
        self.step(&[e, up]);
        self.type_text(text);
    }

    /// Presses and releases at a point of the window.
    pub fn tap_at(&mut self, pos: (f32, f32)) {
        let at = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
            mods: byard_core::KeyModifiers::default(),
        };
        let t = u64::from(self.now);
        self.step(&[
            at(EventKind::PointerDown, t),
            at(EventKind::PointerUp, t + 8),
        ]);
        self.answer_all();
        self.settle();
    }

    /// Waits for what the app asked of Rust or the store, applies the replies,
    /// and settles. Does nothing when nothing is outstanding.
    pub fn answer_all(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.interp.outstanding_continuations() > 0 {
            let mut results = Vec::new();
            while let Some(result) = self.relay.try_recv_io_result() {
                results.push(result);
            }
            if results.is_empty() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "no reply arrived within 5s"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            } else {
                let _ = self.interp.apply_io_results(results);
                for _ in 0..3 {
                    self.step(&[]);
                }
            }
        }
        self.settle();
    }

    pub fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    pub fn shows(&self, text: &str) -> bool {
        self.frame.texts().iter().any(|t| t.text == text)
    }

    /// The position of the last text drawn as `label`.
    pub fn find(&self, label: &str) -> (f32, f32) {
        let line = self
            .frame
            .texts()
            .iter()
            .rev()
            .find(|t| t.text == label)
            .unwrap_or_else(|| panic!("a text {label:?}: {:?}", self.texts()));
        (line.x + 4.0, line.y + line.font_size * 0.5)
    }

    /// Taps the last text drawn as `label`, wherever the layout put it. A
    /// dialog is drawn after the page, so its words win over the page's.
    pub fn tap(&mut self, label: &str) {
        let pos = self.find(label);
        self.tap_at(pos);
    }

    /// Taps the first text drawn as `label`: a filter chip, which the rows
    /// below it (drawn later) may repeat as a category.
    pub fn tap_first(&mut self, label: &str) {
        let line = self
            .frame
            .texts()
            .iter()
            .find(|t| t.text == label)
            .unwrap_or_else(|| panic!("a text {label:?}: {:?}", self.texts()))
            .clone();
        self.tap_at((line.x + 4.0, line.y + line.font_size * 0.5));
    }

    /// Taps `label` and returns how long the first frame after the press took
    /// (the screen it opens being built and laid out), then settles.
    pub fn tap_timed(&mut self, label: &str) -> std::time::Duration {
        let pos = self.find(label);
        let at = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
            mods: byard_core::KeyModifiers::default(),
        };
        let t = u64::from(self.now);
        let start = std::time::Instant::now();
        self.step(&[
            at(EventKind::PointerDown, t),
            at(EventKind::PointerUp, t + 8),
        ]);
        let first = start.elapsed();
        self.answer_all();
        first
    }

    /// Types one character and returns how long the frame that follows took,
    /// with nothing else done, which is what a keystroke costs.
    pub fn keystroke(&mut self, c: char) -> std::time::Duration {
        let e = self.key_event(EventKind::TextInput, &c.to_string());
        let start = std::time::Instant::now();
        self.step(&[e]);
        start.elapsed()
    }

    /// Every solid box as painted, `[x, y, w, h]`.
    pub fn boxes(&self) -> Vec<[f32; 4]> {
        self.frame
            .instances()
            .iter()
            .map(|b| {
                [
                    b.rect[0] + b.transform.translate[0],
                    b.rect[1] + b.transform.translate[1],
                    b.rect[2],
                    b.rect[3],
                ]
            })
            .collect()
    }

    /// The text drawn at or after `start` that begins with `prefix`, the way a
    /// count like "12 of 60" is looked for.
    pub fn starting(&self, prefix: &str) -> Option<String> {
        self.texts().into_iter().find(|t| t.starts_with(prefix))
    }

    /// The vertical order of the given texts on screen, top first.
    pub fn order_of(&self, labels: &[&str]) -> Vec<String> {
        let mut found: Vec<(f32, String)> = labels
            .iter()
            .filter_map(|l| {
                self.frame
                    .texts()
                    .iter()
                    .find(|t| t.text == *l)
                    .map(|t| (t.y, (*l).to_string()))
            })
            .collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.into_iter().map(|(_, l)| l).collect()
    }
}

/// A store file holding `n` generated products and no movements, as a first
/// run would have left it.
pub fn write_store_with(sandbox: &Sandbox, n: usize) {
    let cats = [
        "Hardware",
        "Electronics",
        "Office",
        "Tools",
        "Safety",
        "Packaging",
    ];
    let products: Vec<String> = (1..=n)
        .map(|i| {
            format!(
                r#"{{"id":{i},"sku":"SKU-{i:05}","name":"Item {i:05}","category":"{}","qty":{},"min":10,"price":{}.5,"location":"A1","updated":"2026-10-01T09:00"}}"#,
                cats[i % 6],
                (i * 7) % 40,
                i % 90
            )
        })
        .collect();
    let json = format!(
        r#"{{"seeded":true,"products":[{}],"movements":[],"minDefault":5}}"#,
        products.join(",")
    );
    std::fs::write(sandbox.store(), json).expect("write the store");
}
