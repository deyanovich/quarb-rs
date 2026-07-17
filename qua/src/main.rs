//! `qua` — a structure-aware query tool.
//!
//! Runs a Quarb query against a filesystem directory, a JSON, XML,
//! HTML, or CSV document, or a SQLite database, printing each
//! result one per line. The
//! input format is chosen from the argument: a directory is queried
//! with the filesystem adapter; a `.csv`/`.tsv` file as a table; a
//! file (or piped stdin) is parsed as XML if its name ends in
//! `.xml`/`.svg`/`.xhtml` or its content starts with `<?xml`, as
//! HTML if its name ends in `.html`/`.htm` or its content starts
//! with `<`, otherwise as JSON.

use anyhow::Context;
use clap::Parser;
use quarb::{AllowShell, AstAdapter, NodeId, QuantifierBound, QueryResult, Value, WithNow};
use quarb_archive::ArchiveAdapter;
use quarb_bigquery::BigqueryAdapter;
use quarb_code::CodeAdapter;
use quarb_compose::ComposeAdapter;
use quarb_csv::CsvAdapter;
use quarb_datastore::DatastoreAdapter;
use quarb_duckdb::DuckdbAdapter;
use quarb_firebase::FirebaseAdapter;
use quarb_firestore::FirestoreAdapter;
use quarb_fs::{FsAdapter, FsOptions};
use quarb_git::GitAdapter;
use quarb_gsheet::GsheetAdapter;
use quarb_html::HtmlAdapter;
use quarb_imap::ImapAdapter;
use quarb_json::JsonAdapter;
use quarb_maildir::MaildirAdapter;
use quarb_mount::{Mount, MountAdapter, Shared};
use quarb_mysql::MysqlAdapter;
use quarb_objstore::ObjstoreAdapter;
use quarb_neo4j::Neo4jAdapter;
use quarb_postgres::PostgresAdapter;
use quarb_serve::ServeAdapter;
use quarb_sqlite::SqliteAdapter;
use quarb_xlsx::XlsxAdapter;
use quarb_xml::XmlAdapter;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Query a filesystem tree, a JSON, XML, HTML, or CSV document.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Quarb query, e.g. '//*.rs', '/users/*/name::', or '//a::href'.
    query: String,

    /// Directories (filesystem) and/or `.json`/`.xml`/`.html`/`.csv`
    /// files. One argument queries it directly; several are mounted
    /// as named children of one root (file stem = mount name), so a
    /// single query — including a `<=>` join — spans them all. If
    /// omitted, reads one document from stdin.
    paths: Vec<PathBuf>,

    /// Include hidden entries (filesystem only).
    #[arg(long)]
    hidden: bool,

    /// Do not respect `.gitignore` / `.ignore` (filesystem only).
    #[arg(long = "no-ignore")]
    no_ignore: bool,

    /// Interpret the query as XPath 1.0 and translate it to Quarb
    /// before running (semantic notes go to stderr).
    #[arg(long)]
    xpath: bool,

    /// Interpret the query as a jq filter and translate it to Quarb
    /// before running (semantic notes go to stderr).
    #[arg(long, conflicts_with = "xpath")]
    jq: bool,

    /// Interpret the query as a SQL SELECT statement and translate
    /// it to Quarb before running (semantic notes go to stderr).
    #[arg(long, conflicts_with_all = ["xpath", "jq"])]
    sql: bool,

    /// Emit results as canonical kaiv (.daiv): one typed leaf per
    /// value under /@results/N, with provenance recording the source
    /// document and each value's origin node.
    #[arg(long)]
    daiv: bool,

    /// Load fragment definitions (`def &name(params): body;`) from a
    /// file before parsing the query; inline defs extend them.
    #[arg(long, value_name = "FILE")]
    defs: Option<PathBuf>,

    /// Expand the query's fragments and print the resulting
    /// canonical query text instead of running it (macroexpand).
    #[arg(long)]
    expand: bool,

    /// Disable SQL pushdown for database inputs (always evaluate
    /// through the adapter's scan path).
    #[arg(long = "no-pushdown")]
    no_pushdown: bool,

    /// Explain the pushdown decision on stderr: the SQL a database
    /// query runs server-side, or why it fell back to the scan.
    #[arg(long)]
    explain: bool,

    /// Save the result instead of printing it: `.db`/`.sqlite`
    /// writes a SQLite table (records become columns), `.json` a
    /// JSON array — both first-class inputs for later queries.
    #[arg(long, value_name = "FILE")]
    save: Option<PathBuf>,

    /// The table name for --save into SQLite (default: result).
    #[arg(long = "as", value_name = "NAME", default_value = "result")]
    save_as: String,

    /// Descend through parseable file content (composition): a
    /// .json/.xml/.html/.csv leaf's parsed tree becomes its
    /// children. Default for archives; opt-in for directories.
    #[arg(long)]
    descend: bool,

    /// A declared-references document for schemaless databases
    /// (Firebase): '{"refs": {"field": "container", ...}}' — bare
    /// '~>' and '->' crosslinks work for the declared fields.
    #[arg(long, value_name = "FILE")]
    refs: Option<PathBuf>,

    /// Override the quantifier bound N_max: the depth to which the
    /// open-ended path quantifiers (+, *, {m,}) expand, and the
    /// ceiling of any explicit {m,n}. Default: adapter-provided
    /// (typically 32).
    #[arg(long, value_name = "N")]
    quantifier_bound: Option<usize>,

    /// Allow the sh(...) pipeline stage to run external commands.
    /// Off by default: query text stays inert data — a .quarb
    /// file, a defs file, or a macro can never run a command
    /// without this explicit per-run opt-in.
    #[arg(long)]
    allow_shell: bool,

    /// Pin the invocation instant now() denotes (ISO-8601, e.g.
    /// '2026-07-12T09:00:00Z'). Default: the clock, read once at
    /// startup — evaluation itself never reads a clock, so a
    /// pinned run replays exactly.
    #[arg(long, value_name = "ISO")]
    now: Option<String>,

    /// Interactive session: lines starting with a pipe extend the
    /// current query, anything else starts a new one; ':help' lists
    /// the commands. Inputs re-read per line, so live data stays
    /// live.
    #[arg(short = 'i', long)]
    interactive: bool,
}

