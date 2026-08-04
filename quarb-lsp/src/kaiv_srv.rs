//! The kaivrpc codec: the same core over kaiv documents.
//!
//! A request is a document; a response is a document — so an LSP
//! exchange can be saved, diffed, and replayed with `nc -U`. One
//! request per connection on a Unix socket: the client writes its
//! request doc and shuts down the write side; the response doc
//! comes back and the connection closes. No framing to invent —
//! the connection is the frame.
//!
//! Methods (the `quarb-lsp/` namespace):
//!
//! - `quarb-lsp/diagnostics` — params `text`; data rows are the
//!   anchored diagnostics.
//! - `quarb-lsp/complete` — params `text`, `line`, `col`; data
//!   rows are the candidates.
//!
//! This is the door quai's daemon walks through: one warm server
//! process, editors on stdio JSON-RPC, kaiv-native clients here.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use anyhow::{Context, Result};
use kaivrpc::error::RpcError;
use kaivrpc::request::Request;
use kaivrpc::response::{Envelope, Response};
use serde::{Deserialize, Serialize};

use crate::core;

#[derive(Deserialize)]
struct DiagParams {
    text: String,
}

#[derive(Deserialize)]
struct CompleteParams {
    text: String,
    line: u32,
    col: u32,
}

#[derive(Serialize)]
struct DiagRows {
    rows: Vec<core::Diag>,
}

#[derive(Serialize)]
struct ItemRows {
    rows: Vec<core::Item>,
}

pub fn serve(socket: &str) -> Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {socket}"))?;
    eprintln!("quarb-lsp: kaivrpc on {socket}");
    for conn in listener.incoming() {
        let mut conn = conn?;
        if let Err(e) = handle(&mut conn) {
            eprintln!("quarb-lsp: {e}");
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
            "quarb-lsp/diagnostics" => match req.params::<DiagParams>() {
                Ok(p) => {
                    let rows = core::diagnostics(&p.text);
                    let n = rows.len();
                    resp.buffered(&DiagRows { rows }, n).unwrap_or_default()
                }
                Err(e) => resp
                    .error(&RpcError::new(422, "bad-params", e.to_string()))
                    .unwrap_or_default(),
            },
            "quarb-lsp/complete" => match req.params::<CompleteParams>() {
                Ok(p) => {
                    let rows = core::completions(&p.text, p.line, p.col);
                    let n = rows.len();
                    resp.buffered(&ItemRows { rows }, n).unwrap_or_default()
                }
                Err(e) => resp
                    .error(&RpcError::new(422, "bad-params", e.to_string()))
                    .unwrap_or_default(),
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
