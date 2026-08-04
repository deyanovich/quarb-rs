//! The LSP codec: JSON-RPC 2.0 with Content-Length framing over
//! stdio — the transport every editor speaks natively.
//!
//! v1 surface: initialize/shutdown lifecycle, full-text document
//! sync (didOpen/didChange → publishDiagnostics), and completion.
//! The completion trigger characters are Quarb's own seams: the
//! pipes, the path separator, the colon ladder, `$`, and `<`.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::core;

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut docs: HashMap<String, String> = HashMap::new();
    let mut shutdown = false;

    loop {
        let msg = match read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break, // stdin closed
            Err(e) => bail!("framing: {e}"),
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                respond(&mut writer, id, json!({
                    "capabilities": {
                        "textDocumentSync": 1,
                        "completionProvider": {
                            "triggerCharacters": ["|", "/", ":", "$", "<", "-", "@"]
                        }
                    },
                    "serverInfo": {
                        "name": "quarb-lsp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }))?;
            }
            "initialized" => {}
            "shutdown" => {
                shutdown = true;
                respond(&mut writer, id, Value::Null)?;
            }
            "exit" => {
                std::process::exit(i32::from(!shutdown));
            }
            "textDocument/didOpen" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                let text = str_at(&params, &["textDocument", "text"]);
                docs.insert(uri.clone(), text.clone());
                publish(&mut writer, &uri, &text)?;
            }
            "textDocument/didChange" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                // Full sync: the last content change carries the text.
                let text = params["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                    .unwrap_or("")
                    .to_string();
                docs.insert(uri.clone(), text.clone());
                publish(&mut writer, &uri, &text)?;
            }
            "textDocument/didClose" => {
                docs.remove(&str_at(&params, &["textDocument", "uri"]));
            }
            "textDocument/completion" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
                let character =
                    params["position"]["character"].as_u64().unwrap_or(0) as u32;
                let empty = String::new();
                let text = docs.get(&uri).unwrap_or(&empty);
                let items: Vec<Value> = core::completions(text, line, character)
                    .into_iter()
                    .map(|i| {
                        json!({
                            "label": i.label,
                            "kind": lsp_kind(i.kind),
                            "detail": i.kind,
                        })
                    })
                    .collect();
                respond(&mut writer, id, json!(items))?;
            }
            // Requests we don't implement get an empty success, so
            // editors never hang on a missing answer; notifications
            // fall through silently.
            _ if id.is_some() => respond(&mut writer, id, Value::Null)?,
            _ => {}
        }
    }
    Ok(())
}

/// LSP CompletionItemKind.
fn lsp_kind(kind: &str) -> u32 {
    match kind {
        "function" | "aggregate" => 3, // Function
        "register" => 6,               // Variable
        "child" => 5,                  // Field
        "property" => 10,              // Property
        "trait" => 7,                  // Class
        _ => 1,
    }
}

fn str_at(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_str().unwrap_or("").to_string()
}

fn publish(w: &mut impl Write, uri: &str, text: &str) -> Result<()> {
    let diags: Vec<Value> = core::diagnostics(text)
        .into_iter()
        .map(|d| {
            json!({
                "range": {
                    "start": {"line": d.line, "character": d.col_start},
                    "end": {"line": d.line, "character": d.col_end}
                },
                "severity": 1,
                "source": "quarb",
                "message": d.message
            })
        })
        .collect();
    notify(w, "textDocument/publishDiagnostics", json!({
        "uri": uri,
        "diagnostics": diags
    }))
}

fn respond(w: &mut impl Write, id: Option<Value>, result: Value) -> Result<()> {
    write_message(w, &json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    }))
}

fn notify(w: &mut impl Write, method: &str, params: Value) -> Result<()> {
    write_message(w, &json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    }))
}

fn write_message(w: &mut impl Write, v: &Value) -> Result<()> {
    let body = serde_json::to_string(v)?;
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()?;
    Ok(())
}

fn read_message(r: &mut impl BufRead) -> Result<Option<Value>> {
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
