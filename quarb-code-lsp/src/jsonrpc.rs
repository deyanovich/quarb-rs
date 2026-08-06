//! The LSP codec: the code level's reading gestures on standard
//! JSON-RPC — documentSymbol, definition, references, hover,
//! workspace/symbol. Framing shared with quarb-lsp.

use anyhow::{Result, bail};
use quarb_lsp::framing::{read_message, respond, respond_error, str_at};
use serde_json::{Value, json};

use crate::core::{self, Symbol, Workspace, word_at};

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut ws = Workspace::new(None);
    let mut texts: std::collections::HashMap<String, String> = Default::default();
    let mut shutdown = false;

    loop {
        let msg = match read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => bail!("framing: {e}"),
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                let root = str_at(&params, &["rootUri"]);
                if !root.is_empty() {
                    ws = Workspace::new(Some(core::uri_to_path(&root)));
                }
                respond(&mut writer, id, json!({
                    "capabilities": {
                        "textDocumentSync": 1,
                        "documentSymbolProvider": true,
                        "definitionProvider": true,
                        "referencesProvider": true,
                        "hoverProvider": true,
                        "workspaceSymbolProvider": true
                    },
                    "serverInfo": {
                        "name": "quarb-code-lsp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }))?;
            }
            "initialized" => {}
            "shutdown" => {
                shutdown = true;
                respond(&mut writer, id, Value::Null)?;
            }
            "exit" => std::process::exit(i32::from(!shutdown)),
            "textDocument/didOpen" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                let text = str_at(&params, &["textDocument", "text"]);
                ws.open(&uri, &text);
                texts.insert(uri, text);
            }
            "textDocument/didChange" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                let text = params["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                    .unwrap_or("")
                    .to_string();
                ws.open(&uri, &text);
                texts.insert(uri, text);
            }
            "textDocument/didSave" => ws.saved(),
            "textDocument/didClose" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                ws.close(&uri);
                texts.remove(&uri);
            }
            "textDocument/documentSymbol" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                respond(&mut writer, id, nest_symbols(&ws.symbols(&uri)))?;
            }
            "textDocument/definition" | "textDocument/references" | "textDocument/hover" => {
                let uri = str_at(&params, &["textDocument", "uri"]);
                let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
                let col = params["position"]["character"].as_u64().unwrap_or(0) as u32;
                let empty = String::new();
                let word = word_at(texts.get(&uri).unwrap_or(&empty), line, col);
                let result = match (method, word) {
                    (_, None) => Value::Null,
                    ("textDocument/definition", Some(w)) => {
                        locations(&ws.definition(&uri, &w))
                    }
                    ("textDocument/references", Some(w)) => {
                        locations(&ws.references(&uri, &w))
                    }
                    (_, Some(w)) => hover(&ws.hover(&uri, &w)),
                };
                respond(&mut writer, id, result)?;
            }
            // The vendor door: any code-level query over JSON-RPC —
            // the same rows the kaivrpc method answers ({value} or
            // {file, line, locator}), so both codecs tell one
            // story. A refusal is a RequestFailed error carrying
            // the engine's message verbatim.
            "quarb/query" => {
                let query = str_at(&params, &["query"]);
                let scope = str_at(&params, &["scope"]);
                let doc_uri = str_at(&params, &["textDocument", "uri"]);
                let uri = if doc_uri.is_empty() {
                    texts.keys().next().cloned().unwrap_or_default()
                } else {
                    doc_uri
                };
                match ws.query(&uri, &query, scope == "file") {
                    core::QueryAnswer::Refused(msg) => {
                        respond_error(&mut writer, id, -32803, &msg)?
                    }
                    core::QueryAnswer::Values(vals) => {
                        let rows: Vec<Value> =
                            vals.into_iter().map(|value| json!({"value": value})).collect();
                        respond(&mut writer, id, json!({"rows": rows}))?
                    }
                    core::QueryAnswer::Locations(rows) => {
                        let rows: Vec<Value> = rows
                            .iter()
                            .map(|l| {
                                json!({
                                    "file": l.file,
                                    "line": l.line,
                                    "locator": l.locator,
                                    "location": location_json(l)
                                })
                            })
                            .collect();
                        respond(&mut writer, id, json!({"rows": rows}))?
                    }
                }
            }
            "workspace/symbol" => {
                let query = str_at(&params, &["query"]);
                let uri = texts.keys().next().cloned().unwrap_or_default();
                let rows = ws.workspace_symbols(&query, &uri);
                let out: Vec<Value> = rows
                    .iter()
                    .map(|l| {
                        json!({
                            "name": l.locator.rsplit('/').next().unwrap_or(&l.locator),
                            "kind": 12,
                            "location": location_json(l)
                        })
                    })
                    .collect();
                respond(&mut writer, id, json!(out))?;
            }
            _ if id.is_some() => respond(&mut writer, id, Value::Null)?,
            _ => {}
        }
    }
    Ok(())
}

fn location_json(l: &core::Location) -> Value {
    let line = l.line.saturating_sub(1);
    json!({
        "uri": format!("file://{}", l.file),
        "range": {
            "start": {"line": line, "character": 0},
            "end": {"line": line, "character": 0}
        }
    })
}

fn locations(rows: &[core::Location]) -> Value {
    json!(rows.iter().map(location_json).collect::<Vec<_>>())
}

fn hover(rows: &[core::HoverRow]) -> Value {
    let Some(first) = rows.first() else {
        return Value::Null;
    };
    let mut md = String::new();
    if let Some(sig) = &first.signature {
        md.push_str(&format!("```\n{sig}\n```\n"));
    } else {
        md.push_str(&format!("`{}` ({})\n", first.name, first.construct));
    }
    if let Some(doc) = &first.doc {
        md.push_str("\n");
        md.push_str(doc);
    }
    if rows.len() > 1 {
        md.push_str(&format!("\n\n_{} more candidates (fan-out)_", rows.len() - 1));
    }
    json!({"contents": {"kind": "markdown", "value": md}})
}

/// Flat pre-order symbols → nested DocumentSymbols, by depth.
fn nest_symbols(flat: &[Symbol]) -> Value {
    fn build(flat: &[Symbol], i: &mut usize, depth: u32) -> Vec<Value> {
        let mut out = Vec::new();
        while *i < flat.len() {
            let s = &flat[*i];
            if s.depth < depth {
                break;
            }
            *i += 1;
            let range = json!({
                "start": {"line": s.start_line - 1, "character": s.start_col},
                "end": {"line": s.end_line - 1, "character": s.end_col}
            });
            let children = build(flat, i, depth + 1);
            out.push(json!({
                "name": s.name,
                "detail": s.signature,
                "kind": s.kind,
                "range": range,
                "selectionRange": range,
                "children": children
            }));
        }
        out
    }
    json!(build(flat, &mut 0, 0))
}
