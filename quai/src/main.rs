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
use std::path::{Path, PathBuf};

/// Interactive Quarb: each line becomes a labelled fragment
/// (&1, &2, …), reusable over a standing session.
#[derive(Parser)]
#[command(
    version,
    about,
    after_help = "The manual: `quai man` lists the commands, `quai man NAME` prints a command's page, \
                  `quai man '<=>'` a glyph's (a pipe, a sigil, an operator), `quai man syntax [KIND]` \
                  lists the glyphs; `--ru` for Russian. Inside a session: `man NAME`."
)]
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
    #[arg(long)]
    graft: bool,

    /// Disable grafting entirely: archive members and text columns
    /// stay opaque leaves; refused with the code: prefix.
    #[arg(long = "no-graft", conflicts_with = "graft")]
    no_graft: bool,

    /// Retired: the shell is the environment mount `sh=shell:`
    /// among the inputs (`sh:tr(a-z; A-Z)`); the switch refuses.
    #[arg(long, hide = true)]
    allow_shell: bool,

    /// The diagnostics shown after a line's cells: error, warning
    /// (the default), info or debug; `session(--diagnostics=…)` in a
    /// line moves it for the rest of the session.
    #[arg(long, value_name = "LEVEL", default_value = "warning")]
    diagnostics: String,

    /// `--diagnostics=info`, shorter.
    #[arg(long, conflicts_with_all = ["diagnostics", "debug"])]
    info: bool,

    /// `--diagnostics=debug`, shorter.
    #[arg(long, conflicts_with = "diagnostics")]
    debug: bool,

    /// Colour: the severity word of a diagnostic and the `error` of a
    /// refusal, on a terminal (`auto`, the default: the stream is a
    /// terminal, NO_COLOR unset, TERM not dumb), `always`, or
    /// `never`. The text never changes, so a piped session reads
    /// the same.
    #[arg(long, value_name = "WHEN", default_value = "auto")]
    color: qua::ColorMode,

    /// Pin the invocation instant `now()` denotes (ISO-8601). Default:
    /// the clock, read once at startup, so a session's `now()` is
    /// stable across lines.
    #[arg(long, value_name = "ISO")]
    now: Option<String>,

    /// Seed the fragment table with definitions from a file
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
    /// Sentence bonds: a .desm file (repeatable; see `qua --help`),
    /// loaded for the session — the corpus: reading, `sentences`
    /// and `sc` agree on "Dr. Smith".
    #[arg(long, value_name = "FILE")]
    desm: Vec<PathBuf>,

    /// Back the session with a resident `qua` daemon: materialize the
    /// source once in a background process (shared across quai runs
    /// and with other clients) instead of in-process, and persist the
    /// fragment history under ~/.quarb. Best for expensive sources
    /// reused across sessions; for a RAM-sized source the default
    /// in-process mode is faster.
    #[arg(long)]
    daemon: bool,

    /// With --daemon, let the resident arbor warm-start from (and
    /// populate) the on-disk AST cache for source-code inputs. Cache
    /// and daemon are layers, not alternatives.
    #[arg(long)]
    cache: bool,

    /// Hold every line to a budget before it runs: `nodes=N`,
    /// `bytes=N`, `requests=N` and `strict`, comma-separated (a
    /// figure takes k/M/G or Ki/Mi/Gi; see `qua --help`). A line
    /// whose plan passes the bound is refused unrun; `:plan QUERY`
    /// prints a line's plan. In-process sessions only.
    #[arg(long, value_name = "SPEC")]
    budget: Option<String>,

    /// Run the cells of a .quai file in order, non-interactively, and
    /// print the transcript: `&N  <cell>` (continuation lines indented
    /// four spaces), then the cell's output indented four spaces, a
    /// blank line between cells; a cell ending in a session push
    /// prints nothing after its line.
    #[arg(long, value_name = "FILE.quai")]
    run: Option<PathBuf>,
}

/// What an in-session `:mount` needs to rebuild the executor; absent
/// under `--daemon` (the daemon's arbor is pinned at start).
struct Remount {
    specs: Vec<MountSpec>,
    opts: Options,
    now: (i64, u32),
    allow_shell: bool,
    /// The diagnostics level the command line seeded; a rebuilt
    /// executor starts from it again.
    diagnostics: quarb::Severity,
    /// The environments mounted from the command line.
    environments: Vec<quarb::Environment>,
    /// The command libraries the session runs with: the linked ones
    /// and the `python:` mounts bound as libraries.
    libraries: Vec<std::sync::Arc<dyn quarb::CommandLibrary>>,
    /// The `--model` view, reapplied on every rebuild.
    model: Option<quarb_model::Model>,
    /// `:follow` rounds per line: 0 = off (the default — refs are
    /// only reported), `on` = 1, N = up to N rounds.
    follow_depth: usize,
    /// Documents fetched by `:follow`, mounted beside the local
    /// sources and keyed by URL so the arrow lands on them. They
    /// persist for the rest of the session.
    web: Vec<WebDoc>,
}

/// One `:follow`-fetched document.
struct WebDoc {
    name: String,
    url: String,
    format: String,
    text: String,
}

/// Whether a target names an adapter scheme (qua's dispatch) rather
/// than a file. `git:` stays with the session's own opener.
fn is_schemed(s: &str) -> bool {
    !s.starts_with("git:")
        && s.split_once(':').is_some_and(|(sch, _)| {
            sch.len() >= 2 && sch.chars().all(|c| c.is_ascii_alphanumeric() || c == '+')
        })
}

/// Open one mount spec through the session's opener (quarb-open:
/// the whole fleet — files, readings, archives, databases, `gcl:`,
/// `kafka:`, `neo4j://`, …).
fn build_doc(spec: &MountSpec, opts: &Options) -> Result<(Doc, bool)> {
    let schemed = is_schemed(&spec.path.to_string_lossy());
    Ok((Doc::open(&spec.path, opts)?, schemed))
}

