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
use quarb_age::AgeAdapter;
use quarb_arangodb::ArangoAdapter;
use quarb_archive::ArchiveAdapter;
use quarb_athena::AthenaAdapter;
use quarb_atrep::AtrepAdapter;
use quarb_azlogs::AzlAdapter;
use quarb_bigquery::BigqueryAdapter;
use quarb_cflogs::CflAdapter;
use quarb_compose::{ComposeAdapter, DocumentGraft, SourceGraft};
use quarb_cosmos::CosmosAdapter;
use quarb_csv::CsvAdapter;
use quarb_cwlogs::CwlAdapter;
use quarb_datastore::DatastoreAdapter;
use quarb_ddlogs::DdlAdapter;
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
use quarb_mongodb::MongodbAdapter;
use quarb_mount::{Mount, MountAdapter, Shared};
use quarb_mssql::MssqlAdapter;
use quarb_mysql::MysqlAdapter;
use quarb_neo4j::Neo4jAdapter;
use quarb_neptune::NeptuneAdapter;
use quarb_objstore::ObjstoreAdapter;
use quarb_oracle::OracleAdapter;
use quarb_postgres::PostgresAdapter;
use quarb_redis::RedisAdapter;
use quarb_serve::ServeAdapter;
use quarb_sparql::SparqlAdapter;
use quarb_sqlite::SqliteAdapter;
use quarb_tree_sitter::TreeSitterAdapter;
use quarb_xlsx::XlsxAdapter;
use quarb_xml::XmlAdapter;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Query a filesystem tree, a JSON, XML, HTML, or CSV document.
#[derive(Parser, Default)]
#[command(version, about)]
struct Cli {
    /// Quarb query, e.g. '//*.rs', '/users/*/name::', or '//a::href'.
    query: String,

    /// Directories (filesystem) and/or `.json`/`.xml`/`.html`/`.csv`
    /// files. One argument queries it directly; several are mounted
    /// as named children of one root (file stem = mount name), so a
    /// single query — including a `<=>` join — spans them all.
    /// `NAME=TARGET` picks the mount name explicitly (`ga=events.json`
    /// mounts as `/ga`) — the way to a clean name when the target is
    /// a URL with a query string. If omitted, reads one document
    /// from stdin.
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

    /// Emit results as canonical kaiv: one typed leaf per value
    /// under /@results/N, with provenance recording the source
    /// document and each value's origin node.
    #[arg(long)]
    kaiv: bool,

    /// With --kaiv: how many origins a value's provenance lists by
    /// name before the rest collapse into the elision marker
    /// (`;+N`). A sum over a thousand rows names 8 and says +992.
    #[arg(long, value_name = "N", default_value_t = 8, requires = "kaiv")]
    kaiv_origins: usize,

    /// Reproducible provenance: answer and export only the instants
    /// the data itself recorded, holding back a source's
    /// modification time and the moment of reading, so the same
    /// source gives the same output run after run.
    #[arg(long)]
    reproducible: bool,

    /// Print the results as one JSON document — an array of the
    /// values, records as objects. (The default prints the Quarb
    /// form: `%(k = v; …)` for a record, `@(a; b)` for a list.)
    #[arg(long, conflicts_with_all = ["jsonl", "kaiv"])]
    json: bool,

    /// Print the results as JSON Lines: one JSON document per line.
    #[arg(long, conflicts_with_all = ["json", "kaiv"])]
    jsonl: bool,

    /// Print the results as an aligned text table: records become
    /// columns (the union of their fields, in first-seen order),
    /// scalars a `value` column; widths count grapheme clusters, so
    /// stressed words align.
    #[arg(long, conflicts_with_all = ["json", "jsonl", "kaiv", "csv"])]
    table: bool,

    /// Print the results as CSV (RFC 4180), a header row first; the
    /// same columns as --table.
    #[arg(long, conflicts_with_all = ["json", "jsonl", "kaiv", "table"])]
    csv: bool,

    /// Load fragment definitions (`def &name(params): body;`) from a
    /// file before parsing the query; inline defs extend them.
    #[arg(long, value_name = "FILE")]
    defs: Option<PathBuf>,

    /// Expand the query's fragments and print the resulting
    /// canonical query text instead of running it (macroexpand).
    #[arg(long)]
    expand: bool,

    /// Expand each directly-invoked macro ONE step and print its
    /// generated text before re-expansion, one line per invocation
    /// (macroexpand-1); run the printed text again to take the
    /// next step, --expand for the fixed point.
    #[arg(long = "expand-1", conflicts_with = "expand")]
    expand_1: bool,

    /// Disable SQL pushdown for database inputs (always evaluate
    /// through the adapter's scan path).
    #[arg(long = "no-pushdown")]
    no_pushdown: bool,

    /// Explain the pushdown decision on stderr: the SQL a database
    /// query runs server-side, or why it fell back to the scan.
    #[arg(long)]
    explain: bool,

    /// Hidden: read query lines on stdin and write each back as
    /// syntax-highlighted HTML (the playground's span classes) —
    /// the hook documentation builds use to color transcripts.
    #[arg(long = "highlight-html", hide = true)]
    highlight_html: bool,

    /// Save the result instead of printing it: `.json` writes a
    /// JSON array, any other extension a SQLite table (records
    /// become columns) — both first-class inputs for later queries.
    #[arg(long, value_name = "FILE")]
    save: Option<PathBuf>,

    /// The table name for --save into SQLite (default: result).
    #[arg(long = "as", value_name = "NAME", default_value = "result")]
    save_as: String,

    /// Opt a directory mount into grafting (composition): a
    /// parseable leaf's — .json/.xml/.html/.csv/source — parsed
    /// tree becomes its children. Archives, buckets, and text
    /// columns graft by default.
    #[arg(long)]
    graft: bool,

    /// Disable grafting entirely: no boundary is crossed —
    /// archive members, bucket objects, and JSON text columns
    /// stay opaque leaves, so listings agree with find/tar and
    /// the server's own column types. Refused with the code:
    /// prefix, whose meaning is the grafted view.
    // A future parameterized form (--graft=MOUNT,
    // --no-graft=PATTERN) narrows these; the bare spellings keep
    // meaning all-mounts / all-boundaries. The conflict below
    // then relaxes to "both bare".
    #[arg(long = "no-graft", conflicts_with = "graft")]
    no_graft: bool,

    /// A declared-references document: '{"refs": {"field":
    /// "container", ...}}' — the edges the substrate's own catalog
    /// does not hold. On a SQLite database each declared property gains
    /// the full crosslink fabric ('-->', '->', '<-', '<--') into the
    /// target container (a table, or a view — e.g. a SELECT DISTINCT
    /// dimension view); a property may be scoped as 'table.column'. On
    /// Firebase, bare '-->' and '->' work for the declared properties.
    #[arg(long, value_name = "FILE")]
    refs: Option<PathBuf>,

    /// A model file declaring derived arbor structure over the
    /// source(s): 'node /ips/ip: query;' derives a container whose
    /// children play the role 'ip', 'ref /path/*::f --> C;' a scoped
    /// reference, 'rel A -> B[cond];' a relation no value carries,
    /// 'edge /path/*: ::a -- ::b;' pair edges, 'mount NAME: t;' a
    /// source the model opens itself. Every hop is named for the role
    /// it lands on. The graph the data only implies, made navigable —
    /// over any adapter.
    #[arg(long, value_name = "FILE")]
    model: Option<PathBuf>,
    /// Sentence bonds: a .desm file (repeatable) — abbreviations a
    /// sentence may end in without ending, and /regex/ lines whose
    /// match straddles a UAX #29 break — loaded for the session, so
    /// the corpus: reading, `sentences` and `sc` agree on "Dr.
    /// Smith". `corpus:x?desm=FILE` adds one for a single mount
    #[arg(long, value_name = "FILE")]
    desm: Vec<PathBuf>,

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

    /// Resident session: reuse (or start) a background qua that
    /// keeps the materialized inputs alive, so repeated queries
    /// skip the parse. The first query pays materialization; later
    /// ones answer from the standing arbor. Sessions are keyed by
    /// the canonical target set plus the semantics-affecting flags,
    /// and exit after --resident-ttl idle seconds. The session
    /// serves the inputs as they were when it started: edits to
    /// the files (or to --refs/--defs content) are not seen until
    /// the session expires or is killed.
    #[arg(long)]
    resident: bool,

    /// Idle seconds before a resident session exits. Fixed when
    /// the session starts; later clients of the same session
    /// inherit it (as they do --explain and the other flags the
    /// session was started with).
    #[arg(long, value_name = "SECS", default_value_t = 1800)]
    resident_ttl: u64,

    /// Internal: serve a resident session (spawned by --resident).
    #[arg(long, hide = true)]
    resident_serve: bool,

    /// Print the query with ANSI syntax highlighting and exit — the
    /// terminal counterpart of the JupyterLab highlighter, coloring
    /// paths, sigils, operators, strings, numbers, and stdlib
    /// keywords. Honors NO_COLOR; forces color even off a TTY (so a
    /// pipe into `less -R` works).
    #[arg(long)]
    highlight: bool,

    /// Cache parsed syntax trees for code inputs (.rs/.py/.js/.c…):
    /// the first query over a file parses and caches its AST; later
    /// queries load it and skip the parse. Content-addressed under
    /// ~/.quarb/cache (override with --cache-dir or $QUARB_CACHE_DIR;
    /// remove that directory to clear it). A stale or corrupt entry
    /// is silently ignored and reparsed, so the cache can never
    /// change a result.
    #[arg(long)]
    cache: bool,

    /// The AST cache directory (implies --cache). Default:
    /// $QUARB_CACHE_DIR, else ~/.quarb/cache.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
}

/// How a result line is written (ruling #51): the Quarb form by
/// default, JSON on request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Quarb,
    Json,
    Jsonl,
    Table,
    Csv,
}

thread_local! {
    /// The --json / --jsonl choice. Set once in `main`.
    static OUTPUT: std::cell::Cell<Output> = const { std::cell::Cell::new(Output::Quarb) };
    /// Whether this invocation is `--expand` (print the expanded
    /// query instead of running it). Set once in `main`.
    static EXPAND_FLAG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EXPAND1_FLAG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// `--kaiv-origins`: the bound on named origins per kaiv leaf.
    static KAIV_ORIGINS: std::cell::Cell<usize> = const { std::cell::Cell::new(quarb::ORIGIN_CAP) };
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
    /// Whether --explain should print the executed statement once
    /// a driver has run one.
    static EXPLAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The --model file, parsed. Set once in `main`; `run` wraps
    /// every adapter in a `ModelAdapter` and composes its locator.
    static MODEL: std::cell::RefCell<Option<quarb_model::Model>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(unix)]
thread_local! {
    /// Resident-serve mode: the socket to bind, the idle TTL, and
    /// whether --now pinned the instant (a pinned session replays;
    /// an unpinned one re-reads the clock per query). Set once in
    /// `main`; `run` checks it and enters the serve loop.
    static RESIDENT: std::cell::RefCell<Option<(PathBuf, u64, bool)>> =
        const { std::cell::RefCell::new(None) };
}

/// Split a scheme-prefixed query (`github:/torvalds/…`) into
/// its target scheme and the root-anchored query. Only schemes
/// whose bare form is a complete target qualify — schemes that
/// carry a payload (`git:PATH`, `mongodb://HOST/DB`) keep the
/// two-argument form, where the split would be ambiguous.
fn split_scheme_query(q: &str) -> Option<(&'static str, &str)> {
    for scheme in ["github:", "gitlab:", "k8s:", "kubernetes:"] {
        if let Some(rest) = q.strip_prefix(scheme)
            && rest.starts_with('/')
        {
            return Some((scheme, rest));
        }
    }
    None
}

