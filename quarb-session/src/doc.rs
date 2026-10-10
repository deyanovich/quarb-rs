//! Opening a source into a queryable adapter, and running queries
//! against it.
//!
//! `AstAdapter` is object-safe, but each adapter's *render* method
//! (`pointer` / `locator` / `path`) is an inherent method, not on the
//! trait — so, as the Python bindings do, we hold one of a fixed set
//! of adapter families in an enum and dispatch render (and the
//! query run) by variant.
//!
//! The text-format variants always compile (they are wasm-safe); the
//! native fleet (filesystem, git, SQLite, archives, spreadsheets,
//! source code, mounts) is gated behind the `native` feature, as is
//! the filesystem `open`/`mount` dispatch. The wasm build drives
//! everything through [`Doc::parse`].

use anyhow::{Context, Result, bail};
use quarb::{NodeId, Outcome, QueryResult, Run, Value};

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
    /// The `--refs` document (declared references, `(field,
    /// container)` pairs), read by the relational doors.
    pub refs: Option<std::path::PathBuf>,
    /// The shell gate (qua's --allow-shell): `corpus:…?annotate=CMD`
    /// runs a command only under it.
    pub allow_shell: bool,
    /// The session's sentence bonds (a `.desm` set), for the corpus
    /// reading; `?desm=FILE` on a mount adds to them.
    pub bonds: Option<Rc<syndesmos::Syndesmos>>,
}

