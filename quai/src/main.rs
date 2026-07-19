//! `quai` — interactive Quarb.
//!
//! A session REPL over one or more sources. Each accepted line is
//! labelled `&N` and becomes a reusable query macro: later lines pick
//! it up as `&N` and continue through the pipe (`&2 | /name::`,
//! `&2 | [pred]`, `&2 @| count`). The materialized source is opened
//! once and queried many times.
//!
//! The session logic lives in [`quarb_session`]; `quai` is its native
//! frontend, pairing a [`LocalExecutor`] with a [`MemStore`]. The
//! daemon-backed executor and a persisting store are separate
//! backends behind the same seam.

use anyhow::{Context, Result};
use clap::Parser;
use quarb_session::{
    DaemonExecutor, Doc, FileStore, LocalExecutor, MemStore, Options, Session, Store,
};
use std::io::IsTerminal;
use std::path::PathBuf;

/// Interactive Quarb: each line becomes a reusable query macro
/// (&1, &2, …) over a standing session.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Source paths: a directory (filesystem), a document
    /// (.json/.yaml/.toml/.csv/.tsv/.xml/.html/.md), a SQLite,
    /// spreadsheet, or archive file, a source file, or `git:PATH`.
    /// Several sources mount as named children of one root, so a
    /// single query — including a `<=>` join — spans them all.
    paths: Vec<PathBuf>,

    /// Include hidden entries (filesystem only).
    #[arg(long)]
    hidden: bool,

    /// Do not respect `.gitignore` / `.ignore` (filesystem only).
    #[arg(long = "no-ignore")]
    no_ignore: bool,

    /// Descend through parseable file content: a directory's
    /// .json/.xml/.csv/… leaves graft their parsed tree as children.
    #[arg(long)]
    descend: bool,

    /// Allow the `sh(...)` pipeline stage to run external commands.
    #[arg(long)]
    allow_shell: bool,

    /// Pin the invocation instant `now()` denotes (ISO-8601). Default:
    /// the clock, read once at startup, so a session's `now()` is
    /// stable across lines.
    #[arg(long, value_name = "ISO")]
    now: Option<String>,

    /// Seed the macro table with fragment definitions from a file
    /// before the session starts.
    #[arg(long, value_name = "FILE")]
    defs: Option<PathBuf>,

    /// Back the session with a resident `qua` daemon: materialize the
    /// source once in a background process (shared across quai runs
    /// and with other clients) instead of in-process, and persist the
    /// macro history under ~/.quarb. Best for expensive sources
    /// reused across sessions; for a RAM-sized source the default
    /// in-process mode is faster.
    #[arg(long)]
    daemon: bool,

    /// With --daemon, let the resident arbor warm-start from (and
    /// populate) the on-disk AST cache for source-code inputs. Cache
    /// and daemon are layers, not alternatives.
    #[arg(long)]
    cache: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.paths.is_empty() {
        anyhow::bail!("quai needs at least one source (a directory, a document, or git:PATH)");
    }
    let mut session = if cli.daemon {
        // The daemon holds the arbor (via `qua --resident`); the store
        // persists the macro history across runs.
        let executor = Box::new(DaemonExecutor::new(
            cli.paths.clone(),
            cli.now.clone(),
            cli.allow_shell,
            cli.hidden,
            cli.no_ignore,
            cli.descend,
            cli.cache,
        )?);
        let store: Box<dyn Store> = match FileStore::new(&cli.paths) {
            Ok(fs) => Box::new(fs),
            Err(_) => Box::new(MemStore),
        };
        Session::new(executor, store)
    } else {
        let now = bind_now(cli.now.as_deref())?;
        let opts = Options {
            hidden: cli.hidden,
            respect_ignore: !cli.no_ignore,
            descend: cli.descend,
        };
        let doc = match cli.paths.as_slice() {
            [one] => Doc::open(one, &opts)?,
            many => Doc::mount(many, &opts)?,
        };
        // with_respec lets a `&N!` reading re-open the source live.
        let executor = Box::new(LocalExecutor::with_respec(
            doc,
            now,
            cli.allow_shell,
            cli.paths.clone(),
            opts,
        ));
        Session::new(executor, Box::new(MemStore))
    };
    if let Some(p) = &cli.defs {
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        session.seed_defs(&text)?;
    }
    let sources = cli
        .paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mode = if cli.daemon { "daemon-backed" } else { "in-process" };
    println!(
        "quai — interactive Quarb over {sources} ({mode}).  :help for commands, :quit (or Ctrl-D) to leave."
    );
    repl(&mut session)
}

/// Bind the invocation instant: `--now` pins it; otherwise the clock,
/// read once, so every `now()` in the session denotes one point.
fn bind_now(spec: Option<&str>) -> Result<(i64, u32)> {
    match spec {
        Some(text) => {
            let (secs, nanos, _) = quarb::temporal::parse_iso(text)
                .ok_or_else(|| anyhow::anyhow!("--now needs an ISO-8601 instant, got '{text}'"))?;
            Ok((secs, nanos))
        }
        None => {
            let since = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            Ok((since.as_secs() as i64, since.subsec_nanos()))
        }
    }
}