/// The complete CLI entry point (the `qua` binary is a thin
/// shim over this; the `quarb-full` wheel ships it as
/// `qua-full`).
pub fn cli_main() -> anyhow::Result<()> {
    // Restore the default SIGPIPE disposition. Rust ignores
    // SIGPIPE at startup, which turns a closed downstream pipe
    // (`qua ... | head`) into a panic on the next write; a Unix
    // filter should instead die quietly by the signal.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // A query engine never fetches over the network by surprise:
    // kaiv `.!units` / `.!types` registry imports resolve from the
    // frozen built-in set, local `.!registry` bases, or the warm
    // kaiv cache — not f.kaiv.io. `KAIV_OFFLINE=0` re-enables the
    // fetch explicitly (single-threaded here, so set_var is safe).
    if std::env::var_os("KAIV_OFFLINE").is_none() {
        unsafe {
            std::env::set_var("KAIV_OFFLINE", "1");
        }
    }

    let mut cli = Cli::parse();
    OUTPUT.with(|o| {
        o.set(if cli.json {
            Output::Json
        } else if cli.jsonl {
            Output::Jsonl
        } else if cli.table {
            Output::Table
        } else if cli.csv {
            Output::Csv
        } else {
            Output::Quarb
        })
    });

    // The highlight filter: no query, no target — a pure lexer
    // pass, line in, HTML line out.
    if cli.highlight_html {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            println!("{}", quarb::highlight::highlight_html(&line?));
        }
        return Ok(());
    }

    // A target may ride the query as a scheme prefix —
    // `qua 'github:/torvalds/linux::stars'` is
    // `qua '/torvalds/linux::stars' github:`. Recognized for
    // targets whose bare scheme is a complete target; the first
    // `/` begins the root-anchored query. The two-argument form
    // stays supported.
    if cli.paths.is_empty()
        && let Some((scheme, query)) = split_scheme_query(&cli.query)
    {
        cli.paths.push(PathBuf::from(scheme));
        cli.query = query.to_string();
    }

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

    if cli.highlight {
        // Explicit --highlight forces color (the query is the
        // deliverable), but NO_COLOR still wins.
        if std::env::var_os("NO_COLOR").is_some() {
            println!("{}", cli.query);
        } else {
            println!("{}", quarb::highlight::highlight_ansi(&cli.query));
        }
        return Ok(());
    }

    // A --defs file holds definitions only; validate it as such,
    // then let its statements precede the query, where inline defs
    // (and duplicate detection) already work. Prepended stripped of
    // `#` comment lines — the query lexer has no comment syntax.
    if let Some(defs_path) = &cli.defs {
        let text = std::fs::read_to_string(defs_path)
            .with_context(|| format!("reading {}", defs_path.display()))?;
        // Strip a leading UTF-8 BOM, as the document readers do.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
        quarb::parse_defs(&text)
            .with_context(|| format!("parsing definitions in {}", defs_path.display()))?;
        cli.query = format!("{}\n{}", quarb::strip_defs_comments(&text), cli.query);
    }

    // --expand: print the fragment-expanded canonical query and
    // stop. Without an input, expansion is pure; with one, the
    // dispatch in `execute` opens it and `run` expands against it,
    // so data-aware macros (&name!) can read the data.
    if cli.expand {
        // (a model brings its own mounts and definitions: its
        // expansion waits until the model is read, below)
        if cli.paths.is_empty() && cli.model.is_none() {
            println!(
                "{}",
                quarb::expand(&cli.query, &quarb::Defs::default())
                    .context("expanding the query")?
            );
            return Ok(());
        }
        EXPAND_FLAG.with(|f| f.set(true));
    }
    if cli.reproducible {
        quarb::set_reproducible(true);
    }
    if cli.kaiv {
        KAIV_ORIGINS.with(|c| c.set(cli.kaiv_origins));
    }

    // --expand-1: one ledger step, printed and stop (macroexpand-1).
    if cli.expand_1 {
        if cli.paths.is_empty() {
            for t in quarb::expand_first(&cli.query, &quarb::Defs::default())
                .context("expanding the query")?
            {
                println!("{t}");
            }
            return Ok(());
        }
        EXPAND1_FLAG.with(|f| f.set(true));
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
    // Adapters resolving a relative window (`since=30m`) read the
    // same instant, so a mount is as reproducible as the query.
    quarb::set_invocation_instant(now.0, now.1);

    // The session's sentence bonds, from every --desm file.
    if !cli.desm.is_empty() {
        let mut bonds = syndesmos::Syndesmos::empty();
        for f in &cli.desm {
            bonds.extend(
                syndesmos::Syndesmos::load(f)
                    .map_err(|e| anyhow::anyhow!("reading {}: {e}", f.display()))?,
            );
        }
        quarb::set_sentence_bonds(Some(Rc::new(bonds)));
    }

    // A --model file declares derived arbor structure; parse it once
    // and `run` wraps every mounted source in the enrichment layer.
    if let Some(model_path) = &cli.model {
        let model = quarb_model::parse_model_file(model_path)
            .map_err(|e| anyhow::anyhow!("parsing model {}: {e}", model_path.display()))?;
        // A model's `mount` statements name sources it opens itself:
        // inject them as `NAME=TARGET` inputs, resolving relative
        // targets against the model file's directory. They lead, so
        // any positional CLI targets merge after (a collision under
        // the multi-mount root is refused there).
        let base_dir = model_path.parent();
        for m in &model.mounts {
            let target = quarb_model::resolve_mount_target(&m.target, base_dir);
            cli.paths
                .insert(0, PathBuf::from(format!("{}={}", m.name, target)));
        }
        // A model's `def`/`macro` statements are its domain's
        // vocabulary: in scope for the query too, ahead of any
        // --defs file (prepended after it, so they read first).
        if !model.defs_text.trim().is_empty() {
            quarb::parse_defs(&model.defs_text)
                .with_context(|| format!("parsing definitions in {}", model_path.display()))?;
            cli.query = format!("{}\n{}", model.defs_text, cli.query);
        }
        MODEL.with(|m| *m.borrow_mut() = Some(model));
        // --expand with a model that mounts nothing: the pure
        // expansion, now with the model's definitions in scope.
        if cli.expand && cli.paths.is_empty() {
            println!(
                "{}",
                quarb::expand(&cli.query, &quarb::Defs::default())
                    .context("expanding the query")?
            );
            return Ok(());
        }
    }

    // Enable the AST cache before dispatch, so both a normal run and
    // a resident daemon's per-query parses consult it.
    if cli.cache || cli.cache_dir.is_some() {
        let dir = cli
            .cache_dir
            .clone()
            .unwrap_or_else(quarb_tree_sitter::Cache::default_dir);
        quarb_tree_sitter::set_cache(Some(quarb_tree_sitter::Cache::new(dir)));
    }

    if cli.resident || cli.resident_serve {
        anyhow::ensure!(
            !cli.kaiv && cli.save.is_none() && !cli.expand && !cli.expand_1,
            "--resident does not combine with --kaiv/--save/--expand"
        );
        anyhow::ensure!(
            !cli.paths.is_empty(),
            "--resident needs file/directory inputs (stdin has no session identity)"
        );
        #[cfg(not(unix))]
        anyhow::bail!("--resident needs Unix domain sockets (unavailable on this platform)");
    }
    #[cfg(unix)]
    {
        if cli.resident && !cli.resident_serve {
            return resident_client(&cli);
        }
        if cli.resident_serve {
            let sock = resident_socket(&cli)?;
            RESIDENT.with(|r| *r.borrow_mut() = Some((sock, cli.resident_ttl, cli.now.is_some())));
        }
    }
    execute(&cli, &cli.query)
}

// ---------------------------------------------------------------------------
// Resident sessions: a background qua keeps the materialized
// adapter alive; clients send queries over a Unix socket and read
// framed results. The protocol is deliberately tiny:
//   client → "Q <len>\n" + <len bytes of query text>
//   server → "R <len> <status>\n" + <len bytes>  (status 0 = ok)
// ---------------------------------------------------------------------------

/// The session socket: keyed by the canonical target set plus every
/// flag that changes query semantics, so different views of the
/// same tree get different sessions.
#[cfg(unix)]
fn resident_socket(cli: &Cli) -> anyhow::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in &cli.paths {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.clone())
            .hash(&mut h);
    }
    (
        cli.graft,
        cli.no_graft,
        cli.hidden,
        cli.no_ignore,
        cli.allow_shell,
        cli.quantifier_bound,
        &cli.now,
        &cli.refs,
        &cli.defs,
        &cli.model,
        cli.no_pushdown,
    )
        .hash(&mut h);
    let dir = resident_dir()?;
    Ok(dir.join(format!("quarb-{:016x}.sock", h.finish())))
}

/// The directory holding session sockets. $XDG_RUNTIME_DIR is
/// per-user and 0700; without it, fall back to a per-uid 0700
/// subdirectory of the temp dir — never a world-writable directory
/// directly, where the predictable socket name could be squatted
/// by another local user. The fallback dir is verified to be ours
/// (owned by this uid, mode 0700, not a symlink): a pre-created
/// impostor directory would let its owner remove or replace live
/// sockets, so an unverifiable dir is a hard error rather than a
/// quiet risk.
#[cfg(unix)]
fn resident_dir() -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(d));
    }
    let uid = unsafe { libc::getuid() };
    let d = std::env::temp_dir().join(format!("quarb-{uid}"));
    let _ = std::fs::create_dir(&d);
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    let ok = std::fs::symlink_metadata(&d).is_ok_and(|m| {
        m.file_type().is_dir() && m.uid() == uid && m.permissions().mode() & 0o777 == 0o700
    });
    anyhow::ensure!(
        ok,
        "{} is not a private directory owned by this user \
         (another user may have created it); remove it or set \
         XDG_RUNTIME_DIR to use resident sessions",
        d.display()
    );
    Ok(d)
}

/// Client side: connect to the session (starting it if needed),
/// send the query, stream the result.
#[cfg(unix)]
fn resident_client(cli: &Cli) -> anyhow::Result<()> {
    use std::io::Write as _;
    let sock = resident_socket(cli)?;
    let mut stream = match std::os::unix::net::UnixStream::connect(&sock) {
        Ok(s) => s,
        // No live session. The server owns stale-socket cleanup
        // (removing here would race a concurrent client into
        // orphaning a daemon that just bound).
        Err(_) => spawn_resident(&sock)?,
    };
    let q = cli.query.as_bytes();
    stream.write_all(format!("Q {}\n", q.len()).as_bytes())?;
    stream.write_all(q)?;
    stream.flush()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut header = String::new();
    std::io::BufRead::read_line(&mut reader, &mut header)?;
    let mut parts = header.trim_end().split(' ');
    anyhow::ensure!(
        parts.next() == Some("R"),
        "bad session response: {header:?}"
    );
    let len: usize = parts
        .next()
        .and_then(|s| s.parse().ok())
        .context("bad session response length")?;
    let status: u8 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .context("bad session response status")?;
    let mut body = vec![0u8; len];
    std::io::Read::read_exact(&mut reader, &mut body)?;
    if status == 0 {
        std::io::stdout().write_all(&body)?;
        Ok(())
    } else {
        anyhow::bail!("{}", String::from_utf8_lossy(&body));
    }
}

