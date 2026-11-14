//! The ambient of a run, below everything that computes: the
//! refusal channel (a function that cannot answer says why, once,
//! and the run reports it), and the little the value layer needs of
//! the run's options, the collation and the sentence bonds, beside
//! the instant and the bonds the tool pins for the session. The pipeline opens an ambient for every run it
//! begins; the last one closed keeps its refusal until it is taken.
//! Thread-local, as a run is.
use crate::diagnostic::Diagnostic;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Sentence segmentation with the bonds of a `.desm` file applied:
/// the abbreviations a sentence may end in without ending.
pub trait SentenceBonds {
    fn unicode_sentences<'a>(&self, text: &'a str) -> Vec<&'a str>;
    fn as_any(&self) -> &dyn std::any::Any;
}

#[cfg(feature = "desm")]
impl SentenceBonds for syndesmos::Syndesmos {
    fn unicode_sentences<'a>(&self, text: &'a str) -> Vec<&'a str> {
        syndesmos::Syndesmos::unicode_sentences(self, text)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

thread_local! {
    static NOW: Cell<Option<(i64, u32)>> = const { Cell::new(None) };
    static SENTENCE_BONDS: RefCell<Option<Rc<dyn SentenceBonds>>> = const { RefCell::new(None) };
    static AMBIENTS: RefCell<Vec<Ambient>> = const { RefCell::new(Vec::new()) };
    static FINISHED: RefCell<Ambient> = RefCell::new(Ambient::default());
    /// What sources had to tell when they were mounted, until a run
    /// ends and tells it ([`raise_at_mount`]).
    static AT_MOUNT: RefCell<Vec<Diagnostic>> = const { RefCell::new(Vec::new()) };
}

/// Raise a diagnostic outside a run: what a source has to tell when
/// it is mounted (a sheet of a workbook it could not read). No run
/// is open then to carry it, and one recorded on the last run's
/// record would be dropped by the next. A host takes these with
/// [`take_at_mount`] where it reports a run's diagnostics and tells
/// them in front of the run's own, once: the tools and the session
/// do; a parse or a run a host reports nothing of leaves them be.
pub fn raise_at_mount(d: Diagnostic) {
    AT_MOUNT.with(|m| m.borrow_mut().push(d));
}

/// Take what the mounts raised and no run has told yet.
pub fn take_at_mount() -> Vec<Diagnostic> {
    AT_MOUNT.with(|m| std::mem::take(&mut *m.borrow_mut()))
}

/// The invocation instant, pinned once by the tool layer (`qua
/// --now`, or the clock read once at startup) and read by adapters
/// that would otherwise consult the wall clock when resolving a
/// relative window (`since=30m`). A pinned run replays: evaluation
/// never reads a clock, and neither does a mount. `None` before the
/// tool sets it — a library embedder that never pins gets the old
/// wall-clock behavior.
pub fn set_invocation_instant(secs: i64, nanos: u32) {
    NOW.with(|c| c.set(Some((secs, nanos))));
}

pub fn invocation_instant() -> Option<(i64, u32)> {
    NOW.with(|c| c.get())
}

/// The pinned instant's seconds, or the clock's now when none is
/// pinned.
pub fn now_secs() -> i64 {
    match invocation_instant() {
        Some((secs, _)) => secs,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    }
}

/// The session's sentence bonds, set once by the tool layer.
/// The unit resolver of the run in progress: the mounted
/// document's own declared units beside the built-in table, so the
/// one quantity coercion (a typed column, a unital reading, a
/// comparison's criterion text) sees what `convert` sees.
type UnitScale = dyn Fn(&str) -> Option<(f64, String)>;

thread_local! {
    static UNIT_SCALE: RefCell<Option<*const UnitScale>> = const { RefCell::new(None) };
}

