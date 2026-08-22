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
    DaemonExecutor, Doc, FileStore, LocalExecutor, MemStore, MountSpec, Options, Session, Store,
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
    /// single query — including a `<=>` join — spans them all;
    /// `NAME=TARGET` picks the mount name explicitly. `:mount`
    /// adds a source mid-session.
    paths: Vec<String>,

    /// Include hidden entries (filesystem only).
    #[arg(long)]
    hidden: bool,

    /// Do not respect `.gitignore` / `.ignore` (filesystem only).
    #[arg(long = "no-ignore")]
    no_ignore: bool,

    /// Opt directory mounts into grafting: a directory's
    /// .json/.xml/.csv/… leaves graft their parsed tree as children.
    /// (--descend is the pre-0.24 spelling, kept as an alias.)
    #[arg(long, alias = "descend")]
    graft: bool,

    /// Disable grafting entirely: archive members and text columns
    /// stay opaque leaves; refused with the code: prefix.
    #[arg(long = "no-graft", conflicts_with = "graft")]
    no_graft: bool,

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

    /// A declared-references document: '{"refs": {"field":
    /// "container", ...}}' — the edges the substrate's own catalog
    /// does not hold, consumed by SQLite mounts (see `qua --help`).
    #[arg(long, value_name = "FILE")]
    refs: Option<PathBuf>,

    /// A model file declaring derived arbor structure over the
    /// source(s) — 'node'/'ref'/'rel'/'edge'/'mount' statements (see
    /// `qua --help`). The session runs every line against the
    /// enriched view.
    #[arg(long, value_name = "FILE")]
    model: Option<PathBuf>,

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

/// What an in-session `:mount` needs to rebuild the executor; absent
/// under `--daemon` (the daemon's arbor is pinned at start).
struct Remount {
    specs: Vec<MountSpec>,
    opts: Options,
    now: (i64, u32),
    allow_shell: bool,
}

/// Whether a target names an adapter scheme (qua's dispatch) rather
/// than a file. `git:` stays with the session's own opener.
fn is_schemed(s: &str) -> bool {
    !s.starts_with("git:")
        && s.split_once(':').is_some_and(|(sch, _)| {
            sch.len() >= 2 && sch.chars().all(|c| c.is_ascii_alphanumeric() || c == '+')
        })
}

/// Open one mount spec: adapter schemes go through qua's dispatch
/// (the whole fleet — gcl:, kafka:, neo4j://, …), everything else
/// through the session's file opener.
fn build_doc(spec: &MountSpec, opts: &Options) -> Result<(Doc, bool)> {
    let s = spec.path.to_string_lossy();
    if is_schemed(&s) {
        let (adapter, render) = qua::open_target(&s, &qua::OpenOpts::default())?;
        return Ok((Doc::Boxed(quarb_session::doc::Dyn(adapter), render), true));
    }
    Ok((Doc::open(&spec.path, opts)?, false))
}

