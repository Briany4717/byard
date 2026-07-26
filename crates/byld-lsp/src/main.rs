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
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentSymbolRequest, Formatting, GotoDefinition,
    HoverRequest, PrepareRenameRequest, Rename, SemanticTokensFullRequest, Request as _,
};
use lsp_types::{
    CodeActionOptions, CodeActionProviderCapability, CompletionOptions, HoverProviderCapability,
    OneOf, PublishDiagnosticsParams, RenameOptions, SemanticTokensFullOptions,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};

use state::document::DocumentStore;

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

    if let Ok(init_p) = serde_json::from_value::<lsp_types::InitializeParams>(params) {
        eprintln!(
            "[byld-lsp] Client connected: {:?}",
            init_p.client_info.map(|c| c.name)
        );
    } else {
        eprintln!("[byld-lsp] Could not parse client InitializeParams, proceeding safely.");
    }

    if let Err(e) = connection.initialize_finish(id, initialize_result) {
        eprintln!("[byld-lsp] Error during initialize_finish: {e}");
        return Err(e.into());
    }

    eprintln!("[byld-lsp] Handshake complete. Running main loop...");
    main_loop(connection)?;

    io_threads.join()?;
    eprintln!("[byld-lsp] Server shut down cleanly.");
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn main_loop(connection: Connection) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let document_store = DocumentStore::new();

    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req).unwrap_or(false) {
                    eprintln!("[byld-lsp] Shutdown request received.");
                    return Ok(());
                }

                dispatch_request(&connection, &document_store, req);
            }
            Message::Notification(not) => {
                match not.method.as_str() {
                    DidOpenTextDocument::METHOD => {
                        if let Ok(params) =
                            serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(not.params)
                        {
                            let doc = document_store.insert(
                                params.text_document.uri,
                                Some(params.text_document.version),
                                params.text_document.text,
                            );
                            publish_diagnostics_safe(&connection, &doc);
                        } else {
                            eprintln!("[byld-lsp] Failed to parse DidOpenTextDocument params");
                        }
                    }
                    DidChangeTextDocument::METHOD => {
                        if let Ok(params) =
                            serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(
                                not.params,
                            )
                        {
                            if let Some(change) = params.content_changes.into_iter().next() {
                                let doc = document_store.insert(
                                    params.text_document.uri,
                                    Some(params.text_document.version),
                                    change.text,
                                );
                                publish_diagnostics_safe(&connection, &doc);
                            }
                        } else {
                            eprintln!("[byld-lsp] Failed to parse DidChangeTextDocument params");
                        }
                    }
                    DidSaveTextDocument::METHOD => {
                        if let Ok(params) =
                            serde_json::from_value::<lsp_types::DidSaveTextDocumentParams>(not.params)
                        {
                            if let Some(doc) = document_store.get(&params.text_document.uri) {
                                publish_diagnostics_safe(&connection, &doc);
                            }
                        }
                    }
                    DidCloseTextDocument::METHOD => {
                        if let Ok(params) =
                            serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(
                                not.params,
                            )
                        {
                            let uri = params.text_document.uri;
                            document_store.remove(&uri);

                            let clear_params = PublishDiagnosticsParams {
                                uri,
                                diagnostics: Vec::new(),
                                version: None,
                            };
                            let notification = Notification::new(
                                PublishDiagnostics::METHOD.to_string(),
                                clear_params,
                            );
                            let _ = connection.sender.send(Message::Notification(notification));
                        }
                    }
                    "initialized" => {
                        eprintln!("[byld-lsp] Client initialized notification received.");
                    }
                    "exit" => {
                        eprintln!("[byld-lsp] Exit notification received.");
                        return Ok(());
                    }
                    _ => {
                        eprintln!("[byld-lsp] Ignored notification: {}", not.method);
                    }
                }
            }
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
                Err((id, err)) => return send_invalid_params(connection, id, HoverRequest::METHOD, err),
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
                Err((id, err)) => return send_invalid_params(connection, id, Completion::METHOD, err),
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
                    return send_invalid_params(connection, id, GotoDefinition::METHOD, err)
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
                    return send_invalid_params(connection, id, DocumentSymbolRequest::METHOD, err)
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
                Err((id, err)) => return send_invalid_params(connection, id, Formatting::METHOD, err),
            };
            let uri = params.text_document.uri;
            let options = params.options;

            let edits = document_store
                .get(&uri)
                .and_then(|doc| capabilities::formatting::handle_formatting(&doc, options));
            send_response(connection, Response::new_ok(id, edits));
        }
        PrepareRenameRequest::METHOD => {
            let (id, params) = match cast_request::<PrepareRenameRequest>(req) {
                Ok(res) => res,
                Err((id, err)) => {
                    return send_invalid_params(connection, id, PrepareRenameRequest::METHOD, err)
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
                    return send_invalid_params(connection, id, SemanticTokensFullRequest::METHOD, err)
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
                    return send_invalid_params(connection, id, CodeActionRequest::METHOD, err)
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

fn publish_diagnostics_safe(connection: &Connection, doc: &state::document::Document) {
    let params = capabilities::diagnostics::compute_diagnostics(doc);
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
    use lsp_types::{CompletionResponse, GotoDefinitionResponse, Position, Uri};
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
    fn test_formatting_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main{\nColumn{\nText(\"hello\")\n}\n}".to_string();
        let doc = Document::new(uri, Some(1), content);

        let options = lsp_types::FormattingOptions {
            tab_size: 2,
            insert_spaces: true,
            ..Default::default()
        };
        let edits = capabilities::formatting::handle_formatting(&doc, options).unwrap();
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.contains("  Column{\n    Text(\"hello\")"));
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

    #[test]
    fn test_semantic_tokens_capabilities() {
        let uri: Uri = "file:///test.byd".parse().unwrap();
        let content = "View Main {\n  var count = 10\n}".to_string();
        let doc = Document::new(uri, Some(1), content);

        let result = capabilities::semantic_tokens::handle_semantic_tokens(&doc).unwrap();
        if let lsp_types::SemanticTokensResult::Tokens(tokens) = result {
            assert!(!tokens.data.is_empty());
        } else {
            panic!("Expected SemanticTokensResult::Tokens");
        }
    }
}
