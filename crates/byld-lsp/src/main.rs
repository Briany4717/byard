//! High-performance, ultra-resilient Language Server Protocol (LSP) server for Byld (`.byd`).
//!
//! Architected with modularity, zero-copy index structures, and thread-safe lock-free state (`DashMap`).

#![allow(
    clippy::needless_pass_by_value,
    clippy::mutable_key_type,
    clippy::explicit_counter_loop,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::only_used_in_recursion,
    clippy::format_push_string
)]

/// LSP Capabilities implementation.
pub mod capabilities;
/// Semantic symbol index and resolution.
pub mod semantic;
/// Concurrent document state store.
pub mod state;
/// AST syntax tools and line indexing.
pub mod syntax;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    DidSaveTextDocument, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
    PrepareRenameRequest, Rename, Request as _, SemanticTokensFullRequest,
};
use lsp_types::{
    CodeActionOptions, CodeActionProviderCapability, CompletionOptions, HoverProviderCapability,
    OneOf, PublishDiagnosticsParams, RenameOptions, SemanticTokensFullOptions,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};

use state::document::DocumentStore;
use state::project::ProjectCache;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    eprintln!("[byld-lsp] Starting Byld Language Server...");

    // Set up communication channel over stdio
    let (connection, io_threads) = Connection::stdio();

    // Register full LSP capabilities inside InitializeResult
    let initialize_result = serde_json::to_value(&lsp_types::InitializeResult {
        capabilities: ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            completion_provider: Some(CompletionOptions {
                resolve_provider: Some(false),
                trigger_characters: Some(vec![
                    "#".to_string(),
                    "[".to_string(),
                    ":".to_string(),
                    ".".to_string(),
                    " ".to_string(),
                ]),
                ..Default::default()
            }),
            definition_provider: Some(OneOf::Left(true)),
            document_symbol_provider: Some(OneOf::Left(true)),
            document_formatting_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
            })),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: capabilities::semantic_tokens::get_legend(),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    range: None,
                    work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
                }),
            ),
            code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                code_action_kinds: Some(vec![
                    lsp_types::CodeActionKind::QUICKFIX,
                    lsp_types::CodeActionKind::REFACTOR_REWRITE,
                ]),
                resolve_provider: None,
                work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
            })),
            ..Default::default()
        },
        server_info: Some(lsp_types::ServerInfo {
            name: "byld-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    })?;

    let (id, params) = match connection.initialize_start() {
        Ok(res) => res,
        Err(e) => {
            eprintln!("[byld-lsp] Error during initialize_start: {e}");
            return Err(e.into());
        }
    };

    let mut client_watches_files = false;
    if let Ok(init_p) = serde_json::from_value::<lsp_types::InitializeParams>(params) {
        eprintln!(
            "[byld-lsp] Client connected: {:?}",
            init_p.client_info.map(|c| c.name)
        );
        client_watches_files = init_p
            .capabilities
            .workspace
            .and_then(|w| w.did_change_watched_files)
            .and_then(|d| d.dynamic_registration)
            .unwrap_or(false);
    } else {
        eprintln!("[byld-lsp] Could not parse client InitializeParams, proceeding safely.");
    }

    if let Err(e) = connection.initialize_finish(id, initialize_result) {
        eprintln!("[byld-lsp] Error during initialize_finish: {e}");
        return Err(e.into());
    }

    eprintln!("[byld-lsp] Handshake complete. Running main loop...");
    // `initialize_finish` took the `initialized` notification with it, so this
    // is the first moment the client can be asked for anything.
    if client_watches_files {
        watch_project_files(&connection);
    }
    main_loop(connection)?;

    io_threads.join()?;
    eprintln!("[byld-lsp] Server shut down cleanly.");
    Ok(())
}

/// Asks the client to tell the server when a `.byd` file, a manifest or a
/// lockfile changes on disk, whoever changed it: a `git checkout`, another
/// editor, `byard get`. What the person types arrives as edits; this is how
/// the rest of the project is heard.
fn watch_project_files(connection: &Connection) {
    let watcher = |glob: &str| lsp_types::FileSystemWatcher {
        glob_pattern: lsp_types::GlobPattern::String(glob.to_string()),
        kind: None,
    };
    let options = lsp_types::DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![
            watcher("**/*.byd"),
            watcher("**/byard.toml"),
            watcher("**/byard.lock"),
        ],
    };
    let Ok(register_options) = serde_json::to_value(options) else {
        return;
    };
    let params = lsp_types::RegistrationParams {
        registrations: vec![lsp_types::Registration {
            id: "byld-watch-project-files".to_string(),
            method: DidChangeWatchedFiles::METHOD.to_string(),
            register_options: Some(register_options),
        }],
    };
    let request = Request::new(
        lsp_server::RequestId::from("byld-register-watchers".to_string()),
        "client/registerCapability".to_string(),
        params,
    );
    if let Err(e) = connection.sender.send(Message::Request(request)) {
        eprintln!("[byld-lsp] Error registering the file watchers: {e}");
    }
}

