//! The attached regulae (spec: Attached Regulae): values a push
//! attaches to the node the thread stands at, read by every thread
//! through the node's identity. Part of the executor: see
//! `exec/mod.rs`.
//!
//! The store is the run's: it lives in [`RunState`] as long as the
//! query does and is never written to the data. Reads see the
//! attachments as the previous command left them (the snapshot
//! rule): a command's attachments gather in `pending` and commit
//! when the command ends, so a command that both reads `<-link:.v`
//! and attaches `.:v` is one synchronous round.

use crate::pipeline::*;
use std::any::Any;
use std::hash::Hash;

/// One attached regula: the name, the value, and the origins of
/// its reading.
#[derive(Clone)]
pub(crate) struct Attached {
    pub(crate) name: String,
    pub(crate) value: Value,
    pub(crate) prov: Prov,
}

/// The attachments of one run, keyed by the node's identity in the
/// layer above (`Attachment::Key`).
pub(crate) struct Attachments<K> {
    /// What the commands before the one in progress attached, by
    /// node, each node's regulae in first-attach order (an
    /// attaching push under a name already attached replaces the
    /// value in place, as a repoint keeps its place in the row).
    committed: HashMap<K, Vec<Attached>>,
    /// What the command in progress attached so far, in order.
    pending: Vec<(K, Attached)>,
}

impl<K> Default for Attachments<K> {
    fn default() -> Self {
        Attachments {
            committed: HashMap::new(),
            pending: Vec::new(),
        }
    }
}

/// Run `f` over the run's attachments keyed by `K`; a run whose
/// store belongs to another key type (never the case: one layer
/// per run) sees an empty store.
fn with_attachments<K: Hash + Eq + Clone + 'static, R>(
    f: impl FnOnce(&mut Attachments<K>) -> R,
) -> R {
    with_run(|run| {
        let slot: &mut Box<dyn Any> = run
            .attached
            .get_or_insert_with(|| Box::new(Attachments::<K>::default()));
        match slot.downcast_mut::<Attachments<K>>() {
            Some(a) => f(a),
            None => f(&mut Attachments::<K>::default()),
        }
    })
}

/// `X:.name` — the regula attached to the node under the name, as
/// the previous command left it; none where nothing is attached.
pub(crate) fn attached<K: Hash + Eq + Clone + 'static>(
    key: &K,
    name: &str,
) -> Option<(Value, Prov)> {
    with_attachments::<K, _>(|a| {
        a.committed
            .get(key)?
            .iter()
            .find(|r| r.name == name)
            .map(|r| (r.value.clone(), r.prov.clone()))
    })
}

/// `X:%.` — everything attached to the node, in first-attach
/// order; empty where nothing is.
pub(crate) fn attached_all<K: Hash + Eq + Clone + 'static>(key: &K) -> Vec<Attached> {
    with_attachments::<K, _>(|a| a.committed.get(key).cloned().unwrap_or_default())
}

/// The attached regulae of a node as `(name, value)` pairs: what a
/// round of a quantified push may change at the node, beside the
/// capsa.
pub(crate) fn attached_fields<K: Hash + Eq + Clone + 'static>(key: &K) -> Vec<(String, Value)> {
    attached_all(key)
        .into_iter()
        .map(|r| (r.name, r.value))
        .collect()
}

/// Whether the run has attached anything at all (so a nodes table
/// knows whether to look).
pub(crate) fn any_attached<K: Hash + Eq + Clone + 'static>() -> bool {
    with_attachments::<K, _>(|a| !a.committed.is_empty())
}

/// `.:name(body)` — one thread's attachment to the node it stands
/// at, held until the command commits.
pub(crate) fn attach<K: Hash + Eq + Clone + 'static>(key: K, regula: Attached) {
    with_attachments::<K, _>(|a| a.pending.push((key, regula)));
}

/// The command's end: its attachments become the ones the next
/// command reads. Threads standing at one node that attached under
/// one name must agree (spec: Attached Regulae, "Agreement"):
/// differing values refuse, naming `@| collapse`, and the command
/// attaches nothing. Answers whether any node's attachments
/// changed — what a quantified push's fixpoint asks.
pub(crate) fn commit<K: Hash + Eq + Clone + 'static>() -> std::result::Result<bool, String> {
    with_attachments::<K, _>(|a| {
        let pending = std::mem::take(&mut a.pending);
        // agreement first, before anything lands
        let mut seen: HashMap<(&K, &str), &Value> = HashMap::new();
        for (key, r) in &pending {
            match seen.get(&(key, r.name.as_str())) {
                Some(v) if **v != r.value => {
                    return Err(format!(
                        "threads standing at one node attach differing values under '{}' ({} and {}): \
                         '@| collapse' first, so that one thread stands at each node and attaches the gathered value",
                        r.name,
                        v.display_form(),
                        r.value.display_form()
                    ));
                }
                Some(_) => {}
                None => {
                    seen.insert((key, r.name.as_str()), &r.value);
                }
            }
        }
        let mut changed = false;
        for (key, r) in pending {
            let at = a.committed.entry(key).or_default();
            match at.iter_mut().find(|x| x.name == r.name) {
                Some(x) => {
                    if x.value != r.value {
                        changed = true;
                    }
                    *x = r;
                }
                None => {
                    changed = true;
                    at.push(r);
                }
            }
        }
        Ok(changed)
    })
}
