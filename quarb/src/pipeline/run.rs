//! Part of the executor: see `exec/mod.rs`.

use crate::pipeline::*;
pub use crate::value::ambient::record_refusal;
#[cfg(test)]
pub(crate) use crate::value::ambient::take_refusal;

/// The state of one run — what used to live in a thread-local
/// each: the first refusal, the enclosing topics of the value
/// capsae being navigated, the unresolved external references, the
/// provenance read frames and the command counter, the scope memo,
/// the last link lists, the children memo. One per run, on a stack:
/// a query evaluated from inside another (an adapter that answers
/// through the engine, as the model's tree identities do) gets its
/// own state and gives the outer run's back on return, untouched.
/// What a run is given: the options a front-end decides per run
/// (`qua`'s flags, a session's settings), read by the executor from
/// the run in progress — never from an adapter method or a global.
/// Public as `quarb::Run`.
/// The session memory (spec: Capsa Operations, "Session memory",
/// draft 166): a value store that outlives the query, per session —
/// the quai kernel, the qua REPL, a `.quai` read cell by cell; for a
/// one-shot run the run itself. `| ,name` files the topic under the
/// name, `$,name` reads it wherever a register stands, `%,` is the
/// whole memory as a record. The host owns it and hands it to each
/// run.
pub type SessionMemory = std::rc::Rc<std::cell::RefCell<SessionStore>>;

/// How a session prints its results (spec: Session memory, "Session
/// settings", draft 166): `off` as written, `structures` lists,
/// records and tables through `pretty`, `always` every result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrettyMode {
    #[default]
    Off,
    Structures,
    Always,
}

impl PrettyMode {
    pub fn word(self) -> &'static str {
        match self {
            PrettyMode::Off => "off",
            PrettyMode::Structures => "structures",
            PrettyMode::Always => "always",
        }
    }
    pub fn parse(word: &str) -> Option<PrettyMode> {
        match word {
            "off" => Some(PrettyMode::Off),
            "structures" => Some(PrettyMode::Structures),
            "always" => Some(PrettyMode::Always),
            _ => None,
        }
    }
    /// Whether a result of this kind prints through `pretty`.
    pub fn applies(self, v: &Value) -> bool {
        match self {
            PrettyMode::Off => false,
            PrettyMode::Always => true,
            PrettyMode::Structures => {
                matches!(v, Value::List(_) | Value::Record(_) | Value::Table(_))
            }
        }
    }
}

/// The session's store: the filed values and the settings the
/// `session` command sets (draft 166).
#[derive(Clone, Debug)]
pub struct SessionStore {
    pub memory: std::collections::BTreeMap<String, Value>,
    pub pretty: PrettyMode,
    /// `pretty`'s digits after the point for the session.
    pub precision: usize,
}

impl Default for SessionStore {
    fn default() -> Self {
        SessionStore {
            memory: Default::default(),
            pretty: PrettyMode::Off,
            precision: 6,
        }
    }
}

impl SessionStore {
    /// The settings as the record `session` alone answers.
    pub fn settings(&self) -> Value {
        Value::record(vec![
            (
                "pretty".to_string(),
                Value::Str(self.pretty.word().to_string()),
            ),
            ("precision".to_string(), Value::Int(self.precision as i64)),
        ])
    }
}

/// A fresh, empty session memory with the opening settings.
pub fn session_memory() -> SessionMemory {
    std::rc::Rc::new(std::cell::RefCell::new(SessionStore::default()))
}