/// Build the in-process executor over the current mount specs plus
/// any `:follow`-fetched documents, the `--model` view reapplied.
/// A fresh session memory at the command line's diagnostics level:
/// what every executor the session builds starts from.
fn seeded_memory(remount: &Remount) -> quarb::SessionMemory {
    let memory = quarb::session_memory();
    memory.borrow_mut().diagnostics = remount.diagnostics;
    memory
}

/// The paint of each stream, decided once from `--color`: the
/// diagnostics go to stdout with the cells, the refusals to stderr.
static OUT_PAINT: std::sync::OnceLock<qua::Paint> = std::sync::OnceLock::new();
static ERR_PAINT: std::sync::OnceLock<qua::Paint> = std::sync::OnceLock::new();

fn set_color(mode: qua::ColorMode) {
    let _ = OUT_PAINT.set(qua::Paint::decide(mode, std::io::stdout().is_terminal()));
    let _ = ERR_PAINT.set(qua::Paint::decide(mode, std::io::stderr().is_terminal()));
}

fn out_paint() -> qua::Paint {
    *OUT_PAINT
        .get_or_init(|| qua::Paint::decide(qua::ColorMode::Auto, std::io::stdout().is_terminal()))
}

/// The `error` a refusal opens with, painted when stderr is.
fn error_prefix() -> String {
    ERR_PAINT
        .get_or_init(|| qua::Paint::decide(qua::ColorMode::Auto, std::io::stderr().is_terminal()))
        .error_word("error")
}

/// The level the command line asks for.
fn cli_level(cli: &Cli) -> Result<quarb::Severity> {
    if cli.debug {
        return Ok(quarb::Severity::Debug);
    }
    if cli.info {
        return Ok(quarb::Severity::Info);
    }
    cli.diagnostics
        .parse::<quarb::Severity>()
        .map_err(|e| anyhow::anyhow!("--diagnostics: {e}"))
}