/// Run `f` with `scale` as the unit resolver. A scoped thread-local
/// in the manner of `scoped_tls`: the pointer is installed for the
/// duration of `f` and removed before `scale` can go out of scope,
/// so the lifetime erasure below is never observed.
pub(crate) fn with_unit_scale<R>(
    scale: &(dyn Fn(&str) -> Option<(f64, String)> + '_),
    f: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<*const UnitScale>);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNIT_SCALE.with(|s| *s.borrow_mut() = self.0);
        }
    }
    // SAFETY: the raw pointer outlives neither `scale` nor this
    // call — `Restore` puts the previous value back when `f`
    // returns or unwinds, and `unit_scale` only dereferences the
    // pointer while it is installed.
    let erased: *const UnitScale = unsafe {
        std::mem::transmute::<*const (dyn Fn(&str) -> Option<(f64, String)> + '_), *const UnitScale>(
            scale,
        )
    };
    let previous = UNIT_SCALE.with(|s| s.replace(Some(erased)));
    let _restore = Restore(previous);
    f()
}

/// The run's unit resolver's answer for `expr`: `None` when no run
/// installed one (the built-in table alone applies), `Some(answer)`
/// otherwise, the answer itself `None` for a unit nobody declared.
pub fn unit_scale(expr: &str) -> Option<Option<(f64, String)>> {
    let ptr = UNIT_SCALE.with(|s| *s.borrow());
    // SAFETY: a pointer is present only inside `with_unit_scale`,
    // whose resolver is alive for exactly that span.
    ptr.map(|p| unsafe { (*p)(expr) })
}

pub fn set_sentence_bonds(bonds: Option<Rc<dyn SentenceBonds>>) {
    SENTENCE_BONDS.with(|b| *b.borrow_mut() = bonds);
}

pub fn sentence_bonds() -> Option<Rc<dyn SentenceBonds>> {
    SENTENCE_BONDS.with(|b| b.borrow().clone())
}

/// What a run holds for the layers below the pipeline.
#[derive(Default)]
pub(crate) struct Ambient {
    pub(crate) collation: Option<String>,
    pub(crate) bonds: Option<Rc<dyn SentenceBonds>>,
    /// The first refusal, as the diagnostic it is (an error).
    pub(crate) refusal: Option<Diagnostic>,
    /// Whether withheld data was met this run: one warning at the
    /// end (spec: omitted data).
    pub(crate) omitted: bool,
    /// How deep the run stands in a predicate or another key
    /// evaluation: a withheld read there refuses.
    pub(crate) keying: u32,
    /// The warnings and infos raised, in raising order.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Whether the value in front of a command is a cell of a
    /// structure `map` runs its body on: a numeric command leaves a
    /// cell of a kind it does not take as it is, where a lone value
    /// of that kind answers null (ruling 2026-11-10). It holds for
    /// one run of commands ([`cells_ground`]).
    pub(crate) cells: bool,
    /// The mark `map` leaves for the run of commands it starts next
    /// ([`mark_cells`]), taken by that run.
    pub(crate) cells_next: bool,
    /// The lone values a numeric command could not take, per
    /// command: one warning each when the run ends, counting them.
    pub(crate) untaken: Vec<Untaken>,
    /// The names already put to a declared surface this run, by the
    /// container asked and the name (a label apart from a property):
    /// such a source is asked once per name, and told of once. A
    /// source that declares nothing is not recorded, its containers
    /// being as many as its nodes.
    pub(crate) surfaced: std::collections::HashMap<u64, Vec<(String, bool)>>,
    /// How deep the run stands in a test of presence (`[::name]`,
    /// `[!::name]`): a name a surface lacks is the answer there, and
    /// a mistake only when nothing asked has it.
    pub(crate) presence: u32,
    /// What the run's tests of presence met, by name, in the order
    /// first tested: the verdict falls when the run ends.
    pub(crate) tested: Vec<Tested>,
    /// The field names the run read on records, in the order first
    /// read: a name no record read under it has is told of when the
    /// run ends ([`read_field`]).
    pub(crate) fields: Vec<FieldRead>,
}

/// One field name the run read on records.
#[derive(Default)]
pub(crate) struct FieldRead {
    name: String,
    /// Whether a record read under the name has the field.
    found: bool,
    /// The fields of the first records that lack it, in the order
    /// met: what the warning names.
    there: Vec<String>,
    /// How many records gave their fields to `there`.
    sampled: u8,
}

impl FieldRead {
    /// The records whose fields a warning draws on, and the names it
    /// holds at most: a stream of a million records lacking the field
    /// is a counter after these.
    const SAMPLED: u8 = 32;
    const HELD: usize = 64;

