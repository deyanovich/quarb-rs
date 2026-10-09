//! `quarb-open` — one `open` for every target the Quarb tools
//! speak: filesystem paths, documents by extension or content,
//! the reading prefixes (`text:`, `corpus:`, `lit:`, `koine:`,
//! `code:`, `web:`, `lines:`, `json2:`), archives, databases and
//! the adapter-scheme fleet (`gcl:`, `kafka:`, `neo4j://`, …).
//! The CLI, the session (quai, the Python package) and every
//! other front-end mount through [`open_target`], so a target
//! opens the same way everywhere and a format is dispatched in
//! one place.
//!
//! The source boundary lives here too: `?encoding=` comes off a
//! target before it is opened, `?delimiter=` / `?decimal=` for
//! delimited files, `?references=STANDARD` for a text reading,
//! and the record of the decoding a read made goes on the
//! mount's root as `::::encoding` / `::::replaced`.

use anyhow::Context;
use quarb::{AstAdapter, NodeId};
use quarb_age::AgeAdapter;
use quarb_arangodb::ArangoAdapter;
use quarb_archive::ArchiveAdapter;
#[cfg(feature = "heavy")]
use quarb_athena::AthenaAdapter;
use quarb_atrep::AtrepAdapter;
use quarb_azlogs::AzlAdapter;
#[cfg(feature = "heavy")]
use quarb_bigquery::BigqueryAdapter;
use quarb_cflogs::CflAdapter;
use quarb_compose::{ComposeAdapter, DocumentGraft, SourceGraft};
use quarb_cosmos::CosmosAdapter;
use quarb_csv::CsvAdapter;
use quarb_cwlogs::CwlAdapter;
use quarb_datastore::DatastoreAdapter;
use quarb_ddlogs::DdlAdapter;
#[cfg(feature = "heavy")]
use quarb_duckdb::DuckdbAdapter;
use quarb_dynamodb::DynamodbAdapter;
use quarb_falkordb::FalkorAdapter;
use quarb_firebase::FirebaseAdapter;
use quarb_firestore::FirestoreAdapter;
use quarb_fs::{FsAdapter, FsOptions};
use quarb_gcplogs::GclAdapter;
use quarb_git::GitAdapter;
use quarb_github::GithubAdapter;
use quarb_gitlab::GitlabAdapter;
use quarb_gsheet::GsheetAdapter;
use quarb_html::HtmlAdapter;
use quarb_imap::ImapAdapter;
use quarb_json::JsonAdapter;
use quarb_kafka::KafkaAdapter;
use quarb_kubernetes::KubernetesAdapter;
#[cfg(feature = "kuzu")]
use quarb_kuzu::KuzuAdapter;
use quarb_ldap::LdapAdapter;
use quarb_maildir::MaildirAdapter;
use quarb_memgraph::MemgraphAdapter;
use quarb_metatheca::MetathecaAdapter;
#[cfg(feature = "heavy")]
use quarb_mongodb::MongodbAdapter;
use quarb_mount::Shared;
#[cfg(feature = "heavy")]
use quarb_mssql::MssqlAdapter;
#[cfg(feature = "heavy")]
use quarb_mysql::MysqlAdapter;
use quarb_neo4j::Neo4jAdapter;
use quarb_neptune::NeptuneAdapter;
use quarb_objstore::ObjstoreAdapter;
#[cfg(feature = "heavy")]
use quarb_oracle::OracleAdapter;
use quarb_postgres::PostgresAdapter;
use quarb_redis::RedisAdapter;
use quarb_serve::ServeAdapter;
use quarb_sparql::SparqlAdapter;
use quarb_sqlite::SqliteAdapter;
use quarb_tree_sitter::TreeSitterAdapter;
use quarb_xlsx::XlsxAdapter;
use quarb_xml::XmlAdapter;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The source boundary of one open: the options the target carried
/// — `?encoding=` (the explicit label), `?delimiter=` / `?decimal=`
/// for a delimited file, `?references=STANDARD` for a text reading —
/// and the record of the decoding a read made, which goes on the
/// mount's root as `::::encoding` / `::::replaced`. One per target
/// opened, passed to every door and reader; nothing about a source
/// lives in a thread-local.
#[derive(Clone, Default)]
pub struct SourceCtx {
    encoding: std::cell::RefCell<Option<String>>,
    csv: std::cell::Cell<quarb_csv::CsvOptions>,
    references: std::cell::RefCell<Option<String>>,
    record: std::cell::RefCell<Option<(String, usize)>>,
}

impl SourceCtx {
    /// The `?encoding=` label in force, if one was given.
    pub fn encoding(&self) -> Option<String> {
        self.encoding.borrow().clone()
    }

    fn set_encoding(&self, label: &str) {
        *self.encoding.borrow_mut() = Some(label.to_string());
    }

    fn clear_encoding(&self) {
        *self.encoding.borrow_mut() = None;
    }

    /// The delimited-file options in force.
    pub fn csv(&self) -> quarb_csv::CsvOptions {
        self.csv.get()
    }

    fn set_csv(&self, next: quarb_csv::CsvOptions) {
        self.csv.set(next);
    }

    fn references(&self) -> Option<String> {
        self.references.borrow().clone()
    }

    fn set_references(&self, standard: Option<&str>) {
        *self.references.borrow_mut() = standard.map(str::to_string);
    }

    fn set_record(&self, encoding: String, replaced: usize) {
        *self.record.borrow_mut() = Some((encoding, replaced));
    }

    /// The decoding record of the read this context saw, taken.
    pub fn take_record(&self) -> Option<(String, usize)> {
        self.record.borrow_mut().take()
    }
}

/// Read and parse the `--refs` document into the relational form,
/// `(field, container)` pairs. (The Firebase adapter parses the
/// same file into its own path-shaped form.) Empty when no --refs.
pub fn relational_refs(refs: &Option<PathBuf>) -> anyhow::Result<Vec<(String, String)>> {
    match refs {
        Some(f) => {
            let text = std::fs::read_to_string(f)
                .with_context(|| format!("reading refs file {}", f.display()))?;
            quarb_relational::parse_refs(&text).map_err(|e| anyhow::anyhow!("parsing refs: {e}"))
        }
        None => Ok(Vec::new()),
    }
}

/// Whether the input is a Quarb query file (`.quarb`), to be
/// reflected as a query arbor.
pub fn is_quarb(path: Option<&Path>) -> bool {
    path.and_then(Path::extension)
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("quarb"))
}

/// Whether the input names a PostgreSQL connection rather than a
/// file: a `postgres://` / `postgresql://` URL, or the keyword
/// form (`host=... dbname=...`).
pub fn is_pg_config(s: &str) -> bool {
    s.starts_with("postgres://") || s.starts_with("postgresql://") || s.starts_with("host=")
}

/// Whether the extension belongs to a format the text dispatch
/// owns: the archive/SQLite magic sniffs must not pre-empt these —
/// a CSV whose first cell starts with "PK" is still a CSV.
pub fn known_text_ext(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "quarb"
                | "csv"
                | "tsv"
                | "json"
                | "jsonl"
                | "ndjson"
                | "yaml"
                | "yml"
                | "toml"
                | "md"
                | "markdown"
                | "kaiv"
                | "daiv"
                | "raiv"
                | "atd"
                | "atk"
                | "xml"
                | "fb2"
                | "svg"
                | "xhtml"
                | "html"
                | "htm"
                | "txt"
                | "conllu"
                | "conllup"
        )
    })
}

/// Zip-family or tar archives, by extension or magic bytes. The
/// magic sniff skips extensions the text dispatch owns.
pub fn is_archive(path: &Path) -> bool {
    if let Some(e) = path.extension().and_then(|e| e.to_str())
        && (quarb_archive::archive_extension(e) || quarb_archive::container_extension(e))
    {
        return true;
    }
    if known_text_ext(path) {
        return false;
    }
    let mut buf = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut buf))
        .map(|n| n == 2 && (buf == *b"PK" || buf == [0x1f, 0x8b]))
        .unwrap_or(false)
}

/// Whether the input is a SQLite database: by extension
/// (`.db` / `.sqlite` / `.sqlite3`), or by the 16-byte magic (again
/// skipped for extensions the text dispatch owns).
pub fn is_sqlite(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        e.eq_ignore_ascii_case("db")
            || e.eq_ignore_ascii_case("sqlite")
            || e.eq_ignore_ascii_case("sqlite3")
    }) {
        return true;
    }
    if known_text_ext(path) {
        return false;
    }
    let mut buf = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_ok()
        && &buf == b"SQLite format 3\0"
}

/// The CSV field delimiter implied by the file extension: `.csv`
/// (comma) or `.tsv` (tab), else not a CSV file.
pub fn csv_delimiter(path: Option<&Path>) -> Option<u8> {
    let ext = path?.extension()?.to_str()?;
    if ext.eq_ignore_ascii_case("csv") {
        Some(b',')
    } else if ext.eq_ignore_ascii_case("tsv") {
        Some(b'\t')
    } else {
        None
    }
}

/// Whether the input is an atrep document: the first content line
/// (after an optional shebang) is a dialektos declaration in either
/// sigil — `@@@!<id>` or `\\\!<id>`. Extension dispatch handles
/// `.atd`/`.atk`; this sniff catches stdin and unsuffixed files,
/// and cannot collide with the `<`-leading XML/HTML sniffs.
pub fn is_atrep(text: &str) -> bool {
    let mut lines = text.lines();
    let mut first = lines.next().unwrap_or("");
    if first.starts_with("#!") {
        first = lines.next().unwrap_or("");
    }
    let decl = first.trim_start();
    decl.starts_with("@@@!") || decl.starts_with("\\\\\\!")
}

