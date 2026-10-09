//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// The state of one run — what used to live in a thread-local
/// each: the first refusal, the enclosing topics of the value
/// capsae being navigated, the unresolved external references, the
/// provenance read frames and the stage counter, the scope memo,
/// the last link lists, the children memo. One per run, on a stack:
/// a query evaluated from inside another (an adapter that answers
/// through the engine, as the model's tree identities do) gets its
/// own state and gives the outer run's back on return, untouched.
/// What a run is given: the options a front-end decides per run
/// (`qua`'s flags, a session's settings), read by the executor from
/// the run in progress — never from an adapter method or a global.
/// Public as `quarb::Run`.
#[derive(Clone, Default)]
pub struct RunOptions {
    /// The invocation instant `now()` denotes, bound by the runner
    /// before evaluation (never a clock read during it); `None` when
    /// the runner bound none, and `now()` is null.
    pub now: Option<(i64, u32)>,
    /// Whether the `sh(...)` stage and backtick commands may run
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
    pub ref_targets: std::collections::HashMap<String, NodeId>,
    /// The collation `sort` uses when the query names no locale
    /// (the tool's output locale).
    pub collation: Option<String>,
    /// The session's sentence bonds (a `.desm` set), for
    /// `sentences` and `sc`.
    pub bonds: Option<std::rc::Rc<dyn crate::SentenceBonds>>,
    /// A bound the static plan is held to before the run
    /// ([`crate::plan::Budget`]): a query whose estimate exceeds it
    /// is refused without running.
    pub budget: Option<crate::plan::Budget>,
    /// Report the static plan in the [`crate::Outcome`] (always
    /// reported when a budget is set).
    pub plan: bool,
}

