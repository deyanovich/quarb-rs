//! Python bindings for the Quarb engine via PyO3 / maturin.
//!
//! The Rust extension module `quarb._quarb`. The user-facing API is
//! the `quarb` module (dist: quarb); `python/quarb/__init__.py`
//! re-exports from this module. See the project README for usage.
//!
//! Two layers:
//!
//! - The pythonic layer: [`loads`] / [`load`] parse once into a
//!   [`Document`], whose `values` / `value` / `records` / `nodes`
//!   return *typed* results — ints, floats, `datetime`,
//!   `timedelta`, [`Quantity`], dicts for records — via
//!   [`value_to_py`].
//! - The string-faithful layer: [`run`] / [`run_file`] mirror the
//!   qua CLI exactly (result lines as strings; node results render
//!   through the adapter's pointer/locator).
//!
//! Errors surface as Python `ValueError` with the engine's message
//! — no envelopes. The shell stage stays gated: no `AllowShell`
//! wrapper here, so `sh()` / backticks fail with the engine's
//! normal gate error. The native target has a clock, so the
//! invocation instant for `now()` is `SystemTime::now()` at call
//! time.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyDict, PyList};
use quarb::adapter::WithNow;
use quarb::{NodeId, QueryResult, Value};

/// The invocation instant for `now()`, as (seconds, subsecond
/// nanoseconds) since the Unix epoch.
fn now_parts() -> (i64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        // A pre-epoch system clock; practically unreachable.
        Err(e) => (-(e.duration().as_secs() as i64), 0),
    }
}

/// A parsed input: one variant per adapter family. JSON-model
/// formats (json/yaml/toml) render node results as pointers, the
/// rest as locators — the same rendering the qua CLI uses.
enum Doc {
    Json(quarb_json::JsonAdapter),
    Csv(quarb_csv::CsvAdapter),
    Xml(quarb_xml::XmlAdapter),
    Html(quarb_html::HtmlAdapter),
}

impl Doc {
    fn parse(input: &str, format: &str) -> Result<Doc, String> {
        match format {
            "json" => quarb_json::JsonAdapter::parse(input)
                .map(Doc::Json)
                .map_err(|e| format!("parsing JSON: {e}")),
            "yaml" => quarb_yaml::parse(input)
                .map(Doc::Json)
                .map_err(|e| format!("parsing YAML: {e}")),
            "toml" => quarb_toml::parse(input)
                .map(Doc::Json)
                .map_err(|e| format!("parsing TOML: {e}")),
            "csv" | "tsv" => {
                let delim = if format == "tsv" { b'\t' } else { b',' };
                quarb_csv::CsvAdapter::parse_with_delimiter(input, delim)
                    .map(Doc::Csv)
                    .map_err(|e| format!("parsing CSV: {e}"))
            }
            "xml" => quarb_xml::XmlAdapter::parse(input)
                .map(Doc::Xml)
                .map_err(|e| format!("parsing XML: {e}")),
            "html" => Ok(Doc::Html(quarb_html::HtmlAdapter::parse(input))),
            "markdown" => Ok(Doc::Html(quarb_markdown::parse(input))),
            other => Err(format!("unknown format: {other}")),
        }
    }

    fn execute(&self, query: &str) -> Result<QueryResult, String> {
        let (secs, nanos) = now_parts();
        macro_rules! go {
            ($a:expr) => {{
                let nowed = WithNow {
                    inner: $a,
                    secs,
                    nanos,
                };
                quarb::run(query, &nowed).map_err(|e| e.to_string())
            }};
        }
        match self {
            Doc::Json(a) => go!(a),
            Doc::Csv(a) => go!(a),
            Doc::Xml(a) => go!(a),
            Doc::Html(a) => go!(a),
        }
    }

    fn render(&self, node: NodeId) -> String {
        match self {
            Doc::Json(a) => a.pointer(node),
            Doc::Csv(a) => a.locator(node),
            Doc::Xml(a) => a.locator(node),
            Doc::Html(a) => a.locator(node),
        }
    }
}

/// Infer the format name from a file extension.
fn format_of(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => Some("json"),
        Some("yaml" | "yml") => Some("yaml"),
        Some("toml") => Some("toml"),
        Some("csv") => Some("csv"),
        Some("tsv") => Some("tsv"),
        Some("xml") => Some("xml"),
        Some("html" | "htm") => Some("html"),
        Some("md" | "markdown") => Some("markdown"),
        _ => None,
    }
}