/// Whether the input should be parsed as XML: an `.xml`/`.svg`/
/// `.xhtml` extension, or content that begins with the `<?xml`
/// prologue. Checked before HTML, whose generic `<` sniff would
/// otherwise swallow XML.
pub fn is_xml(path: Option<&Path>, text: &str) -> bool {
    let by_ext = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            ["xml", "svg", "xhtml", "fb2"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        });
    by_ext || text.trim_start().starts_with("<?xml")
}

/// Whether the input should be parsed as HTML: an `.html`/`.htm`
/// extension, or content that begins with `<`.
pub fn is_html(path: Option<&Path>, text: &str) -> bool {
    let by_ext = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    by_ext || text.trim_start().starts_with('<')
}

/// The text-level reading of a document (`text:` targets and
/// `.txt` files): the producer is chosen by extension, `<` sniffs
/// markup for the rest, plain paragraphs are the fallback.
/// The binary text-level producers, by extension: formats whose
/// bytes are a container, not UTF-8. Plain (unprefixed) paths
/// keep their archive reading — `text:` opts into the reader's
/// model.
/// A DjVu by its extension (`.djvu`, `.djv`): the text layer's
/// reading, at `text:`.
pub fn is_djvu(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("djvu") || e.eq_ignore_ascii_case("djv"))
}

pub fn binary_text_kind(path: &Path) -> Option<&'static str> {
    // A Lingvo DSL dictionary (`.dsl`, `.dsl.dz`): the koine route's
    // import, under the dictionary vocabulary.
    if quarb_text_koine::is_dsl_path(path) {
        return Some("dsl");
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "docx" => Some("docx"),
        "epub" => Some("epub"),
        // OpenDocument text: the package, the flat XML, and
        // OpenOffice.org 1.0's; `.ods` is a spreadsheet (xlsx's
        // reading), `.odp` a deck, neither a text.
        "odt" | "fodt" | "sxw" => Some("odt"),
        // Word 97–2003 by its OLE2 magic; a plain-text file named
        // `.doc` (the kernel's m68k notes) keeps the plain reading.
        "doc" => std::fs::read(path)
            .ok()
            .filter(|b| quarb_text_doc::is_ole2(b))
            .map(|_| "doc"),
        // A zip under another name whose `mimetype` member
        // declares an OpenDocument text.
        "zip" => std::fs::read(path)
            .ok()
            .filter(|b| quarb_text_odt::declares_text(b))
            .map(|_| "odt"),
        _ => None,
    }
}

pub fn binary_text_level(
    kind: &str,
    bytes: &[u8],
    path: &Path,
    ctx: &SourceCtx,
) -> anyhow::Result<quarb_text::TextModel> {
    match kind {
        "dsl" => quarb_text_koine::parse_dsl_file(path)
            .map_err(|e| anyhow::anyhow!("as a Lingvo DSL dictionary: {e}")),
        "docx" => quarb_text_docx::parse_with_decoding(bytes, &current_decoding(ctx)?)
            .map(|mut m| {
                // Word's sources list lowers under the Latin
                // campi; the census lets ::author answer.
                quarb_text_koine::attach_bib_aliases(&mut m);
                m
            })
            .map_err(|e| anyhow::anyhow!("as Word: {e}")),
        "odt" => quarb_text_odt::parse_with_decoding(bytes, &current_decoding(ctx)?)
            .map_err(|e| anyhow::anyhow!("as OpenDocument: {e}")),
        "doc" => quarb_text_doc::parse(bytes).map_err(|e| anyhow::anyhow!("as Word 97: {e}")),
        _ => quarb_text_epub::parse_with_decoding(bytes, &current_decoding(ctx)?)
            .map_err(|e| anyhow::anyhow!("as EPUB: {e}")),
    }
}

pub enum WebSite {
    Memory(quarb_web::WebAdapter<quarb_web::MemoryStore>),
    /// The store itself: the caller plans before wrapping it.
    Sqlite(quarb_web::db::sqlite::SqliteStore),
    Postgres(quarb_web::db::postgres::PostgresStore),
}

/// `encoding=LABEL` among a reader's own options: validated and
/// set for the reads that follow. Every reader prefix takes it.
pub fn set_encoding_option(label: &str, ctx: &SourceCtx) -> anyhow::Result<()> {
    quarb::source::encoding_for_label(label)
        .ok_or_else(|| anyhow::anyhow!("unknown encoding label {label:?}"))?;
    ctx.set_encoding(label);
    Ok(())
}

/// The source-boundary rule for the mount being opened (its
/// `?encoding=` and the host's declaration reporter), for the
/// layers that read bytes themselves: a folder's grafts, an
/// archive's members, a mailbox, a site's pages.
pub fn current_decoding(ctx: &SourceCtx) -> anyhow::Result<quarb::source::SourceDecoding> {
    let explicit = ctx.encoding();
    quarb_compose::source_decoding(explicit.as_deref()).map_err(|e| anyhow::anyhow!("{e}"))
}

/// The mailbox rule: the mount's option, then the charset a
/// message's own headers declare.
pub fn mail_decoding(ctx: &SourceCtx) -> anyhow::Result<quarb::source::SourceDecoding> {
    let explicit = ctx.encoding();
    quarb::source::SourceDecoding::default()
        .with_declared(|_, b| quarb_maildir::declared_encoding(b))
        .with_option(explicit.as_deref())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// A reader prefix with no options of its own (`text:`, `lines:`,
/// `json2:`, `code:`) still takes `?encoding=LABEL`; a path as
/// written is taken as is.
pub fn take_reader_encoding(rest: &str, ctx: &SourceCtx) -> anyhow::Result<String> {
    // Each mount states its own options: a `?references=` from an
    // earlier mount does not carry over.
    ctx.set_references(None);
    if Path::new(rest).exists() || !rest.contains('?') {
        return Ok(rest.to_string());
    }
    let (path, query) = rest.rsplit_once('?').unwrap();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some(("encoding", label)) => set_encoding_option(label, ctx)?,
            Some(("references", standard)) => set_references_option(standard, ctx)?,
            _ => anyhow::bail!(
                "unknown option {pair:?} on {path} — this reader takes encoding=, references="
            ),
        }
    }
    Ok(path.to_string())
}

pub fn set_references_option(standard: &str, ctx: &SourceCtx) -> anyhow::Result<()> {
    anyhow::ensure!(
        quarb::bibref::STANDARDS.contains(&standard),
        "references={standard} — references= takes {}",
        quarb::bibref::STANDARDS.join(", ")
    );
    ctx.set_references(Some(standard));
    Ok(())
}

/// Apply the mount's `?references=` to a model just built (every
/// document of a folder mount alike).
pub fn apply_references(
    mut model: quarb_text::TextModel,
    ctx: &SourceCtx,
) -> anyhow::Result<quarb_text::TextModel> {
    if let Some(standard) = ctx.references() {
        model
            .lower_references(&standard)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(model)
}

pub fn web_level(spec: &str, ctx: &SourceCtx) -> anyhow::Result<WebSite> {
    let (path_part, base) = match spec.split_once('?') {
        Some((p, q)) => {
            let mut base = String::new();
            for pair in q.split('&') {
                match pair.split_once('=') {
                    Some(("base", v)) => base = v.to_string(),
                    Some(("encoding", v)) => set_encoding_option(v, ctx)?,
                    _ => anyhow::bail!("unknown web option {pair:?} — supported: base=, encoding="),
                }
            }
            (p, base)
        }
        None => (spec, String::new()),
    };
    if path_part.starts_with("postgres://") || path_part.starts_with("postgresql://") {
        return quarb_web::db::postgres::PostgresStore::open(path_part)
            .map(WebSite::Postgres)
            .map_err(|e| anyhow::anyhow!("opening the site store: {e}"));
    }
    let target = Path::new(path_part);
    if target.is_dir() {
        return quarb_web::fs::open_dir_with_decoding(target, &base, current_decoding(ctx)?)
            .map(WebSite::Memory)
            .with_context(|| format!("reading {} as a site", target.display()));
    }
    if is_archive(target) {
        return quarb_web::archive::open_path_with_decoding(target, &base, current_decoding(ctx)?)
            .map(WebSite::Memory)
            .with_context(|| format!("reading {} as a site", target.display()));
    }
    // An index-backed store: a site.db built by quarb-web-ingest.
    if target
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "db" | "sqlite" | "sqlite3"))
    {
        return quarb_web::db::sqlite::SqliteStore::open(target)
            .map(WebSite::Sqlite)
            .map_err(|e| anyhow::anyhow!("opening {} as a site store: {e}", target.display()));
    }
    anyhow::bail!(
        "web: takes a directory of pages, an archive of them, or a site.db store, not {}",
        target.display()
    )
}

/// The koine reading (`koine:` — and `text:` on native atrep
/// files): native atrep documents lower directly; Markdown, HTML,
/// reStructuredText, Org, and djot arrive through atrep's
/// endomorphosis importers into their mirror dialects. Anything
/// else refuses by design — no atrep import exists for it yet,
/// and a PDF never will (the print reading is `text:`).
pub fn koine_level(spec: &str, ctx: &SourceCtx) -> anyhow::Result<quarb_text::TextModel> {
    apply_references(koine_level_inner(spec, ctx)?, ctx)
}

