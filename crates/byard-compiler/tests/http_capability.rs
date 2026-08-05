//! `Http` and `Json` reached from `byld`, end to end (RFC-0029 O2/O3).
//!
//! The capability's own unit tests (in `byard-core`) prove the client speaks
//! HTTP. These prove the other half: that a `.byd` file can `inject` it, that a
//! JSON response lands in the reactive tree as a record, and that
//! `res.json.current.temperature_2m` reads it, which is the sentence the whole
//! RFC exists to make true.
//!
//! Everything runs against a loopback server, so the suite needs no network and
//! cannot fail because a public API was slow.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::bridge::{ControllerRegistry, Dispatcher};
use byard_core::cap::Http;
use byard_core::frame::RenderFrame;
use byard_core::relay::Relay;

/// A loopback server answering `count` requests with `body`, then stopping.
struct Server {
    port: u16,
}

impl Server {
    fn serve(status: &'static str, content_type: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self { port }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// An interpreter with `Http` pointed at a loopback server.
struct Harness {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    relay: Relay,
    frame: RenderFrame,
}

impl Harness {
    fn new(source: &str, base_url: &str) -> Self {
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let relay = Relay::new().expect("relay");
        let mut registry = ControllerRegistry::new();
        registry.insert(Arc::new(Http::with_base_url(base_url)));
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

    fn texts(&self) -> Vec<String> {
        self.frame.texts().iter().map(|t| t.text.clone()).collect()
    }

    /// Waits for the reply, applies it, re-renders.
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

/// The shape open-meteo answers with, trimmed to what the example reads.
const FORECAST: &str = r#"{"current":{"temperature_2m":21.4,"wind_speed_10m":9.2},
"daily":{"temperature_2m_max":[24.1],"temperature_2m_min":[15.3]}}"#;

const VIEW: &str = r#"
View Main() {
    inject Http as http
    var state: Str = "idle"
    var temp: Float = 0.0
    var high: Float = 0.0
    var message: Str = ""
    Column {
        Text("{state} {temp} {high} {message}")
        Button("go") #[width: 100, height: 40]
            => {
                state = "loading"
                http.get("/v1/forecast")
                    ok res => {
                        state = "ok"
                        temp = res.json.current.temperature_2m
                        high = res.json.daily.temperature_2m_max[0]
                    }
                    err e => { state = "error" message = e.kind }
            }
    }
}
"#;

fn tap(harness: &mut Harness) {
    use byard_core::platform::{EventKind, InputEvent};
    let down = InputEvent {
        kind: EventKind::PointerDown,
        pos: (50.0, 40.0),
        delta: (0.0, 0.0),
        payload: None,
        time_ms: 0,
    };
    let up = InputEvent {
        kind: EventKind::PointerUp,
        time_ms: 10,
        ..down.clone()
    };
    harness.interp.dispatch_events(&[down, up]);
    harness.interp.tick();
    harness.render();
}

#[test]
fn a_json_response_reads_as_a_record_all_the_way_into_a_text_line() {
    let server = Server::serve("200 OK", "application/json", FORECAST);
    let mut harness = Harness::new(VIEW, &server.base_url());

    tap(&mut harness);
    assert!(
        harness.texts()[0].starts_with("loading"),
        "the action returns before the answer: {:?}",
        harness.texts()
    );

    harness.pump();
    // `res.json.current.temperature_2m` and
    // `res.json.daily.temperature_2m_max[0]`, a nested field and an indexed
    // array, both read through the ordinary record/list path.
    assert_eq!(harness.texts()[0], "ok 21.4 24.1 ");
}

#[test]
fn a_server_error_reaches_the_err_arm_with_a_named_kind() {
    let server = Server::serve("503 Service Unavailable", "text/plain", "down");
    let mut harness = Harness::new(VIEW, &server.base_url());
    tap(&mut harness);
    harness.pump();
    assert_eq!(harness.texts()[0], "error 0 0 http_status");
}

#[test]
fn an_unreachable_host_reaches_the_err_arm_rather_than_hanging() {
    // Nothing listens on port 1, and the failure has to be a *named* one so
    // the view can say something other than "loading" forever.
    let mut harness = Harness::new(VIEW, "http://127.0.0.1:1");
    tap(&mut harness);
    harness.pump();
    let text = &harness.texts()[0];
    assert!(
        text.starts_with("error"),
        "expected the err arm to have run: {text}"
    );
}

#[test]
fn the_shipped_weather_example_compiles_and_injects_the_provided_capability() {
    // The example is pure `byld` and reaches the network only through the
    // capability the engine provides, so "does it still compile against the
    // capability set" is the thing that can rot, and this is what asserts it.
    const WEATHER: &str = include_str!("../../byard-cli/examples/weather/src/main.byd");
    let server = Server::serve("200 OK", "application/json", FORECAST);
    let mut harness = Harness::new(WEATHER, &server.base_url());
    harness.render();
    let hard: Vec<&byard_compiler::CompileError> = harness
        .interp
        .errors()
        .iter()
        .filter(|e| !e.is_warning())
        .collect();
    assert!(hard.is_empty(), "the example must render clean: {hard:?}");
    assert!(
        harness
            .texts()
            .iter()
            .any(|t| t.contains("weather over HTTP")),
        "{:?}",
        harness.texts()
    );
}

#[test]
fn a_narrower_viewport_marks_the_lines_whose_wrap_width_it_changed() {
    // The encoder hashes a line's *resolved* bytes, and its wrap width is one
    // of them: the same string wrapped at a different width is a different
    // glyph run and a different redraw region. A resize changes that width
    // without changing any `var`, so if the interpreter reports the line clean
    // the incremental scissor is built from a region that no longer describes
    // it.
    const WEATHER: &str = include_str!("../../byard-cli/examples/weather/src/main.byd");
    let server = Server::serve("200 OK", "application/json", FORECAST);
    let mut harness = Harness::new(WEATHER, &server.base_url());

    harness.frame.clear();
    harness
        .interp
        .render(&harness.tree, &mut harness.frame, 900.0, 600.0);
    let wide: Vec<(String, f32)> = harness
        .frame
        .texts()
        .iter()
        .map(|t| (t.text.clone(), t.x))
        .collect();

    harness.frame.clear();
    harness
        .interp
        .render(&harness.tree, &mut harness.frame, 500.0, 600.0);
    let narrow: Vec<(String, f32, bool)> = harness
        .frame
        .texts()
        .iter()
        .map(|t| (t.text.clone(), t.x, t.dirty))
        .collect();

    assert_eq!(
        wide.len(),
        narrow.len(),
        "the same lines, laid out narrower"
    );
    for (i, ((_, wide_x), (text, narrow_x, dirty))) in wide.iter().zip(narrow.iter()).enumerate() {
        if (wide_x - narrow_x).abs() > f32::EPSILON {
            assert!(
                *dirty,
                "line {i} ({text:?}) moved from x={wide_x} to x={narrow_x} with dirty unset"
            );
        }
    }
}
