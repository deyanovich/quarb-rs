//! The ambient of a run, below everything that computes: the
//! refusal channel (a function that cannot answer says why, once,
//! and the run reports it), and the little the value layer needs of
//! the run's options, the collation and the sentence bonds, beside
//! the instant and the bonds the tool pins for the session. The pipeline opens an ambient for every run it
//! begins; the last one closed keeps its refusal until it is taken.
//! Thread-local, as a run is.
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
    pub(crate) refusal: Option<String>,
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
        if let Some(done) = AMBIENTS.with(|s| s.borrow_mut().pop()) {
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
    with_ambient(|a| {
        if a.refusal.is_none() {
            a.refusal = Some(msg);
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