/// A value on a dimension: the magnitude scaled to the dimension's
/// SI-base expansion (`unit`, e.g. `m`, `kg*m^2/s^3`), with the
/// authored form kept for display (`written`, e.g. `42 km`) when
/// the source carried one.
#[pyclass(frozen, module = "quarb", from_py_object)]
#[derive(Clone, PartialEq)]
struct Quantity {
    #[pyo3(get)]
    magnitude: f64,
    #[pyo3(get)]
    unit: String,
    #[pyo3(get)]
    written: Option<String>,
}

#[pymethods]
impl Quantity {
    fn __repr__(&self) -> String {
        match &self.written {
            Some(w) => format!(
                "Quantity(magnitude={}, unit={:?}, written={:?})",
                self.magnitude, self.unit, w
            ),
            None => format!(
                "Quantity(magnitude={}, unit={:?})",
                self.magnitude, self.unit
            ),
        }
    }

    fn __str__(&self) -> String {
        match &self.written {
            Some(w) => w.clone(),
            None => format!("{} {}", self.magnitude, self.unit),
        }
    }

    fn __eq__(&self, other: &Quantity) -> bool {
        self == other
    }
}

/// Convert an engine [`Value`] to its natural Python object:
/// null → None, booleans/ints/floats/strings to their Python
/// selves, lists → list, records → dict (insertion-ordered),
/// instants → tz-aware `datetime`, durations → `timedelta`,
/// quantities → [`Quantity`].
fn value_to_py(py: Python<'_>, v: &Value) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_py_any(py)?,
        Value::Int(i) => i.into_py_any(py)?,
        Value::Float(f) => f.into_py_any(py)?,
        Value::Str(s) => s.into_py_any(py)?,
        Value::List(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(value_to_py(py, item)?)?;
            }
            list.unbind().into()
        }
        Value::Record(fields) => {
            let dict = PyDict::new(py);
            for (k, val) in fields {
                dict.set_item(k, value_to_py(py, val)?)?;
            }
            dict.unbind().into()
        }
        Value::Instant {
            secs,
            nanos,
            offset_min,
        } => {
            // datetime is microsecond-precision; nanos round down.
            // No source offset means the UTC timeline — the result
            // is tz-aware either way.
            let datetime = py.import("datetime")?;
            let tz = match offset_min {
                Some(m) => {
                    let delta = datetime
                        .getattr("timedelta")?
                        .call1((0, i64::from(*m) * 60))?;
                    datetime.getattr("timezone")?.call1((delta,))?
                }
                None => datetime.getattr("timezone")?.getattr("utc")?,
            };
            datetime
                .getattr("datetime")?
                .call_method1("fromtimestamp", (*secs, &tz))?
                .call_method(
                    "replace",
                    (),
                    Some(&[("microsecond", nanos / 1000)].into_py_dict(py)?),
                )?
                .unbind()
        }
        Value::Duration { secs, nanos } => py
            .import("datetime")?
            .getattr("timedelta")?
            .call1((0, *secs, nanos / 1000))?
            .unbind(),
        Value::Quantity {
            value,
            base,
            written,
        } => Quantity {
            magnitude: *value,
            unit: base.clone(),
            written: written.as_ref().map(|(m, u)| format!("{m} {u}")),
        }
        .into_pyobject(py)?
        .into_any()
        .unbind(),
    })
}

/// A parsed document: parse once with [`loads`] / [`load`], query
/// many times. Query errors raise `ValueError`.
///
/// `unsendable`: the HTML adapter's DOM is not thread-safe, so a
/// Document is bound to the thread that created it.
#[pyclass(frozen, unsendable, module = "quarb")]
struct Document {
    doc: Doc,
    fmt: String,
}

impl Document {
    fn run_query(&self, query: &str) -> PyResult<QueryResult> {
        self.doc.execute(query).map_err(PyValueError::new_err)
    }
}

#[pymethods]
impl Document {
    /// The format this document was parsed as.
    #[getter]
    fn format(&self) -> &str {
        &self.fmt
    }

    /// Execute `query` and return its values, typed: ints, floats,
    /// strings, None, datetimes, timedeltas, Quantity, dicts for
    /// records, lists. A node result renders as pointer/locator
    /// strings.
    fn values(&self, py: Python<'_>, query: &str) -> PyResult<Vec<Py<PyAny>>> {
        match self.run_query(query)? {
            QueryResult::Values(vs) => vs.iter().map(|v| value_to_py(py, v)).collect(),
            QueryResult::Nodes(ns) => ns
                .into_iter()
                .map(|n| self.doc.render(n).into_py_any(py))
                .collect(),
        }
    }

