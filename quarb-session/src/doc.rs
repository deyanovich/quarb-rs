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

/// A boxed adapter as an adapter — plain delegation (the
/// quarb-py `Dyn` pattern).
pub struct Dyn(pub Box<dyn quarb::AstAdapter>);

impl quarb::AstAdapter for Dyn {
    fn root(&self) -> NodeId {
        self.0.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.0.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.0.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.0.parent(node)
    }
    fn traits(&self, node: NodeId) -> Vec<String> {
        self.0.traits(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<quarb::Value> {
        self.0.property(node, name)
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.0.children_named(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<quarb::Value> {
        self.0.default_value(node)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<quarb::Value> {
        self.0.metadata(node, key)
    }
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        self.0.aliased_metadata(node)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.0.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.0.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.0.resolve(node, property, hint)
    }
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<quarb::Value> {
        self.0.link_property(source, label, target, name)
    }
    fn quantifier_bound(&self) -> usize {
        self.0.quantifier_bound()
    }
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        self.0.invocation_instant()
    }
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.0.unit_scale(expr)
    }
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
            "jsonl" | "ndjson" => quarb_json::JsonAdapter::parse_lines(input)
                .map(Doc::Json)
                .context("parsing JSONL"),
            "yaml" | "yml" => quarb_yaml::parse(input).map(Doc::Json).context("parsing YAML"),
            "toml" => quarb_toml::parse(input).map(Doc::Json).context("parsing TOML"),
            "csv" => quarb_csv::CsvAdapter::parse_with_delimiter(input, b',')
                .map(Doc::Csv)
                .context("parsing CSV"),
            "tsv" => quarb_csv::CsvAdapter::parse_with_delimiter(input, b'\t')
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

    pub fn run(&self, query: &str, now: (i64, u32), allow_shell: bool) -> quarb::Result<QueryResult> {
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
        anyhow::bail!(
            "this quarb-session was built without the `sqlite` feature"
        )
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
        Ok(Doc::Mount(quarb_mount::MountAdapter::new(mounts)))
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
        if path.is_dir() {
            let fsopts = quarb_fs::FsOptions {
                hidden: opts.hidden,
                respect_ignore: opts.respect_ignore,
            };
            let fs = quarb_fs::FsAdapter::with_options(path, fsopts)
                .with_context(|| format!("opening directory {}", path.display()))?;
            return Ok(if opts.graft {
                Doc::FsDeep(quarb_compose::ComposeAdapter::with_source_paths(
                    fs,
                    |fs, n| Some(fs.path(n)),
                ))
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
                let text = std::fs::read_to_string(target)
                    .with_context(|| format!("reading {}", target.display()))?;
                Ok(text
                    .strip_prefix('\u{feff}')
                    .map(str::to_owned)
                    .unwrap_or(text))
            };
            if let Some(fmt) = format {
                let imported = match fmt.as_str() {
                    "atd" | "atk" => {
                        let model =
                            quarb_text_koine::parse_file(target).with_context(|| {
                                format!("reading {} as an atrep document", target.display())
                            })?;
                        return Ok(Doc::Text(model));
                    }
                    "md" | "markdown" => quarb_text_koine::parse_markdown(&read()?),
                    "html" => quarb_text_koine::parse_html(&read()?),
                    "rst" => quarb_text_koine::parse_rst(&read()?),
                    "org" => quarb_text_koine::parse_org(&read()?),
                    "dj" | "djot" => quarb_text_koine::parse_djot(&read()?),
                    "tei" | "docbook" | "jats" | "usx" | "osis" => {
                        quarb_text_koine::parse_xml_as(&read()?, &fmt)
                    }
                    other => {
                        return Err(anyhow::anyhow!(
                            "unknown koine format {other:?} — known: md, html, rst, org, djot, tei, docbook, jats, usx, osis, atd"
                        ));
                    }
                };
                return imported.map(Doc::Text).with_context(|| {
                    format!("importing {} through atrep as {fmt}", target.display())
                });
            }
            let imported = match ext.as_deref() {
                Some("atd" | "atk") => {
                    let model = quarb_text_koine::parse_file(target).with_context(|| {
                        format!("reading {} as an atrep document", target.display())
                    })?;
                    return Ok(Doc::Text(model));
                }
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
                                "{} declares no XML identity this route knows — force one with koine:{}?format=tei|docbook|jats|usx|osis",
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
        // A `text:` prefix forces the text-level reading, matching
        // qua's dispatch: producer by extension, `<` sniffing
        // markup, plain paragraphs as the fallback.
        if let Some(rest) = s.strip_prefix("text:")
            && !rest.is_empty()
        {
            let target = Path::new(rest);
            // The binary producers dispatch before the text read:
            // a .docx or .epub is a zip, a .pdf its own format.
            let ext = target
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            // Native atrep files converge on the koine route.
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
            if let Some(ext @ ("docx" | "epub")) = ext.as_deref() {
                let bytes = std::fs::read(target)
                    .with_context(|| format!("reading {}", target.display()))?;
                let model = if ext == "docx" {
                    quarb_text_docx::parse(&bytes)
                        .with_context(|| format!("reading {} as Word", target.display()))?
                } else {
                    quarb_text_epub::parse(&bytes)
                        .with_context(|| format!("reading {} as EPUB", target.display()))?
                };
                return Ok(Doc::Text(model));
            }
            let text = std::fs::read_to_string(target)
                .with_context(|| format!("reading {}", target.display()))?;
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
                _ if text.trim_start().starts_with('<') => "text-html",
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
                    quarb_compose::ComposeAdapter::with_source_paths(fs, |fs, n| {
                        Some(fs.path(n))
                    })
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
            let a = quarb_tree_sitter::TreeSitterAdapter::open(path).context("parsing source file")?;
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
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let text = text
            .strip_prefix('\u{feff}')
            .map(str::to_owned)
            .unwrap_or(text);
        match ext.as_deref() {
            Some("csv") => Doc::parse(&text, "csv"),
            Some("tsv") => Doc::parse(&text, "tsv"),
            Some("yaml" | "yml") => Doc::parse(&text, "yaml"),
            Some("toml") => Doc::parse(&text, "toml"),
            Some("md" | "markdown") => Doc::parse(&text, "markdown"),
            Some("txt") => Doc::parse(&text, "text"),
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
        Ok(Doc::Mount(quarb_mount::MountAdapter::new(mounts)))
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
            "zip" | "tar" | "gz" | "tgz" | "jar" | "war" | "docx" | "pptx" | "odt" | "odp"
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