pub fn koine_level_inner(spec: &str, ctx: &SourceCtx) -> anyhow::Result<quarb_text::TextModel> {
    // The house ?param syntax: `koine:records.xml?format=jats`
    // forces a format — the third dispatch tier, for stdin-ish,
    // extensionless, or undeclared inputs.
    ctx.set_references(None);
    let (path_part, format) = match spec.split_once('?') {
        Some((p, q)) => {
            let mut fmt = None;
            for pair in q.split('&') {
                match pair.split_once('=') {
                    Some(("format", v)) => fmt = Some(v.to_string()),
                    Some(("encoding", v)) => set_encoding_option(v, ctx)?,
                    Some(("references", v)) => set_references_option(v, ctx)?,
                    _ => anyhow::bail!(
                        "unknown koine option {pair:?} — supported: format=, encoding=, references="
                    ),
                }
            }
            (p, fmt)
        }
        None => (spec, None),
    };
    let target = Path::new(path_part);
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let read = || -> anyhow::Result<String> {
        let text = read_prose(target, ctx)?;
        Ok(match text.strip_prefix('\u{feff}') {
            Some(rest) => rest.to_owned(),
            None => text,
        })
    };
    if let Some(fmt) = format {
        let imported = match fmt.as_str() {
            "atd" | "atk" => {
                return quarb_text_koine::parse_file(target)
                    .with_context(|| format!("reading {} as an atrep document", target.display()));
            }
            "md" | "markdown" => quarb_text_koine::parse_markdown(&read()?),
            "html" => quarb_text_koine::parse_html(&read()?),
            "rst" => quarb_text_koine::parse_rst(&read()?),
            "org" => quarb_text_koine::parse_org(&read()?),
            "dj" | "djot" => quarb_text_koine::parse_djot(&read()?),
            "tei" | "docbook" | "jats" | "usx" | "osis" | "fb2" | "rnc" | "opencorpora"
            | "proiel" => quarb_text_koine::parse_xml_as(&read()?, &fmt),
            "usfm" | "sfm" => quarb_text_koine::parse_xml_as(&read()?, "usfm"),
            // Word through atrep's importer (bytes in): the second
            // route beside the native text:x.docx.
            "docx" => quarb_text_koine::parse_docx(&std::fs::read(target)?),
            // A Lingvo DSL dictionary into lexigramma, lowered under
            // the dictionary vocabulary.
            "dsl" => quarb_text_koine::parse_dsl_file(target),
            other => anyhow::bail!(
                "unknown koine format {other:?} — known: md, html, rst, org, djot, tei, docbook, jats, usx, osis, fb2, usfm, docx, dsl, atd"
            ),
        };
        return imported
            .with_context(|| format!("importing {} through atrep as {fmt}", target.display()));
    }
    if quarb_text_koine::is_dsl_path(target) {
        return quarb_text_koine::parse_dsl_file(target)
            .with_context(|| format!("importing {} as a Lingvo DSL dictionary", target.display()));
    }
    let imported = match ext.as_deref() {
        Some("atd" | "atk") => {
            return quarb_text_koine::parse_file(target)
                .with_context(|| format!("reading {} as an atrep document", target.display()));
        }
        Some("docx") => quarb_text_koine::parse_docx(&std::fs::read(target)?),
        Some("usfm" | "sfm") => quarb_text_koine::parse_xml_as(&read()?, "usfm"),
        Some("fb2") => quarb_text_koine::parse_xml_as(&read()?, "fb2"),
        Some("md" | "markdown") => quarb_text_koine::parse_markdown(&read()?),
        Some("html" | "htm") => quarb_text_koine::parse_html(&read()?),
        Some("rst") => quarb_text_koine::parse_rst(&read()?),
        Some("org") => quarb_text_koine::parse_org(&read()?),
        Some("dj" | "djot") => quarb_text_koine::parse_djot(&read()?),
        Some("xml") => {
            // The XML vocabularies dispatch by DECLARED identity:
            // root namespace, DOCTYPE public id, or unambiguous
            // root element — never a statistical guess.
            let text = read()?;
            match quarb_text_koine::detect_xml_kind(&text) {
                Some(kind) => quarb_text_koine::parse_xml_as(&text, kind),
                None => anyhow::bail!(
                    "{} declares no XML identity this route knows (no namespace, DOCTYPE, or unambiguous root) — force one with koine:{}?format=tei|docbook|jats|usx|osis|fb2|rnc|opencorpora|proiel",
                    target.display(),
                    target.display()
                ),
            }
        }
        Some("pdf") => anyhow::bail!(
            "atrep cannot import a PDF — the print reading is text:{}",
            target.display()
        ),
        _ => anyhow::bail!(
            "no atrep import for this format yet — the native reading is text:{}",
            target.display()
        ),
    };
    imported.with_context(|| format!("importing {} through atrep", target.display()))
}

/// The corpus reading (`corpus:`): the text-level
/// reading of a document with every prose block's tokens as its
/// children — the tokenizer decided once, at mount. Every source
/// `text:` reads as a prose model qualifies: plain text, HTML,
/// Markdown, LaTeX, atrep, TEI and the other koine dialects,
/// .docx and .epub. An archive keeps to `text:` for now, and a
/// PDF's reading is line geometry, never prose.
/// A document under the property reading (`json2:`).
pub enum PropertyDoc {
    Json(JsonAdapter),
    Cbor(quarb_cbor::CborAdapter),
}

/// Read `target` under the property reading: the format by
/// extension (JSON by default; JSONL, YAML, TOML, CBOR by theirs),
/// every one of them the JSON data model read the `json2` way —
/// containers as nodes, scalars as properties.
pub fn property_reading(target: &Path, ctx: &SourceCtx) -> anyhow::Result<PropertyDoc> {
    use quarb_json::Reading;
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "cbor" {
        let bytes =
            std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
        let a =
            quarb_cbor::CborAdapter::parse_as(&bytes, Reading::Property).context("parsing CBOR")?;
        return Ok(PropertyDoc::Cbor(a));
    }
    let text = read_prose(target, ctx)?;
    let a = match ext.as_str() {
        "yaml" | "yml" => quarb_yaml::parse_as(&text, Reading::Property).context("parsing YAML")?,
        "toml" => quarb_toml::parse_as(&text, Reading::Property).context("parsing TOML")?,
        "jsonl" | "ndjson" => {
            JsonAdapter::parse_lines_as(&text, Reading::Property).context("parsing JSONL")?
        }
        _ => match JsonAdapter::parse_as(&text, Reading::Property) {
            Ok(a) => a,
            Err(e) => match JsonAdapter::parse_lines_as(&text, Reading::Property) {
                Ok(a) => a,
                Err(_) => return Err(e).context("parsing JSON"),
            },
        },
    };
    Ok(PropertyDoc::Json(a))
}

