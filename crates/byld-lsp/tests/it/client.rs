//! A language client small enough to read: starts the server, frames its
//! messages and waits for the answers a test asks about.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A debug build on a busy machine is slow; nothing here waits for a reason
/// other than the server being asked to do work.
const PATIENCE: Duration = Duration::from_secs(60);

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    /// Notifications that arrived while something else was awaited.
    pending: Vec<Value>,
    next_id: i64,
}

impl Client {
    /// Starts the server and completes the handshake. `watches_files` says
    /// whether the client offers to watch files, which makes the server ask.
    pub fn start(watches_files: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_byld-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start byld-lsp");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, messages) = channel();
        std::thread::spawn(move || {
            while let Some(message) = read_message(&mut stdout) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            messages,
            pending: Vec::new(),
            next_id: 1,
        };
        let capabilities = if watches_files {
            json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } } })
        } else {
            json!({})
        };
        client.request("initialize", json!({ "capabilities": capabilities }));
        client.notify("initialized", json!({}));
        client
    }

    fn send(&mut self, message: &Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Sends a request and waits for its answer. Whatever else arrives meanwhile
    /// is kept, and a request the server makes of the client is answered with
    /// nothing, which is what a client that accepts it says.
    #[allow(clippy::needless_pass_by_value)]
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + PATIENCE;
        loop {
            let message = self.next_message(deadline);
            if message.get("method").is_some() && message.get("id").is_some() {
                let reply = json!({ "jsonrpc": "2.0", "id": message["id"], "result": null });
                self.pending.push(message);
                self.send(&reply);
            } else if message.get("id") == Some(&json!(id)) {
                return message["result"].clone();
            } else {
                self.pending.push(message);
            }
        }
    }

    fn next_message(&mut self, deadline: Instant) -> Value {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.messages.recv_timeout(wait) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => panic!("the server said nothing within {PATIENCE:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("the server went away"),
        }
    }

    pub fn open(&mut self, path: &Path, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri(path), "languageId": "byld", "version": 1, "text": text
            } }),
        );
    }

    pub fn change(&mut self, path: &Path, version: i64, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri(path), "version": version },
                "contentChanges": [{ "text": text }]
            }),
        );
    }

    pub fn save(&mut self, path: &Path) {
        self.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri(path) } }),
        );
    }

    pub fn close(&mut self, path: &Path) {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri(path) } }),
        );
    }

    /// Tells the server a file changed on disk without the editor having it
    /// open, as a file watcher does.
    pub fn watched(&mut self, path: &Path) {
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": [{ "uri": uri(path), "type": 2 }] }),
        );
    }

    /// The diagnostics published for `path` at `version`, waiting for them.
    /// Earlier publications, for any version, are passed over.
    pub fn diagnostics_at(&mut self, path: &Path, version: i64) -> Vec<Value> {
        self.wait_publish(path, |p| p["version"] == json!(version))
    }

    /// The next diagnostics published for `path`, whatever their version.
    pub fn next_diagnostics(&mut self, path: &Path) -> Vec<Value> {
        self.wait_publish(path, |_| true)
    }

    fn wait_publish(&mut self, path: &Path, wanted: impl Fn(&Value) -> bool) -> Vec<Value> {
        let uri = uri(path);
        let is_publish = |m: &Value| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == json!(uri)
        };
        let deadline = Instant::now() + PATIENCE;
        loop {
            while let Some(i) = self.pending.iter().position(is_publish) {
                let message = self.pending.remove(i);
                if wanted(&message["params"]) {
                    return message["params"]["diagnostics"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                }
            }
            let message = self.next_message(deadline);
            self.pending.push(message);
        }
    }

    /// Whether the server asked, after `initialized`, to be told about file changes.
    pub fn asked_to_watch(&mut self) -> bool {
        // The request is made when `initialized` is handled; a round trip of
        // another request puts every message sent before it in `pending`.
        self.request("shutdown-probe", json!(null));
        self.pending
            .iter()
            .any(|m| m["method"] == "client/registerCapability")
    }

    pub fn shutdown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `file:` URI an editor would send for `path`. Built by `url` rather than
/// by gluing `file://` to the path, which on Windows gives `file://C:\…` (or
/// `file://\\?\C:\…` for a canonical path): not a URI, so the server drops the
/// message and never answers.
pub fn uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .unwrap_or_else(|()| panic!("{} is not an absolute path", path.display()))
        .to_string()
}

fn read_message(from: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if from.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length:") {
            length = n.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    from.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// A diagnostic's code, if it has one.
pub fn code(diagnostic: &Value) -> Option<&str> {
    diagnostic["code"].as_str()
}

/// The 1-based line a diagnostic starts on, as `byard check` counts.
pub fn line(diagnostic: &Value) -> u64 {
    diagnostic["range"]["start"]["line"].as_u64().unwrap() + 1
}

/// Whether a diagnostic is an error (severity 1).
pub fn is_error(diagnostic: &Value) -> bool {
    diagnostic["severity"] == json!(1)
}

/// Fails, showing them, unless there are no diagnostics.
pub fn assert_clean(diagnostics: &[Value]) {
    assert_eq!(diagnostics.len(), 0, "{diagnostics:#?}");
}
