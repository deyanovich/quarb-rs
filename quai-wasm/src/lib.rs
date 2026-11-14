//! Browser build of interactive Quarb — the `demo.quarb.org/quai`
//! session playground.
//!
//! One [`QuaiSession`] object wraps a [`quarb_session::Session`] over a
//! single pasted text document (the text-format adapter subset). It
//! carries the full `&N` / `&N!` / `&N#` macro history, exactly as the
//! native `quai` REPL, and hands each line's result back to JS as
//! JSON. Persistence is the JS layer's job (an optional localStorage
//! checkbox): [`QuaiSession::state`] serializes the history and
//! [`QuaiSession::restore`] reloads it.

use quarb_session::{Doc, LocalExecutor, MemStore, Session, Source};
use serde_json::json;
use wasm_bindgen::prelude::*;

/// Highlight a query as HTML `<span class="qh-…">` markup — the same
/// token model as the terminal (`highlight_ansi`) and the JupyterLab
/// CodeMirror extension. The playground uses it to color the input
/// line and the transcript echo.
#[wasm_bindgen]
pub fn highlight(query: &str) -> String {
    quarb::highlight::highlight_html(query)
}

/// The engine version string, for the playground footer.
/// The engine's table stylesheet, scoped under `.quarb-table`: a
/// host includes it once and every grid it renders is dressed.
#[wasm_bindgen]
pub fn table_css() -> String {
    quarb::TABLE_CSS.to_string()
}

/// The stylesheet of every rich form (the grid, the drawings, the
/// diagnostics block): what [`table_css`] names, under the rendering
/// rule's name (`quarb::rich::css`).
#[wasm_bindgen(js_name = richCss)]
pub fn rich_css() -> String {
    quarb::rich::css().to_string()
}

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Convert a JS `Date.now()` millisecond value to the engine's
/// `(seconds, nanos)` invocation instant.
fn now_parts(now_millis: f64) -> (i64, u32) {
    let secs = (now_millis / 1000.0).floor();
    let nanos = ((now_millis - secs * 1000.0) * 1_000_000.0).round() as u32;
    (secs as i64, nanos.min(999_999_999))
}

/// Standard base64 (with or without padding) — enough to carry an
/// uploaded database across the JS boundary without a dependency.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let chars: Vec<u8> = s
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(chars.len() * 3 / 4);
    for chunk in chars.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * i);
        }
        let n = chunk.len();
        if n < 2 {
            return None;
        }
        out.push((acc >> 16) as u8);
        if n > 2 {
            out.push((acc >> 8) as u8);
        }
        if n > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

/// A source as the page handed it over, kept by the session: the
/// page fetched the bytes and the engine cannot, so a
/// `mount NAME: ?options` line re-reads from here.
#[derive(Clone)]
struct Kept {
    name: String,
    format: String,
    text: Option<String>,
    bytes: Option<Vec<u8>>,
    url: Option<String>,
    /// The mount options as a target writes them after `?`.
    options: String,
    /// A model cell's mount always names its root, one mount as
    /// several (spec: .quai).
    named: bool,
    /// The files the options name (`conllu=`, `cast=`, …), each by
    /// the option's value as written and its text: the page fetched
    /// them, the session holds them for a later re-read.
    files: Vec<(String, String)>,
}

/// The sources of a `sources_json` array: `{"name", "format",
/// "text"}` objects, or `"bytes_b64"` for a binary source (a
/// database, a site's tarball), each with optional `"url"`,
/// `"options"` (the mount options, `null=NA&decimal=comma`) and
/// `"named"`.
fn parse_sources(sources_json: &str) -> Result<Vec<Kept>, String> {
    let sources: serde_json::Value =
        serde_json::from_str(sources_json).map_err(|e| format!("bad sources array: {e}"))?;
    let mut kept = Vec::new();
    for s in sources.as_array().into_iter().flatten() {
        let field = |k: &str| s.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        let (Some(name), Some(format)) = (field("name"), field("format")) else {
            return Err("each source needs name and format".into());
        };
        let bytes = match field("bytes_b64") {
            Some(b64) => Some(b64_decode(&b64).ok_or_else(|| format!("'{name}': bad base64"))?),
            None => None,
        };
        let text = field("text");
        if bytes.is_none() && text.is_none() {
            return Err(format!("'{name}': a source needs text or bytes_b64"));
        }
        kept.push(Kept {
            name,
            format,
            text,
            bytes,
            url: field("url"),
            options: field("options")
                .map(|o| o.trim_start_matches('?').to_string())
                .unwrap_or_default(),
            named: s.get("named").and_then(|v| v.as_bool()).unwrap_or(false),
            files: s
                .get("files")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|f| {
                    Some((
                        f.get("name")?.as_str()?.to_owned(),
                        f.get("text")?.as_str()?.to_owned(),
                    ))
                })
                .collect(),
        });
    }
    if kept.is_empty() {
        return Err("no sources to mount".into());
    }
    Ok(kept)
}

/// The mounted [`Doc`] of the kept sources, each read as the native
/// opener reads a target ([`Doc::read`]: its format word, its mount
/// options, the composing layer). A single source mounts path-bare
/// at the root, as one source on the command line does; several
/// mount as named children — and so does a single source carrying
/// `named`, the way a model cell's `mount NAME: TARGET;` always
/// names its root (spec: .quai).
fn build(
    sources: &[Kept],
) -> Result<(Doc, std::collections::HashMap<String, quarb::NodeId>), String> {
    let named = sources.iter().any(|s| s.named);
    let mut parts: Vec<(String, Doc)> = Vec::new();
    let mut urls: Vec<(String, String)> = Vec::new();
    for s in sources {
        let doc = if s.format == "site" {
            // A website as one source: `text` is a JSON object
            // `{"base": "https://example.org/", "pages": {path: html}}`
            // — the web level over the memory store, every page
            // grafted at the text level on entry.
            let text = s
                .text
                .as_deref()
                .ok_or_else(|| format!("'{}': a site source needs text", s.name))?;
            site_doc(text).map_err(|e| format!("'{}': {e}", s.name))?
        } else {
            let files: Vec<(&str, &str)> = s
                .files
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            Doc::read(&Source {
                format: &s.format,
                text: s.text.as_deref(),
                bytes: s.bytes.as_deref(),
                options: &s.options,
                url: s.url.as_deref(),
                files: &files,
            })
            .map_err(|e| format!("'{}': {e:#}", s.name))?
        };
        // A source that knows its URL joins the acquisition story:
        // relative hrefs resolve against it, and the arrow can land
        // on it from a sibling source.
        if let Some(url) = &s.url
            && s.bytes.is_none()
        {
            urls.push((s.name.clone(), url.clone()));
        }
        parts.push((s.name.clone(), doc));
    }
    let doc = match parts.len() {
        // A single source stays path-bare at the root — unless it
        // asked for its name.
        1 if !named => parts.remove(0).1,
        _ => Doc::mount_docs(parts).map_err(|e| format!("{e:#}"))?,
    };
    let roots = doc.url_roots(&urls);
    Ok((doc, roots))
}

/// The executor over the kept sources, under the model's
/// declarations when there is one.
fn executor_of(
    sources: &[Kept],
    model: Option<&str>,
    now_millis: f64,
) -> Result<LocalExecutor, String> {
    let (doc, roots) = build(sources)?;
    let model = match model {
        Some(text) => {
            Some(quarb_model::parse_model(text).map_err(|e| format!("parsing model: {e}"))?)
        }
        None => None,
    };
    Ok(LocalExecutor::new(doc, now_parts(now_millis), false)
        .with_model(model)
        .with_url_roots(roots))
}