pub fn corpus_level(
    rest: &str,
    allow_shell: bool,
    ctx: &SourceCtx,
) -> anyhow::Result<quarb_text::TextModel> {
    // The house ?param syntax: `?conllu=FILE` takes the tokens,
    // sentences and annotation from a CoNLL-U sidecar; `?annotate=CMD`
    // runs a command over the document's prose (stdin) and reads
    // CoNLL-U from its stdout — under the shell gate, like sh().
    // `?format=conllu` names the reading when no extension can
    // (a pipe, an extensionless file); the sniff covers the rest.
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
                    Some(("encoding", v)) => set_encoding_option(v, ctx)?,
                    Some(("format", "conllu")) => format = Some("conllu"),
                    Some(("format", v)) => anyhow::bail!(
                        "corpus: format={v} is not a treebank format — format= takes conllu"
                    ),
                    _ => anyhow::bail!(
                        "unknown corpus option {pair:?} — supported: conllu=, annotate=, desm=, cast=, places=, modernize=, notes=, format=, encoding="
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
    if is_archive(target) && binary_text_kind(target).is_none() {
        anyhow::bail!(
            "corpus: over an archive is not supported yet — mount it as text:{rest}, or corpus: one member"
        );
    }
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if ext.as_deref() == Some("pdf") {
        anyhow::bail!(
            "corpus: needs a prose reading, and a PDF's is line geometry — text:{rest} reads its lines"
        );
    }
    // A treebank is its own annotation: the file's
    // tokens under its sentences; `conllu=` / `annotate=` do not
    // apply.
    let own_annotation = || {
        anyhow::ensure!(
            conllu.is_none() && annotate.is_none(),
            "corpus: {rest} is its own annotation — conllu= and annotate= apply to prose"
        );
        Ok(())
    };
    // A treebank is its own tokens: the cast table and the
    // spelling table still apply.
    let treebank_options = |mut m: quarb_text::TextModel| -> anyhow::Result<quarb_text::TextModel> {
        if let Some(t) = &modernize {
            m.set_orthography(t)
                .map_err(|e| anyhow::anyhow!("corpus: {e}"))?;
        }
        if let Some(f) = &cast {
            let rows = quarb_text::CastRow::read_csv(f)
                .map_err(|e| anyhow::anyhow!("corpus: reading the cast table {f}: {e}"))?;
            m.apply_cast(&rows);
        }
        if let Some(f) = &places {
            let rows = quarb_text::CastRow::read_csv(f)
                .map_err(|e| anyhow::anyhow!("corpus: reading the places table {f}: {e}"))?;
            m.apply_places(&rows);
        }
        Ok(m)
    };
    if matches!(ext.as_deref(), Some("conllu" | "conllup")) || format == Some("conllu") {
        own_annotation()?;
        let text = read_prose(target, ctx)?;
        let mut m = quarb_text::TextModel::parse_conllu_corpus(&text)
            .map_err(|e| anyhow::anyhow!("reading {rest} as CoNLL-U: {e}"))?;
        m.set_document_path(rest);
        return treebank_options(m);
    }
    // A directory of treebank files — a UD treebank's train / dev /
    // test, a release — reads as one corpus, a section per file.
    if target.is_dir() {
        own_annotation()?;
        let files = quarb_text::read_conllu_dir_with(target, &current_decoding(ctx)?)
            .map_err(|e| anyhow::anyhow!("corpus: over a directory reads its treebank — {e}"))?;
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        let mut m = quarb_text::TextModel::parse_conllu_corpus_set(&refs)
            .map_err(|e| anyhow::anyhow!("reading {rest} as CoNLL-U: {e}"))?;
        m.set_document_path(rest);
        return treebank_options(m);
    }
    let model = if matches!(ext.as_deref(), Some("atd" | "atk" | "usfm" | "sfm")) {
        koine_level(rest, ctx)?
    } else if let Some(kind) = binary_text_kind(target) {
        let bytes =
            std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
        binary_text_level(kind, &bytes, target, ctx)
            .with_context(|| format!("reading {}", target.display()))?
    } else {
        let text = read_prose(target, ctx)?;
        if is_xml(Some(target), &text) {
            koine_level(rest, ctx)?
        } else if ext.is_none() && quarb_text::looks_like_conllu(&text) {
            // A pipe or an extensionless file carrying CoNLL-U:
            // the treebank reading, as `?format=conllu` would say.
            own_annotation()?;
            let mut m = quarb_text::TextModel::parse_conllu_corpus(&text)
                .map_err(|e| anyhow::anyhow!("reading {rest} as CoNLL-U: {e}"))?;
            m.set_document_path(rest);
            return treebank_options(m);
        } else {
            let mut m = text_level(&text, Some(target), ctx)?;
            m.set_document_path(&target.display().to_string());
            m
        }
    };
    finish_corpus(
        model,
        conllu,
        annotate,
        &desm,
        cast,
        places,
        modernize,
        notes,
        allow_shell,
        "corpus",
        ctx,
    )
}

/// The corpus reading's second half, shared with `lit:`: the
/// sidecar or command annotation, else the tokenizer with the
/// session's and the mount's sentence bonds.
pub fn finish_corpus(
    mut model: quarb_text::TextModel,
    conllu: Option<String>,
    annotate: Option<String>,
    desm: &[String],
    cast: Option<String>,
    places: Option<String>,
    modernize: Option<String>,
    notes: Option<String>,
    allow_shell: bool,
    level: &str,
    ctx: &SourceCtx,
) -> anyhow::Result<quarb_text::TextModel> {
    match notes.as_deref() {
        None => {}
        Some("omit") => model.omit_notes(true),
        Some(v) => anyhow::bail!(
            "{level}: notes={v} — notes= takes omit (a note is part of the text by default)"
        ),
    }
    if let Some(table) = &modernize {
        model
            .set_orthography(table)
            .map_err(|e| anyhow::anyhow!("{level}: {e}"))?;
    }
    let conllu_text = match (conllu, annotate) {
        (Some(_), Some(_)) => anyhow::bail!("{level}: takes conllu= or annotate=, not both"),
        (Some(file), None) => Some(
            // A sidecar is a source too; its record stays its own.
            quarb_compose::read_source(Path::new(&file), ctx.encoding().as_deref())
                .with_context(|| format!("reading {file} as CoNLL-U"))?
                .map_err(|e| anyhow::anyhow!("reading {file} as CoNLL-U: {e}"))?
                .text,
        ),
        (None, Some(cmd)) => {
            anyhow::ensure!(
                allow_shell,
                "{level}: annotate= runs a command; pass --allow-shell to permit it"
            );
            Some(annotate_with(&cmd, &model.corpus_text())?)
        }
        (None, None) => None,
    };
    match conllu_text {
        Some(text) => model
            .annotate_conllu(&text)
            .map_err(|e| anyhow::anyhow!("{level}: {e}"))?,
        None => {
            // The session's bonds, plus this mount's own `?desm=` files.
            let mut bonds = quarb::sentence_bonds()
                .and_then(|b| b.as_any().downcast_ref::<syndesmos::Syndesmos>().cloned());
            for f in desm {
                let more = syndesmos::Syndesmos::load(f)
                    .map_err(|e| anyhow::anyhow!("reading {f}: {e}"))?;
                bonds
                    .get_or_insert_with(syndesmos::Syndesmos::empty)
                    .extend(more);
            }
            model.tokenize_with(bonds.as_ref());
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
    Ok(model)
}

/// The literary reading, `lit:` — litogramma, TEI, USX and OSIS
/// through atrep's importers with every inline simmere kept as a
/// node named by its sim, its genoses as traits and its aphanes
/// monosims as properties (`//quotation<said>[::prosopon =
/// "tom"]`), then the corpus reading's tokens beneath, each token
/// answering the spans that cover it. The same `?conllu=`,
/// `?annotate=`, `?desm=` options as `corpus:`; `?format=` names
/// the vocabulary (`tei`, `usx`, `osis`, `atd`) when the file
/// does not.
pub fn lit_level(
    rest: &str,
    allow_shell: bool,
    ctx: &SourceCtx,
) -> anyhow::Result<quarb_text::TextModel> {
    let (rest, conllu, annotate, desm, format, cast, places, scheme, modernize, notes) = match rest
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
                    Some(("format", v)) => format = Some(v.to_string()),
                    Some(("scheme", v)) => scheme = Some(v.to_string()),
                    Some(("modernize", v)) => modernize = Some(v.to_string()),
                    Some(("notes", v)) => notes = Some(v.to_string()),
                    Some(("encoding", v)) => set_encoding_option(v, ctx)?,
                    _ => anyhow::bail!(
                        "unknown lit option {pair:?} — supported: conllu=, annotate=, desm=, cast=, places=, format=, scheme=, modernize=, notes=, encoding="
                    ),
                }
            }
            (
                p, conllu, annotate, desm, format, cast, places, scheme, modernize, notes,
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
    if is_archive(target) && binary_text_kind(target).is_none() {
        anyhow::bail!(
            "lit: over an archive is not supported yet — mount it as text:{rest}, or lit: one member"
        );
    }
    // A word-processor file declares no literary vocabulary: its
    // prose reads under corpus:, its structure under text:.
    if let Some(kind) = binary_text_kind(target) {
        if kind == "dsl" {
            anyhow::bail!(
                "lit: reads TEI, USX, OSIS, USFM, FictionBook, RNC, OpenCorpora, PROIEL and litogramma — {rest} is a dictionary, no literary edition; koine:{rest} reads it (text:{rest} and corpus:{rest} alike)"
            );
        }
        let what = match kind {
            "docx" => "a Word document",
            "doc" => "a Word 97 document",
            "odt" => "an OpenDocument text",
            _ => "an EPUB",
        };
        anyhow::bail!(
            "lit: reads TEI, USX, OSIS, USFM, FictionBook, RNC, OpenCorpora, PROIEL and litogramma — {rest} is {what}, whose prose corpus:{rest} reads (text:{rest} its structure)"
        );
    }
    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let kind = match format.as_deref() {
        Some(k) => k.to_string(),
        None => match ext.as_deref() {
            Some("atd" | "atk") => "atd".to_string(),
            Some("usx") => "usx".to_string(),
            Some("usfm" | "sfm") => "usfm".to_string(),
            Some("fb2") => "fb2".to_string(),
            _ => {
                let text = read_prose(target, ctx)?;
                match quarb_text_koine::detect_xml_kind(&text) {
                    Some(k) => k.to_string(),
                    None if is_xml(Some(target), &text) => anyhow::bail!(
                        "lit: reads TEI, USX, OSIS, FictionBook, RNC, OpenCorpora, PROIEL and litogramma — {rest} declares none of them (koine:{rest} reads the other XML vocabularies)"
                    ),
                    None => anyhow::bail!(
                        "lit: reads TEI, USX, OSIS, FictionBook, RNC, OpenCorpora, PROIEL and litogramma — corpus:{rest} reads plain prose"
                    ),
                }
            }
        },
    };
    let mut model = match kind.as_str() {
        "atd" | "atk" => quarb_text_koine::parse_lit_file_with(target, scheme.as_deref())
            .with_context(|| format!("reading {} as an atrep document", target.display()))?,
        "tei" | "usx" | "osis" | "fb2" | "usfm" | "rnc" | "opencorpora" | "proiel" => {
            quarb_text_koine::parse_lit_xml_as_with(
                &read_prose(target, ctx)?,
                &kind,
                scheme.as_deref(),
            )
            .with_context(|| format!("reading {} as {}", target.display(), kind.to_uppercase()))?
        }
        other => anyhow::bail!(
            "lit: reads TEI, USX, OSIS, USFM, RNC, OpenCorpora, PROIEL and litogramma — not {other} (koine:{rest} reads the other XML vocabularies)"
        ),
    };
    model.set_document_path(rest);
    finish_corpus(
        model,
        conllu,
        annotate,
        &desm,
        cast,
        places,
        modernize,
        notes,
        allow_shell,
        "lit",
        ctx,
    )
}

/// Run an annotator command under `sh -c` with `text` on its
/// stdin, returning its stdout — CoNLL-U, by the contract of
/// `corpus:…?annotate=`.
pub fn annotate_with(cmd: &str, text: &str) -> anyhow::Result<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("running annotator {cmd:?}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .context("feeding the annotator")?;
    }
    let out = child
        .wait_with_output()
        .context("waiting for the annotator")?;
    anyhow::ensure!(
        out.status.success(),
        "annotator {cmd:?} failed: {}",
        out.status
    );
    String::from_utf8(out.stdout).context("annotator output is not UTF-8")
}

/// A prose document's text: the file, or standard input for `-`
/// (`text:-`, `corpus:-` — the pipe reading `text:/dev/stdin`
/// spelled the Unix way), a leading BOM dropped.
/// Read a source's text at the source boundary: `?encoding=` on
/// the target, then a byte-order mark, then what the bytes declare
/// (an XML prologue, an HTML meta charset), then strict UTF-8 —
/// refused, naming the option, when none applies. The record of
/// the decoding waits for `run` / `open_mount` to put it on the
/// root. Standard input (`-`) is a source like any other.
pub fn read_prose(target: &Path, ctx: &SourceCtx) -> anyhow::Result<String> {
    let explicit = ctx.encoding();
    let decoded = if target == Path::new("-") {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .context("reading standard input")?;
        quarb_compose::source_decoding(explicit.as_deref())
            .and_then(|d| d.decode("-", &bytes))
            .map_err(|e| anyhow::anyhow!("reading standard input: {e}"))?
    } else {
        quarb_compose::read_source(target, explicit.as_deref())
            .with_context(|| format!("reading {}", target.display()))?
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", target.display()))?
    };
    ctx.set_record(decoded.encoding, decoded.replaced);
    Ok(decoded.text)
}

/// Take `?encoding=LABEL` off a plain target (one that is not a
/// path as written): the one option a file or directory mount
/// takes. The label is validated here, before anything is read.
/// Prefixed readers (`text:`, `corpus:`, `lit:`, `koine:`) parse
/// the option among their own.
pub fn take_encoding_option(p: &Path, ctx: &SourceCtx) -> anyhow::Result<PathBuf> {
    ctx.clear_encoding();
    ctx.set_csv(quarb_csv::CsvOptions::default());
    let Some(s) = p.to_str() else {
        return Ok(p.to_path_buf());
    };
    // A scheme or reader prefix (`text:`, `imap://`, `lines:`)
    // parses its own options; a path as written is taken as is.
    let prefixed = s.split_once(':').is_some_and(|(pre, _)| {
        !pre.is_empty()
            && pre
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    if p.exists() || !s.contains('?') || prefixed {
        return Ok(p.to_path_buf());
    }
    let (path, query) = s.rsplit_once('?').unwrap();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some(("encoding", label)) => {
                quarb::source::encoding_for_label(label).ok_or_else(|| {
                    anyhow::anyhow!("{}: unknown encoding label {label:?}", p.display())
                })?;
                ctx.set_encoding(label);
            }
            Some(("delimiter", label)) => set_csv_option(|c| c.with_delimiter_label(label), ctx)?,
            Some(("decimal", label)) => set_csv_option(|c| c.with_decimal_label(label), ctx)?,
            _ => anyhow::bail!(
                "unknown option {pair:?} on {path} — a file or folder takes encoding=, \
                 delimiter=, decimal="
            ),
        }
    }
    Ok(PathBuf::from(path))
}

/// `delimiter=` / `decimal=` as read off a target: validated here,
/// before anything is read.
pub fn set_csv_option(
    f: impl FnOnce(quarb_csv::CsvOptions) -> Result<quarb_csv::CsvOptions, quarb_csv::CsvError>,
    ctx: &SourceCtx,
) -> anyhow::Result<()> {
    let next = f(ctx.csv()).map_err(|e| anyhow::anyhow!("{e}"))?;
    ctx.set_csv(next);
    Ok(())
}

/// The delimited-file options in force for the target being
/// opened (`?delimiter=` / `?decimal=`), for a front-end that
/// composes its own adapter with them.
pub fn csv_options(ctx: &SourceCtx) -> quarb_csv::CsvOptions {
    ctx.csv()
}

/// Put the decoding record, if a read made one, on the adapter's
/// root: `::::encoding` and `::::replaced` answer there.
pub fn take_source_record(ctx: &SourceCtx) -> Option<(String, usize)> {
    ctx.take_record()
}

/// `DIR[?options]` when the target is a folder of documents: a
/// directory that holds no treebank. The options ride to every
/// document in it.
pub fn document_dir(rest: &str) -> Option<(&Path, Option<String>)> {
    let (path, opts) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q.to_string())),
        None => (rest, None),
    };
    let dir = Path::new(path);
    (dir.is_dir() && !holds_treebank(dir) && holds_documents(dir)).then_some((dir, opts))
}