    fn warning(&self) -> Diagnostic {
        let name = &self.name;
        Diagnostic::warning(
            crate::diagnostic::UNKNOWN_FIELD,
            format!(":{name}"),
            format!(
                "no record read here has the field '{name}'; the fields there: {}",
                crate::graph::steps::surface_names(&self.there, name)
            ),
        )
    }
}

/// One name the run tested for presence.
#[derive(Default)]
pub(crate) struct Tested {
    name: String,
    /// Whether anything asked has the property: a declared surface
    /// that lists it, or a node that read it.
    found: bool,
    /// The declared containers asked, each once.
    asked: Vec<u64>,
    /// The warnings kept for the containers that lack it.
    lacking: Vec<Diagnostic>,
}

impl Ambient {
    fn tested(&mut self, name: &str) -> &mut Tested {
        match self.tested.iter().position(|t| t.name == name) {
            Some(i) => &mut self.tested[i],
            None => {
                self.tested.push(Tested {
                    name: name.to_string(),
                    ..Tested::default()
                });
                self.tested.last_mut().expect("just pushed")
            }
        }
    }
}

/// The lone values of a kind one numeric command does not take.
pub(crate) struct Untaken {
    command: String,
    /// What the command takes: "a number", "a number or a quantity".
    takes: &'static str,
    count: usize,
    /// The first of them, kind and value, as the warning shows it.
    first: String,
}

impl Untaken {
    fn warning(&self) -> Diagnostic {
        let Untaken {
            command,
            takes,
            count,
            first,
        } = self;
        Diagnostic::warning(
            crate::diagnostic::NOT_NUMERIC,
            "pipeline",
            if *count == 1 {
                format!(
                    "{command} takes {takes}: 1 value is of another kind ({first}) and answers \
                     null"
                )
            } else {
                format!(
                    "{command} takes {takes}: {count} values are of another kind (the first: \
                     {first}) and answer null"
                )
            },
        )
    }
}

/// Open an ambient for a run; it closes when the guard drops, and
/// its refusal then waits in the finished slot.
pub(crate) fn enter(a: Ambient) -> AmbientGuard {
    AMBIENTS.with(|s| s.borrow_mut().push(a));
    AmbientGuard
}

pub(crate) struct AmbientGuard;

impl Drop for AmbientGuard {
    fn drop(&mut self) {
        if let Some(mut done) = AMBIENTS.with(|s| s.borrow_mut().pop()) {
            // the one warning for every omission of the run, unless the
            // run already refused for the same reason
            if done.omitted {
                omitted_warning(&mut done);
            }
            // one warning per numeric command for the lone values it
            // could not take: one line for the stream, not one per
            // thread
            let untaken: Vec<Diagnostic> = done.untaken.iter().map(Untaken::warning).collect();
            done.diagnostics.extend(untaken);
            // the verdict of the tests of presence: a name nothing
            // asked has is told of, container by container; one that
            // something has was the question the test put
            for t in std::mem::take(&mut done.tested) {
                if t.found {
                    continue;
                }
                for d in t.lacking {
                    if !done
                        .diagnostics
                        .iter()
                        .any(|k| k.code == d.code && k.message == d.message)
                    {
                        done.diagnostics.push(d);
                    }
                }
            }
            // the verdict of the field reads: a name no record read
            // under it has, one warning each, after the answers; a
            // run that refused has its error to tell
            let fields = std::mem::take(&mut done.fields);
            if done.refusal.is_none() {
                let unknown: Vec<Diagnostic> = fields
                    .iter()
                    .filter(|f| !f.found)
                    .map(FieldRead::warning)
                    .collect();
                done.diagnostics.extend(unknown);
            }
            FINISHED.with(|f| *f.borrow_mut() = done);
        }
    }
}

fn with_ambient<R>(f: impl FnOnce(&mut Ambient) -> R) -> R {
    AMBIENTS.with(|s| {
        let mut s = s.borrow_mut();
        match s.last_mut() {
            Some(top) => f(top),
            None => FINISHED.with(|fin| f(&mut fin.borrow_mut())),
        }
    })
}