thread_local! {
    /// Whether this invocation is `--expand` (print the expanded
    /// query instead of running it). Set once in `main`.
    static EXPAND_FLAG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// The --save target: (file, table name). Set once in `main`.
    static SAVE_TARGET: std::cell::RefCell<Option<(PathBuf, String)>> =
        const { std::cell::RefCell::new(None) };
}

thread_local! {
    /// The --quantifier-bound override. Set once in `main`; `run`
    /// wraps every adapter with it.
    static QUANT_BOUND: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static ALLOW_SHELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The invocation instant now() denotes: --now, or the clock
    /// read ONCE at startup. Set once in `main`; `run` wraps every
    /// adapter with it, so every occurrence in a query denotes the
    /// same point and evaluation never reads a clock.
    static NOW_INSTANT: std::cell::Cell<(i64, u32)> = const { std::cell::Cell::new((0, 0)) };
}

struct Expand;
static EXPAND: Expand = Expand;
impl Expand {
    fn get(&self) -> bool {
        EXPAND_FLAG.with(|f| f.get())
    }
    fn set(&self, v: bool) {
        EXPAND_FLAG.with(|f| f.set(v));
    }
}

fn main() -> anyhow::Result<()> {
    // Restore the default SIGPIPE disposition. Rust ignores
    // SIGPIPE at startup, which turns a closed downstream pipe
    // (`qua ... | head`) into a panic on the next write; a Unix
    // filter should instead die quietly by the signal.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let mut cli = Cli::parse();

    if cli.xpath {
        let translation = quarb_xpath::translate(&cli.query).context("translating XPath")?;
        for note in &translation.notes {
            eprintln!("note: {note}");
        }
        cli.query = translation.query;
    }
    if cli.jq {
        let translation = quarb_jq::translate(&cli.query).context("translating jq")?;
        for note in &translation.notes {
            eprintln!("note: {note}");
        }
        cli.query = translation.query;
    }
    if cli.sql {
        let translation = quarb_sql::translate(&cli.query).context("translating SQL")?;
        for note in &translation.notes {
            eprintln!("note: {note}");
        }
        cli.query = translation.query;
    }

    // Interactive: the session manages its own defs and queries.
    // Ergonomics: `qua -i data.csv` parses data.csv as the query
    // positional; shift an existing path over.
    if cli.interactive {
        if std::path::Path::new(&cli.query).exists() {
            let p = std::mem::take(&mut cli.query);
            cli.paths.insert(0, PathBuf::from(p));
        }
        return repl(&cli);
    }

    // A --defs file holds definitions only; validate it as such,
    // then let its statements precede the query, where inline defs
    // (and duplicate detection) already work.
    if let Some(defs_path) = &cli.defs {
        let text = std::fs::read_to_string(defs_path)
            .with_context(|| format!("reading {}", defs_path.display()))?;
        quarb::parse_defs(&text)
            .with_context(|| format!("parsing definitions in {}", defs_path.display()))?;
        cli.query = format!("{text}\n{}", cli.query);
    }

    // --expand: print the fragment-expanded canonical query and
    // stop. Without an input, expansion is pure; with one, the
    // dispatch in `execute` opens it and `run` expands against it,
    // so data-aware macros (&name!) can read the data.
    if cli.expand {
        if cli.paths.is_empty() {
            println!(
                "{}",
                quarb::expand(&cli.query, &quarb::Defs::default())
                    .context("expanding the query")?
            );
            return Ok(());
        }
        EXPAND.set(true);
    }

    if let Some(path) = &cli.save {
        SAVE_TARGET.with(|t| *t.borrow_mut() = Some((path.clone(), cli.save_as.clone())));
    }
    if let Some(n) = cli.quantifier_bound {
        anyhow::ensure!(n >= 1, "--quantifier-bound must be at least 1");
        QUANT_BOUND.with(|b| b.set(Some(n)));
    }
    if cli.allow_shell {
        ALLOW_SHELL.with(|b| b.set(true));
    }
    // Bind the invocation instant: --now pins it; otherwise the
    // clock, read exactly once, here — never during evaluation.
    let now = match &cli.now {
        Some(text) => {
            let (secs, nanos, _) = quarb::temporal::parse_iso(text)
                .ok_or_else(|| anyhow::anyhow!("--now needs an ISO-8601 instant, got '{text}'"))?;
            (secs, nanos)
        }
        None => {
            let since = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            (since.as_secs() as i64, since.subsec_nanos())
        }
    };
    NOW_INSTANT.with(|c| c.set(now));
    execute(&cli, &cli.query)
}