/// The documents whose diagnostics are owed to the client.
///
/// A burst of edits (typing, a paste, a format-on-save) arrives as many
/// notifications faster than they can be answered, and only the last one's
/// answer is of any use. So the answer is owed, not given, until the queue is
/// empty: each document is analysed once per burst, from its latest text.
#[derive(Default)]
struct Owed(Vec<lsp_types::Uri>);

impl Owed {
    fn add(&mut self, uri: &lsp_types::Uri) {
        if !self.0.contains(uri) {
            self.0.push(uri.clone());
        }
    }

    fn add_all(&mut self, documents: &DocumentStore) {
        for doc in documents.all() {
            self.add(&doc.uri);
        }
    }

    fn pay(&mut self, connection: &Connection, documents: &DocumentStore, projects: &ProjectCache) {
        for uri in self.0.drain(..) {
            if let Some(doc) = documents.get(&uri) {
                publish_diagnostics_safe(connection, &doc, projects, documents);
            }
        }
    }
}

#[allow(clippy::unnecessary_wraps)]
fn main_loop(connection: Connection) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let document_store = DocumentStore::new();
    let projects = ProjectCache::new();
    let mut owed = Owed::default();

    loop {
        let msg = if let Ok(msg) = connection.receiver.try_recv() {
            msg
        } else {
            // Nothing is waiting: this is the moment the owed answers are given.
            owed.pay(&connection, &document_store, &projects);
            match connection.receiver.recv() {
                Ok(msg) => msg,
                Err(_) => break,
            }
        };
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req).unwrap_or(false) {
                    eprintln!("[byld-lsp] Shutdown request received.");
                    return Ok(());
                }

                dispatch_request(&connection, &document_store, req);
            }
            Message::Notification(not) => match not.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    if let Ok(params) =
                        serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(not.params)
                    {
                        let doc = document_store.insert(
                            params.text_document.uri,
                            Some(params.text_document.version),
                            params.text_document.text,
                        );
                        owed.add(&doc.uri);
                    } else {
                        eprintln!("[byld-lsp] Failed to parse DidOpenTextDocument params");
                    }
                }
                DidChangeTextDocument::METHOD => {
                    if let Ok(params) =
                        serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(not.params)
                    {
                        if let Some(change) = params.content_changes.into_iter().next() {
                            let doc = document_store.insert(
                                params.text_document.uri,
                                Some(params.text_document.version),
                                change.text,
                            );
                            owed.add(&doc.uri);
                        }
                    } else {
                        eprintln!("[byld-lsp] Failed to parse DidChangeTextDocument params");
                    }
                }
                DidSaveTextDocument::METHOD => {
                    // A save is the moment the rest of the project may have
                    // changed under every other open file.
                    projects.invalidate();
                    owed.add_all(&document_store);
                }
                DidChangeWatchedFiles::METHOD => {
                    projects.invalidate();
                    owed.add_all(&document_store);
                }
                DidCloseTextDocument::METHOD => {
                    if let Ok(params) =
                        serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(not.params)
                    {
                        let uri = params.text_document.uri;
                        document_store.remove(&uri);
                        owed.0.retain(|u| u != &uri);

                        let clear_params = PublishDiagnosticsParams {
                            uri,
                            diagnostics: Vec::new(),
                            version: None,
                        };
                        let notification =
                            Notification::new(PublishDiagnostics::METHOD.to_string(), clear_params);
                        let _ = connection.sender.send(Message::Notification(notification));
                        // Its text no longer stands for the file: whatever
                        // the others made of it, the disk now decides.
                        owed.add_all(&document_store);
                    }
                }
                "exit" => {
                    eprintln!("[byld-lsp] Exit notification received.");
                    return Ok(());
                }
                _ => {
                    eprintln!("[byld-lsp] Ignored notification: {}", not.method);
                }
            },
            Message::Response(_) => {}
        }
    }

    Ok(())
}

