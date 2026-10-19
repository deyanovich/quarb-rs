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
use quarb::{AstAdapter, NodeId, QueryResult, Value};
#[cfg(feature = "adapters")]
use quarb_athena::AthenaAdapter;
#[cfg(feature = "adapters")]
use quarb_atrep::AtrepAdapter;
#[cfg(feature = "adapters")]
use quarb_bigquery::BigqueryAdapter;
#[cfg(feature = "adapters")]
use quarb_compose::ComposeAdapter;
#[cfg(feature = "adapters")]
use quarb_csv::CsvAdapter;
#[cfg(feature = "adapters")]
use quarb_duckdb::DuckdbAdapter;
#[cfg(feature = "adapters")]
use quarb_html::HtmlAdapter;
#[cfg(feature = "adapters")]
use quarb_json::JsonAdapter;
#[cfg(feature = "kuzu")]
#[cfg(feature = "adapters")]
use quarb_kuzu::KuzuAdapter;
#[cfg(feature = "adapters")]
use quarb_mount::{Latent, Mount, MountAdapter};
#[cfg(feature = "adapters")]
use quarb_mysql::MysqlAdapter;
#[cfg(feature = "adapters")]
use quarb_open::*;
#[cfg(feature = "adapters")]
pub use quarb_open::{Mounted, OpenOpts, SourceCtx, open_target};
#[cfg(feature = "adapters")]
use quarb_oracle::OracleAdapter;
#[cfg(feature = "adapters")]
use quarb_postgres::PostgresAdapter;
#[cfg(feature = "adapters")]
use quarb_sqlite::SqliteAdapter;
#[cfg(feature = "adapters")]
use quarb_xml::XmlAdapter;
#[cfg(feature = "adapters")]
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
#[cfg(feature = "adapters")]
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

    /// The calculator's: one thread per line of the input, the line
    /// as its topic (`seq 5 | qc -l '$_ * 2'`); sugar for
    /// `$_ | lines |` at the head.
    #[arg(short = 'l', long = "lines", hide = true)]
    lines: bool,

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

    /// The notation of the document on standard input, which is
    /// read as JSON by default and never sniffed: one of json jsonl
    /// yaml toml xml html markdown text csv tsv fb2 conllu kaiv quarb,
    /// an atrep extension (atd atk usfm), or a source grammar's name
    /// (rust, python, …). The flags --yaml --toml --xml --html
    /// --markdown --text --fb2 name the common ones directly.
    #[arg(long = "format", value_name = "NOTATION")]
    stdin_format: Option<String>,
    #[arg(long = "yaml", conflicts_with = "stdin_format")]
    in_yaml: bool,
    #[arg(long = "toml", conflicts_with = "stdin_format")]
    in_toml: bool,
    #[arg(long = "xml", conflicts_with = "stdin_format")]
    in_xml: bool,
    #[arg(long = "html", conflicts_with = "stdin_format")]
    in_html: bool,
    #[arg(long = "markdown", conflicts_with = "stdin_format")]
    in_markdown: bool,
    #[arg(long = "text", conflicts_with = "stdin_format")]
    in_text: bool,
    #[arg(long = "fb2", conflicts_with = "stdin_format")]
    in_fb2: bool,

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
    #[arg(long = "pretty", alias = "table", conflicts_with_all = ["json", "jsonl", "kaiv", "csv"])]
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

    /// Print the static plan (what the query will touch, step by
    /// step, with each adapter's estimate) as a kaiv document and
    /// stop, running nothing.
    #[arg(long)]
    plan: bool,

    /// Hold the run to a budget, refusing before running when the
    /// plan's estimate exceeds it: `nodes=N`, `bytes=N`,
    /// `requests=N`, comma-separated, and `strict` to refuse an
    /// unknown estimate too.
    #[arg(long, value_name = "BOUNDS")]
    budget: Option<String>,

    /// The output locale: how results are rendered (the decimal
    /// mark and grouping of numbers, the form of instants, csv's
    /// delimiter, sort's collation), never what they are. Layers,
    /// nearest wins: this flag, QUARB_LOCALE, ~/.quarb/config.toml
    /// ([output] locale = "…"), the system locale (LC_ALL, LANG),
    /// else the canonical form (C). A tag no table covers is
    /// reported and renders canonically.
    #[arg(long, value_name = "TAG")]
    locale: Option<String>,

    /// Group digits as the output locale does (1 234 567,5). Off by
    /// default: a locale sets the decimal mark, and a year or an id
    /// prints as written. Also QUARB_GROUPING=1, or `grouping =
    /// true` under [output] in the config file.
    #[arg(long)]
    grouping: bool,

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

    /// Allow the sh(...) pipeline command to run external commands.
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

/// How a result line is written: the Quarb form by
/// default, JSON on request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Quarb,
    Json,
    Jsonl,
    Table,
    Csv,
}

/// Everything that shapes how a query's result comes out, decided
/// once from the command line and carried to the run chain — no
/// thread-local, so a resident session can build one per query (the
/// output command is each query's own; the instant too unless --now
/// pinned it) and nothing of one query leaks into the next.
#[derive(Clone)]
struct OutputCtx {
    /// The --json / --jsonl / --table / --csv choice.
    output: Output,
    /// --expand / --expand-1 with a target: print the expansion.
    expand: bool,
    expand_1: bool,
    /// --kaiv-origins: the bound on named origins per kaiv leaf.
    kaiv_origins: usize,
    /// --save: (file, table name).
    save: Option<(PathBuf, String)>,
    /// --quantifier-bound.
    quant_bound: Option<usize>,
    allow_shell: bool,
    /// The environments mounted from the command line
    /// (`go=exec:~/go/bin`, `sh=shell:`; `--allow-shell` is
    /// `sh=shell:` under its older name).
    environments: Vec<quarb::Environment>,
    /// The command libraries this build links (`morph:` under the
    /// `morph` feature), reached by their prefix with no mount.
    libraries: Vec<std::sync::Arc<dyn quarb::CommandLibrary>>,
    /// --reproducible: provenance without the machine-dependent
    /// instant rungs.
    reproducible: bool,
    /// The invocation instant now() denotes: --now, or the clock
    /// read once at startup (per query in an unpinned resident
    /// session), so evaluation never reads a clock.
    now: (i64, u32),
    /// --explain: print the executed statement once a driver has
    /// run one, and the unresolved references.
    explain: bool,
    /// --plan: print the static plan and stop.
    plan: bool,
    /// The calculator's input: its pipe, read as one text, the topic
    /// the expression head's thread opens with (`None` at a terminal).
    /// The calculator profile: the run goes through the engine's
    /// calculator entry, no arbor.
    calc: bool,
    topic: Option<Value>,
    /// --budget, parsed.
    budget: Option<quarb::plan::Budget>,
    /// The --model file, parsed.
    #[cfg(feature = "adapters")]
    model: Option<quarb_model::Model>,
    /// The output locale in force, with the layer that set it.
    locale: Option<(quarb::locale::OutputLocale, &'static str)>,
    /// Resident-serve mode: the socket, the idle TTL, and whether
    /// --now pinned the instant.
    #[cfg(feature = "adapters")]
    resident: Option<(PathBuf, u64, bool)>,
    /// `@| encoding(LABEL)` taken off the end of the query.
    encoding: Option<(String, quarb::source::Escape)>,
}

/// Split a scheme-prefixed query (`github:/torvalds/…`) into
/// its target scheme and the root-anchored query. Only schemes
/// whose bare form is a complete target qualify — schemes that
/// carry a payload (`git:PATH`, `mongodb://HOST/DB`) keep the
/// two-argument form, where the split would be ambiguous.
#[cfg(feature = "adapters")]
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

#[cfg(feature = "adapters")]
impl From<&Cli> for OpenOpts {
    fn from(cli: &Cli) -> Self {
        OpenOpts {
            hidden: cli.hidden,
            no_ignore: cli.no_ignore,
            graft: cli.graft,
            no_graft: cli.no_graft,
            refs: cli.refs.clone(),
            allow_shell: cli.allow_shell,
        }
    }
}

/// Whether a target is written with a reader prefix or an adapter
/// scheme (`text:…`, `imap://…`, `neo4j://…`) rather than as a
/// path; a one-letter prefix is a Windows drive, a path.
#[cfg(feature = "adapters")]
fn is_prefixed_target(s: &str) -> bool {
    s.split_once(':').is_some_and(|(pre, _)| {
        pre.len() > 1
            && pre
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    })
}

/// The relational targets the single flow keeps typed, for pushdown.
#[cfg(feature = "adapters")]
fn is_relational_target(s: &str) -> bool {
    s.starts_with("mssql://")
        || s.starts_with("oracle://")
        || s.starts_with("bigquery://")
        || s.starts_with("athena:")
        || s.starts_with("mysql://")
        || is_pg_config(s)
}

/// The complete CLI entry point (the `qua` binary is a thin
/// shim over this; the `quarb-full` wheel ships it as
/// `qua-full`).
/// Which command runs: `qua`, with every source it can open, or
/// `qc`, the calculator — the same code, the adapters compiled out
/// and a query that would need one refused by name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    Qua,
    Calc,
}

