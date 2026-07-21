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
use quarb_atrep::AtrepAdapter;
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
use quarb_github::GithubAdapter;
use quarb_gitlab::GitlabAdapter;
use quarb_gsheet::GsheetAdapter;
use quarb_html::HtmlAdapter;
use quarb_imap::ImapAdapter;
use quarb_json::JsonAdapter;
use quarb_kubernetes::KubernetesAdapter;
use quarb_maildir::MaildirAdapter;
use quarb_metatheca::MetathecaAdapter;
use quarb_ldap::LdapAdapter;
use quarb_mongodb::MongodbAdapter;
use quarb_mssql::MssqlAdapter;
use quarb_oracle::OracleAdapter;
use quarb_mount::{Mount, MountAdapter, Shared};
use quarb_mysql::MysqlAdapter;
use quarb_neo4j::Neo4jAdapter;
use quarb_objstore::ObjstoreAdapter;
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
    /// document and each value's origin node. (--daiv remains a
    /// hidden alias from the 0.2.0 era.)
    #[arg(long, alias = "daiv")]
    kaiv: bool,

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

    /// Save the result instead of printing it: `.json` writes a
    /// JSON array, any other extension a SQLite table (records
    /// become columns) — both first-class inputs for later queries.
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

    let mut cli = Cli::parse();

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
    // (and duplicate detection) already work.
    if let Some(defs_path) = &cli.defs {
        let text = std::fs::read_to_string(defs_path)
            .with_context(|| format!("reading {}", defs_path.display()))?;
        // Strip a leading UTF-8 BOM, as the document readers do.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
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
        EXPAND_FLAG.with(|f| f.set(true));
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

    // Enable the AST cache before dispatch, so both a normal run and
    // a resident daemon's per-query parses consult it.
    if cli.cache || cli.cache_dir.is_some() {
        let dir = cli
            .cache_dir
            .clone()
            .unwrap_or_else(quarb_code::Cache::default_dir);
        quarb_code::set_cache(Some(quarb_code::Cache::new(dir)));
    }

    if cli.resident || cli.resident_serve {
        anyhow::ensure!(
            !cli.kaiv && cli.save.is_none() && !cli.expand,
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
            RESIDENT
                .with(|r| *r.borrow_mut() = Some((sock, cli.resident_ttl, cli.now.is_some())));
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
        cli.descend,
        cli.hidden,
        cli.no_ignore,
        cli.allow_shell,
        cli.quantifier_bound,
        &cli.now,
        &cli.refs,
        &cli.defs,
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

/// Run one query text against the CLI's inputs, printing results —
/// the whole adapter dispatch.
fn execute(cli: &Cli, query: &str) -> anyhow::Result<()> {
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
            mounts.push(Mount { name, adapter });
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
            let adapter = ComposeAdapter::with_source_paths(
                FsAdapter::with_options(path, opts)?,
                |fs, n| Some(fs.path(n)),
            );
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

    // A SQL Server database: mssql://USER:PASS@HOST[:PORT]/DB.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && s.starts_with("mssql://")
    {
        if let Some(plan) = pushdown_plan(cli, query, Some(quarb_sql::Dialect::Mssql)) {
            match quarb_mssql::raw_query(
                s,
                &plan.sql,
                plan.order_table.as_deref(),
                plan.join_left.as_ref().map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    if cli.explain {
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => MssqlAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => MssqlAdapter::connect(s),
        }
        .context("connecting to SQL Server")?;
        return run_relational(adapter, query, |a, n| a.locator(n), cli.kaiv.then_some(s));
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
                plan.join_left.as_ref().map(|(t, c)| (t.as_str(), c.as_slice())),
            ) {
                Ok((cols, rows)) => {
                    print_raw(&cols, rows)?;
                    return Ok(());
                }
                Err(e) => {
                    if cli.explain {
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => OracleAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => OracleAdapter::connect(s),
        }
        .context("connecting to Oracle")?;
        return run_relational(adapter, query, |a, n| a.locator(n), cli.kaiv.then_some(s));
    }

    // An LDAP directory: ldap[s]://[USER:PASS@]HOST[:PORT]/BASE_DN.
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("ldap://") || s.starts_with("ldaps://"))
    {
        let adapter = LdapAdapter::connect(s).context("connecting to LDAP")?;
        return run(query, &adapter, |n| adapter.locator(n), cli.kaiv.then_some(s));
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
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => BigqueryAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => BigqueryAdapter::connect(s),
        }
        .context("connecting to BigQuery")?;
        return run_relational(adapter, query, |a, n| a.locator(n), cli.kaiv.then_some(s));
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
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => MysqlAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => MysqlAdapter::connect(s),
        }
        .context("connecting to MySQL")?;
        return run_relational(adapter, query, |a, n| a.locator(n), cli.kaiv.then_some(s));
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
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query) {
            Some(p) => PostgresAdapter::connect_filtered(s, &p.table, &p.where_sql),
            None => PostgresAdapter::connect(s),
        }
        .context("connecting to PostgreSQL")?;
        return run_relational(adapter, query, |a, n| a.locator(n), cli.kaiv.then_some(s));
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
    if let Some(s) = path.as_ref().and_then(|p| p.to_str())
        && (s.starts_with("gs://") || s.starts_with("s3://"))
    {
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
            .is_some_and(|e| quarb_code::supported(&e.to_ascii_lowercase()))
    {
        let adapter = CodeAdapter::open(p).context("parsing source file")?;
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
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = DuckdbAdapter::open(p).context("opening DuckDB database")?;
        let src = p.display().to_string();
        return run_relational(adapter, query, |a, n| a.locator(n), cli.kaiv.then_some(src.as_str()));
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
                        eprintln!("pushdown: plan not executed ({e}); scanning");
                    }
                }
            }
        }
        let adapter = match partial_plan(cli, query) {
            Some(pl) => SqliteAdapter::open_filtered(p, &pl.table, &pl.where_sql),
            None => SqliteAdapter::open(p),
        }
        .context("opening SQLite database")?;
        let src = p.display().to_string();
        return run_relational(adapter, query, |a, n| a.locator(n), cli.kaiv.then_some(src.as_str()));
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
        if matches!(ext, "atd" | "atk") {
            let dir = path.and_then(|p| p.parent()).unwrap_or(Path::new("."));
            let adapter =
                AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?;
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
        && cli.save.is_none()
        && !cli.resident
        && !cli.resident_serve
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
/// values for one column, records for several. Buffered: one
/// flush at the end, not a syscall per line.
fn print_raw(cols: &[String], rows: Vec<Vec<Value>>) -> anyhow::Result<()> {
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for row in rows {
        if cols.len() <= 1 {
            for v in row {
                writeln!(out, "{v}")?;
            }
        } else {
            let rec = Value::Record(cols.iter().cloned().zip(row).collect());
            writeln!(out, "{rec}")?;
        }
    }
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

/// An input argument's explicit mount alias: `NAME=TARGET` mounts
/// TARGET as `/NAME`. The prefix must look like a mount name (a
/// letter or `_`, then letters, digits, `_`, `-`) and the argument
/// must not name an existing file — a real file called `a=b.json`
/// still mounts by its stem.
fn split_alias(p: &Path) -> Option<(String, PathBuf)> {
    let s = p.to_str()?;
    let (name, target) = s.split_once('=')?;
    if target.is_empty() || p.exists() {
        return None;
    }
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    Some((name.to_string(), PathBuf::from(target)))
}

/// A boxed adapter and its locator renderer, ready to mount.
type Mounted = (Box<dyn AstAdapter>, Box<dyn Fn(NodeId) -> String>);

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
        if cli.descend {
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
        let a = Rc::new(MssqlAdapter::connect(s).context("connecting to SQL Server")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
    }
    if let Some(s) = p.to_str()
        && s.starts_with("oracle://")
    {
        let a = Rc::new(OracleAdapter::connect(s).context("connecting to Oracle")?);
        let r = a.clone();
        return Ok((Box::new(Shared(a)), Box::new(move |n| r.locator(n))));
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
    // Source code: files with a tree-sitter grammar parse into
    // their syntax tree, matching the single-input flow.
    if p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| quarb_code::supported(&e.to_ascii_lowercase()))
    {
        let a = Rc::new(CodeAdapter::open(p).context("parsing source file")?);
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
        let a = Rc::new(DuckdbAdapter::open(p).context("opening DuckDB database")?);
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
        && matches!(
            ext.to_ascii_lowercase().as_str(),
            "daiv" | "kaiv" | "raiv"
        )
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
        if matches!(ext, "jsonl" | "ndjson") {
            let a = Rc::new(JsonAdapter::parse_lines(&text).context("parsing JSONL")?);
            let r = a.clone();
            return Ok((Box::new(Shared(a)), Box::new(move |n| r.pointer(n))));
        }
        if matches!(ext, "atd" | "atk") {
            let dir = path.and_then(|p| p.parent()).unwrap_or(Path::new("."));
            let a =
                Rc::new(AtrepAdapter::parse_str(&text, dir).context("parsing atrep document")?);
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
fn run_relational<A: AstAdapter>(
    inner: A,
    query: &str,
    outer_loc: impl Fn(&A, NodeId) -> String,
    kaiv_source: Option<&str>,
) -> anyhow::Result<()> {
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
    if EXPAND_FLAG.with(|f| f.get()) {
        println!(
            "{}",
            quarb::expand_with(query, &quarb::Defs::default(), adapter)
                .context("expanding the query")?
        );
        return Ok(());
    }
    if let Some(source) = kaiv_source {
        let rows = quarb::run_traced(query, adapter)?;
        print!("{}", emit_kaiv(&rows, source, render)?);
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
    match quarb::run(query, adapter)? {
        QueryResult::Nodes(nodes) => {
            for node in nodes {
                writeln!(out, "{}", render(node))?;
            }
        }
        QueryResult::Values(values) => {
            for value in values {
                writeln!(out, "{value}")?;
            }
        }
    }
    out.flush()?;
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

/// Render traced results as canonical kaiv. Each result becomes one
/// leaf (or one leaf per record field) under `/@results/N`, typed by
/// the value's kind; provenance carries the source document (`?q`)
/// and the origin node's locator, identifier-sanitized, as `#dpid`.
/// A value canonical kaiv cannot hold on a flat line falls back to
/// its JSON text (quoted, single-line) as `str`.
fn emit_kaiv(
    rows: &[(NodeId, Option<Value>)],
    source: &str,
    render: impl Fn(NodeId) -> String,
) -> anyhow::Result<String> {
    use kaiv::{KaivBuilder, Provenance};
    let err = |e: kaiv::PipelineError| anyhow::anyhow!("emitting kaiv: {e}");
    let mut b = KaivBuilder::new();
    b.declare_source("q", source).map_err(err)?;
    for (i, (node, topic)) in rows.iter().enumerate() {
        let prov = Provenance {
            source: Some("q".to_string()),
            timestamp: None,
            dpid: Some(ident_of(&render(*node))),
        };
        // Sanitization can collide distinct field names ("a b" and
        // "a-b" both become "a-b"); suffix rather than abort.
        let mut used = std::collections::HashSet::new();
        let mut put = |field: &str, value: &Value| -> anyhow::Result<()> {
            let mut id = ident_of(field);
            if !used.insert(id.clone()) {
                let mut k = 2;
                loop {
                    let candidate = format!("{id}-{k}");
                    if used.insert(candidate.clone()) {
                        id = candidate;
                        break;
                    }
                    k += 1;
                }
            }
            let namepath = format!("/@results/{i}::{id}");
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
                // Durations emit on the seconds unit: a time-unit
                // annotation mints a duration at the re-mount (one
                // ontology per dimension of time), so the loop is
                // lossless.
                Value::Duration { secs, nanos } => {
                    let v = *secs as f64 + *nanos as f64 / 1e9;
                    if b.leaf_with_unit(&namepath, "float", Some("s"), &v.to_string(), Some(&prov))
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
                    if b.leaf(&namepath, ty, &value.to_string(), Some(&prov))
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                _ => {}
            }
            let (t, payload) = kaiv_scalar(value);
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
    b.finish().map_err(err)
}

/// The kaiv type annotation and payload for one value. Lists and
/// records ride as JSON text.
fn kaiv_scalar(v: &Value) -> (&'static str, String) {
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

/// Sanitize a locator or field name into kaiv's identifier charset:
/// ASCII alphanumerics and `_` pass through; each run of any other
/// characters (including `.` and `-`) collapses to one `-`.
fn ident_of(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
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
}