/// Run one query text against the CLI's inputs, printing results —
/// the whole adapter dispatch. Inputs are (re-)read on every call,
/// so an interactive session sees live data.
fn execute(cli: &Cli, query: &str) -> anyhow::Result<()> {
    // Several inputs are mounted as named children of one root.
    if cli.paths.len() >= 2 {
        let mut mounts: Vec<Mount> = Vec::new();
        let mut renders: Vec<Box<dyn Fn(NodeId) -> String>> = Vec::new();
        for (i, p) in cli.paths.iter().enumerate() {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("doc{i}"));
            // Mounts are addressed by file stem, so two inputs sharing
            // a stem would silently union under one name with no way to
            // target either — refuse rather than merge distinct sources.
            if mounts.iter().any(|m| m.name == stem) {
                anyhow::bail!(
                    "input '{}' mounts as '{stem}', colliding with an earlier input of the \
                     same file stem; give each a distinct name (rename or use inputs with \
                     different basenames)",
                    p.display()
                );
            }
            let (adapter, render) = open_mount(p, cli)?;
            mounts.push(Mount {
                name: stem,
                adapter,
            });
            renders.push(render);
        }
        let sources = cli
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let adapter = MountAdapter::new(mounts);
        return run(
            query,
            &adapter,
            |n| match adapter.decode(n) {
                None => "/".to_string(),
                Some((m, inner)) => {
                    format!("/{}{}", adapter.mount_name(m), renders[m](inner))
                }
            },
            cli.daiv.then_some(sources.as_str()),
        );
    }
    let path = cli.paths.first().cloned();

    // A directory is a filesystem query; everything else is a
    // document read from a file or stdin.
    if let Some(path) = &path
        && path.is_dir()
    {
        let opts = FsOptions {
            hidden: cli.hidden,
            respect_ignore: !cli.no_ignore,
        };
        let src = path.display().to_string();
        if cli.descend {
            let adapter = ComposeAdapter::new(FsAdapter::with_options(path, opts)?);
            return run(
                query,
                &adapter,
                |n| adapter.locator(n, |o| adapter.outer().path(o).display().to_string()),
                cli.daiv.then_some(src.as_str()),
            );
        }
        let adapter = FsAdapter::with_options(path, opts)?;
        return run(
            query,
            &adapter,
            |n| adapter.path(n).display().to_string(),
            cli.daiv.then_some(src.as_str()),
        );
    }

    // Google Firestore / Datastore targets.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("firestore://")
    {
        let adapter = FirestoreAdapter::connect(s).context("connecting to Firestore")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("datastore://")
    {
        let adapter = DatastoreAdapter::connect(s).context("connecting to Datastore")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A Neo4j property graph: neo4j://HOST[/DB][?key=PROP].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("neo4j://")
    {
        let adapter = Neo4jAdapter::connect(s).context("connecting to Neo4j")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A git repository: `git:PATH` (any directory inside the
    // repo).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && let Some(repo) = s.strip_prefix("git:")
    {
        let adapter =
            GitAdapter::open(std::path::Path::new(repo)).context("opening git repository")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A Firebase RTDB target navigates the remote JSON tree
    // lazily (no pushdown: not SQL — every touched node is one
    // GET).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
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
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A BigQuery target connects and introspects the dataset.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("bigquery://")
    {
        if let Some(plan) = pushdown_plan(cli, query)
            && let Ok((cols, rows)) =
                quarb_bigquery::raw_query(s, &plan.sql, plan.order_table.as_deref())
        {
            print_raw(&cols, rows);
            return Ok(());
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => BigqueryAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => BigqueryAdapter::connect(s),
        }
        .context("connecting to BigQuery")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A MySQL/MariaDB URL connects and introspects the database.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("mysql://")
    {
        if let Some(plan) = pushdown_plan(cli, query)
            && let Ok((cols, rows)) =
                quarb_mysql::raw_query(s, &plan.sql, plan.order_table.as_deref())
        {
            print_raw(&cols, rows);
            return Ok(());
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => MysqlAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => MysqlAdapter::connect(s),
        }
        .context("connecting to MySQL")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A PostgreSQL connection string connects and materializes the
    // public schema (postgres:// / postgresql:// URL, or the
    // keyword form starting with host=).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && is_pg_config(s)
    {
        if let Some(plan) = pushdown_plan(cli, query)
            && let Ok((cols, rows)) =
                quarb_postgres::raw_query(s, &plan.sql, plan.order_table.as_deref())
        {
            print_raw(&cols, rows);
            return Ok(());
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => PostgresAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => PostgresAdapter::connect(s),
        }
        .context("connecting to PostgreSQL")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A served adapter: `serve:COMMAND` spawns the command and
    // speaks the serve protocol — any tool exposes its data
    // without qua linking it.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && let Some(cmd) = s.strip_prefix("serve:")
    {
        let adapter = ServeAdapter::spawn(cmd).context("spawning served adapter")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A Google Sheets spreadsheet.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("gsheet://")
    {
        let adapter = GsheetAdapter::connect(s).context("connecting to Google Sheets")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // Object stores (gs:// / s3://), composed by default —
    // grafting a bucket of JSON/CSV/source files is the point.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("gs://") || s.starts_with("s3://"))
    {
        let adapter =
            ComposeAdapter::new(ObjstoreAdapter::connect(s).context("connecting to bucket")?);
        return run(
            query,
            &adapter,
            |n| adapter.locator(n, |o| adapter.outer().locator(o)),
            cli.daiv.then_some(s),
        );
    }

    // A remote IMAP mailbox.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("imap://") || s.starts_with("imaps://"))
    {
        let adapter = ImapAdapter::connect(s).context("connecting to IMAP")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // A mailbox: `mail:PATH` (a Maildir directory or an mbox
    // file).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && let Some(mb) = s.strip_prefix("mail:")
    {
        let adapter = MaildirAdapter::open(std::path::Path::new(mb)).context("opening mailbox")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(s),
        );
    }

    // Source code: files with a tree-sitter grammar parse into
    // their syntax tree.
    if let Some(p) = &path
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| quarb_code::supported(&e.to_ascii_lowercase()))
    {
        let adapter = CodeAdapter::open(p).context("parsing source file")?;
        let src = p.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(src.as_str()),
        );
    }

    // Spreadsheets (before the archive check — .xlsx/.ods ARE
    // zips, but the sheets are the point).
    if let Some(p) = &path
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "xlsx" | "xls" | "ods"))
    {
        let adapter = XlsxAdapter::open(p).context("opening workbook")?;
        let src = p.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(src.as_str()),
        );
    }

    // DuckDB databases, by extension.
    if let Some(p) = &path
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("duckdb") || e.eq_ignore_ascii_case("ddb"))
    {
        if let Some(plan) = pushdown_plan(cli, query)
            && let Ok((cols, rows)) =
                quarb_duckdb::raw_query(p, &plan.sql, plan.order_table.as_deref())
        {
            print_raw(&cols, rows);
            return Ok(());
        }
        let adapter = DuckdbAdapter::open(p).context("opening DuckDB database")?;
        let src = p.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(src.as_str()),
        );
    }

    // Archives are binary: dispatch before the text read (zip/PK
    // or gzip magic, or a .tar extension). Composition is on by
    // default — the point of opening a .docx is the XML inside.
    if let Some(p) = &path
        && is_archive(p)
    {
        let src = p.display().to_string();
        let adapter = ComposeAdapter::new(ArchiveAdapter::open(p).context("opening archive")?);
        return run(
            query,
            &adapter,
            |n| adapter.locator(n, |o| adapter.outer().locator(o)),
            cli.daiv.then_some(src.as_str()),
        );
    }

    // SQLite databases are binary: dispatch before the text read
    // (by extension, or the 16-byte magic).
    if let Some(p) = &path
        && is_sqlite(p)
    {
        if let Some(plan) = pushdown_plan(cli, query)
            && let Ok((cols, rows)) =
                quarb_sqlite::raw_query(p, &plan.sql, plan.order_table.as_deref())
        {
            print_raw(&cols, rows);
            return Ok(());
        }
        let adapter = match partial_plan(cli, query) {
            Some(pl) => SqliteAdapter::open_filtered(p, &pl.table, &pl.where_sql),
            None => SqliteAdapter::open(p),
        }
        .context("opening SQLite database")?;
        let src = p.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.daiv.then_some(src.as_str()),
        );
    }

    let (text, path) = match &path {
        Some(path) => (
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
            Some(path.as_path()),
        ),
        None => {
            if std::io::stdin().is_terminal() {
                anyhow::bail!("no input: give a directory, a file, or pipe a document to stdin");
            }
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            (text, None)
        }
    };
    // Strip a leading UTF-8 BOM (RFC 8259 permits ignoring it): it
    // otherwise breaks JSON parsing and defeats the XML/HTML sniffers,
    // since U+FEFF is not whitespace so `trim_start` leaves it in place.
    let text = match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    };

    let source = path.map_or_else(|| "stdin".to_string(), |p| p.display().to_string());
    let daiv = cli.daiv.then_some(source.as_str());

    // A .quarb file holds a Quarb query: reflect it as an arbor and
    // query the query (extension-only, like CSV).
    if is_quarb(path) {
        let adapter = quarb::reflect::QueryArbor::parse(&text).context("parsing Quarb query")?;
        return run(query, &adapter, |n| adapter.locator(n), daiv);
    }
    // CSV/TSV are extension-only (tabular text is not sniffable).
    if let Some(delim) = csv_delimiter(path) {
        let adapter = CsvAdapter::parse_with_delimiter(&text, delim).context("parsing CSV")?;
        return run(query, &adapter, |n| adapter.locator(n), daiv);
    }
    // YAML/TOML are extension-only (both share the JSON model).
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        if matches!(ext, "yaml" | "yml") {
            let adapter = quarb_yaml::parse(&text).context("parsing YAML")?;
            return run(query, &adapter, |n| adapter.pointer(n), daiv);
        }
        if ext == "toml" {
            let adapter = quarb_toml::parse(&text).context("parsing TOML")?;
            return run(query, &adapter, |n| adapter.pointer(n), daiv);
        }
        if matches!(ext, "md" | "markdown") {
            let adapter = quarb_markdown::parse(&text);
            return run(query, &adapter, |n| adapter.locator(n), daiv);
        }
        // kaiv documents — the typed arbor whose namepaths ARE
        // quarb paths, so --daiv output re-mounts (graft and join
        // over typed results). Extension picks the pipeline stage:
        // .daiv is canonical, .kaiv compiles first, .raiv
        // denormalizes its $field references.
        if matches!(ext, "daiv" | "kaiv" | "raiv") {
            let dir = path.and_then(|p| p.parent());
            let adapter = parse_kaiv_ext(ext, &text, dir)?;
            return run(query, &adapter, |n| adapter.locator(n), daiv);
        }
    }
    if is_xml(path, &text) {
        let adapter = XmlAdapter::parse(&text).context("parsing XML")?;
        run(query, &adapter, |n| adapter.locator(n), daiv)
    } else if is_html(path, &text) {
        let adapter = HtmlAdapter::parse(&text);
        run(query, &adapter, |n| adapter.locator(n), daiv)
    } else {
        let adapter = JsonAdapter::parse(&text).context("parsing JSON")?;
        run(query, &adapter, |n| adapter.pointer(n), daiv)
    }
}