    /// Execute `query` expecting at most one value: the typed
    /// value, or None when the result is empty. More than one
    /// value raises `ValueError` — use `values` for streams.
    fn value(&self, py: Python<'_>, query: &str) -> PyResult<Py<PyAny>> {
        let mut vs = self.values(py, query)?;
        match vs.len() {
            0 => Ok(py.None()),
            1 => Ok(vs.pop().unwrap()),
            n => Err(PyValueError::new_err(format!(
                "query returned {n} values; use values() for streams"
            ))),
        }
    }

    /// Execute `query` expecting record results: a list of dicts.
    /// A non-record value raises `TypeError`.
    fn records(&self, py: Python<'_>, query: &str) -> PyResult<Vec<Py<PyAny>>> {
        match self.run_query(query)? {
            QueryResult::Values(vs) => vs
                .iter()
                .map(|v| match v {
                    Value::Record(_) => value_to_py(py, v),
                    other => Err(PyTypeError::new_err(format!(
                        "records() expects record results (rec(...)); got {other}"
                    ))),
                })
                .collect(),
            QueryResult::Nodes(_) => Err(PyTypeError::new_err(
                "records() expects record results; the query returned nodes",
            )),
        }
    }

    /// Execute `query` expecting a node result: the matched nodes'
    /// pointers/locators. A value result raises `TypeError`.
    fn nodes(&self, query: &str) -> PyResult<Vec<String>> {
        match self.run_query(query)? {
            QueryResult::Nodes(ns) => Ok(ns.into_iter().map(|n| self.doc.render(n)).collect()),
            QueryResult::Values(_) => Err(PyTypeError::new_err(
                "nodes() expects a node result; the query projected values (use values())",
            )),
        }
    }

    fn __repr__(&self) -> String {
        format!("<quarb.Document format='{}'>", self.fmt)
    }
}

/// Parse `text` as `format` into a [`Document`]
/// (json | yaml | toml | csv | tsv | xml | html | markdown).
#[pyfunction]
fn loads(text: &str, format: &str) -> PyResult<Document> {
    Ok(Document {
        doc: Doc::parse(text, format).map_err(PyValueError::new_err)?,
        fmt: format.to_string(),
    })
}

/// Read and parse the file at `path` into a [`Document`]. The
/// format is inferred from the extension unless given explicitly.
#[pyfunction]
#[pyo3(signature = (path, format=None))]
fn load(path: PathBuf, format: Option<&str>) -> PyResult<Document> {
    let format = match format {
        Some(f) => f.to_string(),
        None => format_of(&path)
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "cannot infer format from extension: {}",
                    path.display()
                ))
            })?
            .to_string(),
    };
    let text = std::fs::read_to_string(&path)?;
    Ok(Document {
        doc: Doc::parse(&text, &format).map_err(PyValueError::new_err)?,
        fmt: format,
    })
}

/// Execute `query` against `input` parsed as `format` and return
/// the result lines as strings — the qua CLI's exact rendering.
/// The low-level layer; prefer [`loads`] + [`Document`] for typed
/// results.
#[pyfunction]
fn run(query: &str, input: &str, format: &str) -> PyResult<Vec<String>> {
    let doc = Doc::parse(input, format).map_err(PyValueError::new_err)?;
    render_lines(&doc, query)
}

/// Execute `query` against the file at `path`, inferring the
/// format from the extension; result lines as strings, as with
/// [`run`].
#[pyfunction]
fn run_file(query: &str, path: PathBuf) -> PyResult<Vec<String>> {
    let format = format_of(&path).ok_or_else(|| {
        PyValueError::new_err(format!(
            "cannot infer format from extension: {}",
            path.display()
        ))
    })?;
    let input = std::fs::read_to_string(&path)?;
    let doc = Doc::parse(&input, format).map_err(PyValueError::new_err)?;
    render_lines(&doc, query)
}

fn render_lines(doc: &Doc, query: &str) -> PyResult<Vec<String>> {
    match doc.execute(query).map_err(PyValueError::new_err)? {
        QueryResult::Nodes(ns) => Ok(ns.into_iter().map(|n| doc.render(n)).collect()),
        QueryResult::Values(vs) => Ok(vs.into_iter().map(|v| v.to_string()).collect()),
    }
}

#[pymodule]
fn _quarb(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Document>()?;
    m.add_class::<Quantity>()?;
    m.add_function(wrap_pyfunction!(loads, m)?)?;
    m.add_function(wrap_pyfunction!(load, m)?)?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    m.add_function(wrap_pyfunction!(run_file, m)?)?;
    Ok(())
}