/// The statements of a line or a cell that re-read mounted sources
/// under options: `mount NAME: ?options` (the name left out where
/// one source is mounted), one or several, a `;` after each or
/// none. On a line every statement must be one: `None` where the
/// text is anything else. In a notebook's model cell (`in_file`)
/// the other statements were applied when the session opened and
/// are passed over; `None` where the cell re-reads nothing.
fn remount_statements(text: &str, in_file: bool) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    for stmt in text.split(';') {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        let re_read = stmt
            .strip_prefix("mount")
            .filter(|rest| rest.starts_with(|c: char| c.is_whitespace() || c == ':'))
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(name, target)| {
                let options = target.trim().strip_prefix('?')?;
                Some((name.trim().to_string(), options.to_string()))
            });
        match re_read {
            Some(pair) => out.push(pair),
            None if in_file => continue,
            None => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The web level over pages in memory, riding the session's Boxed
/// door the way kaiv does: the adapter is Rc-shared between the
/// Doc and its locator renderer.
fn site_doc(text: &str) -> Result<Doc, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("reading the site: {e}"))?;
    let base = v
        .get("base")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let pages = v
        .get("pages")
        .and_then(|p| p.as_object())
        .ok_or_else(|| "a site needs a \"pages\" object of path to html".to_string())?;
    let files = pages
        .iter()
        .filter_map(|(path, html)| {
            html.as_str().map(|h| quarb_web::PageFile {
                path: path.clone(),
                html: h.to_string(),
            })
        })
        .collect();
    let store = quarb_web::MemoryStore::build(
        quarb_web::SiteInput {
            base_url: base,
            snapshot: None,
        },
        files,
    );
    let a = std::rc::Rc::new(quarb_web::WebAdapter::new(store));
    let r = a.clone();
    Ok(Doc::Boxed(
        quarb_session::doc::Dyn(Box::new(quarb_mount::Shared(a))),
        Box::new(move |n| r.locator(n)),
        "web",
    ))
}

/// Match a bare capture ref `&<digits><suffix>` (the whole line).
fn numeric_ref_with(line: &str, suffix: char) -> Option<usize> {
    line.strip_suffix(suffix)?
        .strip_prefix('&')?
        .parse::<usize>()
        .ok()
}

