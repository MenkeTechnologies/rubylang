//! Language Server Protocol over stdio (`ruby --lsp`).
//!
//! Self-contained and read-only: diagnostics come from the same `parser::parse`
//! the runtime uses (a syntax error maps to the reported line); hover and
//! completion draw on the builtin method/kernel corpus. No output ever reaches
//! the terminal — JSON-RPC on stdio only. Structure follows the sibling `-rs`
//! interpreters' `lsp.rs`.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, ExtractError, Message, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, HoverRequest, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    MarkupContent, MarkupKind, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};

/// The builtin method / kernel names offered for completion and hover.
const CORPUS: &[(&str, &str)] = &[
    (
        "puts",
        "Kernel#puts — write each argument + a newline to stdout.",
    ),
    (
        "print",
        "Kernel#print — write each argument to stdout (no newline).",
    ),
    (
        "p",
        "Kernel#p — write `inspect` of each argument; returns the argument.",
    ),
    (
        "require",
        "Kernel#require — no-op in rubylang (returns true).",
    ),
    (
        "raise",
        "Kernel#raise — raise a runtime error with the given message.",
    ),
    ("loop", "Kernel#loop — run the block forever until `break`."),
    (
        "each",
        "Enumerable#each — yield every element to the block.",
    ),
    (
        "map",
        "Enumerable#map — collect the block's result for each element.",
    ),
    (
        "select",
        "Enumerable#select — keep elements the block is truthy for.",
    ),
    (
        "reject",
        "Enumerable#reject — drop elements the block is truthy for.",
    ),
    (
        "reduce",
        "Enumerable#reduce — fold the collection with the block.",
    ),
    ("times", "Integer#times — yield 0..self-1 to the block."),
    ("upto", "Integer#upto — yield self..limit ascending."),
    ("length", "length/size — element or character count."),
    ("push", "Array#push — append elements, mutating the array."),
    (
        "join",
        "Array#join — concatenate elements with a separator.",
    ),
    ("keys", "Hash#keys — an array of the hash's keys."),
    ("values", "Hash#values — an array of the hash's values."),
    ("upcase", "String#upcase — an uppercased copy."),
    (
        "split",
        "String#split — split into an array on a separator.",
    ),
    ("to_s", "to_s — the string form of any object."),
    ("to_i", "to_i — the integer form."),
    ("inspect", "inspect — the debug string form."),
];

/// The builtin corpus, exposed for the offline docs generator (`gen-docs`).
pub fn corpus() -> &'static [(&'static str, &'static str)] {
    CORPUS
}

type Docs = HashMap<String, String>;

/// Entry point for `ruby --lsp`.
pub fn run() -> Result<(), String> {
    spawn_orphan_guard();
    let (conn, io_threads) = Connection::stdio();
    let (init_id, _params) = conn
        .initialize_start()
        .map_err(|e| format!("lsp initialize: {e}"))?;
    let init_result = serde_json::json!({
        "capabilities": server_capabilities(),
        "serverInfo": { "name": "rubylang", "version": env!("CARGO_PKG_VERSION") },
    });
    conn.sender
        .send(Response::new_ok(init_id, init_result).into())
        .map_err(|e| format!("lsp send: {e}"))?;

    let mut docs: Docs = HashMap::new();
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn
                    .handle_shutdown(&req)
                    .map_err(|e| format!("lsp shutdown: {e}"))?
                {
                    break;
                }
                dispatch_request(&conn, req);
            }
            Message::Notification(not) => dispatch_notification(&conn, &mut docs, not),
            Message::Response(_) => {}
        }
    }
    drop(conn);
    io_threads.join().map_err(|_| "lsp io join".to_string())?;
    Ok(())
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..Default::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

fn handle<P, R>(conn: &Connection, req: Request, f: impl FnOnce(P) -> R)
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
{
    let method = req.method.clone();
    let id = req.id.clone();
    match req.extract::<P>(&method) {
        Ok((id, params)) => {
            let value = serde_json::to_value(f(params)).unwrap_or(serde_json::Value::Null);
            let _ = conn.sender.send(Response::new_ok(id, value).into());
        }
        Err(ExtractError::JsonError { error, .. }) => {
            let _ = conn.sender.send(
                Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()).into(),
            );
        }
        Err(ExtractError::MethodMismatch(_)) => unreachable!("method matched before extract"),
    }
}

fn dispatch_request(conn: &Connection, req: Request) {
    match req.method.as_str() {
        Completion::METHOD => handle(conn, req, |_p: CompletionParams| completions()),
        HoverRequest::METHOD => handle(conn, req, |_p: HoverParams| hover()),
        _ => {
            let _ = conn.sender.send(
                Response::new_err(req.id, ErrorCode::MethodNotFound as i32, "unhandled".into())
                    .into(),
            );
        }
    }
}

fn dispatch_notification(conn: &Connection, docs: &mut Docs, not: lsp_server::Notification) {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.insert(uri.as_str().to_string(), p.text_document.text.clone());
                publish_diagnostics(conn, &uri, &p.text_document.text);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(not.params) {
                if let Some(change) = p.content_changes.into_iter().last() {
                    let uri = p.text_document.uri;
                    docs.insert(uri.as_str().to_string(), change.text.clone());
                    publish_diagnostics(conn, &uri, &change.text);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.remove(uri.as_str());
                publish_diagnostics(conn, &uri, "");
            }
        }
        _ => {}
    }
}

fn completions() -> CompletionResponse {
    let items = CORPUS
        .iter()
        .map(|(name, doc)| CompletionItem {
            label: name.to_string(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some((*doc).to_string()),
            ..Default::default()
        })
        .collect();
    CompletionResponse::Array(items)
}

fn hover() -> Hover {
    let body = "**rubylang** — Ruby on the fusevm bytecode VM + Cranelift JIT.".to_string();
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: body,
        }),
        range: None,
    }
}

fn publish_diagnostics(conn: &Connection, uri: &Uri, text: &str) {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: compute_diagnostics(text),
        version: None,
    };
    let not = lsp_server::Notification::new(PublishDiagnostics::METHOD.to_string(), params);
    let _ = conn.sender.send(not.into());
}

/// Compile the whole document; a syntax error maps to a single diagnostic on the
/// line named in its `line N:` prefix (or line 0 if unlabeled).
fn compute_diagnostics(text: &str) -> Vec<Diagnostic> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    match crate::parser::parse(text) {
        Ok(_) => Vec::new(),
        Err(e) => {
            let line = parse_error_line(&e).saturating_sub(1);
            vec![Diagnostic {
                range: Range {
                    start: Position { line, character: 0 },
                    end: Position {
                        line,
                        character: 200,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                message: e,
                ..Default::default()
            }]
        }
    }
}

/// Extract the `line N` number from a parser error message.
fn parse_error_line(e: &str) -> u32 {
    e.strip_prefix("line ")
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// Exit if reparented to pid 1 (the editor died) so we never leak.
fn spawn_orphan_guard() {
    std::thread::spawn(|| {
        #[cfg(target_os = "linux")]
        // SAFETY: prctl(PR_SET_PDEATHSIG, ...) only registers a signal disposition.
        unsafe {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            // SAFETY: getppid takes no arguments and never fails.
            if unsafe { libc::getppid() } == 1 {
                std::process::exit(0);
            }
        }
    });
}