/// Whether a prose document lies anywhere beneath a directory — a
/// folder with neither a treebank nor a document is refused by the
/// treebank reading, naming what it looked for.
pub fn holds_documents(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(|e| e.ok().map(|e| e.path())).any(|p| {
        if p.is_dir() {
            return holds_documents(&p);
        }
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        matches!(
            ext.as_deref(),
            Some(
                "md" | "markdown"
                    | "html"
                    | "htm"
                    | "txt"
                    | "tex"
                    | "xml"
                    | "fb2"
                    | "djvu"
                    | "rst"
                    | "org"
                    | "atd"
                    | "atk"
                    | "usfm"
                    | "sfm"
            )
        ) || binary_text_kind(&p).is_some()
    })
}

/// A folder of documents as one arbor: the folder tree, subfolders
/// walked, each document leaf read by `reader` (or, where it
/// declines, at the text level when the format is one the composed
/// view parses itself).
pub fn document_folder(
    dir: &Path,
    hidden: bool,
    no_ignore: bool,
    reader: quarb_compose::DocumentReader,
    ctx: &SourceCtx,
) -> anyhow::Result<ComposeAdapter<FsAdapter>> {
    let opts = FsOptions {
        hidden,
        respect_ignore: !no_ignore,
    };
    Ok(
        ComposeAdapter::with_source_paths(FsAdapter::with_options(dir, opts)?, |fs, n| {
            Some(fs.path(n))
        })
        .with_document_graft(DocumentGraft::Text)
        .with_document_reader(reader)
        .with_decoding(current_decoding(ctx)?)
        .with_csv_options(ctx.csv()),
    )
}

/// The reader a folder mount hands its document leaves to, by the
/// prefix's reading: `text` (the formats the composed view does not
/// parse itself), `corpus` (every prose format, with its tokens and
/// sentences), `lit` (the marked-up editions). A file the reading
/// does not take, or cannot read, stays a plain leaf.
pub fn folder_reader(
    level: &'static str,
    opts: Option<String>,
    allow_shell: bool,
    ctx: &SourceCtx,
) -> quarb_compose::DocumentReader {
    // The reader outlives the open: it carries its own copy of the
    // boundary (the options are what matter; a leaf's record stays
    // with its graft).
    let ctx = ctx.clone();
    Rc::new(move |path: &Path| {
        let ctx = &ctx;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())?;
        let spec = match &opts {
            Some(q) => format!("{}?{q}", path.display()),
            None => path.display().to_string(),
        };
        let edition = matches!(ext.as_str(), "atd" | "atk" | "usfm" | "sfm" | "fb2");
        match level {
            "corpus" => {
                let prose = matches!(
                    ext.as_str(),
                    "md" | "markdown"
                        | "html"
                        | "htm"
                        | "txt"
                        | "tex"
                        | "xml"
                        | "rst"
                        | "org"
                        | "rtf"
                ) || edition
                    || binary_text_kind(path).is_some();
                prose
                    .then(|| corpus_level(&spec, allow_shell, ctx).ok())
                    .flatten()
            }
            "lit" => (edition || ext == "xml")
                .then(|| lit_level(&spec, allow_shell, ctx).ok())
                .flatten(),
            _ => {
                if edition {
                    koine_level(&spec, ctx).ok()
                } else if let Some(kind) = binary_text_kind(path) {
                    binary_text_level(kind, &std::fs::read(path).ok()?, path, ctx).ok()
                } else if matches!(ext.as_str(), "tex" | "rst" | "org" | "rtf") {
                    let mut m = text_level(&read_prose(path, ctx).ok()?, Some(path), ctx).ok()?;
                    m.set_document_path(&path.display().to_string());
                    Some(m)
                } else {
                    None
                }
            }
        }
    })
}

/// Whether a directory holds a treebank — a `.conllu` file anywhere
/// beneath it. `text:DIR` reads the treebank when there is one, and
/// the folder's documents otherwise.
pub fn holds_treebank(dir: &Path) -> bool {
    quarb_text::read_conllu_dir(dir).is_ok_and(|files| !files.is_empty())
}

/// A directory's treebank files as one document (`text:DIR`): the
/// `.conllu` files beneath it, a level-1 section each, without
/// tokens — `corpus:DIR` is the same set with them.
pub fn conllu_dir_text(dir: &Path, ctx: &SourceCtx) -> anyhow::Result<quarb_text::TextModel> {
    let files = quarb_text::read_conllu_dir_with(dir, &current_decoding(ctx)?)
        .map_err(|e| anyhow::anyhow!("text: over a directory reads its treebank — {e}"))?;
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let mut m = quarb_text::TextModel::parse_conllu_text_set(&refs)
        .map_err(|e| anyhow::anyhow!("reading {} as CoNLL-U: {e}", dir.display()))?;
    m.set_document_path(&dir.display().to_string());
    Ok(m)
}

pub fn text_level(
    text: &str,
    path: Option<&Path>,
    ctx: &SourceCtx,
) -> anyhow::Result<quarb_text::TextModel> {
    apply_references(text_level_inner(text, path)?, ctx)
}