/// Read the current ambient.
pub(crate) fn ambient<R>(f: impl FnOnce(&Ambient) -> R) -> R {
    with_ambient(|a| f(a))
}

/// Record a refusal from inside an evaluation — the engine's own
/// bounds and an adapter's failed fetch alike (a fetch that fails
/// mid-query must surface as an error, never as an empty answer):
/// the entry points turn the first one into [`QuarbError::refused`]
/// and treat the result as void. Outside an evaluation it is
/// cleared by the next one.
pub fn record_refusal(msg: String) {
    record_refusal_coded(crate::diagnostic::REFUSED, "query", msg);
}

/// [`record_refusal`] under the code of the rule that declines the
/// query (`QUANTIFIER_BOUND`, `FRONTIER_BOUND`) and the site that
/// raised it (`round 17`). The first refusal of a run is the one
/// reported.
pub fn record_refusal_coded(code: &str, site: impl Into<String>, msg: String) {
    with_ambient(|a| {
        if a.refusal.is_none() {
            a.refusal = Some(Diagnostic::error(code, site, msg));
        }
    });
}

/// Raise a warning or an info beside the answer (spec:
/// Diagnostics): kept in raising order and handed back with the
/// run's outcome. An error goes through [`record_refusal_coded`].
/// Push the omission warning once, unless the run refused as OMITTED.
fn omitted_warning(a: &mut Ambient) {
    let refused = a
        .refusal
        .as_ref()
        .is_some_and(|r| r.code == crate::diagnostic::OMITTED);
    let warned = a.diagnostics.iter().any(|d| {
        d.code == crate::diagnostic::OMITTED && d.severity == crate::diagnostic::Severity::Warning
    });
    if !refused && !warned {
        a.diagnostics.push(Diagnostic::warning(
            crate::diagnostic::OMITTED,
            "source",
            crate::diagnostic::OMITTED_TEXT,
        ));
    }
}
/// A withheld property was read (spec: omitted data): the run warns
/// once at its end; inside a key evaluation it refuses now.
pub fn record_omitted() {
    with_ambient(|a| a.omitted = true);
    if with_ambient(|a| a.keying) > 0 {
        refuse_omitted();
    }
}
/// The source dropped whole nodes for the caller (spec: omitted
/// data): the warning, once, after the run.
pub fn record_omitted_nodes() {
    with_ambient(|a| {
        a.omitted = true;
        omitted_warning(a);
    });
}
/// Run `f` as a key evaluation: a predicate, a sort key, a group or
/// top key, where a withheld read refuses rather than reads absent.
pub fn with_keying<R>(f: impl FnOnce() -> R) -> R {
    with_ambient(|a| a.keying += 1);
    let out = f();
    with_ambient(|a| a.keying = a.keying.saturating_sub(1));
    out
}
/// Run `f` as a test of presence: a predicate that is a property
/// alone asks whether it is there, so a declared surface that lacks
/// the name is not told of at once. The warning is kept, and raised
/// when the run ends only if nothing the test asked has the name
/// (ruling 2026-11-11): `//*[::name]` over a database tells the
/// tables that have the column from those that lack it in silence,
/// and `/employees/*[::manger_id]` is still a misspelling.
pub(crate) fn with_presence<R>(f: impl FnOnce() -> R) -> R {
    with_ambient(|a| a.presence += 1);
    let out = f();
    with_ambient(|a| a.presence = a.presence.saturating_sub(1));
    out
}
/// Whether the run stands in a test of presence.
pub(crate) fn in_presence() -> bool {
    with_ambient(|a| a.presence > 0)
}
/// Whether a test of presence already put `name` to the declared
/// surface of `container`.
pub(crate) fn presence_asked(container: u64, name: &str) -> bool {
    with_ambient(|a| {
        a.tested
            .iter()
            .any(|t| t.name == name && t.asked.contains(&container))
    })
}
/// A test of presence put `name` to the declared surface of
/// `container`: it has the property, or the warning that it lacks
/// it is kept for the verdict.
pub(crate) fn presence_ask(container: u64, name: &str, lacking: Option<Diagnostic>) {
    with_ambient(|a| {
        let t = a.tested(name);
        t.asked.push(container);
        match lacking {
            Some(d) => t.lacking.push(d),
            None => t.found = true,
        }
    });
}
/// A test of presence read `name` on a node: something has it.
pub(crate) fn presence_found(name: &str) {
    with_ambient(|a| {
        // the common case first: the name already found
        match a.tested.iter_mut().find(|t| t.name == name) {
            Some(t) => t.found = true,
            None => a.tested(name).found = true,
        }
    });
}
/// The field of `record` under `name`, for a read the query wrote
/// (`:name` on a record, `$.r:name`, `r["name"]`): the answer is the
/// field or nothing, as ever, and the read is tallied. A name that no
/// record read under it has is a warning when the run ends, naming
/// the fields the records have (UNKNOWN_FIELD, ruling 2026-11-11: a
/// record's missing field is null, and no longer in silence); where
/// one record has it the others' nulls are the data's.
pub(crate) fn read_field<'a>(
    record: &'a crate::value::Record,
    name: &str,
) -> Option<&'a crate::value::Value> {
    let read = record.get(name);
    field_read(record, name, read.is_some());
    read
}
/// Tally one read of `name` on `record` for the run's verdict
/// ([`read_field`]): `found` is whether the record has the field (a
/// label lookup finds a key as a value, `r[[k]]`).
pub(crate) fn field_read(record: &crate::value::Record, name: &str, found: bool) {
    with_ambient(|a| {
        let i = match a.fields.iter().position(|f| f.name == name) {
            Some(i) => i,
            None => {
                a.fields.push(FieldRead {
                    name: name.to_string(),
                    ..FieldRead::default()
                });
                a.fields.len() - 1
            }
        };
        let f = &mut a.fields[i];
        if found {
            f.found = true;
        } else if !f.found && f.sampled < FieldRead::SAMPLED {
            f.sampled += 1;
            for (k, _) in record {
                if f.there.len() >= FieldRead::HELD {
                    break;
                }
                if !f.there.iter().any(|t| t == k) {
                    f.there.push(k.clone());
                }
            }
        }
    });
}
/// The refusal a key over omitted values raises.
pub fn refuse_omitted() {
    record_refusal_coded(
        crate::diagnostic::OMITTED,
        "source",
        crate::diagnostic::OMITTED_TEXT.to_string(),
    );
}
pub fn record_diagnostic(d: Diagnostic) {
    with_ambient(|a| a.diagnostics.push(d));
}

