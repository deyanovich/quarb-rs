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
//! — no envelopes. The shell command stays gated: the run never
//! opens it, so `sh()` / backticks fail with the engine's normal
//! gate error. The native target has a clock, so the
//! invocation instant for `now()` is `SystemTime::now()` at call
//! time.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use quarb::AstAdapter;
use quarb::{NodeId, QueryResult, Value};
use quarb_pyvalue::{Quantity, value_to_py};

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
    Text(quarb_text::TextModel),
    Mount(quarb_mount::MountAdapter),
    Remote(Dyn),
}

/// A name-path locator built from the adapter trait alone
/// (`parent`/`name`) — used for the mount adapter, whose per-source
/// render functions the CLI threads through but Python doesn't keep.
fn generic_locator<A: AstAdapter>(a: &A, node: NodeId) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if let Some(nm) = a.name(n) {
            parts.push(nm);
        }
        cur = a.parent(n);
    }
    parts.reverse();
    format!("/{}", parts.join("/"))
}

impl Doc {
    fn parse(input: &str, format: &str) -> Result<Doc, String> {
        match format {
            "json" => quarb_json::JsonAdapter::parse(input)
                .map(Doc::Json)
                .map_err(|e| format!("parsing JSON: {e}")),
            // The property reading: containers are nodes, scalars
            // are properties.
            "json2" => quarb_json::JsonAdapter::parse_as(input, quarb_json::Reading::Property)
                .map(Doc::Json)
                .map_err(|e| format!("parsing JSON: {e}")),
            "jsonl" | "ndjson" => quarb_json::JsonAdapter::parse_lines(input)
                .map(Doc::Json)
                .map_err(|e| format!("parsing JSONL: {e}")),
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
            // The text level: the shared section/paragraph
            // vocabulary, produced per source format ("text" is
            // plain text — blank-line paragraphs).
            "text-html" => Ok(Doc::Text(quarb_text_html::parse(input))),
            "text-markdown" => Ok(Doc::Text(quarb_text_markdown::parse(input))),
            "text" => Ok(Doc::Text(quarb_text::TextModel::parse_plain(input))),
            // A treebank: as a document, and as a corpus with its
            // own tokens.
            "conllu" => quarb_text::TextModel::parse_conllu_text(input)
                .map(Doc::Text)
                .map_err(|e| format!("reading CoNLL-U: {e}")),
            "corpus-conllu" => quarb_text::TextModel::parse_conllu_corpus(input)
                .map(Doc::Text)
                .map_err(|e| format!("reading CoNLL-U: {e}")),
            // The corpus reading: the same three, tokenized.
            "corpus-html" | "corpus-markdown" | "corpus" => {
                let mut model = match format {
                    "corpus-html" => quarb_text_html::parse(input),
                    "corpus-markdown" => quarb_text_markdown::parse(input),
                    _ => quarb_text::TextModel::parse_plain(input),
                };
                model.tokenize();
                Ok(Doc::Text(model))
            }
            // The code level: identifiers as names; the format
            // name carries the language.
            "code-rust" => quarb_code::CodeModel::parse(input, "rs")
                .map(Doc::boxed_code)
                .map_err(|e| format!("parsing Rust at the code level: {e}")),
            "code-python" => quarb_code::CodeModel::parse(input, "py")
                .map(Doc::boxed_code)
                .map_err(|e| format!("parsing Python at the code level: {e}")),
            "code-javascript" => quarb_code::CodeModel::parse(input, "js")
                .map(Doc::boxed_code)
                .map_err(|e| format!("parsing JavaScript at the code level: {e}")),
            "code-c" => quarb_code::CodeModel::parse(input, "c")
                .map(Doc::boxed_code)
                .map_err(|e| format!("parsing C at the code level: {e}")),
            other => Err(format!("unknown format: {other}")),
        }
    }