fn repl(session: &mut Session) -> Result<()> {
    use rustyline::error::ReadlineError;
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    // A real line editor: backspace, arrow keys, and Up/Down history
    // all work regardless of the terminal's erase-char quirks.
    let mut rl = rustyline::DefaultEditor::new()?;
    loop {
        let prompt = if color {
            format!("\x1b[36m&{}\x1b[0m ", session.line_no())
        } else {
            format!("&{} ", session.line_no())
        };
        let input = match rl.readline(&prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => continue, // Ctrl-C: drop the line
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                eprintln!("error: {e}");
                break;
            }
        };
        let line = input.trim();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(line); // Up/Down recalls prior lines
        // A `:` command (a query cannot start with a lone `:`).
        if line.starts_with(':') && !line.starts_with("::") {
            if command(session, line) {
                break;
            }
            continue;
        }
        // A definition extends the macro table but is not itself run.
        if line.starts_with("def ")
            || line == "def"
            || line.starts_with("macro ")
            || line == "macro"
        {
            if let Err(e) = session.add_def(line) {
                eprintln!("error: {e:#}");
            }
            continue;
        }
        // A capture reference (`&N#` frozen, `&N!` live) is resolved
        // by the session, not the engine — the engine's lexer has no
        // `#`, and its `!` signage rejects a bang on a pure fragment.
        match prepare(line) {
            Err(e) => eprintln!("error: {e}"),
            Ok(Prepared::Frozen(n)) => match session.frozen(n) {
                Some(cells) => {
                    let cells = cells.clone();
                    for c in &cells {
                        println!("{}", c.display());
                    }
                    session.record_frozen(cells);
                }
                None => eprintln!("error: &{n}# has no captured result (line {n} hasn't run)"),
            },
            Ok(Prepared::Live(q)) => run_and_commit(session, &q, true),
            Ok(Prepared::Eval(q)) => run_and_commit(session, &q, false),
        }
    }
    Ok(())
}

/// Evaluate a query line (against the standing arbor, or `fresh` for a
/// live re-read), print the result, and register it as `&N`.
fn run_and_commit(session: &mut Session, q: &str, fresh: bool) {
    let result = if fresh {
        session.eval_fresh(q)
    } else {
        session.eval(q)
    };
    match result {
        Ok(cells) => {
            for c in &cells {
                println!("{}", c.display());
            }
            let n = session.line_no();
            if !session.commit(q, cells) {
                eprintln!("note: &{n} is not referenceable (its shape can't be a macro body)");
            }
        }
        Err(e) => eprintln!("error: {e:#}"),
    }
}

/// How a line resolves once capture refs are handled.
enum Prepared {
    /// A standalone `&N#` — replay line N's frozen footprint.
    Frozen(usize),
    /// A `&N!` live reading — re-run line N against a freshly
    /// re-materialized source.
    Live(String),
    /// Ordinary query text, run against the standing arbor.
    Eval(String),
}

fn prepare(line: &str) -> Result<Prepared> {
    if let Some(n) = numeric_ref_with(line, '#') {
        return Ok(Prepared::Frozen(n));
    }
    if let Some(n) = numeric_ref_with(line, '!') {
        return Ok(Prepared::Live(format!("&{n}")));
    }
    if line.contains('#') {
        anyhow::bail!(
            "'#' is the frozen-history suffix, valid only as a standalone '&N#' in this build; \
             continuation off a frozen closure ('&N# | …') rides the daemon"
        );
    }
    Ok(Prepared::Eval(line.to_string()))
}

/// Match a bare capture ref `&<digits><suffix>` (the whole trimmed
/// line), returning N.
fn numeric_ref_with(line: &str, suffix: char) -> Option<usize> {
    line.strip_suffix(suffix)?
        .strip_prefix('&')?
        .parse::<usize>()
        .ok()
}

/// Handle a `:` command; returns true to exit the loop.
fn command(session: &mut Session, line: &str) -> bool {
    match line {
        ":q" | ":quit" => return true,
        ":help" | ":?" => {
            println!(
                "  <query>       run a query; its result is labelled &N and reusable\n  \
                 &N            re-run line N (a macro); continue with a pipe: &N | /key::\n  \
                 &N#           replay line N's frozen output (as it was when it ran)\n  \
                 &N!           re-run line N live — re-reads the source; diverges from &N# under drift\n  \
                 def &x: …;    add a named fragment to the session\n  \
                 :history      show the macro table (&1, &2, …)\n  \
                 :reset        clear the history and restart numbering\n  \
                 :quit         leave (also Ctrl-D)"
            );
        }
        ":history" => {
            let h = session.history();
            if h.trim().is_empty() {
                println!("(no history yet)");
            } else {
                print!("{h}");
            }
        }
        ":reset" => session.reset(),
        other => println!("unknown command '{other}' (:help lists them)"),
    }
    false
}