/// Mark the run of commands started next as standing on a cell of a
/// structure (`map`'s body on a cell of a table, an item of a list,
/// a field of a record) or, with `false`, on a lone value (its body
/// on a scalar topic).
pub(crate) fn mark_cells(cells: bool) {
    with_ambient(|a| a.cells_next = cells);
}

/// A nested pipeline's own ground: its commands stand on a cell
/// where `map` marked the run ([`mark_cells`]), on lone values
/// otherwise, whatever the enclosing run stands on, until the guard
/// drops. So the mark reaches the commands of `map`'s body and no
/// further: a path piped inside the body
/// (`map((^/books/* | ::title | sqrt @| first))`) runs its commands
/// on threads, each a lone value.
pub(crate) struct CellsGround(bool);

pub(crate) fn cells_ground() -> CellsGround {
    CellsGround(with_ambient(|a| {
        let here = std::mem::take(&mut a.cells_next);
        std::mem::replace(&mut a.cells, here)
    }))
}

impl Drop for CellsGround {
    fn drop(&mut self) {
        let before = self.0;
        with_ambient(|a| a.cells = before);
    }
}

/// Whether the value in front of a command is a cell
/// ([`cells_ground`]).
pub(crate) fn in_cells() -> bool {
    with_ambient(|a| a.cells)
}

/// A lone value the numeric command `command` could not take: the
/// answer is null, and the run warns once for the command when it
/// ends, counting them and showing the first.
pub(crate) fn record_untaken(command: &str, takes: &'static str, first: impl FnOnce() -> String) {
    with_ambient(
        |a| match a.untaken.iter_mut().find(|u| u.command == command) {
            Some(u) => u.count += 1,
            None => a.untaken.push(Untaken {
                command: command.to_string(),
                takes,
                count: 1,
                first: first(),
            }),
        },
    );
}

