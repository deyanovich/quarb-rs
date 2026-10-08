//! Opening a source into a queryable adapter, and running queries
//! against it.
//!
//! `AstAdapter` is object-safe, but each adapter's *render* method
//! (`pointer` / `locator` / `path`) is an inherent method, not on the
//! trait — so, as the Python bindings do, we hold one of a fixed set
//! of adapter families in an enum and dispatch render (and the
//! `WithNow`/`AllowShell` query wrap) by variant.
//!
//! The text-format variants always compile (they are wasm-safe); the
//! native fleet (filesystem, git, SQLite, archives, spreadsheets,
//! source code, mounts) is gated behind the `native` feature, as is
//! the filesystem `open`/`mount` dispatch. The wasm build drives
//! everything through [`Doc::parse`].

use anyhow::{Context, Result, bail};
use quarb::{AllowShell, NodeId, QueryResult, WithNow};

#[cfg(feature = "native")]
use std::path::Path;
use std::rc::Rc;

/// Options that shape how native sources open (unused on wasm, which
/// only parses text).
#[derive(Clone, Default)]
pub struct Options {
    pub hidden: bool,
    pub respect_ignore: bool,
    /// Opt directory mounts into grafting (qua's --graft).
    pub graft: bool,
    /// Disable grafting entirely (qua's --no-graft).
    pub no_graft: bool,
    /// Declared references, `(field, container)` pairs — the parsed
    /// `--refs` document, consumed by the SQLite mounts.
    pub refs: Rc<Vec<(String, String)>>,
    /// The shell gate (qua's --allow-shell): `corpus:…?annotate=CMD`
    /// runs a command only under it.
    pub allow_shell: bool,
    /// The session's sentence bonds (a `.desm` set), for the corpus
    /// reading; `?desm=FILE` on a mount adds to them.
    pub bonds: Option<Rc<syndesmos::Syndesmos>>,
}

#[cfg(feature = "native")]
/// The session's declaration reporter at the source boundary: an
/// XML prologue for XML-family names, the HTML prescan for HTML
/// names, both tried for a name that says nothing.
fn declared_encoding(name: &str, bytes: &[u8]) -> Option<String> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "xml" | "svg" | "xhtml" | "fb2" | "tei" | "rss" | "atom" | "opml" | "xsd" | "xsl" => {
            quarb_xml::declared_encoding(bytes)
        }
        "html" | "htm" => quarb_html::declared_encoding(bytes),
        // RTF's header names its code page (`\ansicpg1251`).
        "rtf" => quarb_text_rtf::declared_encoding(bytes),
        _ => quarb_xml::declared_encoding(bytes).or_else(|| quarb_html::declared_encoding(bytes)),
    }
}

#[cfg(feature = "native")]
/// Read one file at the source boundary: `?encoding=` (the
/// `explicit` label), then a byte-order mark, then what the bytes
/// declare, then strict UTF-8 — refused, naming the option, when
/// none applies.
fn read_source(path: &Path, explicit: Option<&str>) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    quarb::source::SourceDecoding::default()
        .with_declared(declared_encoding)
        .with_option(explicit)
        .and_then(|d| d.decode(name, &bytes))
        .map(|d| d.text)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))
}

#[cfg(feature = "native")]
/// Take `?encoding=LABEL` off a plain target (one that is not a
/// path as written; a reader prefix parses its own options).
fn take_encoding_option(path: &Path) -> Result<(std::path::PathBuf, Option<String>)> {
    let (p, explicit, _) = take_source_options(path)?;
    Ok((p, explicit))
}

#[cfg(feature = "native")]
/// The source options off a plain target: `?encoding=`, and for a
/// delimited file `?delimiter=` / `?decimal=`.
/// Take `references=STANDARD` off a target's query, wherever it
/// stands among the other options; the standard is validated here.
fn take_references_option(path: &Path) -> Result<(std::path::PathBuf, Option<String>)> {
    let Some(s) = path.to_str() else {
        return Ok((path.to_path_buf(), None));
    };
    if path.exists() || !s.contains("references=") {
        return Ok((path.to_path_buf(), None));
    }
    let Some((p, query)) = s.rsplit_once('?') else {
        return Ok((path.to_path_buf(), None));
    };
    let mut standard = None;
    let mut kept = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some(("references", v)) => {
                anyhow::ensure!(
                    quarb::bibref::STANDARDS.contains(&v),
                    "{s}: references={v} — references= takes {}",
                    quarb::bibref::STANDARDS.join(", ")
                );
                standard = Some(v.to_string());
            }
            _ => kept.push(pair),
        }
    }
    let rebuilt = if kept.is_empty() {
        p.to_string()
    } else {
        format!("{p}?{}", kept.join("&"))
    };
    Ok((std::path::PathBuf::from(rebuilt), standard))
}

#[cfg(feature = "native")]
fn take_source_options(
    path: &Path,
) -> Result<(std::path::PathBuf, Option<String>, quarb_csv::CsvOptions)> {
    let mut csv = quarb_csv::CsvOptions::default();
    let Some(s) = path.to_str() else {
        return Ok((path.to_path_buf(), None, csv));
    };
    let prefixed = s.split_once(':').is_some_and(|(pre, _)| {
        !pre.is_empty()
            && pre
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    if path.exists() || !s.contains('?') || prefixed {
        return Ok((path.to_path_buf(), None, csv));
    }
    let (p, query) = s.rsplit_once('?').unwrap();
    let mut explicit = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some(("encoding", label)) => {
                quarb::source::encoding_for_label(label)
                    .ok_or_else(|| anyhow::anyhow!("{s}: unknown encoding label {label:?}"))?;
                explicit = Some(label.to_string());
            }
            Some(("delimiter", label)) => {
                csv = csv
                    .with_delimiter_label(label)
                    .map_err(|e| anyhow::anyhow!("{s}: {e}"))?;
            }
            Some(("decimal", label)) => {
                csv = csv
                    .with_decimal_label(label)
                    .map_err(|e| anyhow::anyhow!("{s}: {e}"))?;
            }
            _ => bail!(
                "unknown option {pair:?} on {p} — a file or folder takes encoding=, \
                 delimiter=, decimal="
            ),
        }
    }
    Ok((std::path::PathBuf::from(p), explicit, csv))
}

/// A materialized source: one variant per adapter family. JSON-model
/// formats (json/yaml/toml) render node results as pointers, the rest
/// as locators.
pub enum Doc {
    Json(quarb_json::JsonAdapter),
    Csv(quarb_csv::CsvAdapter),
    Xml(quarb_xml::XmlAdapter),
    Html(quarb_html::HtmlAdapter),
    Text(quarb_text::TextModel),
    #[cfg(feature = "sqlite")]
    Sqlite(quarb_sqlite::SqliteAdapter),
    #[cfg(feature = "native")]
    Fs(quarb_fs::FsAdapter),
    #[cfg(feature = "native")]
    FsDeep(quarb_compose::ComposeAdapter<quarb_fs::FsAdapter>),
    #[cfg(feature = "native")]
    Git(quarb_git::GitAdapter),
    #[cfg(feature = "native")]
    Archive(quarb_compose::ComposeAdapter<quarb_archive::ArchiveAdapter>),
    /// An archive held opaque (`no_graft`): the member tree with
    /// leaves as plain entries — the tar -t view.
    #[cfg(feature = "native")]
    ArchiveRaw(quarb_archive::ArchiveAdapter),
    #[cfg(feature = "native")]
    Xlsx(quarb_xlsx::XlsxAdapter),
    #[cfg(feature = "native")]
    Syntax(quarb_tree_sitter::TreeSitterAdapter),
    #[cfg(feature = "native")]
    Code(quarb_code::CodeModel),
    Mount(quarb_mount::MountAdapter),
    /// Any adapter behind the object-safe trait, with its locator
    /// renderer — the carrier for scheme targets opened through
    /// qua's dispatch (`gcl:`, `kafka:`, `neo4j://`, …).
    Boxed(Dyn, Box<dyn Fn(NodeId) -> String>),
}