fn dispatch_request(connection: &Connection, document_store: &DocumentStore, req: Request) {
    match req.method.as_str() {
        HoverRequest::METHOD => {
            let (id, params) = match cast_request::<HoverRequest>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, HoverRequest::METHOD, err);
                }
            };
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;

            let hover_info = document_store
                .get(&uri)
                .and_then(|doc| capabilities::hover::handle_hover(&doc, pos));
            send_response(connection, Response::new_ok(id, hover_info));
        }
        Completion::METHOD => {
            let (id, params) = match cast_request::<Completion>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, Completion::METHOD, err);
                }
            };
            let uri = params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;

            let completion_info = document_store
                .get(&uri)
                .and_then(|doc| capabilities::completion::handle_completion(&doc, pos));
            send_response(connection, Response::new_ok(id, completion_info));
        }
        GotoDefinition::METHOD => {
            let (id, params) = match cast_request::<GotoDefinition>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, GotoDefinition::METHOD, err);
                }
            };
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;

            let def_info = document_store
                .get(&uri)
                .and_then(|doc| capabilities::definition::handle_definition(&doc, pos));
            send_response(connection, Response::new_ok(id, def_info));
        }
        DocumentSymbolRequest::METHOD => {
            let (id, params) = match cast_request::<DocumentSymbolRequest>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, DocumentSymbolRequest::METHOD, err);
                }
            };
            let uri = params.text_document.uri;

            let symbol_info = document_store
                .get(&uri)
                .and_then(|doc| capabilities::document_symbol::handle_document_symbol(&doc));
            send_response(connection, Response::new_ok(id, symbol_info));
        }
        Formatting::METHOD => {
            let (id, params) = match cast_request::<Formatting>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, Formatting::METHOD, err);
                }
            };
            let uri = params.text_document.uri;
            let options = params.options;

            let edits = document_store
                .get(&uri)
                .and_then(|doc| capabilities::formatting::handle_formatting(&doc, &options));
            send_response(connection, Response::new_ok(id, edits));
        }
        PrepareRenameRequest::METHOD => {
            let (id, params) = match cast_request::<PrepareRenameRequest>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, PrepareRenameRequest::METHOD, err);
                }
            };
            let uri = params.text_document.uri;
            let pos = params.position;

            let prepare = document_store
                .get(&uri)
                .and_then(|doc| capabilities::rename::handle_prepare_rename(&doc, pos));
            send_response(connection, Response::new_ok(id, prepare));
        }
        Rename::METHOD => {
            let (id, params) = match cast_request::<Rename>(req) {
                Ok(res) => res,
                Err((id, err)) => return send_invalid_params(connection, id, Rename::METHOD, err),
            };
            let uri = params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;

            let edit = document_store
                .get(&uri)
                .and_then(|doc| capabilities::rename::handle_rename(&doc, pos, params.new_name));
            send_response(connection, Response::new_ok(id, edit));
        }
        SemanticTokensFullRequest::METHOD => {
            let (id, params) = match cast_request::<SemanticTokensFullRequest>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(
                        connection,
                        id,
                        SemanticTokensFullRequest::METHOD,
                        err,
                    );
                }
            };
            let uri = params.text_document.uri;

            let tokens = document_store
                .get(&uri)
                .and_then(|doc| capabilities::semantic_tokens::handle_semantic_tokens(&doc));
            send_response(connection, Response::new_ok(id, tokens));
        }
        CodeActionRequest::METHOD => {
            let (id, params) = match cast_request::<CodeActionRequest>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, CodeActionRequest::METHOD, err);
                }
            };
            let uri = params.text_document.uri.clone();

            let actions = document_store
                .get(&uri)
                .and_then(|doc| capabilities::code_actions::handle_code_action(&doc, params));
            send_response(connection, Response::new_ok(id, actions));
        }
        unhandled_method => {
            eprintln!("[byld-lsp] Unhandled request method: {unhandled_method}");
            let err_response = Response::new_err(
                req.id,
                ErrorCode::MethodNotFound as i32,
                format!("Method '{unhandled_method}' is not supported by byld-lsp"),
            );
            send_response(connection, err_response);
        }
    }
}