/// `qua`'s entry point.
pub fn cli_main() -> anyhow::Result<()> {
    let r = cli_main_with(Profile::Qua);
    #[cfg(feature = "alloc-stats")]
    alloc_stats::report();
    r
}

/// `qc`'s entry point: the calculator. The query runs as `qua` runs
/// it after the expression head `=`; the `=` is accepted and not
/// required, and anything that needs an input, a mount or a path
/// from a node is refused, pointing at `qua`.
pub fn calc_main() -> anyhow::Result<()> {
    let r = cli_main_with(Profile::Calc);
    #[cfg(feature = "alloc-stats")]
    alloc_stats::report();
    r
}

/// The benchmark harness's allocation counter (feature
/// `alloc-stats`, never on in a shipped binary): a global allocator
/// that counts every allocation and reallocation, the bytes they
/// requested, and the peak heap in use, and reports the three on
/// stderr at exit. Relaxed atomics: the counts are totals, not an
/// ordering, and a thread's increments need no synchronization
/// with another's.
#[cfg(feature = "alloc-stats")]
mod alloc_stats {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);
    static LIVE: AtomicU64 = AtomicU64::new(0);
    static PEAK: AtomicU64 = AtomicU64::new(0);

    struct Counting;

    fn note(size: usize) {
        COUNT.fetch_add(1, Relaxed);
        BYTES.fetch_add(size as u64, Relaxed);
        let live = LIVE.fetch_add(size as u64, Relaxed) + size as u64;
        PEAK.fetch_max(live, Relaxed);
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            note(l.size());
            unsafe { System.alloc(l) }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            note(l.size());
            unsafe { System.alloc_zeroed(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            LIVE.fetch_sub(l.size() as u64, Relaxed);
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
            LIVE.fetch_sub(l.size() as u64, Relaxed);
            note(new_size);
            unsafe { System.realloc(p, l, new_size) }
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    pub fn report() {
        eprintln!(
            "alloc-stats: allocations {} bytes {} peak-heap {}",
            COUNT.load(Relaxed),
            BYTES.load(Relaxed),
            PEAK.load(Relaxed)
        );
    }
}

fn cli_main_with(profile: Profile) -> anyhow::Result<()> {
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

    let mut cli = match profile {
        Profile::Qua => Cli::parse(),
        // The calculator's help names it, and says what it is.
        Profile::Calc => {
            use clap::{CommandFactory, FromArgMatches};
            let cmd = Cli::command()
                .name("qc")
                .about("The Quarb calculator: qua after the expression head, with every adapter off")
                .override_usage("qc [OPTIONS] <EXPRESSION>...")
                // The calculator takes no input, so every positional
                // argument is expression text: the words are joined
                // with spaces, and `qc 3 + 5` needs no quotes. A
                // leading negative number is a value, not an option.
                .mut_arg("query", |a| {
                    a.value_name("EXPRESSION")
                        .allow_negative_numbers(true)
                        .help("The expression, e.g. '2 + 3 * 4' or '@(1; 2) +| 3'; the head `=` is accepted and not required")
                })
                .mut_arg("paths", |a| {
                    a.value_name("WORD")
                        .allow_negative_numbers(true)
                        .help("The rest of the expression: the arguments are joined with spaces, so quotes are needed only around what the shell would take (| * ; ( ) $ \")")
                })
                .mut_arg("lines", |a| a.hide(false));
            Cli::from_arg_matches(&cmd.get_matches())?
        }
    };
    // Environment mounts (`ALIAS=SCHEME:TARGET` with an environment
    // scheme) are taken off the inputs here, from the command line
    // only: mounting one is the opt-in that lets `ALIAS:name(…)`
    // run code outside the query.
    let environments = take_environments(&mut cli)?;
    let locale = set_output_locale(&cli)?;
    if profile == Profile::Calc {
        calc_check_options(&mut cli)?;
    }
    let output = if cli.json {
        Output::Json
    } else if cli.jsonl {
        Output::Jsonl
    } else if cli.table {
        Output::Table
    } else if cli.csv {
        Output::Csv
    } else {
        Output::Quarb
    };

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
    #[cfg(feature = "adapters")]
    if cli.paths.is_empty()
        && let Some((scheme, query)) = split_scheme_query(&cli.query)
    {
        cli.paths.push(PathBuf::from(scheme));
        cli.query = query.to_string();
    }

    #[cfg(feature = "adapters")]
    if cli.xpath {
        let translation = quarb_xpath::translate(&cli.query).context("translating XPath")?;
        for note in &translation.notes {
            eprintln!("note: {note}");
        }
        cli.query = translation.query;
    }
    #[cfg(feature = "adapters")]
    if cli.jq {
        let translation = quarb_jq::translate(&cli.query).context("translating jq")?;
        for note in &translation.notes {
            eprintln!("note: {note}");
        }
        cli.query = translation.query;
    }
    #[cfg(feature = "adapters")]
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
    // The calculator reads its query once the definitions are in
    // front of it, so a fragment a --defs file supplies is known.
    if profile == Profile::Calc {
        calc_check_query(&cli)?;
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
    }

    if let Some(n) = cli.quantifier_bound {
        anyhow::ensure!(n >= 1, "--quantifier-bound must be at least 1");
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
    // Adapters resolving a relative window (`since=30m`) read the
    // same instant, so a mount is as reproducible as the query.
    quarb::set_invocation_instant(now.0, now.1);

    // The session's sentence bonds, from every --desm file.
    #[cfg(feature = "adapters")]
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
    #[cfg(feature = "adapters")]
    let mut parsed_model: Option<quarb_model::Model> = None;
    #[cfg(feature = "adapters")]
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
            // An environment is mounted from the command line only:
            // a model file mounts sources, never code.
            if quarb::Environment::is_environment_target(&target) {
                anyhow::bail!(
                    "model {}: mount '{}' names an environment ({target}); environments \
                     are mounted from the command line only",
                    model_path.display(),
                    m.name
                );
            }
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
        parsed_model = Some(model);
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
    #[cfg(feature = "adapters")]
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
    #[cfg_attr(not(feature = "adapters"), allow(unused_mut))]
    let mut oc = OutputCtx {
        output,
        expand: cli.expand,
        expand_1: cli.expand_1,
        kaiv_origins: cli.kaiv_origins,
        save: cli.save.as_ref().map(|p| (p.clone(), cli.save_as.clone())),
        quant_bound: cli.quantifier_bound,
        allow_shell: cli.allow_shell,
        environments: environments.clone(),
        libraries: linked_libraries(),
        reproducible: cli.reproducible,
        now,
        explain: cli.explain,
        plan: cli.plan,
        calc: profile == Profile::Calc,
        topic: None,
        budget: cli.budget.as_deref().map(parse_budget).transpose()?,
        #[cfg(feature = "adapters")]
        model: parsed_model,
        locale,
        #[cfg(feature = "adapters")]
        resident: None,
        encoding: None,
    };
    // The calculator's input: a pipe is read whole, as one text, the
    // topic the expression head's thread opens with; nothing is
    // parsed (a document on a pipe is qua's). At a terminal the
    // topic stays null.
    if profile == Profile::Calc {
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut text)
                .context("reading standard input")?;
            oc.topic = Some(Value::Str(text));
        }
    } else if cli.lines {
        anyhow::bail!("--lines is the calculator's: pipe the text to qc");
    }
    #[cfg(all(unix, feature = "adapters"))]
    {
        if cli.resident && !cli.resident_serve {
            return resident_client(&cli, &oc);
        }
        if cli.resident_serve {
            let sock = resident_socket(&cli, &oc)?;
            oc.resident = Some((sock, cli.resident_ttl, cli.now.is_some()));
        }
    }
    if profile == Profile::Calc {
        return calc_execute(&cli.query, cli.kaiv, &oc);
    }
    #[cfg(feature = "adapters")]
    return execute(&cli, &cli.query, &oc);
    #[cfg(not(feature = "adapters"))]
    unreachable!("the adapters are compiled out: this build is the calculator")
}

/// The calculator's checks on the command line: no input, no mount,
/// none of the options that open or translate one; the expression
/// head supplied where the query lacks it.
fn calc_check_options(cli: &mut Cli) -> anyhow::Result<()> {
    // Every positional argument is expression text: `qc 3 + 5` is
    // the expression `3 + 5`. The arguments stay in `paths` so that
    // a query which fails to parse can name an existing file among
    // them as the likely cause.
    if !cli.paths.is_empty() {
        let mut words = vec![cli.query.clone()];
        words.extend(cli.paths.iter().map(|p| p.to_string_lossy().into_owned()));
        cli.query = words.join(" ");
    }
    for (on, flag) in [
        (cli.model.is_some(), "--model"),
        (!cli.desm.is_empty(), "--desm"),
        (cli.resident || cli.resident_serve, "--resident"),
        (cli.cache || cli.cache_dir.is_some(), "--cache"),
        (cli.xpath, "--xpath"),
        (cli.jq, "--jq"),
        (cli.sql, "--sql"),
    ] {
        if on {
            anyhow::bail!(
                "qc is the calculator: {flag} opens or translates a source — run it with qua"
            );
        }
    }
    let q = cli.query.trim_start();
    let body = q.strip_prefix('=').unwrap_or(q).trim_start();
    // `-l`: one thread per line of the input — the head's text topic
    // split by `lines`, the query running over the lines as after
    // an iteration pipe. A body that opens with a pipe joins as it
    // stands; any other body is a command of its own.
    // A body that opens with a pipe stands for `$_ | …`: the topic
    // (the input) is what it pipes from.
    let opens_with_pipe = ["|", "@|", "+|", "__", "@__", "+__"]
        .iter()
        .any(|p| body.starts_with(p));
    cli.query = match (cli.lines, opens_with_pipe) {
        (true, true) => format!("= $_ | lines {body}"),
        (true, false) => format!("= $_ | lines | {body}"),
        (false, true) => format!("= $_ {body}"),
        (false, false) => format!("= {body}"),
    };
    Ok(())
}

/// The calculator's check on the query, its definitions in front:
/// one that navigates, or whose operand reads a document, is
/// refused by name.
fn calc_check_query(cli: &Cli) -> anyhow::Result<()> {
    if !quarb::is_calculator(&cli.query) {
        // A query that fails to parse says so, and when one of the
        // arguments joined into it is a file on disk, the likely
        // cause is an input the calculator does not take; one that
        // parses but navigates is refused by what it needs.
        if let Err(e) = quarb::expand(&cli.query, &quarb::Defs::default()) {
            if let Some(p) = cli.paths.iter().find(|p| p.exists()) {
                anyhow::bail!(
                    "qc is the calculator and takes no input: '{}' names a file (an unquoted * or ? expands to the files of the directory: quote the expression) — a query over files runs with qua",
                    p.display()
                );
            }
            return Err(e).context("parsing the query");
        }
        anyhow::bail!(
            "qc is the calculator: this query navigates (a path from a node, or a mount) — run it with qua"
        );
    }
    if let Some(path) = quarb::calculator_reads(&cli.query) {
        anyhow::bail!(
            "qc is the calculator: '{path}' reads a document — run the query with qua over an input"
        );
    }
    Ok(())
}

/// A calculator query, run against the bare root: what `qua` runs
/// for an expression head without an input, and what `qc` runs for
/// everything — one path, so the two print the same bytes.
fn calc_execute(query: &str, kaiv: bool, oc: &OutputCtx) -> anyhow::Result<()> {
    let adapter = quarb::adapter::Bare;
    run_wrapped(
        query,
        &adapter,
        |_| String::new(),
        kaiv.then_some("stdin"),
        oc,
    )
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
#[cfg(feature = "adapters")]
fn resident_socket(cli: &Cli, oc: &OutputCtx) -> anyhow::Result<PathBuf> {
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
        // A session renders under the locale it was started with.
        oc.locale.map(|(l, _)| l).map(|l| l.tag),
    )
        .hash(&mut h);
    // A session renders as it was started: its output form and its
    // grouping are part of its identity, so a --json session never
    // answers a plain client. A relative source (`t.txt`,
    // `text:t.txt`) names a different file from each directory.
    (
        cli.json,
        cli.jsonl,
        cli.csv,
        cli.table,
        cli.kaiv,
        cli.grouping,
    )
        .hash(&mut h);
    std::env::current_dir().ok().hash(&mut h);
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
#[cfg(feature = "adapters")]
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
#[cfg(feature = "adapters")]
fn resident_client(cli: &Cli, oc: &OutputCtx) -> anyhow::Result<()> {
    use std::io::Write as _;
    let sock = resident_socket(cli, oc)?;
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
#[cfg(feature = "adapters")]
fn spawn_resident(sock: &std::path::Path) -> anyhow::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::process::CommandExt as _;
    let log = sock.with_extension("log");
    let logfile =
        std::fs::File::create(&log).with_context(|| format!("creating {}", log.display()))?;
    let exe = std::env::current_exe().context("resolving qua binary")?;
    let mut cmd = std::process::Command::new(exe);
    // The flag goes first: after a `--` in the user's arguments it
    // would be a path, and the child would be a client spawning its
    // own child, without end. The marker is the second lock.
    if std::env::var_os("QUARB_RESIDENT_CHILD").is_some() {
        anyhow::bail!("a resident session does not spawn a session of its own");
    }
    cmd.arg("--resident-serve")
        .args(std::env::args_os().skip(1))
        .env("QUARB_RESIDENT_CHILD", "1")
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
#[cfg(feature = "adapters")]
const RESIDENT_MAX_QUERY: usize = 1 << 20;

/// Server side: bind the socket and answer queries against the
/// standing adapter until the idle TTL expires. Queries run
/// serially; each failure answers that client and the session
/// lives on.
#[cfg(unix)]
#[cfg(feature = "adapters")]
fn resident_serve_loop<A: AstAdapter>(
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    sock: &std::path::Path,
    ttl: u64,
    now_pinned: bool,
    oc: &OutputCtx,
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
                // The output command is each query's own: taken off
                // and reset here, never inherited from the last.
                let (query, encoding) = match take_output_encoding(&query) {
                    Ok(q) => q,
                    Err(e) => {
                        let body = format!("{e:#}").into_bytes();
                        let _ = conn.write_all(format!("R {} 1\n", body.len()).as_bytes());
                        let _ = conn.write_all(&body);
                        continue;
                    }
                };
                // Each query is its own invocation instant unless
                // the session was pinned with --now.
                let mut now = oc.now;
                if !now_pinned {
                    let since = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default();
                    now = (since.as_secs() as i64, since.subsec_nanos());
                    quarb::set_invocation_instant(now.0, now.1);
                }
                let per_query = OutputCtx {
                    encoding,
                    now,
                    ..oc.clone()
                };
                let (result, output) =
                    with_stdout_capture(|| run_wrapped(&query, adapter, &render, None, &per_query));
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
#[cfg(feature = "adapters")]
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
#[cfg(feature = "adapters")]
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

/// Run one query text against the CLI's inputs, printing results —
/// the whole adapter dispatch.
/// The plan against the inputs' catalogs, before any is opened:
/// printed under --plan (`true`: handled); refused under a --budget
/// it exceeds; else `false`, and the inputs open as usual (the run
/// checks the budget again on the opened adapters' own estimates).
/// `false` too when an input has no catalog — a database, a
/// service — since its plan is the opened adapter's to price.
#[cfg(feature = "adapters")]
fn preflight(cli: &Cli, query: &str, oc: &OutputCtx) -> anyhow::Result<bool> {
    let cap = oc.budget.as_ref().map_or(quarb::Cap::NONE, |b| b.cap());
    let mut catalogs = Vec::new();
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
        let t = target.to_string_lossy().into_owned();
        if quarb_open::catalog(&t, cap).is_none() {
            return Ok(false);
        }
        let lookup: quarb::CatalogLookup = Box::new(move |ask| quarb_open::catalog_for(&t, ask));
        catalogs.push((name, target.display().to_string(), lookup));
    }
    if catalogs.is_empty() {
        return Ok(false);
    }
    let run = oc.run_opts();
    let defs = quarb::Defs::default();
    let plan = if catalogs.len() >= 2 || cli.paths.iter().any(|p| split_alias(p).is_some()) {
        let mut m = MountAdapter::new(Vec::new());
        for (name, target, lookup) in catalogs {
            m.declare(Latent {
                name,
                target: Some(target),
                opener: Box::new(|| Err("planned, not opened".into())),
                catalog: Some(lookup),
            })
            .map_err(|e| anyhow::anyhow!("mount: {e}"))?;
        }
        let q = quarb::Query::parse_with(query, &defs, &m, &run)?;
        match &oc.budget {
            Some(b) => q.plan_within(&m, b),
            None => q.plan(&m),
        }
    } else {
        let a = quarb::Unopened(catalogs.pop().expect("one input").2);
        let q = quarb::Query::parse_with(query, &defs, &a, &run)?;
        match &oc.budget {
            Some(b) => q.plan_within(&a, b),
            None => q.plan(&a),
        }
    };
    if oc.plan {
        print!("{}", plan.to_kaiv(query));
        return Ok(true);
    }
    if let Some(budget) = &oc.budget
        && let Some(why) = budget.exceeded_by(&plan)
    {
        return Err(quarb::QuarbError::refused(why).into());
    }
    Ok(false)
}

/// Take the environment mounts off the inputs: `go=exec:~/go/bin`,
/// `sh=shell:`. `--allow-shell` mounts `sh=shell:`, and a `shell:`
/// mount opens the shell gate: the two are one opt-in. Two
/// environments may not share an alias.
/// The command libraries compiled into this binary, by feature.
fn linked_libraries() -> Vec<std::sync::Arc<dyn quarb::CommandLibrary>> {
    #[allow(unused_mut)]
    let mut libraries: Vec<std::sync::Arc<dyn quarb::CommandLibrary>> = Vec::new();
    #[cfg(feature = "morph")]
    libraries.push(std::sync::Arc::new(quarb_lib_morph::Morph));
    #[cfg(feature = "scipy")]
    libraries.push(std::sync::Arc::new(quarb_lib_scipy::Scipy));
    #[cfg(feature = "sklearn")]
    libraries.push(std::sync::Arc::new(quarb_lib_sklearn::Sklearn));
    #[cfg(feature = "numpy")]
    libraries.push(std::sync::Arc::new(quarb_lib_numpy::Numpy));
    #[cfg(feature = "pandas")]
    libraries.push(std::sync::Arc::new(quarb_lib_pandas::Pandas));
    libraries
}

fn take_environments(cli: &mut Cli) -> anyhow::Result<Vec<quarb::Environment>> {
    let libraries = linked_libraries();
    let mut environments: Vec<quarb::Environment> = Vec::new();
    let mut kept = Vec::new();
    for p in std::mem::take(&mut cli.paths) {
        match quarb::Environment::parse_mount(&p.to_string_lossy()) {
            None => kept.push(p),
            Some(Ok(e)) => {
                if libraries.iter().any(|l| l.prefix() == e.alias) {
                    anyhow::bail!(
                        "'{}' is the prefix of a linked command library; an environment \
                         takes another alias",
                        e.alias
                    );
                }
                if environments.iter().any(|x| x.alias == e.alias) {
                    anyhow::bail!(
                        "two environments mounted as '{}'; give each its own alias",
                        e.alias
                    );
                }
                environments.push(e);
            }
            Some(Err(e)) => anyhow::bail!("{e}"),
        }
    }
    cli.paths = kept;
    if cli.allow_shell && !environments.iter().any(|e| e.alias == "sh") {
        environments.push(quarb::Environment {
            alias: "sh".to_string(),
            kind: quarb::EnvKind::Shell,
            target: String::new(),
        });
    }
    if environments.iter().any(|e| e.kind == quarb::EnvKind::Shell) {
        cli.allow_shell = true;
    }
    Ok(environments)
}

#[cfg(feature = "adapters")]
fn execute(cli: &Cli, query: &str, oc: &OutputCtx) -> anyhow::Result<()> {
    // The source boundary of this invocation's one target.
    let ctx = &SourceCtx::default();
    // `--expand` shows the query whole; otherwise the output command
    // comes off the end here, for the sink to apply.
    let (query, encoding) = if cli.expand || cli.expand_1 {
        (query.to_string(), None)
    } else {
        take_output_encoding(query)?
    };
    let query = &query;
    let oc = &OutputCtx {
        encoding,
        ..oc.clone()
    };
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
    // --plan and --budget price the inputs before paying to open
    // them, when each has a catalog (a file, an archive, a
    // directory). Anything else is priced once open, by its own
    // adapter.
    if (oc.plan || oc.budget.is_some()) && preflight(cli, query, oc)? {
        return Ok(());
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
            let Mounted {
                adapter, render, ..
            } = open_mount(&target, &OpenOpts::from(cli))?;
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
        let adapter = MountAdapter::try_new(mounts).map_err(|e| anyhow::anyhow!("mount: {e}"))?;
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
            ctx,
            oc,
        );
    }
    let path = match cli.paths.first() {
        Some(p) => Some(take_encoding_option(p, ctx)?),
        None => None,
    };

    // A reader prefix or an adapter scheme that is not one of the
    // relational doors below opens through quarb-open, as every
    // front-end does; the relational doors stay typed for pushdown.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && is_prefixed_target(s)
        && !is_relational_target(s)
        && !s.starts_with("web:")
    {
        let src = s.to_string();
        let m = open_mount(cli.paths.first().expect("a target"), &OpenOpts::from(cli))?;
        return run(
            query,
            &m.adapter,
            |n| (m.render)(n),
            cli.kaiv.then_some(src.as_str()),
            ctx,
            oc,
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
        return match web_level(rest, ctx)? {
            WebSite::Memory(adapter) => run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src.as_str()),
                ctx,
                oc,
            ),
            WebSite::Sqlite(store) => run_web_store(cli, query, store, &src, ctx, oc),
            WebSite::Postgres(store) => run_web_store(cli, query, store, &src, ctx, oc),
        };
    }
    // Kùzu: kuzu:PATH (opt-in: built with --features kuzu).
    #[cfg(feature = "kuzu")]
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
                    print_raw(&cols, rows, oc)?;
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
        let adapter = match partial_plan_with(cli, query, Some(quarb_sql::Dialect::Mssql), true) {
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
            ctx,
            oc,
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
                    print_raw(&cols, rows, oc)?;
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
        let adapter = match partial_plan_with(cli, query, Some(quarb_sql::Dialect::Oracle), true) {
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
            ctx,
            oc,
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
                    print_raw(&cols, rows, oc)?;
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
            ctx,
            oc,
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
                    print_raw(&cols, rows, oc)?;
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
            ctx,
            oc,
        );
    }

    // A MySQL/MariaDB URL connects and introspects the database.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("mysql://")
    {
        if let Some(plan) =
            pushdown_plan(cli, query, Some(quarb_sql::Dialect::MySql)).filter(|plan| {
                column_types_hold(cli, "pushdown", &plan.column_types, || {
                    let tables: Vec<&str> = plan.tables.iter().map(String::as_str).collect();
                    Ok(quarb_mysql::column_catalog(s, &tables)?)
                })
            })
        {
            match quarb_mysql::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows, oc)?;
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
        let adapter = match partial_plan_with(cli, query, Some(quarb_sql::Dialect::MySql), true)
            .filter(|pl| {
                column_types_hold(cli, "partial pushdown", &pl.column_types, || {
                    Ok(quarb_mysql::column_catalog(s, &[pl.table.as_str()])?)
                })
            }) {
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
            ctx,
            oc,
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
                    print_raw(&cols, rows, oc)?;
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
        let adapter = match partial_plan_with(cli, query, Some(quarb_sql::Dialect::Postgres), true)
        {
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
            ctx,
            oc,
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
                    print_raw(&cols, rows, oc)?;
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
            ctx,
            oc,
        );
    }

    // SQLite databases are binary: dispatch before the text read
    // (by extension, or the 16-byte magic).
    if let Some(p) = &path
        && is_sqlite(p)
    {
        if let Some(plan) =
            pushdown_plan(cli, query, Some(quarb_sql::Dialect::Sqlite)).filter(|plan| {
                column_types_hold(cli, "pushdown", &plan.column_types, || {
                    let tables: Vec<&str> = plan.tables.iter().map(String::as_str).collect();
                    Ok(quarb_sqlite::column_catalog(p, &tables)?)
                })
            })
        {
            match quarb_sqlite::raw_query(
                p,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left
                    .as_ref()
                    .map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows, oc)?;
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
        let adapter = match partial_plan_with(cli, query, Some(quarb_sql::Dialect::Sqlite), true)
            .filter(|pl| {
                column_types_hold(cli, "partial pushdown", &pl.column_types, || {
                    Ok(quarb_sqlite::column_catalog(p, &[pl.table.as_str()])?)
                })
            }) {
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
            ctx,
            oc,
        );
    }

    // Everything else — a directory, a document by extension or
    // content, an archive, a workbook, source code, a kaiv file —
    // opens through quarb-open, the same door every front-end
    // mounts through.
    if let Some(p) = &path {
        let src = p.display().to_string();
        let m = open_mount(cli.paths.first().expect("a target"), &OpenOpts::from(cli))?;
        return run(
            query,
            &m.adapter,
            |n| (m.render)(n),
            cli.kaiv.then_some(src.as_str()),
            ctx,
            oc,
        );
    }

    // No target: the document comes down the pipe (or, with none,
    // an expression head runs as a calculator).
    let text = match &path {
        Some(_) => unreachable!("a target opened above"),
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
            // The calculator: the bare root, the path `qc` runs.
            return calc_execute(query, cli.kaiv, oc);
        }
        None => read_prose(Path::new("-"), ctx)?,
    };
    // Strip a leading UTF-8 BOM (RFC 8259 permits ignoring it): it
    // otherwise breaks JSON parsing, since U+FEFF is not whitespace
    // so `trim_start` leaves it in place.
    let text = match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    };
    let kaiv = cli.kaiv.then_some("stdin");
    // An empty pipe is no input: an expression head runs as the
    // calculator against the bare root, anything that navigates is
    // refused as with no input at all.
    if text.trim().is_empty() {
        if quarb::is_calculator(&cli.query) {
            return calc_execute(query, cli.kaiv, oc);
        }
        quarb::expand(&cli.query, &quarb::Defs::default()).context("parsing the query")?;
        anyhow::bail!(
            "no input: standard input is empty — give a directory, a file, or pipe a document; \
             an expression head '= expr' runs without one"
        );
    }
    // Standard input is read as JSON by default and never sniffed
    // (spec: Grafting; the qua chapter): a notation flag names
    // anything else.
    let notation = stdin_notation(cli)?;
    stdin_dispatch(&notation, &text, query, kaiv, ctx, oc)
}

/// The notation the flags name for standard input: `--format NAME`
/// or one of the direct flags, JSON without any; two at once are
/// refused.
fn stdin_notation(cli: &Cli) -> anyhow::Result<String> {
    let direct: Vec<&str> = [
        (cli.in_yaml, "yaml"),
        (cli.in_toml, "toml"),
        (cli.in_xml, "xml"),
        (cli.in_html, "html"),
        (cli.in_markdown, "markdown"),
        (cli.in_text, "text"),
        (cli.in_fb2, "fb2"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, n)| n)
    .collect();
    anyhow::ensure!(
        direct.len() <= 1,
        "standard input has one notation: --{} and --{} name two",
        direct[0],
        direct[1]
    );
    Ok(match (&cli.stdin_format, direct.first()) {
        // the extension synonyms read as their notation (the
        // calculator profile has no compose crate to ask)
        (Some(f), _) => match f.trim().to_ascii_lowercase().as_str() {
            "yml" => "yaml".to_string(),
            "md" => "markdown".to_string(),
            "ndjson" => "jsonl".to_string(),
            "txt" | "plain" => "text".to_string(),
            "htm" => "html".to_string(),
            other => other.to_string(),
        },
        (None, Some(n)) => n.to_string(),
        (None, None) => "json".to_string(),
    })
}

/// Run `query` over standard input read as `notation`.
#[cfg(feature = "adapters")]
fn stdin_dispatch(
    notation: &str,
    text: &str,
    query: &str,
    kaiv: Option<&str>,
    ctx: &SourceCtx,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    match notation {
        "json" => {
            let adapter = JsonAdapter::parse(text).with_context(|| {
                "parsing standard input as JSON (its default notation; --format NAME or \
                 --yaml, --xml, --html, --text … name another)"
            })?;
            run(query, &adapter, |n| adapter.pointer(n), kaiv, ctx, oc)
        }
        "jsonl" => {
            let adapter = JsonAdapter::parse_lines(text).context("parsing JSON Lines")?;
            run(query, &adapter, |n| adapter.pointer(n), kaiv, ctx, oc)
        }
        "yaml" => {
            let adapter = quarb_yaml::parse(text).context("parsing YAML")?;
            run(query, &adapter, |n| adapter.pointer(n), kaiv, ctx, oc)
        }
        "toml" => {
            let adapter = quarb_toml::parse(text).context("parsing TOML")?;
            run(query, &adapter, |n| adapter.pointer(n), kaiv, ctx, oc)
        }
        "xml" => {
            let adapter = XmlAdapter::parse(text).context("parsing XML")?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "html" => {
            let adapter = HtmlAdapter::parse(text);
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "markdown" => {
            let adapter = quarb_markdown::parse(text);
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "text" => {
            let adapter = quarb_text::TextModel::parse_plain(text);
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "csv" | "tsv" => {
            let delim = if notation == "csv" { b',' } else { b'\t' };
            let adapter =
                CsvAdapter::parse_declared(text, delim, ctx.csv()).context("parsing CSV")?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "fb2" => {
            let adapter = quarb_text_koine::parse_xml_as(text, "fb2")
                .map_err(|e| anyhow::anyhow!("reading FictionBook: {e}"))?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "conllu" | "conllup" => {
            let adapter = quarb_text::TextModel::parse_conllu_text(text)
                .map_err(|e| anyhow::anyhow!("reading CoNLL-U: {e}"))?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "quarb" => {
            let adapter = quarb::reflect::QueryArbor::parse(text).context("parsing Quarb query")?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "kaiv" | "daiv" | "raiv" => {
            let adapter = parse_kaiv_ext(notation, text, None)?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        "atd" | "atk" | "usfm" | "sfm" | "atrep" => {
            let adapter =
                AtrepAdapter::parse_str(text, Path::new(".")).context("parsing atrep document")?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        ext if quarb_tree_sitter::supported(ext) => {
            let adapter = quarb_tree_sitter::TreeSitterAdapter::parse(text, ext)
                .map_err(|e| anyhow::anyhow!("parsing {ext} source: {e}"))?;
            run(query, &adapter, |n| adapter.locator(n), kaiv, ctx, oc)
        }
        other => anyhow::bail!(
            "--format {other}: no such notation; standard input reads as one of {}, conllu, \
             kaiv, quarb, an atrep extension (atd atk usfm) or a source grammar's name",
            quarb_compose::NOTATIONS.join(", ")
        ),
    }
}

/// Whether pushdown applies: enabled, not emitting kaiv (which
/// needs node provenance), not in --expand mode, not saving, and
/// not resident. A resident daemon must reach the serve loop with
/// the unfiltered adapter: a first-query full pushdown would answer
/// and exit before binding the socket, and a partial pushdown would
/// bake its WHERE into the standing arbor every later query reuses.
#[cfg(feature = "adapters")]
fn pushdown_applies(cli: &Cli) -> bool {
    // A planned or budgeted run goes through the engine, whose run
    // prices the query against the store's estimator and prints or
    // refuses before anything is read; a pushdown would execute the
    // SQL outright and drop both flags.
    !cli.no_pushdown
        && !cli.plan
        && cli.budget.is_none()
        && !cli.kaiv
        && !cli.expand
        && !cli.expand_1
        && cli.save.is_none()
        && !cli.resident
        && !cli.resident_serve
}

/// The partial-pushdown plan (a WHERE for one table's fetch), with
/// --explain commentary. Tried only after full pushdown refused.
#[cfg(feature = "adapters")]
fn partial_plan(
    cli: &Cli,
    query: &str,
    dialect: Option<quarb_sql::Dialect>,
) -> Option<quarb_sql::Partial> {
    partial_plan_with(cli, query, dialect, false)
}

/// Whether a plan's column-type assumptions hold in the driver's
/// catalog. SQLite and MySQL coerce a mixed comparison by rules of
/// their own, Quarb compares by value: a plan that compares a
/// column with a number is the scan's only on a numeric column,
/// and one that compares it with text only on a column that is
/// not. A catalog that cannot be read answers no, and the query
/// scans.
#[cfg(feature = "adapters")]
fn column_types_hold(
    cli: &Cli,
    what: &str,
    types: &quarb_sql::ColumnTypes,
    catalog: impl FnOnce() -> anyhow::Result<Vec<(String, bool, bool)>>,
) -> bool {
    if types.is_empty() {
        return true;
    }
    let unmet = match catalog() {
        Ok(c) => types.unmet(&c),
        Err(_) => types
            .numeric
            .iter()
            .chain(&types.text)
            .chain(&types.json)
            .cloned()
            .collect(),
    };
    if !unmet.is_empty() && cli.explain {
        eprintln!(
            "{what}: not used (the comparison on {} would follow the column's type where the scan follows the value); scanning",
            unmet.join(", ")
        );
    }
    unmet.is_empty()
}

/// [`partial_plan`]; `keyed_hops` for an adapter that reads hops by
/// key, where crosslinks and resolutions keep the prefilter.
#[cfg(feature = "adapters")]
fn partial_plan_with(
    cli: &Cli,
    query: &str,
    dialect: Option<quarb_sql::Dialect>,
    keyed_hops: bool,
) -> Option<quarb_sql::Partial> {
    if !pushdown_applies(cli) {
        return None;
    }
    let plan = if keyed_hops {
        quarb_sql::partial_pushdown_keyed(query, dialect)
    } else {
        quarb_sql::partial_pushdown_explained(query, dialect)
    };
    match plan {
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
#[cfg(feature = "adapters")]
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
#[cfg(feature = "adapters")]
fn print_raw(cols: &[String], rows: Vec<Vec<Value>>, oc: &OutputCtx) -> anyhow::Result<()> {
    // --explain: the statement the driver executed, verbatim,
    // ORDER BY and all. Recorded at execution because the key is
    // a per-adapter catalog lookup.
    if oc.explain
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
    let mut out = OutSink::new(oc.encoding.clone());
    emit_values(&mut out, values, oc.output, oc)?;
    out.finish()
}

/// Where a result goes: standard output, buffered — or, under
/// `@| encoding(LABEL)`, a buffer encoded as one text when the
/// query is done, so the bytes land as the encoding has them and
/// a character the encoding cannot represent refuses the whole
/// output (nothing half-written).
struct OutSink {
    buf: Vec<u8>,
    encoding: Option<(String, quarb::source::Escape)>,
}

impl OutSink {
    fn new(encoding: OutputEncoding) -> Self {
        OutSink {
            buf: Vec::new(),
            encoding,
        }
    }

    fn finish(self) -> anyhow::Result<()> {
        use std::io::Write as _;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        match self.encoding {
            Some((label, escape)) => {
                let text = String::from_utf8(self.buf).context("the output is not UTF-8")?;
                let bytes = quarb::source::encode_with(&text, &label, escape)
                    .map_err(|e| anyhow::anyhow!("encoding({label}): {e}"))?;
                out.write_all(&bytes)?;
            }
            None => out.write_all(&self.buf)?,
        }
        out.flush()?;
        Ok(())
    }
}

impl std::io::Write for OutSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Take `@| encoding(LABEL)` off the end of a query: the output
/// boundary is the tool's, not the engine's (it has no byte
/// value). The command anywhere else reaches the executor and is
/// refused there. The command before it decides what a character
/// the encoding lacks becomes: after `@| json` / `@| jsonl` the
/// format's `\u` escape; after anything else the output is
/// refused at the first such character.
type OutputEncoding = Option<(String, quarb::source::Escape)>;

#[cfg(feature = "adapters")]
fn take_output_encoding(query: &str) -> anyhow::Result<(String, OutputEncoding)> {
    let trimmed = query.trim_end();
    let Some(head) = trimmed.strip_suffix(')') else {
        return Ok((query.to_string(), None));
    };
    let Some(open) = head.rfind('(') else {
        return Ok((query.to_string(), None));
    };
    let (before, label) = head.split_at(open);
    let before = before.trim_end();
    let Some(rest) = before.strip_suffix("encoding") else {
        return Ok((query.to_string(), None));
    };
    let rest = rest.trim_end();
    let Some(rest) = rest.strip_suffix("@|") else {
        return Ok((query.to_string(), None));
    };
    let label = label[1..].trim().trim_matches('"');
    quarb::source::encoding_for_label(label).ok_or_else(|| {
        anyhow::anyhow!(
            "encoding: unknown label {label:?} (labels are the Encoding Standard's: utf-8, \
             windows-1251, koi8-r, iso-8859-2, …)"
        )
    })?;
    let rest = rest.trim_end();
    let escape = match rest.rsplit("@|").next().map(str::trim) {
        Some(command)
            if command == "json" || command == "jsonl" || command.starts_with("json(") =>
        {
            quarb::source::Escape::Json
        }
        _ => quarb::source::Escape::Refuse,
    };
    Ok((rest.to_string(), Some((label.to_string(), escape))))
}

/// The web level (`web:`): a directory of built pages or an
/// archive of them (`.tar`, `.tgz`, `.zip`) as one site. Pages
/// join against `?base=<url>`, else the base read off a page's
/// canonical URL.
/// Run a query over an index-backed site store: the planner first
/// — a count outright, a candidate set the engine re-verifies, or
/// nothing (the scan) — with `--explain` commentary on stderr.
#[cfg(feature = "adapters")]
fn run_web_store<S: quarb_web::db::SqlStore + 'static>(
    cli: &Cli,
    query: &str,
    store: S,
    src: &str,
    ctx: &SourceCtx,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    use quarb_web::plan::{Plan, Rung};
    let model = oc.model.is_some();
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
                ctx,
                oc,
            )
        }
        Rung::Scan => {
            let adapter = quarb_web::WebAdapter::new(store);
            run(
                query,
                &adapter,
                |n| adapter.locator(n),
                cli.kaiv.then_some(src),
                ctx,
                oc,
            )
        }
    }
}

#[cfg(feature = "adapters")]
fn web_rung_name(r: &quarb_web::plan::Rung) -> &'static str {
    match r {
        quarb_web::plan::Rung::Full => "full",
        quarb_web::plan::Rung::Prefilter => "prefilter",
        quarb_web::plan::Rung::Scan => "scan",
    }
}

/// Run a relational query with JSON-column grafting: the adapter
/// is wrapped in `ComposeAdapter`, so a text column whose value
/// parses as JSON grafts an inner arbor navigable in place
/// (`/orders/*/data/user/age`). `outer_loc` is the wrapped
/// adapter's own locator, threaded through the bang-locator.
/// Under --no-graft the wrap is skipped: a text column stays the
/// server's own scalar.
#[cfg(feature = "adapters")]
fn run_relational<A: AstAdapter>(
    inner: A,
    no_graft: bool,
    query: &str,
    outer_loc: impl Fn(&A, NodeId) -> String,
    kaiv_source: Option<&str>,
    ctx: &SourceCtx,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    if no_graft {
        return run(
            query,
            &inner,
            |n| outer_loc(&inner, n),
            kaiv_source,
            ctx,
            oc,
        );
    }
    let adapter = ComposeAdapter::new(inner)
        .with_decoding(current_decoding(ctx)?)
        .with_csv_options(ctx.csv());
    run(
        query,
        &adapter,
        |n| adapter.locator(n, |o| outer_loc(adapter.outer(), o)),
        kaiv_source,
        ctx,
        oc,
    )
}

/// Run `query` against `adapter`, printing node locations (via
/// `render`) or projected values, one per line.
#[cfg(feature = "adapters")]
fn run<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
    ctx: &SourceCtx,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    // The source boundary's record (the single-input flow read one
    // source) answers `:::encoding` / `:::replaced` on the root.
    if let Some((encoding, replaced)) = ctx.take_record() {
        let sourced = quarb::source::Sourced {
            inner: quarb_model::Borrowed(adapter),
            encoding,
            replaced,
        };
        return run_modelled(query, &sourced, render, kaiv_source, oc);
    }
    run_modelled(query, adapter, render, kaiv_source, oc)
}

#[cfg(feature = "adapters")]
fn run_modelled<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    // A --model file enriches every source with derived structure,
    // and its derived nodes render through the composed locator.
    if let Some(model) = oc.model.clone() {
        let documents = model.nodes.iter().any(|n| n.trait_name == "document");
        let enriched = quarb_model::ModelAdapter::new(quarb_model::Borrowed(adapter), model);
        enriched
            .check()
            .map_err(|e| anyhow::anyhow!("reading the model: {e}"))?;
        let base_render = &render;
        if documents {
            // a node constructor whose role wears the trait
            // `document` declares a corpus (spec: The Corpus
            // Reading): each member's text is read as prose, its
            // other keys declared, and the member serves the corpus
            // commands like a mounted document
            let reader: quarb_compose::DocumentNodes<
                quarb_model::ModelAdapter<quarb_model::Borrowed<'_>>,
            > = std::rc::Rc::new(|m, n| {
                let (text, declared) = m.document_member(n)?;
                let path = m.locator(n, |_| String::new());
                let doc = quarb_open::document_of(&text, &path, &declared);
                let ctx = SourceCtx::default();
                Some(
                    quarb_open::finish_corpus(
                        doc,
                        None,
                        None,
                        None,
                        &[],
                        None,
                        None,
                        None,
                        None,
                        false,
                        "corpus",
                        &ctx,
                    )
                    .unwrap_or_else(|_| quarb_open::document_of(&text, &path, &declared)),
                )
            });
            let composed = ComposeAdapter::new(enriched)
                .with_document_graft(quarb_compose::DocumentGraft::Text)
                .with_document_nodes(reader);
            let model_render =
                |n: NodeId| composed.locator(n, |o| composed.outer().locator(o, base_render));
            return run_dispatch(query, &composed, model_render, kaiv_source, oc);
        }
        let model_render = |n: NodeId| enriched.locator(n, base_render);
        return run_dispatch(query, &enriched, model_render, kaiv_source, oc);
    }
    run_dispatch(query, adapter, render, kaiv_source, oc)
}

/// The resident check and wrap chain, shared by the plain and
/// model-enriched paths.
#[cfg(feature = "adapters")]
fn run_dispatch<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    // Every adapter dispatch funnels through here — which makes it
    // the one place a resident session takes over: the adapter is
    // built and materialized, so instead of answering once and
    // exiting, serve queries against it until the TTL.
    #[cfg(all(unix, feature = "adapters"))]
    if let Some((sock, ttl, pinned)) = oc.resident.clone() {
        return resident_serve_loop(adapter, render, &sock, ttl, pinned, oc);
    }
    run_wrapped(query, adapter, &render, kaiv_source, oc)
}

impl OutputCtx {
    /// The options one query runs under: the flags (--allow-shell,
    /// --quantifier-bound, --reproducible), the invocation instant
    /// (always bound in the CLI: --now, or one startup clock read),
    /// the output locale's collation, the session's sentence bonds.
    fn run_opts(&self) -> quarb::Run {
        quarb::Run {
            now: Some(self.now),
            allow_shell: self.allow_shell,
            environments: self.environments.clone(),
            libraries: self.libraries.clone(),
            quantifier_bound: self.quant_bound,
            reproducible: self.reproducible,
            collation: self.locale.map(|(l, _)| l.tag.to_string()),
            budget: self.budget.clone(),
            plan: self.plan,
            topic: self.topic.clone(),
            ..quarb::Run::ambient()
        }
    }

    /// --explain: the unresolved references a run left, and the
    /// folder leaves a reading refused (they answer null).
    fn explain_refs(&self, refs: &[String]) {
        #[cfg(feature = "adapters")]
        let notes = quarb_open::take_notes();
        #[cfg(not(feature = "adapters"))]
        let notes: Vec<String> = Vec::new();
        if self.explain {
            for r in refs {
                eprintln!(
                    "unresolved external reference: {r} (mount it, or declare a tree for it)"
                );
            }
            for n in notes {
                eprintln!("{n}");
            }
        }
    }
}

/// `--budget nodes=N,bytes=N,requests=N[,strict]`.
fn parse_budget(text: &str) -> anyhow::Result<quarb::plan::Budget> {
    quarb::plan::Budget::parse(text).map_err(|e| anyhow::anyhow!("--{e}"))
}

/// Execution for one query under the run's options — `run` for the
/// one-shot path, and per-query inside a resident session.
fn run_wrapped<A: AstAdapter>(
    query: &str,
    adapter: &A,
    render: impl Fn(NodeId) -> String,
    kaiv_source: Option<&str>,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    let run = oc.run_opts();
    // --expand with an input: expansion with the dataset at hand,
    // so data-aware macros (&name!) can read it.
    if oc.expand_1 {
        for t in quarb::expand_first_with(query, &quarb::Defs::default(), adapter, &run)
            .context("expanding the query")?
        {
            println!("{t}");
        }
        return Ok(());
    }
    if oc.expand {
        println!(
            "{}",
            quarb::expand_with(query, &quarb::Defs::default(), adapter, &run)
                .context("expanding the query")?
        );
        return Ok(());
    }
    let parsed = quarb::Query::parse_with(query, &quarb::Defs::default(), adapter, &run)?;
    if oc.plan {
        print!("{}", parsed.plan(adapter).to_kaiv(query));
        return Ok(());
    }
    let run_query = |q: &quarb::Query| -> anyhow::Result<quarb::Outcome> {
        Ok(if oc.calc {
            q.run_calc(&run)?
        } else {
            q.run(adapter, &run)?
        })
    };
    if let Some(source) = kaiv_source {
        let rows = if oc.calc {
            parsed.run_traced_prov_calc(&run)?
        } else {
            parsed.run_traced_prov(adapter, &run)?
        };
        print!(
            "{}",
            emit_kaiv(
                &rows,
                source,
                &render,
                |n| quarb::resolved_provenance(adapter, n),
                oc.kaiv_origins,
            )?
        );
        return Ok(());
    }
    let save = oc.save.clone();
    if let Some((path, table)) = save {
        let out = run_query(&parsed)?;
        oc.explain_refs(&out.refs);
        let values = match out.result {
            QueryResult::Values(vs) => vs,
            QueryResult::Nodes(ns) => ns.into_iter().map(|n| Value::Str(render(n))).collect(),
        };
        let n = values.len();
        save_result(&path, &table, values)?;
        eprintln!("saved {n} row(s) to {}", path.display());
        return Ok(());
    }
    // Buffered: one flush at the end, not a syscall per line.
    let out = OutSink::new(oc.encoding.clone());
    let mode = oc.output;
    // Under an output locale a closing `@| csv` / `@| table` is the
    // tool's to render: the rows come back as they stand before it.
    let outcome = match oc.locale.map(|(l, _)| l) {
        Some(_) if mode == Output::Quarb => run_query(&parsed.for_rendering())?,
        _ => run_query(&parsed)?,
    };
    let quarb::Outcome {
        result,
        closing,
        refs,
        plan: _,
    } = outcome;
    // The unresolved references follow the result (the sink flushes
    // first), as the transcripts read them.
    let printed = print_result(out, result, closing, render, oc);
    oc.explain_refs(&refs);
    printed
}

fn print_result(
    mut out: OutSink,
    result: QueryResult,
    closing: Option<quarb::ClosingTable>,
    render: impl Fn(NodeId) -> String,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mode = oc.output;
    if let (Some(t), Some(l)) = (&closing, oc.locale.map(|(l, _)| l)) {
        let rows = match result {
            QueryResult::Values(values) => values,
            QueryResult::Nodes(nodes) => nodes.into_iter().map(|n| Value::Str(render(n))).collect(),
        };
        let columns = (!t.columns.is_empty()).then_some(t.columns.as_slice());
        writeln!(
            out,
            "{}",
            quarb::tabulate_in(
                &rows,
                columns,
                t.csv,
                t.delimiter.unwrap_or(l.csv_delimiter),
                &|v| l.cell(v)
            )
        )?;
        return out.finish();
    }
    let values = match result {
        QueryResult::Nodes(nodes) => {
            if mode == Output::Quarb {
                for node in nodes {
                    writeln!(out, "{}", render(node))?;
                }
                return out.finish();
            }
            nodes.into_iter().map(|n| Value::Str(render(n))).collect()
        }
        QueryResult::Values(values) => values,
    };
    emit_values(&mut out, values, mode, oc)?;
    out.finish()
}

/// Write the values in the chosen output form.
fn emit_values(
    out: &mut impl std::io::Write,
    values: Vec<Value>,
    mode: Output,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    // A library value prints as its own text; every other output
    // form serializes its rows, each a value of the output.
    let values = match mode {
        Output::Quarb => values,
        _ => {
            let mut rows = Vec::with_capacity(values.len());
            for v in values {
                match v {
                    Value::Foreign(f) => match f.rows() {
                        Ok(Value::List(items)) => rows.extend(items.iter().cloned()),
                        Ok(other) => rows.push(other),
                        Err(_) => rows.push(Value::Str(f.display())),
                    },
                    Value::Table(t) => match t.rows_value() {
                        Value::List(items) => rows.extend(items.iter().cloned()),
                        other => rows.push(other),
                    },
                    other => rows.push(other),
                }
            }
            rows
        }
    };
    match mode {
        // The Quarb form: a scalar bare, a record `%(k = v; …)`, a
        // list `@(a; b)` — text that reads back as a query; under
        // an output locale, rendered in its numbers and dates.
        Output::Quarb => {
            let locale = oc.locale.map(|(l, _)| l);
            for value in values {
                match &locale {
                    Some(l) => writeln!(out, "{}", l.display(&value))?,
                    None => writeln!(out, "{}", value.display_form())?,
                }
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
        Output::Table => write_table(out, &values, false, oc)?,
        Output::Csv => write_table(out, &values, true, oc)?,
    }
    Ok(())
}

/// Records as columns, as the `@| table` and `@| csv` commands print
/// them: `--table` and `--csv` are the query with the command appended.
fn write_table(
    out: &mut impl std::io::Write,
    values: &[Value],
    csv: bool,
    oc: &OutputCtx,
) -> anyhow::Result<()> {
    match oc.locale.map(|(l, _)| l) {
        Some(l) => writeln!(
            out,
            "{}",
            quarb::tabulate_in(values, None, csv, l.csv_delimiter, &|v| l.cell(v))
        )?,
        None => writeln!(out, "{}", quarb::tabulate(values, None, csv))?,
    }
    Ok(())
}

/// Decide the output locale by its layers, nearest first: --locale,
/// QUARB_LOCALE, the config file, the system locale (LC_ALL, then
/// LANG; C and POSIX are canonical), else canonical. A tag no table
/// covers is reported on stderr and renders canonically.
fn set_output_locale(
    cli: &Cli,
) -> anyhow::Result<Option<(quarb::locale::OutputLocale, &'static str)>> {
    let layers: [(&'static str, Option<String>); 4] = [
        ("--locale", cli.locale.clone()),
        ("QUARB_LOCALE", std::env::var("QUARB_LOCALE").ok()),
        ("config", config_locale()),
        (
            "system",
            std::env::var("LC_ALL")
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| std::env::var("LANG").ok()),
        ),
    ];
    let chosen = layers
        .iter()
        .find_map(|(layer, tag)| tag.as_deref().map(|t| (*layer, t.to_string())));
    let Some((layer, tag)) = chosen else {
        return Ok(None);
    };
    if quarb::locale::is_canonical(&tag) {
        return Ok(None);
    }
    match quarb::locale::for_tag(&tag) {
        Some(mut l) => {
            l.grouping = cli.grouping
                || std::env::var("QUARB_GROUPING").is_ok_and(|v| v == "1" || v == "true")
                || config_output("grouping").is_some_and(|v| v.as_bool() == Some(true));
            if cli.explain {
                eprintln!("locale: {tag} (from {layer})");
            }
            return Ok(Some((l, layer)));
        }
        None => {
            if layer == "--locale" {
                anyhow::bail!(
                    "--locale {tag}: no rendering rules for this locale (shipped: {}; C is the canonical form)",
                    quarb::locale::SHIPPED.join(", ")
                );
            }
            if cli.explain {
                eprintln!("locale: {tag} (from {layer}) has no rendering rules; canonical");
            }
        }
    }
    Ok(None)
}

/// `[output] locale = "…"` in ~/.quarb/config.toml (QUARB_CONFIG
/// names another file).
fn config_locale() -> Option<String> {
    config_output("locale")?.as_str().map(str::to_string)
}

/// One key of the config file's `[output]` table.
fn config_output(key: &str) -> Option<toml::Value> {
    let path = match std::env::var_os("QUARB_CONFIG") {
        Some(p) => PathBuf::from(p),
        None => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".quarb/config.toml"))?,
    };
    let text = std::fs::read_to_string(path).ok()?;
    let doc: toml::Table = toml::from_str(&text).ok()?;
    doc.get("output")?.get(key).cloned()
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
        out.extend(p.origins.positions().map(quarb::NodeId::from));
        for (_, q) in p.parts.iter() {
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
                o.positions().map(quarb::NodeId::from).collect()
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
                    position: (NodeId(n)).into(),
                    command: 1,
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
        std::sync::Arc::make_mut(&mut prov.parts).push(("k".into(), Prov::default()));
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
            prov("/@results/0/n:provenance"),
            [
                "?https://s.example.com/1@2026-07-17T12:00:00Z#row-1;https://s.example.com/2@2026-07-18T12:00:00Z#row-2"
            ]
        );
        assert_eq!(prov("/@results/0/n:instant"), ["2026-07-18T12:00:00Z"]);
        assert_eq!(prov("/@results/0/n:@provenance | count"), ["2"]);
        assert_eq!(prov("/@results/0/v:elided"), ["0"]);

        // The bound: a value read from twelve nodes names `cap` of
        // them and counts the rest; the engine's own bound adds to
        // the count.
        let mut o = Origins::default();
        for n in 1..=12 {
            o.insert(Origin {
                position: (NodeId(n)).into(),
                command: 1,
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
        assert_eq!(prov_back(&back, "/@results/0/value:elided"), ["9"]);
        assert_eq!(
            prov_back(&back, "/@results/0/value:@provenance | count"),
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
