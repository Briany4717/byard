//! The first-party `Http` capability (RFC-0029 O2).
//!
//! ```byld
//! inject Http as http
//! on mount => http.get("https://example.com/weather?q=Tokyo")
//!     ok  res => { report = res.json }
//!     err e   => { message = e.message }
//! ```
//!
//! Three methods, one response shape:
//!
//! ```text
//! http.get(url)            -> { status: Int, ok: Bool, body: Str, json: Record|List|Unit }
//! http.post(url, body)     -> same
//! http.request(record)     -> same, with full control over method/headers/body/timeout
//! ```
//!
//! ## Why the framework ships an HTTP client at all
//!
//! Because a weather app should work from `byard new` without the developer
//! wiring reqwest by hand, and because the alternative is that every app writes
//! the same fifty lines of controller and gets the timeout wrong. `Http` is an
//! ordinary [`Controller`]: it holds no privileged position, and an app that
//! wants auth, retries, caching or gRPC writes its own and never injects this
//! one. A convenience floor, not a ceiling.
//!
//! ## Why a non-2xx is an error and not a response
//!
//! Both readings are defensible and the language has to pick one. A view that
//! writes `ok res => { data = res.json }` has said what it wants to happen when
//! the request worked; silently binding a 404's error page into `data` is not
//! that. Sending it to `err`, with the `status` still on the record, means the
//! common case is right by default and the uncommon one (a caller that really
//! wants to branch on `404` itself) reads `e.status`.
//!
//! ## Why rustls
//!
//! `unsafe_code = "deny"` holds all the way down, there is no OpenSSL to find
//! at build time and no platform certificate store to differ at run time, and
//! the build is identical on every OS. Roots come from `webpki-roots`, bundled,
//! so a machine with an empty system trust store still works.

use std::sync::OnceLock;
use std::time::Duration;

use crate::bridge::{BoxFuture, Controller, HostValue};
use crate::cap::json;

/// Default per-request timeout (RFC-0029, resolved question "HTTP defaults").
///
/// Long enough for a slow mobile network, short enough that a hung request
/// eventually reaches the caller's `err` arm instead of leaving a spinner up
/// forever. `http.request({ timeout_ms: … })` overrides it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum redirects followed before the request fails.
const MAX_REDIRECTS: usize = 10;

/// Idle connections kept alive per host, so a view that polls one endpoint
/// reuses its connection instead of re-handshaking TLS every refresh.
const POOL_IDLE_PER_HOST: usize = 4;

/// The `Http` capability (RFC-0029 O2).
///
/// The `reqwest::Client` inside is built **once** and shared: it owns the
/// connection pool and the TLS session cache, so constructing one per request
/// would re-handshake every time and quietly turn a 40 ms refresh into a 300 ms
/// one. Cloning `Http` clones a handle to the same client.
#[derive(Debug, Default, Clone)]
pub struct Http {
    /// Prepended to any request path that is not already absolute, so an app
    /// can write `http.get("/weather?q=Tokyo")`. Empty by default.
    base_url: String,
}

impl Http {
    /// A capability with no base URL: every request must be an absolute URL.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A capability whose relative paths resolve against `base_url`.
    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    /// The process-wide client.
    ///
    /// A `OnceLock` rather than a field because the client is stateless from
    /// the app's point of view and expensive to build (it constructs the TLS
    /// root store), and because two `Http` values, the default one and an
    /// app's `with_base_url`, should share one connection pool rather than
    /// competing for sockets.
    fn client() -> Option<&'static reqwest::Client> {
        static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
        CLIENT
            .get_or_init(|| {
                reqwest::Client::builder()
                    .timeout(DEFAULT_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                    .pool_max_idle_per_host(POOL_IDLE_PER_HOST)
                    .build()
                    .ok()
            })
            .as_ref()
    }

    /// Resolves `url` against the base URL, if it is not already absolute.
    fn resolve(&self, url: &str) -> String {
        if self.base_url.is_empty() || url.starts_with("http://") || url.starts_with("https://") {
            return url.to_string();
        }
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            url.trim_start_matches('/')
        )
    }
}

/// One request, already normalised from whichever method built it.
struct Request {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    timeout: Duration,
}

