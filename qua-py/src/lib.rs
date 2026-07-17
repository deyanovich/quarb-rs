//! Python bindings for the Quarb engine via PyO3 / maturin.
//!
//! The Rust extension module `quarb._quarb`. The user-facing API is the
//! `quarb` Python module (dist: qua-cli); `python/quarb/__init__.py` re-exports from
//! this module. See the project README for usage.
//!
//! Dispatch mirrors `quarb-wasm`: parse the input with the matching
//! text-format adapter, execute the query, and render node results
//! through the adapter's pointer/locator, value results through
//! their display form. Errors surface as Python `ValueError` with
//! the engine's message — no envelopes.
//!
//! The shell stage stays gated: no `AllowShell` wrapper here, so
//! `sh()` / backticks fail with the engine's normal gate error.
//! Unlike wasm, the native target has a clock of its own, so the
//! invocation instant for `now()` is `SystemTime::now()` at call
//! time.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use quarb::adapter::WithNow;
use quarb::{AstAdapter, NodeId, QueryResult};

/// The invocation instant for `now()`, as (seconds, subsecond
/// nanoseconds) since the Unix epoch.
fn now_parts() -> (i64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        // A pre-epoch system clock; practically unreachable.
        Err(e) => (-(e.duration().as_secs() as i64), 0),
    }
}

/// Execute `query` against `input` parsed as `format`
/// (json | yaml | toml | csv | tsv | xml | html | markdown).
fn dispatch(query: &str, input: &str, format: &str) -> Result<Vec<String>, String> {
    let (secs, nanos) = now_parts();
    match format {
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
        Ok(QueryResult::Values(values)) => Ok(values.into_iter().map(|v| v.to_string()).collect()),
        Err(e) => Err(e.to_string()),
    }
}

/// Execute `query` against `input` parsed as `format`
/// (json | yaml | toml | csv | tsv | xml | html | markdown) and
/// return the result lines.
///
/// Node results render through the adapter's pointer/locator; value
/// results through their display form — the same rendering `qua`
/// uses. Parse and execution errors raise `ValueError` with the
/// engine's message.
#[pyfunction]
fn run(query: &str, input: &str, format: &str) -> PyResult<Vec<String>> {
    dispatch(query, input, format).map_err(PyValueError::new_err)
}

/// Execute `query` against the file at `path`, inferring the format
/// from the extension (.json .yaml/.yml .toml .csv .tsv .xml
/// .html/.htm .md/.markdown).
///
/// An unknown extension raises `ValueError`; an unreadable file
/// raises `OSError`; parse and execution errors raise `ValueError`
/// with the engine's message, as with `run`.
#[pyfunction]
fn run_file(query: &str, path: PathBuf) -> PyResult<Vec<String>> {
    let format = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => "json",
        Some("yaml" | "yml") => "yaml",
        Some("toml") => "toml",
        Some("csv") => "csv",
        Some("tsv") => "tsv",
        Some("xml") => "xml",
        Some("html" | "htm") => "html",
        Some("md" | "markdown") => "markdown",
        _ => {
            return Err(PyValueError::new_err(format!(
                "cannot infer format from extension: {}",
                path.display()
            )));
        }
    };
    let input = std::fs::read_to_string(&path)?;
    dispatch(query, &input, format).map_err(PyValueError::new_err)
}

#[pymodule]
fn _quarb(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    m.add_function(wrap_pyfunction!(run_file, m)?)?;
    Ok(())
}
