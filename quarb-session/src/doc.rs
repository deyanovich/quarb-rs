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
#[cfg(feature = "native")]
use std::rc::Rc;

/// Options that shape how native sources open (unused on wasm, which
/// only parses text).
#[derive(Clone, Copy, Default)]
pub struct Options {
    pub hidden: bool,
    pub respect_ignore: bool,
    pub descend: bool,
}

/// A materialized source: one variant per adapter family. JSON-model
/// formats (json/yaml/toml) render node results as pointers, the rest
/// as locators.
pub enum Doc {
    Json(quarb_json::JsonAdapter),
    Csv(quarb_csv::CsvAdapter),
    Xml(quarb_xml::XmlAdapter),
    Html(quarb_html::HtmlAdapter),
    #[cfg(feature = "native")]
    Sqlite(quarb_sqlite::SqliteAdapter),
    #[cfg(feature = "native")]
    Fs(quarb_fs::FsAdapter),
    #[cfg(feature = "native")]
    FsDeep(quarb_compose::ComposeAdapter<quarb_fs::FsAdapter>),
    #[cfg(feature = "native")]
    Git(quarb_git::GitAdapter),
    #[cfg(feature = "native")]
    Archive(quarb_compose::ComposeAdapter<quarb_archive::ArchiveAdapter>),
    #[cfg(feature = "native")]
    Xlsx(quarb_xlsx::XlsxAdapter),
    #[cfg(feature = "native")]
    Code(quarb_code::CodeAdapter),
    #[cfg(feature = "native")]
    Mount(quarb_mount::MountAdapter),
}

impl Doc {
    /// Parse a text document by format name — the wasm entry point,
    /// and the text tail of the native `open`. Formats: json, yaml,
    /// toml, csv, tsv, xml, html, markdown.
    pub fn parse(input: &str, format: &str) -> Result<Doc> {
        match format {
            "json" => quarb_json::JsonAdapter::parse(input)
                .map(Doc::Json)
                .context("parsing JSON"),
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
            other => bail!("unknown format: {other}"),
        }
    }

    /// Run one query against this source with the session's invocation
    /// instant and shell permission. The query text carries any macro
    /// definitions inline (the session prepends its table), which
    /// `quarb::run` expands.
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
            #[cfg(feature = "native")]
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
            Doc::Xlsx(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Code(a) => go!(a),
            #[cfg(feature = "native")]
            Doc::Mount(a) => go!(a),
        }
    }

    /// Render a node result as its source-appropriate locator.
    pub fn render(&self, node: NodeId) -> String {
        match self {
            Doc::Json(a) => a.pointer(node),
            Doc::Csv(a) => a.locator(node),
            Doc::Xml(a) => a.locator(node),
            Doc::Html(a) => a.locator(node),
            #[cfg(feature = "native")]
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
            Doc::Xlsx(a) => a.locator(node),
            #[cfg(feature = "native")]
            Doc::Code(a) => a.locator(node),
            #[cfg(feature = "native")]
            Doc::Mount(a) => generic_locator(a, node),
        }
    }
}

// ---------------------------------------------------------------------
// Native-only: filesystem/db/git dispatch and multi-source mounts.
// ---------------------------------------------------------------------

#[cfg(feature = "native")]
impl Doc {
    /// Open one path as a local source. Directories are filesystem
    /// trees (`--descend` grafts parseable leaves); `git:PATH` opens a
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
            return Ok(if opts.descend {
                Doc::FsDeep(quarb_compose::ComposeAdapter::new(fs))
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

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        if let Some(e) = &ext
            && quarb_code::supported(e)
        {
            let a = quarb_code::CodeAdapter::open(path).context("parsing source file")?;
            return Ok(Doc::Code(a));
        }
        if matches!(ext.as_deref(), Some("xlsx" | "xls" | "ods")) {
            let a = quarb_xlsx::XlsxAdapter::open(path).context("opening workbook")?;
            return Ok(Doc::Xlsx(a));
        }
        if is_sqlite(path) {
            let a = quarb_sqlite::SqliteAdapter::open(path).context("opening SQLite database")?;
            return Ok(Doc::Sqlite(a));
        }
        if is_archive(path) {
            let a = quarb_archive::ArchiveAdapter::open(path).context("opening archive")?;
            return Ok(Doc::Archive(quarb_compose::ComposeAdapter::new(a)));
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
        let mut mounts: Vec<quarb_mount::Mount> = Vec::new();
        for (i, p) in paths.iter().enumerate() {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("doc{i}"));
            if mounts.iter().any(|m| m.name == stem) {
                bail!(
                    "input '{}' mounts as '{stem}', colliding with an earlier input of the \
                     same file stem; give each a distinct basename",
                    p.display()
                );
            }
            let adapter = Doc::open(p, opts)?.into_boxed()?;
            mounts.push(quarb_mount::Mount { name: stem, adapter });
        }
        Ok(Doc::Mount(quarb_mount::MountAdapter::new(mounts)))
    }

    /// Box this source as a shared adapter — a mount child.
    fn into_boxed(self) -> Result<Box<dyn quarb::AstAdapter>> {
        use quarb_mount::Shared;
        Ok(match self {
            Doc::Json(a) => Box::new(Shared(Rc::new(a))),
            Doc::Csv(a) => Box::new(Shared(Rc::new(a))),
            Doc::Xml(a) => Box::new(Shared(Rc::new(a))),
            Doc::Html(a) => Box::new(Shared(Rc::new(a))),
            Doc::Sqlite(a) => Box::new(Shared(Rc::new(a))),
            Doc::Fs(a) => Box::new(Shared(Rc::new(a))),
            Doc::FsDeep(a) => Box::new(Shared(Rc::new(a))),
            Doc::Git(a) => Box::new(Shared(Rc::new(a))),
            Doc::Archive(a) => Box::new(Shared(Rc::new(a))),
            Doc::Xlsx(a) => Box::new(Shared(Rc::new(a))),
            Doc::Code(a) => Box::new(Shared(Rc::new(a))),
            Doc::Mount(_) => bail!("cannot nest a mount inside a mount"),
        })
    }
}

/// A name-path locator built from the adapter trait alone
/// (`parent`/`name`) — used for a mount, whose per-source render
/// functions we do not keep.
#[cfg(feature = "native")]
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