/// The interactive session: a read-eval-print loop where the
/// current query is living state. A line starting with a pipe
/// extends it (and is rolled back if it fails); a `def` line adds a
/// fragment; any other query line starts fresh. Inputs are re-read
/// per line, so live data stays live.
fn repl(cli: &Cli) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};

    if cli.paths.is_empty() {
        anyhow::bail!("interactive mode needs an input path (stdin drives the session)");
    }
    // Definition lines accumulate; --defs seeds them.
    let mut defs = match &cli.defs {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            quarb::parse_defs(&text)
                .with_context(|| format!("parsing definitions in {}", path.display()))?;
            text
        }
        None => String::new(),
    };
    // The current query, one segment per accepted line (`:undo`
    // pops one).
    let mut segments: Vec<String> = Vec::new();
    let combined = |defs: &str, segments: &[String]| {
        let q = segments.join(" ");
        if defs.is_empty() {
            q
        } else {
            format!("{defs}\n{q}")
        }
    };

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("qua> ");
        std::io::stdout().flush()?;
        let Some(line) = lines.next() else { break };
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Commands: `:word` (a query cannot start with a lone `:`).
        if line.starts_with(':') && !line.starts_with("::") {
            match line {
                ":q" | ":quit" => break,
                ":help" | ":h" => {
                    println!(
                        "  <query>        run a fresh query\n                           | <stage> ...   extend the current query (rolls back on error)\n                           def &n: ...;    add a fragment definition\n                           :show           print the current query (and its expansion)\n                           :undo           drop the last accepted line\n                           :reset          clear the current query\n                           :quit           leave (also Ctrl-D)"
                    );
                }
                ":show" | ":s" => {
                    if segments.is_empty() {
                        println!("(no current query)");
                    } else {
                        let q = segments.join(" ");
                        println!("{q}");
                        if !defs.is_empty()
                            && let Ok(expanded) =
                                quarb::expand(&combined(&defs, &segments), &quarb::Defs::default())
                        {
                            println!("expanded: {expanded}");
                        }
                    }
                }
                ":undo" | ":u" => {
                    if segments.pop().is_none() {
                        println!("(nothing to undo)");
                    } else if !segments.is_empty() {
                        let _ = execute(cli, &combined(&defs, &segments));
                    }
                }
                ":reset" | ":r" => segments.clear(),
                other => println!("unknown command '{other}' (:help lists them)"),
            }
            continue;
        }

        // A definition line (template or macro) extends the
        // session's fragment table.
        if line.starts_with("def ")
            || line == "def"
            || line.starts_with("macro ")
            || line == "macro"
        {
            let candidate = format!("{defs}\n{line}");
            match quarb::parse_defs(&candidate) {
                Ok(_) => defs = candidate,
                Err(e) => eprintln!("error: {e}"),
            }
            continue;
        }

        // A pipe line extends the current query; anything else
        // starts a new one. Failed lines are not committed.
        let candidate: Vec<String> = if line.starts_with('|') || line.starts_with("@|") {
            if segments.is_empty() {
                eprintln!("error: no current query to extend (start with a path)");
                continue;
            }
            let mut c = segments.clone();
            c.push(line.to_string());
            c
        } else {
            vec![line.to_string()]
        };
        match execute(cli, &combined(&defs, &candidate)) {
            Ok(()) => segments = candidate,
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
    Ok(())
}