    fn execute(&self, query: &str) -> Result<QueryResult, String> {
        let (secs, nanos) = now_parts();
        let run = quarb::Run {
            now: Some((secs, nanos)),
            ..Default::default()
        };
        macro_rules! go {
            ($a:expr) => {{
                quarb::Query::parse_with(query, &quarb::Defs::default(), $a, &run)
                    .and_then(|q| q.run($a, &run))
                    .map(|o| o.result)
                    .map_err(|e| e.to_string())
            }};
        }
        match self {
            Doc::Json(a) => go!(a),
            Doc::Csv(a) => go!(a),
            Doc::Xml(a) => go!(a),
            Doc::Html(a) => go!(a),
            Doc::Text(a) => go!(a),
            Doc::Mount(a) => go!(a),
            Doc::Remote(a) => go!(a),
        }
    }

    /// Run `query` with its closing `@| table` taken off: the rows
    /// (a koine `table` node as its rows' records), the text table,
    /// and the HTML grid.
    fn table(&self, query: &str) -> Result<(Vec<Value>, String, String), String> {
        let (secs, nanos) = now_parts();
        let run = quarb::Run {
            now: Some((secs, nanos)),
            ..Default::default()
        };
        macro_rules! go {
            ($a:expr) => {{
                let out = quarb::Query::parse_with(query, &quarb::Defs::default(), $a, &run)
                    .and_then(|q| q.without_closing().run($a, &run))
                    .map_err(|e| e.to_string())?;
                let closing = out
                    .closing
                    .filter(|c| !c.csv)
                    .ok_or_else(|| "the query does not close in @| table".to_string())?;
                let rows: Vec<Value> =
                    quarb::table_rows_of($a, out.result, &closing.columns, &|n| self.render(n));
                let columns = (!closing.columns.is_empty()).then_some(closing.columns.as_slice());
                let text = quarb::tabulate_with(&rows, columns, false, b',');
                let html = quarb::table_html_plain(&rows, columns);
                Ok((rows, text, html))
            }};
        }
        match self {
            Doc::Json(a) => go!(a),
            Doc::Csv(a) => go!(a),
            Doc::Xml(a) => go!(a),
            Doc::Html(a) => go!(a),
            Doc::Text(a) => go!(a),
            Doc::Mount(a) => go!(a),
            Doc::Remote(a) => go!(a),
        }
    }

    /// The static plan of `query`: every step priced by the
    /// adapter's own estimate, under the budget's cap when given.
    fn plan(
        &self,
        query: &str,
        budget: Option<&quarb::plan::Budget>,
    ) -> Result<quarb::plan::Plan, String> {
        let (secs, nanos) = now_parts();
        let run = quarb::Run {
            now: Some((secs, nanos)),
            ..Default::default()
        };
        macro_rules! go {
            ($a:expr) => {{
                quarb::Query::parse_with(query, &quarb::Defs::default(), $a, &run)
                    .map(|q| match budget {
                        Some(b) => q.plan_within($a, b),
                        None => q.plan($a),
                    })
                    .map_err(|e| e.to_string())
            }};
        }
        match self {
            Doc::Json(a) => go!(a),
            Doc::Csv(a) => go!(a),
            Doc::Xml(a) => go!(a),
            Doc::Html(a) => go!(a),
            Doc::Text(a) => go!(a),
            Doc::Mount(a) => go!(a),
            Doc::Remote(a) => go!(a),
        }
    }

    fn render(&self, node: NodeId) -> String {
        match self {
            Doc::Json(a) => a.pointer(node),
            Doc::Csv(a) => a.locator(node),
            Doc::Xml(a) => a.locator(node),
            Doc::Html(a) => a.locator(node),
            Doc::Text(a) => a.locator(node),
            Doc::Mount(a) => generic_locator(a, node),
            Doc::Remote(a) => generic_locator(a, node),
        }
    }
}

/// A boxed adapter behind the one trait — the remote targets'
/// uniform carrier, a local type so `Doc` can name it; every
/// method forwards.
struct Dyn(Box<dyn AstAdapter>);

