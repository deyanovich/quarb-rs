//! Browser playground entry point: run a Quarb query over pasted
//! text in one of the text-based formats, entirely client-side.
//!
//! One exported function, [`run`], dispatches on the format name,
//! parses the input with the matching adapter, executes the query,
//! and returns a JSON envelope (`{"ok":true,"lines":[...]}` or
//! `{"ok":false,"error":"..."}`). Rendering mirrors `qua`: node
//! results render through the adapter's pointer/locator, value
//! results through their display form.
//!
//! The shell stage stays gated: no `AllowShell` wrapper here, so
//! `sh()` / backticks fail with the engine's normal gate error.
//! The invocation instant for `now()` is supplied by the caller
//! (`Date.now()` in the page) — wasm32-unknown-unknown has no
//! clock of its own.

use quarb::adapter::WithNow;
use quarb::{AstAdapter, NodeId, QueryResult};
use wasm_bindgen::prelude::*;

/// The engine version shown in the playground footer.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Execute `query` against `input` parsed as `format`
/// (json | yaml | toml | csv | tsv | xml | html | markdown).
/// `now_millis` is the invocation instant for `now()`, as from
/// `Date.now()`. Returns a JSON envelope; never throws.
#[wasm_bindgen]
pub fn run(format: &str, input: &str, query: &str, now_millis: f64) -> String {
    let secs = (now_millis / 1000.0).floor() as i64;
    let nanos = ((now_millis / 1000.0).fract() * 1e9) as u32;
    let outcome = match format {
        "json" => quarb_json::JsonAdapter::parse(input)
            .map_err(|e| format!("parsing JSON: {e}"))
            .and_then(|a| go(query, &a, |n| a.pointer(n), secs, nanos)),
        "yaml" => quarb_yaml::parse(input)
            .map_err(|e| format!("parsing YAML: {e}"))
            .and_then(|a| go(query, &a, |n| a.pointer(n), secs, nanos)),
        "toml" => quarb_toml::parse(input)
            .map_err(|e| format!("parsing TOML: {e}"))
            .and_then(|a| go(query, &a, |n| a.pointer(n), secs, nanos)),
        "csv" | "tsv" => {
            let delim = if format == "tsv" { b'\t' } else { b',' };
            quarb_csv::CsvAdapter::parse_with_delimiter(input, delim)
                .map_err(|e| format!("parsing CSV: {e}"))
                .and_then(|a| go(query, &a, |n| a.locator(n), secs, nanos))
        }
        "xml" => quarb_xml::XmlAdapter::parse(input)
            .map_err(|e| format!("parsing XML: {e}"))
            .and_then(|a| go(query, &a, |n| a.locator(n), secs, nanos)),
        "html" => {
            let a = quarb_html::HtmlAdapter::parse(input);
            go(query, &a, |n| a.locator(n), secs, nanos)
        }
        "markdown" => {
            let a = quarb_markdown::parse(input);
            go(query, &a, |n| a.locator(n), secs, nanos)
        }
        other => Err(format!("unknown format: {other}")),
    };
    match outcome {
        Ok(lines) => serde_json::json!({ "ok": true, "lines": lines }).to_string(),
        Err(e) => serde_json::json!({ "ok": false, "error": e }).to_string(),
    }
}

fn go<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    secs: i64,
    nanos: u32,
) -> Result<Vec<String>, String> {
    let nowed = WithNow {
        inner: adapter,
        secs,
        nanos,
    };
    match quarb::run(query, &nowed) {
        Ok(QueryResult::Nodes(nodes)) => Ok(nodes.into_iter().map(render).collect()),
        Ok(QueryResult::Values(values)) => {
            Ok(values.into_iter().map(|v| v.to_string()).collect())
        }
        Err(e) => Err(e.to_string()),
    }
}