/// Whether the input is a Quarb query file (`.quarb`), to be
/// reflected as a query arbor.
fn is_quarb(path: Option<&Path>) -> bool {
    path.and_then(Path::extension)
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("quarb"))
}

/// Whether pushdown applies: enabled, not emitting daiv (which
/// needs node provenance), and not in --expand mode.
fn pushdown_applies(cli: &Cli) -> bool {
    !cli.no_pushdown && !cli.daiv && !EXPAND.get() && cli.save.is_none()
}

/// The partial-pushdown plan (a WHERE for one table's fetch), with
/// --explain commentary. Tried only after full pushdown refused.
fn partial_plan(cli: &Cli, query: &str) -> Option<quarb_sql::Partial> {
    if !pushdown_applies(cli) {
        return None;
    }
    match quarb_sql::partial_pushdown_explained(query) {
        Ok(p) => {
            if cli.explain {
                eprintln!(
                    "partial pushdown: WHERE {} on {}; the rest scans the filtered set",
                    p.where_sql, p.table
                );
            }
            Some(p)
        }
        Err(e) => {
            if cli.explain {
                eprintln!("partial pushdown refused: {e}; scanning");
            }
            None
        }
    }
}

/// The pushdown plan for a database input, with --explain
/// commentary on stderr either way.
fn pushdown_plan(cli: &Cli, query: &str) -> Option<quarb_sql::Pushdown> {
    if !pushdown_applies(cli) {
        if cli.explain {
            eprintln!("pushdown: disabled; scanning");
        }
        return None;
    }
    match quarb_sql::pushdown_explained(query) {
        Ok(plan) => {
            if cli.explain {
                match &plan.order_table {
                    Some(t) => eprintln!("pushdown: {} -- ordered by {t}'s key", plan.sql),
                    None => eprintln!("pushdown: {}", plan.sql),
                }
            }
            Some(plan)
        }
        Err(e) => {
            if cli.explain {
                eprintln!("pushdown refused: {e}; scanning");
            }
            None
        }
    }
}

/// Print a pushed-down result the way the engine would: bare
/// values for one column, records for several.
fn print_raw(cols: &[String], rows: Vec<Vec<Value>>) {
    for row in rows {
        if cols.len() <= 1 {
            for v in row {
                println!("{v}");
            }
        } else {
            let rec = Value::Record(cols.iter().cloned().zip(row).collect());
            println!("{rec}");
        }
    }
}

/// Whether the input names a PostgreSQL connection rather than a
/// file: a `postgres://` / `postgresql://` URL, or the keyword
/// form (`host=... dbname=...`).
fn is_pg_config(s: &str) -> bool {
    s.starts_with("postgres://") || s.starts_with("postgresql://") || s.starts_with("host=")
}