#[derive(Clone, Default)]
pub struct RunOptions {
    /// The session memory the run reads and files into, if the host
    /// keeps one; without it a session read refuses.
    pub session: Option<SessionMemory>,
    /// The invocation instant `now()` denotes, bound by the runner
    /// before evaluation (never a clock read during it); `None` when
    /// the runner bound none, and `now()` is null.
    pub now: Option<(i64, u32)>,
    /// Whether the `sh(...)` command and backtick commands may run
    /// (`qua --allow-shell`). Off: query text stays inert data.
    pub allow_shell: bool,
    /// The bound on open path quantifiers and iterations
    /// (`qua --quantifier-bound`); `None` leaves the adapter's own
    /// ([`AstAdapter::quantifier_bound`], 32 unless its structures
    /// run deeper).
    pub quantifier_bound: Option<usize>,
    /// `--reproducible`: hold back the machine-dependent instant
    /// rungs of provenance (a source's mtime, the moment of
    /// reading) so the same source answers the same provenance run
    /// after run.
    pub reproducible: bool,
    /// Mounted-document roots by external-reference identifier
    /// (fragment-stripped), consulted when a property references
    /// another document (`::href-->`).
    /// The collation `sort` uses when the query names no locale
    /// (the tool's output locale).
    pub collation: Option<String>,
    /// The session's sentence bonds (a `.desm` set), for
    /// `sentences` and `sc`.
    pub bonds: Option<std::rc::Rc<dyn crate::value::ambient::SentenceBonds>>,
    /// A bound the static plan is held to before the run
    /// ([`crate::graph::plan::Budget`]): a query whose estimate exceeds it
    /// is refused without running.
    /// Report the static plan in the [`crate::Outcome`] (always
    /// reported when a budget is set).
    /// The topic the expression head's thread opens with: what a
    /// calculator front-end read from its pipe (`qc`'s standard
    /// input, as one text). `None` leaves the head's topic null.
    pub topic: Option<Value>,
    /// The environments the host mounted (`go=exec:~/go/bin`,
    /// `sh=shell:`): the namespaces `ALIAS:name(…)` reaches. The
    /// mount is the opt-in, so this comes from the host only.
    pub environments: Vec<crate::pipeline::env::Environment>,
    /// The command libraries the host linked (`morph:`), reached by
    /// their declared prefix with no mount; the standard library is
    /// the prefix-less one and is not listed.
    pub libraries: Vec<std::sync::Arc<dyn crate::pipeline::library::CommandLibrary>>,
}

impl RunOptions {
    /// Whether `sh(...)` may run: `--allow-shell`, or a `shell:`
    /// environment mounted (the flag is that mount's older name).
    pub fn shell_allowed(&self) -> bool {
        self.allow_shell
            || self
                .environments
                .iter()
                .any(|e| e.kind == crate::pipeline::env::EnvKind::Shell)
    }

    /// The mounted environment under `alias`.
    pub fn environment(&self, alias: &str) -> Option<&crate::pipeline::env::Environment> {
        self.environments.iter().find(|e| e.alias == alias)
    }

    /// The linked command library declared under `prefix`.
    pub fn library(&self, prefix: &str) -> Option<&dyn crate::pipeline::library::CommandLibrary> {
        self.libraries
            .iter()
            .find(|l| l.prefix() == prefix)
            .map(|l| l.as_ref())
    }
}

#[derive(Default)]
pub(crate) struct RunState {
    /// What this run was given.
    pub(crate) options: RunOptions,
    /// The first execution refusal recorded during the run.
    /// Evaluation is single-threaded per query and has no error
    /// channel below the entry points; this carries a refusal
    /// (e.g. a quantifier walk cut off at the bound) out to
    /// [`eval`]'s callers, which convert it to an error. First
    /// write wins.
    /// The topics of the value capsas whose path operands are
    /// being navigated, innermost last: what `$_` answers inside
    /// such a path's predicates, where the predicate scope has no
    /// topic of its own.
    pub(crate) enclosing_topic: Vec<Value>,
    /// The unresolved external references of the run: identifiers
    /// (a URL, for html) of documents queries referenced but could
    /// not reach because they are not mounted. The engine performs
    /// no IO — the host may acquire these, remount, and re-run.
    pub(crate) refs: std::collections::BTreeSet<String>,
    /// The read frames: while a command evaluates a value, the
    /// innermost frame collects the origins of every node read.
    /// No frame, no cost — navigation predicates and filters
    /// record nothing.
    pub(crate) reads: Vec<Origins>,
    /// The 1-based ordinal of the top-level pipeline command in
    /// progress (0 during navigation and at the branch level);
    /// the second coordinate of an origin.
    pub(crate) command: u32,
    /// The HTML form of the run's drawing (`@| tree`,
    /// `@| histogram`), beside the text it answers: a browser host
    /// renders it where the terminal prints the text. The last
    /// drawing wins.
    pub(crate) drawing_html: Option<String>,
}