/// Start the session daemon (this binary, same arguments, plus the
/// internal serve flag), detach it from the terminal, and wait for
/// its socket — the wait covers materialization, which for a large
/// tree is exactly the cost the session exists to amortize.
#[cfg(unix)]
fn spawn_resident(sock: &std::path::Path) -> anyhow::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::process::CommandExt as _;
    let log = sock.with_extension("log");
    let logfile =
        std::fs::File::create(&log).with_context(|| format!("creating {}", log.display()))?;
    let exe = std::env::current_exe().context("resolving qua binary")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(std::env::args_os().skip(1))
        .arg("--resident-serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(logfile));
    // A session of its own: survives this client and its terminal.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("starting resident session")?;
    eprintln!(
        "resident session starting (first query pays materialization; \
         log: {})",
        log.display()
    );
    let started = std::time::Instant::now();
    let mut last_note = 0u64;
    loop {
        if let Ok(s) = std::os::unix::net::UnixStream::connect(sock) {
            return Ok(s);
        }
        if let Some(status) = child.try_wait()? {
            // A clean exit can mean our spawn lost a race and
            // deferred to an already-live session — connect to it.
            if let Ok(s) = std::os::unix::net::UnixStream::connect(sock) {
                return Ok(s);
            }
            let tail = std::fs::read_to_string(&log).unwrap_or_default();
            let tail = tail.lines().rev().take(5).collect::<Vec<_>>();
            anyhow::bail!(
                "resident session exited ({status}) before binding its socket:\n{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        let elapsed = started.elapsed().as_secs();
        if elapsed >= last_note + 15 {
            eprintln!("  … materializing ({elapsed}s)");
            last_note = elapsed;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// The largest query frame a session accepts. Query text is typed
/// by a person; the cap only exists so a garbled length header
/// cannot make the daemon allocate gigabytes.
#[cfg(unix)]
const RESIDENT_MAX_QUERY: usize = 1 << 20;

/// Server side: bind the socket and answer queries against the
/// standing adapter until the idle TTL expires. Queries run
/// serially; each failure answers that client and the session
/// lives on.
#[cfg(unix)]
fn resident_serve_loop<A: AstAdapter>(
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    sock: &std::path::Path,
    ttl: u64,
    now_pinned: bool,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    // Clients can vanish mid-response (Ctrl-C, a closed pipe on
    // their stdout): the write then raises SIGPIPE, and the SIG_DFL
    // disposition cli_main restores (right for the one-shot filter)
    // would kill the whole session — and its materialization — with
    // it. Ignore it here; the write fails with EPIPE instead, which
    // the per-client `let _ =` absorbs.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    // Exclusive bind. When the path is taken, probe it: a live
    // daemon answering means another spawn won the race — defer to
    // it and exit, instead of unbinding it and idling as an
    // unreachable copy of the (possibly huge) materialization.
    // Only a dead socket (connect refused) is stale and removable.
    let listener = match std::os::unix::net::UnixListener::bind(sock) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if std::os::unix::net::UnixStream::connect(sock).is_ok() {
                eprintln!("resident session already live; deferring to it");
                return Ok(());
            }
            let _ = std::fs::remove_file(sock);
            std::os::unix::net::UnixListener::bind(sock)
                .with_context(|| format!("binding {}", sock.display()))?
        }
        Err(e) => return Err(e).with_context(|| format!("binding {}", sock.display())),
    };
    // Belt over the 0700 directory: the socket itself is private.
    let _ = std::fs::set_permissions(sock, {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::Permissions::from_mode(0o600)
    });
    listener.set_nonblocking(true)?;
    let mut idle = std::time::Instant::now();
    loop {
        match listener.accept() {
            Ok((mut conn, _)) => {
                idle = std::time::Instant::now();
                conn.set_nonblocking(false)?;
                // A stalled client (stopped, wedged) must not hang
                // the serial loop past the TTL's reach.
                let _ = conn.set_read_timeout(Some(std::time::Duration::from_secs(30)));
                let _ = conn.set_write_timeout(Some(std::time::Duration::from_secs(30)));
                let mut reader = std::io::BufReader::new(conn.try_clone()?);
                let mut header = String::new();
                if std::io::BufRead::read_line(&mut reader, &mut header).is_err() {
                    continue;
                }
                let len: usize = match header
                    .trim_end()
                    .strip_prefix("Q ")
                    .and_then(|s| s.parse().ok())
                {
                    Some(n) if n <= RESIDENT_MAX_QUERY => n,
                    Some(_) => {
                        let msg = b"query exceeds the resident frame limit";
                        let _ = conn.write_all(format!("R {} 1\n", msg.len()).as_bytes());
                        let _ = conn.write_all(msg);
                        continue;
                    }
                    None => continue,
                };
                let mut qbytes = vec![0u8; len];
                if std::io::Read::read_exact(&mut reader, &mut qbytes).is_err() {
                    continue;
                }
                let query = String::from_utf8_lossy(&qbytes).into_owned();
                // Each query is its own invocation instant unless
                // the session was pinned with --now.
                if !now_pinned {
                    let since = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default();
                    NOW_INSTANT.with(|c| c.set((since.as_secs() as i64, since.subsec_nanos())));
                    quarb::set_invocation_instant(since.as_secs() as i64, since.subsec_nanos());
                }
                let (result, output) =
                    with_stdout_capture(|| run_wrapped(&query, adapter, &render, None));
                let (status, body) = match result {
                    Ok(()) => (0u8, output),
                    Err(e) => (1u8, format!("{e:#}").into_bytes()),
                };
                let _ = conn.write_all(format!("R {} {}\n", body.len(), status).as_bytes());
                let _ = conn.write_all(&body);
                let _ = conn.flush();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if idle.elapsed().as_secs() >= ttl {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("resident session accept error: {e}");
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
    let _ = std::fs::remove_file(sock);
    Ok(())
}

/// Run `f` with stdout captured to a byte buffer (fd-level, so the
/// existing print-based output paths need no plumbing, and
/// non-UTF-8 output survives verbatim). Queries in a session run
/// serially, which keeps the fd dance safe.
#[cfg(unix)]
fn with_stdout_capture<R>(f: impl FnOnce() -> R) -> (R, Vec<u8>) {
    use std::io::{Read as _, Seek as _, Write as _};
    use std::os::fd::AsRawFd as _;
    let _ = std::io::stdout().flush();
    let mut tmp = match tempfile_in_temp() {
        Ok(t) => t,
        Err(_) => return (f(), Vec::new()),
    };
    let saved = unsafe { libc::dup(1) };
    if saved < 0 {
        return (f(), Vec::new());
    }
    unsafe { libc::dup2(tmp.as_raw_fd(), 1) };
    let r = f();
    let _ = std::io::stdout().flush();
    unsafe {
        libc::dup2(saved, 1);
        libc::close(saved);
    }
    let mut out = Vec::new();
    let _ = tmp.seek(std::io::SeekFrom::Start(0));
    let _ = tmp.read_to_end(&mut out);
    (r, out)
}

/// An anonymous scratch file for the capture (unlinked at once).
#[cfg(unix)]
fn tempfile_in_temp() -> std::io::Result<std::fs::File> {
    let path = std::env::temp_dir().join(format!(
        "quarb-capture-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let f = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(f)
}

/// Read and parse the `--refs` document into the relational form,
/// `(field, container)` pairs. (The Firebase adapter parses the
/// same file into its own path-shaped form.) Empty when no --refs.
fn relational_refs(refs: &Option<PathBuf>) -> anyhow::Result<Vec<(String, String)>> {
    match refs {
        Some(f) => {
            let text = std::fs::read_to_string(f)
                .with_context(|| format!("reading refs file {}", f.display()))?;
            quarb_relational::parse_refs(&text).map_err(|e| anyhow::anyhow!("parsing refs: {e}"))
        }
        None => Ok(Vec::new()),
    }
}

/// Run one query text against the CLI's inputs, printing results —
/// the whole adapter dispatch.
fn execute(cli: &Cli, query: &str) -> anyhow::Result<()> {
    // A refs document only means something to an adapter that
    // consumes it; passing one alongside targets that all ignore it
    // deserves a loud note, not silence.
    if cli.refs.is_some() {
        let consumes = |p: &PathBuf| {
            let target = split_alias(p).map(|(_, t)| t).unwrap_or_else(|| p.clone());
            is_sqlite(&target)
                || target
                    .to_str()
                    .is_some_and(|s| s.starts_with("firebase://"))
        };
        if !cli.paths.iter().any(consumes) {
            eprintln!(
                "qua: --refs: no target consumes a declared-references document \
                 (SQLite databases and firebase:// do); ignoring it"
            );
        }
    }
    // Several inputs are mounted as named children of one root; a
    // single `NAME=TARGET` input mounts too, so its name is real.
    if cli.paths.len() >= 2 || cli.paths.iter().any(|p| split_alias(p).is_some()) {
        let mut mounts: Vec<Mount> = Vec::new();
        let mut renders: Vec<Box<dyn Fn(NodeId) -> String>> = Vec::new();
        for (i, p) in cli.paths.iter().enumerate() {
            let (name, target) = match split_alias(p) {
                Some(alias) => alias,
                None => (
                    p.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| format!("doc{i}")),
                    p.clone(),
                ),
            };
            // Mounts are addressed by name, so two inputs sharing one
            // would silently union under it with no way to target
            // either — refuse rather than merge distinct sources.
            if mounts.iter().any(|m| m.name == name) {
                anyhow::bail!(
                    "input '{}' mounts as '{name}', colliding with an earlier input of the \
                     same name; give one an explicit alias (NAME=TARGET)",
                    p.display()
                );
            }
            let (adapter, render) = open_mount(&target, cli)?;
            mounts.push(Mount {
                name,
                target: Some(target.display().to_string()),
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
            cli.kaiv.then_some(sources.as_str()),
        );
    }
    let path = cli.paths.first().cloned();

    // A `lines:` prefix mounts a file as line atoms — every line
    // a node, `<blank>` traited, totals on the root. The reading
    // wc/grep -c/cloc assume, given an arbor.
    if let Some(p) = &path
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("lines:"))
        && !rest.is_empty()
    {
        let target = Path::new(rest);
        let text = std::fs::read_to_string(target)
            .with_context(|| format!("reading {}", target.display()))?;
        let adapter = quarb_lines::LinesAdapter::parse(&text);
        let src = target.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }
    // A `text:` prefix forces the text-level reading of a document
    // whose extension would otherwise pick the DOM-level adapter
    // (html, md); the producer is chosen by the remaining
    // extension, with `<` sniffing markup for the rest and plain
    // paragraphs as the fallback.
    // A `koine:` prefix takes the koine route to the same
    // reader's model: native atrep formats plus everything
    // atrep's endomorphosis imports; `?format=` forces one.
    if let Some(p) = &path
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("koine:"))
        && !rest.is_empty()
    {
        let adapter = koine_level(rest)?;
        let src = rest.to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }
    // The web level: a site — a directory of built pages or an
    // archive of them — as `/sites/<host>/pages/…`, each page
    // grafted at the text level on entry.
    if let Some(p) = &path
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("web:"))
        && !rest.is_empty()
    {
        let src = rest.to_string();
        return match web_level(rest)? {
            WebSite::Memory(adapter) => run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
            ),
            WebSite::Sqlite(store) => run_web_store(cli, query, store, &src),
            WebSite::Postgres(store) => run_web_store(cli, query, store, &src),
        };
    }
    // The literary reading: the corpus reading with the inline
    // simmeres kept.
    if let Some(p) = &path
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("lit:"))
        && !rest.is_empty()
    {
        let src = rest.split('?').next().unwrap_or(rest).to_string();
        if let Some((dir, opts)) = document_dir(rest) {
            let adapter = document_folder(
                dir,
                cli.hidden,
                cli.no_ignore,
                folder_reader("lit", opts, cli.allow_shell),
            )?;
            return run(
                query,
                &adapter,
                |n| adapter.locator(n, |o| adapter.outer().path(o).display().to_string()),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        let adapter = lit_level(rest, cli.allow_shell)?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }
    // The corpus reading: the text level plus tokens (ruling #62).
    if let Some(p) = &path
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("corpus:"))
        && !rest.is_empty()
    {
        let src = rest.split('?').next().unwrap_or(rest).to_string();
        if let Some((dir, opts)) = document_dir(rest) {
            let adapter = document_folder(
                dir,
                cli.hidden,
                cli.no_ignore,
                folder_reader("corpus", opts, cli.allow_shell),
            )?;
            return run(
                query,
                &adapter,
                |n| adapter.locator(n, |o| adapter.outer().path(o).display().to_string()),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        let adapter = corpus_level(rest, cli.allow_shell)?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }
    if let Some(p) = &path
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("text:"))
        && !rest.is_empty()
    {
        let target = Path::new(rest);
        // A directory of documents: the folder tree with every
        // document leaf (.md, .html, .txt) read at the text level,
        // as an archive's members are — `/*` the files, sections
        // and paragraphs beneath each.
        if target.is_dir() && !holds_treebank(target) {
            let adapter = document_folder(
                target,
                cli.hidden,
                cli.no_ignore,
                folder_reader("text", None, cli.allow_shell),
            )?;
            let src = target.display().to_string();
            return run(
                query,
                &adapter,
                |n| adapter.locator(n, |o| adapter.outer().path(o).display().to_string()),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        // A directory of treebank files reads as one document, a
        // section per file (ruling #63; `corpus:` adds the tokens).
        if target.is_dir() {
            let adapter = conllu_dir_text(target)?;
            let src = target.display().to_string();
            return run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        // An archive at the text level: every marked-up member
        // grafts as the reader's model — a site's pages as
        // sections and paragraphs, each page wearing what its
        // head declares. A container with a producer of its own
        // (.docx, .epub) is not an archive here: it is a document.
        if is_archive(target) && binary_text_kind(target).is_none() {
            let adapter =
                ComposeAdapter::new(ArchiveAdapter::open(target).context("opening archive")?)
                    .with_document_graft(DocumentGraft::Text);
            let src = target.display().to_string();
            return run(
                query,
                &adapter,
                |n| adapter.locator(n, |o| adapter.outer().locator(o)),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        // A native atrep file is already koine: the two prefixes
        // converge, and text: aliases to the koine route.
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("atd") || e.eq_ignore_ascii_case("atk"))
        {
            let adapter = koine_level(rest)?;
            let src = target.display().to_string();
            return run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        // The binary producers dispatch before the text read: a
        // .docx is a zip, not UTF-8. (Plain `report.docx` keeps
        // its archive reading — three readings, one file.)
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            let bytes =
                std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
            let adapter = quarb_text_pdf::parse(&bytes)
                .with_context(|| format!("reading {} as PDF", target.display()))?;
            let src = target.display().to_string();
            return run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        if let Some(kind) = binary_text_kind(target) {
            let bytes =
                std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
            let adapter = binary_text_level(kind, &bytes)
                .with_context(|| format!("reading {}", target.display()))?;
            let src = target.display().to_string();
            return run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        // BibTeX / BibLaTeX: atrep's importer parses it into
        // bibliogramma, and the entries land as bib blocks.
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("bib"))
        {
            let text = std::fs::read_to_string(target)
                .with_context(|| format!("reading {}", target.display()))?;
            let adapter = quarb_text_koine::parse_bibtex(&text).with_context(|| {
                format!(
                    "reading {} as BibTeX through bibliogramma",
                    target.display()
                )
            })?;
            let src = target.display().to_string();
            return run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        let text = read_prose(target)?;
        let mut adapter = text_level(&text, Some(target))?;
        // The base `::href` links against, when the page declares
        // no canonical URL of its own.
        adapter.set_document_path(&target.display().to_string());
        let src = target.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }
    // A `code:` prefix forces the code-level reading of source
    // whose extension would otherwise pick the syntax level:
    // declared identifiers as node names (`//lex`, not
    // `//function_item[::name = "lex"]`). On a directory it
    // implies the composed, grafted view with every supported
    // source leaf grafted at the code level — the prefix has no
    // other meaning there — so `qua --graft '//lex' code:src/`
    // and the flagless spelling agree.
    if let Some(p) = &path
        && let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("code:"))
        && !rest.is_empty()
    {
        if cli.no_graft {
            anyhow::bail!(
                "--no-graft refuses the code: prefix: the prefix's whole \
                 meaning is the grafted code-level view"
            );
        }
        let target = Path::new(rest);
        let src = target.display().to_string();
        if target.is_dir() {
            let opts = FsOptions {
                hidden: cli.hidden,
                respect_ignore: !cli.no_ignore,
            };
            let adapter = ComposeAdapter::with_source_paths(
                FsAdapter::with_options(target, opts)?,
                |fs, n| Some(fs.path(n)),
            )
            .with_source_graft(SourceGraft::Code);
            return run(
                query,
                &adapter,
                |n| adapter.locator(n, |o| adapter.outer().path(o).display().to_string()),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        let adapter = quarb_code::CodeModel::open(target)
            .with_context(|| format!("parsing {} at the code level", target.display()))?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }

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
        if cli.graft {
            let adapter =
                ComposeAdapter::with_source_paths(FsAdapter::with_options(path, opts)?, |fs, n| {
                    Some(fs.path(n))
                });
            return run(
                query,
                &adapter,
                |n| adapter.locator(n, |o| adapter.outer().path(o).display().to_string()),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        let adapter = FsAdapter::with_options(path, opts)?;
        return run(
            query,
            &adapter,
            |n| adapter.path(n).display().to_string(),
            cli.kaiv.then_some(src.as_str()),
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
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
        );
    }

    // GitHub, through the gh CLI: github:[OWNER[/REPO]].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("github:")
    {
        let adapter = GithubAdapter::connect(s).context("connecting to GitHub")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // GitLab, through the glab CLI: gitlab:[PATH] (a group,
    // project, or user namespace — groups nest arbitrarily).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("gitlab:")
    {
        let adapter = GitlabAdapter::connect(s).context("connecting to GitLab")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // A Kubernetes cluster, through kubectl: k8s:[CONTEXT].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("k8s:") || s.starts_with("kubernetes:"))
    {
        let adapter = KubernetesAdapter::connect(s).context("connecting to Kubernetes")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Cloud Logging: gcl:PROJECT?since=1h&filter=…&limit=N — a
    // bounded snapshot, through gcloud.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("gcl:") || s.starts_with("gcplogs:"))
    {
        let adapter = GclAdapter::open(s).context("reading Cloud Logging")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // CloudWatch Logs: cwl:[GROUP]?since=1h&filter=…&limit=N —
    // the same bounded snapshot over SigV4.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("cwl:") || s.starts_with("cloudwatch:"))
    {
        let adapter = CwlAdapter::open(s).context("reading CloudWatch Logs")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Datadog Logs: ddl:?since=1h&query=…&limit=N.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("ddl:") || s.starts_with("datadog:"))
    {
        let adapter = DdlAdapter::open(s).context("reading Datadog Logs")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Azure Monitor Logs: azl:WORKSPACE?table=…&since=1h&limit=N.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("azl:") || s.starts_with("azlogs:"))
    {
        let adapter = AzlAdapter::open(s).context("reading Azure Monitor Logs")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Cloudflare edge logs: cfl:ZONE?since=1h&limit=N.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("cfl:") || s.starts_with("cflogs:"))
    {
        let adapter = CflAdapter::open(s).context("reading Cloudflare logs")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // A MongoDB database: a standard connection string with the
    // database as the path.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("mongodb://") || s.starts_with("mongodb+srv://"))
    {
        let adapter = MongodbAdapter::connect(s).context("connecting to MongoDB")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // DynamoDB: dynamodb://[REGION][?endpoint=URL].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("dynamodb:")
    {
        let adapter = DynamodbAdapter::connect(s).context("connecting to DynamoDB")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Neptune: neptune://HOST[?region&key&endpoint].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("neptune://")
    {
        let adapter = NeptuneAdapter::connect(s).context("connecting to Neptune")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Redis: redis://HOST[:PORT][/DB][?scan=GLOB].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("redis://")
    {
        let adapter = RedisAdapter::connect(s).context("connecting to Redis")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Redis TLS: rediss:// variant.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("rediss://")
    {
        let adapter = RedisAdapter::connect(s).context("connecting to Redis")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // FalkorDB: falkor://HOST[:PORT]/GRAPH[?key=].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("falkor://")
    {
        let adapter = FalkorAdapter::connect(s).context("connecting to FalkorDB")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Memgraph: memgraph://HOST[:7687][?key=].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("memgraph://")
    {
        let adapter = MemgraphAdapter::connect(s).context("connecting to Memgraph")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // ArangoDB: arango://USER:PASS@HOST[:8529]/DB.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("arango://")
    {
        let adapter = ArangoAdapter::connect(s).context("connecting to ArangoDB")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // SPARQL: sparql:URL[#limit&key&lang].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("sparql:")
    {
        let adapter = SparqlAdapter::connect(s).context("connecting to the SPARQL endpoint")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Apache AGE: age://[USER[:PASS]@]HOST[:PORT]/DB/GRAPH.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("age://")
    {
        let adapter = AgeAdapter::connect(s).context("connecting to AGE")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Kùzu: kuzu:PATH (opt-in: built with --features kuzu).
    #[cfg(feature = "kuzu")]
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("kuzu:")
    {
        let adapter = KuzuAdapter::open(s).context("opening Kuzu database")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Kafka: kafka://HOST:PORT[,…][?topics=…&from=…&until=…].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("kafka:")
    {
        let adapter = KafkaAdapter::connect(s).context("connecting to Kafka")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Cosmos DB: cosmos://ACCOUNT/DATABASE[?endpoint=URL].
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("cosmos://")
    {
        let adapter = CosmosAdapter::connect(s).context("connecting to Cosmos DB")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // A SQL Server database: mssql://USER:PASS@HOST[:PORT]/DB.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("mssql://")
    {
        if let Some(plan) = pushdown_plan(cli, query, Some(quarb_sql::Dialect::Mssql)) {
            match quarb_mssql::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query, Some(quarb_sql::Dialect::Mssql)) {
            Some(pl) => {
                let a = MssqlAdapter::connect_filtered(s, &pl.table, &pl.where_sql)
                    .context("connecting to SQL Server")?;
                match a.prefetch(&pl.table) {
                    Ok(()) => a,
                    Err(e) => {
                        if cli.explain {
                            eprintln!("partial pushdown: prefilter rejected ({e}); scanning");
                        }
                        MssqlAdapter::connect(s).context("connecting to SQL Server")?
                    }
                }
            }
            None => MssqlAdapter::connect(s).context("connecting to SQL Server")?,
        };
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // An Oracle database: oracle://USER:PASS@HOST[:PORT]/SERVICE.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("oracle://")
    {
        if let Some(plan) = pushdown_plan(cli, query, Some(quarb_sql::Dialect::Oracle)) {
            match quarb_oracle::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query, Some(quarb_sql::Dialect::Oracle)) {
            Some(pl) => {
                let a = OracleAdapter::connect_filtered(s, &pl.table, &pl.where_sql)
                    .context("connecting to Oracle")?;
                match a.prefetch(&pl.table) {
                    Ok(()) => a,
                    Err(e) => {
                        if cli.explain {
                            eprintln!("partial pushdown: prefilter rejected ({e}); scanning");
                        }
                        OracleAdapter::connect(s).context("connecting to Oracle")?
                    }
                }
            }
            None => OracleAdapter::connect(s).context("connecting to Oracle")?,
        };
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // An LDAP directory: ldap[s]://[USER:PASS@]HOST[:PORT]/BASE_DN.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("ldap://") || s.starts_with("ldaps://"))
    {
        let adapter = LdapAdapter::connect(s).context("connecting to LDAP")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
        );
    }

    // A metatheca vault: `metatheca:PATH` or `mt:PATH` (the vault
    // root — the directory holding `cella/`).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && let Some(vault) = s
            .strip_prefix("metatheca:")
            .or_else(|| s.strip_prefix("mt:"))
    {
        let adapter = MetathecaAdapter::open(std::path::Path::new(vault))
            .context("opening metatheca vault")?;
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
        );
    }

    // A BigQuery target connects and introspects the dataset.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("bigquery://")
    {
        if let Some(plan) = pushdown_plan(cli, query, None) {
            match quarb_bigquery::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    // The plan can fail catalog-side checks (e.g. the
                    // witness-JOIN uniqueness obligation): fall back to
                    // the scan, but never silently under --explain.
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query, None) {
            Some(pl) => {
                let a = BigqueryAdapter::connect_filtered(s, &pl.table, &pl.where_sql)
                    .context("connecting to BigQuery")?;
                match a.prefetch(&pl.table) {
                    Ok(()) => a,
                    Err(e) => {
                        if cli.explain {
                            eprintln!("partial pushdown: prefilter rejected ({e}); scanning");
                        }
                        BigqueryAdapter::connect(s).context("connecting to BigQuery")?
                    }
                }
            }
            None => BigqueryAdapter::connect(s).context("connecting to BigQuery")?,
        };
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // Athena: the S3 datalake's query layer. Billed by bytes
    // scanned, so the same ladder as BigQuery: full pushdown,
    // else a filtered fetch, else the lazy scan.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("athena:")
    {
        if let Some(plan) = pushdown_plan(cli, query, None) {
            match quarb_athena::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query, None) {
            Some(pl) => {
                let a = AthenaAdapter::connect_filtered(s, &pl.table, &pl.where_sql)
                    .context("connecting to Athena")?;
                match a.prefetch(&pl.table) {
                    Ok(()) => a,
                    Err(e) => {
                        if cli.explain {
                            eprintln!("partial pushdown: prefilter rejected ({e}); scanning");
                        }
                        AthenaAdapter::connect(s).context("connecting to Athena")?
                    }
                }
            }
            None => AthenaAdapter::connect(s).context("connecting to Athena")?,
        };
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // A MySQL/MariaDB URL connects and introspects the database.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("mysql://")
    {
        if let Some(plan) = pushdown_plan(cli, query, Some(quarb_sql::Dialect::MySql)) {
            match quarb_mysql::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    // The plan can fail catalog-side checks (e.g. the
                    // witness-JOIN uniqueness obligation): fall back to
                    // the scan, but never silently under --explain.
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query, Some(quarb_sql::Dialect::MySql)) {
            Some(pl) => {
                let a = MysqlAdapter::connect_filtered(s, &pl.table, &pl.where_sql)
                    .context("connecting to MySQL")?;
                match a.prefetch(&pl.table) {
                    Ok(()) => a,
                    Err(e) => {
                        if cli.explain {
                            eprintln!("partial pushdown: prefilter rejected ({e}); scanning");
                        }
                        MysqlAdapter::connect(s).context("connecting to MySQL")?
                    }
                }
            }
            None => MysqlAdapter::connect(s).context("connecting to MySQL")?,
        };
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(s),
        );
    }

    // A PostgreSQL connection string connects and materializes the
    // public schema (postgres:// / postgresql:// URL, or the
    // keyword form starting with host=).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && is_pg_config(s)
    {
        if let Some(plan) = pushdown_plan(cli, query, Some(quarb_sql::Dialect::Postgres)) {
            match quarb_postgres::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    // The plan can fail catalog-side checks (e.g. the
                    // witness-JOIN uniqueness obligation): fall back to
                    // the scan, but never silently under --explain.
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query, Some(quarb_sql::Dialect::Postgres)) {
            Some(pl) => {
                let a = PostgresAdapter::connect_filtered(s, &pl.table, &pl.where_sql)
                    .context("connecting to PostgreSQL")?;
                match a.prefetch(&pl.table) {
                    Ok(()) => a,
                    Err(e) => {
                        if cli.explain {
                            eprintln!("partial pushdown: prefilter rejected ({e}); scanning");
                        }
                        PostgresAdapter::connect(s).context("connecting to PostgreSQL")?
                    }
                }
            }
            None => PostgresAdapter::connect(s).context("connecting to PostgreSQL")?,
        };
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
        );
    }

    // Object stores (gs:// / s3://), composed by default —
    // grafting a bucket of JSON/CSV/source files is the point.
    // --no-graft holds the objects opaque (names, sizes, sums).
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("gs://") || s.starts_with("s3://") || s.starts_with("az://"))
    {
        if cli.no_graft {
            let adapter = ObjstoreAdapter::connect(s).context("connecting to bucket")?;
            return run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(s),
            );
        }
        let adapter =
            ComposeAdapter::new(ObjstoreAdapter::connect(s).context("connecting to bucket")?);
        return run(
            query,
            &adapter,
            |n| adapter.locator(n, |o| adapter.outer().locator(o)),
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
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
            cli.kaiv.then_some(s),
        );
    }

    // Source code: files with a tree-sitter grammar parse into
    // their syntax tree.
    if let Some(p) = &path
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| quarb_tree_sitter::supported(&e.to_ascii_lowercase()))
    {
        let adapter = TreeSitterAdapter::open(p).context("parsing source file")?;
        let src = p.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
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
            cli.kaiv.then_some(src.as_str()),
        );
    }

    // DuckDB databases, by extension.
    if let Some(p) = &path
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("duckdb") || e.eq_ignore_ascii_case("ddb"))
    {
        if let Some(plan) = pushdown_plan(cli, query, None) {
            match quarb_duckdb::raw_query(
                p,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    // The plan can fail catalog-side checks (e.g. the
                    // witness-JOIN uniqueness obligation): fall back to
                    // the scan, but never silently under --explain.
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = DuckdbAdapter::open(p).context("opening DuckDB database")?;
        let src = p.display().to_string();
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }

    // A PDF is its own object graph (quarb-pdf): plain file.pdf
    // opens the internals reading, text:file.pdf the reader's
    // model — the two-level split, as html/DOM vs text-html.
    if let Some(p) = &path
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        let adapter = quarb_pdf::PdfAdapter::load(&bytes)
            .with_context(|| format!("reading {} as PDF", p.display()))?;
        let src = p.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }

    // Archives are binary: dispatch before the text read (zip/PK
    // or gzip magic, or a .tar extension). Composition is on by
    // default — the point of opening a .docx is the XML inside.
    // --no-graft keeps the member tree with opaque leaves: the
    // tar -t view, for sizing and checksumming.
    if let Some(p) = &path
        && is_archive(p)
    {
        let src = p.display().to_string();
        if cli.no_graft {
            let adapter = ArchiveAdapter::open(p).context("opening archive")?;
            return run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
            );
        }
        let adapter = ComposeAdapter::new(ArchiveAdapter::open(p).context("opening archive")?);
        return run(
            query,
            &adapter,
            |n| adapter.locator(n, |o| adapter.outer().locator(o)),
            cli.kaiv.then_some(src.as_str()),
        );
    }

    // CBOR is binary: dispatch on the raw bytes before the text
    // read (extension-only — CBOR has no reliable magic).
    if let Some(p) = &path
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cbor"))
    {
        let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        let adapter = quarb_cbor::CborAdapter::parse(&bytes).context("parsing CBOR")?;
        let src = p.display().to_string();
        return run(
            query,
            &adapter,
            |n| adapter.pointer(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }

    // SQLite databases are binary: dispatch before the text read
    // (by extension, or the 16-byte magic).
    if let Some(p) = &path
        && is_sqlite(p)
    {
        if let Some(plan) = pushdown_plan(cli, query, Some(quarb_sql::Dialect::Sqlite)) {
            match quarb_sqlite::raw_query(
                p,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    // The plan can fail catalog-side checks (e.g. the
                    // witness-JOIN uniqueness obligation): fall back to
                    // the scan, but never silently under --explain.
                    if cli.explain {
                        eprintln!("pushdown: {}", plan.sql);
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let refs = relational_refs(&cli.refs)?;
        let adapter = match partial_plan(cli, query, Some(quarb_sql::Dialect::Sqlite)) {
            Some(pl) => {
                let a = SqliteAdapter::open_filtered_with_refs(p, &pl.table, &pl.where_sql, &refs)
                    .context("opening SQLite database")?;
                match a.prefetch(&pl.table) {
                    Ok(()) => a,
                    Err(e) => {
                        if cli.explain {
                            eprintln!("partial pushdown: prefilter rejected ({e}); scanning");
                        }
                        SqliteAdapter::open_with_refs(p, &refs)
                            .context("opening SQLite database")?
                    }
                }
            }
            None => SqliteAdapter::open_with_refs(p, &refs).context("opening SQLite database")?,
        };
        let src = p.display().to_string();
        return run_relational(
            adapter,
            cli.no_graft,
            query,
            |a, n| a.locator(n),
            cli.kaiv.then_some(src.as_str()),
        );
    }

    let (text, path) = match &path {
        Some(path) => (
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
            Some(path.as_path()),
        ),
        // No target and no pipe: an expression-headed query runs as
        // a calculator against a bare root; anything that navigates
        // is refused loudly (silent emptiness would read as data).
        None if std::io::stdin().is_terminal() => {
            if !quarb::is_calculator(&cli.query) {
                // A query that fails to parse should say so — not
                // masquerade as a missing target.
                quarb::expand(&cli.query, &quarb::Defs::default()).context("parsing the query")?;
                anyhow::bail!(
                    "no input: give a directory, a file, or pipe a document to \
                     stdin — an expression head '= expr' runs without one; a \
                     --model file opens its own sources with `mount NAME: target;`"
                );
            }
            ("{}".to_owned(), None)
        }
        None => {
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
    let kaiv = cli.kaiv.then_some(source.as_str());

    // A .quarb file holds a Quarb query: reflect it as an arbor and
    // query the query (extension-only, like CSV).
    if is_quarb(path) {
        let adapter = quarb::reflect::QueryArbor::parse(&text).context("parsing Quarb query")?;
        return run(query, &adapter, |n| adapter.locator(n), kaiv);
    }
    // CSV/TSV are extension-only (tabular text is not sniffable).
    if let Some(delim) = csv_delimiter(path) {
        let adapter = CsvAdapter::parse_with_delimiter(&text, delim).context("parsing CSV")?;
        return run(query, &adapter, |n| adapter.locator(n), kaiv);
    }
    // YAML/TOML are extension-only (both share the JSON model).
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        let ext = ext.as_str();
        if matches!(ext, "yaml" | "yml") {
            let adapter = quarb_yaml::parse(&text).context("parsing YAML")?;
            return run(query, &adapter, |n| adapter.pointer(n), kaiv);
        }
        if ext == "toml" {
            let adapter = quarb_toml::parse(&text).context("parsing TOML")?;
            return run(query, &adapter, |n| adapter.pointer(n), kaiv);
        }
        if matches!(ext, "md" | "markdown") {
            let adapter = quarb_markdown::parse(&text);
            return run(query, &adapter, |n| adapter.locator(n), kaiv);
        }
        // Plain text mounts at the text level: blank-line-separated
        // paragraphs (`text:` forces the same reading for html/md).
        if ext == "txt" {
            let adapter = quarb_text::TextModel::parse_plain(&text);
            return run(query, &adapter, |n| adapter.locator(n), kaiv);
        }
        // A treebank reads as a document (ruling #63); `corpus:`
        // adds its tokens.
        if matches!(ext, "conllu" | "conllup") {
            let adapter = quarb_text::TextModel::parse_conllu_text(&text)
                .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?;
            return run(query, &adapter, |n| adapter.locator(n), kaiv);
        }
        if matches!(ext, "jsonl" | "ndjson") {
            let adapter = JsonAdapter::parse_lines(&text).context("parsing JSONL")?;
            return run(query, &adapter, |n| adapter.pointer(n), kaiv);
        }
        // kaiv documents — the typed arbor whose namepaths ARE
        // quarb paths, so --kaiv output re-mounts (graft and join
        // over typed results). Extension picks the pipeline stage:
        // .kaiv is canonical, .daiv compiles first, .raiv
        // denormalizes its $field references.
        if matches!(ext, "daiv" | "kaiv" | "raiv") {
            let dir = path.and_then(|p| p.parent());
            let adapter = parse_kaiv_ext(ext, &text, dir)?;
            return run(query, &adapter, |n| adapter.locator(n), kaiv);
        }
        // atrep documents mount through the dialektos they
        // declare (.atd deltos, .atk kanon); the file's directory
        // anchors dialektos resolution, std definitions embedded.
        if matches!(ext, "atd" | "atk" | "usfm" | "sfm") {
            let dir = path.and_then(|p| p.parent()).unwrap_or(Path::new("."));
            let adapter = AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?;
            return run(query, &adapter, |n| adapter.locator(n), kaiv);
        }
    }
    if is_atrep(&text) {
        let dir = path
            .and_then(|p| p.parent())
            .unwrap_or_else(|| Path::new("."));
        let adapter = AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?;
        return run(query, &adapter, |n| adapter.locator(n), kaiv);
    }
    if is_xml(path, &text) {
        let adapter = XmlAdapter::parse(&text).context("parsing XML")?;
        run(query, &adapter, |n| adapter.locator(n), kaiv)
    } else if is_html(path, &text) {
        let adapter = HtmlAdapter::parse(&text);
        run(query, &adapter, |n| adapter.locator(n), kaiv)
    } else {
        // A whole-document parse first; a stream of per-line values
        // (JSONL — qua's own output shape) second, so results pipe
        // back in. The original error wins if neither reading fits.
        let adapter = match JsonAdapter::parse(&text) {
            Ok(a) => a,
            Err(e) => match JsonAdapter::parse_lines(&text) {
                Ok(a) => a,
                Err(_) => return Err(e).context("parsing JSON"),
            },
        };
        run(query, &adapter, |n| adapter.pointer(n), kaiv)
    }
}

/// Whether the input is a Quarb query file (`.quarb`), to be
/// reflected as a query arbor.
fn is_quarb(path: Option<&Path>) -> bool {
    path.and_then(Path::extension)
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("quarb"))
}

/// Whether pushdown applies: enabled, not emitting kaiv (which
/// needs node provenance), not in --expand mode, not saving, and
/// not resident. A resident daemon must reach the serve loop with
/// the unfiltered adapter: a first-query full pushdown would answer
/// and exit before binding the socket, and a partial pushdown would
/// bake its WHERE into the standing arbor every later query reuses.
fn pushdown_applies(cli: &Cli) -> bool {
    !cli.no_pushdown
        && !cli.kaiv
        && !EXPAND_FLAG.with(|f| f.get())
        && !EXPAND1_FLAG.with(|f| f.get())
        && cli.save.is_none()
        && !cli.resident
        && !cli.resident_serve
}

/// The partial-pushdown plan (a WHERE for one table's fetch), with
/// --explain commentary. Tried only after full pushdown refused.
fn partial_plan(
    cli: &Cli,
    query: &str,
    dialect: Option<quarb_sql::Dialect>,
) -> Option<quarb_sql::Partial> {
    if !pushdown_applies(cli) {
        return None;
    }
    match quarb_sql::partial_pushdown_explained(query, dialect) {
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
fn pushdown_plan(
    cli: &Cli,
    query: &str,
    dialect: Option<quarb_sql::Dialect>,
) -> Option<quarb_sql::Pushdown> {
    if !pushdown_applies(cli) {
        if cli.explain {
            eprintln!("pushdown: disabled; scanning");
        }
        return None;
    }
    match quarb_sql::pushdown_explained(query, dialect) {
        Ok(plan) => {
            if cli.explain {
                // The plan as exported. What actually ran prints
                // after execution (the driver resolves the ORDER
                // BY key from the catalog), verbatim.
                // What ran prints after execution (print_raw), so
                // the plan itself is only announced when it does
                // not run — the refusal branches below.
                EXPLAIN.with(|e| e.set(true));
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
/// values for one column, records for several. Buffered: one
/// flush at the end, not a syscall per line.
fn print_raw(cols: &[String], rows: Vec<Vec<Value>>) -> anyhow::Result<()> {
    use std::io::Write as _;
    // --explain: the statement the driver executed, verbatim,
    // ORDER BY and all. Recorded at execution because the key is
    // a per-adapter catalog lookup.
    if EXPLAIN.with(|e| e.get())
        && let Some(sql) = quarb_relational::take_executed()
    {
        eprintln!("pushdown: {sql}");
    }
    // The same printer as the scan path, so --json / --jsonl /
    // --table / --csv hold whether or not the plan was pushed down.
    let values: Vec<Value> = rows
        .into_iter()
        .flat_map(|row| {
            if cols.len() <= 1 {
                row
            } else {
                vec![Value::Record(cols.iter().cloned().zip(row).collect())]
            }
        })
        .collect();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    emit_values(&mut out, values, OUTPUT.with(|o| o.get()))?;
    out.flush()?;
    Ok(())
}

/// Whether the input names a PostgreSQL connection rather than a
/// file: a `postgres://` / `postgresql://` URL, or the keyword
/// form (`host=... dbname=...`).
fn is_pg_config(s: &str) -> bool {
    s.starts_with("postgres://") || s.starts_with("postgresql://") || s.starts_with("host=")
}

/// Whether the extension belongs to a format the text dispatch
/// owns: the archive/SQLite magic sniffs must not pre-empt these —
/// a CSV whose first cell starts with "PK" is still a CSV.
fn known_text_ext(path: &Path) -> bool {
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
fn is_archive(path: &Path) -> bool {
    if let Some(e) = path.extension().and_then(|e| e.to_str())
        && matches!(
            e.to_ascii_lowercase().as_str(),
            "zip" | "jar" | "docx" | "odt" | "epub" | "tar" | "tgz" | "gz"
        )
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
fn is_sqlite(path: &Path) -> bool {
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

/// Whether the input is an atrep document: the first content line
/// (after an optional shebang) is a dialektos declaration in either
/// sigil — `@@@!<id>` or `\\\!<id>`. Extension dispatch handles
/// `.atd`/`.atk`; this sniff catches stdin and unsuffixed files,
/// and cannot collide with the `<`-leading XML/HTML sniffs.
fn is_atrep(text: &str) -> bool {
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

/// The text-level reading of a document (`text:` targets and
/// `.txt` files): the producer is chosen by extension, `<` sniffs
/// markup for the rest, plain paragraphs are the fallback.
/// The binary text-level producers, by extension: formats whose
/// bytes are a container, not UTF-8. Plain (unprefixed) paths
/// keep their archive reading — `text:` opts into the reader's
/// model.
fn binary_text_kind(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "docx" => Some("docx"),
        "epub" => Some("epub"),
        _ => None,
    }
}

fn binary_text_level(kind: &str, bytes: &[u8]) -> anyhow::Result<quarb_text::TextModel> {
    match kind {
        "docx" => quarb_text_docx::parse(bytes).map_err(|e| anyhow::anyhow!("as Word: {e}")),
        _ => quarb_text_epub::parse(bytes).map_err(|e| anyhow::anyhow!("as EPUB: {e}")),
    }
}

/// The web level (`web:`): a directory of built pages or an
/// archive of them (`.tar`, `.tgz`, `.zip`) as one site. Pages
/// join against `?base=<url>`, else the base read off a page's
/// canonical URL.
/// Run a query over an index-backed site store: the planner first
/// — a count outright, a candidate set the engine re-verifies, or
/// nothing (the scan) — with `--explain` commentary on stderr.
fn run_web_store<S: quarb_web::db::SqlStore + 'static>(
    cli: &Cli,
    query: &str,
    store: S,
    src: &str,
) -> anyhow::Result<()> {
    use quarb_web::plan::{Plan, Rung};
    let model = MODEL.with(|m| m.borrow().is_some());
    let plan = if pushdown_applies(cli) {
        quarb_web::plan::plan(query, store.dialect(), model)
    } else {
        Plan {
            rung: Rung::Scan,
            where_sql: String::new(),
            params: Vec::new(),
            reason: "disabled".into(),
            unverified: Vec::new(),
            limit: None,
        }
    };
    if cli.explain {
        eprintln!("web: {} — {}", web_rung_name(&plan.rung), plan.reason);
        if plan.rung != Rung::Scan {
            eprintln!("web: WHERE {}", plan.where_sql);
            if let Some(e) = store.estimate_where(&plan.where_sql, &plan.params) {
                eprintln!("web: plan {e}");
            }
        }
    }
    match plan.rung {
        Rung::Full => {
            let n = store.count_where(&plan.where_sql, &plan.params);
            println!("{n}");
            Ok(())
        }
        Rung::Prefilter => {
            let keys = match &plan.limit {
                Some(l) => store.keys_where_top(
                    &plan.where_sql,
                    &plan.params,
                    &l.column,
                    l.descending,
                    l.n,
                ),
                None => store.keys_where(&plan.where_sql, &plan.params),
            };
            if cli.explain {
                eprintln!("web: {} candidate(s)", keys.len());
            }
            let adapter = quarb_web::WebAdapter::new(store).with_scope(keys);
            run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src),
            )
        }
        Rung::Scan => {
            let adapter = quarb_web::WebAdapter::new(store);
            run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src),
            )
        }
    }
}

enum WebSite {
    Memory(quarb_web::WebAdapter<quarb_web::MemoryStore>),
    /// The store itself: the caller plans before wrapping it.
    Sqlite(quarb_web::db::sqlite::SqliteStore),
    Postgres(quarb_web::db::postgres::PostgresStore),
}

fn web_rung_name(r: &quarb_web::plan::Rung) -> &'static str {
    match r {
        quarb_web::plan::Rung::Full => "full",
        quarb_web::plan::Rung::Prefilter => "prefilter",
        quarb_web::plan::Rung::Scan => "scan",
    }
}

fn web_level(spec: &str) -> anyhow::Result<WebSite> {
    let (path_part, base) = match spec.split_once('?') {
        Some((p, q)) => {
            let mut base = String::new();
            for pair in q.split('&') {
                match pair.split_once('=') {
                    Some(("base", v)) => base = v.to_string(),
                    _ => anyhow::bail!("unknown web option {pair:?} — supported: base="),
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
        return quarb_web::fs::open_dir(target, &base)
            .map(WebSite::Memory)
            .with_context(|| format!("reading {} as a site", target.display()));
    }
    if is_archive(target) {
        return quarb_web::archive::open_path(target, &base)
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
fn koine_level(spec: &str) -> anyhow::Result<quarb_text::TextModel> {
    // The house ?param syntax: `koine:records.xml?format=jats`
    // forces a format — the third dispatch tier, for stdin-ish,
    // extensionless, or undeclared inputs.
    let (path_part, format) = match spec.split_once('?') {
        Some((p, q)) => {
            let mut fmt = None;
            for pair in q.split('&') {
                match pair.split_once('=') {
                    Some(("format", v)) => fmt = Some(v.to_string()),
                    _ => anyhow::bail!("unknown koine option {pair:?} — supported: format="),
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
        let text = std::fs::read_to_string(target)
            .with_context(|| format!("reading {}", target.display()))?;
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
            "tei" | "docbook" | "jats" | "usx" | "osis" | "rnc" | "opencorpora" | "proiel" => {
                quarb_text_koine::parse_xml_as(&read()?, &fmt)
            }
            "usfm" | "sfm" => quarb_text_koine::parse_xml_as(&read()?, "usfm"),
            other => anyhow::bail!(
                "unknown koine format {other:?} — known: md, html, rst, org, djot, tei, docbook, jats, usx, osis, usfm, atd"
            ),
        };
        return imported
            .with_context(|| format!("importing {} through atrep as {fmt}", target.display()));
    }
    let imported = match ext.as_deref() {
        Some("atd" | "atk") => {
            return quarb_text_koine::parse_file(target)
                .with_context(|| format!("reading {} as an atrep document", target.display()));
        }
        Some("usfm" | "sfm") => quarb_text_koine::parse_xml_as(&read()?, "usfm"),
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
                    "{} declares no XML identity this route knows (no namespace, DOCTYPE, or unambiguous root) — force one with koine:{}?format=tei|docbook|jats|usx|osis|rnc|opencorpora|proiel",
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

/// The corpus reading (`corpus:` — ruling #62): the text-level
/// reading of a document with every prose block's tokens as its
/// children — the tokenizer decided once, at mount. Every source
/// `text:` reads as a prose model qualifies: plain text, HTML,
/// Markdown, LaTeX, atrep, TEI and the other koine dialects,
/// .docx and .epub. An archive keeps to `text:` for now, and a
/// PDF's reading is line geometry, never prose.
fn corpus_level(rest: &str, allow_shell: bool) -> anyhow::Result<quarb_text::TextModel> {
    // The house ?param syntax: `?conllu=FILE` takes the tokens,
    // sentences and annotation from a CoNLL-U sidecar; `?annotate=CMD`
    // runs a command over the document's prose (stdin) and reads
    // CoNLL-U from its stdout — under the shell gate, like sh().
    // `?format=conllu` names the reading when no extension can
    // (a pipe, an extensionless file); the sniff covers the rest.
    let (rest, conllu, annotate, desm, format, cast, places, modernize) = match rest.split_once('?')
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
            ) = (None, None, Vec::new(), None, None, None, None);
            for pair in q.split('&') {
                match pair.split_once('=') {
                    Some(("conllu", v)) => conllu = Some(v.to_string()),
                    Some(("annotate", v)) => annotate = Some(v.to_string()),
                    Some(("desm", v)) => desm.push(v.to_string()),
                    Some(("cast", v)) => cast = Some(v.to_string()),
                    Some(("places", v)) => places = Some(v.to_string()),
                    Some(("modernize", v)) => modernize = Some(v.to_string()),
                    Some(("format", "conllu")) => format = Some("conllu"),
                    Some(("format", v)) => anyhow::bail!(
                        "corpus: format={v} is not a treebank format — format= takes conllu"
                    ),
                    _ => anyhow::bail!(
                        "unknown corpus option {pair:?} — supported: conllu=, annotate=, desm=, cast=, places=, modernize=, format="
                    ),
                }
            }
            (p, conllu, annotate, desm, format, cast, places, modernize)
        }
        None => (rest, None, None, Vec::new(), None, None, None, None),
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
    // A treebank is its own annotation (ruling #63): the file's
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
        let text = read_prose(target)?;
        let mut m = quarb_text::TextModel::parse_conllu_corpus(&text)
            .map_err(|e| anyhow::anyhow!("reading {rest} as CoNLL-U: {e}"))?;
        m.set_document_path(rest);
        return treebank_options(m);
    }
    // A directory of treebank files — a UD treebank's train / dev /
    // test, a release — reads as one corpus, a section per file.
    if target.is_dir() {
        own_annotation()?;
        let files = quarb_text::read_conllu_dir(target)
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
        koine_level(rest)?
    } else if let Some(kind) = binary_text_kind(target) {
        let bytes =
            std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
        binary_text_level(kind, &bytes).with_context(|| format!("reading {}", target.display()))?
    } else {
        let text = read_prose(target)?;
        if is_xml(Some(target), &text) {
            koine_level(rest)?
        } else if ext.is_none() && quarb_text::looks_like_conllu(&text) {
            // A pipe or an extensionless file carrying CoNLL-U:
            // the treebank reading, as `?format=conllu` would say.
            own_annotation()?;
            let mut m = quarb_text::TextModel::parse_conllu_corpus(&text)
                .map_err(|e| anyhow::anyhow!("reading {rest} as CoNLL-U: {e}"))?;
            m.set_document_path(rest);
            return treebank_options(m);
        } else {
            let mut m = text_level(&text, Some(target))?;
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
        allow_shell,
        "corpus",
    )
}

/// The corpus reading's second half, shared with `lit:`: the
/// sidecar or command annotation, else the tokenizer with the
/// session's and the mount's sentence bonds.
fn finish_corpus(
    mut model: quarb_text::TextModel,
    conllu: Option<String>,
    annotate: Option<String>,
    desm: &[String],
    cast: Option<String>,
    places: Option<String>,
    modernize: Option<String>,
    allow_shell: bool,
    level: &str,
) -> anyhow::Result<quarb_text::TextModel> {
    if let Some(table) = &modernize {
        model
            .set_orthography(table)
            .map_err(|e| anyhow::anyhow!("{level}: {e}"))?;
    }
    let conllu_text = match (conllu, annotate) {
        (Some(_), Some(_)) => anyhow::bail!("{level}: takes conllu= or annotate=, not both"),
        (Some(file), None) => Some(
            std::fs::read_to_string(&file).with_context(|| format!("reading {file} as CoNLL-U"))?,
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
            let mut bonds = quarb::sentence_bonds().map(|b| (*b).clone());
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
fn lit_level(rest: &str, allow_shell: bool) -> anyhow::Result<quarb_text::TextModel> {
    let (rest, conllu, annotate, desm, format, cast, places, scheme, modernize) = match rest
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
            ) = (None, None, Vec::new(), None, None, None, None, None);
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
                    _ => anyhow::bail!(
                        "unknown lit option {pair:?} — supported: conllu=, annotate=, desm=, cast=, places=, format=, scheme=, modernize="
                    ),
                }
            }
            (
                p, conllu, annotate, desm, format, cast, places, scheme, modernize,
            )
        }
        None => (rest, None, None, Vec::new(), None, None, None, None, None),
    };
    let target = Path::new(rest);
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
            _ => {
                let text = read_prose(target)?;
                match quarb_text_koine::detect_xml_kind(&text) {
                    Some(k) => k.to_string(),
                    None if is_xml(Some(target), &text) => anyhow::bail!(
                        "lit: reads TEI, USX, OSIS, RNC, OpenCorpora, PROIEL and litogramma — {rest} declares none of them (koine:{rest} reads the other XML vocabularies)"
                    ),
                    None => anyhow::bail!(
                        "lit: reads TEI, USX, OSIS, RNC, OpenCorpora, PROIEL and litogramma — corpus:{rest} reads plain prose"
                    ),
                }
            }
        },
    };
    let mut model = match kind.as_str() {
        "atd" | "atk" => quarb_text_koine::parse_lit_file_with(target, scheme.as_deref())
            .with_context(|| format!("reading {} as an atrep document", target.display()))?,
        "tei" | "usx" | "osis" | "usfm" | "rnc" | "opencorpora" | "proiel" => {
            quarb_text_koine::parse_lit_xml_as_with(&read_prose(target)?, &kind, scheme.as_deref())
                .with_context(|| {
                    format!("reading {} as {}", target.display(), kind.to_uppercase())
                })?
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
        allow_shell,
        "lit",
    )
}

/// Run an annotator command under `sh -c` with `text` on its
/// stdin, returning its stdout — CoNLL-U, by the contract of
/// `corpus:…?annotate=`.
fn annotate_with(cmd: &str, text: &str) -> anyhow::Result<String> {
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
fn read_prose(target: &Path) -> anyhow::Result<String> {
    let text = if target == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading standard input")?;
        text
    } else {
        std::fs::read_to_string(target).with_context(|| format!("reading {}", target.display()))?
    };
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    })
}

/// `DIR[?options]` when the target is a folder of documents: a
/// directory that holds no treebank. The options ride to every
/// document in it.
fn document_dir(rest: &str) -> Option<(&Path, Option<String>)> {
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
fn holds_documents(dir: &Path) -> bool {
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
fn document_folder(
    dir: &Path,
    hidden: bool,
    no_ignore: bool,
    reader: quarb_compose::DocumentReader,
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
        .with_document_reader(reader),
    )
}

/// The reader a folder mount hands its document leaves to, by the
/// prefix's reading: `text` (the formats the composed view does not
/// parse itself), `corpus` (every prose format, with its tokens and
/// sentences), `lit` (the marked-up editions). A file the reading
/// does not take, or cannot read, stays a plain leaf.
fn folder_reader(
    level: &'static str,
    opts: Option<String>,
    allow_shell: bool,
) -> quarb_compose::DocumentReader {
    Rc::new(move |path: &Path| {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())?;
        let spec = match &opts {
            Some(q) => format!("{}?{q}", path.display()),
            None => path.display().to_string(),
        };
        let edition = matches!(ext.as_str(), "atd" | "atk" | "usfm" | "sfm");
        match level {
            "corpus" => {
                let prose = matches!(
                    ext.as_str(),
                    "md" | "markdown" | "html" | "htm" | "txt" | "tex" | "xml" | "rst" | "org"
                ) || edition
                    || binary_text_kind(path).is_some();
                prose
                    .then(|| corpus_level(&spec, allow_shell).ok())
                    .flatten()
            }
            "lit" => (edition || ext == "xml")
                .then(|| lit_level(&spec, allow_shell).ok())
                .flatten(),
            _ => {
                if edition {
                    koine_level(&spec).ok()
                } else if let Some(kind) = binary_text_kind(path) {
                    binary_text_level(kind, &std::fs::read(path).ok()?).ok()
                } else if matches!(ext.as_str(), "tex" | "rst" | "org") {
                    let mut m = text_level(&read_prose(path).ok()?, Some(path)).ok()?;
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
fn holds_treebank(dir: &Path) -> bool {
    quarb_text::read_conllu_dir(dir).is_ok_and(|files| !files.is_empty())
}

/// A directory's treebank files as one document (`text:DIR`): the
/// `.conllu` files beneath it, a level-1 section each, without
/// tokens — `corpus:DIR` is the same set with them.
fn conllu_dir_text(dir: &Path) -> anyhow::Result<quarb_text::TextModel> {
    let files = quarb_text::read_conllu_dir(dir)
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

fn text_level(text: &str, path: Option<&Path>) -> anyhow::Result<quarb_text::TextModel> {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    Ok(match ext.as_deref() {
        // A treebank read as a document (ruling #63): sections,
        // paragraphs and sentences from its comments.
        Some("conllu" | "conllup") => quarb_text::TextModel::parse_conllu_text(text)
            .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?,
        Some("html" | "htm") => quarb_text_html::parse(text),
        Some("md" | "markdown") => quarb_text_markdown::parse(text),
        Some("tex" | "latex") => quarb_text_latex::parse(text),
        Some("txt") => quarb_text::TextModel::parse_plain(text),
        // Scripture in USFM: through atrep's importer, as the
        // koine route reads it.
        Some("usfm" | "sfm") => quarb_text_koine::parse_xml_as(text, "usfm")
            .map_err(|e| anyhow::anyhow!("reading USFM: {e}"))?,
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
fn split_alias(p: &Path) -> Option<(String, PathBuf)> {
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
pub type Mounted = (Box<dyn AstAdapter>, Box<dyn Fn(NodeId) -> String>);

/// Mount a relational adapter with the same JSON-column graft the
/// single-input flow applies: `run_relational` wraps every
/// relational adapter in `ComposeAdapter`, and a mount must too —
/// otherwise a text column full of JSON navigates as a subtree in
/// one flow and comes back flat in the other. Under --no-graft
/// both flows skip the wrap.
fn mounted_relational<A: AstAdapter + 'static>(
    no_graft: bool,
    inner: A,
    outer_loc: impl Fn(&A, NodeId) -> String + 'static,
) -> Mounted {
    if no_graft {
        let a = Rc::new(inner);
        let r = a.clone();
        return (Box::new(Shared(a)), Box::new(move |n| outer_loc(&r, n)));
    }
    let a = Rc::new(ComposeAdapter::new(inner));
    let r = a.clone();
    (
        Box::new(Shared(a)),
        Box::new(move |n| r.locator(n, |o| outer_loc(r.outer(), o))),
    )
}

/// The subset of the CLI that opening a target consults — the
/// public face for session tools (quai) that mount through qua's
/// dispatch without a full CLI.
#[derive(Default)]
pub struct OpenOpts {
    pub hidden: bool,
    pub no_ignore: bool,
    /// Opt directory mounts into grafting (the CLI's --graft).
    pub graft: bool,
    /// Disable grafting entirely (the CLI's --no-graft).
    pub no_graft: bool,
    pub refs: Option<PathBuf>,
}

/// Open any target qua speaks — filesystem paths, documents, and
/// the full adapter-scheme fleet (`gcl:`, `kafka:`, `neo4j://`,
/// …) — as a boxed adapter plus its locator renderer. The door
/// session tools use to mount what the CLI mounts.
pub fn open_target(target: &str, opts: &OpenOpts) -> anyhow::Result<Mounted> {
    let cli = Cli {
        hidden: opts.hidden,
        no_ignore: opts.no_ignore,
        graft: opts.graft,
        no_graft: opts.no_graft,
        refs: opts.refs.clone(),
        ..Cli::default()
    };
    open_mount(Path::new(target), &cli)
}

/// Mount kaiv text by its extension's pipeline stage: `.kaiv` is
/// canonical, `.daiv` is authored (compile + denormalize), `.raiv`
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

/// Open one input as a boxed adapter plus its locator renderer, for
/// mounting. Format detection matches the single-input flow.
fn open_mount(p: &Path, cli: &Cli) -> anyhow::Result<Mounted> {
    if p.is_dir() {
        let opts = FsOptions {
            hidden: cli.hidden,
            respect_ignore: !cli.no_ignore,
        };
        if cli.graft {
            let a = Rc::new(ComposeAdapter::with_source_paths(
                FsAdapter::with_options(p, opts)?,
                |fs, n| Some(fs.path(n)),
            ));
            let r = a.clone();
            return Ok((
                Box::new(Shared(a)),
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
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
    // A `lines:` prefix mounts line atoms, matching the
    // single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("lines:")
        && !rest.is_empty()
    {
        let target = Path::new(rest);
        let text = std::fs::read_to_string(target)
            .with_context(|| format!("reading {}", target.display()))?;
        let a = Rc::new(quarb_lines::LinesAdapter::parse(&text));
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    // A `koine:` prefix takes the koine route, matching the
    // single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("koine:")
        && !rest.is_empty()
    {
        let a = Rc::new(koine_level(rest)?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("web:")
        && !rest.is_empty()
    {
        match web_level(rest)? {
            WebSite::Memory(a) => {
                let a = Rc::new(a);
                let r = a.clone();
                return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
            }
            WebSite::Sqlite(store) => {
                let a = Rc::new(quarb_web::WebAdapter::new(store));
                let r = a.clone();
                return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
            }
            WebSite::Postgres(store) => {
                let a = Rc::new(quarb_web::WebAdapter::new(store));
                let r = a.clone();
                return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
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
                folder_reader("lit", opts, cli.allow_shell),
            )?);
            let r = a.clone();
            return Ok((
                Box::new(Shared(a)),
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        let a = Rc::new(lit_level(rest, cli.allow_shell)?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
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
                folder_reader("corpus", opts, cli.allow_shell),
            )?);
            let r = a.clone();
            return Ok((
                Box::new(Shared(a)),
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        let a = Rc::new(corpus_level(rest, cli.allow_shell)?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    // A `text:` prefix forces the text-level reading, matching the
    // single-input flow.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("text:")
        && !rest.is_empty()
    {
        let target = Path::new(rest);
        // A directory of documents: the folder tree, each document
        // leaf read at the text level.
        if target.is_dir() && !holds_treebank(target) {
            let a = Rc::new(document_folder(
                target,
                cli.hidden,
                cli.no_ignore,
                folder_reader("text", None, cli.allow_shell),
            )?);
            let r = a.clone();
            return Ok((
                Box::new(Shared(a)),
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        // A directory of treebank files: one document, a section
        // per file.
        if target.is_dir() {
            let a = Rc::new(conllu_dir_text(target)?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
        // Native atrep files converge on the koine route.
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("atd") || e.eq_ignore_ascii_case("atk"))
        {
            let a = Rc::new(koine_level(rest)?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
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
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
        if let Some(kind) = binary_text_kind(target) {
            let bytes =
                std::fs::read(target).with_context(|| format!("reading {}", target.display()))?;
            let a = Rc::new(
                binary_text_level(kind, &bytes)
                    .with_context(|| format!("reading {}", target.display()))?,
            );
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
        // BibTeX / BibLaTeX: atrep's importer parses it into
        // bibliogramma, and the entries land as bib blocks.
        if target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("bib"))
        {
            let text = std::fs::read_to_string(target)
                .with_context(|| format!("reading {}", target.display()))?;
            let a = Rc::new(quarb_text_koine::parse_bibtex(&text).with_context(|| {
                format!(
                    "reading {} as BibTeX through bibliogramma",
                    target.display()
                )
            })?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
        let text = std::fs::read_to_string(target)
            .with_context(|| format!("reading {}", target.display()))?;
        let text = match text.strip_prefix('\u{feff}') {
            Some(rest) => rest.to_owned(),
            None => text,
        };
        let a = Rc::new(text_level(&text, Some(target))?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    // A `code:` prefix forces the code-level reading, matching
    // the single-input flow; a directory mounts the composed,
    // grafted view with source leaves grafted at the code
    // level.
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("code:")
        && !rest.is_empty()
    {
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
                .with_source_graft(SourceGraft::Code),
            );
            let r = a.clone();
            return Ok((
                Box::new(Shared(a)),
                Box::new(move |n| r.locator(n, |o| r.outer().path(o).display().to_string())),
            ));
        }
        let a = Rc::new(
            quarb_code::CodeModel::open(target)
                .with_context(|| format!("parsing {} at the code level", target.display()))?,
        );
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
        && s.starts_with("mssql://")
    {
        return Ok(mounted_relational(
            cli.no_graft,
            MssqlAdapter::connect(s).context("connecting to SQL Server")?,
            |a, n| a.locator(n),
        ));
    }
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
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("github:")
    {
        let a = Rc::new(GithubAdapter::connect(s).context("connecting to GitHub")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("gitlab:")
    {
        let a = Rc::new(GitlabAdapter::connect(s).context("connecting to GitLab")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("k8s:") || s.starts_with("kubernetes:"))
    {
        let a = Rc::new(KubernetesAdapter::connect(s).context("connecting to Kubernetes")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("gcl:") || s.starts_with("gcplogs:"))
    {
        let a = Rc::new(GclAdapter::open(s).context("reading Cloud Logging")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("cwl:") || s.starts_with("cloudwatch:"))
    {
        let a = Rc::new(CwlAdapter::open(s).context("reading CloudWatch Logs")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("ddl:") || s.starts_with("datadog:"))
    {
        let a = Rc::new(DdlAdapter::open(s).context("reading Datadog Logs")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("azl:") || s.starts_with("azlogs:"))
    {
        let a = Rc::new(AzlAdapter::open(s).context("reading Azure Monitor Logs")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("cfl:") || s.starts_with("cflogs:"))
    {
        let a = Rc::new(CflAdapter::open(s).context("reading Cloudflare logs")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && (s.starts_with("mongodb://") || s.starts_with("mongodb+srv://"))
    {
        let a = Rc::new(MongodbAdapter::connect(s).context("connecting to MongoDB")?);
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
        && s.starts_with("dynamodb:")
    {
        let a = Rc::new(DynamodbAdapter::connect(s).context("connecting to DynamoDB")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("neptune://")
    {
        let a = Rc::new(NeptuneAdapter::connect(s).context("connecting to Neptune")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("redis://")
    {
        let a = Rc::new(RedisAdapter::connect(s).context("connecting to Redis")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("rediss://")
    {
        let a = Rc::new(RedisAdapter::connect(s).context("connecting to Redis")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("falkor://")
    {
        let a = Rc::new(FalkorAdapter::connect(s).context("connecting to FalkorDB")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("memgraph://")
    {
        let a = Rc::new(MemgraphAdapter::connect(s).context("connecting to Memgraph")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("arango://")
    {
        let a = Rc::new(ArangoAdapter::connect(s).context("connecting to ArangoDB")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("sparql:")
    {
        let a = Rc::new(SparqlAdapter::connect(s).context("connecting to the SPARQL endpoint")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("age://")
    {
        let a = Rc::new(AgeAdapter::connect(s).context("connecting to AGE")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    #[cfg(feature = "kuzu")]
    if let Some(s) = p.to_str()
        && s.starts_with("kuzu:")
    {
        let a = Rc::new(KuzuAdapter::open(s).context("opening Kuzu database")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("kafka:")
    {
        let a = Rc::new(KafkaAdapter::connect(s).context("connecting to Kafka")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("cosmos://")
    {
        let a = Rc::new(CosmosAdapter::connect(s).context("connecting to Cosmos DB")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
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
            GitAdapter::open(std::path::Path::new(repo)).context("opening git repository")?,
        );
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && let Some(vault) = s
            .strip_prefix("metatheca:")
            .or_else(|| s.strip_prefix("mt:"))
    {
        let a = Rc::new(
            MetathecaAdapter::open(std::path::Path::new(vault))
                .context("opening metatheca vault")?,
        );
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
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
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("bigquery://")
    {
        return Ok(mounted_relational(
            cli.no_graft,
            BigqueryAdapter::connect(s).context("connecting to BigQuery")?,
            |a, n| a.locator(n),
        ));
    }
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
        && (t.starts_with("gs://") || t.starts_with("s3://") || t.starts_with("az://"))
    {
        if cli.no_graft {
            let a = Rc::new(ObjstoreAdapter::connect(t).context("connecting to bucket")?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
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
    // Source code: files with a tree-sitter grammar parse into
    // their syntax tree, matching the single-input flow.
    if p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| quarb_tree_sitter::supported(&e.to_ascii_lowercase()))
    {
        let a = Rc::new(TreeSitterAdapter::open(p).context("parsing source file")?);
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
    // DuckDB databases, by extension.
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
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if is_archive(p) {
        if cli.no_graft {
            let a = Rc::new(ArchiveAdapter::open(p).context("opening archive")?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
        let a = Rc::new(ComposeAdapter::new(
            ArchiveAdapter::open(p).context("opening archive")?,
        ));
        let r = a.clone();
        return Ok((
            Box::new(Shared(a)),
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
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.pointer(n))));
    }
    if is_sqlite(p) {
        let refs = relational_refs(&cli.refs)?;
        return Ok(mounted_relational(
            cli.no_graft,
            SqliteAdapter::open_with_refs(p, &refs).context("opening SQLite database")?,
            |a, n| a.locator(n),
        ));
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
        && matches!(ext.to_ascii_lowercase().as_str(), "daiv" | "kaiv" | "raiv")
    {
        let dir = path.and_then(|p| p.parent());
        let a = Rc::new(parse_kaiv_ext(&ext.to_ascii_lowercase(), &text, dir)?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    // YAML/TOML/Markdown are extension-only, matching the single-input
    // flow (YAML/TOML share the JSON pointer model; Markdown locates).
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        let ext = ext.as_str();
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
        // Plain text mounts at the text level, matching the
        // single-input flow.
        if ext == "txt" {
            let a = Rc::new(quarb_text::TextModel::parse_plain(&text));
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
        if matches!(ext, "conllu" | "conllup") {
            let a = Rc::new(
                quarb_text::TextModel::parse_conllu_text(&text)
                    .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?,
            );
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
        if matches!(ext, "jsonl" | "ndjson") {
            let a = Rc::new(JsonAdapter::parse_lines(&text).context("parsing JSONL")?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.pointer(n))));
        }
        if matches!(ext, "atd" | "atk" | "usfm" | "sfm") {
            let dir = path.and_then(|p| p.parent()).unwrap_or(Path::new("."));
            let a = Rc::new(AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
        }
    }
    if is_atrep(&text) {
        let dir = path
            .and_then(|p| p.parent())
            .unwrap_or_else(|| Path::new("."));
        let a = Rc::new(AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
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
        Ok((Box::new(Shared(a)), Box::new(move |n| r.pointer(n))))
    }
}

/// Run a relational query with JSON-column grafting: the adapter
/// is wrapped in `ComposeAdapter`, so a text column whose value
/// parses as JSON grafts an inner arbor navigable in place
/// (`/orders/*/data/user/age`). `outer_loc` is the wrapped
/// adapter's own locator, threaded through the bang-locator.
/// Under --no-graft the wrap is skipped: a text column stays the
/// server's own scalar.
fn run_relational<A: AstAdapter>(
    inner: A,
    no_graft: bool,
    query: &str,
    outer_loc: impl Fn(&A, NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
    if no_graft {
        return run(query, &inner, |n| outer_loc(&inner, n), kaiv_source);
    }
    let adapter = ComposeAdapter::new(inner);
    run(
        query,
        &adapter,
        |n| adapter.locator(n, |o| outer_loc(adapter.outer(), o)),
        kaiv_source,
    )
}

/// Run `query` against `adapter`, printing node locations (via
/// `render`) or projected values, one per line.
fn run<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
    // A --model file enriches every source with derived structure,
    // and its derived nodes render through the composed locator.
    if let Some(model) = MODEL.with(|m| m.borrow().clone()) {
        let enriched = quarb_model::ModelAdapter::new(quarb_model::Borrowed(adapter), model);
        let base_render = &render;
        let model_render = |n: NodeId| enriched.locator(n, base_render);
        return run_dispatch(query, &enriched, model_render, kaiv_source);
    }
    run_dispatch(query, adapter, render, kaiv_source)
}

/// The resident check and wrap chain, shared by the plain and
/// model-enriched paths.
fn run_dispatch<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
    // Every adapter dispatch funnels through here — which makes it
    // the one place a resident session takes over: the adapter is
    // built and materialized, so instead of answering once and
    // exiting, serve queries against it until the TTL.
    #[cfg(unix)]
    if let Some((sock, ttl, pinned)) = RESIDENT.with(|r| r.borrow().clone()) {
        return resident_serve_loop(adapter, render, &sock, ttl, pinned);
    }
    run_wrapped(query, adapter, &render, kaiv_source)
}

/// The wrap chain (--allow-shell, --quantifier-bound, now-binding)
/// and execution for one query — `run` for the one-shot path, and
/// per-query inside a resident session.
fn run_wrapped<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: &impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
    if ALLOW_SHELL.with(|b| b.get()) {
        let shelled = AllowShell { inner: adapter };
        return run_bounded(query, &shelled, render, kaiv_source);
    }
    run_bounded(query, adapter, render, kaiv_source)
}

fn run_bounded<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(n) = QUANT_BOUND.with(|b| b.get()) {
        let bounded = QuantifierBound {
            inner: adapter,
            bound: n,
        };
        return run_nowed(query, &bounded, render, kaiv_source);
    }
    run_nowed(query, adapter, render, kaiv_source)
}

fn run_nowed<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
    // The invocation instant is always bound in the CLI (main set
    // it from --now or one startup clock read).
    let (secs, nanos) = NOW_INSTANT.with(|c| c.get());
    let nowed = WithNow {
        inner: adapter,
        secs,
        nanos,
    };
    run_inner(query, &nowed, render, kaiv_source)
}

fn run_inner<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
    // --expand with an input: expansion with the dataset at hand,
    // so data-aware macros (&name!) can read it.
    if EXPAND1_FLAG.with(|f| f.get()) {
        for t in quarb::expand_first_with(query, &quarb::Defs::default(), adapter)
            .context("expanding the query")?
        {
            println!("{t}");
        }
        return Ok(());
    }
    if EXPAND_FLAG.with(|f| f.get()) {
        println!(
            "{}",
            quarb::expand_with(query, &quarb::Defs::default(), adapter)
                .context("expanding the query")?
        );
        return Ok(());
    }
    if let Some(source) = kaiv_source {
        let rows = quarb::run_traced_prov(query, adapter)?;
        print!(
            "{}",
            emit_kaiv(
                &rows,
                source,
                &render,
                |n| quarb::resolved_provenance(adapter, n),
                KAIV_ORIGINS.with(|c| c.get()),
            )?
        );
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
    // Buffered: one flush at the end, not a syscall per line.
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mode = OUTPUT.with(|o| o.get());
    let values = match quarb::run(query, adapter)? {
        QueryResult::Nodes(nodes) => {
            if mode == Output::Quarb {
                for node in nodes {
                    writeln!(out, "{}", render(node))?;
                }
                out.flush()?;
                return Ok(());
            }
            nodes.into_iter().map(|n| Value::Str(render(n))).collect()
        }
        QueryResult::Values(values) => values,
    };
    emit_values(&mut out, values, mode)?;
    out.flush()?;
    Ok(())
}

/// Write the values in the chosen output form.
fn emit_values(
    out: &mut impl std::io::Write,
    values: Vec<Value>,
    mode: Output,
) -> anyhow::Result<()> {
    match mode {
        // The Quarb form: a scalar bare, a record `%(k = v; …)`, a
        // list `@(a; b)` — text that reads back as a query.
        Output::Quarb => {
            for value in values {
                writeln!(out, "{}", value.display_form())?;
            }
        }
        // One JSON document: the values as an array.
        Output::Json => {
            let items: Vec<String> = values.iter().map(Value::to_json).collect();
            writeln!(out, "[{}]", items.join(", "))?;
        }
        // JSON Lines: one document per line.
        Output::Jsonl => {
            for value in values {
                writeln!(out, "{}", value.to_json())?;
            }
        }
        // An aligned table, or CSV: records as columns.
        Output::Table => write_table(out, &values, false)?,
        Output::Csv => write_table(out, &values, true)?,
    }
    Ok(())
}

/// Records as columns, as the `@| table` and `@| csv` stages print
/// them: `--table` and `--csv` are the query with the stage appended.
fn write_table(out: &mut impl std::io::Write, values: &[Value], csv: bool) -> anyhow::Result<()> {
    writeln!(out, "{}", quarb::tabulate(values, None, csv))?;
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
        use std::io::Write as _;
        // create_new: the existence check and the create are one
        // atomic open, so a concurrent writer cannot slip between.
        let mut f = match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::bail!("{} already exists (refusing to overwrite)", path.display())
            }
            Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
        };
        let items: Vec<String> = values.iter().map(|v| v.to_json()).collect();
        f.write_all(
            format!(
                "[{}]
",
                items.join(
                    ",
 "
                )
            )
            .as_bytes(),
        )?;
        return Ok(());
    }
    // SQLite: records become columns (first-appearance union),
    // scalars a single `value` column.
    let mut conn =
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
    // Identifiers come from the data (record field names can be
    // arbitrary document keys): escape embedded quotes rather than
    // letting them break — or rewrite — the statement.
    let ident = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let decl: Vec<String> = columns.iter().map(|c| ident(c)).collect();
    // One transaction for the whole save: per-row implicit
    // transactions would fsync every insert.
    let tx = conn.transaction()?;
    tx.execute(
        &format!("CREATE TABLE {} ({})", ident(table), decl.join(", ")),
        [],
    )?;
    let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
    {
        let mut stmt = tx.prepare(&format!(
            "INSERT INTO {} ({}) VALUES ({})",
            ident(table),
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
    }
    tx.commit()?;
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

/// The `--kaiv` document: every result under `/@results/N` — a
/// record spreading as its namespace's fields, a node as `::node`
/// (its locator), anything else as `::value` — with each leaf line
/// carrying the provenance of the value it holds: the value's
/// origins (where it was read, per field for a record, per item for
/// a list), resolved through the adapter's layers into kaiv
/// entries, `;`-separated, deduplicated by (source, dpid) with the
/// newest instant kept, at most `cap` named before the elision
/// marker `;+N` counts the rest. A value with no recorded origin
/// (a literal, a count) is attributed to its row's node. Sources
/// are declared once each, `.?srcN uri`, in first-appearance
/// order; `q` names the query's own source (the fallback). Lists of
/// records place their fields under `/@results/N/@tags/M` (records
/// open namespaces, lists open arrays); quantities keep their unit
/// (`!float:B`). What canonical kaiv cannot hold on a flat line
/// falls back to its JSON text (quoted, single-line) as `str`.
fn emit_kaiv(
    rows: &[quarb::Traced],
    source: &str,
    render: impl Fn(NodeId) -> String,
    prov_of: impl Fn(NodeId) -> quarb::ProvenanceList,
    cap: usize,
) -> anyhow::Result<String> {
    use kaiv::{KaivBuilder, ProvEntry, Provenance};
    use quarb::Prov;
    use quarb::kaiv_out::{ProvCtx, ident_of, kaiv_put};
    let mut b = KaivBuilder::new();
    b.declare_source("q", source).map_err(kaiv_err)?;
    // Every node a leaf may be attributed to, in first-appearance
    // order: the row's own, then its value's origins, tree-walked.
    fn origin_nodes(p: &Prov, out: &mut Vec<NodeId>) {
        out.extend(p.origins.nodes());
        for (_, q) in &p.parts {
            origin_nodes(q, out);
        }
    }
    let mut nodes: Vec<NodeId> = Vec::new();
    for r in rows {
        nodes.push(r.node);
        origin_nodes(&r.prov, &mut nodes);
    }
    // Declare each distinct source once under a short id — the
    // point of the declaration is that the URI travels once, up
    // top. A source the builder refuses maps to the fallback.
    let mut source_ids: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for n in &nodes {
        for p in prov_of(*n).entries {
            let Some(src) = p.source else { continue };
            if source_ids.contains_key(&src) {
                continue;
            }
            let id = format!("src{}", source_ids.len() + 1);
            let id = match b.declare_source(&id, &src) {
                Ok(()) => id,
                Err(_) => "q".to_string(),
            };
            source_ids.insert(src, id);
        }
    }
    // One kaiv entry per (origin node, recorded entry): the declared
    // source id (else `q`), the instant in the canonical dashed form
    // (an instant the form cannot hold — a year outside 0000–9999 —
    // is dropped rather than emitted invalid), and the dpid — the
    // source's own, else the node's path within the source (the
    // rule for a source without keys), else the sanitized locator;
    // never one that only repeats the source (a filesystem node's
    // source is its path, and a document's root is `/`).
    let entry_of = |n: NodeId, p: &quarb::Provenance| -> ProvEntry {
        ProvEntry {
            source: p
                .source
                .as_ref()
                .and_then(|s| source_ids.get(s).cloned())
                .unwrap_or_else(|| "q".to_string()),
            timestamp: p
                .instant
                .map(|(secs, _, _)| quarb::temporal::format_instant(secs, 0, Some(0)))
                .filter(|t| t.len() == 20),
            dpid: p.dpid.as_deref().map(ident_of).or_else(|| {
                let loc = match p.path.as_deref() {
                    Some(path) if path != "/" => ident_of(path),
                    _ => ident_of(&render(n)),
                };
                match &p.source {
                    Some(s) if ident_of(s) == loc => None,
                    _ => Some(loc),
                }
            }),
        }
    };
    for (i, row) in rows.iter().enumerate() {
        // A leaf's prefix from its origin set: the entries in
        // first-read order, deduplicated by (source, dpid) keeping
        // the newest instant, bounded by `cap`; what the bound and
        // the origins' own records elide is counted. No origin at
        // all attributes the value to the row's node.
        let resolve = |o: &quarb::Origins| -> Option<Provenance> {
            let mut entries: Vec<ProvEntry> = Vec::new();
            let mut elided = o.more;
            let ns: Vec<NodeId> = if o.is_empty() {
                vec![row.node]
            } else {
                o.nodes().collect()
            };
            for n in ns {
                let list = prov_of(n);
                elided = elided.saturating_add(list.elided);
                let ps = if list.entries.is_empty() {
                    vec![quarb::Provenance::default()]
                } else {
                    list.entries
                };
                for p in ps {
                    let e = entry_of(n, &p);
                    match entries
                        .iter()
                        .position(|q| q.source == e.source && q.dpid == e.dpid)
                    {
                        // The dashed form orders as it reads.
                        Some(at) => {
                            if e.timestamp > entries[at].timestamp {
                                entries[at].timestamp = e.timestamp;
                            }
                        }
                        None if entries.len() < cap => entries.push(e),
                        None => elided = elided.saturating_add(1),
                    }
                }
            }
            Some(Provenance { entries, elided })
        };
        let ctx = ProvCtx {
            prov: Some(&row.prov),
            resolve: &resolve,
        };
        let base = format!("/@results/{i}");
        let mut used = std::collections::HashSet::new();
        match &row.topic {
            None => {
                let loc = render(row.node);
                kaiv_put(&mut b, &base, "node", &Value::Str(loc), &ctx, &mut used)
                    .map_err(anyhow::Error::msg)?;
            }
            Some(Value::Record(fields)) => {
                for (k, v) in fields {
                    let part = row.prov.part(k);
                    let fc = ProvCtx {
                        prov: Some(&part),
                        resolve: &resolve,
                    };
                    kaiv_put(&mut b, &base, k, v, &fc, &mut used).map_err(anyhow::Error::msg)?;
                }
            }
            Some(v) => {
                kaiv_put(&mut b, &base, "value", v, &ctx, &mut used).map_err(anyhow::Error::msg)?
            }
        }
    }
    b.finish().map_err(kaiv_err)
}

fn kaiv_err(e: kaiv::PipelineError) -> anyhow::Error {
    anyhow::anyhow!("emitting kaiv: {e}")
}

#[cfg(test)]
mod tests {
    use super::{split_alias, split_scheme_query};
    use std::path::{Path, PathBuf};

    #[test]
    fn mount_aliases_split() {
        assert_eq!(
            split_alias(Path::new("ga=bigquery://p/quarb_ga?account=a@b.c")),
            Some((
                "ga".to_string(),
                PathBuf::from("bigquery://p/quarb_ga?account=a@b.c")
            ))
        );
        assert_eq!(
            split_alias(Path::new("raw_2026-06=events.jsonl")),
            Some(("raw_2026-06".to_string(), PathBuf::from("events.jsonl")))
        );
        // Not aliases: no '=', empty target, non-name prefix.
        assert_eq!(split_alias(Path::new("events.jsonl")), None);
        assert_eq!(split_alias(Path::new("ga=")), None);
        // A mount name in another script.
        assert_eq!(
            split_alias(Path::new("толстой=corpus:anna-karenina.atd")),
            Some((
                "толстой".to_string(),
                PathBuf::from("corpus:anna-karenina.atd")
            ))
        );
        assert_eq!(split_alias(Path::new("2ga=x.json")), None);
        assert_eq!(split_alias(Path::new("a/b=x.json")), None);
    }

    #[test]
    fn scheme_prefixed_queries_split() {
        assert_eq!(
            split_scheme_query("github:/torvalds/linux::stars"),
            Some(("github:", "/torvalds/linux::stars"))
        );
        assert_eq!(
            split_scheme_query("gitlab:/tesslab//*<repo>"),
            Some(("gitlab:", "/tesslab//*<repo>"))
        );
        assert_eq!(
            split_scheme_query("k8s:/namespaces/*"),
            Some(("k8s:", "/namespaces/*"))
        );
        // Anchored targets, payload schemes, and plain queries
        // keep the two-argument form.
        assert_eq!(split_scheme_query("github:torvalds/linux"), None);
        assert_eq!(split_scheme_query("git:/repo"), None);
        assert_eq!(split_scheme_query("/a/b::c"), None);
    }

    fn row(n: u64, topic: Option<quarb::Value>) -> quarb::Traced {
        quarb::Traced {
            node: quarb::NodeId(n),
            topic,
            prov: quarb::Prov::default(),
        }
    }

    #[test]
    fn emit_kaiv_provenance_per_row() {
        use quarb::{NodeId, Provenance, ProvenanceList, Value};
        // Values with no recorded origin are attributed to their
        // row's node.
        let rows = vec![
            row(1, Some(Value::Int(7))),
            row(2, Some(Value::Int(9))),
            row(3, Some(Value::Int(11))),
        ];
        let render = |n: NodeId| format!("/row/{}", n.0);
        // Node 1: a full triple. Node 2: same source, no ts/dpid.
        // Node 3: nothing — falls back to `q` + locator dpid.
        let (secs, _, _) = quarb::temporal::parse_iso("2026-07-17T12:00:00Z").unwrap();
        let prov_of = move |n: NodeId| match n.0 {
            1 => Provenance {
                source: Some("https://sensors.example.com/1".into()),
                instant: Some((secs, 0, Some(0))),
                dpid: Some("req-42".into()),
                ..Default::default()
            },
            2 => Provenance {
                source: Some("https://sensors.example.com/1".into()),
                ..Default::default()
            },
            _ => Provenance::default(),
        };
        let out = super::emit_kaiv(
            &rows,
            "a.daiv, b.csv",
            render,
            |n| ProvenanceList::single(prov_of(n)),
            8,
        )
        .unwrap();
        // One declaration per distinct source, after the fallback.
        assert!(out.contains(".?q a.daiv, b.csv\n"));
        assert_eq!(out.matches("sensors.example.com").count(), 1);
        // The declared id is short (`src1`, first appearance); it
        // carries the dashed instant and the pass-through dpid on
        // row 0 (authored block form); row 1 shares the source but
        // falls back to its locator dpid; row 2 rides `q`.
        assert!(out.contains(".?src1 https://sensors.example.com/1\n"));
        assert!(
            out.contains("!int?src1@2026-07-17T12:00:00Z#req-42\nvalue=7"),
            "{out}"
        );
        assert!(out.contains("!int?src1#row-2\nvalue=9"));
        assert!(out.contains("!int?q#row-3\nvalue=11"));

        // A locator that only repeats the source (a filesystem node's
        // source is its own path) adds no dpid.
        let fs = super::emit_kaiv(
            &[row(1, Some(Value::Int(1)))],
            ".",
            |_| "/a/b.txt".to_string(),
            |_| {
                ProvenanceList::single(Provenance {
                    source: Some("/a/b.txt".into()),
                    ..Default::default()
                })
            },
            8,
        )
        .unwrap();
        assert!(fs.contains("!int?src1\nvalue=1"), "{fs}");
        assert!(!fs.contains('#'), "{fs}");

        // A record field opens a namespace; a list an array; a
        // quantity keeps its unit.
        let nested = super::emit_kaiv(
            &[row(
                1,
                Some(Value::Record(vec![
                    (
                        "r".to_string(),
                        Value::Record(vec![
                            ("n".to_string(), Value::Str("grc.atd".into())),
                            (
                                "size".to_string(),
                                Value::Quantity {
                                    value: 4025440.0,
                                    base: "B".into(),
                                    written: None,
                                },
                            ),
                        ]),
                    ),
                    (
                        "tags".to_string(),
                        Value::list(vec![Value::Str("a".into()), Value::Str("b".into())]),
                    ),
                ])),
            )],
            "u.json",
            |_| "/0".to_string(),
            |_| ProvenanceList::default(),
            8,
        )
        .unwrap();
        assert!(nested.contains("(/r)"), "{nested}");
        assert!(nested.contains("\nn=grc.atd\n"), "{nested}");
        assert!(nested.contains("!float:B?q#0\nsize=4025440\n"), "{nested}");
        assert!(nested.contains("@tags"), "{nested}");
        assert!(!nested.contains("{\"n\""), "{nested}");

        // Provenance-less rows emit exactly the pre-upgrade shape.
        let plain = super::emit_kaiv(
            &rows,
            "data.json",
            |n: NodeId| format!("/r/{}", n.0),
            |_| ProvenanceList::default(),
            8,
        )
        .unwrap();
        assert!(plain.contains(".?q data.json\n"));
        assert!(plain.contains("!int?q#r-1\nvalue=7"));
        assert!(!plain.contains(".?q-"));
    }

    fn prov_back(back: &quarb_kaiv::KaivAdapter, q: &str) -> Vec<String> {
        match quarb::run(q, back).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn emit_kaiv_per_field_and_plural() {
        use quarb::{NodeId, Origin, Origins, Prov, Provenance, ProvenanceList, Value};
        let (t1, _, _) = quarb::temporal::parse_iso("2026-07-17T12:00:00Z").unwrap();
        let (t2, _, _) = quarb::temporal::parse_iso("2026-07-18T12:00:00Z").unwrap();
        let prov_of = move |n: NodeId| {
            ProvenanceList::single(Provenance {
                source: Some(format!("https://s.example.com/{}", n.0)),
                instant: Some((if n.0 == 2 { t2 } else { t1 }, 0, Some(0))),
                dpid: Some(format!("row-{}", n.0)),
                ..Default::default()
            })
        };
        let at = |ns: &[u64]| {
            let mut o = Origins::default();
            for &n in ns {
                o.insert(Origin {
                    node: NodeId(n),
                    stage: 1,
                });
            }
            Prov::leaf(o)
        };
        // A record whose fields were read at different nodes: each
        // leaf line carries its own prefix; a field read from two
        // nodes lists both, `;`-separated, in first-read order.
        let rec = Value::Record(vec![
            ("v".into(), Value::Int(1)),
            ("n".into(), Value::Int(2)),
            ("k".into(), Value::Int(3)),
        ]);
        let mut prov = Prov::record(vec![("v".into(), at(&[1])), ("n".into(), at(&[1, 2]))]);
        // A field with no origin of its own (a literal) rides the
        // row's node.
        prov.parts.push(("k".into(), Prov::default()));
        let rows = vec![quarb::Traced {
            node: NodeId(3),
            topic: Some(rec),
            prov,
        }];
        let out = super::emit_kaiv(&rows, "t", |n| format!("/r/{}", n.0), prov_of, 8).unwrap();
        assert!(out.contains(".?src1 https://s.example.com/3\n"), "{out}");
        assert!(out.contains(".?src2 https://s.example.com/1\n"), "{out}");
        assert!(out.contains(".?src3 https://s.example.com/2\n"), "{out}");
        assert!(
            out.contains("!int?src2@2026-07-17T12:00:00Z#row-1\nv=1\n"),
            "{out}"
        );
        assert!(
            out.contains(
                "!int?src2@2026-07-17T12:00:00Z#row-1;src3@2026-07-18T12:00:00Z#row-2\nn=2\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("!int?src1@2026-07-17T12:00:00Z#row-3\nk=3\n"),
            "{out}"
        );
        // The round trip: the emitted document re-mounts, and the
        // list-carrying field answers the same two entries — the
        // declared ids resolved back to their URIs.
        let back = quarb_kaiv::KaivAdapter::parse_kaiv(&out).unwrap();
        let prov = |q: &str| match quarb::run(q, &back).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            prov("/@results/0/n:::provenance"),
            [
                "?https://s.example.com/1@2026-07-17T12:00:00Z#row-1;https://s.example.com/2@2026-07-18T12:00:00Z#row-2"
            ]
        );
        assert_eq!(prov("/@results/0/n:::instant"), ["2026-07-18T12:00:00Z"]);
        assert_eq!(prov("/@results/0/n:::@provenance | count"), ["2"]);
        assert_eq!(prov("/@results/0/v:::elided"), ["0"]);

        // The bound: a value read from twelve nodes names `cap` of
        // them and counts the rest; the engine's own bound adds to
        // the count.
        let mut o = Origins::default();
        for n in 1..=12 {
            o.insert(Origin {
                node: NodeId(n),
                stage: 1,
            });
        }
        assert_eq!(o.more, 12 - quarb::ORIGIN_CAP as u32);
        let rows = vec![quarb::Traced {
            node: NodeId(1),
            topic: Some(Value::Int(78)),
            prov: Prov::leaf(o),
        }];
        let out = super::emit_kaiv(&rows, "t", |n| format!("/r/{}", n.0), prov_of, 3).unwrap();
        let line = out.lines().find(|l| l.starts_with("!int?")).unwrap();
        assert_eq!(line.matches("#row-").count(), 3, "{line}");
        assert!(line.ends_with(";+9"), "{line}");
        let back = quarb_kaiv::KaivAdapter::parse_kaiv(&out).unwrap();
        assert_eq!(prov_back(&back, "/@results/0/value:::elided"), ["9"]);
        assert_eq!(
            prov_back(&back, "/@results/0/value:::@provenance | count"),
            ["3"]
        );

        // The same (source, dpid) read twice keeps one entry with
        // the newest instant.
        let same = move |_: NodeId| {
            ProvenanceList::single(Provenance {
                source: Some("https://s.example.com/x".into()),
                instant: None,
                dpid: Some("row-x".into()),
                ..Default::default()
            })
        };
        let rows = vec![quarb::Traced {
            node: NodeId(1),
            topic: Some(Value::Int(1)),
            prov: at(&[1, 2]),
        }];
        let out = super::emit_kaiv(&rows, "t", |n| format!("/r/{}", n.0), same, 8).unwrap();
        assert!(out.contains("!int?src1#row-x\nvalue=1\n"), "{out}");
    }
}