/// Whether `name` was already put to the declared surface of
/// `container` this run (`label` tells a label from a property). A
/// lookup and no more: every null read of a property passes here,
/// on any source, and one that declares nothing is never recorded.
pub(crate) fn surface_asked(container: u64, name: &str, label: bool) -> bool {
    with_ambient(|a| {
        a.surfaced
            .get(&container)
            .is_some_and(|seen| seen.iter().any(|(n, l)| n == name && *l == label))
    })
}

/// Record that `name` was put to the declared surface of
/// `container`: such a source is asked once per name, its containers
/// being few.
pub(crate) fn surface_ask(container: u64, name: &str, label: bool) {
    with_ambient(|a| {
        a.surfaced
            .entry(container)
            .or_default()
            .push((name.to_string(), label));
    });
}

/// Raise a warning beside the answer from an adapter, once per run
/// for one text: what a read could not answer the way it was asked,
/// where the run goes on (a refusal ends it: [`record_refusal`]).
pub fn record_warning(code: &str, site: &str, message: String) {
    record_warning_once(code, site, message);
}

/// Raise a warning beside the answer, once per run for one text.
pub(crate) fn record_warning_once(code: &str, site: &str, message: String) {
    with_ambient(|a| {
        if !a
            .diagnostics
            .iter()
            .any(|d| d.code == code && d.message == message)
        {
            a.diagnostics.push(Diagnostic::warning(code, site, message));
        }
    });
}

/// Raise an info beside the answer, once per run for one text: a
/// command that runs on every thread tells what it did once.
pub(crate) fn record_info_once(code: &str, site: &str, message: String) {
    with_ambient(|a| {
        if !a
            .diagnostics
            .iter()
            .any(|d| d.code == code && d.message == message)
        {
            a.diagnostics.push(Diagnostic::info(code, site, message));
        }
    });
}

/// Drop the diagnostics raised so far in the run that `drop` names:
/// a warning on dirt a later command cleaned (`| # | clean`) is no
/// longer a warning the answer should carry.
pub fn retract_diagnostics(drop: impl Fn(&Diagnostic) -> bool) {
    with_ambient(|a| a.diagnostics.retain(|d| !drop(d)));
}

/// Raise a debug diagnostic when the run asks for them
/// (`RunOptions::debug`); the message is built only then. Behind
/// the `debug-diagnostics` feature: a build without it raises none,
/// whatever the level.
#[cfg(feature = "debug-diagnostics")]
pub fn record_debug(code: &str, site: impl Into<String>, message: impl FnOnce() -> String) {
    if crate::pipeline::run_option(|o| o.debug) {
        record_diagnostic(Diagnostic::new(
            crate::diagnostic::Severity::Debug,
            code,
            site,
            message(),
        ));
    }
}

/// The debug diagnostics are not built in (the `debug-diagnostics`
/// feature is off): nothing is raised.
#[cfg(not(feature = "debug-diagnostics"))]
pub fn record_debug(_code: &str, _site: impl Into<String>, _message: impl FnOnce() -> String) {}

/// Take the warnings and infos the last run raised.
pub fn take_diagnostics() -> Vec<Diagnostic> {
    FINISHED.with(|f| std::mem::take(&mut f.borrow_mut().diagnostics))
}

/// Take the refusal recorded during the last run as the diagnostic
/// it is: code, site and message.
pub fn take_refusal_diagnostic() -> Option<Diagnostic> {
    FINISHED.with(|f| f.borrow_mut().refusal.take())
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

/// Whether a refusal stands recorded in this evaluation: a walk
/// that meets one mid-expansion (a cycle under the acyclic promise,
/// a negative weight under a budget) ends with nothing rather than
/// walking on with the paths it has left.
pub(crate) fn refusal_pending() -> bool {
    with_ambient(|a| a.refusal.is_some())
}

/// Take the refusal recorded during the last [`eval`] /
/// [`eval_traced`], if any. The caller that observes it must treat
/// the accompanying result as void.
pub fn take_refusal() -> Option<String> {
    take_refusal_diagnostic().map(|d| d.message)
}