thread_local! {
    /// The runs in progress, innermost last.
    static RUNS: std::cell::RefCell<Vec<RunState>> = const { std::cell::RefCell::new(Vec::new()) };
    /// What the last finished run left for its caller: the refusal
    /// `take_refusal` reports and the references `take_refs` lists.
    /// Also the scratch state of anything that records outside a
    /// run (an adapter refusing at open time), which the next run
    /// does not inherit.
    static FINISHED: std::cell::RefCell<RunState> = std::cell::RefCell::new(RunState::default());
}

/// Read or write the run in progress (the innermost), or the
/// finished-run slot when nothing is running.
pub(crate) fn with_run<R>(f: impl FnOnce(&mut RunState) -> R) -> R {
    RUNS.with(|runs| {
        let mut runs = runs.borrow_mut();
        match runs.last_mut() {
            Some(top) => f(top),
            None => FINISHED.with(|fin| f(&mut fin.borrow_mut())),
        }
    })
}

/// A run's lifetime: pushed on entry, popped into the finished
/// slot on exit — by `Drop`, so a panic unwinding through the
/// executor leaves no stale frame under the next run.
pub(crate) struct RunGuard {
    _ambient: crate::value::ambient::AmbientGuard,
}

/// Begin a run with `options`, or — for a run begun from inside
/// another (an adapter answering through the engine, a macro body
/// at expansion time) — with the enclosing run's.
pub(crate) fn begin_run(options: Option<RunOptions>) -> RunGuard {
    RUNS.with(|runs| {
        let mut runs = runs.borrow_mut();
        let options = options
            .or_else(|| runs.last().map(|r| r.options.clone()))
            .unwrap_or_default();
        let ambient = crate::value::ambient::Ambient {
            collation: options.collation.clone(),
            bonds: options.bonds.clone(),
            refusal: None,
        };
        runs.push(RunState {
            options,
            ..RunState::default()
        });
        RunGuard {
            _ambient: crate::value::ambient::enter(ambient),
        }
    })
}

/// Read the options of the run in progress (of the last finished
/// run, between runs — a tool resolving provenance after the run
/// reads the options that run had).
pub(crate) fn run_option<R>(f: impl FnOnce(&RunOptions) -> R) -> R {
    with_run(|s| f(&s.options))
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(done) = RUNS.with(|runs| runs.borrow_mut().pop()) {
            FINISHED.with(|fin| *fin.borrow_mut() = done);
        }
    }
}

/// Run `f` with `scope`'s topic (if any) reachable as `$_` inside
/// the predicates of the paths it navigates (see `Operand::Topic`).
pub(crate) fn with_enclosing_topic<X: Attachment, R>(
    scope: &Scope<'_, X>,
    f: impl FnOnce() -> R,
) -> R {
    let carried = scope.topic.is_some();
    if let Some(t) = scope.topic {
        with_run(|s| s.enclosing_topic.push(t.clone()));
    }
    let out = f();
    if carried {
        with_run(|s| {
            s.enclosing_topic.pop();
        });
    }
    out
}

/// Run `f` under a fresh read frame and return what it read.
pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, Origins) {
    with_run(|s| s.reads.push(Origins::default()));
    let out = f();
    let origins = with_run(|s| s.reads.pop()).unwrap_or_default();
    (out, origins)
}

/// Run `f` under a frame that is dropped: a predicate's reads are
/// tests, not origins of the value being computed.
pub(crate) fn discard<T>(f: impl FnOnce() -> T) -> T {
    with_run(|s| s.reads.push(Origins::default()));
    let out = f();
    with_run(|s| s.reads.pop());
    out
}

/// Record a node read into the innermost frame, if any.
pub(crate) fn note(position: Position) {
    with_run(|s| {
        let command = s.command;
        if let Some(top) = s.reads.last_mut() {
            top.insert(Origin {
                position,
                command: command,
            });
        }
    });
}