impl Doc {
    /// The code level rides the boxed door (no typed variant).
    fn boxed_code(m: quarb_code::CodeModel) -> Doc {
        Doc::Remote(Dyn(Box::new(quarb_mount::Shared(std::rc::Rc::new(m)))))
    }
}

impl AstAdapter for Dyn {
    quarb::forward_ast_adapter!(0);
}

/// Open one source as a boxed adapter — the building block for a
/// multi-source [`mount`], through quarb-open like [`open`].
fn open_boxed(path: &str, graft: bool, no_graft: bool) -> Result<Box<dyn AstAdapter>, String> {
    let opts = quarb_open::OpenOpts {
        graft,
        no_graft,
        ..Default::default()
    };
    quarb_open::open_target(path, &opts)
        .map(|m| m.adapter)
        .map_err(|e| format!("{e:#}"))
}

/// Mount several sources under one root — the multi-source
/// document, so a single query can correlate across them (a YAML
/// fleet `<=>` a SQLite CMDB). Each source is addressed by its file
/// stem: `mount(["fleet.yaml", "cmdb.db"])` answers `/fleet/...`
/// and `/cmdb/...`. A single path just [`open`]s (keeping its typed
/// rendering). `graft` grafts parseable leaves under directory
/// mounts; `no_graft` holds every boundary opaque. (`descend` is
/// the pre-0.24 spelling of `graft`, kept as an alias.)
#[pyfunction]
#[pyo3(signature = (paths, graft=false, no_graft=false, descend=false))]
fn mount(paths: Vec<String>, graft: bool, no_graft: bool, descend: bool) -> PyResult<Document> {
    let graft = graft || descend;
    if paths.is_empty() {
        return Err(PyValueError::new_err("mount needs at least one path"));
    }
    if paths.len() == 1 {
        return open(&paths[0], graft, no_graft, false);
    }
    let mut mounts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in &paths {
        let stem = Path::new(path.strip_prefix("git:").unwrap_or(path))
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string();
        if !seen.insert(stem.clone()) {
            return Err(PyValueError::new_err(format!(
                "input {path:?} mounts as {stem:?}, colliding with an earlier \
                 input of the same stem — give each a distinct basename"
            )));
        }
        let adapter = open_boxed(path, graft, no_graft).map_err(PyValueError::new_err)?;
        mounts.push(quarb_mount::Mount {
            name: stem,
            target: Some(path.clone()),
            adapter,
        });
    }
    Ok(Document {
        doc: Doc::Mount(
            quarb_mount::MountAdapter::try_new(mounts)
                .map_err(|e| PyValueError::new_err(format!("mount: {e}")))?,
        ),
        fmt: "mount".into(),
    })
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
        Some("jsonl" | "ndjson") => Some("jsonl"),
        Some("yaml" | "yml") => Some("yaml"),
        Some("toml") => Some("toml"),
        Some("csv") => Some("csv"),
        Some("tsv") => Some("tsv"),
        Some("xml") => Some("xml"),
        Some("html" | "htm") => Some("html"),
        Some("md" | "markdown") => Some("markdown"),
        Some("txt") => Some("text"),
        _ => None,
    }
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
    fn run_query(&self, query: &str, budget: Option<&str>) -> PyResult<QueryResult> {
        if let Some(spec) = budget {
            let b = quarb::plan::Budget::parse(spec).map_err(PyValueError::new_err)?;
            let plan = self
                .doc
                .plan(query, Some(&b))
                .map_err(PyValueError::new_err)?;
            if let Some(why) = b.exceeded_by(&plan) {
                return Err(PyValueError::new_err(
                    quarb::QuarbError::refused(why).to_string(),
                ));
            }
        }
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
    /// strings. `budget` ("nodes=1M,bytes=2GiB,strict") prices the
    /// query first and raises `ValueError` when its plan passes the
    /// bound, before anything is read.
    #[pyo3(signature = (query, budget=None))]
    fn values(
        &self,
        py: Python<'_>,
        query: &str,
        budget: Option<&str>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        match self.run_query(query, budget)? {
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
    #[pyo3(signature = (query, budget=None))]
    fn value(&self, py: Python<'_>, query: &str, budget: Option<&str>) -> PyResult<Py<PyAny>> {
        let mut vs = self.values(py, query, budget)?;
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
    #[pyo3(signature = (query, budget=None))]
    fn records(
        &self,
        py: Python<'_>,
        query: &str,
        budget: Option<&str>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        match self.run_query(query, budget)? {
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
    #[pyo3(signature = (query, budget=None))]
    fn nodes(&self, query: &str, budget: Option<&str>) -> PyResult<Vec<String>> {
        match self.run_query(query, budget)? {
            QueryResult::Nodes(ns) => Ok(ns.into_iter().map(|n| self.doc.render(n)).collect()),
            QueryResult::Values(_) => Err(PyTypeError::new_err(
                "nodes() expects a node result; the query projected values (use values())",
            )),
        }
    }

    /// Run a query whose closing command is `@| table` and return
    /// `(rows, text, html)`: the rows as typed values (records as
    /// dicts; a document's own table as its rows' records), the
    /// text table as `qua` prints it, and the HTML grid every
    /// browser host renders (dress it with `quarb.TABLE_CSS`).
    /// Raises `ValueError` when the query does not close in
    /// `@| table`, or when `budget` refuses it.
    #[pyo3(signature = (query, budget=None))]
    fn table(
        &self,
        py: Python<'_>,
        query: &str,
        budget: Option<&str>,
    ) -> PyResult<(Vec<Py<PyAny>>, String, String)> {
        if let Some(spec) = budget {
            let b = quarb::plan::Budget::parse(spec).map_err(PyValueError::new_err)?;
            let plan = self
                .doc
                .plan(query, Some(&b))
                .map_err(PyValueError::new_err)?;
            if let Some(why) = b.exceeded_by(&plan) {
                return Err(PyValueError::new_err(
                    quarb::QuarbError::refused(why).to_string(),
                ));
            }
        }
        let (rows, text, html) = self.doc.table(query).map_err(PyValueError::new_err)?;
        let rows = rows
            .iter()
            .map(|v| value_to_py(py, v))
            .collect::<PyResult<Vec<_>>>()?;
        Ok((rows, text, html))
    }

    /// Price `query` before it runs: its static plan as the
    /// `quarb/plan` kaiv payload — per step the nodes and bytes it
    /// may reach as ranges (`nodes-min`, `nodes-max`, …), from the
    /// document's own estimate. With `budget`, the estimators stop
    /// counting at the budget's cap, as `values(..., budget=)` does
    /// before refusing.
    #[pyo3(signature = (query, budget=None))]
    fn plan(&self, query: &str, budget: Option<&str>) -> PyResult<String> {
        let b = budget
            .map(quarb::plan::Budget::parse)
            .transpose()
            .map_err(PyValueError::new_err)?;
        let plan = self
            .doc
            .plan(query, b.as_ref())
            .map_err(PyValueError::new_err)?;
        Ok(plan.to_kaiv(query))
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

/// Open `path` with the adapter its kind calls for — the full
/// local fleet, mirroring the qua CLI's dispatch:
///
/// - a directory mounts as a filesystem tree (`graft=True`
///   grafts parseable leaves — JSON, CSV, code, … — as subtrees;
///   `descend=True` is the pre-0.24 spelling, kept as an alias);
/// - `no_graft=True` disables grafting entirely: archive members
///   stay opaque entries (the tar -t view);
/// - `.db` / `.sqlite` / `.sqlite3` opens SQLite;
/// - `.kaiv` / `.daiv` / `.raiv` mounts typed kaiv (units, instants,
///   durations — sibling `.faiv` unit libraries resolve relative to
///   the document);
/// - `.zip` / `.tar` / `.tgz` / `.tar.gz` mounts the archive, its
///   parseable entries grafted;
/// - `.xlsx` opens the workbook; source-code extensions (`.rs`,
///   `.py`, `.js`, `.c`, …) parse to a syntax arbor;
/// - a repository path given as `git:PATH` (or a `.git` directory)
///   mounts the commit graph;
/// - anything else falls back to text parsing by extension, as
///   [`load`] does.
#[pyfunction]
#[pyo3(signature = (path, graft=false, no_graft=false, descend=false))]
fn open(path: &str, graft: bool, no_graft: bool, descend: bool) -> PyResult<Document> {
    let graft = graft || descend;
    let opts = quarb_open::OpenOpts {
        graft,
        no_graft,
        ..Default::default()
    };
    let m = quarb_open::open_target(path, &opts)
        .map_err(|e| PyValueError::new_err(format!("{e:#}")))?;
    Ok(Document {
        doc: Doc::Remote(Dyn(m.adapter)),
        fmt: m.family.to_string(),
    })
}

/// Colorize a Quarb query with ANSI escapes — the terminal
/// syntax highlighter, the same token model as the JupyterLab
/// extension. Honors nothing about the terminal itself (no TTY or
/// NO_COLOR checks); the caller decides whether to emit it.
#[pyfunction]
fn highlight(query: &str) -> String {
    quarb::highlight::highlight_ansi(query)
}

/// Translate a jq filter, an XPath 1.0 expression, or a SQL SELECT
/// into Quarb query text (`lang` = "jq" | "xpath" | "sql") — the
/// same bridges as the CLI's --jq/--xpath/--sql.
#[pyfunction]
fn translate(source: &str, lang: &str) -> PyResult<String> {
    let q = match lang {
        "jq" => quarb_jq::translate(source)
            .map(|t| t.query)
            .map_err(|e| e.to_string()),
        "xpath" => quarb_xpath::translate(source)
            .map(|t| t.query)
            .map_err(|e| e.to_string()),
        "sql" => quarb_sql::translate(source)
            .map(|t| t.query)
            .map_err(|e| e.to_string()),
        other => Err(format!(
            "unknown source language: {other} (jq | xpath | sql)"
        )),
    }
    .map_err(PyValueError::new_err)?;
    Ok(q)
}

/// Validate `text` as a definitions file — `def`/`macro` statements
/// only, `#` line comments allowed — raising ValueError on the first
/// malformed statement. The session layer uses this to accept a
/// defs-only cell into its cross-cell fragment table.
#[pyfunction]
fn parse_defs(text: &str) -> PyResult<()> {
    quarb::parse_defs(text)
        .map(|_| ())
        .map_err(|e| PyValueError::new_err(e.to_string()))
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
        QueryResult::Values(vs) => Ok(vs.into_iter().map(|v| v.display_form()).collect()),
    }
}

/// Whether `query` closes in `@| table` — the rule under which a
/// result renders as the HTML grid in every browser host, and the
/// notebook's rich display switches on the same condition.
#[pyfunction]
fn closes_with_table(query: &str) -> bool {
    quarb::closes_with_table(query)
}

#[pymodule]
fn _quarb(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("TABLE_CSS", quarb::TABLE_CSS)?;
    m.add_function(wrap_pyfunction!(closes_with_table, m)?)?;
    m.add_class::<Document>()?;
    m.add_class::<Quantity>()?;
    m.add_function(wrap_pyfunction!(loads, m)?)?;
    m.add_function(wrap_pyfunction!(load, m)?)?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add_function(wrap_pyfunction!(mount, m)?)?;
    m.add_function(wrap_pyfunction!(translate, m)?)?;
    m.add_function(wrap_pyfunction!(parse_defs, m)?)?;
    m.add_function(wrap_pyfunction!(highlight, m)?)?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    m.add_function(wrap_pyfunction!(run_file, m)?)?;
    Ok(())
}