#[cfg(feature = "native")]
impl Options {
    /// The same options as quarb-open takes them.
    pub fn open_opts(&self) -> quarb_open::OpenOpts {
        quarb_open::OpenOpts {
            hidden: self.hidden,
            no_ignore: !self.respect_ignore,
            graft: self.graft,
            no_graft: self.no_graft,
            refs: self.refs.clone(),
            allow_shell: self.allow_shell,
        }
    }
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
    Mount(quarb_mount::MountAdapter),
    /// Any adapter behind the object-safe trait, with its locator
    /// renderer — the carrier for scheme targets opened through
    /// qua's dispatch (`gcl:`, `kafka:`, `neo4j://`, …).
    Boxed(Dyn, Box<dyn Fn(NodeId) -> String>, &'static str),
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
            "kaiv",
        )
    }

    /// The code level rides the Boxed door too.
    #[cfg(feature = "native")]
    fn boxed_code(m: quarb_code::CodeModel) -> Doc {
        let a = std::rc::Rc::new(m);
        let r = a.clone();
        Doc::Boxed(
            Dyn(Box::new(quarb_mount::Shared(a))),
            Box::new(move |n| r.locator(n)),
            "code",
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
                .map(Doc::boxed_code)
                .context("parsing Rust at the code level"),
            #[cfg(feature = "native")]
            "code-python" => quarb_code::CodeModel::parse(input, "py")
                .map(Doc::boxed_code)
                .context("parsing Python at the code level"),
            #[cfg(feature = "native")]
            "code-javascript" => quarb_code::CodeModel::parse(input, "js")
                .map(Doc::boxed_code)
                .context("parsing JavaScript at the code level"),
            #[cfg(feature = "native")]
            "code-c" => quarb_code::CodeModel::parse(input, "c")
                .map(Doc::boxed_code)
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
            Doc::Mount(a) => a,
            Doc::Boxed(a, _, _) => &*a.0,
        }
    }

    /// Run `query` and render its results as exportable markup:
    /// `md`/`markdown`, `html`, or `txt`/`text`. Node results render
    /// structurally through the text vocabulary — sections back to
    /// headings, lists to lists — and kinds outside it degrade to
    /// prose paragraphs; value results render as lines.
    pub fn export(&self, query: &str, run: &Run, kind: &str) -> Result<String> {
        let render = quarb_text::Render::from_name(kind)
            .ok_or_else(|| anyhow::anyhow!("unknown export format: {kind} (md, html, txt)"))?;
        // An empty query exports the whole document.
        if query.trim().is_empty() {
            let base = self.base_dyn();
            return Ok(quarb_text::render_nodes(base, &[base.root()], render));
        }
        match self
            .run(query, run)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .result
        {
            QueryResult::Nodes(nodes) => {
                Ok(quarb_text::render_nodes(self.base_dyn(), &nodes, render))
            }
            QueryResult::Values(values) => Ok(quarb_text::render::render_values(&values, render)),
        }
    }

    /// Run against a `--model`-enriched view of this source: the
    /// derived containers, references, and edges the model declares,
    /// over this `Doc`'s base. `run.now` binds `now()` for the base
    /// and its constructor queries alike.
    pub fn run_modeled(
        &self,
        query: &str,
        run: &Run,
        model: &quarb_model::Model,
    ) -> quarb::Result<Outcome> {
        let base = quarb_model::Borrowed(self.base_dyn());
        let enriched = quarb_model::ModelAdapter::new(base, model.clone());
        enriched
            .check()
            .map_err(|e| quarb::QuarbError::parse(format!("reading the model: {e}")))?;
        quarb::Query::parse_with(query, &quarb::Defs::default(), &enriched, run)?
            .run(&enriched, run)
    }

    /// Render a node from a model-enriched run: `/container/value`
    /// for derived nodes, the base's own renderer otherwise.
    pub fn render_modeled(&self, node: NodeId, model: &quarb_model::Model) -> String {
        let enriched =
            quarb_model::ModelAdapter::new(quarb_model::Borrowed(self.base_dyn()), model.clone());
        enriched.locator(node, |bn| self.render(bn))
    }

    /// Run `query` under `run` (its instant, shell gate, reference
    /// targets).
    pub fn run(&self, query: &str, run: &Run) -> quarb::Result<Outcome> {
        macro_rules! go {
            ($a:expr) => {{ quarb::Query::parse_with(query, &quarb::Defs::default(), $a, run)?.run($a, run) }};
        }
        match self {
            Doc::Json(a) => go!(a),
            Doc::Csv(a) => go!(a),
            Doc::Xml(a) => go!(a),
            Doc::Html(a) => go!(a),
            Doc::Text(a) => go!(a),
            #[cfg(feature = "sqlite")]
            Doc::Sqlite(a) => go!(a),
            Doc::Mount(a) => go!(a),
            Doc::Boxed(a, _, _) => go!(a),
        }
    }

    /// Run `query` with a closing `@| table` taken off
    /// ([`quarb::Query::without_closing`]): what stands before it in
    /// the outcome's result, the closing command in `closing`, for a
    /// host that draws the table itself.
    pub fn run_rendering(&self, query: &str, run: &Run) -> quarb::Result<Outcome> {
        macro_rules! go {
            ($a:expr) => {{
                quarb::Query::parse_with(query, &quarb::Defs::default(), $a, run)?
                    .without_closing()
                    .run($a, run)
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
            Doc::Mount(a) => go!(a),
            Doc::Boxed(a, _, _) => go!(a),
        }
    }

    /// The rows of a rendered table ([`quarb::table_rows_of`]): the
    /// values as they are; a node that is a document's own table as
    /// its rows' records, any other node as the record of the asked
    /// columns, else its locator.
    pub fn table_rows(&self, result: QueryResult, columns: &[String]) -> Vec<Value> {
        quarb::table_rows_of(self.base_dyn(), result, columns, &|n| self.render(n))
    }

    /// The static plan of `query` against this document: every
    /// step priced by the adapter's own estimate, the budget's cap
    /// (when one is given) stopping the estimators early.
    pub fn plan(
        &self,
        query: &str,
        run: &Run,
        budget: Option<&quarb::plan::Budget>,
    ) -> quarb::Result<quarb::plan::Plan> {
        macro_rules! go {
            ($a:expr) => {{
                let q = quarb::Query::parse_with(query, &quarb::Defs::default(), $a, run)?;
                Ok(match budget {
                    Some(b) => q.plan_within($a, b),
                    None => q.plan($a),
                })
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
            Doc::Mount(a) => go!(a),
            Doc::Boxed(a, _, _) => go!(a),
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
            Doc::Mount(_) => "mount",
            Doc::Boxed(_, _, family) => family,
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
            Doc::Mount(a) => generic_locator(a, node),
            Doc::Boxed(_, render, _) => render(node),
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

    /// Declare each mounted source's URL as an identity of its root
    /// (`urls` is `(mount name, url)`), so a reference naming the
    /// URL lands on the mount through the adapter — the run needs
    /// no reference-target map. On a single source, nothing to
    /// declare: [`Doc::url_roots`] answers for it.
    pub fn declare_identities(&mut self, urls: &[(String, String)]) {
        if let Doc::Mount(m) = self {
            for (name, url) in urls {
                if let Some(idx) = (0..m.len()).find(|&i| m.mount_name(i) == name) {
                    m.declare_identity(idx, url.clone());
                }
            }
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
            Doc::Mount(_) => bail!("cannot nest a mount inside a mount"),
            Doc::Boxed(a, _, _) => a.0,
        })
    }
}

// ---------------------------------------------------------------------
// Native-only: filesystem/db/git dispatch and multi-source mounts.
// ---------------------------------------------------------------------

#[cfg(feature = "native")]
impl Doc {
    /// Open one target as a local source through quarb-open — the
    /// door the command line mounts through, so a target opens the
    /// same way everywhere: paths, documents by extension or
    /// content, the reading prefixes (`text:`, `corpus:`, `lit:`,
    /// `koine:`, `code:`, `web:`, `lines:`, `json2:`), archives,
    /// databases and the adapter-scheme fleet.
    pub fn open(path: &Path, opts: &Options) -> Result<Doc> {
        let target = path.to_string_lossy();
        let m = quarb_open::open_target(&target, &opts.open_opts())?;
        Ok(Doc::Boxed(Dyn(m.adapter), m.render, m.family))
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
