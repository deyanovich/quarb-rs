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

use quarb_session::{Doc, LocalExecutor, MemStore, Session};
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
    let chars: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
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

/// Match a bare capture ref `&<digits><suffix>` (the whole line).
fn numeric_ref_with(line: &str, suffix: char) -> Option<usize> {
    line.strip_suffix(suffix)?
        .strip_prefix('&')?
        .parse::<usize>()
        .ok()
}

/// A result envelope for JS: the `&N` label, the output lines, and an
/// optional note or error (exactly one of note/error, or neither).
fn envelope(label: &str, lines: Vec<String>, note: Option<String>, error: Option<String>) -> String {
    json!({
        "label": label,
        "lines": lines,
        "note": note,
        "error": error,
    })
    .to_string()
}

#[wasm_bindgen]
pub struct QuaiSession {
    session: Session,
}

#[wasm_bindgen]
impl QuaiSession {
    /// Open a session over `input` parsed as `format` (json, yaml,
    /// toml, csv, tsv, xml, html, markdown). `now_millis` pins the
    /// session's `now()` (pass `Date.now()`).
    #[wasm_bindgen(constructor)]
    pub fn new(format: &str, input: &str, now_millis: f64) -> Result<QuaiSession, JsError> {
        let doc = Doc::parse(input, format).map_err(|e| JsError::new(&e.to_string()))?;
        let executor = Box::new(LocalExecutor::new(doc, now_parts(now_millis), false));
        Ok(QuaiSession {
            session: Session::new(executor, Box::new(MemStore)),
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
        let sources: serde_json::Value = serde_json::from_str(sources_json)
            .map_err(|e| JsError::new(&format!("bad sources array: {e}")))?;
        let mut parts: Vec<(String, Doc)> = Vec::new();
        for s in sources.as_array().into_iter().flatten() {
            let field = |k: &str| s.get(k).and_then(|v| v.as_str()).map(str::to_owned);
            let (Some(name), Some(format)) = (field("name"), field("format")) else {
                return Err(JsError::new("each source needs name and format"));
            };
            let doc = if let Some(b64) = field("bytes_b64") {
                let bytes = b64_decode(&b64)
                    .ok_or_else(|| JsError::new(&format!("'{name}': bad base64")))?;
                match format.as_str() {
                    "sqlite" | "db" => Doc::sqlite_bytes(&bytes)
                        .map_err(|e| JsError::new(&format!("'{name}': {e:#}")))?,
                    other => {
                        return Err(JsError::new(&format!(
                            "'{name}': binary sources must be sqlite, not {other}"
                        )));
                    }
                }
            } else if let Some(text) = field("text") {
                Doc::parse(&text, &format)
                    .map_err(|e| JsError::new(&format!("'{name}': {e:#}")))?
            } else {
                return Err(JsError::new(&format!(
                    "'{name}': a source needs text or bytes_b64"
                )));
            };
            parts.push((name, doc));
        }
        let doc = match parts.len() {
            0 => return Err(JsError::new("no sources to mount")),
            // A single source stays path-bare at the root.
            1 => parts.remove(0).1,
            _ => Doc::mount_docs(parts).map_err(|e| JsError::new(&format!("{e:#}")))?,
        };
        let executor = Box::new(LocalExecutor::new(doc, now_parts(now_millis), false));
        Ok(QuaiSession {
            session: Session::new(executor, Box::new(MemStore)),
        })
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

    /// Run one input line, returning a JSON envelope (label, lines,
    /// note, error). Mirrors the native REPL's dispatch: `def`/`macro`
    /// definitions, `&N#` frozen recall, `&N!` live re-read, and
    /// ordinary queries.
    pub fn run(&mut self, line: &str) -> String {
        let line = line.trim();
        if line.is_empty() {
            return envelope("", vec![], None, None);
        }

        // A definition extends the table but is not run.
        if line.starts_with("def ")
            || line == "def"
            || line.starts_with("macro ")
            || line == "macro"
        {
            return match self.session.add_def(line) {
                Ok(()) => envelope("", vec![], Some("definition added".into()), None),
                Err(e) => envelope("", vec![], None, Some(format!("{e:#}"))),
            };
        }

        let label = format!("&{}", self.session.line_no());

        // A standalone `&N#` — replay the frozen footprint.
        if let Some(n) = numeric_ref_with(line, '#') {
            return match self.session.frozen(n).cloned() {
                Some(cells) => {
                    let lines = cells.iter().map(|c| c.display()).collect();
                    self.session.record_frozen(cells);
                    envelope(&label, lines, None, None)
                }
                None => envelope(
                    "",
                    vec![],
                    None,
                    Some(format!("&{n}# has no captured result (line {n} hasn't run)")),
                ),
            };
        }

        // `&N!` live, ordinary query, or a misplaced `#`.
        let (query, fresh) = if let Some(n) = numeric_ref_with(line, '!') {
            (format!("&{n}"), true)
        } else if line.contains('#') {
            return envelope(
                "",
                vec![],
                None,
                Some("'#' is the frozen-history suffix, valid only as a standalone '&N#'".into()),
            );
        } else {
            (line.to_string(), false)
        };

        let result = if fresh {
            self.session.eval_fresh(&query)
        } else {
            self.session.eval(&query)
        };
        match result {
            Ok(cells) => {
                let lines = cells.iter().map(|c| c.display()).collect();
                let note = if self.session.commit(&query, cells) {
                    None
                } else {
                    Some(format!("{label} is not referenceable"))
                };
                envelope(&label, lines, note, None)
            }
            Err(e) => envelope("", vec![], None, Some(format!("{e:#}"))),
        }
    }
}