fn publish_diagnostics_safe(
    connection: &Connection,
    doc: &state::document::Document,
    projects: &ProjectCache,
    documents: &DocumentStore,
) {
    let params = capabilities::diagnostics::compute_project_diagnostics(doc, projects, documents);
    let notification = Notification::new(PublishDiagnostics::METHOD.to_string(), params);
    if let Err(e) = connection.sender.send(Message::Notification(notification)) {
        eprintln!("[byld-lsp] Error sending PublishDiagnostics notification: {e}");
    }
}

fn send_response(connection: &Connection, res: Response) {
    if let Err(e) = connection.sender.send(Message::Response(res)) {
        eprintln!("[byld-lsp] Error sending response over STDIO: {e}");
    }
}

fn send_invalid_params(
    connection: &Connection,
    id: lsp_server::RequestId,
    method: &str,
    err: serde_json::Error,
) {
    eprintln!("[byld-lsp] Invalid params for method {method}: {err}");
    let err_response = Response::new_err(
        id,
        ErrorCode::InvalidParams as i32,
        format!("Invalid params for method {method}: {err}"),
    );
    send_response(connection, err_response);
}

fn cast_request<R>(
    req: Request,
) -> Result<(lsp_server::RequestId, R::Params), (lsp_server::RequestId, serde_json::Error)>
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
{
    let id = req.id.clone();
    match serde_json::from_value(req.params) {
        Ok(params) => Ok((id, params)),
        Err(err) => Err((id, err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        CompletionResponse, DocumentSymbolResponse, GotoDefinitionResponse, Position, Uri,
    };
    use state::document::Document;

    #[test]
    fn test_completion_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main {\n  Column {\n  }\n}".to_string();
        let doc = Document::new(uri, Some(1), content);

        let pos = Position::new(2, 2);
        let resp = capabilities::completion::handle_completion(&doc, pos).unwrap();
        if let CompletionResponse::Array(items) = resp {
            assert!(items.iter().any(|item| item.label == "Column"));
            assert!(items.iter().any(|item| item.label == "Button"));
            assert!(items.iter().any(|item| item.label == "var"));
            assert!(items.iter().any(|item| item.label == "Main"));
        } else {
            panic!("Expected Array completion response");
        }
    }

    #[test]
    fn test_hover_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main {\n  Column {\n  }\n}".to_string();
        let doc = Document::new(uri, Some(1), content);

        let pos = Position::new(1, 4); // on `Column`
        let hover = capabilities::hover::handle_hover(&doc, pos).unwrap();
        if let lsp_types::HoverContents::Markup(markup) = hover.contents {
            assert!(markup.value.contains("Intrinsic `Column`"));
        } else {
            panic!("Expected Markup hover content");
        }
    }

    #[test]
    fn test_definition_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main {\n  var my_var = 10\n  Text(my_var)\n}".to_string();
        let doc = Document::new(uri.clone(), Some(1), content);

        let pos = Position::new(2, 8); // on my_var reference
        let resp = capabilities::definition::handle_definition(&doc, pos).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = resp {
            assert_eq!(loc.uri, uri);
            assert_eq!(loc.range.start.line, 1);
        } else {
            panic!("Expected Scalar definition response");
        }
    }

    #[test]
    fn test_document_symbol_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main(title: Str) {\n  var count = 0\n}".to_string();
        let doc = Document::new(uri, Some(1), content);

        let resp = capabilities::document_symbol::handle_document_symbol(&doc).unwrap();
        if let lsp_types::DocumentSymbolResponse::Nested(symbols) = resp {
            assert_eq!(symbols.len(), 1);
            assert_eq!(symbols[0].name, "Main");
            let children = symbols[0].children.as_ref().unwrap();
            assert!(children.iter().any(|s| s.name == "title"));
            assert!(children.iter().any(|s| s.name == "count"));
        } else {
            panic!("Expected Nested DocumentSymbolResponse");
        }
    }

    #[test]
    fn test_rename_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main {\n  var my_var = 10\n  Text(my_var)\n}".to_string();
        let doc = Document::new(uri.clone(), Some(1), content);

        let pos = Position::new(1, 8); // on `my_var` declaration
        let prepare = capabilities::rename::handle_prepare_rename(&doc, pos);
        assert!(prepare.is_some());

        let edit = capabilities::rename::handle_rename(&doc, pos, "new_var".to_string()).unwrap();
        let changes = edit.changes.unwrap();
        let edits = changes.get(&uri).unwrap();
        assert_eq!(edits.len(), 2); // declaration + usage
    }

    /// The three effect members RFC-0029 and RFC-0038 added must appear in the
    /// outline. Before they had arms of their own they were simply invisible.
    #[test]
    fn test_document_symbol_includes_effect_members() {
        let uri: Uri = "file:///effects.byd".parse().unwrap();
        let content = "View Main() {\n  var n: Int = 0\n  on mount => { n = 1 }\n  every 1s => { n = n + 1 }\n}".to_string();
        let doc = Document::new(uri, Some(1), content);

        let resp = capabilities::document_symbol::handle_document_symbol(&doc)
            .expect("a parsed view yields an outline");
        let DocumentSymbolResponse::Nested(symbols) = resp else {
            panic!("expected a nested document-symbol response");
        };
        let names: Vec<String> = symbols
            .iter()
            .flat_map(|s| {
                std::iter::once(s.name.clone())
                    .chain(s.children.iter().flatten().map(|c| c.name.clone()))
            })
            .collect();
        assert!(
            names.iter().any(|n| n == "on mount"),
            "the lifecycle effect must be in the outline: {names:?}"
        );
        assert!(
            names.iter().any(|n| n.starts_with("every ")),
            "the timer effect must be in the outline: {names:?}"
        );
    }

    /// A rename must reach inside an effect's action body. Skipping it would
    /// rewrite every other mention of the binding and silently leave this one,
    /// which is a broken rename rather than a missing feature.
    #[test]
    fn test_rename_reaches_inside_an_effect_action() {
        let uri: Uri = "file:///effects.byd".parse().unwrap();
        let content =
            "View Main() {\n  var count: Int = 0\n  on mount => { count = 1 }\n}".to_string();
        let doc = Document::new(uri, Some(1), content.clone());

        // Position on the `count` declaration.
        let pos = Position::new(1, 6);
        let edit = capabilities::rename::handle_rename(&doc, pos, "total".to_string())
            .expect("renaming a declared var yields an edit");
        let edits = edit
            .changes
            .as_ref()
            .and_then(|c| c.values().next())
            .expect("one file's worth of edits");
        assert!(
            edits.len() >= 2,
            "the declaration and the use inside `on mount` must both be renamed, got {}: {edits:?}",
            edits.len()
        );
    }

    /// The traversal used to stop at a catch-all arm, so a mention inside an
    /// assignment, a `++`, or an arithmetic expression was left un-renamed
    /// while every other mention was rewritten. That corrupts the file, so it
    /// is guarded here rather than only at the effect that first exposed it.
    #[test]
    fn test_rename_covers_lvalues_and_operators() {
        let uri: Uri = "file:///ops.byd".parse().unwrap();
        let content = concat!(
            "View Main() {\n",
            "  var count: Int = 0\n",
            "  Button(\"go\") #[on_tap: { count++ }]\n",
            "  Text(\"{count + 1}\")\n",
            "}"
        )
        .to_string();
        let doc = Document::new(uri, Some(1), content);

        let pos = Position::new(1, 6);
        let edit = capabilities::rename::handle_rename(&doc, pos, "total".to_string())
            .expect("renaming a declared var yields an edit");
        let edits = edit
            .changes
            .as_ref()
            .and_then(|c| c.values().next())
            .expect("one file's worth of edits");
        assert!(
            edits.len() >= 3,
            "the declaration, the `count++`, and the `count + 1` must all be \
             renamed, got {}: {edits:?}",
            edits.len()
        );

        // The declaration edit must cover `count`, not `var c`. Anchoring at
        // the member span start rewrote the keyword and dropped the tail,
        // turning `var count` into `totalount`.
        let decl = edits
            .iter()
            .find(|e| e.range.start.line == 1)
            .expect("an edit on the declaration line");
        assert_eq!(
            (decl.range.start.character, decl.range.end.character),
            (6, 11),
            "the declaration edit must span `count` exactly: {decl:?}"
        );
    }

    #[test]
    fn test_semantic_tokens_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main {\n  var count = 10\n}".to_string();
        let doc = Document::new(uri, Some(1), content);

        let result = capabilities::semantic_tokens::handle_semantic_tokens(&doc).unwrap();
        if let lsp_types::SemanticTokensResult::Tokens(tokens) = result {
            assert!(!tokens.data.is_empty(), "{:?}", tokens.data);
        } else {
            panic!("Expected SemanticTokensResult::Tokens");
        }
    }

    /// Applies one edit to `text`, as a client would.
    fn apply(doc: &Document, edit: &lsp_types::TextEdit) -> String {
        let start = doc
            .line_index
            .position_to_offset(&doc.content, edit.range.start)
            .unwrap();
        let end = doc
            .line_index
            .position_to_offset(&doc.content, edit.range.end)
            .unwrap();
        format!(
            "{}{}{}",
            &doc.content[..start],
            edit.new_text,
            &doc.content[end..]
        )
    }

    /// The first edit of every code action the document offers at `pos`
    /// (with `diagnostics` as the client's context), as `(title, edited text)`.
    fn actions_at(
        doc: &Document,
        pos: Position,
        diagnostics: Vec<lsp_types::Diagnostic>,
    ) -> Vec<(String, String)> {
        let actions = capabilities::code_actions::handle_code_action(
            doc,
            lsp_types::CodeActionParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: doc.uri.clone(),
                },
                range: lsp_types::Range::new(pos, pos),
                context: lsp_types::CodeActionContext {
                    diagnostics,
                    only: None,
                    trigger_kind: None,
                },
                work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
                partial_result_params: lsp_types::PartialResultParams::default(),
            },
        )
        .unwrap_or_default();
        actions
            .into_iter()
            .filter_map(|a| match a {
                lsp_types::CodeActionOrCommand::CodeAction(ca) => {
                    let edit = ca.edit?.changes?.get(&doc.uri)?.first()?.clone();
                    Some((ca.title, apply(doc, &edit)))
                }
                lsp_types::CodeActionOrCommand::Command(_) => None,
            })
            .collect()
    }

    /// "Convert var to let" used to rewrite the initializer through a printer
    /// that knew three kinds of expression and wrote `0` for the rest, so
    /// applying it to `var x = a + b` deleted the person's code.
    #[test]
    fn test_var_to_let_keeps_the_initializer_exactly() {
        for init in [
            "1 + 2",
            "\"hello {name}\"",
            "items.filter(|i| i.done).len()",
            "flag ? 10 : 20 // a note",
        ] {
            let uri: Uri = "file:///test.byd".parse().unwrap();
            let content = format!("View Main() {{\n  var count = {init}\n}}\n");
            let doc = Document::new(uri, Some(1), content.clone());
            let actions = actions_at(&doc, Position::new(1, 6), Vec::new());
            let (title, edited) = actions
                .iter()
                .find(|(t, _)| t.contains("to 'let count'"))
                .unwrap_or_else(|| panic!("no conversion offered for `{init}`: {actions:?}"));
            assert!(title.contains("var count"), "{title}");
            assert_eq!(
                edited,
                &content.replacen("var count", "let count", 1),
                "only the keyword may change for `{init}`"
            );
        }
    }

    #[test]
    fn test_var_to_let_keeps_the_declared_type() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main() {\n  var count: Int = 1\n}\n".to_string();
        let doc = Document::new(uri, Some(1), content);
        let actions = actions_at(&doc, Position::new(1, 6), Vec::new());
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].1, "View Main() {\n  let count: Int = 1\n}\n");
    }

    /// The quick fix looked for the words "unknown property" and "invalid
    /// attribute" in a message that says "unknown attribute", so it never
    /// appeared. It now follows the diagnostic's code, and what it removes
    /// leaves a list that still parses.
    #[test]
    fn test_remove_unknown_attribute_quick_fix() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        for (written, left) in [
            ("#[bogus: 1]", "#[]"),
            ("#[bogus: 1, gap: 4]", "#[gap: 4]"),
            ("#[gap: 4, bogus: 1]", "#[gap: 4]"),
            ("#[gap: 4, bogus: 1, p: 2]", "#[gap: 4, p: 2]"),
        ] {
            let content = format!("View Main() {{\n  Column {written} {{}}\n}}\n");
            let doc = Document::new(uri.clone(), Some(1), content.clone());
            let diagnostics = capabilities::diagnostics::compute_diagnostics(&doc).diagnostics;
            let unknown = diagnostics
                .iter()
                .find(|d| {
                    d.code == Some(lsp_types::NumberOrString::String("UnknownAttribute".into()))
                })
                .unwrap_or_else(|| panic!("no UnknownAttribute for `{written}`: {diagnostics:?}"));
            assert!(unknown.message.contains("unknown attribute"), "{unknown:?}");

            let actions = actions_at(&doc, unknown.range.start, vec![unknown.clone()]);
            let (_, edited) = actions
                .iter()
                .find(|(t, _)| t == "Remove invalid attribute")
                .unwrap_or_else(|| panic!("no quick fix for `{written}`: {actions:?}"));
            assert_eq!(edited, &content.replace(written, left), "for `{written}`");
            assert!(
                byard_compiler::parser::parse(edited).errors.is_empty(),
                "the list left behind must still parse: `{left}`"
            );
        }
    }

    #[test]
    fn test_quick_fix_does_not_fire_for_other_problems() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main() {\n  Nowhere()\n}\n".to_string();
        let doc = Document::new(uri, Some(1), content);
        let diagnostics = capabilities::diagnostics::compute_diagnostics(&doc).diagnostics;
        assert_ne!(diagnostics.len(), 0);
        let actions = actions_at(&doc, Position::new(1, 2), diagnostics);
        assert!(
            actions.iter().all(|(t, _)| t != "Remove invalid attribute"),
            "{actions:?}"
        );
    }

    /// A warning is advice: `byard check` prints it and passes, so the editor
    /// must not paint it red.
    #[test]
    fn test_warnings_are_not_errors() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main() {\n  inject Greeter as greeter\n  Text(\"hi\")\n}\n".to_string();
        let doc = Document::new(uri, Some(1), content);
        let diagnostics = capabilities::diagnostics::compute_diagnostics(&doc).diagnostics;
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].severity,
            Some(lsp_types::DiagnosticSeverity::WARNING)
        );
        assert_eq!(
            diagnostics[0].code,
            Some(lsp_types::NumberOrString::String(
                "UncheckableInject".into()
            ))
        );
    }

    #[test]
    fn test_errors_are_errors() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main() {\n  Nowhere()\n}\n".to_string();
        let doc = Document::new(uri, Some(1), content);
        let diagnostics = capabilities::diagnostics::compute_diagnostics(&doc).diagnostics;
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].severity,
            Some(lsp_types::DiagnosticSeverity::ERROR)
        );
    }

    /// The framework's own capabilities are known without the app's Rust.
    #[test]
    fn test_builtin_capabilities_are_known() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = concat!(
            "View Main() {\n",
            "  inject Http as http\n",
            "  inject Store as store\n",
            "  Text(\"hi\")\n",
            "}\n"
        )
        .to_string();
        let doc = Document::new(uri, Some(1), content);
        let diagnostics = capabilities::diagnostics::compute_diagnostics(&doc).diagnostics;
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn test_document_symbols_include_top_level_types_and_functions() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = concat!(
            "type Item = { id: Int, title: Str, note: Str? }\n",
            "fn double(n: Int) -> Int => n * 2\n",
            "View Main() {\n",
            "  Text(\"hi\")\n",
            "}\n"
        )
        .to_string();
        let doc = Document::new(uri, Some(1), content);
        let DocumentSymbolResponse::Nested(symbols) =
            capabilities::document_symbol::handle_document_symbol(&doc).unwrap()
        else {
            panic!("Expected Nested DocumentSymbolResponse");
        };
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Item", "double", "Main"]);

        let item = &symbols[0];
        assert_eq!(item.kind, lsp_types::SymbolKind::STRUCT);
        let fields: Vec<&str> = item
            .children
            .iter()
            .flatten()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(fields, ["id", "title", "note"]);
        assert_eq!(symbols[1].kind, lsp_types::SymbolKind::FUNCTION);
        // The ranges are the declarations', not the file's.
        assert_eq!(item.range.start.line, 0);
        assert_eq!(symbols[1].range.start.line, 1);
    }

    /// The tokens as `(line, column, length, type)`.
    fn decode(doc: &Document) -> Vec<(u32, u32, u32, u32)> {
        let Some(lsp_types::SemanticTokensResult::Tokens(tokens)) =
            capabilities::semantic_tokens::handle_semantic_tokens(doc)
        else {
            panic!("Expected SemanticTokensResult::Tokens");
        };
        let (mut line, mut col) = (0, 0);
        tokens
            .data
            .iter()
            .map(|t| {
                line += t.delta_line;
                col = if t.delta_line == 0 {
                    col + t.delta_start
                } else {
                    t.delta_start
                };
                (line, col, t.length, t.token_type)
            })
            .collect()
    }

    /// Where `needle` first occurs in the document, as `(line, column)`.
    fn at(doc: &Document, needle: &str) -> (u32, u32) {
        let p = doc
            .line_index
            .offset_to_position(&doc.content, doc.content.find(needle).unwrap());
        (p.line, p.character)
    }

    /// The traversal stopped at a catch-all, so a name inside an operator, a
    /// ternary, an assignment or a lambda was never coloured.
    #[test]
    fn test_semantic_tokens_reach_inside_every_expression() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = concat!(
            "fn clamp(n: Int, hi: Int) -> Int => n > hi ? hi : n\n",
            "View Main() {\n",
            "  var count = 10\n",
            "  let doubled = count * 2\n",
            "  let label = -count > 0 ? \"up\" : \"down\"\n",
            "  let mine = { total: count, ..other }\n",
            "  let at = items[count]\n",
            "  let each = items.map(|i| i + count)\n",
            "  Button(\"go\") #[on_tap: { if count > 1 { count = count - 1 } else { count++ } }]\n",
            "  Box #[bg: style { radius: count }]\n",
            "}\n"
        )
        .to_string();
        let doc = Document::new(uri, Some(1), content);
        assert!(doc.parsed.errors.is_empty(), "{:?}", doc.parsed.errors);
        let tokens = decode(&doc);
        let variable = 3;
        let coloured = |line: u32, col: u32| {
            tokens
                .iter()
                .any(|&(l, c, _, ty)| l == line && c == col && ty == variable)
        };

        // Every occurrence of `count` after its declaration is a variable.
        let mut found = 0;
        for (line, text) in doc.content.lines().enumerate().skip(3) {
            let mut from = 0;
            while let Some(i) = text[from..].find("count") {
                let col = (from + i) as u32;
                // Not part of a longer word or a string.
                assert!(
                    coloured(line as u32, col),
                    "`count` at {line}:{col} in `{text}` is not coloured: {tokens:?}"
                );
                found += 1;
                from += i + 5;
            }
        }
        assert_eq!(found, 10, "the test counts the names it expects to find");

        // The top-level function is walked too: `hi` in the ternary.
        let (line, col) = at(&doc, "hi : n");
        assert!(coloured(line, col), "{tokens:?}");
        // And named: `fn` is a keyword, `clamp` a function.
        assert!(tokens.contains(&(0, 0, 2, 0)), "{tokens:?}");
        assert!(tokens.contains(&(0, 3, 5, 2)), "{tokens:?}");
    }

    #[test]
    fn test_semantic_tokens_handle_angles_and_style_values() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = concat!(
            "View Main() {\n",
            "  var turn = 10\n",
            "  Box #[rotate: 90deg, bg: style { gap: turn }]\n",
            "}\n"
        )
        .to_string();
        let doc = Document::new(uri, Some(1), content);
        let tokens = decode(&doc);
        let (line, col) = at(&doc, "90deg");
        assert!(
            tokens
                .iter()
                .any(|&(l, c, _, ty)| (l, c, ty) == (line, col, 6)),
            "an angle is a number: {tokens:?}"
        );
        let (line, col) = at(&doc, "turn }");
        assert!(
            tokens
                .iter()
                .any(|&(l, c, _, ty)| (l, c, ty) == (line, col, 3)),
            "a name inside a style value is a variable: {tokens:?}"
        );
        let (line, col) = at(&doc, "gap:");
        assert!(
            tokens
                .iter()
                .any(|&(l, c, _, ty)| (l, c, ty) == (line, col, 4)),
            "and its attributes are properties: {tokens:?}"
        );
    }
}