impl Request {
    /// Reads the `http.request({ … })` record form (RFC-0029 §3).
    fn from_record(http: &Http, record: &HostValue) -> Result<Self, HostValue> {
        let url = match record.field("url") {
            Some(HostValue::Str(url)) => http.resolve(url),
            _ => {
                return Err(json::error(
                    "bad_request",
                    "`http.request` needs a `url` field",
                ));
            }
        };
        let method = match record.field("method") {
            Some(HostValue::Str(m)) => m.to_uppercase(),
            _ => "GET".to_string(),
        };
        let headers = match record.field("headers") {
            Some(HostValue::Record(fields)) => fields
                .iter()
                .map(|(k, v)| (k.clone(), scalar_text(v)))
                .collect(),
            _ => Vec::new(),
        };
        // A record body is serialised as JSON, because that is what the caller
        // meant by writing a record; a string body is sent verbatim, because
        // that is what the caller meant by writing a string.
        let body = match record.field("body") {
            None | Some(HostValue::Unit) => None,
            Some(HostValue::Str(s)) => Some(s.clone()),
            Some(other) => Some(json::host_to_json(other).to_string()),
        };
        let timeout = match record.field("timeout_ms") {
            Some(HostValue::Int(ms)) if *ms > 0 => {
                Duration::from_millis(u64::try_from(*ms).unwrap_or(u64::MAX))
            }
            _ => DEFAULT_TIMEOUT,
        };
        Ok(Self {
            method,
            url,
            headers,
            body,
            timeout,
        })
    }

    /// Performs the request and shapes the reply.
    async fn send(self) -> Result<HostValue, HostValue> {
        let Some(client) = Http::client() else {
            return Err(json::error(
                "client_unavailable",
                "the HTTP client could not be initialised (TLS roots unavailable)",
            ));
        };
        let method = reqwest::Method::from_bytes(self.method.as_bytes()).map_err(|_| {
            json::error(
                "bad_request",
                &format!("`{}` is not an HTTP method", self.method),
            )
        })?;

        let mut builder = client.request(method, &self.url).timeout(self.timeout);
        // A record body implies JSON unless the caller said otherwise, so the
        // common case needs no `headers` at all.
        let has_content_type = self
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = self.body {
            if !has_content_type {
                builder = builder.header("content-type", "application/json");
            }
            builder = builder.body(body);
        }

        let response = builder.send().await.map_err(|e| transport_error(&e))?;
        let status = i64::from(response.status().as_u16());
        let ok = response.status().is_success();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response.text().await.map_err(|e| transport_error(&e))?;

        // Parsed when the server said JSON. A body that claims to be JSON and
        // is not leaves `json` as `Unit` with `body` intact, rather than
        // failing the whole request: the caller can still read what arrived,
        // which is the only way to debug a server that lies about its type
        // (INV-4).
        let parsed = if content_type.contains("json") {
            json::parse(&body).unwrap_or(HostValue::Unit)
        } else {
            HostValue::Unit
        };

        let record = HostValue::Record(vec![
            ("status".to_string(), HostValue::Int(status)),
            ("ok".to_string(), HostValue::Bool(ok)),
            ("body".to_string(), HostValue::Str(body)),
            ("json".to_string(), parsed),
        ]);
        if ok {
            Ok(record)
        } else {
            // The status and the body ride along, so a caller that does want to
            // branch on `404` has everything it needs on the error record.
            Err(HostValue::Record(vec![
                (
                    "kind".to_string(),
                    HostValue::Str("http_status".to_string()),
                ),
                (
                    "message".to_string(),
                    HostValue::Str(format!("the server answered {status}")),
                ),
                ("status".to_string(), HostValue::Int(status)),
                (
                    "body".to_string(),
                    record.field("body").cloned().unwrap_or(HostValue::Unit),
                ),
                (
                    "json".to_string(),
                    record.field("json").cloned().unwrap_or(HostValue::Unit),
                ),
            ]))
        }
    }
}

/// Maps a transport failure onto the standard error record, naming *which*
/// kind of failure so a view can tell "you are offline" from "that host does
/// not exist" from "it took too long".
fn transport_error(error: &reqwest::Error) -> HostValue {
    let kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_decode() {
        "decode"
    } else if error.is_builder() {
        "bad_request"
    } else {
        "transport"
    };
    json::error(kind, &error.to_string())
}

/// A header value written as any scalar, rendered the way `Text` would render
/// it, so `#[headers: { "x-count": 3 }]` does not need a manual conversion.
fn scalar_text(value: &HostValue) -> String {
    match value {
        HostValue::Str(s) => s.clone(),
        HostValue::Int(n) => n.to_string(),
        HostValue::Float(f) => f.to_string(),
        HostValue::Bool(b) => b.to_string(),
        other => json::host_to_json(other).to_string(),
    }
}