pub fn text_level_inner(text: &str, path: Option<&Path>) -> anyhow::Result<quarb_text::TextModel> {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    Ok(match ext.as_deref() {
        // A treebank read as a document: sections,
        // paragraphs and sentences from its comments.
        Some("conllu" | "conllup") => quarb_text::TextModel::parse_conllu_text(text)
            .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?,
        Some("html" | "htm") => quarb_text_html::parse(text),
        Some("md" | "markdown") => quarb_text_markdown::parse(text),
        Some("tex" | "latex") => quarb_text_latex::parse(text),
        Some("txt") => quarb_text::TextModel::parse_plain(text),
        // Rich Text: the declared outline levels, lists, tables,
        // notes and fields, its `\'xx` bytes in the code page the
        // header names.
        Some("rtf") => quarb_text_rtf::parse(text.as_bytes())
            .map_err(|e| anyhow::anyhow!("reading RTF: {e}"))?,
        // Scripture in USFM: through atrep's importer, as the
        // koine route reads it.
        Some("usfm" | "sfm") => quarb_text_koine::parse_xml_as(text, "usfm")
            .map_err(|e| anyhow::anyhow!("reading USFM: {e}"))?,
        // FictionBook: by its extension, or an `.xml` declaring the
        // FictionBook namespace — through the koine route.
        Some("fb2") => quarb_text_koine::parse_xml_as(text, "fb2")
            .map_err(|e| anyhow::anyhow!("reading FictionBook: {e}"))?,
        // An XML vocabulary (TEI, DocBook, OSIS, FictionBook, …)
        // reads through the koine route by its declared identity,
        // as `corpus:` and `lit:` read it; a `.xml` declaring none
        // is refused with the pointer rather than read as HTML.
        Some("xml" | "tei") => match quarb_text_koine::detect_xml_kind(text) {
            Some(kind) => quarb_text_koine::parse_xml_as(text, kind)
                .map_err(|e| anyhow::anyhow!("reading {kind} through the koine route: {e}"))?,
            None => anyhow::bail!(
                "the file declares no XML identity the text level knows (no namespace, \
                 DOCTYPE, or unambiguous root) — name the vocabulary with \
                 koine:FILE?format=tei|docbook|jats|usx|osis|fb2|rnc|opencorpora|proiel, \
                 or read the markup itself with xml:FILE"
            ),
        },
        _ if text.trim_start().starts_with('<')
            && let Some(kind) = quarb_text_koine::detect_xml_kind(text) =>
        {
            quarb_text_koine::parse_xml_as(text, kind)
                .map_err(|e| anyhow::anyhow!("reading {kind} through the koine route: {e}"))?
        }
        _ if text.trim_start().starts_with('<') => quarb_text_html::parse(text),
        // A pipe or an extensionless file carrying CoNLL-U.
        None if quarb_text::looks_like_conllu(text) => {
            quarb_text::TextModel::parse_conllu_text(text)
                .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?
        }
        _ => quarb_text::TextModel::parse_plain(text),
    })
}

/// An input argument's explicit mount alias: `NAME=TARGET` mounts
/// TARGET as `/NAME`. The prefix must look like a mount name (a
/// letter or `_`, then letters, digits, `_`, `-`; letters in any
/// script, so `достоевский=corpus:…` names its mount as a Russian
/// page would) and the argument must not name an existing file —
/// a real file called `a=b.json` still mounts by its stem.
pub fn split_alias(p: &Path) -> Option<(String, PathBuf)> {
    let s = p.to_str()?;
    let (name, target) = s.split_once('=')?;
    if target.is_empty() || p.exists() {
        return None;
    }
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') {
        return None;
    }
    if !chars.all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    Some((name.to_string(), PathBuf::from(target)))
}

/// A boxed adapter and its locator renderer, ready to mount.
/// What opening a target yields: the adapter behind the object-safe
/// trait, its locator renderer, and the adapter family it was
/// mounted through (`json`, `text`, `fs`, `sqlite`, `neo4j`, …),
/// the name a trace payload's meta carries.
pub struct Mounted {
    pub adapter: Box<dyn AstAdapter>,
    pub render: Box<dyn Fn(NodeId) -> String>,
    pub family: &'static str,
}

impl Mounted {
    /// Share `a` as the mounted adapter; the family is read off the
    /// adapter's type (its crate's name after `quarb-`, the inner
    /// type of a composed adapter).
    pub fn of<A: AstAdapter + 'static>(a: Rc<A>, render: Box<dyn Fn(NodeId) -> String>) -> Mounted {
        Mounted {
            adapter: Box::new(Shared(a)),
            render,
            family: family_of::<A>(),
        }
    }
}

/// The adapter family of a concrete adapter type: the last
/// `quarb_*` crate named in its type (so a composed adapter answers
/// for what it wraps), hyphenated, with the property-model readers
/// (YAML, TOML) under `json` as the session has always named them.
pub fn family_of<A: 'static>() -> &'static str {
    let name = std::any::type_name::<A>();
    let last = name
        .rmatch_indices("quarb")
        .next()
        .map(|(i, _)| &name[i..])
        .unwrap_or(name);
    let krate = last.split("::").next().unwrap_or(last);
    match krate {
        "quarb" => "quarb",
        "quarb_yaml" | "quarb_toml" | "quarb_cbor" => "json",
        "quarb_text_pdf" | "quarb_text_koine" | "quarb_text_djvu" => "text",
        other => Box::leak(
            other
                .strip_prefix("quarb_")
                .unwrap_or(other)
                .replace('_', "-")
                .into_boxed_str(),
        ),
    }
}

/// Mount a relational adapter with the same JSON-column graft the
/// single-input flow applies: `run_relational` wraps every
/// relational adapter in `ComposeAdapter`, and a mount must too —
/// otherwise a text column full of JSON navigates as a subtree in
/// one flow and comes back flat in the other. Under --no-graft
/// both flows skip the wrap.
pub fn mounted_relational<A: AstAdapter + 'static>(
    no_graft: bool,
    inner: A,
    outer_loc: impl Fn(&A, NodeId) -> String + 'static,
) -> Mounted {
    if no_graft {
        let a = Rc::new(inner);
        let r = a.clone();
        return Mounted::of(a, Box::new(move |n| outer_loc(&r, n)));
    }
    let a = Rc::new(ComposeAdapter::new(inner));
    let r = a.clone();
    Mounted::of(
        a,
        Box::new(move |n| r.locator(n, |o| outer_loc(r.outer(), o))),
    )
}

/// What opening a target consults — the subset of the CLI's flags
/// every front-end can supply.
#[derive(Default)]
pub struct OpenOpts {
    pub hidden: bool,
    pub no_ignore: bool,
    /// Opt directory mounts into grafting (the CLI's --graft).
    pub graft: bool,
    /// Disable grafting entirely (the CLI's --no-graft).
    pub no_graft: bool,
    pub refs: Option<PathBuf>,
    /// Let `sh(...)` / backtick stages run (the CLI's --allow-shell);
    /// the literary and corpus readings' `annotate=` needs it.
    pub allow_shell: bool,
}

/// Open any target qua speaks — filesystem paths, documents, and
/// the full adapter-scheme fleet (`gcl:`, `kafka:`, `neo4j://`,
/// …) — as a boxed adapter plus its locator renderer. The door
/// session tools use to mount what the CLI mounts.
pub fn open_target(target: &str, opts: &OpenOpts) -> anyhow::Result<Mounted> {
    open_mount(Path::new(target), opts)
}

/// Mount kaiv text by its extension's pipeline stage: `.kaiv` is
/// canonical, `.daiv` is authored (compile + denormalize), `.raiv`
/// is relational (denormalize). The file's directory anchors the
/// resolver, so `.!units` / `.!types` imports (and a sibling
/// `kaiv.kaiv`) resolve exactly as `kaiv build` there would.
pub fn parse_kaiv_ext(
    ext: &str,
    text: &str,
    dir: Option<&Path>,
) -> anyhow::Result<quarb_kaiv::KaivAdapter> {
    let parsed = match ext {
        "kaiv" => quarb_kaiv::KaivAdapter::parse_kaiv_at(text, dir),
        "raiv" => quarb_kaiv::KaivAdapter::parse_raiv_at(text, dir),
        _ => quarb_kaiv::KaivAdapter::parse_daiv_at(text, dir),
    };
    parsed.map_err(|e| anyhow::anyhow!("parsing {ext}: {e}"))
}

/// Open one input as a boxed adapter plus its locator renderer, for
/// mounting. Format detection matches the single-input flow. The
/// source boundary is applied here: `?encoding=` comes off the
/// target first, and the decoding record a read leaves goes on the
/// mount's root.
pub fn open_mount(p: &Path, cli: &OpenOpts) -> anyhow::Result<Mounted> {
    let ctx = &SourceCtx::default();
    let p = take_encoding_option(p, ctx)?;
    let _ = ctx.take_record();
    let m = open_mount_inner(&p, cli, ctx)?;
    ctx.clear_encoding();
    Ok(match take_source_record(ctx) {
        Some((encoding, replaced)) => Mounted {
            adapter: Box::new(quarb::source::Sourced {
                inner: m.adapter,
                encoding,
                replaced,
            }),
            render: m.render,
            family: m.family,
        },
        None => m,
    })
}

