//! JSON-RPC 2.0 with Content-Length framing over stdio — the
//! transport every editor speaks natively. Shared by quarb-lsp
//! and quarb-code-lsp; each server owns its dispatch, this
//! module owns the bytes.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Reply to a request: `{jsonrpc, id, result}`.
pub fn respond(w: &mut impl Write, id: Option<Value>, result: Value) -> Result<()> {
    write_message(w, &json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    }))
}

/// Reply with a JSON-RPC error: `{jsonrpc, id, error}`.
pub fn respond_error(
    w: &mut impl Write,
    id: Option<Value>,
    code: i64,
    message: &str,
) -> Result<()> {
    write_message(w, &json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {"code": code, "message": message}
    }))
}

/// Send a server-initiated notification: `{jsonrpc, method, params}`.
pub fn notify(w: &mut impl Write, method: &str, params: Value) -> Result<()> {
    write_message(w, &json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    }))
}

pub fn write_message(w: &mut impl Write, v: &Value) -> Result<()> {
    let body = serde_json::to_string(v)?;
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()?;
    Ok(())
}

/// One framed message, or `None` on a closed stream.
pub fn read_message(r: &mut impl BufRead) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            content_length = Some(v.trim().parse().context("Content-Length")?);
        }
    }
    let len = content_length.context("missing Content-Length")?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

/// A string at a JSON path, `""` when absent.
pub fn str_at(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_str().unwrap_or("").to_string()
}
