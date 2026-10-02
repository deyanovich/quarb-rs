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
//! [`Site`] is the second entry point: a tarball of pages loaded
//! once and queried many times at the web level — a whole site
//! as one arbor (`/sites/<host>/pages/<path>` for every page,
//! the declared tags and categories as rows, every hyperlink
//! resolved), each page grafted at the text level on entry
//! (sections and paragraphs, the page wearing what its head
//! declares). Lowering is lazy per page and cached, so the first
//! sweep pays for the pages it touches and later queries are
//! fast.
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
/// (json | yaml | toml | csv | tsv | xml | html | markdown |
/// text-html | text-markdown | text | corpus-html | corpus-markdown | corpus).
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
        // The text level: the shared section/paragraph vocabulary,
        // produced per source format ("text" is plain text —
        // blank-line paragraphs).
        "text-html" => {
            let a = quarb_text_html::parse(input);
            go(query, &a, |n| a.locator(n), secs, nanos)
        }
        "text-markdown" => {
            let a = quarb_text_markdown::parse(input);
            go(query, &a, |n| a.locator(n), secs, nanos)
        }
        "text" => {
            let a = quarb_text::TextModel::parse_plain(input);
            go(query, &a, |n| a.locator(n), secs, nanos)
        }
        // A treebank: as a document, and as a corpus with its own
        // tokens.
        "conllu" | "corpus-conllu" => {
            let parsed = if format == "conllu" {
                quarb_text::TextModel::parse_conllu_text(input)
            } else {
                quarb_text::TextModel::parse_conllu_corpus(input)
            };
            match parsed {
                Ok(a) => go(query, &a, |n| a.locator(n), secs, nanos),
                Err(e) => Err(format!("reading CoNLL-U: {e}")),
            }
        }
        // The corpus reading: the same three, tokenized.
        "corpus-html" | "corpus-markdown" | "corpus" => {
            let mut a = match format {
                "corpus-html" => quarb_text_html::parse(input),
                "corpus-markdown" => quarb_text_markdown::parse(input),
                _ => quarb_text::TextModel::parse_plain(input),
            };
            a.tokenize();
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
            Ok(values.into_iter().map(|v| v.display_form()).collect())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// A site held in memory: a tar or tar.gz of its pages at the
/// web level — `/sites/<host>/pages/<path>` for every page, the
/// tags and categories the pages declare as rows of their own,
/// every hyperlink resolved (`->link`, `<-link`), each page
/// grafted at the text level on entry. Load once, query many
/// times.
#[wasm_bindgen]
pub struct Site {
    inner: quarb_model::ModelAdapter<quarb_web::WebAdapter<quarb_web::MemoryStore>>,
    /// The model's own `def`/`macro` statements, in scope for every
    /// query over the site.
    defs: String,
}

#[wasm_bindgen]
impl Site {
    /// Load a tarball (gzipped or not). Pages join against the
    /// base read off a page's canonical URL. `text_level` is kept
    /// for the caller's sake: the web level always reads its
    /// pages as the reader's model.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8], text_level: bool) -> Result<Site, JsError> {
        Self::with_model(bytes, text_level, "")
    }

    /// Like [`new`](Self::new), with a model file over the site:
    /// aliases (`alias <s/^tag://i>;`), derived nodes, edges.
    pub fn with_model(bytes: &[u8], _text_level: bool, model: &str) -> Result<Site, JsError> {
        Self::open(bytes, "", model)
    }

    /// Open a tarball (gzipped or not) with every option spelled
    /// out: `base_url` is the site's root (`https://example.org/`)
    /// that every page path and relative link joins against —
    /// empty to read it off a page's canonical URL, falling back
    /// to `https://localhost/` when no page declares one; `model`
    /// is a model file over the site, empty for none.
    pub fn open(bytes: &[u8], base_url: &str, model: &str) -> Result<Site, JsError> {
        let site = quarb_web::archive::open_tar(bytes, base_url)
            .map_err(|e| JsError::new(&format!("reading the archive: {e}")))?;
        Self::over(site, model)
    }

    /// The same site from pages already in memory: `pages` is a
    /// JSON object of site-relative path to HTML
    /// (`{"index.html": "<!doctype html>…", "about/index.html": …}`)
    /// — no tarball needed for a site small enough to hold in a
    /// string. `base_url` and `model` as in [`open`](Self::open).
    pub fn from_pages(pages: &str, base_url: &str, model: &str) -> Result<Site, JsError> {
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(pages)
            .map_err(|e| JsError::new(&format!("reading the pages: {e}")))?;
        let files = map
            .into_iter()
            .map(|(path, html)| {
                let html = match html {
                    serde_json::Value::String(s) => Ok(s),
                    _ => Err(JsError::new(&format!(
                        "page {path}: the value must be an HTML string"
                    ))),
                }?;
                Ok(quarb_web::PageFile { path, html })
            })
            .collect::<Result<Vec<_>, JsError>>()?;
        let store = quarb_web::MemoryStore::build(
            quarb_web::SiteInput {
                base_url: base_url.to_string(),
                snapshot: None,
            },
            files,
        );
        Self::over(quarb_web::WebAdapter::new(store), model)
    }

    fn over(
        site: quarb_web::WebAdapter<quarb_web::MemoryStore>,
        model: &str,
    ) -> Result<Site, JsError> {
        let model = quarb_model::parse_model(model)
            .map_err(|e| JsError::new(&format!("reading the model: {e}")))?;
        let defs = model.defs_text.clone();
        Ok(Site {
            inner: quarb_model::ModelAdapter::new(site, model),
            defs,
        })
    }

    /// Run `query` over the site; the same JSON envelope as
    /// [`run`]. Node results render as locators
    /// (`/sites/quarb.org/pages/guides/jq.html!/section[2]`); a
    /// model's derived nodes as `/<container>/<role>[n]`.
    pub fn query(&self, query: &str, now_millis: f64) -> String {
        let secs = (now_millis / 1000.0).floor() as i64;
        let nanos = ((now_millis / 1000.0).fract() * 1e9) as u32;
        let a = &self.inner;
        let render = |n: NodeId| a.locator(n, |b| a.base().locator(b));
        let query = if self.defs.trim().is_empty() {
            query.to_string()
        } else {
            format!("{}\n{}", self.defs, query)
        };
        let outcome = go(&query, a, render, secs, nanos);
        match outcome {
            Ok(lines) => serde_json::json!({ "ok": true, "lines": lines }).to_string(),
            Err(e) => serde_json::json!({ "ok": false, "error": e }).to_string(),
        }
    }

    /// The pages as locators, for a quick listing without a query.
    pub fn entries(&self) -> String {
        serde_json::json!(self.pages().into_iter().map(|(l, _)| l).collect::<Vec<_>>()).to_string()
    }

    /// Every page's locator with its declared title, as a JSON
    /// object — read from the store, no page parsed.
    pub fn titles(&self) -> String {
        let map: serde_json::Map<String, serde_json::Value> = self
            .pages()
            .into_iter()
            .filter_map(|(l, t)| t.map(|t| (l, serde_json::Value::String(t))))
            .collect();
        serde_json::Value::Object(map).to_string()
    }
}

impl Site {
    fn pages(&self) -> Vec<(String, Option<String>)> {
        use quarb_web::{Container, PageKind, WebStore};
        let a = self.inner.base();
        let store = a.store();
        let mut out = Vec::new();
        for site in &store.catalog().sites {
            let mut stack: Vec<Option<quarb_web::PageKey>> = vec![None];
            while let Some(parent) = stack.pop() {
                for k in store.children(site.id, Container::Pages, parent) {
                    let Some(r) = store.row(k) else { continue };
                    if r.kind == PageKind::Page {
                        out.push((format!("/sites/{}/pages/{}", site.host, r.path), r.title));
                    } else {
                        stack.push(Some(k));
                    }
                }
            }
        }
        out.sort();
        out
    }
}