pub fn open_mount_inner(p: &Path, cli: &OpenOpts, ctx: &SourceCtx) -> anyhow::Result<Mounted> {
    if p.is_dir() {
        let opts = FsOptions {
            hidden: cli.hidden,
            respect_ignore: !cli.no_ignore,
        };
        if cli.graft {
            let a = Rc::new(
                ComposeAdapter::with_source_paths(FsAdapter::with_options(p, opts)?, |fs, n| {
                    Some(fs.path(n))
                })
                .with_decoding(current_decoding(ctx)?)
                .with_csv_options(ctx.csv()),
            );
            let r = a.clone();
            return Ok(Mounted::of(
                a,
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        let a = Rc::new(FsAdapter::with_options(p, opts)?);
        let r = a.clone();
        return Ok(Mounted::of(
            a,
            Box::new(move |n| r.path(n).display().to_string()),
        ));
    }
    if let Some(s) = p.to_str()
        && let Some(cmd) = s.strip_prefix("serve:")
    {
        let a = Rc::new(ServeAdapter::spawn(cmd).context("spawning served adapter")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // A `lines:` prefix mounts line atoms, matching the
    // single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("lines:")
        && !rest.is_empty()
    {
        let rest = &take_reader_encoding(rest, ctx)?;
        let target = Path::new(rest);
        let text = read_prose(target, ctx)?;
        let a = Rc::new(quarb_lines::LinesAdapter::parse(&text));
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // A `koine:` prefix takes the koine route, matching the
    // single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("koine:")
        && !rest.is_empty()
    {
        let a = Rc::new(koine_level(rest, ctx)?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("web:")
        && !rest.is_empty()
    {
        match web_level(rest, ctx)? {
            WebSite::Memory(a) => {
                let a = Rc::new(a);
                let r = a.clone();
                return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
            }
            WebSite::Sqlite(store) => {
                let a = Rc::new(quarb_web::WebAdapter::new(store));
                let r = a.clone();
                return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
            }
            WebSite::Postgres(store) => {
                let a = Rc::new(quarb_web::WebAdapter::new(store));
                let r = a.clone();
                return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
            }
        }
    }
    // The literary reading, matching the single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("lit:")
        && !rest.is_empty()
    {
        if let Some((dir, opts)) = document_dir(rest) {
            let a = Rc::new(document_folder(
                dir,
                cli.hidden,
                cli.no_ignore,
                folder_reader("lit", opts, cli.allow_shell, ctx),
                ctx,
            )?);
            let r = a.clone();
            return Ok(Mounted::of(
                a,
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        let a = Rc::new(lit_level(rest, cli.allow_shell, ctx)?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // The property reading (`json2:`), matching the single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("json2:")
        && !rest.is_empty()
    {
        let rest = &take_reader_encoding(rest, ctx)?;
        return Ok(match property_reading(Path::new(rest), ctx)? {
            PropertyDoc::Json(adapter) => {
                let a = Rc::new(adapter);
                let r = a.clone();
                Mounted::of(a, Box::new(move |n| r.pointer(n)))
            }
            PropertyDoc::Cbor(adapter) => {
                let a = Rc::new(adapter);
                let r = a.clone();
                Mounted::of(a, Box::new(move |n| r.pointer(n)))
            }
        });
    }
    // The corpus reading, matching the single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("corpus:")
        && !rest.is_empty()
    {
        if let Some((dir, opts)) = document_dir(rest) {
            let a = Rc::new(document_folder(
                dir,
                cli.hidden,
                cli.no_ignore,
                folder_reader("corpus", opts, cli.allow_shell, ctx),
                ctx,
            )?);
            let r = a.clone();
            return Ok(Mounted::of(
                a,
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        let a = Rc::new(corpus_level(rest, cli.allow_shell, ctx)?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // A `text:` prefix forces the text-level reading, matching the
    // single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("text:")
        && !rest.is_empty()
    {
        let rest = &take_reader_encoding(rest, ctx)?;
        let target = Path::new(rest);
        // A directory of documents: the folder tree, each document
        // leaf read at the text level.
        if target.is_dir() && !holds_treebank(target) {
            let a = Rc::new(document_folder(
                target,
                cli.hidden,
                cli.no_ignore,
                folder_reader("text", None, cli.allow_shell, ctx),
                ctx,
            )?);
            let r = a.clone();
            return Ok(Mounted::of(
                a,
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        // A directory of treebank files: one document, a section
        // per file.
        if target.is_dir() {
            let a = Rc::new(conllu_dir_text(target, ctx)?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        // An archive at the text level, as on the single flow:
        // every marked-up member grafts as the reader's model.
        if is_archive(target) && binary_text_kind(target).is_none() {
            let a = Rc::new(
                ComposeAdapter::new(
                    ArchiveAdapter::open_with_decoding(target, current_decoding(ctx)?)
                        .context("opening archive")?,
                )
                .with_decoding(current_decoding(ctx)?)
                .with_csv_options(ctx.csv())
                .with_document_graft(DocumentGraft::Text),
            );
            let r = a.clone();
            return Ok(Mounted::of(
                a,
                Box::new(move |n| r.locator(n, |o| r.outer().locator(o))),
            ));
        }
        // Native atrep files converge on the koine route.
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("atd") || e.eq_ignore_ascii_case("atk"))
        {
            let a = Rc::new(koine_level(rest, ctx)?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            let bytes =
                std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
            let a = Rc::new(
                quarb_text_pdf::parse(&bytes)
                    .with_context(|| format!("reading {} as PDF", target.display()))?,
            );
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        if is_djvu(target) {
            let a = Rc::new(
                quarb_text_djvu::parse_file(target)
                    .with_context(|| format!("reading {} as DjVu", target.display()))?,
            );
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        if let Some(kind) = binary_text_kind(target) {
            let bytes =
                std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
            let a = Rc::new(
                binary_text_level(kind, &bytes, target, ctx)
                    .with_context(|| format!("reading {}", target.display()))?,
            );
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        // BibTeX / BibLaTeX: atrep's importer parses it into
        // bibliogramma, and the entries land as bib blocks.
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("bib"))
        {
            let text = read_prose(target, ctx)?;
            let a = Rc::new(quarb_text_koine::parse_bibtex(&text).with_context(|| {
                format!(
                    "reading {} as BibTeX through bibliogramma",
                    target.display()
                )
            })?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        let text = read_prose(target, ctx)?;
        let text = match text.strip_prefix('\u{feff}') {
            Some(rest) => rest.to_owned(),
            None => text,
        };
        let a = Rc::new(text_level(&text, Some(target), ctx)?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // A `code:` prefix forces the code-level reading, matching
    // the single-input flow; a directory mounts the composed,
    // grafted view with source leaves grafted at the code
    // level.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("code:")
        && !rest.is_empty()
    {
        let rest = &take_reader_encoding(rest, ctx)?;
        if cli.no_graft {
            anyhow::bail!(
                "--no-graft refuses the code: prefix: the prefix's whole \
                 meaning is the grafted code-level view"
            );
        }
        let target = Path::new(rest);
        if target.is_dir() {
            let opts = FsOptions {
                hidden: cli.hidden,
                respect_ignore: !cli.no_ignore,
            };
            let a = Rc::new(
                ComposeAdapter::with_source_paths(
                    FsAdapter::with_options(target, opts)?,
                    |fs, n| Some(fs.path(n)),
                )
                .with_source_graft(SourceGraft::Code)
                .with_decoding(current_decoding(ctx)?)
                .with_csv_options(ctx.csv()),
            );
            let r = a.clone();
            return Ok(Mounted::of(
                a,
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        let a = Rc::new(
            quarb_code::CodeModel::open_with_decoding(target, &current_decoding(ctx)?)
                .with_context(|| format!("parsing {} at the code level", target.display()))?,
        );
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("firestore://")
    {
        let a = Rc::new(FirestoreAdapter::connect(s).context("connecting to Firestore")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("datastore://")
    {
        let a = Rc::new(DatastoreAdapter::connect(s).context("connecting to Datastore")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    #[cfg(feature = "heavy")]
    if let Some(s) = p.to_str()
        && s.starts_with("mssql://")
    {
        return Ok(mounted_relational(
            cli.no_graft,
            MssqlAdapter::connect(s).context("connecting to SQL Server")?,
            |a, n| a.locator(n),
        ));
    }
    #[cfg(feature = "heavy")]
    if let Some(s) = p.to_str()
        && s.starts_with("oracle://")
    {
        return Ok(mounted_relational(
            cli.no_graft,
            OracleAdapter::connect(s).context("connecting to Oracle")?,
            |a, n| a.locator(n),
        ));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("ldap://") || s.starts_with("ldaps://"))
    {
        let a = Rc::new(LdapAdapter::connect(s).context("connecting to LDAP")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("github:")
    {
        let a = Rc::new(GithubAdapter::connect(s).context("connecting to GitHub")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("gitlab:")
    {
        let a = Rc::new(GitlabAdapter::connect(s).context("connecting to GitLab")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("k8s:") || s.starts_with("kubernetes:"))
    {
        let a = Rc::new(KubernetesAdapter::connect(s).context("connecting to Kubernetes")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("gcl:") || s.starts_with("gcplogs:"))
    {
        let a = Rc::new(GclAdapter::open(s).context("reading Cloud Logging")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("cwl:") || s.starts_with("cloudwatch:"))
    {
        let a = Rc::new(CwlAdapter::open(s).context("reading CloudWatch Logs")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("ddl:") || s.starts_with("datadog:"))
    {
        let a = Rc::new(DdlAdapter::open(s).context("reading Datadog Logs")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("azl:") || s.starts_with("azlogs:"))
    {
        let a = Rc::new(AzlAdapter::open(s).context("reading Azure Monitor Logs")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("cfl:") || s.starts_with("cflogs:"))
    {
        let a = Rc::new(CflAdapter::open(s).context("reading Cloudflare logs")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    #[cfg(feature = "heavy")]
    if let Some(s) = p.to_str()
        && (s.starts_with("mongodb://") || s.starts_with("mongodb+srv://"))
    {
        let a = Rc::new(MongodbAdapter::connect(s).context("connecting to MongoDB")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("neo4j://")
    {
        let a = Rc::new(Neo4jAdapter::connect(s).context("connecting to Neo4j")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("dynamodb:")
    {
        let a = Rc::new(DynamodbAdapter::connect(s).context("connecting to DynamoDB")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("neptune://")
    {
        let a = Rc::new(NeptuneAdapter::connect(s).context("connecting to Neptune")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("redis://")
    {
        let a = Rc::new(RedisAdapter::connect(s).context("connecting to Redis")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("rediss://")
    {
        let a = Rc::new(RedisAdapter::connect(s).context("connecting to Redis")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("falkor://")
    {
        let a = Rc::new(FalkorAdapter::connect(s).context("connecting to FalkorDB")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("memgraph://")
    {
        let a = Rc::new(MemgraphAdapter::connect(s).context("connecting to Memgraph")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("arango://")
    {
        let a = Rc::new(ArangoAdapter::connect(s).context("connecting to ArangoDB")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("sparql:")
    {
        let a = Rc::new(SparqlAdapter::connect(s).context("connecting to the SPARQL endpoint")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("age://")
    {
        let a = Rc::new(AgeAdapter::connect(s).context("connecting to AGE")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    #[cfg(feature = "kuzu")]
    if let Some(s) = p.to_str()
        && s.starts_with("kuzu:")
    {
        let a = Rc::new(KuzuAdapter::open(s).context("opening Kuzu database")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("kafka:")
    {
        let a = Rc::new(KafkaAdapter::connect(s).context("connecting to Kafka")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("cosmos://")
    {
        let a = Rc::new(CosmosAdapter::connect(s).context("connecting to Cosmos DB")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    #[cfg(feature = "heavy")]
    if let Some(s) = p.to_str()
        && s.starts_with("athena:")
    {
        return Ok(mounted_relational(
            cli.no_graft,
            AthenaAdapter::connect(s).context("connecting to Athena")?,
            |a, n| a.locator(n),
        ));
    }
    if let Some(s) = p.to_str()
        && let Some(repo) = s.strip_prefix("git:")
    {
        let a = Rc::new(
            GitAdapter::open_with_decoding(std::path::Path::new(repo), current_decoding(ctx)?)
                .context("opening git repository")?,
        );
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && let Some(vault) = s
            .strip_prefix("metatheca:")
            .or_else(|| s.strip_prefix("mt:"))
    {
        let a = Rc::new(
            MetathecaAdapter::open_with_decoding(
                std::path::Path::new(vault),
                current_decoding(ctx)?,
            )
            .context("opening metatheca vault")?,
        );
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("firebase://")
    {
        let adapter = match &cli.refs {
            Some(f) => {
                let text = std::fs::read_to_string(f)
                    .with_context(|| format!("reading refs file {}", f.display()))?;
                let refs = quarb_firebase::parse_refs(&text).context("parsing refs")?;
                FirebaseAdapter::connect_with_refs(s, refs)
            }
            None => FirebaseAdapter::connect(s),
        }
        .context("connecting to Firebase")?;
        let a = Rc::new(adapter);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    #[cfg(feature = "heavy")]
    if let Some(s) = p.to_str()
        && s.starts_with("bigquery://")
    {
        return Ok(mounted_relational(
            cli.no_graft,
            BigqueryAdapter::connect(s).context("connecting to BigQuery")?,
            |a, n| a.locator(n),
        ));
    }
    #[cfg(feature = "heavy")]
    if let Some(s) = p.to_str()
        && s.starts_with("mysql://")
    {
        return Ok(mounted_relational(
            cli.no_graft,
            MysqlAdapter::connect(s).context("connecting to MySQL")?,
            |a, n| a.locator(n),
        ));
    }
    if let Some(s) = p.to_str()
        && is_pg_config(s)
    {
        return Ok(mounted_relational(
            cli.no_graft,
            PostgresAdapter::connect(s).context("connecting to PostgreSQL")?,
            |a, n| a.locator(n),
        ));
    }
    if let Some(t) = p.to_str()
        && let Some(mb) = t.strip_prefix("mail:")
    {
        let a = Rc::new(
            MaildirAdapter::open_with_decoding(std::path::Path::new(mb), mail_decoding(ctx)?)
                .context("opening mailbox")?,
        );
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(t) = p.to_str()
        && t.starts_with("gsheet://")
    {
        let a = Rc::new(GsheetAdapter::connect(t).context("connecting to Google Sheets")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(t) = p.to_str()
        && (t.starts_with("gs://") || t.starts_with("s3://") || t.starts_with("az://"))
    {
        if cli.no_graft {
            let a = Rc::new(ObjstoreAdapter::connect(t).context("connecting to bucket")?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        let a = Rc::new(
            ComposeAdapter::new(ObjstoreAdapter::connect(t).context("connecting to bucket")?)
                .with_decoding(current_decoding(ctx)?)
                .with_csv_options(ctx.csv()),
        );
        let r = a.clone();
        return Ok(Mounted::of(
            a,
            Box::new(move |n| r.locator(n, |o| r.outer().locator(o))),
        ));
    }
    if let Some(t) = p.to_str()
        && (t.starts_with("imap://") || t.starts_with("imaps://"))
    {
        let a = Rc::new(
            ImapAdapter::connect_with_decoding(t, mail_decoding(ctx)?)
                .context("connecting to IMAP")?,
        );
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // Source code: files with a tree-sitter grammar parse into
    // their syntax tree, matching the single-input flow.
    if p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| quarb_tree_sitter::supported(&e.to_ascii_lowercase()))
    {
        let a = Rc::new(TreeSitterAdapter::open(p).context("parsing source file")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // Spreadsheets before the archive check — .xlsx/.ods ARE zips (PK
    // magic), but the sheets are the point, not the raw XML entries.
    if let Some(ext) = p.extension().and_then(|e| e.to_str())
        && matches!(ext.to_ascii_lowercase().as_str(), "xlsx" | "xls" | "ods")
    {
        let a = Rc::new(XlsxAdapter::open(p).context("opening workbook")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // DuckDB databases, by extension.
    #[cfg(feature = "heavy")]
    if p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("duckdb") || e.eq_ignore_ascii_case("ddb"))
    {
        return Ok(mounted_relational(
            cli.no_graft,
            DuckdbAdapter::open(p).context("opening DuckDB database")?,
            |a, n| a.locator(n),
        ));
    }
    if is_djvu(p) {
        anyhow::bail!(
            "{}: a DjVu reads at the text level — text:{}",
            p.display(),
            p.display()
        );
    }
    if p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        let a = Rc::new(
            quarb_pdf::PdfAdapter::load(&bytes)
                .with_context(|| format!("reading {} as PDF", p.display()))?,
        );
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if is_archive(p) {
        if cli.no_graft {
            let a = Rc::new(
                ArchiveAdapter::open_with_decoding(p, current_decoding(ctx)?)
                    .context("opening archive")?,
            );
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        let a = Rc::new(
            ComposeAdapter::new(
                ArchiveAdapter::open_with_decoding(p, current_decoding(ctx)?)
                    .context("opening archive")?,
            )
            .with_decoding(current_decoding(ctx)?)
            .with_csv_options(ctx.csv()),
        );
        let r = a.clone();
        return Ok(Mounted::of(
            a,
            Box::new(move |n| r.locator(n, |o| r.outer().locator(o))),
        ));
    }
    // CBOR is binary: dispatch on the extension before the text
    // read, matching the single-input flow.
    if p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cbor"))
    {
        let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        let a = Rc::new(quarb_cbor::CborAdapter::parse(&bytes).context("parsing CBOR")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.pointer(n))));
    }
    if is_sqlite(p) {
        let refs = relational_refs(&cli.refs)?;
        return Ok(mounted_relational(
            cli.no_graft,
            SqliteAdapter::open_with_refs(p, &refs).context("opening SQLite database")?,
            |a, n| a.locator(n),
        ));
    }
    let text = read_prose(p, ctx)?;
    // Strip a leading UTF-8 BOM, as the single-input flow does: it
    // breaks JSON parsing and slips past the XML/HTML sniffers.
    let text = match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    };
    let path = Some(p);
    if is_quarb(path) {
        let a = Rc::new(quarb::reflect::QueryArbor::parse(&text).context("parsing Quarb query")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str())
        && matches!(ext.to_ascii_lowercase().as_str(), "daiv" | "kaiv" | "raiv")
    {
        let dir = path.and_then(|p| p.parent());
        let a = Rc::new(parse_kaiv_ext(&ext.to_ascii_lowercase(), &text, dir)?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    // YAML/TOML/Markdown are extension-only, matching the single-input
    // flow (YAML/TOML share the JSON pointer model; Markdown locates).
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        let ext = ext.as_str();
        if matches!(ext, "yaml" | "yml") {
            let a = Rc::new(quarb_yaml::parse(&text).context("parsing YAML")?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.pointer(n))));
        }
        if ext == "toml" {
            let a = Rc::new(quarb_toml::parse(&text).context("parsing TOML")?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.pointer(n))));
        }
        if matches!(ext, "md" | "markdown") {
            let a = Rc::new(quarb_markdown::parse(&text));
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        // Plain text mounts at the text level, matching the
        // single-input flow.
        if ext == "txt" {
            let a = Rc::new(quarb_text::TextModel::parse_plain(&text));
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        if matches!(ext, "conllu" | "conllup") {
            let a = Rc::new(
                quarb_text::TextModel::parse_conllu_text(&text)
                    .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?,
            );
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
        if matches!(ext, "jsonl" | "ndjson") {
            let a = Rc::new(JsonAdapter::parse_lines(&text).context("parsing JSONL")?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.pointer(n))));
        }
        if matches!(ext, "atd" | "atk" | "usfm" | "sfm") {
            let dir = path.and_then(|p| p.parent()).unwrap_or(Path::new("."));
            let a = Rc::new(AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?);
            let r = a.clone();
            return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
        }
    }
    if is_atrep(&text) {
        let dir = path
            .and_then(|p| p.parent())
            .unwrap_or_else(|| Path::new("."));
        let a = Rc::new(AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?);
        let r = a.clone();
        return Ok(Mounted::of(a, Box::new(move |n| r.locator(n))));
    }
    if let Some(delim) = csv_delimiter(path) {
        let a =
            Rc::new(CsvAdapter::parse_declared(&text, delim, ctx.csv()).context("parsing CSV")?);
        let r = a.clone();
        Ok(Mounted::of(a, Box::new(move |n| r.locator(n))))
    } else if is_xml(path, &text) {
        let a = Rc::new(XmlAdapter::parse(&text).context("parsing XML")?);
        let r = a.clone();
        Ok(Mounted::of(a, Box::new(move |n| r.locator(n))))
    } else if is_html(path, &text) {
        let a = Rc::new(HtmlAdapter::parse(&text));
        let r = a.clone();
        Ok(Mounted::of(a, Box::new(move |n| r.locator(n))))
    } else {
        // Whole-document JSON first, per-line (JSONL) second —
        // matching the single-input flow.
        let a = match JsonAdapter::parse(&text) {
            Ok(a) => Rc::new(a),
            Err(e) => match JsonAdapter::parse_lines(&text) {
                Ok(a) => Rc::new(a),
                Err(_) => return Err(e).context("parsing JSON"),
            },
        };
        let r = a.clone();
        Ok(Mounted::of(a, Box::new(move |n| r.pointer(n))))
    }
}