/// Whether a frozen reference `&N#` stands inside a longer line —
/// `&3# | count`, `(&3#)` — where it has no meaning: the frozen
/// footprint replays only as the whole line. Every other `#` is the
/// query's (`| #`, `#(--null=…)`, a `#` comment).
fn embedded_frozen_ref(line: &str) -> bool {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'&' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < b.len() && b[j] == b'#' {
                let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
                let after_ok = j + 1 >= b.len() || !b[j + 1].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// One diagnostic as the page reads it: `{severity, code, site,
/// message}`, the severity word in lower case.
fn diag_json(d: &quarb::Diagnostic) -> serde_json::Value {
    json!({
        "severity": d.severity.word(),
        "code": d.code,
        "site": d.site,
        "message": d.message,
    })
}

/// The envelope of a withheld answer: `error` the displayed message
/// (as before), and the same error as the diagnostics array's one
/// item with its code and site (a Quarb error's own; `ERROR` at
/// `query` for a host failure).
fn error_envelope(label: &str, e: &anyhow::Error) -> String {
    let message = format!("{e:#}");
    let d = match e.downcast_ref::<quarb::QuarbError>() {
        Some(q) => q.diagnostic(),
        None => quarb::Diagnostic::error("ERROR", "query", message.clone()),
    };
    envelope_full(
        label,
        vec![],
        None,
        Some(message),
        Vec::new(),
        None,
        vec![d],
    )
}

/// A result envelope for JS: the `&N` label, the output lines, and an
/// optional note or error (exactly one of note/error, or neither).
fn envelope(
    label: &str,
    lines: Vec<String>,
    note: Option<String>,
    error: Option<String>,
) -> String {
    envelope_refs(label, lines, note, error, Vec::new())
}

/// The envelope of a line that asks for the manual
/// ([`quarb::man::line`]): the page's lines under no label, or the
/// door's message as the error; `None` for any other line.
fn manual(line: &str) -> Option<String> {
    Some(match quarb::man::line(line)? {
        Ok(text) => envelope("", text.lines().map(String::from).collect(), None, None),
        Err(e) => envelope("", vec![], None, Some(e)),
    })
}

/// [`envelope`], carrying the run's unresolved external
/// references — the identifiers (URLs) the query referenced
/// (`::href-->`) but could not reach. The host may acquire them,
/// remount, and re-run.
fn envelope_refs(
    label: &str,
    lines: Vec<String>,
    note: Option<String>,
    error: Option<String>,
    refs: Vec<String>,
) -> String {
    envelope_html(label, lines, note, error, refs, None)
}

/// [`envelope_refs`], carrying the HTML grid of a result whose
/// closing command is `@| pretty` (`html`, null otherwise): the
/// host draws it in place of the lines, dressed by
/// [`table_css`] once.
fn envelope_html(
    label: &str,
    lines: Vec<String>,
    note: Option<String>,
    error: Option<String>,
    refs: Vec<String>,
    html: Option<String>,
) -> String {
    envelope_full(label, lines, note, error, refs, html, Vec::new())
}

/// [`envelope_html`] with the run's diagnostics (spec: Diagnostics):
/// the warnings and infos beside an answer, in raising order; for a
/// withheld answer the error itself as the first and only item, so
/// a host reads one array whatever the outcome.
fn envelope_full(
    label: &str,
    lines: Vec<String>,
    note: Option<String>,
    error: Option<String>,
    refs: Vec<String>,
    html: Option<String>,
    diagnostics: Vec<quarb::Diagnostic>,
) -> String {
    json!({
        "label": label,
        "lines": lines,
        "html": html,
        "note": note,
        "diagnostics": diagnostics.iter().map(diag_json).collect::<Vec<_>>(),
        "error": error,
        "refs": refs,
    })
    .to_string()
}

#[wasm_bindgen]
pub struct QuaiSession {
    session: Session,
    /// What the session is mounted from, for a re-read under
    /// options (`mount NAME: ?options`).
    sources: Vec<Kept>,
    /// The model the page handed over with the sources.
    model: Option<String>,
    now_millis: f64,
}

impl QuaiSession {
    /// A session over the kept sources.
    fn open(
        sources: Vec<Kept>,
        model: Option<String>,
        now_millis: f64,
    ) -> Result<QuaiSession, String> {
        let executor = Box::new(executor_of(&sources, model.as_deref(), now_millis)?);
        Ok(QuaiSession {
            session: Session::new(executor, Box::new(MemStore)),
            sources,
            model,
            now_millis,
        })
    }

    /// `mount NAME: ?options` (spec: .quai, the mount line; ruling
    /// 2026-11-12): re-read the named source under the options, the
    /// history and the labels kept. In a session the page mounted
    /// the sources, so a mount line can only re-read one; a line
    /// that names a target is told so. In a notebook's replay
    /// (`in_file`) a model cell that names targets was applied when
    /// the session opened and is left to that. `None` for any other
    /// text.
    fn remount(&mut self, text: &str, in_file: bool) -> Option<String> {
        let first = text.split_whitespace().next().unwrap_or_default();
        let model_cell = in_file
            && quarb_model::STATEMENTS.contains(&first)
            && !matches!(first, "def" | "macro");
        if first != "mount" && !first.starts_with("mount:") && !model_cell {
            return None;
        }
        let Some(statements) = remount_statements(text, in_file) else {
            if in_file {
                return None;
            }
            return Some(envelope(
                "",
                vec![],
                None,
                Some(
                    "a mount line re-reads a mounted source under options, \
                     'mount NAME: ?null=NA&decimal=comma'; a new source is the page's to add"
                        .into(),
                ),
            ));
        };
        let names = || {
            self.sources
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        // every statement resolved before anything is re-read
        let mut changes: Vec<(usize, String)> = Vec::new();
        for (name, options) in statements {
            let at = if name.is_empty() {
                if self.sources.len() != 1 {
                    return Some(envelope(
                        "",
                        vec![],
                        None,
                        Some(format!(
                            "name the source to re-read, 'mount NAME: ?{options}'; the \
                             sources: {}",
                            names()
                        )),
                    ));
                }
                0
            } else {
                match self.sources.iter().position(|s| s.name == name) {
                    Some(at) => at,
                    None => {
                        return Some(envelope(
                            "",
                            vec![],
                            None,
                            Some(format!(
                                "no source is mounted as '{name}'; the sources: {}",
                                names()
                            )),
                        ));
                    }
                }
            };
            changes.push((at, options));
        }
        let before: Vec<(usize, String)> = changes
            .iter()
            .map(|(at, _)| (*at, self.sources[*at].options.clone()))
            .collect();
        for (at, options) in &changes {
            self.sources[*at].options = options.clone();
        }
        let executor = match executor_of(&self.sources, self.model.as_deref(), self.now_millis) {
            Ok(executor) => executor,
            Err(e) => {
                for (at, options) in before {
                    self.sources[at].options = options;
                }
                return Some(envelope("", vec![], None, Some(e)));
            }
        };
        // the session's settings and what it filed stay with it
        let executor = match self.session.session_memory() {
            Some(memory) => executor.with_session(memory),
            None => executor,
        };
        self.session.set_executor(Box::new(executor));
        let label = format!("&{}", self.session.line_no());
        self.session.commit(text, Vec::new());
        let note = changes
            .iter()
            .map(|(at, options)| {
                let name = &self.sources[*at].name;
                if options.is_empty() {
                    format!("{name} re-read with no options")
                } else {
                    format!("{name} re-read under {options}")
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        let mounts: Vec<serde_json::Value> = changes
            .iter()
            .map(|(at, options)| json!({"name": self.sources[*at].name, "options": options}))
            .collect();
        Some(
            json!({
                "label": label,
                "lines": Vec::<String>::new(),
                "html": null,
                "note": note,
                "diagnostics": Vec::<serde_json::Value>::new(),
                "error": null,
                "refs": Vec::<String>::new(),
                "mount": mounts,
            })
            .to_string(),
        )
    }
}

#[wasm_bindgen]
impl QuaiSession {
    /// Open a session over `input` parsed as `format` (json, yaml,
    /// toml, csv, tsv, xml, html, markdown). `now_millis` pins the
    /// session's `now()` (pass `Date.now()`).
    #[wasm_bindgen(constructor)]
    pub fn new(format: &str, input: &str, now_millis: f64) -> Result<QuaiSession, JsError> {
        let one = Kept {
            name: format.to_string(),
            format: format.to_string(),
            text: Some(input.to_string()),
            bytes: None,
            url: None,
            options: String::new(),
            named: false,
            files: Vec::new(),
        };
        // the one document's error as it was: the parser's own
        QuaiSession::open(vec![one], None, now_millis).map_err(|e| {
            let own = e.strip_prefix(&format!("'{format}': ")).unwrap_or(&e);
            JsError::new(own)
        })
    }

    /// Open a session over *several* documents mounted as named
    /// children of one root, so a single query — including a `<=>`
    /// join — spans them all (`/name/...` paths). `sources_json` is a
    /// JSON array of `{"name", "format", "text"}` objects — or, for
    /// binary sources (an uploaded `.db`), `{"name", "format":
    /// "sqlite", "bytes_b64"}`. Names must be distinct. A single
    /// source mounts at the root, path-bare, exactly as the
    /// one-document constructor does.
    pub fn mount(sources_json: &str, now_millis: f64) -> Result<QuaiSession, JsError> {
        parse_sources(sources_json)
            .and_then(|sources| QuaiSession::open(sources, None, now_millis))
            .map_err(|e| JsError::new(&e))
    }

    /// [`mount`], enriched by a `--model` file (`model_text`): the
    /// session runs every line against the derived structure the
    /// model declares over the mounted sources — value containers,
    /// references, and pair edges. The model's own `mount` statements
    /// are ignored here (the sources come from the caller, not from
    /// disk the wasm cannot read); its `node`/`ref`/`edge`/`def`
    /// statements apply.
    #[wasm_bindgen(js_name = mountWithModel)]
    pub fn mount_with_model(
        sources_json: &str,
        model_text: &str,
        now_millis: f64,
    ) -> Result<QuaiSession, JsError> {
        parse_sources(sources_json)
            .and_then(|sources| {
                QuaiSession::open(sources, Some(model_text.to_string()), now_millis)
            })
            .map_err(|e| JsError::new(&e))
    }

    /// Run a line and render its results as exportable markup:
    /// `kind` is `md`, `html`, or `txt`. Text-level node results
    /// render structurally (sections back to headings, lists to
    /// lists); anything else degrades to prose lines. History refs
    /// (`&N`) resolve as in a normal line.
    pub fn render(&self, line: &str, kind: &str) -> Result<String, JsError> {
        // the HTML export is the rich form (the grid, a drawing) with
        // the stylesheet and the diagnostics, as the panel shows it;
        // the text lines in a `<pre>` where there is none
        if kind == "html" {
            return self
                .session
                .export_rich(line)
                .map_err(|e| JsError::new(&format!("{e:#}")));
        }
        self.session
            .export(line, kind)
            .map_err(|e| JsError::new(&format!("{e:#}")))
    }

    /// The session's `--pretty` setting from the host: `off`,
    /// `structures` or `always` — what `session(--pretty=…)` sets from
    /// a line. The extension opens at `structures`, the playground
    /// at `off` (its button runs the command).
    #[wasm_bindgen(js_name = setPretty)]
    pub fn set_pretty(&self, word: &str) -> Result<(), JsError> {
        let mode = quarb::PrettyMode::parse(word).ok_or_else(|| {
            JsError::new(&format!(
                "setPretty({word:?}): the setting is off, structures or always"
            ))
        })?;
        self.session.set_pretty(mode);
        Ok(())
    }

    /// The session's `--pretty` setting as its word.
    #[wasm_bindgen(getter)]
    pub fn pretty(&self) -> String {
        self.session.pretty_mode().word().to_string()
    }

    /// The `&N` a fresh line will claim.
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> usize {
        self.session.line_no()
    }

    /// The macro table (`def &1: …;` lines), for a history panel.
    pub fn history(&self) -> String {
        self.session.history().to_string()
    }

    /// Clear the history and restart numbering.
    pub fn reset(&mut self) {
        self.session.reset();
    }

    /// Serialize the durable state for localStorage.
    pub fn state(&self) -> String {
        json!({
            "line_no": self.session.line_no(),
            "defs_text": self.session.history(),
        })
        .to_string()
    }

    /// Restore a persisted state (from localStorage).
    pub fn restore(&mut self, state_json: &str) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(state_json) {
            let defs = v
                .get("defs_text")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let line_no = v.get("line_no").and_then(|n| n.as_u64()).unwrap_or(1) as usize;
            self.session.restore(defs, line_no);
        }
    }

    /// Run one notebook-shaped cell, mirroring the Quarb kernel: a
    /// defs-only cell (allowing `#` comment lines) extends the macro
    /// table; anything else runs whole as a query — newlines are
    /// whitespace to the engine, and a cell may carry inline `def`s
    /// ahead of its query (the pivot-macro shape).
    pub fn run_cell(&mut self, text: &str) -> String {
        let stripped = quarb::strip_defs_comments(text);
        let t = stripped.trim();
        if t.is_empty() {
            return envelope("", vec![], None, None);
        }
        // `man sort`: the manual, as on a line
        if let Some(page) = manual(t) {
            return page;
        }
        // `mount NAME: ?options`: a re-read under options
        if let Some(reply) = self.remount(t, true) {
            return reply;
        }
        // A notebook names its cell: the previous-line forms are the
        // interactive line's (ruling 2026-11-11).
        if let Some(why) = quarb::previous_line_in_a_file(t) {
            return envelope("", vec![], None, Some(why));
        }
        let label = format!("&{}", self.session.line_no());
        // A definition cell takes its ordinal and answers nothing.
        if (t.starts_with("def ") || t.starts_with("macro ")) && quarb::parse_defs(t).is_ok() {
            return match self.session.add_def(t) {
                Ok(()) => envelope(&label, vec![], Some("definitions added".into()), None),
                Err(e) => envelope("", vec![], None, Some(format!("{e:#}"))),
            };
        }
        // A model cell (spec: .quai, Kinds): the page applied its
        // mounts and declarations when it opened the session
        // (`mountWithModel`); here it takes its ordinal, so the
        // labels match the file's, and answers nothing.
        let first = t.split_whitespace().next().unwrap_or_default();
        // the model crate's one list of statement keywords (`def` and
        // `macro` are cells of their own above)
        if quarb_model::STATEMENTS.contains(&first) && !matches!(first, "def" | "macro") {
            self.session.commit(t, Vec::new());
            return envelope(
                &label,
                vec![],
                Some("model cell — applied when the session opened".into()),
                None,
            );
        }
        match self.session.eval_rich(t) {
            Ok((cells, html)) => {
                let refs = self.session.refs();
                let lines = cells.iter().map(|c| c.display()).collect();
                let note = if self.session.commit(t, cells) {
                    None
                } else {
                    Some(format!("{label} is not referenceable"))
                };
                let diagnostics = self.session.diagnostics();
                envelope_full(&label, lines, note, None, refs, html, diagnostics)
            }
            Err(e) => error_envelope("", &e),
        }
    }

    /// Run one cell of a notebook being replayed, as `quai --run`
    /// does: [`run_cell`](Self::run_cell), but a cell that is
    /// refused keeps its ordinal, so the labels after it are the
    /// file's (the page's loader replays with this; a cell the
    /// reader edits and runs again is a line of the session, and a
    /// failed line takes no label). The manual's page takes none
    /// here either, as in the file.
    pub fn replay_cell(&mut self, text: &str) -> String {
        let label = format!("&{}", self.session.line_no());
        let reply = self.run_cell(text);
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&reply) else {
            return reply;
        };
        let refused = v.get("error").is_some_and(|e| !e.is_null());
        let unlabelled = v.get("label").and_then(|l| l.as_str()) == Some("");
        if refused && unlabelled {
            let stripped = quarb::strip_defs_comments(text);
            self.session.commit(stripped.trim(), Vec::new());
            v["label"] = json!(label);
            return v.to_string();
        }
        reply
    }

    /// Run one input line, returning a JSON envelope (label, lines,
    /// note, error). Mirrors the native REPL's dispatch: `def`/`macro`
    /// definitions, `&N#` frozen recall, `&N!` live re-read, and
    /// ordinary queries.
    pub fn run(&mut self, line: &str) -> String {
        let line = line.trim();
        if line.is_empty() {
            return envelope("", vec![], None, None);
        }
        // `man sort`, `man @|`, `man syntax`: the manual. The page
        // has no shell and no colon commands; this line is its door
        // (the user, 2026-11-12). It is no query: it takes no label.
        if let Some(page) = manual(line) {
            return page;
        }
        // `mount NAME: ?options`: re-read a mounted source under
        // options (ruling 2026-11-12). It takes its ordinal, as a
        // notebook's model cell does, and answers nothing.
        if let Some(reply) = self.remount(line, false) {
            return reply;
        }
        // `&_` — the line before this one, `&-N` the line N back
        // (ruling 2026-11-11): resolved to the label it names before
        // anything reads the line, so the history and a saved
        // notebook carry the `&N` line it is.
        let last = self.session.line_no().saturating_sub(1);
        let resolved = match quarb::resolve_previous_line(line, last) {
            Ok(resolved) => resolved,
            Err(e) => return envelope("", vec![], None, Some(e)),
        };
        let line = resolved.as_deref().unwrap_or(line);

        let label = format!("&{}", self.session.line_no());

        // A definition extends the table but is not run; it takes
        // its ordinal all the same.
        if line.starts_with("def ")
            || line == "def"
            || line.starts_with("macro ")
            || line == "macro"
        {
            return match self.session.add_def(line) {
                Ok(()) => envelope(&label, vec![], Some("definition added".into()), None),
                Err(e) => envelope("", vec![], None, Some(format!("{e:#}"))),
            };
        }

        // A standalone `&N#` — replay the frozen footprint (its rich
        // form under the session's setting, as any line's).
        if let Some(n) = numeric_ref_with(line, '#') {
            return match self.session.frozen(n).cloned() {
                Some(cells) => {
                    let lines = cells.iter().map(|c| c.display()).collect();
                    let html = self.session.rich_of(&cells);
                    self.session.record_frozen(cells);
                    envelope_html(&label, lines, None, None, Vec::new(), html)
                }
                None => envelope(
                    "",
                    vec![],
                    None,
                    Some(format!(
                        "&{n}# has no captured result (line {n} hasn't run)"
                    )),
                ),
            };
        }

        // `&N!` live, ordinary query, or a misplaced frozen reference.
        // A `#` elsewhere is the query's own — the table constructor
        // `| #`, `#(--null=…)`, a line comment — never the suffix
        // (the extension could reach no table before, 2026-11-09).
        let (query, fresh) = if let Some(n) = numeric_ref_with(line, '!') {
            (format!("&{n}"), true)
        } else if embedded_frozen_ref(line) {
            return envelope(
                "",
                vec![],
                None,
                Some("'&N#' recalls a frozen result, valid only as a standalone line".into()),
            );
        } else {
            (line.to_string(), false)
        };

        // every line through the rendering rule (2026-11-09): the
        // rich form beside the lines where the session's setting
        // draws one
        let result = if fresh {
            self.session.eval_fresh(&query).map(|cells| {
                let html = self.session.rich_of(&cells);
                (cells, html)
            })
        } else {
            self.session.eval_rich(&query)
        };
        match result {
            Ok((cells, html)) => {
                let refs = self.session.refs();
                let lines = cells.iter().map(|c| c.display()).collect();
                let note = if self.session.commit(&query, cells) {
                    None
                } else {
                    Some(format!("{label} is not referenceable"))
                };
                let diagnostics = self.session.diagnostics();
                envelope_full(&label, lines, note, None, refs, html, diagnostics)
            }
            Err(e) => error_envelope("", &e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_carries_the_rich_form_under_the_setting() {
        let csv = "Port,Draught (m)\nPiraeus,18.5\nKoper,14\n";
        let mut s = QuaiSession::new("csv", csv, 1_753_500_000_000.0).unwrap();
        // the playground's default: off
        assert_eq!(s.pretty(), "off");
        let r: serde_json::Value = serde_json::from_str(&s.run("/row @| #")).unwrap();
        assert!(r["html"].is_null(), "{r}");
        // the panel's default: structures
        s.set_pretty("structures").unwrap();
        let r: serde_json::Value = serde_json::from_str(&s.run("/row @| #")).unwrap();
        let html = r["html"].as_str().expect("a table value renders");
        assert!(html.starts_with("<table class=\"quarb-table\">"), "{html}");
        assert!(r["lines"][0].as_str().unwrap().starts_with("#(@("), "{r}");
        // the frozen replay keeps the form; a line sets the setting
        let label = r["label"].as_str().unwrap().to_string();
        let r: serde_json::Value = serde_json::from_str(&s.run(&format!("{label}#"))).unwrap();
        assert!(r["html"].as_str().is_some(), "{r}");
        s.run("session(--pretty=off)");
        assert_eq!(s.pretty(), "off");
        let r: serde_json::Value = serde_json::from_str(&s.run("/row @| #")).unwrap();
        assert!(r["html"].is_null(), "{r}");
        // the export: the document form
        s.set_pretty("structures").unwrap();
        let doc = s.render("/row @| #", "html").unwrap();
        assert!(
            doc.starts_with("<style>") && doc.contains("quarb-table"),
            "{doc}"
        );
        assert!(rich_css().contains(".quarb-table th .type"));
    }

    /// `man sort` as a line and as a cell (the user, 2026-11-12): the
    /// page has no shell and no colon commands, so the line is its
    /// door to the manual. It is no query: no label, the history as
    /// it was.
    #[test]
    fn man_is_a_line_of_the_session() {
        let csv = "Port,Draught (m)\nPiraeus,18.5\nKoper,14\n";
        let mut s = QuaiSession::new("csv", csv, 1_753_500_000_000.0).unwrap();
        let read = |text: String| -> serde_json::Value { serde_json::from_str(&text).unwrap() };
        let r = read(s.run("man upper"));
        assert_eq!(r["label"], "", "{r}");
        assert!(r["error"].is_null(), "{r}");
        assert_eq!(r["lines"][0], "upper — the text in upper case", "{r}");
        // a glyph, a pipe (its page, then its commands), the glyphs
        let r = read(s.run("man <=>"));
        assert_eq!(r["lines"][0], "<=> — correlation", "{r}");
        let r = read(s.run("man '@|'"));
        assert_eq!(r["lines"][0], "@| — aggregation pipe", "{r}");
        assert!(
            r["lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l.as_str().unwrap().starts_with("tree ")),
            "{r}"
        );
        let r = read(s.run("man syntax"));
        assert!(r["lines"].as_array().unwrap().len() > 250, "{r}");
        let r = read(s.run("man --ru upper"));
        assert!(r["lines"][0].as_str().unwrap().contains("верхнем"), "{r}");
        // a cell the same
        let r = read(s.run_cell("man sort"));
        assert!(
            r["lines"][0].as_str().unwrap().starts_with("sort — "),
            "{r}"
        );
        // a word the manual lacks is the error, and nothing else
        let r = read(s.run("man qqqq"));
        assert!(
            r["error"].as_str().unwrap().contains("no command 'qqqq'"),
            "{r}"
        );
        // no line of these took a label: the first query is &1
        assert_eq!(s.line(), 1);
        let r = read(s.run("/row @| count"));
        assert_eq!(r["label"], "&1", "{r}");
        // a query that only looks like it is still a query
        let r = read(s.run("/row[::Port = \"man\"] @| count"));
        assert_eq!(r["label"], "&2", "{r}");
    }

    /// `&_` and `&-N` (ruling 2026-11-11): the line form resolves
    /// them to a label, the history carries the label, and a
    /// notebook cell is told to name its cell.
    #[test]
    fn the_previous_line_forms_are_the_line_s() {
        let csv = "Port,Draught (m)\nPiraeus,18.5\nKoper,14\n";
        let mut s = QuaiSession::new("csv", csv, 1_753_500_000_000.0).unwrap();
        let run = |s: &mut QuaiSession, line: &str| -> serde_json::Value {
            serde_json::from_str(&s.run(line)).unwrap()
        };
        let r = run(&mut s, "&_");
        assert_eq!(r["error"], "'&_': no line before this one", "{r}");
        let r = run(&mut s, "/row::Port");
        assert_eq!(r["label"], "&1", "{r}");
        let r = run(&mut s, "&_ @| count");
        assert_eq!(r["label"], "&2", "{r}");
        assert_eq!(r["lines"][0], "2", "{r}");
        // a failed line takes no label and is not the previous line
        let r = run(&mut s, "/row::Port |");
        assert!(r["error"].is_string(), "{r}");
        let r = run(&mut s, "&-2 | upper");
        assert_eq!(r["label"], "&3", "{r}");
        assert_eq!(r["lines"][0], "PIRAEUS", "{r}");
        let r = run(&mut s, "&_#");
        assert_eq!(r["lines"][1], "KOPER", "{r}");
        // what a saved notebook is written from
        let history = s.history();
        assert!(history.contains("def &2: &1 @| count ;"), "{history}");
        assert!(history.contains("def &3: &1 | upper ;"), "{history}");
        assert!(!history.contains("&_"), "{history}");
        // a notebook cell names its cell
        let r: serde_json::Value = serde_json::from_str(&s.run_cell("&_ @| count")).unwrap();
        assert_eq!(
            r["error"],
            "'&_' is the interactive form of a session: a notebook names its cell ('&N')",
            "{r}"
        );
    }

    fn read(text: String) -> serde_json::Value {
        serde_json::from_str(&text).unwrap()
    }

    fn lines(r: &serde_json::Value) -> Vec<String> {
        r["lines"]
            .as_array()
            .map(|a| a.iter().map(|l| l.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    /// `graft(NOTATION)` in a query, in the browser's session (the
    /// user, 2026-11-12): every source the page hands over is read
    /// with the composing layer a native mount gets, so a text
    /// grafts and the thread walks into it. It was refused ("this
    /// host has none").
    #[test]
    fn a_text_grafts_in_the_session() {
        let mut s = QuaiSession::new("json", r#"{"a": 1}"#, 1_753_500_000_000.0).unwrap();
        let r = read(
            s.run(r#"= "{\"scrolls\": [\"Iliad\", \"Odyssey\"]}" | graft(json) | /scrolls/*::"#),
        );
        assert!(r["error"].is_null(), "{r}");
        assert_eq!(lines(&r), ["Iliad", "Odyssey"], "{r}");
        let r = read(s.run(r#"= "<p>One <b>bold</b> word" | graft(html) | //b::"#));
        assert_eq!(lines(&r), ["bold"], "{r}");
        let r = read(s.run(
            r#"= "<scroll pages=\"24\"><title>Iliad</title></scroll>" | graft(xml) | /scroll/title::"#,
        ));
        assert_eq!(lines(&r), ["Iliad"], "{r}");
        // a reading of a lone string, by the session's own readers
        let r = read(
            s.run(r##"= "# Proem\n\nSing, goddess." | graft(text:markdown) | //paragraph::"##),
        );
        assert!(r["error"].is_null(), "{r}");
        // the source's own nodes are as they were
        let r = read(s.run("/a::"));
        assert_eq!(lines(&r), ["1"], "{r}");
        // several sources: the graft lands in a mount's layer
        let sources = r#"[{"name": "a", "format": "json", "text": "{\"k\": 1}"},
                          {"name": "b", "format": "csv", "text": "x,y\n1,2\n"}]"#;
        let mut s = QuaiSession::mount(sources, 1_753_500_000_000.0).unwrap();
        let r = read(s.run(r#"= "[1, 2, 3]" | graft(json) | /* | :: @| sum"#));
        assert_eq!(lines(&r), ["6"], "{r}");
        let r = read(s.run("/b/row | ::y"));
        assert_eq!(lines(&r), ["2"], "{r}");
    }

    /// The formats the loader may name beside the old ones: line
    /// atoms, the property reading of YAML and TOML under their own
    /// names, a reading of a notation through the koine reader.
    #[test]
    fn the_session_reads_lines_the_second_readings_and_the_koine_formats() {
        let mut s = QuaiSession::new("lines", "one\ntwo\nthree\n", 1_753_500_000_000.0).unwrap();
        let r = read(s.run("/* @| count"));
        assert_eq!(lines(&r), ["3"], "{r}");
        let r = read(s.run("/*[2]::"));
        assert_eq!(lines(&r), ["two"], "{r}");

        let mut s =
            QuaiSession::new("yaml2", "room:\n  library: Mouseion\n  open: true\n", 1.0).unwrap();
        let r = read(s.run("/room | %"));
        assert_eq!(
            lines(&r),
            [r#"%(library = "Mouseion"; open = true)"#],
            "{r}"
        );
        let mut s = QuaiSession::new("toml2", "[room]\nlibrary = \"Mouseion\"\n", 1.0).unwrap();
        let r = read(s.run("/room::library"));
        assert_eq!(lines(&r), ["Mouseion"], "{r}");

        // TEI at the text level, by its declared identity
        let tei = r#"<?xml version="1.0"?>
<TEI xmlns="http://www.tei-c.org/ns/1.0"><teiHeader><fileDesc><titleStmt><title>Proem</title></titleStmt>
<publicationStmt><p>x</p></publicationStmt><sourceDesc><p>x</p></sourceDesc></fileDesc></teiHeader>
<text><body><div><head>Book One</head><p>Sing, goddess, the wrath.</p><p>Many a brave soul.</p></div></body></text></TEI>"#;
        let mut s = QuaiSession::new("text-xml", tei, 1.0).unwrap();
        let r = read(s.run("//section/paragraph @| count"));
        assert_eq!(lines(&r), ["2"], "{r}");
        let r = read(s.run("//section::lemma"));
        assert_eq!(lines(&r), ["Book One"], "{r}");
        // the corpus reading of the same text: its tokens beneath
        let mut s = QuaiSession::new("corpus-xml", tei, 1.0).unwrap();
        let r = read(s.run("//section/paragraph[1]//token<word> @| count"));
        assert_eq!(lines(&r), ["4"], "{r}");
    }

    /// `mount NAME: ?options` (ruling 2026-11-12): a line of the
    /// session re-reads a mounted source under the options a native
    /// mount writes after `?`. It takes its ordinal and answers
    /// nothing; the history and the labels stay, and the reply
    /// tells the page what was set.
    #[test]
    fn a_mount_line_re_reads_a_source_under_options() {
        let sources = r#"[{"name": "grain", "format": "csv", "text": "port,tons\nPiraeus,NA\nKoper,\"1,5\"\n"}]"#;
        let mut s = QuaiSession::mount(sources, 1_753_500_000_000.0).unwrap();
        let r = read(s.run("/row | ::tons"));
        assert_eq!(lines(&r), ["NA", "1,5"], "{r}");
        assert_eq!(r["label"], "&1");
        // the name left out with one source
        let r = read(s.run("mount: ?null=NA&decimal=comma"));
        assert!(r["error"].is_null(), "{r}");
        assert_eq!(r["label"], "&2", "{r}");
        assert_eq!(
            r["note"], "grain re-read under null=NA&decimal=comma",
            "{r}"
        );
        assert_eq!(r["mount"][0]["name"], "grain", "{r}");
        assert_eq!(r["mount"][0]["options"], "null=NA&decimal=comma", "{r}");
        let r = read(s.run("/row | ::tons"));
        assert_eq!(lines(&r), ["null", "1.5"], "{r}");
        assert_eq!(r["label"], "&3");
        // the earlier line reads the source as it now is
        let r = read(s.run("&1"));
        assert_eq!(lines(&r), ["null", "1.5"], "{r}");
        // by name, with a closing semicolon; no options resets
        let r = read(s.run("mount grain: ?;"));
        assert_eq!(r["note"], "grain re-read with no options", "{r}");
        let r = read(s.run("/row[1] | ::tons"));
        assert_eq!(lines(&r), ["NA"], "{r}");
        // an option the source does not take is refused, the source
        // left as it was, no label taken
        let before = s.line();
        let r = read(s.run("mount grain: ?nulls=NA"));
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .contains("unknown option \"nulls=NA\""),
            "{r}"
        );
        assert_eq!(r["label"], "", "{r}");
        assert_eq!(s.line(), before);
        let r = read(s.run("/row[1] | ::tons"));
        assert_eq!(lines(&r), ["NA"], "{r}");
        // a name that mounts nothing; a line that names a target
        let r = read(s.run("mount wheat: ?null=NA"));
        assert_eq!(
            r["error"], "no source is mounted as 'wheat'; the sources: grain",
            "{r}"
        );
        let r = read(s.run("mount more: https://example.org/more.csv"));
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .starts_with("a mount line re-reads a mounted source"),
            "{r}"
        );
        // the session's settings stay with it across a re-read
        s.set_pretty("structures").unwrap();
        s.run("mount: ?null=NA");
        assert_eq!(s.pretty(), "structures");

        // a semicolon file opens under the option the page hands over
        let sources = r#"[{"name": "grain", "format": "csv", "text": "port;tons\nPiraeus;1,5\n",
                           "options": "delimiter=semicolon&decimal=comma"},
                          {"name": "ports", "format": "json", "text": "{\"n\": 2}"}]"#;
        let mut s = QuaiSession::mount(sources, 1_753_500_000_000.0).unwrap();
        let r = read(s.run("/grain/row | ::tons"));
        assert_eq!(lines(&r), ["1.5"], "{r}");
        // several sources: the name is asked for
        let r = read(s.run("mount: ?decimal=point"));
        assert_eq!(
            r["error"],
            "name the source to re-read, 'mount NAME: ?decimal=point'; the sources: grain, ports",
            "{r}"
        );
        // an option of another format is told so
        let r = read(s.run("mount ports: ?delimiter=semicolon"));
        assert_eq!(
            r["error"],
            "'ports': \"delimiter=semicolon\" is a CSV option, and this source is read as json",
            "{r}"
        );
        // a page grabbed as plain text is read again as what it is
        let sources = r#"[{"name": "page1", "format": "text", "text": "port,tons\nPiraeus,5\n"}]"#;
        let mut s = QuaiSession::mount(sources, 1_753_500_000_000.0).unwrap();
        let r = read(s.run("mount: ?format=csv"));
        assert!(r["error"].is_null(), "{r}");
        let r = read(s.run("/row | ::tons"));
        assert_eq!(lines(&r), ["5"], "{r}");
        // a notebook's model cell: what it mounts was applied when
        // the session opened, what it re-reads is done now
        let sources = r#"[{"name": "grain", "format": "csv", "text": "port,tons\nPiraeus,NA\n", "named": true}]"#;
        let mut s = QuaiSession::mount(sources, 1_753_500_000_000.0).unwrap();
        let r = read(
            s.replay_cell("mount grain: https://example.org/grain.csv;\nmount grain: ?null=NA;"),
        );
        assert_eq!(r["label"], "&1", "{r}");
        assert_eq!(r["note"], "grain re-read under null=NA", "{r}");
        let r = read(s.replay_cell("/grain/row | ::tons"));
        assert_eq!(lines(&r), ["null"], "{r}");
        let r = read(s.replay_cell("mount grain: https://example.org/grain.csv;"));
        assert_eq!(
            r["note"], "model cell — applied when the session opened",
            "{r}"
        );
        // the unit convention and the root, as a native mount takes them
        let sources =
            r#"[{"name": "trip", "format": "csv", "text": "leg,distance_km\na,12\nb,30\n"}]"#;
        let mut s = QuaiSession::mount(sources, 1_753_500_000_000.0).unwrap();
        s.run("mount: ?autounits");
        let r = read(s.run("/row | ::distance @| sum"));
        assert_eq!(lines(&r), ["42_km"], "{r}");
    }

    /// A notebook's replay keeps the file's ordinals (the user,
    /// 2026-11-12): a cell that is refused takes its number, as
    /// `quai --run` gives it, so the labels after it are the
    /// page's. A line the reader types still takes none.
    /// Base64 of a fixture's bytes, as a page hands a binary source.
    fn b64(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// A mount whose refusal is its text: `QuaiSession::mount` answers
    /// a `JsError`, which exists in a browser alone.
    fn mount(sources: &str, now: f64) -> Result<QuaiSession, String> {
        QuaiSession::open(parse_sources(sources)?, None, now)
    }

    const PROSE: &str = "Tom opened his jacket. Becky laughed.\n";
    const LAYER: &str = "# text = Tom opened his jacket.\n\
        1\tTom\tTom\tPROPN\t_\t_\t2\tnsubj\t_\t_\n\
        2\topened\topen\tVERB\t_\t_\t0\troot\t_\t_\n\
        3\this\this\tPRON\t_\t_\t4\tposs\t_\t_\n\
        4\tjacket\tjacket\tNOUN\t_\t_\t2\tobj\t_\tSpaceAfter=No\n\
        5\t.\t.\tPUNCT\t_\t_\t2\tpunct\t_\t_\n\n\
        # text = Becky laughed.\n\
        1\tBecky\tBecky\tPROPN\t_\t_\t2\tnsubj\t_\t_\n\
        2\tlaughed\tlaugh\tVERB\t_\t_\t0\troot\t_\tSpaceAfter=No\n\
        3\t.\t.\tPUNCT\t_\t_\t2\tpunct\t_\t_\n\n";
    const CAST: &str = "id,name,mention\ntom,Tom Sawyer,Tom\nbecky,Becky Thatcher,Becky\n";

    /// One source as a page hands it, with the files its options name.
    fn source(format: &str, options: &str, files: &[(&str, &str)]) -> String {
        serde_json::json!([{
            "name": "book", "format": format, "text": PROSE, "named": true,
            "options": options,
            "files": files.iter().map(|(n, t)| serde_json::json!({"name": n, "text": t})).collect::<Vec<_>>(),
        }])
        .to_string()
    }

    #[test]
    fn a_mount_option_names_a_second_file() {
        let now = 1_753_500_000_000.0;
        // the annotation layer, by the name the option wrote: a full
        // URL in a published notebook
        let layer = "https://data.example/book.conllu";
        let mut s = mount(
            &source("corpus", &format!("conllu={layer}"), &[(layer, LAYER)]),
            now,
        )
        .unwrap();
        let r = read(s.run(r#"/book//token[::upos = "VERB"] | %(form = ::; ::lemma)"#));
        assert_eq!(
            lines(&r),
            [
                r#"%(form = "opened"; lemma = "open")"#,
                r#"%(form = "laughed"; lemma = "laugh")"#
            ],
            "{r}"
        );
        // without the layer the tokens are the tokenizer's: no tags
        let mut s = mount(&source("corpus", "", &[]), now).unwrap();
        let r = read(s.run(r#"/book//token[::upos = "VERB"] @| count"#));
        assert_eq!(lines(&r), ["0"], "{r}");
        // the layer and a cast table together; the table alone
        let mut s = mount(
            &source(
                "corpus",
                "conllu=book.conllu&cast=cast.csv",
                &[("book.conllu", LAYER), ("cast.csv", CAST)],
            ),
            now,
        )
        .unwrap();
        let r = read(s.run("/book//token[::prosopon] | %(form = ::; ::prosopon; ::upos)"));
        assert_eq!(
            lines(&r),
            [
                r#"%(form = "Tom"; prosopon = "tom"; upos = "PROPN")"#,
                r#"%(form = "Becky"; prosopon = "becky"; upos = "PROPN")"#
            ],
            "{r}"
        );
        // a given segmentation: one sentence where the tokenizer
        // finds two
        let given = mount(
            &source(
                "corpus",
                "sentences=book.sentences",
                &[("book.sentences", PROSE)],
            ),
            now,
        );
        let mut s = given.unwrap();
        let r = read(s.run("/book//sentence @| count"));
        assert_eq!(lines(&r), ["1"], "{r}");
        // a file the page did not hand over is named
        let e = mount(&source("corpus", "conllu=book.conllu", &[]), now)
            .err()
            .unwrap();
        assert!(
            e.contains("conllu=book.conllu names a file this host was not handed"),
            "{e}"
        );
        // a command is never run; an option of the readings on
        // another source, and the layer over a treebank, are refused
        // as the native opener refuses them
        let e = mount(&source("corpus", "annotate=cat", &[]), now)
            .err()
            .unwrap();
        assert!(e.contains("runs a command over the text"), "{e}");
        let e = mount(&source("text", "cast=cast.csv", &[("cast.csv", CAST)]), now)
            .err()
            .unwrap();
        assert!(
            e.contains("is an option of the corpus: and lit: readings"),
            "{e}"
        );
        let treebank = serde_json::json!([{
            "name": "book", "format": "corpus-conllu", "text": LAYER,
            "options": "conllu=book.conllu",
            "files": [{"name": "book.conllu", "text": LAYER}],
        }])
        .to_string();
        let e = mount(&treebank, now).err().unwrap();
        assert!(e.contains("a treebank is its own annotation"), "{e}");
        // a plain text that opens with a curly quote: its ninth
        // byte is inside a character, and sniffing it for a DOCTYPE
        // once cut there
        let curly = serde_json::json!([{
            "name": "book", "format": "corpus", "named": true,
            "text": "\u{201c}Tom!\u{201d}\n\nNo answer.\n",
            "options": "cast=cast.csv",
            "files": [{"name": "cast.csv", "text": CAST}],
        }])
        .to_string();
        let mut s = mount(&curly, now).unwrap();
        let r = read(s.run("/book//token[::prosopon] | ::prosopon"));
        assert_eq!(lines(&r), ["tom"], "{r}");
        // a mount line re-reads the source with a file the session
        // already holds, and refuses one it does not
        let mut s = mount(
            &source(
                "corpus",
                "cast=cast.csv",
                &[("cast.csv", CAST), ("book.conllu", LAYER)],
            ),
            now,
        )
        .unwrap();
        let r = read(s.run("/book//token[::prosopon] @| count"));
        assert_eq!(lines(&r), ["2"], "{r}");
        let r = read(s.run("mount book: ?conllu=book.conllu"));
        assert!(r["error"].is_null(), "{r}");
        let r = read(s.run(r#"/book//token[::upos = "VERB"] @| count"#));
        assert_eq!(lines(&r), ["2"], "{r}");
        let r = read(s.run("mount book: ?places=places.csv"));
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .contains("places=places.csv names a file this host was not handed"),
            "{r}"
        );
    }

    #[test]
    fn the_session_reads_a_workbook_an_archive_and_a_word_processors_file() {
        let now = 1_753_500_000_000.0;
        let one = |name: &str, format: &str, bytes: &[u8], options: &str| {
            serde_json::json!([{
                "name": name, "format": format, "bytes_b64": b64(bytes),
                "named": true, "options": options,
            }])
            .to_string()
        };
        // a workbook: every sheet a table, as the opener mounts one
        let ledger = include_bytes!("../tests/fixtures/ledger.xlsx");
        let mut s = mount(&one("ledger", "xlsx", ledger, ""), now).unwrap();
        let r = read(s.run("/ledger/* | :name"));
        assert_eq!(lines(&r), ["scrolls", "loans"], "{r}");
        let r = read(s.run(r#"/ledger/scrolls/*[::shelf = "epic"] | ::title"#));
        assert_eq!(lines(&r), ["Iliad", "Odyssey", "Theogony"], "{r}");
        // under a mount option
        let mut s = mount(&one("scrolls", "xlsx", ledger, "root=/scrolls"), now).unwrap();
        let r = read(s.run("/scrolls/* | ::lines @| sum"));
        assert_eq!(lines(&r), ["28824"], "{r}");
        // an archive, zip or tar by its bytes: the members, and a
        // member that names a notation walked into
        let members = [
            "/room/catalogue.md",
            "/room/epic/iliad.txt",
            "/room/epic/odyssey.txt",
            "/room/lyric/sappho.txt",
            "/room/notes.json",
        ];
        for (format, bytes) in [
            (
                "zip",
                &include_bytes!("../tests/fixtures/east-room.zip")[..],
            ),
            (
                "tar",
                &include_bytes!("../tests/fixtures/east-room.tar.gz")[..],
            ),
        ] {
            let mut s = mount(&one("room", format, bytes, ""), now).unwrap();
            let r = read(s.run("/room//*<file> | :name-path"));
            assert_eq!(lines(&r), members, "{format}: {r}");
            let r = read(s.run("/room/notes.json/keeper::"));
            assert_eq!(lines(&r), ["Callimachus"], "{format}: {r}");
        }
        // an archive inside an archive opens in place: a zip that
        // holds a zip, a member of the inner one walked into
        let nested = include_bytes!("../tests/fixtures/shipment.zip");
        let mut s = mount(&one("shipment", "zip", nested, ""), now).unwrap();
        let r = read(s.run("/shipment/east-room.zip/notes.json/keeper::"));
        assert_eq!(lines(&r), ["Callimachus"], "{r}");
        let r = read(s.run("/shipment/east-room.zip/epic/* | :name"));
        assert_eq!(lines(&r), ["iliad.txt", "odyssey.txt"], "{r}");
        // a Word document and an OpenDocument text: the text level,
        // and the prose as a corpus
        for (kind, bytes) in [
            (
                "docx",
                &include_bytes!("../tests/fixtures/pinakes.docx")[..],
            ),
            ("odt", &include_bytes!("../tests/fixtures/pinakes.odt")[..]),
        ] {
            let text = format!("text-{kind}");
            let mut s = mount(&one("pinakes", &text, bytes, ""), now).unwrap();
            let r = read(s.run("/pinakes//section | ::lemma"));
            assert_eq!(
                lines(&r),
                ["The Pinakes", "The epic shelf", "The lyric shelf"],
                "{kind}: {r}"
            );
            let corpus = format!("corpus-{kind}");
            let mut s = mount(&one("pinakes", &corpus, bytes, ""), now).unwrap();
            let r = read(s.run("/pinakes//token @| count"));
            assert_eq!(lines(&r), ["60"], "{kind}: {r}");
            // the literary reading has no vocabulary to read there
            let lit = format!("lit-{kind}");
            for options in ["", "notes=omit"] {
                let e = mount(&one("pinakes", &lit, bytes, options), now)
                    .err()
                    .unwrap();
                assert!(e.contains("whose prose corpus: reads"), "{kind}: {e}");
            }
            let koine = format!("koine-{kind}");
            let e = mount(&one("pinakes", &koine, bytes, ""), now)
                .err()
                .unwrap();
            assert!(e.contains("at the text level alone"), "{kind}: {e}");
        }
        // a worksheet the reader fails on is left out, and the first
        // line the session reports says so, once; a chart sheet is
        // left out without a word
        let torn = include_bytes!("../tests/fixtures/torn.xlsx");
        let _ = quarb::take_at_mount();
        let mut s = mount(&one("ledger", "xlsx", torn, ""), now).unwrap();
        let r = read(s.run("/ledger/* | :name"));
        assert_eq!(lines(&r), ["scrolls"], "{r}");
        let told: Vec<&str> = r["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["code"].as_str().unwrap())
            .collect();
        assert_eq!(told, ["UNREADABLE_SHEET"], "{r}");
        assert!(
            r["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .starts_with("the sheet 'torn' could not be read and is left out: "),
            "{r}"
        );
        let r = read(s.run("/ledger/scrolls/* @| count"));
        assert!(
            r["diagnostics"].as_array().is_none_or(|d| d.is_empty()),
            "{r}"
        );
        // handed over as text, a binary source says what it is
        let e = mount(
            r#"[{"name": "ledger", "format": "xlsx", "text": "PK"}]"#,
            now,
        )
        .err()
        .unwrap();
        assert!(e.contains("a workbook is handed over as bytes"), "{e}");
    }

    #[test]
    fn a_refused_cell_keeps_its_label_in_a_replay() {
        let csv = "leader,ships\nAgamemnon,100\nNestor,90\n";
        let mut s = QuaiSession::new("csv", csv, 1_753_500_000_000.0).unwrap();
        let r = read(s.replay_cell("/row @| count"));
        assert_eq!(r["label"], "&1", "{r}");
        let r = read(s.replay_cell("/row @| # | [:shipz > 60]"));
        assert!(
            r["error"].as_str().unwrap().contains("no column 'shipz'"),
            "{r}"
        );
        assert_eq!(r["label"], "&2", "{r}");
        let r = read(s.replay_cell("/row[..1] | ::leader"));
        assert_eq!(r["label"], "&3", "{r}");
        assert_eq!(lines(&r), ["Agamemnon"], "{r}");
        // the manual takes no ordinal in a file either
        let r = read(s.replay_cell("man upper"));
        assert_eq!(r["label"], "", "{r}");
        let r = read(s.replay_cell("/row @| count"));
        assert_eq!(r["label"], "&4", "{r}");
        // a line of the session that fails takes none
        let r = read(s.run("/row @| # | [:shipz > 60]"));
        assert_eq!(r["label"], "", "{r}");
        let r = read(s.run("/row @| count"));
        assert_eq!(r["label"], "&5", "{r}");
    }

    #[test]
    fn a_hash_in_a_query_is_the_querys_own() {
        assert!(!embedded_frozen_ref("//table[1] | #"));
        assert!(!embedded_frozen_ref("//table[1] | #(--null=@(\"—\"))"));
        assert!(!embedded_frozen_ref("= @(%(a = 1)) | # | dim"));
        assert!(!embedded_frozen_ref("# a comment"));
        assert!(!embedded_frozen_ref("&3"));
        assert!(embedded_frozen_ref("&3# | count"));
        assert!(embedded_frozen_ref("(&12#) | dim"));
        assert!(embedded_frozen_ref("&3#"));
        assert_eq!(numeric_ref_with("&3#", '#'), Some(3));
    }
}