/// Whether the input is a SQLite database: by extension
/// (`.db` / `.sqlite` / `.sqlite3`), or by the 16-byte magic.
/// Zip-family or tar archives, by magic bytes or extension.
fn is_archive(path: &Path) -> bool {
    if let Some(e) = path.extension().and_then(|e| e.to_str())
        && matches!(
            e.to_ascii_lowercase().as_str(),
            "zip" | "jar" | "docx" | "odt" | "epub" | "tar" | "tgz" | "gz"
        )
    {
        return true;
    }
    let mut buf = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut buf))
        .map(|n| n == 2 && (buf == *b"PK" || buf == [0x1f, 0x8b]))
        .unwrap_or(false)
}

fn is_sqlite(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        e.eq_ignore_ascii_case("db")
            || e.eq_ignore_ascii_case("sqlite")
            || e.eq_ignore_ascii_case("sqlite3")
    }) {
        return true;
    }
    let mut buf = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_ok()
        && &buf == b"SQLite format 3\0"
}

/// The CSV field delimiter implied by the file extension: `.csv`
/// (comma) or `.tsv` (tab), else not a CSV file.
fn csv_delimiter(path: Option<&Path>) -> Option<u8> {
    let ext = path?.extension()?.to_str()?;
    if ext.eq_ignore_ascii_case("csv") {
        Some(b',')
    } else if ext.eq_ignore_ascii_case("tsv") {
        Some(b'\t')
    } else {
        None
    }
}

/// Whether the input should be parsed as XML: an `.xml`/`.svg`/
/// `.xhtml` extension, or content that begins with the `<?xml`
/// prologue. Checked before HTML, whose generic `<` sniff would
/// otherwise swallow XML.
fn is_xml(path: Option<&Path>, text: &str) -> bool {
    let by_ext = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            ["xml", "svg", "xhtml"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        });
    by_ext || text.trim_start().starts_with("<?xml")
}

/// Whether the input should be parsed as HTML: an `.html`/`.htm`
/// extension, or content that begins with `<`.
fn is_html(path: Option<&Path>, text: &str) -> bool {
    let by_ext = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    by_ext || text.trim_start().starts_with('<')
}

/// Run `query` against `adapter`, printing node locations (via
/// `render`) or projected values, one per line.
/// A boxed adapter and its locator renderer, ready to mount.
type Mounted = (Box<dyn AstAdapter>, Box<dyn Fn(NodeId) -> String>);

