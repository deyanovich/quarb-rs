//! The kaivrpc codec: the same core over kaiv documents — the
//! door the vim and VS Code extensions walk through. A request
//! is a document; a response is a document; the connection is
//! the frame (one request per Unix connection, exactly as
//! quarb-lsp serves).
//!
//! Methods (the `quarb-code-lsp/` namespace), all word-based —
//! kaiv-native clients extract the identifier themselves (or
//! pass `line`/`col` with `text` and let `word-at` do it):
//!
//! - `quarb-code-lsp/symbols` — params `text`, `lang` (rs / py /
//!   js / c): the outline rows.
//! - `quarb-code-lsp/definition` — params `root`, `word`, and
//!   optionally `file`+`text` for the overlay: locations.
//! - `quarb-code-lsp/references` — same params: call sites.
//! - `quarb-code-lsp/hover` — same params: signature + doc rows.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use anyhow::{Context, Result};
use kaivrpc::error::RpcError;
use kaivrpc::request::Request;
use kaivrpc::response::{Envelope, Response};
use serde::{Deserialize, Serialize};

use crate::core::{self, Workspace};

#[derive(Deserialize)]
struct SymbolsParams {
    /// Inline source, with `lang` (rs / py / js / c) — or `file`,
    /// read from disk with the language inferred from the
    /// extension (the vim client's path: `:update`, then ask).
    text: Option<String>,
    lang: Option<String>,
    file: Option<String>,
}

#[derive(Deserialize)]
struct WordParams {
    root: Option<String>,
    file: Option<String>,
    text: Option<String>,
    word: String,
}

#[derive(Deserialize)]
struct QueryParams {
    root: Option<String>,
    file: Option<String>,
    text: Option<String>,
    query: String,
    /// "file" pins the query to the open file; anything else
    /// (or absent) runs over the workspace forest.
    scope: Option<String>,
}

#[derive(Serialize)]
struct ValueRow {
    value: String,
}

#[derive(Serialize)]
struct ValueRows {
    rows: Vec<ValueRow>,
}

#[derive(Serialize)]
struct SymbolRows {
    rows: Vec<core::Symbol>,
}

#[derive(Serialize)]
struct LocationRows {
    rows: Vec<core::Location>,
}

#[derive(Serialize)]
struct HoverRows {
    rows: Vec<core::HoverRow>,
}

pub fn serve(socket: &str) -> Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {socket}"))?;
    eprintln!("quarb-code-lsp: kaivrpc on {socket}");
    for conn in listener.incoming() {
        let mut conn = conn?;
        if let Err(e) = handle(&mut conn) {
            eprintln!("quarb-code-lsp: {e}");
        }
    }
    Ok(())
}

fn handle(conn: &mut UnixStream) -> Result<()> {
    let mut body = Vec::new();
    conn.read_to_end(&mut body)?;
    let out = answer(&body);
    conn.write_all(out.as_bytes())?;
    Ok(())
}

/// One word-based workspace, per request (the connection is the
/// frame; state is the client's).
fn word_workspace(p: &WordParams) -> (Workspace, String) {
    let mut ws = Workspace::new(p.root.as_ref().map(PathBuf::from));
    let uri = match (&p.file, &p.text) {
        (Some(f), Some(t)) => {
            let uri = format!("file://{f}");
            ws.open(&uri, t);
            uri
        }
        (Some(f), None) => {
            let uri = format!("file://{f}");
            if let Ok(t) = std::fs::read_to_string(f) {
                ws.open(&uri, &t);
            }
            uri
        }
        _ => String::new(),
    };
    (ws, uri)
}

/// Decode, dispatch, encode — the whole exchange, also the unit
/// tests' entry (no socket needed).
pub fn answer(body: &[u8]) -> String {
    let envelope = Envelope::kaivrpc();
    let resp = Response::new(envelope, None, &[]);
    match Request::parse(body) {
        Err(e) => resp
            .error(&RpcError::new(400, "bad-request", e.to_string()))
            .unwrap_or_default(),
        Ok(req) => match req.method() {
            "quarb-code-lsp/symbols" => match req.params::<SymbolsParams>() {
                Ok(p) => {
                    let mut ws = Workspace::new(None);
                    let uri = match (&p.file, &p.text) {
                        (Some(f), _) => {
                            let uri = format!("file://{f}");
                            if let Ok(t) = std::fs::read_to_string(f) {
                                ws.open(&uri, &t);
                            }
                            uri
                        }
                        (None, Some(t)) => {
                            let uri = format!(
                                "file://untitled.{}",
                                p.lang.as_deref().unwrap_or("")
                            );
                            ws.open(&uri, t);
                            uri
                        }
                        (None, None) => String::new(),
                    };
                    let rows = ws.symbols(&uri);
                    let n = rows.len();
                    resp.buffered(&SymbolRows { rows }, n).unwrap_or_default()
                }
                Err(e) => bad_params(&resp, e),
            },
            "quarb-code-lsp/definition" => match req.params::<WordParams>() {
                Ok(p) => {
                    let (ws, uri) = word_workspace(&p);
                    let rows = ws.definition(&uri, &p.word);
                    let n = rows.len();
                    resp.buffered(&LocationRows { rows }, n).unwrap_or_default()
                }
                Err(e) => bad_params(&resp, e),
            },
            "quarb-code-lsp/references" => match req.params::<WordParams>() {
                Ok(p) => {
                    let (ws, uri) = word_workspace(&p);
                    let rows = ws.references(&uri, &p.word);
                    let n = rows.len();
                    resp.buffered(&LocationRows { rows }, n).unwrap_or_default()
                }
                Err(e) => bad_params(&resp, e),
            },
            "quarb-code-lsp/hover" => match req.params::<WordParams>() {
                Ok(p) => {
                    let (ws, uri) = word_workspace(&p);
                    let rows = ws.hover(&uri, &p.word);
                    let n = rows.len();
                    resp.buffered(&HoverRows { rows }, n).unwrap_or_default()
                }
                Err(e) => bad_params(&resp, e),
            },
            // The query door: any code-level query, results as
            // locations (nodes) or values; an engine refusal
            // comes back verbatim as the error — what refuses
            // here refuses in qua.
            "quarb-code-lsp/query" => match req.params::<QueryParams>() {
                Ok(p) => {
                    let wp = WordParams {
                        root: p.root,
                        file: p.file,
                        text: p.text,
                        word: String::new(),
                    };
                    let (ws, uri) = word_workspace(&wp);
                    let file_only = p.scope.as_deref() == Some("file");
                    match ws.query(&uri, &p.query, file_only) {
                        core::QueryAnswer::Locations(rows) => {
                            let n = rows.len();
                            resp.buffered(&LocationRows { rows }, n).unwrap_or_default()
                        }
                        core::QueryAnswer::Values(vals) => {
                            let rows: Vec<ValueRow> =
                                vals.into_iter().map(|value| ValueRow { value }).collect();
                            let n = rows.len();
                            resp.buffered(&ValueRows { rows }, n).unwrap_or_default()
                        }
                        core::QueryAnswer::Refused(msg) => resp
                            .error(&RpcError::new(422, "refused", msg))
                            .unwrap_or_default(),
                    }
                }
                Err(e) => bad_params(&resp, e),
            },
            other => resp
                .error(&RpcError::new(
                    404,
                    "no-such-method",
                    format!("unknown method {other:?}"),
                ))
                .unwrap_or_default(),
        },
    }
}

fn bad_params(resp: &Response, e: impl std::fmt::Display) -> String {
    resp.error(&RpcError::new(422, "bad-params", e.to_string()))
        .unwrap_or_default()
}