/// Record a value's origins into the innermost frame, if any.
pub(crate) fn note_all(o: &Origins) {
    with_run(|s| {
        if let Some(top) = s.reads.last_mut() {
            top.union(o);
        }
    });
}

pub(crate) fn command_now() -> u32 {
    with_run(|s| s.command)
}

pub(crate) fn record_ref(id: String) {
    with_run(|s| {
        s.refs.insert(id);
    });
}

/// Drain the unresolved external references recorded since the
/// last clear: the identifiers of documents queries referenced
/// but could not reach.
/// Record the HTML form of a drawing the run produced.
pub(crate) fn set_drawing_html(html: String) {
    with_run(|s| s.drawing_html = Some(html));
}

/// The HTML form of the last finished run's drawing, taken once.
pub(crate) fn take_drawing_html() -> Option<String> {
    FINISHED.with(|f| f.borrow_mut().drawing_html.take())
}

pub(crate) fn take_refs() -> Vec<String> {
    FINISHED.with(|f| {
        std::mem::take(&mut f.borrow_mut().refs)
            .into_iter()
            .collect()
    })
}

pub(crate) fn gate(query: &Query, run: &RunOptions) -> crate::Result<()> {
    if uses_shell_query(query) && !run.shell_allowed() {
        return Err(crate::QuarbError::unsupported(
            "the sh(...) command runs external commands; pass --allow-shell",
        ));
    }
    // Every prefix a command carries names a linked library or a
    // mounted environment. A library's commands are checked against
    // their synopsis here, before the run, and the library's
    // run-time requirement once; an environment must be mounted,
    // the mount being the opt-in granted from the host's side.
    let mut checked: Vec<String> = Vec::new();
    for (call, agg) in prefixed_calls(query) {
        let (prefix, name) = crate::pipeline::env::split_env(&call.name).expect("prefixed");
        if let Some(lib) = run.library(prefix) {
            if !checked.iter().any(|p| p == prefix) {
                lib.available().map_err(|why| {
                    crate::QuarbError::unsupported(format!(
                        "library '{prefix}' is not usable here: {why}"
                    ))
                })?;
                checked.push(prefix.to_string());
            }
            crate::pipeline::library::check(lib, name, &call, agg)
                .map_err(crate::QuarbError::unsupported)?;
        } else if run.environment(prefix).is_none() {
            // Not a linked library either: the same refusal names
            // the mount, which is the opt-in a user can grant; a
            // library's absence is the build's.
            return Err(crate::QuarbError::unsupported(format!(
                "environment '{prefix}' is not mounted; mount it from the command line \
                 ('{prefix}=exec:DIR', '{prefix}=shell:')"
            )));
        }
    }
    Ok(())
}

/// Every prefixed command of the query (`lib:name(…)`), with whether
/// it stands after the aggregation pipe.
pub(crate) fn prefixed_calls(q: &Query) -> Vec<(FnCall, bool)> {
    struct Calls(Vec<(FnCall, bool)>);
    impl visit::Visitor for Calls {
        fn command(&mut self, s: &Command) {
            match s {
                Command::Func(c) if crate::pipeline::env::split_env(&c.name).is_some() => {
                    self.0.push((c.clone(), false))
                }
                Command::Agg(c) if crate::pipeline::env::split_env(&c.name).is_some() => {
                    self.0.push((c.clone(), true))
                }
                _ => {}
            }
            visit::walk_command(self, s);
        }
    }
    let mut v = Calls(Vec::new());
    v.query(q);
    v.0
}

/// Whether the query names the shell anywhere a command can stand
/// (`sh(...)` or a backtick command), down to record fields, list
/// items, interpolation holes and macro-expanded bodies.
pub(crate) fn uses_shell_query(q: &Query) -> bool {
    struct Shell(bool);
    impl visit::Visitor for Shell {
        fn command(&mut self, s: &Command) {
            if matches!(s, Command::Func(c) | Command::Agg(c) if c.name == "sh") {
                self.0 = true;
            }
            visit::walk_command(self, s);
        }
    }
    let mut v = Shell(false);
    v.query(q);
    v.0
}