impl Controller for Http {
    fn type_name(&self) -> &'static str {
        "Http"
    }

    fn invoke(
        &self,
        method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
        let mut args = args.into_iter();
        let request = match method {
            "get" => match args.next() {
                Some(HostValue::Str(url)) => Ok(Request {
                    method: "GET".to_string(),
                    url: self.resolve(&url),
                    headers: Vec::new(),
                    body: None,
                    timeout: DEFAULT_TIMEOUT,
                }),
                _ => Err(json::error("bad_argument", "`http.get` takes a URL string")),
            },
            "post" => match args.next() {
                Some(HostValue::Str(url)) => Ok(Request {
                    method: "POST".to_string(),
                    url: self.resolve(&url),
                    headers: Vec::new(),
                    body: match args.next() {
                        None | Some(HostValue::Unit) => None,
                        Some(HostValue::Str(s)) => Some(s),
                        Some(other) => Some(json::host_to_json(&other).to_string()),
                    },
                    timeout: DEFAULT_TIMEOUT,
                }),
                _ => Err(json::error(
                    "bad_argument",
                    "`http.post` takes a URL string",
                )),
            },
            "request" => match args.next() {
                Some(record @ HostValue::Record(_)) => Request::from_record(self, &record),
                _ => Err(json::error(
                    "bad_argument",
                    "`http.request` takes a record with at least a `url`",
                )),
            },
            other => Err(json::error(
                "unknown_method",
                &format!("`Http` has no method `{other}`; try get, post or request"),
            )),
        };
        Box::pin(async move {
            match request {
                Ok(request) => request.send().await,
                Err(error) => Err(error),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A one-shot HTTP server on a loopback port.
    ///
    /// Hand-written rather than pulled from a mock-server crate: the tests need
    /// to answer with a *deliberately wrong* content type and a *deliberately
    /// malformed* body, which is exactly what a polite mock library normalises
    /// away, and those are the two cases INV-4 is about.
    struct Server {
        port: u16,
        handle: Option<std::thread::JoinHandle<Vec<String>>>,
    }

    impl Server {
        /// Serves `responses` in order, one per connection, then stops.
        fn serve(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("addr").port();
            let handle = std::thread::spawn(move || {
                let mut seen = Vec::new();
                for response in responses {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    // Read the request head so the client is not left waiting,
                    // and record the request line for assertions.
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut request = String::new();
                    let mut line = String::new();
                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                        if line == "\r\n" {
                            break;
                        }
                        request.push_str(&line);
                        line.clear();
                    }
                    seen.push(request);
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
                seen
            });
            Self {
                port,
                handle: Some(handle),
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }

        /// The request heads the server received, once it has stopped.
        fn requests(mut self) -> Vec<String> {
            self.handle
                .take()
                .expect("served once")
                .join()
                .expect("server thread")
        }
    }

    /// Builds an HTTP/1.1 response with an explicit content type.
    fn response(status: &str, content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn call(http: &Http, method: &str, args: Vec<HostValue>) -> Result<HostValue, HostValue> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(http.invoke(method, args))
    }

    #[test]
    fn a_json_response_arrives_parsed_and_ready_to_read() {
        let server = Server::serve(vec![response(
            "200 OK",
            "application/json",
            r#"{"tempC": 21, "city": "Tokyo"}"#,
        )]);
        let result = call(&Http::new(), "get", vec![HostValue::Str(server.url("/wx"))])
            .expect("a 200 is a success");

        assert_eq!(result.field("status"), Some(&HostValue::Int(200)));
        assert_eq!(result.field("ok"), Some(&HostValue::Bool(true)));
        let parsed = result.field("json").expect("json field");
        assert_eq!(parsed.field("tempC"), Some(&HostValue::Int(21)));
        assert_eq!(parsed.field("city"), Some(&HostValue::Str("Tokyo".into())));
    }

    #[test]
    fn a_non_json_response_leaves_json_unit_with_the_body_intact() {
        let server = Server::serve(vec![response("200 OK", "text/plain", "plain words")]);
        let result = call(&Http::new(), "get", vec![HostValue::Str(server.url("/t"))])
            .expect("a 200 is a success");
        assert_eq!(result.field("json"), Some(&HostValue::Unit));
        assert_eq!(
            result.field("body"),
            Some(&HostValue::Str("plain words".into()))
        );
    }

    #[test]
    fn a_body_that_lies_about_being_json_does_not_fail_the_request() {
        // INV-4 on data the app does not control. The caller can still read
        // `body`, which is the only way to debug a server like this.
        let server = Server::serve(vec![response("200 OK", "application/json", "{not json")]);
        let result = call(&Http::new(), "get", vec![HostValue::Str(server.url("/x"))])
            .expect("the request itself succeeded");
        assert_eq!(result.field("json"), Some(&HostValue::Unit));
        assert_eq!(
            result.field("body"),
            Some(&HostValue::Str("{not json".into()))
        );
    }

    #[test]
    fn a_500_reaches_the_err_arm_carrying_its_status_and_body() {
        let server = Server::serve(vec![response(
            "500 Internal Server Error",
            "application/json",
            r#"{"detail": "boom"}"#,
        )]);
        let error = call(&Http::new(), "get", vec![HostValue::Str(server.url("/e"))])
            .expect_err("a 500 is a failure");
        assert_eq!(
            error.field("kind"),
            Some(&HostValue::Str("http_status".into()))
        );
        assert_eq!(error.field("status"), Some(&HostValue::Int(500)));
        // The parsed body rides along, so a caller that wants the server's own
        // explanation has it.
        assert_eq!(
            error.field("json").and_then(|j| j.field("detail")),
            Some(&HostValue::Str("boom".into()))
        );
    }

    #[test]
    fn a_refused_connection_is_a_transport_error_not_a_panic() {
        // Port 1 on loopback: nothing is listening, and nothing may.
        let error = call(
            &Http::new(),
            "get",
            vec![HostValue::Str("http://127.0.0.1:1/nope".into())],
        )
        .expect_err("a refused connection fails");
        assert!(
            matches!(error.field("kind"), Some(HostValue::Str(k)) if k == "connect" || k == "transport"),
            "unexpected kind: {error:?}"
        );
    }

    #[test]
    fn post_sends_a_record_as_json_with_the_content_type_set() {
        let server = Server::serve(vec![response("200 OK", "application/json", "{}")]);
        let url = server.url("/submit");
        let body = HostValue::Record(vec![("name".into(), HostValue::Str("ada".into()))]);
        call(&Http::new(), "post", vec![HostValue::Str(url), body]).expect("posted");

        let seen = server.requests();
        assert!(seen[0].starts_with("POST /submit"), "{}", seen[0]);
        assert!(
            seen[0]
                .to_lowercase()
                .contains("content-type: application/json"),
            "a record body implies JSON: {}",
            seen[0]
        );
    }

    #[test]
    fn request_takes_the_method_headers_and_body_it_is_given() {
        let server = Server::serve(vec![response("200 OK", "text/plain", "ok")]);
        let record = HostValue::Record(vec![
            ("url".into(), HostValue::Str(server.url("/thing/7"))),
            ("method".into(), HostValue::Str("delete".into())),
            (
                "headers".into(),
                HostValue::Record(vec![("x-token".into(), HostValue::Str("abc".into()))]),
            ),
        ]);
        call(&Http::new(), "request", vec![record]).expect("deleted");

        let seen = server.requests();
        assert!(seen[0].starts_with("DELETE /thing/7"), "{}", seen[0]);
        assert!(
            seen[0].to_lowercase().contains("x-token: abc"),
            "{}",
            seen[0]
        );
    }

    #[test]
    fn a_relative_path_resolves_against_the_base_url() {
        let server = Server::serve(vec![response("200 OK", "text/plain", "ok")]);
        let http = Http::with_base_url(server.url(""));
        call(&http, "get", vec![HostValue::Str("/wx".into())]).expect("fetched");
        let seen = server.requests();
        assert!(seen[0].starts_with("GET /wx"), "{}", seen[0]);
    }

    #[test]
    fn an_absolute_url_ignores_the_base_url() {
        let server = Server::serve(vec![response("200 OK", "text/plain", "ok")]);
        let http = Http::with_base_url("http://example.invalid");
        call(&http, "get", vec![HostValue::Str(server.url("/direct"))]).expect("fetched");
        let seen = server.requests();
        assert!(seen[0].starts_with("GET /direct"), "{}", seen[0]);
    }

    #[test]
    fn an_unknown_method_names_the_ones_that_exist() {
        let error = call(&Http::new(), "fetch", vec![]).expect_err("no such method");
        let HostValue::Str(message) = error.field("message").expect("message") else {
            panic!("expected a message");
        };
        assert!(message.contains("get"), "{message}");
    }

    #[test]
    fn a_request_without_a_url_fails_before_touching_the_network() {
        let error = call(
            &Http::new(),
            "request",
            vec![HostValue::Record(vec![(
                "method".into(),
                HostValue::Str("GET".into()),
            )])],
        )
        .expect_err("no url");
        assert_eq!(
            error.field("kind"),
            Some(&HostValue::Str("bad_request".into()))
        );
    }
}