fn local_executor(remount: &Remount) -> Result<Box<LocalExecutor>> {
    // No source at all: the calculator session — a bare root for
    // expression-headed lines (`= expr`; spec: The Expression
    // Head). `:mount` grafts real sources in later; until then &N!
    // reads the same bare root as &N.
    if remount.specs.is_empty() && remount.web.is_empty() {
        let doc = Doc::parse("{}", "json").map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(Box::new(
            LocalExecutor::new(doc, remount.now, remount.allow_shell)
                .with_environments(remount.environments.clone())
                .with_libraries(remount.libraries.clone())
                .with_model(remount.model.clone())
                .with_session(seeded_memory(remount)),
        ));
    }
    let mut schemed = false;
    let mut doc = match (remount.specs.as_slice(), remount.web.is_empty()) {
        ([one], true) if one.name.is_none() => {
            let (doc, sch) = build_doc(one, &remount.opts)?;
            schemed |= sch;
            doc
        }
        (many, _) => {
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
            // The fetched documents mount beside the locals, each
            // declaring its URL so the arrow lands on it and its
            // relative hrefs join correctly.
            for w in &remount.web {
                let mut doc = Doc::parse(&w.text, &w.format)
                    .with_context(|| format!("parsing '{}' ({})", w.name, w.url))?;
                doc.attach_url(&w.url);
                parts.push((w.name.clone(), doc));
            }
            Doc::mount_docs(parts)?
        }
    };
    let urls: Vec<(String, String)> = remount
        .web
        .iter()
        .map(|w| (w.name.clone(), w.url.clone()))
        .collect();
    let roots = doc.url_roots(&urls);
    doc.declare_identities(&urls);
    // Live re-reads (&N!) re-open through the file path machinery,
    // which scheme targets bypass — and fetched documents are not
    // re-fetched: those sessions read the standing snapshot for
    // both &N and &N!.
    if schemed || !remount.web.is_empty() {
        return Ok(Box::new(
            LocalExecutor::new(doc, remount.now, remount.allow_shell)
                .with_model(remount.model.clone())
                .with_url_roots(roots)
                .with_environments(remount.environments.clone())
                .with_libraries(remount.libraries.clone())
                .with_session(seeded_memory(remount)),
        ));
    }
    Ok(Box::new(
        LocalExecutor::with_respec(
            doc,
            remount.now,
            remount.allow_shell,
            remount.specs.clone(),
            remount.opts.clone(),
        )
        .with_environments(remount.environments.clone())
        .with_model(remount.model.clone())
        .with_libraries(remount.libraries.clone())
        .with_session(seeded_memory(remount)),
    ))
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
    // A refusal is a message, as in qua: `RUST_BACKTRACE=1` is a
    // panic's; an error's stack is asked for by `RUST_LIB_BACKTRACE=1`.
    if std::env::var_os("RUST_LIB_BACKTRACE").is_none() {
        unsafe {
            std::env::set_var("RUST_LIB_BACKTRACE", "0");
        }
    }
    // `quai man [--ru] [NAME | GLYPH | syntax]`: the manual from the
    // shell, as `qua man` is — the text `:man` answers inside a
    // session. Read before the arguments are parsed, where `man`
    // was taken for a source to mount ("reading man: No such file").
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("man") {
        let args: Vec<&str> = argv[2..].iter().map(String::as_str).collect();
        return match quarb::man::man(&args) {
            Ok(text) => {
                println!("{text}");
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("{e}")),
        };
    }
    let mut cli = Cli::parse();
    set_color(cli.color);
    // A --model file may declare its own sources; parse it and inject
    // the mounts (relative targets resolved against the model file's
    // directory) as NAME=TARGET inputs, ahead of any positional ones.
    // The mounts the model states, by the input each is injected
    // as: the name as the statement gives it, never read back by
    // the command line's narrower `NAME=TARGET` rule.
    let mut stated: std::collections::HashMap<String, MountSpec> = Default::default();
    let model = match &cli.model {
        Some(path) => {
            let m = quarb_model::parse_model_file(path)
                .map_err(|e| anyhow::anyhow!("parsing model {}: {e}", path.display()))?;
            let base_dir = path.parent();
            for mt in m.mounts.iter().rev() {
                let target = quarb_model::resolve_mount_target(&mt.target, base_dir);
                let target = resolve_option_files(&target, base_dir);
                // An environment is mounted from the command line
                // only: a model file mounts sources, never code.
                if quarb::Environment::is_environment_target(&target) {
                    anyhow::bail!(
                        "model {}: mount '{}' names an environment ({target}); environments \
                         are mounted from the command line only",
                        path.display(),
                        mt.name
                    );
                }
                let input = format!("{}={}", mt.name, target);
                stated.insert(input.clone(), MountSpec::stated(&mt.name, &target));
                cli.paths.insert(0, input);
            }
            Some(m)
        }
        None => None,
    };
    if cli.paths.is_empty() && cli.daemon {
        anyhow::bail!("--daemon needs at least one source (a directory, a document, or git:PATH)");
    }
    // The raw inputs go to a daemon as written (qua takes its own
    // environment mounts off them); the local session takes them
    // off here, from the command line only.
    let raw_paths: Vec<PathBuf> = cli.paths.iter().map(PathBuf::from).collect();
    let mut environments: Vec<quarb::Environment> = Vec::new();
    let mut kept = Vec::new();
    for p in std::mem::take(&mut cli.paths) {
        match quarb::Environment::parse_mount(&p) {
            None => kept.push(p),
            Some(Ok(e)) => {
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
    if cli.allow_shell {
        anyhow::bail!(
            "--allow-shell is retired: mount the shell, sh=shell:, and write sh:tr(a-z; A-Z)"
        );
    }
    if environments.iter().any(|e| e.kind == quarb::EnvKind::Shell) {
        cli.allow_shell = true;
    }
    // The linked libraries and the python: mounts bound as
    // libraries, as qua binds them.
    let libraries = qua::session_libraries(&environments)?;
    let specs: Vec<MountSpec> = cli
        .paths
        .iter()
        .map(|a| {
            stated
                .get(a)
                .cloned()
                .unwrap_or_else(|| MountSpec::parse(a))
        })
        .collect();
    let mut remount: Option<Remount> = None;
    let session = if cli.daemon {
        // The daemon holds the arbor (via `qua --resident`); the store
        // persists the macro history across runs. Raw args pass
        // through: `qua` itself understands NAME=TARGET.
        // The daemon reads the model itself and mounts what it
        // states: handing it those mounts as inputs too mounted each
        // name twice, and the resident session refused to start.
        let own: Vec<PathBuf> = raw_paths
            .iter()
            .filter(|p| p.to_str().is_none_or(|a| !stated.contains_key(a)))
            .cloned()
            .collect();
        let executor = Box::new(DaemonExecutor::new(
            own,
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
        let bonds = if cli.desm.is_empty() {
            None
        } else {
            let mut b = syndesmos::Syndesmos::empty();
            for f in &cli.desm {
                b.extend(
                    syndesmos::Syndesmos::load(f)
                        .map_err(|e| anyhow::anyhow!("reading {}: {e}", f.display()))?,
                );
            }
            Some(std::rc::Rc::new(b))
        };
        quarb::set_sentence_bonds(
            bonds
                .clone()
                .map(|b| b as std::rc::Rc<dyn quarb::SentenceBonds>),
        );
        let opts = Options {
            hidden: cli.hidden,
            respect_ignore: !cli.no_ignore,
            graft: cli.graft,
            no_graft: cli.no_graft,
            refs: cli.refs.clone(),
            allow_shell: cli.allow_shell,
            bonds,
        };
        let ctx = Remount {
            specs,
            opts,
            now,
            allow_shell: cli.allow_shell,
            diagnostics: cli_level(&cli)?,
            environments,
            libraries,
            model: model.clone(),
            // Batteries included: the arrow in a hand-typed line is
            // the consent, so an interactive session follows refs
            // by default (one round). A piped session stays off —
            // nobody is watching a script fetch (:follow on opts
            // in; the refs note says so).
            follow_depth: usize::from(std::io::stdin().is_terminal()),
            web: Vec::new(),
        };
        let executor = local_executor(&ctx)?;
        remount = Some(ctx);
        Session::new(executor, Box::new(MemStore))
    };
    let session = std::rc::Rc::new(std::cell::RefCell::new(session));
    // A model's own `def`/`macro` statements come first, then --defs.
    if let Some(m) = &model
        && !m.defs_text.trim().is_empty()
    {
        session.borrow_mut().seed_defs(&m.defs_text)?;
    }
    if let Some(p) = &cli.defs {
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        session.borrow_mut().seed_defs(&text)?;
    }
    if let Some(spec) = &cli.budget {
        let b = quarb::plan::Budget::parse(spec).map_err(|e| anyhow::anyhow!("--{e}"))?;
        session.borrow_mut().set_budget(Some(b));
    }
    if let Some(file) = &cli.run {
        return run_file(&session, &mut remount, file);
    }
    let sources = if cli.paths.is_empty() {
        "a bare root — calculator session; lines open with '= expr', :mount adds sources"
            .to_string()
    } else {
        cli.paths.join(", ")
    };
    let mode = if cli.daemon {
        "daemon-backed"
    } else {
        "in-process"
    };
    println!(
        "quai — interactive Quarb over {sources} ({mode}).  :help for commands, :quit (or Ctrl-D) to leave."
    );
    repl(&session, &mut remount)
}

/// Bind the invocation instant: `--now` pins it; otherwise the clock,
/// read once, so every `now()` in the session denotes one point — and
/// the adapters resolve their relative windows against the same one.
/// `quai --run FILE.quai SOURCES…` (the replayer's contract): the
/// file's cells — blocks of lines indented four spaces, a blank line
/// between them — run in order over the session, and the transcript
/// is printed as the guides show it: `&N  <first line>`, the
/// continuation lines and the output indented four spaces, a blank
/// line after each cell; a cell whose last command is a session push
/// prints nothing after its lines; a refusal prints as `error: …`.
/// The notebook shape (spec: .quai; the playground's `?src=` door
/// reads the same file): a block whose lines are not indented is
/// Markdown prose, skipped in silence; a cell whose first token is a
/// model-file keyword is a MODEL CELL, applied to the session — its
/// `mount NAME: TARGET;` statements mount sources (a relative target
/// against the file's directory; a URL stays as it is), its other
/// statements join the session's model — and takes its ordinal with
/// no output; a `def`/`macro` cell extends the fragment table and
/// takes its ordinal the same way (spec: .quai, Numbering).
fn run_file(
    session: &std::rc::Rc<std::cell::RefCell<Session>>,
    remount: &mut Option<Remount>,
    file: &Path,
) -> Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let mut cells: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut indented = true;
    let mut close = |current: &mut Vec<String>, indented: &mut bool| {
        if !current.is_empty() && *indented {
            cells.push(std::mem::take(current));
        }
        current.clear();
        *indented = true;
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            close(&mut current, &mut indented);
            continue;
        }
        match line
            .strip_prefix("    ")
            .or_else(|| line.strip_prefix('\t'))
        {
            Some(rest) => current.push(rest.to_string()),
            None => {
                indented = false;
                current.push(line.to_string());
            }
        }
    }
    close(&mut current, &mut indented);
    let base_dir = file.parent();
    let mut failed = false;
    for cell in cells {
        let mut q = cell.join("\n");
        // A manual cell (`man sort`): the page, as a session's line
        // answers it. It is no query: it takes no ordinal, so the
        // labels of the cells after it are the ones every host gives.
        if let Some(page) = quarb::man::line(&q) {
            println!("    {}", cell[0]);
            match page {
                Ok(text) => text.lines().for_each(|l| println!("    {l}")),
                Err(e) => {
                    failed = true;
                    println!("    error: {e}");
                }
            }
            println!();
            continue;
        }
        let n = session.borrow().line_no();
        println!("&{n}  {}", cell[0]);
        for l in &cell[1..] {
            println!("    {l}");
        }
        // A file names its cell: `&_` and `&-N` are the interactive
        // forms (ruling 2026-11-11), and "the previous cell" would
        // break in silence when cells are reordered. The cell takes
        // its number, so the labels still match the file's.
        if let Some(why) = quarb::previous_line_in_a_file(&q) {
            failed = true;
            println!("    error: {why}");
            session.borrow_mut().commit(&q, Vec::new());
            println!();
            continue;
        }
        // A model cell: mounts and declarations, no output.
        let first = q.split_whitespace().next().unwrap_or_default();
        if first == "def" || first == "macro" {
            if let Err(e) = session.borrow_mut().add_def(&q) {
                failed = true;
                println!("    error: {e:#}");
            }
            println!();
            continue;
        }
        // `mount: ?options` re-reads the one source; the bare
        // `mount:` is no keyword to the splitter above.
        let re_read = remount_cell(&q);
        if quarb_model::STATEMENTS.contains(&first) || re_read.is_some() {
            let cell = re_read.as_deref().unwrap_or(&q);
            if let Err(e) = apply_model_cell(session, remount, cell, base_dir) {
                failed = true;
                println!("    error: {e:#}");
            }
            session.borrow_mut().commit(&q, Vec::new());
            println!();
            continue;
        }
        // A capture reference, as the session reads it (spec: .quai;
        // `:help`): `&N#` replays line N's frozen footprint and takes
        // the next number, `&N!` re-runs line N against a freshly read
        // source; the engine alone would refuse both spellings. Either
        // may head a pipeline (`&4# | ,df`, `&4! | count`; finding
        // 2026-11-05), as the bare `&4 | …` does through the macro
        // table. Only a cell that starts so is a history reference;
        // any other `#` (a table literal, `#.`, a string) is the
        // query's own.
        let reference = match history_head(q.trim()) {
            Some((n, '#', "")) => Some(Prepared::Frozen(n)),
            Some((n, '#', tail)) => {
                // Bound first: matched in place, the session's borrow
                // lived across the arms, and the error arm's
                // `borrow_mut` panicked ("RefCell already borrowed"),
                // losing the rest of the notebook. The REPL binds it
                // the same way.
                let headed = frozen_head_query(&session.borrow(), n, tail);
                match headed {
                    Ok(headed) => Some(Prepared::Eval(headed)),
                    Err(e) => {
                        failed = true;
                        println!("    error: {e}");
                        session.borrow_mut().commit(&q, Vec::new());
                        println!();
                        continue;
                    }
                }
            }
            Some((n, '!', tail)) => Some(Prepared::Live(
                format!("&{n} {tail}").trim_end().to_string(),
            )),
            _ => None,
        };
        match reference {
            Some(Prepared::Frozen(n)) => {
                let cells = session.borrow().frozen(n).cloned();
                match cells {
                    Some(cells) => {
                        let memory = session.borrow().session_memory();
                        for c in &cells {
                            for l in c.display_in(memory.as_ref()).lines() {
                                println!("    {l}");
                            }
                        }
                        session.borrow_mut().record_frozen(cells);
                    }
                    None => {
                        failed = true;
                        println!("    error: &{n}# has no captured result (line {n} hasn't run)");
                        session.borrow_mut().commit(&q, Vec::new());
                    }
                }
                println!();
                continue;
            }
            Some(Prepared::Live(live)) => {
                let result = session.borrow().eval_fresh(&live);
                match result {
                    Ok(cells) => {
                        let memory = session.borrow().session_memory();
                        for c in &cells {
                            for l in c.display_in(memory.as_ref()).lines() {
                                println!("    {l}");
                            }
                        }
                        print_diagnostics(&session.borrow(), "    ");
                        session.borrow_mut().commit(&live, cells);
                    }
                    Err(e) => {
                        failed = true;
                        println!("    error: {e:#}");
                        session.borrow_mut().commit(&live, Vec::new());
                    }
                }
                println!();
                continue;
            }
            Some(Prepared::Eval(headed)) => q = headed,
            _ => {}
        }
        // a session push and a `session(…)` that sets answer nothing:
        // no line after them (the engine yields no thread; the guard
        // here keeps the transcript's shape whatever the host prints)
        let silent = q
            .rsplit('|')
            .next()
            .is_some_and(|last| last.trim_start().starts_with(','))
            || (q.trim_start().starts_with("session(") && !q.contains('|'));
        let result = session.borrow().eval_table(&q);
        match result {
            Ok((cells, grid)) => {
                if !silent {
                    match grid {
                        Some(g) => {
                            for l in g.lines() {
                                println!("    {l}");
                            }
                        }
                        None => {
                            let memory = session.borrow().session_memory();
                            for c in &cells {
                                for l in c.display_in(memory.as_ref()).lines() {
                                    println!("    {l}");
                                }
                            }
                        }
                    }
                }
                print_diagnostics(&session.borrow(), "    ");
                session.borrow_mut().commit(&q, cells);
            }
            Err(e) => {
                failed = true;
                println!("    error: {e:#}");
                session.borrow_mut().commit(&q, Vec::new());
            }
        }
        println!();
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// The reading schemes a mount target may open with, as on the
/// command line (`text:FILE`, `corpus:DIR`, `lit:FILE`, `koine:FILE`):
/// a relative path behind one resolves against the file's directory.
const READING_SCHEMES: [&str; 8] = [
    "text", "corpus", "lit", "koine", "json2", "yaml2", "toml2", "cbor2",
];

/// Apply a model cell to the session: parse it as a model file, mount
/// what it mounts (named, relative targets against `base_dir`), join
/// its declarations to the session's model, seed its defs, and
/// rebuild the executor.
fn apply_model_cell(
    session: &std::rc::Rc<std::cell::RefCell<Session>>,
    remount: &mut Option<Remount>,
    text: &str,
    base_dir: Option<&Path>,
) -> Result<()> {
    let m = quarb_model::parse_model(text).map_err(|e| anyhow::anyhow!("model cell: {e}"))?;
    let Some(ctx) = remount else {
        anyhow::bail!("a model cell needs an in-process session (not --daemon)");
    };
    let added = ctx.specs.len();
    // the targets a re-read replaces, to put back if it is refused
    let mut replaced: Vec<(usize, std::path::PathBuf)> = Vec::new();
    for mt in &m.mounts {
        // `mount NAME: ?options` (ruling 2026-11-12): the source
        // already mounted as NAME, read again under the options, in
        // place of whatever its target carried after `?`. The name
        // may be left out where one source is mounted.
        if let Some(options) = mt.target.strip_prefix('?') {
            let names = |specs: &[MountSpec]| {
                specs
                    .iter()
                    .map(mounted_name)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let at = if mt.name.is_empty() {
                if ctx.specs.len() != 1 {
                    anyhow::bail!(
                        "name the source to re-read, 'mount NAME: ?{options}'; the sources: {}",
                        names(&ctx.specs)
                    );
                }
                0
            } else {
                ctx.specs
                    .iter()
                    .position(|s| mounted_name(s) == mt.name)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "no source is mounted as '{}'; the sources: {}",
                            mt.name,
                            names(&ctx.specs)
                        )
                    })?
            };
            let old = ctx.specs[at].path.clone();
            let target = old.to_string_lossy();
            // a file whose name holds a `?` is taken as it is
            let bare = match target.rsplit_once('?') {
                Some((path, _)) if !old.exists() => path.to_string(),
                _ => target.to_string(),
            };
            ctx.specs[at].path = if options.is_empty() {
                std::path::PathBuf::from(bare)
            } else {
                std::path::PathBuf::from(resolve_option_files(
                    &format!("{bare}?{options}"),
                    base_dir,
                ))
            };
            replaced.push((at, old));
            continue;
        }
        // A reading scheme ahead of a relative path (`text:tom.txt`,
        // `corpus:dir/`) resolves the path against the file's
        // directory too, scheme kept; a URL or an absolute path stays.
        let target = match mt.target.split_once(':') {
            Some((scheme, rest))
                if READING_SCHEMES.contains(&scheme)
                    && !rest.starts_with("//")
                    && !std::path::Path::new(rest).is_absolute() =>
            {
                format!(
                    "{scheme}:{}",
                    quarb_model::resolve_mount_target(rest, base_dir)
                )
            }
            _ => quarb_model::resolve_mount_target(&mt.target, base_dir),
        };
        let target = resolve_option_files(&target, base_dir);
        if quarb::Environment::is_environment_target(&target) {
            anyhow::bail!(
                "mount '{}' names an environment ({target}); environments are mounted \
                 from the command line only",
                mt.name
            );
        }
        ctx.specs.push(MountSpec::stated(&mt.name, &target));
    }
    let merged = match ctx.model.take() {
        None => m.clone(),
        Some(mut base) => {
            base.absorb(&m);
            base
        }
    };
    ctx.model = Some(merged);
    match local_executor(ctx) {
        Ok(executor) => session.borrow_mut().set_executor(executor),
        Err(e) => {
            ctx.specs.truncate(added);
            for (at, old) in replaced {
                ctx.specs[at].path = old;
            }
            return Err(e);
        }
    }
    if !m.defs_text.trim().is_empty() {
        session.borrow_mut().seed_defs(&m.defs_text)?;
    }
    Ok(())
}

/// The mount options that name a file of their own.
const FILE_OPTIONS: [&str; 5] = ["conllu", "sentences", "desm", "cast", "places"];

/// A mount target with the files its options name resolved as the
/// target itself is: a relative one against the notebook's
/// directory (`base_dir`), a URL or an absolute path left alone.
/// Such a file was read against the working directory, so a notebook
/// run from another directory failed its mount and the cells after
/// it answered nothing.
fn resolve_option_files(target: &str, base_dir: Option<&Path>) -> String {
    let Some((path, query)) = target.rsplit_once('?') else {
        return target.to_string();
    };
    let pairs: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, file)) if FILE_OPTIONS.contains(&key) && !file.is_empty() => {
                format!(
                    "{key}={}",
                    quarb_model::resolve_mount_target(file, base_dir)
                )
            }
            _ => pair.to_string(),
        })
        .collect();
    format!("{path}?{}", pairs.join("&"))
}