#[derive(Default)]
pub(super) struct RunState {
    /// What this run was given.
    pub(super) options: RunOptions,
    /// The first execution refusal recorded during the run.
    /// Evaluation is single-threaded per query and has no error
    /// channel below the entry points; this carries a refusal
    /// (e.g. a quantifier walk cut off at the bound) out to
    /// [`eval`]'s callers, which convert it to an error. First
    /// write wins.
    pub(super) refusal: Option<String>,
    /// The topics of the value capsas whose path operands are
    /// being navigated, innermost last: what `$_` answers inside
    /// such a path's predicates, where the predicate scope has no
    /// topic of its own.
    pub(super) enclosing_topic: Vec<Value>,
    /// The unresolved external references of the run: identifiers
    /// (a URL, for html) of documents queries referenced but could
    /// not reach because they are not mounted. The engine performs
    /// no IO — the host may acquire these, remount, and re-run.
    pub(super) refs: std::collections::BTreeSet<String>,
    /// The read frames: while a stage evaluates a value, the
    /// innermost frame collects the origins of every node read.
    /// No frame, no cost — navigation predicates and filters
    /// record nothing.
    pub(super) reads: Vec<Origins>,
    /// The 1-based ordinal of the top-level pipeline stage in
    /// progress (0 during navigation and at the branch level);
    /// the second coordinate of an origin.
    pub(super) stage: u32,
    pub(super) scopes: HashMap<(usize, String), std::rc::Rc<ScopeMemo>>,
    pub(super) last_out: LastLinks,
    pub(super) last_in: LastLinks,
    /// The children of the parents the sibling hops and `:::index`
    /// asked about: a sibling walk over n children of one parent is
    /// n lookups into one list, not n fresh lists.
    pub(super) children: HashMap<NodeId, std::rc::Rc<Vec<NodeId>>>,
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
pub(super) fn with_run<R>(f: impl FnOnce(&mut RunState) -> R) -> R {
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
pub(crate) struct RunGuard;

/// Begin a run with `options`, or — for a run begun from inside
/// another (an adapter answering through the engine, a macro body
/// at expansion time) — with the enclosing run's.
pub(crate) fn begin_run(options: Option<RunOptions>) -> RunGuard {
    RUNS.with(|runs| {
        let mut runs = runs.borrow_mut();
        let options = options
            .or_else(|| runs.last().map(|r| r.options.clone()))
            .unwrap_or_default();
        runs.push(RunState {
            options,
            ..RunState::default()
        });
    });
    RunGuard
}

/// Whether a run is in progress on this thread.
pub(crate) fn in_run() -> bool {
    RUNS.with(|runs| !runs.borrow().is_empty())
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
pub(super) fn with_enclosing_topic<R>(scope: &Scope<'_>, f: impl FnOnce() -> R) -> R {
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
pub(super) fn capture<T>(f: impl FnOnce() -> T) -> (T, Origins) {
    with_run(|s| s.reads.push(Origins::default()));
    let out = f();
    let origins = with_run(|s| s.reads.pop()).unwrap_or_default();
    (out, origins)
}

/// Run `f` under a frame that is dropped: a predicate's reads are
/// tests, not origins of the value being computed.
pub(super) fn discard<T>(f: impl FnOnce() -> T) -> T {
    with_run(|s| s.reads.push(Origins::default()));
    let out = f();
    with_run(|s| s.reads.pop());
    out
}

/// Record a node read into the innermost frame, if any.
pub(super) fn note(node: NodeId) {
    with_run(|s| {
        let stage = s.stage;
        if let Some(top) = s.reads.last_mut() {
            top.insert(Origin { node, stage });
        }
    });
}

/// Record a value's origins into the innermost frame, if any.
pub(super) fn note_all(o: &Origins) {
    with_run(|s| {
        if let Some(top) = s.reads.last_mut() {
            top.union(o);
        }
    });
}

pub(super) fn stage_now() -> u32 {
    with_run(|s| s.stage)
}

pub(super) fn record_ref(id: String) {
    with_run(|s| {
        s.refs.insert(id);
    });
}

pub(super) fn ref_target(id: &str) -> Option<NodeId> {
    run_option(|o| o.ref_targets.get(id).copied())
}

/// Drain the unresolved external references recorded since the
/// last clear: the identifiers of documents queries referenced
/// but could not reach.
pub(crate) fn take_refs() -> Vec<String> {
    FINISHED.with(|f| {
        std::mem::take(&mut f.borrow_mut().refs)
            .into_iter()
            .collect()
    })
}

/// Record a refusal from inside an evaluation — the engine's own
/// bounds and an adapter's failed fetch alike (a fetch that fails
/// mid-query must surface as an error, never as an empty answer):
/// the entry points turn the first one into [`QuarbError::refused`]
/// and treat the result as void. Outside an evaluation it is
/// cleared by the next one.
pub fn record_refusal(msg: String) {
    with_run(|s| {
        if s.refusal.is_none() {
            s.refusal = Some(msg);
        }
    });
}

/// A fetch's outcome inside an evaluation: its value, or — when
/// the read failed — a refusal recorded under `what` and the
/// type's default, so a failed read never answers as an empty
/// one. For a read whose caller caches its answer, match the
/// result instead and return before caching.
pub fn fetched<T: Default, E: std::fmt::Display>(
    result: Result<T, E>,
    what: impl FnOnce() -> String,
) -> T {
    match result {
        Ok(v) => v,
        Err(e) => {
            record_refusal(format!("{}: {e}", what()));
            T::default()
        }
    }
}

/// [`fetched`] for a read answered as an `Option`: `None` after
/// the refusal is recorded.
pub fn fetched_opt<T, E: std::fmt::Display>(
    result: Result<T, E>,
    what: impl FnOnce() -> String,
) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            record_refusal(format!("{}: {e}", what()));
            None
        }
    }
}

/// Take the refusal recorded during the last [`eval`] /
/// [`eval_traced`], if any. The caller that observes it must treat
/// the accompanying result as void.
pub fn take_refusal() -> Option<String> {
    FINISHED.with(|f| f.borrow_mut().refusal.take())
}

/// Evaluate `query` from the root.
/// Refuse a query that uses the `sh(...)` stage unless the adapter
/// allows it (`qua --allow-shell`). Query text stays inert data by
/// default — a `.quarb` file, a defs file, or a macro can never run
/// a command without the explicit per-run opt-in.
/// The quantifier bound in force: the run's, else the adapter's.
pub(crate) fn quantifier_bound(adapter: &impl AstAdapter) -> usize {
    run_option(|o| o.quantifier_bound).unwrap_or_else(|| adapter.quantifier_bound())
}

pub(crate) fn gate_shell(query: &Query, allow_shell: bool) -> crate::Result<()> {
    if uses_shell_query(query) && !allow_shell {
        return Err(crate::QuarbError::unsupported(
            "the sh(...) stage runs external commands; pass --allow-shell",
        ));
    }
    Ok(())
}

/// Whether the query names the shell anywhere a stage can stand
/// (`sh(...)` or a backtick command), down to record fields, list
/// items, interpolation holes and macro-expanded bodies.
pub(super) fn uses_shell_query(q: &Query) -> bool {
    struct Shell(bool);
    impl visit::Visitor for Shell {
        fn stage(&mut self, s: &Stage) {
            if matches!(s, Stage::Func(c) | Stage::Agg(c) if c.name == "sh") {
                self.0 = true;
            }
            visit::walk_stage(self, s);
        }
    }
    let mut v = Shell(false);
    v.query(q);
    v.0
}