/// Open one input as a boxed adapter plus its locator renderer, for
/// mounting. Format detection matches the single-input flow.
/// Mount kaiv text by its extension's pipeline stage: `.daiv` is
/// canonical, `.kaiv` is authored (compile + denormalize), `.raiv`
/// is relational (denormalize). The file's directory anchors the
/// resolver, so `.!units` / `.!types` imports (and a sibling
/// `kaiv.kaiv`) resolve exactly as `kaiv build` there would.
fn parse_kaiv_ext(
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

fn open_mount(p: &Path, cli: &Cli) -> anyhow::Result<Mounted> {
    if p.is_dir() {
        let opts = FsOptions {
            hidden: cli.hidden,
            respect_ignore: !cli.no_ignore,
        };
        let a = Rc::new(FsAdapter::with_options(p, opts)?);
        let r = a.clone();
        return Ok((
            Box::new(Shared(a)),
            Box::new(move |n| r.path(n).display().to_string()),
        ));
    }
    if let Some(s) = p.to_str()
        && let Some(cmd) = s.strip_prefix("serve:")
    {
        let a = Rc::new(ServeAdapter::spawn(cmd).context("spawning served adapter")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("firestore://")
    {
        let a = Rc::new(FirestoreAdapter::connect(s).context("connecting to Firestore")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("datastore://")
    {
        let a = Rc::new(DatastoreAdapter::connect(s).context("connecting to Datastore")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("neo4j://")
    {
        let a = Rc::new(Neo4jAdapter::connect(s).context("connecting to Neo4j")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && let Some(repo) = s.strip_prefix("git:")
    {
        let a = Rc::new(
            GitAdapter::open(std::path::Path::new(repo)).context("opening git repository")?,
        );
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("firebase://")
    {
        let a = Rc::new(FirebaseAdapter::connect(s).context("connecting to Firebase")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("bigquery://")
    {
        let a = Rc::new(BigqueryAdapter::connect(s).context("connecting to BigQuery")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("mysql://")
    {
        let a = Rc::new(MysqlAdapter::connect(s).context("connecting to MySQL")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && is_pg_config(s)
    {
        let a = Rc::new(PostgresAdapter::connect(s).context("connecting to PostgreSQL")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(t) = p.to_str()
        && let Some(mb) = t.strip_prefix("mail:")
    {
        let a = Rc::new(MaildirAdapter::open(std::path::Path::new(mb)).context("opening mailbox")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(t) = p.to_str()
        && t.starts_with("gsheet://")
    {
        let a = Rc::new(GsheetAdapter::connect(t).context("connecting to Google Sheets")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(t) = p.to_str()
        && (t.starts_with("gs://") || t.starts_with("s3://"))
    {
        let a = Rc::new(ComposeAdapter::new(
            ObjstoreAdapter::connect(t).context("connecting to bucket")?,
        ));
        let r = a.clone();
        return Ok((
            Box::new(Shared(a)),
            Box::new(move |n| r.locator(n, |o| r.outer().locator(o))),
        ));
    }
    if let Some(t) = p.to_str()
        && (t.starts_with("imap://") || t.starts_with("imaps://"))
    {
        let a = Rc::new(ImapAdapter::connect(t).context("connecting to IMAP")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    // Spreadsheets before the archive check — .xlsx/.ods ARE zips (PK
    // magic), but the sheets are the point, not the raw XML entries.
    if let Some(ext) = p.extension().and_then(|e| e.to_str())
        && matches!(ext.to_ascii_lowercase().as_str(), "xlsx" | "xls" | "ods")
    {
        let a = Rc::new(XlsxAdapter::open(p).context("opening workbook")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if is_archive(p) {
        let a = Rc::new(ComposeAdapter::new(
            ArchiveAdapter::open(p).context("opening archive")?,
        ));
        let r = a.clone();
        return Ok((
            Box::new(Shared(a)),
            Box::new(move |n| r.locator(n, |o| r.outer().locator(o))),
        ));
    }
    if is_sqlite(p) {
        let a = Rc::new(SqliteAdapter::open(p).context("opening SQLite database")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
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
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str())
        && matches!(ext, "daiv" | "kaiv" | "raiv")
    {
        let dir = path.and_then(|p| p.parent());
        let a = Rc::new(parse_kaiv_ext(ext, &text, dir)?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    // YAML/TOML/Markdown are extension-only, matching the single-input
    // flow (YAML/TOML share the JSON pointer model; Markdown locates).
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        if matches!(ext, "yaml" | "yml") {
            let a = Rc::new(quarb_yaml::parse(&text).context("parsing YAML")?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.pointer(n))));
        }
        if ext == "toml" {
            let a = Rc::new(quarb_toml::parse(&text).context("parsing TOML")?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.pointer(n))));
        }
        if matches!(ext, "md" | "markdown") {
            let a = Rc::new(quarb_markdown::parse(&text));
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
    }
    if let Some(delim) = csv_delimiter(path) {
        let a = Rc::new(CsvAdapter::parse_with_delimiter(&text, delim).context("parsing CSV")?);
        let r = a.clone();
        Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))))
    } else if is_xml(path, &text) {
        let a = Rc::new(XmlAdapter::parse(&text).context("parsing XML")?);
        let r = a.clone();
        Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))))
    } else if is_html(path, &text) {
        let a = Rc::new(HtmlAdapter::parse(&text));
        let r = a.clone();
        Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))))
    } else {
        let a = Rc::new(JsonAdapter::parse(&text).context("parsing JSON")?);
        let r = a.clone();
        Ok((Box::new(Shared(a)), Box::new(move |n| r.pointer(n))))
    }
}

fn run<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    daiv_source: Option<&str>,
) -> anyhow::Result<()> {
    // Every adapter dispatch funnels through here: the one place
    // the --quantifier-bound and --allow-shell overrides wrap the
    // adapter.
    if ALLOW_SHELL.with(|b| b.get()) {
        let shelled = AllowShell { inner: adapter };
        return run_bounded(query, &shelled, render, daiv_source);
    }
    run_bounded(query, adapter, render, daiv_source)
}

fn run_bounded<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    daiv_source: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(n) = QUANT_BOUND.with(|b| b.get()) {
        let bounded = QuantifierBound { inner: adapter, bound: n };
        return run_nowed(query, &bounded, render, daiv_source);
    }
    run_nowed(query, adapter, render, daiv_source)
}

fn run_nowed<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    daiv_source: Option<&str>,
) -> anyhow::Result<()> {
    // The invocation instant is always bound in the CLI (main set
    // it from --now or one startup clock read).
    let (secs, nanos) = NOW_INSTANT.with(|c| c.get());
    let nowed = WithNow { inner: adapter, secs, nanos };
    run_inner(query, &nowed, render, daiv_source)
}

fn run_inner<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    daiv_source: Option<&str>,
) -> anyhow::Result<()> {
    // --expand with an input: expansion with the dataset at hand,
    // so data-aware macros (&name!) can read it.
    if EXPAND.get() {
        println!(
            "{}",
            quarb::expand_with(query, &quarb::Defs::default(), adapter)
                .context("expanding the query")?
        );
        return Ok(());
    }
    if let Some(source) = daiv_source {
        let rows = quarb::run_traced(query, adapter)?;
        print!("{}", emit_daiv(&rows, source, render)?);
        return Ok(());
    }
    let save = SAVE_TARGET.with(|t| t.borrow().clone());
    if let Some((path, table)) = save {
        let values = match quarb::run(query, adapter)? {
            QueryResult::Values(vs) => vs,
            QueryResult::Nodes(ns) => ns.into_iter().map(|n| Value::Str(render(n))).collect(),
        };
        let n = values.len();
        save_result(&path, &table, values)?;
        eprintln!("saved {n} row(s) to {}", path.display());
        return Ok(());
    }
    match quarb::run(query, adapter)? {
        QueryResult::Nodes(nodes) => {
            for node in nodes {
                println!("{}", render(node));
            }
        }
        QueryResult::Values(values) => {
            for value in values {
                println!("{value}");
            }
        }
    }
    Ok(())
}

/// Materialize a result: `.json` writes a JSON array (records as
/// objects — the shape the JSON adapter reads back); anything else
/// writes a SQLite table (records become columns, scalars a
/// `value` column). Refuses to overwrite: an existing .json file,
/// or an existing table in a .db.
fn save_result(path: &Path, table: &str, values: Vec<Value>) -> anyhow::Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "json" {
        if path.exists() {
            anyhow::bail!("{} already exists (refusing to overwrite)", path.display());
        }
        let items: Vec<String> = values.iter().map(|v| v.to_json()).collect();
        std::fs::write(
            path,
            format!(
                "[{}]
",
                items.join(
                    ",
 "
                )
            ),
        )?;
        return Ok(());
    }
    // SQLite: records become columns (first-appearance union),
    // scalars a single `value` column.
    let conn =
        rusqlite::Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if exists > 0 {
        anyhow::bail!(
            "table '{table}' already exists in {} (refusing to overwrite)",
            path.display()
        );
    }
    let mut columns: Vec<String> = Vec::new();
    let all_records = values.iter().all(|v| matches!(v, Value::Record(_)));
    if all_records {
        for v in &values {
            if let Value::Record(fields) = v {
                for (k, _) in fields {
                    if !columns.contains(k) {
                        columns.push(k.clone());
                    }
                }
            }
        }
    }
    if columns.is_empty() {
        columns.push("value".to_string());
    }
    let decl: Vec<String> = columns.iter().map(|c| format!("\"{c}\"")).collect();
    conn.execute(
        &format!("CREATE TABLE \"{table}\" ({})", decl.join(", ")),
        [],
    )?;
    let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
    let mut stmt = conn.prepare(&format!(
        "INSERT INTO \"{table}\" ({}) VALUES ({})",
        decl.join(", "),
        placeholders.join(", ")
    ))?;
    for v in values {
        let row: Vec<rusqlite::types::Value> = if all_records {
            let Value::Record(fields) = &v else {
                unreachable!()
            };
            columns
                .iter()
                .map(|c| {
                    fields
                        .iter()
                        .find(|(k, _)| k == c)
                        .map(|(_, v)| sqlite_value(v))
                        .unwrap_or(rusqlite::types::Value::Null)
                })
                .collect()
        } else {
            vec![sqlite_value(&v)]
        };
        stmt.execute(rusqlite::params_from_iter(row))?;
    }
    Ok(())
}

fn sqlite_value(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
        Value::Int(n) => rusqlite::types::Value::Integer(*n),
        Value::Float(f) => rusqlite::types::Value::Real(*f),
        other => rusqlite::types::Value::Text(other.to_string()),
    }
}

/// Render traced results as canonical kaiv. Each result becomes one
/// leaf (or one leaf per record field) under `/@results/N`, typed by
/// the value's kind; provenance carries the source document (`?q`)
/// and the origin node's locator, identifier-sanitized, as `#dpid`.
/// A value canonical kaiv cannot hold on a flat line falls back to
/// its JSON text (quoted, single-line) as `str`.
fn emit_daiv(
    rows: &[(NodeId, Option<Value>)],
    source: &str,
    render: impl Fn(NodeId) -> String,
) -> anyhow::Result<String> {
    use kaiv::{DaivBuilder, Provenance};
    let err = |e: kaiv::PipelineError| anyhow::anyhow!("emitting .daiv: {e}");
    let mut b = DaivBuilder::new();
    b.declare_source("q", source).map_err(err)?;
    for (i, (node, topic)) in rows.iter().enumerate() {
        let prov = Provenance {
            source: Some("q".to_string()),
            timestamp: None,
            dpid: Some(ident_of(&render(*node))),
        };
        let mut put = |field: &str, value: &Value| -> anyhow::Result<()> {
            let namepath = format!("/@results/{i}::{}", ident_of(field));
            // Quantities emit unit-annotated (`!float:km`), in
            // their written unit so the authored form survives the
            // loop; instants emit as their std/time type, so a
            // re-mount re-mints them.
            match value {
                Value::Quantity {
                    value: bv,
                    base,
                    written,
                } => {
                    let (v, u) = written.clone().unwrap_or((*bv, base.clone()));
                    if b.leaf_with_unit(&namepath, "float", Some(&u), &v.to_string(), Some(&prov))
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                } => {
                    let ty = if offset_min.is_some() {
                        "std/time/datetime"
                    } else if *nanos == 0 && secs.rem_euclid(86400) == 0 {
                        "std/time/date"
                    } else {
                        "std/time/localdatetime"
                    };
                    b.declare_types("std/time").map_err(err)?;
                    if b.leaf(&namepath, ty, &value.to_string(), Some(&prov)).is_ok() {
                        return Ok(());
                    }
                }
                _ => {}
            }
            let (t, payload) = daiv_scalar(value);
            if b.leaf(&namepath, t, &payload, Some(&prov)).is_err() {
                // Not flat-line representable: carry the JSON text.
                b.leaf(&namepath, "str", &value.to_json(), Some(&prov))
                    .map_err(err)?;
            }
            Ok(())
        };
        match topic {
            None => {
                let loc = render(*node);
                put("node", &Value::Str(loc))?;
            }
            Some(Value::Record(fields)) => {
                for (k, v) in fields {
                    put(k, v)?;
                }
            }
            Some(v) => put("value", v)?,
        }
    }
    Ok(b.finish())
}

/// The kaiv type annotation and payload for one value. Lists and
/// records ride as JSON text.
fn daiv_scalar(v: &Value) -> (&'static str, String) {
    match v {
        Value::Null => ("null", String::new()),
        Value::Bool(b) => ("bool", b.to_string()),
        Value::Int(n) => ("int", n.to_string()),
        Value::Float(f) => ("float", f.to_string()),
        Value::Str(s) => ("str", s.clone()),
        Value::List(_) | Value::Record(_) => ("str", v.to_json()),
        // The fallback route: instants normally emit typed
        // (std/time, in `put`); durations have no kaiv type yet
        // and quantities normally emit unit-annotated. All ride
        // as text here.
        Value::Instant { .. } | Value::Duration { .. } | Value::Quantity { .. } => {
            ("str", v.to_string())
        }
    }
}

/// Sanitize a locator or field name into kaiv's identifier charset
/// ([A-Za-z0-9_.-]): runs of other characters become one `-`.
fn ident_of(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.') {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "value".to_string()
    } else {
        trimmed
    }
}