/// The name a spec is mounted under: its alias, else its file's
/// stem (a reading prefix and the options after `?` set aside).
fn mounted_name(spec: &MountSpec) -> String {
    if let Some(name) = &spec.name {
        return name.clone();
    }
    let target = spec.path.to_string_lossy();
    let path = match target.rsplit_once('?') {
        Some((path, _)) if !spec.path.exists() => path,
        _ => &target,
    };
    let path = match path.split_once(':') {
        Some((scheme, rest)) if READING_SCHEMES.contains(&scheme) => rest,
        _ => path,
    };
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A line or a cell that re-reads mounted sources under options,
/// as a model cell the parser reads: `mount NAME: ?options`, the
/// name left out where one source is mounted (`mount: ?null=NA`),
/// a closing `;` or none. `None` for any other text: a query, or a
/// model cell whose mounts name targets.
fn remount_cell(text: &str) -> Option<String> {
    let mut out = String::new();
    for stmt in text.split(';') {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        let rest = stmt.strip_prefix("mount")?;
        if !rest.starts_with(|c: char| c.is_whitespace() || c == ':') {
            return None;
        }
        let (name, target) = rest.split_once(':')?;
        if !target.trim().starts_with('?') {
            return None;
        }
        out.push_str(&format!("mount {}: {};\n", name.trim(), target.trim()));
    }
    (!out.is_empty()).then_some(out)
}

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
                eprintln!("{}: {e}", error_prefix());
                break;
            }
        };
        let line = input.trim();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(line); // Up/Down recalls prior lines
        // The manual: `man sort`, the line every host of a session
        // takes (a query never opens with a bare word).
        if let Some(page) = quarb::man::line(line) {
            match page {
                Ok(text) => println!("{text}"),
                Err(e) => println!("{e}"),
            }
            continue;
        }
        // `mount NAME: ?options`: a mounted source read again under
        // options (ruling 2026-11-12), the line every host of a
        // session takes. It takes its ordinal, as a notebook's model
        // cell does, and answers nothing; a refused re-read leaves
        // the source as it was and takes none.
        if let Some(cell) = remount_cell(line) {
            match apply_model_cell(&session, remount, &cell, None) {
                Ok(()) => {
                    session.borrow_mut().commit(line, Vec::new());
                }
                Err(e) => eprintln!("{}: {e:#}", error_prefix()),
            }
            continue;
        }
        // A `:` command (a query cannot start with a lone `:`).
        if line.starts_with(':') && !line.starts_with("::") {
            if command(&mut session.borrow_mut(), remount, line) {
                break;
            }
            continue;
        }
        // `&_` — the line before this one, `&-N` the line N back
        // (ruling 2026-11-11): resolved here to the label it names,
        // so the line is read, recalled and saved as the `&N` line it
        // is. A failed line takes no label, so the latest label is
        // the last line that ran.
        let last = session.borrow().line_no().saturating_sub(1);
        let resolved = match quarb::resolve_previous_line(line, last) {
            Ok(resolved) => resolved,
            Err(e) => {
                eprintln!("{}: {e}", error_prefix());
                continue;
            }
        };
        let line = resolved.as_deref().unwrap_or(line);
        // A definition extends the macro table but is not itself run.
        if line.starts_with("def ")
            || line == "def"
            || line.starts_with("macro ")
            || line == "macro"
        {
            if let Err(e) = session.borrow_mut().add_def(line) {
                eprintln!("{}: {e:#}", error_prefix());
            }
            continue;
        }
        // A capture reference (`&N#` frozen, `&N!` live) is resolved
        // by the session, not the engine — the engine's lexer has no
        // `#`, and its `!` signage rejects a bang on a pure fragment.
        match prepare(line) {
            Err(e) => eprintln!("{}: {e}", error_prefix()),
            // (the snapshot is cloned out before the session is
            // borrowed again to record the replay: a borrow held across
            // the match scrutinee poisoned every later line)
            Ok(Prepared::Frozen(n)) => {
                let cells = session.borrow().frozen(n).cloned();
                match cells {
                    Some(cells) => {
                        for c in &cells {
                            println!("{}", c.display());
                        }
                        session.borrow_mut().record_frozen(cells);
                    }
                    None => eprintln!(
                        "{}: &{n}# has no captured result (line {n} hasn't run)",
                        error_prefix()
                    ),
                }
            }
            Ok(Prepared::FrozenHead(n, tail)) => {
                let headed = frozen_head_query(&session.borrow(), n, &tail);
                match headed {
                    Ok(q) => run_and_commit(&mut session.borrow_mut(), remount, &q, false),
                    Err(e) => eprintln!("{}: {e}", error_prefix()),
                }
            }
            Ok(Prepared::Live(q)) => run_and_commit(&mut session.borrow_mut(), remount, &q, true),
            Ok(Prepared::Eval(q)) => run_and_commit(&mut session.borrow_mut(), remount, &q, false),
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
            .map_or(0, |i| {
                i + head[i..].chars().next().map_or(1, char::len_utf8)
            });
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

/// Fetch one external reference: a plain GET, the format from the
/// content type, else a shape sniff. The raw bytes as served — the
/// curl view, not a rendered DOM (that is the browser extension's
/// grab).
fn fetch_web(url: &str) -> Result<(String, String)> {
    const CAP: u64 = 10_000_000;
    // under the offline switch a link is not followed off this
    // machine (the shared agent would refuse it too; this says why)
    quarb_http::check_url(url).map_err(|e| anyhow::anyhow!("{e}"))?;
    let resp = quarb_http::agent()
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let ct = resp.content_type().to_ascii_lowercase();
    let mut text = String::new();
    use std::io::Read;
    resp.into_reader()
        .take(CAP + 1)
        .read_to_string(&mut text)
        .context("reading the body")?;
    if text.len() as u64 > CAP {
        anyhow::bail!("too large (the cap is {CAP} bytes)");
    }
    let t = text.trim_start();
    let format = if ct.contains("html") {
        "html"
    } else if ct.contains("json") {
        "json"
    } else if ct.contains("xml") {
        "xml"
    } else if t.starts_with('{') || t.starts_with('[') {
        "json"
    } else if t.to_ascii_lowercase().starts_with("<!doctype html") || t.starts_with("<html") {
        "html"
    } else if t.starts_with('<') {
        "xml"
    } else {
        "text"
    };
    Ok((format.to_string(), text))
}

/// Prefix the query's leading path opener with `prefix` — needed
/// when a follow turns a single path-bare source into named
/// children. Only the leading opener moves: a path after the
/// arrow or inside a command is relative to its context, not the
/// root (a multi-branch join may need a hand adjustment — the
/// note prints exactly what re-ran).
fn rebase(q: &str, prefix: &str) -> String {
    let t = q.trim_start();
    if t.starts_with('/') {
        format!("{prefix}{t}")
    } else {
        q.to_string()
    }
}

/// Acquire the run's unresolved external references and re-run the
/// line — up to `follow_depth` rounds. Returns the (possibly
/// rebased) query text the final result belongs to.
fn follow_refs(
    session: &mut Session,
    remount: &mut Option<Remount>,
    q: &str,
) -> (String, Option<anyhow::Result<Vec<quarb_session::Cell>>>) {
    let mut q = q.to_string();
    let mut result = None;
    let mut rounds = 0;
    loop {
        let refs = session.refs();
        if refs.is_empty() {
            break;
        }
        let Some(ctx) = remount.as_mut() else {
            eprintln!(
                "note: {} external reference(s) not mounted — under --daemon the \
                 arbor is pinned at start",
                refs.len()
            );
            break;
        };
        if rounds >= ctx.follow_depth {
            let shown: Vec<&str> = refs.iter().take(3).map(String::as_str).collect();
            let more = refs.len().saturating_sub(3);
            eprintln!(
                "note: {} external reference(s) not mounted: {}{} — {}",
                refs.len(),
                shown.join(", "),
                if more > 0 {
                    format!(" (+{more})")
                } else {
                    String::new()
                },
                if ctx.follow_depth == 0 {
                    ":follow on acquires and re-runs"
                } else {
                    ":follow N raises the rounds"
                },
            );
            break;
        }
        rounds += 1;
        let was_single_bare =
            ctx.web.is_empty() && matches!(ctx.specs.as_slice(), [one] if one.name.is_none());
        let single_stem = ctx.specs.first().and_then(|s| {
            s.name
                .clone()
                .or_else(|| s.path.file_stem().map(|x| x.to_string_lossy().into_owned()))
        });
        let mut fetched = 0;
        for url in &refs {
            let taken: std::collections::HashSet<&str> =
                ctx.web.iter().map(|w| w.name.as_str()).collect();
            let mut n = 1;
            while taken.contains(format!("page{n}").as_str()) {
                n += 1;
            }
            match fetch_web(url) {
                Ok((format, text)) => {
                    eprintln!("followed: /page{n} <- {url} ({format})");
                    ctx.web.push(WebDoc {
                        name: format!("page{n}"),
                        url: url.clone(),
                        format,
                        text,
                    });
                    fetched += 1;
                }
                Err(e) => eprintln!("follow: {url}: {e:#}"),
            }
        }
        if fetched == 0 {
            break;
        }
        match local_executor(ctx) {
            Ok(executor) => session.set_executor(executor),
            Err(e) => {
                eprintln!("{} rebuilding the mount: {e:#}", error_prefix());
                break;
            }
        }
        if was_single_bare {
            // The lone source now mounts as a named child; the
            // line's root-relative paths move with it.
            if let Some(stem) = single_stem {
                let rebased = rebase(&q, &format!("/{stem}"));
                if rebased != q {
                    eprintln!(
                        "note: sources now mount as named children — re-running as: {rebased}"
                    );
                    q = rebased;
                }
            }
        }
        result = Some(session.eval(&q));
        if result.as_ref().is_some_and(|r| r.is_err()) {
            break;
        }
    }
    (q, result)
}

/// Evaluate a query line (against the standing arbor, or `fresh` for a
/// live re-read), follow any external references the run surfaced,
/// print the result, and register it as `&N`.
fn run_and_commit(session: &mut Session, remount: &mut Option<Remount>, q: &str, fresh: bool) {
    let mut result = if fresh {
        session.eval_fresh(q)
    } else {
        session.eval(q)
    };
    let mut q = q.to_string();
    if result.is_ok() {
        let (q2, followed) = follow_refs(session, remount, &q);
        q = q2;
        if let Some(r) = followed {
            result = r;
        }
    }
    match result {
        Ok(cells) => {
            for c in &cells {
                println!("{}", c.display());
            }
            print_diagnostics(session, "");
            let n = session.line_no();
            if !session.commit(&q, cells) {
                eprintln!("note: &{n} is not referenceable (its shape can't be a fragment's body)");
            }
        }
        Err(e) => eprintln!("{}: {e:#}", error_prefix()),
    }
}

/// The diagnostics the last evaluation raised beside its answer,
/// down to the session's level (`session(--diagnostics=…)`), each on
/// its own line after the cell's lines (spec: Diagnostics — a
/// diagnostic belongs to the cell that raised it).
fn print_diagnostics(session: &Session, indent: &str) {
    let level = session.diagnostics_level();
    let paint = out_paint();
    for d in session.diagnostics() {
        if d.severity.shown_at(level) {
            println!("{indent}{}", paint.line(&d));
        }
    }
}

/// How a line resolves once capture refs are handled.
enum Prepared {
    /// A standalone `&N#` — replay line N's frozen footprint.
    Frozen(usize),
    /// `&N# | …` — line N's frozen footprint heading a pipeline.
    FrozenHead(usize, String),
    /// A `&N!` live reading — re-run line N against a freshly
    /// re-materialized source.
    Live(String),
    /// Ordinary query text, run against the standing arbor.
    Eval(String),
}

fn prepare(line: &str) -> Result<Prepared> {
    // `&N#` / `&N!` alone, or heading a pipeline (finding
    // 2026-11-05); any other `#` is the query's own (`#.`, `#(…)`)
    Ok(match history_head(line) {
        Some((n, '#', "")) => Prepared::Frozen(n),
        Some((n, '#', tail)) => Prepared::FrozenHead(n, tail.to_string()),
        Some((n, '!', tail)) => Prepared::Live(format!("&{n} {tail}").trim_end().to_string()),
        _ => Prepared::Eval(line.to_string()),
    })
}

/// `&N#` / `&N!` heading a line, alone or before a pipe tail
/// (`&4# | ,df`): N, the suffix, and the tail from its pipe (empty
/// when the reference stands alone).
fn history_head(line: &str) -> Option<(usize, char, &str)> {
    let rest = line.trim().strip_prefix('&')?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let n: usize = rest[..digits].parse().ok()?;
    let rest = &rest[digits..];
    let suffix = rest.chars().next().filter(|c| matches!(c, '#' | '!'))?;
    let tail = rest[1..].trim_start();
    if !(tail.is_empty()
        || tail.starts_with('|')
        || tail.starts_with("@|")
        || tail.starts_with("*__"))
    {
        return None;
    }
    Some((n, suffix, tail))
}

/// `&N# | …` — line N's frozen footprint as the head of a pipeline:
/// the captured cells as literals, one cell the value itself,
/// several a list spread into one thread each (a node cell its
/// locator as text), then the tail.
fn frozen_head_query(
    session: &Session,
    n: usize,
    tail: &str,
) -> std::result::Result<String, String> {
    let cells = session
        .frozen(n)
        .ok_or_else(|| format!("&{n}# has no captured result (line {n} hasn't run)"))?;
    let lits: Vec<String> = cells
        .iter()
        .map(|c| match c {
            quarb_session::Cell::Value(v) => v.to_quarb(),
            quarb_session::Cell::Node(s) => quarb::Value::Str(s.clone()).to_quarb(),
        })
        .collect();
    let head = match lits.as_slice() {
        [one] => one.clone(),
        many => format!("@({}) | ...", many.join("; ")),
    };
    Ok(format!("= {head} {tail}"))
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
                let was_single = matches!(ctx.specs.as_slice(), [one] if one.name.is_none());
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
                        eprintln!("{}: {e:#}", error_prefix());
                    }
                }
            }
        }
        return false;
    }
    if let Some(arg) = line.strip_prefix(":plan ").map(str::trim)
        && !arg.is_empty()
    {
        match session.plan_text(arg) {
            Ok(text) => print!("{text}"),
            Err(e) => eprintln!("{}: {e:#}", error_prefix()),
        }
        return false;
    }
    if line == ":follow" || line.starts_with(":follow ") {
        let arg = line.strip_prefix(":follow").unwrap_or_default().trim();
        match remount {
            None => println!(
                "note: :follow is in-process only — under --daemon the arbor is \
                 pinned at start"
            ),
            Some(ctx) => {
                match arg {
                    "" => {}
                    "off" => ctx.follow_depth = 0,
                    "on" => ctx.follow_depth = 1,
                    n => match n.parse::<usize>() {
                        Ok(v) => ctx.follow_depth = v,
                        Err(_) => {
                            println!("usage: :follow [on|off|N] — rounds of acquisition per line");
                            return false;
                        }
                    },
                }
                println!(
                    "follow: {} — a line's unresolved external references (::href-->) {}",
                    match ctx.follow_depth {
                        0 => "off".to_string(),
                        1 => "on (1 round)".to_string(),
                        n => format!("on ({n} rounds)"),
                    },
                    if ctx.follow_depth == 0 {
                        "are reported, not fetched"
                    } else {
                        "are fetched, mounted, and the line re-runs"
                    },
                );
            }
        }
        return false;
    }
    // the manual, with the colon or without: `man sort` is the line
    // every host of a session takes (the browser's have no colon
    // commands)
    if let Some(page) = quarb::man::line(line) {
        match page {
            Ok(text) => println!("{text}"),
            Err(e) => println!("{e}"),
        }
        return false;
    }
    match line {
        ":q" | ":quit" => return true,
        ":help" | ":?" => {
            println!(
                "  <query>       run a query; its result is labelled &N and reusable\n  \
                 &N            re-run line N (a labelled fragment); continue with a pipe: &N | /key::\n  \
                 &N#           replay line N's frozen output (as it was when it ran)\n  \
                 &N!           re-run line N live — re-reads the source; diverges from &N# under drift\n  \
                 &_            the line before this one (the last that ran), as &N: &_ | count, &_#, &_!\n  \
                 &-N           the line N back: &-1 is &_, &-2 the one before it\n  \
                 def &x: …;    add a named fragment to the session\n  \
                 :mount SPEC   add a source (PATH or NAME=TARGET) to the session\n  \
                 mount NAME: ?OPTIONS  read a mounted source again under options: mount grain: ?null=NA&decimal=comma (one source: mount: ?…)\n  \
                 :follow [on|off|N]  acquisition rounds per line for ::href--> refs (interactive default: on)\n  \
                 :plan QUERY   price a query before it runs: its static plan as kaiv (--budget sets the bound)\n  \
                 man [NAME]    the manual: every command, one command's page, a glyph's (man <=>, man @|), man syntax [KIND] for the glyphs; --ru for Russian (also :man)\n  \
                 :history      show the fragment table (&1, &2, …)\n  \
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