/// Build the in-process executor over the current mount specs.
fn local_executor(remount: &Remount) -> Result<Box<LocalExecutor>> {
    // No source at all: the calculator session — a bare root for
    // expression-headed lines (`= expr`; spec: The Expression
    // Head). `:mount` grafts real sources in later; until then &N!
    // reads the same bare root as &N.
    if remount.specs.is_empty() {
        let doc = Doc::parse("{}", "json").map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(Box::new(LocalExecutor::new(
            doc,
            remount.now,
            remount.allow_shell,
        )));
    }
    let mut schemed = false;
    let doc = match remount.specs.as_slice() {
        [one] if one.name.is_none() => {
            let (doc, sch) = build_doc(one, &remount.opts)?;
            schemed |= sch;
            doc
        }
        many => {
            let mut parts: Vec<(String, Doc)> = Vec::new();
            for spec in many {
                let (doc, sch) = build_doc(spec, &remount.opts)?;
                schemed |= sch;
                let name = spec.name.clone().unwrap_or_else(|| {
                    if sch {
                        // A scheme target has no useful file stem;
                        // require an explicit mount name.
                        String::new()
                    } else {
                        spec.path
                            .file_stem()
                            .map(|x| x.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    }
                });
                if name.is_empty() {
                    anyhow::bail!(
                        "'{}': a scheme target in a mount needs an explicit \
                         name — spell it NAME={}",
                        spec.path.display(),
                        spec.path.display()
                    );
                }
                parts.push((name, doc));
            }
            Doc::mount_docs(parts)?
        }
    };
    // Live re-reads (&N!) re-open through the file path machinery,
    // which scheme targets bypass; their sessions read the standing
    // snapshot for both &N and &N!.
    if schemed {
        return Ok(Box::new(LocalExecutor::new(
            doc,
            remount.now,
            remount.allow_shell,
        )));
    }
    Ok(Box::new(LocalExecutor::with_respec(
        doc,
        remount.now,
        remount.allow_shell,
        remount.specs.clone(),
        remount.opts.clone(),
    )))
}

fn main() -> Result<()> {
    // Offline kaiv resolution by default, as in qua: registry
    // imports come from built-ins, local bases, or the warm cache
    // — never a surprise fetch. `KAIV_OFFLINE=0` opts back in.
    if std::env::var_os("KAIV_OFFLINE").is_none() {
        unsafe {
            std::env::set_var("KAIV_OFFLINE", "1");
        }
    }
    let mut cli = Cli::parse();
    // A --model file may declare its own sources; parse it and inject
    // the mounts (relative targets resolved against the model file's
    // directory) as NAME=TARGET inputs, ahead of any positional ones.
    let model = match &cli.model {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
            let m = quarb_model::parse_model(&text)
                .map_err(|e| anyhow::anyhow!("parsing model {}: {e}", path.display()))?;
            let base_dir = path.parent();
            for mt in m.mounts.iter().rev() {
                let target = quarb_model::resolve_mount_target(&mt.target, base_dir);
                cli.paths.insert(0, format!("{}={}", mt.name, target));
            }
            Some(m)
        }
        None => None,
    };
    if cli.paths.is_empty() && cli.daemon {
        anyhow::bail!("--daemon needs at least one source (a directory, a document, or git:PATH)");
    }
    let specs: Vec<MountSpec> = cli.paths.iter().map(|a| MountSpec::parse(a)).collect();
    let raw_paths: Vec<PathBuf> = cli.paths.iter().map(PathBuf::from).collect();
    let mut remount: Option<Remount> = None;
    let session = if cli.daemon {
        // The daemon holds the arbor (via `qua --resident`); the store
        // persists the macro history across runs. Raw args pass
        // through: `qua` itself understands NAME=TARGET.
        let executor = Box::new(DaemonExecutor::new(
            raw_paths.clone(),
            cli.now.clone(),
            cli.allow_shell,
            cli.hidden,
            cli.no_ignore,
            cli.graft,
            cli.no_graft,
            cli.cache,
            cli.refs.clone(),
            cli.model.clone(),
        )?);
        let store: Box<dyn Store> = match FileStore::new(&raw_paths) {
            Ok(fs) => Box::new(fs),
            Err(_) => Box::new(MemStore),
        };
        Session::new(executor, store)
    } else {
        let now = bind_now(cli.now.as_deref())?;
        let refs = match &cli.refs {
            Some(f) => {
                let text = std::fs::read_to_string(f)
                    .with_context(|| format!("reading refs file {}", f.display()))?;
                quarb_relational::parse_refs(&text)
                    .map_err(|e| anyhow::anyhow!("parsing refs: {e}"))?
            }
            None => Vec::new(),
        };
        let opts = Options {
            hidden: cli.hidden,
            respect_ignore: !cli.no_ignore,
            graft: cli.graft,
            no_graft: cli.no_graft,
            refs: std::rc::Rc::new(refs),
        };
        let ctx = Remount {
            specs,
            opts,
            now,
            allow_shell: cli.allow_shell,
        };
        let executor = (*local_executor(&ctx)?).with_model(model.clone());
        remount = Some(ctx);
        Session::new(Box::new(executor), Box::new(MemStore))
    };
    let session = std::rc::Rc::new(std::cell::RefCell::new(session));
    if let Some(p) = &cli.defs {
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        session.borrow_mut().seed_defs(&text)?;
    }
    let sources = if cli.paths.is_empty() {
        "a bare root — calculator session; lines open with '= expr', :mount adds sources".to_string()
    } else {
        cli.paths.join(", ")
    };
    let mode = if cli.daemon { "daemon-backed" } else { "in-process" };
    println!(
        "quai — interactive Quarb over {sources} ({mode}).  :help for commands, :quit (or Ctrl-D) to leave."
    );
    repl(&session, &mut remount)
}

