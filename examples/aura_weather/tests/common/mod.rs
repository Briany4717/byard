//! The app, driven the way it runs: the project is read from this directory,
//! the program goes through the interpreter, `Http` answers from a local
//! server rather than the network, and taps land where a person would tap.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use byard_compiler::interp::eval::{Interpreter, RenderNode};
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::cap::Http;
use byard_core::frame::RenderFrame;
use byard_core::platform::{EventKind, InputEvent};
use byard_core::relay::Relay;

pub const W: f32 = 440.0;
pub const H: f32 = 852.0;

/// A local server that answers every request with one response and keeps
/// the request lines it saw.
pub struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub fn serve(status: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                let mut first = true;
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if first {
                        log.lock().unwrap().push(line.trim_end().to_string());
                        first = false;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self { port, seen }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The request lines received so far, like `GET /v1/forecast?... HTTP/1.1`.
    pub fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
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
    /// Starts the app with its `Http` answering from `server`.
    pub fn start(server: &Server) -> Self {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = byard_project::manifest::Manifest::discover(Some(dir)).unwrap();
        assert_eq!(
            manifest.http_base_url.as_deref(),
            Some("https://api.open-meteo.com"),
            "the app itself talks to open-meteo; only the tests swap the host"
        );
        let (program, _) = byard_project::deps::resolve_project(&manifest).unwrap();
        assert!(program.errors.is_empty(), "{:?}", program.errors);

        let relay = Relay::new().expect("relay");
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(Http::with_base_url(server.base_url())));
        let dispatcher = Dispatcher::new(registry, relay.io_handle(), relay.io_result_sender());

        let mut interp = Interpreter::new();
        interp.set_theme(manifest.theme);
        interp.set_dispatcher(dispatcher);
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
        app
    }

    pub fn step(&mut self, events: &[InputEvent]) {
        self.interp.set_now_ms(self.now);
        self.interp.dispatch_events(events);
        self.interp.tick();
        self.frame = RenderFrame::new();
        self.interp.render(&self.tree, &mut self.frame, W, H);
        self.now += 16;
    }

    /// Runs long enough for a transition to finish.
    pub fn settle(&mut self) {
        for _ in 0..60 {
            self.step(&[]);
        }
    }

    /// Waits for the next reply from the server, applies it and settles.
    pub fn answer(&mut self) {
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
        self.settle();
    }

    pub fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    pub fn shows(&self, text: &str) -> bool {
        self.frame.texts().iter().any(|t| t.text == text)
    }

    /// Taps the last text drawn as `label`, wherever the layout put it. The
    /// tab bar is drawn after the screen, so a tab wins over a screen text
    /// with the same words.
    pub fn tap(&mut self, label: &str) {
        let line = self
            .frame
            .texts()
            .iter()
            .rev()
            .find(|t| t.text == label)
            .unwrap_or_else(|| panic!("a text {label}: {:?}", self.texts()));
        let pos = (line.x + 4.0, line.y + line.font_size * 0.5);
        let at = |kind, t| InputEvent {
            kind,
            pos,
            delta: (0.0, 0.0),
            payload: None,
            time_ms: t,
        };
        let t = u64::from(self.now);
        self.step(&[
            at(EventKind::PointerDown, t),
            at(EventKind::PointerUp, t + 8),
        ]);
        self.settle();
    }
}