/// A boxed adapter as an adapter — plain delegation, every method
/// forwarded.
pub struct Dyn(pub Box<dyn quarb::AstAdapter>);

impl quarb::AstAdapter for Dyn {
    quarb::forward_ast_adapter!(0);
}

impl Doc {
    /// The kaiv door: the adapter is Rc-shared between the Doc and
    /// its locator renderer (the `Shared` pattern qua's scheme
    /// mounts use), riding the Boxed variant.
    fn boxed_kaiv(a: quarb_kaiv::KaivAdapter) -> Doc {
        let a = std::rc::Rc::new(a);
        let r = a.clone();
        Doc::Boxed(
            Dyn(Box::new(quarb_mount::Shared(a))),
            Box::new(move |n| r.locator(n)),
        )
    }

    /// Parse a text document by format name — the wasm entry point,
    /// and the text tail of the native `open`. Formats: json, yaml,
    /// toml, csv, tsv, xml, html, markdown, jsonl/ndjson, kaiv/daiv.
    pub fn parse(input: &str, format: &str) -> Result<Doc> {
        match format {
            // kaiv rides the Boxed door (no dedicated variant): the
            // offline resolver — a browser mount has no filesystem
            // or registry, so `.!units`/`.!types` imports beyond the
            // embedded core fail with kaiv's own pointed error.
            "kaiv" => {
                let a = quarb_kaiv::KaivAdapter::parse_kaiv(input)
                    .map_err(|e| anyhow::anyhow!("parsing kaiv: {e}"))?;
                return Ok(Self::boxed_kaiv(a));
            }
            "daiv" => {
                let a = quarb_kaiv::KaivAdapter::parse_daiv(input)
                    .map_err(|e| anyhow::anyhow!("parsing daiv: {e}"))?;
                return Ok(Self::boxed_kaiv(a));
            }
            "json" => quarb_json::JsonAdapter::parse(input)
                .map(Doc::Json)
                .context("parsing JSON"),
            // The property reading: containers are nodes, scalars
            // are properties (json2:FILE in qua; the format name for
            // a pasted document).
            "json2" => quarb_json::JsonAdapter::parse_as(input, quarb_json::Reading::Property)
                .map(Doc::Json)
                .context("parsing JSON"),
            "jsonl" | "ndjson" => quarb_json::JsonAdapter::parse_lines(input)
                .map(Doc::Json)
                .context("parsing JSONL"),
            "yaml" | "yml" => quarb_yaml::parse(input)
                .map(Doc::Json)
                .context("parsing YAML"),
            "toml" => quarb_toml::parse(input)
                .map(Doc::Json)
                .context("parsing TOML"),
            "csv" => quarb_csv::CsvAdapter::parse_declared(input, b',', Default::default())
                .map(Doc::Csv)
                .context("parsing CSV"),
            "tsv" => quarb_csv::CsvAdapter::parse_declared(input, b'\t', Default::default())
                .map(Doc::Csv)
                .context("parsing TSV"),
            "xml" => quarb_xml::XmlAdapter::parse(input)
                .map(Doc::Xml)
                .context("parsing XML"),
            "html" => Ok(Doc::Html(quarb_html::HtmlAdapter::parse(input))),
            "markdown" | "md" => Ok(Doc::Html(quarb_markdown::parse(input))),
            // LaTeX source, through the text-level producer — a
            // pure text parser, so the wasm build carries it too.
            "tex" | "latex" => Ok(Doc::Text(quarb_text_latex::parse(input))),
            // The text level: the shared section/paragraph
            // vocabulary, produced per source format ("text" is
            // plain text — blank-line paragraphs).
            "text-html" => Ok(Doc::Text(quarb_text_html::parse(input))),
            "text-markdown" | "text-md" => Ok(Doc::Text(quarb_text_markdown::parse(input))),
            "text" => Ok(Doc::Text(quarb_text::TextModel::parse_plain(input))),
            "text-rtf" => quarb_text_rtf::parse(input.as_bytes())
                .map(Doc::Text)
                .map_err(|e| anyhow::anyhow!("reading RTF: {e}")),
            // A treebank: as a document, and as a corpus
            // with its own tokens.
            "conllu" => quarb_text::TextModel::parse_conllu_text(input)
                .map(Doc::Text)
                .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}")),
            "corpus-conllu" => quarb_text::TextModel::parse_conllu_corpus(input)
                .map(Doc::Text)
                .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}")),
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
            // BibTeX / BibLaTeX, imported through atrep's
            // bibliogramma: a document of bib entries.
            #[cfg(feature = "native")]
            "text-bibtex" | "bibtex" => quarb_text_koine::parse_bibtex(input)
                .map(Doc::Text)
                .map_err(|e| anyhow::anyhow!("{e}")),
            // FictionBook 2 at the text level, through the koine
            // route (atrep's importer).
            #[cfg(feature = "native")]
            "text-fb2" | "fb2" => quarb_text_koine::parse_xml_as(input, "fb2")
                .map(Doc::Text)
                .map_err(|e| anyhow::anyhow!("{e}")),
            // The code level: identifiers as names; the format
            // name carries the language.
            #[cfg(feature = "native")]
            "code-rust" => quarb_code::CodeModel::parse(input, "rs")
                .map(Doc::Code)
                .context("parsing Rust at the code level"),
            #[cfg(feature = "native")]
            "code-python" => quarb_code::CodeModel::parse(input, "py")
                .map(Doc::Code)
                .context("parsing Python at the code level"),
            #[cfg(feature = "native")]
            "code-javascript" => quarb_code::CodeModel::parse(input, "js")
                .map(Doc::Code)
                .context("parsing JavaScript at the code level"),
            #[cfg(feature = "native")]
            "code-c" => quarb_code::CodeModel::parse(input, "c")
                .map(Doc::Code)
                .context("parsing C at the code level"),
            other => bail!("unknown format: {other}"),
        }
    }

    /// Run one query against this source with the session's invocation
    /// instant and shell permission. The query text carries any macro
    /// definitions inline (the session prepends its table), which
    /// `quarb::run` expands.
    /// The concrete adapter behind this `Doc`, as `&dyn` — the base a
    /// `--model` enrichment layer wraps (one match, so the model
    /// paths avoid duplicating the variant arms). Wasm-safe; the
    /// native-only variants are compiled in only under `native`.
    pub(crate) fn base_dyn(&self) -> &dyn quarb::AstAdapter {
        match self {
            Doc::Json(a) => a,
            Doc::Csv(a) => a,
            Doc::Xml(a) => a,
            Doc::Html(a) => a,
            Doc::Text(a) => a,
            #[cfg(feature = "sqlite")]
            Doc::Sqlite(a) => a,
            #[cfg(feature = "native")]
            Doc::Fs(a) => a,
            #[cfg(feature = "native")]
            Doc::FsDeep(a) => a,
            #[cfg(feature = "native")]
            Doc::Git(a) => a,
            #[cfg(feature = "native")]
            Doc::Archive(a) => a,
            #[cfg(feature = "native")]
            Doc::ArchiveRaw(a) => a,
            #[cfg(feature = "native")]
            Doc::Xlsx(a) => a,
            #[cfg(feature = "native")]
            Doc::Syntax(a) => a,
            #[cfg(feature = "native")]
            Doc::Code(a) => a,
            Doc::Mount(a) => a,
            Doc::Boxed(a, _) => &*a.0,
        }
    }

    /// Run `query` and render its results as exportable markup:
    /// `md`/`markdown`, `html`, or `txt`/`text`. Node results render
    /// structurally through the text vocabulary — sections back to
    /// headings, lists to lists — and kinds outside it degrade to
    /// prose paragraphs; value results render as lines.
    pub fn export(
        &self,
        query: &str,
        now: (i64, u32),
        allow_shell: bool,
        kind: &str,
    ) -> Result<String> {
        let render = quarb_text::Render::from_name(kind)
            .ok_or_else(|| anyhow::anyhow!("unknown export format: {kind} (md, html, txt)"))?;
        // An empty query exports the whole document.
        if query.trim().is_empty() {
            let base = self.base_dyn();
            return Ok(quarb_text::render_nodes(base, &[base.root()], render));
        }
        match self
            .run(query, now, allow_shell)
            .map_err(|e| anyhow::anyhow!("{e}"))?
        {
            QueryResult::Nodes(nodes) => {
                Ok(quarb_text::render_nodes(self.base_dyn(), &nodes, render))
            }
            QueryResult::Values(values) => Ok(quarb_text::render::render_values(&values, render)),
        }
    }

    /// Run against a `--model`-enriched view of this source: the
    /// derived containers, references, and edges the model declares,
    /// over this `Doc`'s base. `now` binds `now()` for the base and
    /// its constructor queries alike.
    pub fn run_modeled(
        &self,
        query: &str,
        now: (i64, u32),
        allow_shell: bool,
        model: &quarb_model::Model,
    ) -> quarb::Result<QueryResult> {
        let (secs, nanos) = now;
        let base = quarb_model::Borrowed(self.base_dyn());
        let nowed = WithNow {
            inner: &base,
            secs,
            nanos,
        };
        let enriched = quarb_model::ModelAdapter::new(nowed, model.clone());
        enriched
            .check()
            .map_err(|e| quarb::QuarbError::Parse(format!("reading the model: {e}")))?;
        if allow_shell {
            quarb::run(query, &AllowShell { inner: &enriched })
        } else {
            quarb::run(query, &enriched)
        }
    }

    /// Render a node from a model-enriched run: `/container/value`
    /// for derived nodes, the base's own renderer otherwise.
    pub fn render_modeled(&self, node: NodeId, model: &quarb_model::Model) -> String {
        let enriched =
            quarb_model::ModelAdapter::new(quarb_model::Borrowed(self.base_dyn()), model.clone());
        enriched.locator(node, |bn| self.render(bn))
    }

    pub fn run(
        &self,
        query: &str,
        now: (i64, u32),
        allow_shell: bool,
    ) -> quarb::Result<QueryResult> {
        let (secs, nanos) = now;
        macro_rules! go {
            ($a:expr) => {{
                let nowed = WithNow {
                    inner: $a,
                    secs,
                    nanos,
                };
                if allow_shell {
                    quarb::run(query, &AllowShell { inner: &nowed })
                } else {
                    quarb::run(query, &nowed)
                }
            }};
        }
        match self {
            Doc::Json(a) => go!(a),
            Doc::Csv(a) => go!(a),
            Doc::Xml(a) => go!(a),
            Doc::Html(a) => go!(a),
            Doc::Text(a) => go!(a),
            #[cfg(feature = "sqlite")]
            Doc::Sqlite(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Fs(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::FsDeep(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Git(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Archive(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::ArchiveRaw(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Xlsx(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Syntax(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Code(a) => go!(a),
            Doc::Mount(a) => go!(a),
            Doc::Boxed(a, _) => go!(a),
        }
    }

    /// Render a node result as its source-appropriate locator.
    /// The adapter family this document is mounted through — the
    /// `adapter` a trace payload's meta names.
    pub fn family(&self) -> &'static str {
        match self {
            Doc::Json(_) => "json",
            Doc::Csv(_) => "csv",
            Doc::Xml(_) => "xml",
            Doc::Html(_) => "html",
            Doc::Text(_) => "text",
            #[cfg(feature = "sqlite")]
            Doc::Sqlite(_) => "sqlite",
            #[cfg(feature = "native")]
            Doc::Fs(_) | Doc::FsDeep(_) => "fs",
            #[cfg(feature = "native")]
            Doc::Git(_) => "git",
            #[cfg(feature = "native")]
            Doc::Archive(_) | Doc::ArchiveRaw(_) => "archive",
            #[cfg(feature = "native")]
            Doc::Xlsx(_) => "xlsx",
            #[cfg(feature = "native")]
            Doc::Syntax(_) => "tree-sitter",
            #[cfg(feature = "native")]
            Doc::Code(_) => "code",
            Doc::Mount(_) => "mount",
            Doc::Boxed(..) => "boxed",
        }
    }

    /// The `quarb/trace` payload for `committed` (plus an uncommitted
    /// `staged` suffix) over this document — every snapshot, thread,
    /// ribbon, and lookahead verdict, windowed per `opts`. Locators
    /// are this document's [`render`](Self::render).
    pub fn trace(
        &self,
        committed: &str,
        staged: Option<&str>,
        opts: &quarb::trace::Options,
    ) -> quarb::Result<quarb_trace::Payload> {
        let base = quarb_model::Borrowed(self.base_dyn());
        quarb::trace::trace(committed, staged, &base, &|n| self.render(n), opts)
    }

    pub fn render(&self, node: NodeId) -> String {
        match self {
            Doc::Json(a) => a.pointer(node),
            Doc::Csv(a) => a.locator(node),
            Doc::Xml(a) => a.locator(node),
            Doc::Html(a) => a.locator(node),
            Doc::Text(a) => a.locator(node),
            #[cfg(feature = "sqlite")]
            Doc::Sqlite(a) => a.locator(node),
            #[cfg(feature = "native")]
            Doc::Fs(a) => a.path(node).display().to_string(),
            #[cfg(feature = "native")]
            Doc::FsDeep(a) => a.locator(node, |o| a.outer().path(o).display().to_string()),
            #[cfg(feature = "native")]
            Doc::Git(a) => a.locator(node),
            #[cfg(feature = "native")]
            Doc::Archive(a) => a.locator(node, |o| a.outer().locator(o)),
            #[cfg(feature = "native")]
            Doc::ArchiveRaw(a) => a.locator(node),
            #[cfg(feature = "native")]
            Doc::Xlsx(a) => a.locator(node),
            #[cfg(feature = "native")]
            Doc::Syntax(a) => a.locator(node),
            #[cfg(feature = "native")]
            Doc::Code(a) => a.locator(node),
            Doc::Mount(a) => generic_locator(a, node),
            Doc::Boxed(_, render) => render(node),
        }
    }

    /// Open a SQLite database from its file bytes — a `.db` that
    /// never touched a filesystem (the browser's uploaded files).
    #[cfg(feature = "sqlite")]
    pub fn sqlite_bytes(bytes: &[u8]) -> Result<Doc> {
        Ok(Doc::Sqlite(
            quarb_sqlite::SqliteAdapter::from_bytes(bytes)
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("opening SQLite bytes")?,
        ))
    }

    /// The refusal twin: the API is present either way, so a
    /// consumer compiles against both builds and the absence
    /// reports itself instead of failing to link.
    #[cfg(not(feature = "sqlite"))]
    pub fn sqlite_bytes(_bytes: &[u8]) -> Result<Doc> {
        anyhow::bail!("this quarb-session was built without the `sqlite` feature")
    }

    /// Mount already-built documents as named children of one root —
    /// the general wasm-safe mount, for callers that assembled their
    /// `Doc`s from text or bytes rather than paths.
    pub fn mount_docs(parts: Vec<(String, Doc)>) -> Result<Doc> {
        let mut mounts: Vec<quarb_mount::Mount> = Vec::new();
        for (name, doc) in parts {
            if mounts.iter().any(|m| m.name == name) {
                bail!("two sources mount as '{name}'; give each a distinct name");
            }
            mounts.push(quarb_mount::Mount {
                name,
                // Assembled from text/bytes: no real-world address to
                // record — the mount name stands in for :::source.
                target: None,
                adapter: doc.into_boxed()?,
            });
        }
        Ok(Doc::Mount(
            quarb_mount::MountAdapter::try_new(mounts)
                .map_err(|e| anyhow::anyhow!("mount: {e}"))?,
        ))
    }

    /// Declare a source's own URL on its adapter — today the html
    /// DOM reading, whose relative hrefs join against it for the
    /// acquisition arrow (`::href-->` across documents). Other
    /// formats ignore it (their cross-references carry no URL yet).
    pub fn attach_url(&mut self, url: &str) {
        match self {
            Doc::Html(a) => a.set_document_url(url),
            Doc::Text(a) => a.set_document_url(url),
            _ => {}
        }
    }

    /// The reference targets for this doc: external-reference
    /// identifier (its declared URL) → the root the arrow lands
    /// on, for every mounted source that declared one. `urls` is
    /// `(mount name, url)`; on a single-source (path-bare) doc the
    /// one root answers.
    pub fn url_roots(
        &self,
        urls: &[(String, String)],
    ) -> std::collections::HashMap<String, quarb::NodeId> {
        use quarb::AstAdapter;
        let a = self.base_dyn();
        let root = a.root();
        match self {
            Doc::Mount(_) => urls
                .iter()
                .filter_map(|(name, url)| {
                    a.children_named(root, name)
                        .first()
                        .map(|&n| (url.clone(), n))
                })
                .collect(),
            _ => urls
                .iter()
                .take(1)
                .map(|(_, url)| (url.clone(), root))
                .collect(),
        }
    }

    /// Mount several already-parsed text documents as named children
    /// of one root — [`Doc::mount_docs`] over [`Doc::parse`], for
    /// callers that hold text (the browser playground's paste
    /// boxes). `parts` is `(name, format, text)`.
    pub fn mount_texts(parts: &[(String, String, String)]) -> Result<Doc> {
        let mut docs: Vec<(String, Doc)> = Vec::new();
        for (name, format, text) in parts {
            let doc = Doc::parse(text, format)
                .with_context(|| format!("parsing '{name}' as {format}"))?;
            docs.push((name.clone(), doc));
        }
        Doc::mount_docs(docs)
    }

    /// Box this source as a shared adapter — a mount child.
    fn into_boxed(self) -> Result<Box<dyn quarb::AstAdapter>> {
        use quarb_mount::Shared;
        Ok(match self {
            Doc::Json(a) => Box::new(Shared(Rc::new(a))),
            Doc::Csv(a) => Box::new(Shared(Rc::new(a))),
            Doc::Xml(a) => Box::new(Shared(Rc::new(a))),
            Doc::Html(a) => Box::new(Shared(Rc::new(a))),
            Doc::Text(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "sqlite")]
            Doc::Sqlite(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::Fs(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::FsDeep(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::Git(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::Archive(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::ArchiveRaw(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::Xlsx(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::Syntax(a) => Box::new(Shared(Rc::new(a))),
            #[cfg(feature = "native")]
            Doc::Code(a) => Box::new(Shared(Rc::new(a))),
            Doc::Mount(_) => bail!("cannot nest a mount inside a mount"),
            Doc::Boxed(a, _) => a.0,
        })
    }
}

// ---------------------------------------------------------------------
// Native-only: filesystem/db/git dispatch and multi-source mounts.
// ---------------------------------------------------------------------

#[cfg(feature = "native")]
impl Doc {
    /// Open one path as a local source. Directories are filesystem
    /// trees (`--graft` grafts parseable leaves); `git:PATH` opens a
    /// repository; binary kinds (SQLite, spreadsheets, archives) and
    /// source files dispatch by extension/magic; everything else is a
    /// text document parsed by extension or content sniff.
    pub fn open(path: &Path, opts: &Options) -> Result<Doc> {
        // `?references=STANDARD` applies to the text model once it
        // is built, whichever door built it; the pair comes off
        // the target before the door reads its own options.
        let (stripped, references) = take_references_option(path)?;
        let doc = Self::open_inner(stripped.as_path(), opts)?;
        match (doc, references) {
            (Doc::Text(mut model), Some(standard)) => {
                model
                    .lower_references(&standard)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                Ok(Doc::Text(model))
            }
            (doc, Some(_)) => {
                let _ = doc;
                bail!(
                    "{}: references= applies to a text-level reading (text:, koine:)",
                    path.display()
                )
            }
            (doc, None) => Ok(doc),
        }
    }

    fn open_inner(path: &Path, opts: &Options) -> Result<Doc> {
        let (path, explicit, csv) = take_source_options(path)?;
        let path = path.as_path();
        let explicit = explicit.as_deref();
        if path.is_dir() {
            let fsopts = quarb_fs::FsOptions {
                hidden: opts.hidden,
                respect_ignore: opts.respect_ignore,
            };
            let fs = quarb_fs::FsAdapter::with_options(path, fsopts)
                .with_context(|| format!("opening directory {}", path.display()))?;
            return Ok(if opts.graft {
                Doc::FsDeep(
                    quarb_compose::ComposeAdapter::with_source_paths(fs, |fs, n| Some(fs.path(n)))
                        .with_decoding(
                            quarb_compose::source_decoding(explicit)
                                .map_err(|e| anyhow::anyhow!("{e}"))?,
                        )
                        .with_csv_options(csv),
                )
            } else {
                Doc::Fs(fs)
            });
        }

        let s = path.to_string_lossy();
        if let Some(repo) = s.strip_prefix("git:") {
            let a =
                quarb_git::GitAdapter::open(Path::new(repo)).context("opening git repository")?;
            return Ok(Doc::Git(a));
        }
        // The lines reading (`lines:`): every line a node, as on
        // the command line.
        if let Some(rest) = s.strip_prefix("lines:")
            && !rest.is_empty()
        {
            let (target, opt) = take_encoding_option(Path::new(rest))?;
            let text = read_source(&target, opt.as_deref().or(explicit))?;
            let a = std::rc::Rc::new(quarb_lines::LinesAdapter::parse(&text));
            let r = a.clone();
            return Ok(Doc::Boxed(
                Dyn(Box::new(quarb_mount::Shared(a))),
                Box::new(move |n| r.locator(n)),
            ));
        }
        // The web level (`web:`): a directory of pages or an
        // archive of them as one site, in memory. An index-backed
        // store (site.db, postgres://) is the command line's own
        // run loop and is refused here naming it.
        if let Some(rest) = s.strip_prefix("web:")
            && !rest.is_empty()
        {
            let (path_part, base) = match rest.split_once('?') {
                Some((p, q)) => {
                    let mut base = String::new();
                    for pair in q.split('&') {
                        match pair.split_once('=') {
                            Some(("base", v)) => base = v.to_string(),
                            _ => {
                                return Err(anyhow::anyhow!(
                                    "unknown web option {pair:?} — supported: base="
                                ));
                            }
                        }
                    }
                    (p, base)
                }
                None => (rest, String::new()),
            };
            let decoding =
                quarb_compose::source_decoding(explicit).map_err(|e| anyhow::anyhow!("{e}"))?;
            let target = Path::new(path_part);
            let site = if target.is_dir() {
                quarb_web::fs::open_dir_with_decoding(target, &base, decoding)
                    .with_context(|| format!("reading {} as a site", target.display()))?
            } else if target.is_file() && !path_part.starts_with("postgres") {
                quarb_web::archive::open_path_with_decoding(target, &base, decoding)
                    .with_context(|| format!("reading {} as a site", target.display()))?
            } else {
                return Err(anyhow::anyhow!(
                    "web: in a session takes a directory of pages or an archive of them; a site store ({path_part}) is read on the command line (qua … web:{path_part})"
                ));
            };
            let a = std::rc::Rc::new(site);
            let r = a.clone();
            return Ok(Doc::Boxed(
                Dyn(Box::new(quarb_mount::Shared(a))),
                Box::new(move |n| r.locator(n)),
            ));
        }
        // A `koine:` prefix takes the koine route to the same
        // reader's model: the native atrep formats today, foreign
        // formats once atrep grows import homs. Refusals name the
        // native spelling.
        if let Some(rest) = s.strip_prefix("koine:")
            && !rest.is_empty()
        {
            // The house ?param syntax: ?format= forces a format.
            let (path_part, format) = match rest.split_once('?') {
                Some((p, q)) => {
                    let mut fmt = None;
                    for pair in q.split('&') {
                        match pair.split_once('=') {
                            Some(("format", v)) => fmt = Some(v.to_string()),
                            _ => {
                                return Err(anyhow::anyhow!(
                                    "unknown koine option {pair:?} — supported: format="
                                ));
                            }
                        }
                    }
                    (p, fmt)
                }
                None => (rest, None),
            };
            let target = Path::new(path_part);
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            let read = || -> Result<String> {
                let text = read_source(target, explicit)?;
                Ok(text
                    .strip_prefix('\u{feff}')
                    .map(str::to_owned)
                    .unwrap_or(text))
            };
            if let Some(fmt) = format {
                let imported = match fmt.as_str() {
                    "atd" | "atk" => {
                        let model = quarb_text_koine::parse_file(target).with_context(|| {
                            format!("reading {} as an atrep document", target.display())
                        })?;
                        return Ok(Doc::Text(model));
                    }
                    "md" | "markdown" => quarb_text_koine::parse_markdown(&read()?),
                    "html" => quarb_text_koine::parse_html(&read()?),
                    "rst" => quarb_text_koine::parse_rst(&read()?),
                    "org" => quarb_text_koine::parse_org(&read()?),
                    "dj" | "djot" => quarb_text_koine::parse_djot(&read()?),
                    "tei" | "docbook" | "jats" | "usx" | "osis" | "fb2" | "rnc" | "opencorpora"
                    | "proiel" => quarb_text_koine::parse_xml_as(&read()?, &fmt),
                    "docx" => quarb_text_koine::parse_docx(&std::fs::read(target)?),
                    "dsl" => quarb_text_koine::parse_dsl_file(target),
                    other => {
                        return Err(anyhow::anyhow!(
                            "unknown koine format {other:?} — known: md, html, rst, org, djot, tei, docbook, jats, usx, osis, fb2, docx, dsl, atd"
                        ));
                    }
                };
                return imported.map(Doc::Text).with_context(|| {
                    format!("importing {} through atrep as {fmt}", target.display())
                });
            }
            if quarb_text_koine::is_dsl_path(target) {
                let model = quarb_text_koine::parse_dsl_file(target).with_context(|| {
                    format!("importing {} as a Lingvo DSL dictionary", target.display())
                })?;
                return Ok(Doc::Text(model));
            }
            let imported = match ext.as_deref() {
                Some("atd" | "atk") => {
                    let model = quarb_text_koine::parse_file(target).with_context(|| {
                        format!("reading {} as an atrep document", target.display())
                    })?;
                    return Ok(Doc::Text(model));
                }
                Some("docx") => quarb_text_koine::parse_docx(&std::fs::read(target)?),
                Some("usfm" | "sfm") => quarb_text_koine::parse_xml_as(&read()?, "usfm"),
                Some("fb2") => quarb_text_koine::parse_xml_as(&read()?, "fb2"),
                Some("bib") => quarb_text_koine::parse_bibtex(&read()?),
                Some("md" | "markdown") => quarb_text_koine::parse_markdown(&read()?),
                Some("html" | "htm") => quarb_text_koine::parse_html(&read()?),
                Some("rst") => quarb_text_koine::parse_rst(&read()?),
                Some("org") => quarb_text_koine::parse_org(&read()?),
                Some("dj" | "djot") => quarb_text_koine::parse_djot(&read()?),
                Some("xml") => {
                    let text = read()?;
                    match quarb_text_koine::detect_xml_kind(&text) {
                        Some(kind) => quarb_text_koine::parse_xml_as(&text, kind),
                        None => {
                            return Err(anyhow::anyhow!(
                                "{} declares no XML identity this route knows — force one with koine:{}?format=tei|docbook|jats|usx|osis|rnc|opencorpora|proiel",
                                target.display(),
                                target.display()
                            ));
                        }
                    }
                }
                Some("pdf") => {
                    return Err(anyhow::anyhow!(
                        "atrep cannot import a PDF — the print reading is text:{}",
                        target.display()
                    ));
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "no atrep import for this format yet — the native reading is text:{}",
                        target.display()
                    ));
                }
            };
            return imported
                .map(Doc::Text)
                .with_context(|| format!("importing {} through atrep", target.display()));
        }
        // The literary reading: the corpus reading with the inline
        // simmeres kept — litogramma, TEI, USX and OSIS through
        // atrep's importers; the unseen metadata as properties.
        if let Some(rest) = s.strip_prefix("lit:")
            && !rest.is_empty()
        {
            let (rest, conllu, annotate, desm, format, cast, places, scheme, modernize, notes) =
                match rest.split_once('?') {
                    Some((p, q)) => {
                        let (
                            mut conllu,
                            mut annotate,
                            mut desm,
                            mut format,
                            mut cast,
                            mut places,
                            mut scheme,
                            mut modernize,
                            mut notes,
                        ) = (None, None, Vec::new(), None, None, None, None, None, None);
                        for pair in q.split('&') {
                            match pair.split_once('=') {
                                Some(("conllu", v)) => conllu = Some(v.to_string()),
                                Some(("annotate", v)) => annotate = Some(v.to_string()),
                                Some(("desm", v)) => desm.push(v.to_string()),
                                Some(("cast", v)) => cast = Some(v.to_string()),
                                Some(("places", v)) => places = Some(v.to_string()),
                                Some(("scheme", v)) => scheme = Some(v.to_string()),
                                Some(("modernize", v)) => modernize = Some(v.to_string()),
                                Some(("notes", v)) => notes = Some(v.to_string()),
                                Some(("format", v)) => format = Some(v.to_string()),
                                // `encoding=` is read off the whole target
                                // before the door (`explicit`).
                                Some(("encoding", _)) => {}
                                _ => bail!(
                                    "unknown lit option {pair:?} — supported: conllu=, annotate=, desm=, cast=, places=, format=, scheme=, modernize=, notes=, encoding="
                                ),
                            }
                        }
                        (
                            p, conllu, annotate, desm, format, cast, places, scheme, modernize,
                            notes,
                        )
                    }
                    None => (
                        rest,
                        None,
                        None,
                        Vec::new(),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    ),
                };
            let target = Path::new(rest);
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            let read = || -> Result<String> {
                let text = read_source(target, explicit)?;
                Ok(text
                    .strip_prefix('\u{feff}')
                    .map(str::to_owned)
                    .unwrap_or(text))
            };
            let kind = match format {
                Some(k) => k,
                None => match ext.as_deref() {
                    Some("atd" | "atk") => "atd".to_string(),
                    Some("usx") => "usx".to_string(),
                    Some("usfm" | "sfm") => "usfm".to_string(),
                    Some("fb2") => "fb2".to_string(),
                    _ => match quarb_text_koine::detect_xml_kind(&read()?) {
                        Some(k) => k.to_string(),
                        None => bail!(
                            "lit: reads TEI, USX, OSIS, USFM, FictionBook, RNC, OpenCorpora, PROIEL and litogramma — {rest} declares none of them (koine:{rest} reads the other XML vocabularies, corpus:{rest} plain prose)"
                        ),
                    },
                },
            };
            let mut model = match kind.as_str() {
                "atd" | "atk" => quarb_text_koine::parse_lit_file_with(target, scheme.as_deref())
                    .with_context(|| {
                    format!("reading {} as an atrep document", target.display())
                })?,
                "tei" | "usx" | "osis" | "fb2" | "usfm" | "rnc" | "opencorpora" | "proiel" => {
                    quarb_text_koine::parse_lit_xml_as_with(&read()?, &kind, scheme.as_deref())
                        .with_context(|| {
                            format!("reading {} as {}", target.display(), kind.to_uppercase())
                        })?
                }
                other => bail!(
                    "lit: reads TEI, USX, OSIS, USFM, FictionBook, RNC, OpenCorpora, PROIEL and litogramma — not {other} (koine:{rest} reads the other XML vocabularies)"
                ),
            };
            model.set_document_path(rest);
            return finish_corpus(
                model, conllu, annotate, &desm, cast, places, modernize, notes, opts, "lit",
            );
        }
        // The corpus reading: the text-level reading
        // with every prose block's tokens as children, the
        // tokenizer decided once, at mount — where the resident
        // session keeps it. An XML source takes the koine route
        // (TEI and the other dialects); everything else the text:
        // route; a PDF is refused, its reading being line geometry.
        if let Some(rest) = s.strip_prefix("corpus:")
            && !rest.is_empty()
        {
            // `?conllu=FILE` / `?annotate=CMD`, as in qua.
            let (rest, conllu, annotate, desm, format, cast, places, modernize, notes) = match rest
                .split_once('?')
            {
                Some((p, q)) => {
                    let (
                        mut conllu,
                        mut annotate,
                        mut desm,
                        mut format,
                        mut cast,
                        mut places,
                        mut modernize,
                        mut notes,
                    ) = (None, None, Vec::new(), None, None, None, None, None);
                    for pair in q.split('&') {
                        match pair.split_once('=') {
                            Some(("conllu", v)) => conllu = Some(v.to_string()),
                            Some(("annotate", v)) => annotate = Some(v.to_string()),
                            Some(("desm", v)) => desm.push(v.to_string()),
                            Some(("cast", v)) => cast = Some(v.to_string()),
                            Some(("places", v)) => places = Some(v.to_string()),
                            Some(("modernize", v)) => modernize = Some(v.to_string()),
                            Some(("notes", v)) => notes = Some(v.to_string()),
                            Some(("format", "conllu")) => format = Some("conllu"),
                            Some(("format", v)) => bail!(
                                "corpus: format={v} is not a treebank format — format= takes conllu"
                            ),
                            _ => bail!(
                                "unknown corpus option {pair:?} — supported: conllu=, annotate=, desm=, cast=, places=, modernize=, notes=, format="
                            ),
                        }
                    }
                    (
                        p, conllu, annotate, desm, format, cast, places, modernize, notes,
                    )
                }
                None => (rest, None, None, Vec::new(), None, None, None, None, None),
            };
            let target = Path::new(rest);
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if ext.as_deref() == Some("pdf") {
                bail!(
                    "corpus: needs a prose reading, and a PDF's is line geometry — text:{rest} reads its lines"
                );
            }
            // A treebank is its own annotation: by
            // extension, by `?format=conllu`, or sniffed on an
            // extensionless file; a directory of treebank files is
            // one corpus, a section per file.
            let own_annotation = || {
                if conllu.is_some() || annotate.is_some() {
                    bail!(
                        "corpus: {rest} is its own annotation — conllu= and annotate= apply to prose"
                    );
                }
                Ok(())
            };
            if target.is_dir() {
                own_annotation()?;
                let files = quarb_text::read_conllu_dir(target).map_err(|e| {
                    anyhow::anyhow!("corpus: over a directory reads its treebank — {e}")
                })?;
                let refs: Vec<(&str, &str)> = files
                    .iter()
                    .map(|(n, t)| (n.as_str(), t.as_str()))
                    .collect();
                let mut model = quarb_text::TextModel::parse_conllu_corpus_set(&refs)
                    .map_err(|e| anyhow::anyhow!("reading {rest} as CoNLL-U: {e}"))?;
                model.set_document_path(rest);
                return treebank_options(model, &cast, &places, &modernize);
            }
            let treebank = matches!(ext.as_deref(), Some("conllu" | "conllup"))
                || format == Some("conllu")
                || (ext.is_none()
                    && read_source(target, explicit)
                        .is_ok_and(|t| quarb_text::looks_like_conllu(&t)));
            if treebank {
                own_annotation()?;
                let text = read_source(target, explicit)?;
                let text = text
                    .strip_prefix('\u{feff}')
                    .map(str::to_owned)
                    .unwrap_or(text);
                let Doc::Text(mut model) = Doc::parse(&text, "corpus-conllu")? else {
                    bail!("corpus: needs a prose reading of {rest}")
                };
                model.set_document_path(rest);
                return treebank_options(model, &cast, &places, &modernize);
            }
            let route = if matches!(ext.as_deref(), Some("xml" | "tei")) {
                format!("koine:{rest}")
            } else {
                format!("text:{rest}")
            };
            let Doc::Text(model) = Doc::open(Path::new(&route), opts)? else {
                bail!("corpus: needs a prose reading of {rest}")
            };
            return finish_corpus(
                model, conllu, annotate, &desm, cast, places, modernize, notes, opts, "corpus",
            );
        }
        // `json2:FILE` — the property reading of a JSON-model
        // document (JSON, JSONL, YAML, TOML), matching qua.
        if let Some(rest) = s.strip_prefix("json2:")
            && !rest.is_empty()
        {
            use quarb_json::Reading;
            let target = Path::new(rest);
            let text = read_source(target, explicit)?;
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            let a = match ext.as_str() {
                "yaml" | "yml" => {
                    quarb_yaml::parse_as(&text, Reading::Property).context("parsing YAML")?
                }
                "toml" => quarb_toml::parse_as(&text, Reading::Property).context("parsing TOML")?,
                "jsonl" | "ndjson" => {
                    quarb_json::JsonAdapter::parse_lines_as(&text, Reading::Property)
                        .context("parsing JSONL")?
                }
                _ => quarb_json::JsonAdapter::parse_as(&text, Reading::Property)
                    .context("parsing JSON")?,
            };
            return Ok(Doc::Json(a));
        }
        // A `text:` prefix forces the text-level reading, matching
        // qua's dispatch: producer by extension, `<` sniffing
        // markup, plain paragraphs as the fallback.
        if let Some(rest) = s.strip_prefix("text:")
            && !rest.is_empty()
        {
            let target = Path::new(rest);
            // A directory of treebank files reads as one document,
            // a section per file (`corpus:` adds the tokens).
            if target.is_dir() {
                let files = quarb_text::read_conllu_dir(target).map_err(|e| {
                    anyhow::anyhow!("text: over a directory reads its treebank — {e}")
                })?;
                let refs: Vec<(&str, &str)> = files
                    .iter()
                    .map(|(n, t)| (n.as_str(), t.as_str()))
                    .collect();
                let mut model = quarb_text::TextModel::parse_conllu_text_set(&refs)
                    .map_err(|e| anyhow::anyhow!("reading {rest} as CoNLL-U: {e}"))?;
                model.set_document_path(rest);
                return Ok(Doc::Text(model));
            }
            // The binary producers dispatch before the text read:
            // a .docx or .epub is a zip, a .pdf its own format.
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            // Native atrep files converge on the koine route.
            if let Some("usfm" | "sfm") = ext.as_deref() {
                let text = read_source(target, explicit)?;
                let text = text
                    .strip_prefix('\u{feff}')
                    .map(str::to_owned)
                    .unwrap_or(text);
                let model = quarb_text_koine::parse_xml_as(&text, "usfm")
                    .with_context(|| format!("reading {} as USFM", target.display()))?;
                return Ok(Doc::Text(model));
            }
            if let Some("atd" | "atk") = ext.as_deref() {
                let model = quarb_text_koine::parse_file(target).with_context(|| {
                    format!("reading {} as an atrep document", target.display())
                })?;
                return Ok(Doc::Text(model));
            }
            if ext.as_deref() == Some("pdf") {
                let bytes = std::fs::read(target)
                    .with_context(|| format!("reading {}", target.display()))?;
                let model = quarb_text_pdf::parse(&bytes)
                    .with_context(|| format!("reading {} as PDF", target.display()))?;
                let a = std::rc::Rc::new(model);
                let r = a.clone();
                return Ok(Doc::Boxed(
                    Dyn(Box::new(quarb_mount::Shared(a))),
                    Box::new(move |n| r.locator(n)),
                ));
            }
            if matches!(ext.as_deref(), Some("djvu" | "djv")) {
                let model = quarb_text_djvu::parse_file(target)
                    .with_context(|| format!("reading {} as DjVu", target.display()))?;
                let a = std::rc::Rc::new(model);
                let r = a.clone();
                return Ok(Doc::Boxed(
                    Dyn(Box::new(quarb_mount::Shared(a))),
                    Box::new(move |n| r.locator(n)),
                ));
            }
            // A `.doc` is Word 97 only by its OLE2 magic; a plain
            // text file so named reads as plain text below.
            // A Lingvo DSL dictionary reads through the koine route
            // under text: and corpus: alike.
            if quarb_text_koine::is_dsl_path(target) {
                let model = quarb_text_koine::parse_dsl_file(target).with_context(|| {
                    format!("reading {} as a Lingvo DSL dictionary", target.display())
                })?;
                return Ok(Doc::Text(model));
            }
            let word97 = ext.as_deref() == Some("doc")
                && std::fs::read(target).is_ok_and(|b| quarb_text_doc::is_ole2(&b));
            if let Some(ext @ ("docx" | "epub" | "odt" | "fodt" | "sxw" | "doc")) =
                ext.as_deref().filter(|e| *e != "doc" || word97)
            {
                let bytes = std::fs::read(target)
                    .with_context(|| format!("reading {}", target.display()))?;
                let decoding =
                    quarb_compose::source_decoding(explicit).map_err(|e| anyhow::anyhow!("{e}"))?;
                let model = match ext {
                    "doc" => quarb_text_doc::parse(&bytes)
                        .with_context(|| format!("reading {} as Word 97", target.display()))?,
                    "docx" => {
                        let mut m = quarb_text_docx::parse_with_decoding(&bytes, &decoding)
                            .with_context(|| format!("reading {} as Word", target.display()))?;
                        quarb_text_koine::attach_bib_aliases(&mut m);
                        m
                    }
                    "odt" | "fodt" | "sxw" => quarb_text_odt::parse_with_decoding(
                        &bytes, &decoding,
                    )
                    .with_context(|| format!("reading {} as OpenDocument", target.display()))?,
                    _ => quarb_text_epub::parse_with_decoding(&bytes, &decoding)
                        .with_context(|| format!("reading {} as EPUB", target.display()))?,
                };
                return Ok(Doc::Text(model));
            }
            let text = read_source(target, explicit)?;
            let text = text
                .strip_prefix('\u{feff}')
                .map(str::to_owned)
                .unwrap_or(text);
            let format = match target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref()
            {
                Some("html" | "htm") => "text-html",
                Some("md" | "markdown") => "text-markdown",
                Some("tex" | "latex") => "tex",
                Some("txt") => "text",
                Some("rtf") => "text-rtf",
                Some("conllu" | "conllup") => "conllu",
                #[cfg(feature = "native")]
                Some("bib") => "text-bibtex",
                // FictionBook by its extension or its namespace.
                #[cfg(feature = "native")]
                Some("fb2") => "text-fb2",
                #[cfg(feature = "native")]
                _ if text.trim_start().starts_with('<')
                    && quarb_text_koine::detect_xml_kind(&text) == Some("fb2") =>
                {
                    "text-fb2"
                }
                _ if text.trim_start().starts_with('<') => "text-html",
                None if quarb_text::looks_like_conllu(&text) => "conllu",
                _ => "text",
            };
            return Doc::parse(&text, format);
        }
        // A `code:` prefix forces the code-level reading,
        // matching qua's dispatch; a directory mounts the
        // composed, grafted view with source leaves grafted
        // at the code level.
        if let Some(rest) = s.strip_prefix("code:")
            && !rest.is_empty()
        {
            if opts.no_graft {
                bail!(
                    "no_graft refuses the code: prefix: the prefix's whole \
                     meaning is the grafted code-level view"
                );
            }
            let target = Path::new(rest);
            if target.is_dir() {
                let fsopts = quarb_fs::FsOptions {
                    hidden: opts.hidden,
                    respect_ignore: opts.respect_ignore,
                };
                let fs = quarb_fs::FsAdapter::with_options(target, fsopts)
                    .with_context(|| format!("opening directory {}", target.display()))?;
                return Ok(Doc::FsDeep(
                    quarb_compose::ComposeAdapter::with_source_paths(fs, |fs, n| Some(fs.path(n)))
                        .with_source_graft(quarb_compose::SourceGraft::Code),
                ));
            }
            let a = quarb_code::CodeModel::open(target)
                .with_context(|| format!("parsing {} at the code level", target.display()))?;
            return Ok(Doc::Code(a));
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        if let Some(e) = &ext
            && quarb_tree_sitter::supported(e)
        {
            let a =
                quarb_tree_sitter::TreeSitterAdapter::open(path).context("parsing source file")?;
            return Ok(Doc::Syntax(a));
        }
        if matches!(ext.as_deref(), Some("xlsx" | "xls" | "ods")) {
            let a = quarb_xlsx::XlsxAdapter::open(path).context("opening workbook")?;
            return Ok(Doc::Xlsx(a));
        }
        if is_sqlite(path) {
            // Refuse, never fall through: a .db is binary, and
            // letting it reach the text sniffers would trade a
            // clear absence for a confusing parse error.
            #[cfg(not(feature = "sqlite"))]
            anyhow::bail!(
                "{}: this quarb-session was built without the `sqlite` feature",
                path.display()
            );
            #[cfg(feature = "sqlite")]
            {
                let a = quarb_sqlite::SqliteAdapter::open_with_refs(path, &opts.refs)
                    .context("opening SQLite database")?;
                return Ok(Doc::Sqlite(a));
            }
        }
        if is_archive(path) {
            let a = quarb_archive::ArchiveAdapter::open(path).context("opening archive")?;
            return Ok(if opts.no_graft {
                Doc::ArchiveRaw(a)
            } else {
                Doc::Archive(quarb_compose::ComposeAdapter::new(a))
            });
        }

        // Text documents.
        let text = read_source(path, explicit)?;
        let text = text
            .strip_prefix('\u{feff}')
            .map(str::to_owned)
            .unwrap_or(text);
        match ext.as_deref() {
            // The kaiv family by extension, resolving imports beside
            // the file (the command line's door).
            Some("kaiv") => quarb_kaiv::KaivAdapter::parse_kaiv_at(&text, path.parent())
                .map(Doc::boxed_kaiv)
                .map_err(|e| anyhow::anyhow!("parsing kaiv: {e}")),
            Some("daiv") => quarb_kaiv::KaivAdapter::parse_daiv_at(&text, path.parent())
                .map(Doc::boxed_kaiv)
                .map_err(|e| anyhow::anyhow!("parsing daiv: {e}")),
            Some("raiv") => quarb_kaiv::KaivAdapter::parse_raiv_at(&text, path.parent())
                .map(Doc::boxed_kaiv)
                .map_err(|e| anyhow::anyhow!("parsing raiv: {e}")),
            // A Quarb query file reflects as a query arbor.
            Some("quarb") => {
                let a = std::rc::Rc::new(
                    quarb::reflect::QueryArbor::parse(&text).context("parsing Quarb query")?,
                );
                let r = a.clone();
                Ok(Doc::Boxed(
                    Dyn(Box::new(quarb_mount::Shared(a))),
                    Box::new(move |n| r.locator(n)),
                ))
            }
            Some("csv") => quarb_csv::CsvAdapter::parse_declared(&text, b',', csv)
                .map(Doc::Csv)
                .context("parsing CSV"),
            Some("tsv") => quarb_csv::CsvAdapter::parse_declared(&text, b'\t', csv)
                .map(Doc::Csv)
                .context("parsing TSV"),
            Some("yaml" | "yml") => Doc::parse(&text, "yaml"),
            Some("toml") => Doc::parse(&text, "toml"),
            Some("md" | "markdown") => Doc::parse(&text, "markdown"),
            Some("txt") => Doc::parse(&text, "text"),
            Some("conllu" | "conllup") => Doc::parse(&text, "conllu"),
            Some("jsonl" | "ndjson") => Doc::parse(&text, "jsonl"),
            _ => {
                if is_xml(path, &text) {
                    Doc::parse(&text, "xml")
                } else if is_html(path, &text) {
                    Doc::parse(&text, "html")
                } else {
                    Doc::parse(&text, "json")
                }
            }
        }
    }

    /// Open several sources as named children of one root (file stem =
    /// mount name), so a single query — including a `<=>` join — spans
    /// them all.
    pub fn mount(paths: &[std::path::PathBuf], opts: &Options) -> Result<Doc> {
        let specs: Vec<crate::MountSpec> = paths
            .iter()
            .map(|p| crate::MountSpec {
                name: None,
                path: p.clone(),
            })
            .collect();
        Doc::mount_specs(&specs, opts)
    }

    /// [`Doc::mount`] with optional explicit mount names
    /// (`NAME=TARGET`); an unnamed spec mounts under its file stem.
    pub fn mount_specs(specs: &[crate::MountSpec], opts: &Options) -> Result<Doc> {
        let mut mounts: Vec<quarb_mount::Mount> = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            let name = spec.name.clone().unwrap_or_else(|| {
                spec.path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("doc{i}"))
            });
            if mounts.iter().any(|m| m.name == name) {
                bail!(
                    "input '{}' mounts as '{name}', colliding with an earlier input of the \
                     same name; give each a distinct basename (or a NAME=TARGET alias)",
                    spec.path.display()
                );
            }
            let adapter = Doc::open(&spec.path, opts)?.into_boxed()?;
            mounts.push(quarb_mount::Mount {
                name,
                target: Some(spec.path.display().to_string()),
                adapter,
            });
        }
        Ok(Doc::Mount(
            quarb_mount::MountAdapter::try_new(mounts)
                .map_err(|e| anyhow::anyhow!("mount: {e}"))?,
        ))
    }
}

/// A name-path locator built from the adapter trait alone
/// (`parent`/`name`) — used for a mount, whose per-source render
/// functions we do not keep.
fn generic_locator<A: quarb::AstAdapter>(a: &A, node: NodeId) -> String {
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

/// Whether a file is a SQLite database — by extension, or the 16-byte
/// header magic.
#[cfg(feature = "native")]
fn is_sqlite(path: &Path) -> bool {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "db" | "sqlite" | "sqlite3"))
    {
        return true;
    }
    use std::io::Read as _;
    let mut buf = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
        && &buf == b"SQLite format 3\0"
}

/// Whether a file is an archive — by extension, or zip/gzip magic.
#[cfg(feature = "native")]
fn is_archive(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "zip"
                | "tar"
                | "gz"
                | "tgz"
                | "jar"
                | "war"
                | "docx"
                | "pptx"
                | "odt"
                | "odp"
                | "7z"
                | "xz"
                | "txz"
                | "bz2"
                | "tbz2"
                | "zst"
                | "tzst"
        )
    }) {
        return true;
    }
    use std::io::Read as _;
    let mut buf = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
        && (&buf == b"PK" || buf == [0x1f, 0x8b])
}

/// Whether to parse as XML: an `.xml`/`.svg`/`.xhtml` name, or a
/// `<?xml` prolog.
#[cfg(feature = "native")]
fn is_xml(path: &Path, text: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "xml" | "svg" | "xhtml"))
        || text.trim_start().starts_with("<?xml")
}

/// Whether to parse as HTML: an `.html`/`.htm` name, or content that
/// starts with `<`.
#[cfg(feature = "native")]
fn is_html(path: &Path, text: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "html" | "htm"))
        || text.trim_start().starts_with('<')
}

#[cfg(feature = "native")]
/// The corpus reading's second half, shared with `lit:`: the
/// sidecar or command annotation, else the tokenizer with the
/// session's and the mount's sentence bonds.
/// A treebank is its own tokens; the cast table and the spelling
/// table of the mount still apply.
fn treebank_options(
    mut model: quarb_text::TextModel,
    cast: &Option<String>,
    places: &Option<String>,
    modernize: &Option<String>,
) -> Result<Doc> {
    if let Some(t) = modernize {
        model
            .set_orthography(t)
            .map_err(|e| anyhow::anyhow!("corpus: {e}"))?;
    }
    if let Some(f) = cast {
        let rows = quarb_text::CastRow::read_csv(f)
            .map_err(|e| anyhow::anyhow!("corpus: reading the cast table {f}: {e}"))?;
        model.apply_cast(&rows);
    }
    if let Some(f) = places {
        let rows = quarb_text::CastRow::read_csv(f)
            .map_err(|e| anyhow::anyhow!("corpus: reading the places table {f}: {e}"))?;
        model.apply_places(&rows);
    }
    Ok(Doc::Text(model))
}

#[cfg(feature = "native")]
fn finish_corpus(
    mut model: quarb_text::TextModel,
    conllu: Option<String>,
    annotate: Option<String>,
    desm: &[String],
    cast: Option<String>,
    places: Option<String>,
    modernize: Option<String>,
    notes: Option<String>,
    opts: &Options,
    level: &str,
) -> Result<Doc> {
    match notes.as_deref() {
        None => {}
        Some("omit") => model.omit_notes(true),
        Some(v) => {
            bail!("{level}: notes={v} — notes= takes omit (a note is part of the text by default)")
        }
    }
    if let Some(table) = &modernize {
        model
            .set_orthography(table)
            .map_err(|e| anyhow::anyhow!("{level}: {e}"))?;
    }
    let conllu_text = match (conllu, annotate) {
        (Some(_), Some(_)) => bail!("{level}: takes conllu= or annotate=, not both"),
        (Some(file), None) => Some(
            read_source(Path::new(&file), None)
                .with_context(|| format!("reading {file} as CoNLL-U"))?,
        ),
        (None, Some(cmd)) => {
            if !opts.allow_shell {
                bail!("{level}: annotate= runs a command; the shell gate is closed");
            }
            use std::io::Write;
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .with_context(|| format!("running annotator {cmd:?}"))?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(model.corpus_text().as_bytes())
                    .context("feeding the annotator")?;
            }
            let out = child
                .wait_with_output()
                .context("waiting for the annotator")?;
            if !out.status.success() {
                bail!("annotator {cmd:?} failed: {}", out.status);
            }
            Some(String::from_utf8(out.stdout).context("annotator output is not UTF-8")?)
        }
        (None, None) => None,
    };
    match conllu_text {
        Some(text) => model
            .annotate_conllu(&text)
            .map_err(|e| anyhow::anyhow!("{level}: {e}"))?,
        None => {
            let mut bonds = opts.bonds.as_ref().map(|b| (**b).clone());
            for f in desm {
                let more = syndesmos::Syndesmos::load(f)
                    .map_err(|e| anyhow::anyhow!("reading {f}: {e}"))?;
                bonds
                    .get_or_insert_with(syndesmos::Syndesmos::empty)
                    .extend(more);
            }
            model.tokenize_with(bonds.as_ref())
        }
    }
    if let Some(file) = cast {
        let rows = quarb_text::CastRow::read_csv(&file)
            .map_err(|e| anyhow::anyhow!("{level}: reading the cast table {file}: {e}"))?;
        model.apply_cast(&rows);
    }
    if let Some(file) = places {
        let rows = quarb_text::CastRow::read_csv(&file)
            .map_err(|e| anyhow::anyhow!("{level}: reading the places table {file}: {e}"))?;
        model.apply_places(&rows);
    }
    Ok(Doc::Text(model))
}