/// Bind the invocation instant: `--now` pins it; otherwise the clock,
/// read once, so every `now()` in the session denotes one point — and
/// the adapters resolve their relative windows against the same one.
fn bind_now(spec: Option<&str>) -> Result<(i64, u32)> {
    let now = match spec {
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
    quarb::set_invocation_instant(now.0, now.1);
    Ok(now)
}

fn repl(
    session: &std::rc::Rc<std::cell::RefCell<Session>>,
    remount: &mut Option<Remount>,
) -> Result<()> {
    use rustyline::error::ReadlineError;
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    // A real line editor: backspace, arrow keys, and Up/Down history
    // all work regardless of the terminal's erase-char quirks — and
    // Tab completes: the session answers from its live arbor (real
    // children, real annotations), the stdlib registry otherwise.
    let mut rl = rustyline::Editor::<QuaiHelper, rustyline::history::DefaultHistory>::new()?;
    rl.set_helper(Some(QuaiHelper {
        session: std::rc::Rc::clone(session),
    }));
    loop {
        let prompt = if color {
            format!("\x1b[36m&{}\x1b[0m ", session.borrow().line_no())
        } else {
            format!("&{} ", session.borrow().line_no())
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
            if command(&mut session.borrow_mut(), remount, line) {
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
            if let Err(e) = session.borrow_mut().add_def(line) {
                eprintln!("error: {e:#}");
            }
            continue;
        }
        // A capture reference (`&N#` frozen, `&N!` live) is resolved
        // by the session, not the engine — the engine's lexer has no
        // `#`, and its `!` signage rejects a bang on a pure fragment.
        match prepare(line) {
            Err(e) => eprintln!("error: {e}"),
            Ok(Prepared::Frozen(n)) => match session.borrow().frozen(n).cloned() {
                Some(cells) => {
                    for c in &cells {
                        println!("{}", c.display());
                    }
                    session.borrow_mut().record_frozen(cells);
                }
                None => eprintln!("error: &{n}# has no captured result (line {n} hasn't run)"),
            },
            Ok(Prepared::Live(q)) => run_and_commit(&mut session.borrow_mut(), &q, true),
            Ok(Prepared::Eval(q)) => run_and_commit(&mut session.borrow_mut(), &q, false),
        }
    }
    Ok(())
}

/// The rustyline helper: Tab asks the session. Completion is the
/// only non-default behavior — hints, highlighting, and validation
/// stay stock (live line-coloring via quarb::highlight is a
/// recorded idea, not yet wired).
struct QuaiHelper {
    session: std::rc::Rc<std::cell::RefCell<Session>>,
}

impl rustyline::completion::Completer for QuaiHelper {
    type Candidate = rustyline::completion::Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let cands = self.session.borrow().complete(line, pos);
        // Replace from the start of the word under the cursor.
        let head = &line[..pos.min(line.len())];
        let mut start = head
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
            .map_or(0, |i| i + head[i..].chars().next().map_or(1, char::len_utf8));
        // Register spellings carry their `$`; widen the span so the
        // typed sigil isn't doubled.
        if start > 0
            && head[..start].ends_with('$')
            && !cands.is_empty()
            && cands.iter().all(|c| c.text.starts_with('$'))
        {
            start -= 1;
        }
        Ok((
            start,
            cands
                .into_iter()
                .map(|c| rustyline::completion::Pair {
                    display: c.text.clone(),
                    replacement: c.text,
                })
                .collect(),
        ))
    }
}

impl rustyline::hint::Hinter for QuaiHelper {
    type Hint = String;
}
impl rustyline::highlight::Highlighter for QuaiHelper {}
impl rustyline::validate::Validator for QuaiHelper {}
impl rustyline::Helper for QuaiHelper {}

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
fn command(session: &mut Session, remount: &mut Option<Remount>, line: &str) -> bool {
    if let Some(arg) = line.strip_prefix(":mount ").map(str::trim)
        && !arg.is_empty()
    {
        match remount {
            None => println!(
                "note: :mount is in-process only — under --daemon the arbor is \
                 pinned at start; restart quai with the source added"
            ),
            Some(ctx) => {
                let was_single =
                    matches!(ctx.specs.as_slice(), [one] if one.name.is_none());
                ctx.specs.push(MountSpec::parse(arg));
                match local_executor(ctx) {
                    Ok(executor) => {
                        session.set_executor(executor);
                        let names: Vec<String> = ctx
                            .specs
                            .iter()
                            .map(|s| {
                                s.name.clone().unwrap_or_else(|| {
                                    s.path
                                        .file_stem()
                                        .map(|x| x.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                })
                            })
                            .collect();
                        println!("mounted: /{}", names.join(", /"));
                        if was_single {
                            println!(
                                "note: sources now mount as named children — earlier \
                                 lines wrote root-relative paths"
                            );
                        }
                    }
                    Err(e) => {
                        ctx.specs.pop();
                        eprintln!("error: {e:#}");
                    }
                }
            }
        }
        return false;
    }
    match line {
        ":q" | ":quit" => return true,
        ":help" | ":?" => {
            println!(
                "  <query>       run a query; its result is labelled &N and reusable\n  \
                 &N            re-run line N (a macro); continue with a pipe: &N | /key::\n  \
                 &N#           replay line N's frozen output (as it was when it ran)\n  \
                 &N!           re-run line N live — re-reads the source; diverges from &N# under drift\n  \
                 def &x: …;    add a named fragment to the session\n  \
                 :mount SPEC   add a source (PATH or NAME=TARGET) to the session\n  \
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
