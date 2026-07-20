//! Metatheca vault adapter for the Quarb query engine.
//!
//! A vault is an arbor whose crosslink fabric is the state chain —
//! metatheca's append-only history. The root exposes four names:
//!
//! - `/paths/<dir>/<file>` — the path tree at the head state; leaves
//!   are entries.
//! - `/entries/<uuid>` — every entry at the head state, path-bound or
//!   not (retracted "ghosts" included; they carry `<orphan>`).
//! - `/states/<hash>` — the chain, newest first. Navigating by
//!   literal name accepts anything a metatheca stateref does — a
//!   full hash, a `>=8`-hex prefix, an ISO-8601 instant, `~N`,
//!   `current` — without enumerating. A state's children are
//!   `entries` and `paths`: the *same* tree as the root's, as of
//!   that state, giving time-travel reads at any coordinate.
//! - `/head` — an alias of the head state, same treatment.
//!
//! An entry's children are its *fact events* in chain order — the
//! entry's history is a walkable axis. `/paths/docs/a.md/*` is the
//! file's timeline: every `core/path` binding, every `core/blob-ref`
//! content change, every namespaced fact, each stamped (`::at`) with
//! the instant of its introducing state. An entry's properties are
//! its *current* facts at the node's coordinate: typed core
//! projections (`::mime`, `::size`, `::path`, `::mtime`, …) and the
//! generic `::'ns/name'` for any fact kind (quoted — kinds contain
//! `/`); a single-field body unwraps to a typed scalar, a
//! multi-field body reads as canonical JSON.
//!
//! History is inherent, no schema needed:
//!
//! ```text
//! /head(->previous)* @| count                     the chain walk
//! /states/'~1'/paths/docs/a.md::                  as-of content read
//! /head(->previous)*[/paths/img.png]? | ;;;short  when did it last exist
//! /paths/docs/a.md/*[::kind='core/path'] | ::path the renames, visible
//! /states/'~2'->added | ::kind                    what changed in a state
//! /states/'~2'/entries/*<changed>                 …as a diff surface
//! ```
//!
//! Everything is read through the `metatheca` crate — the persistent
//! SQLite index answers current-state questions; history questions
//! trigger one sweep of the chain (every fact blob, once), cached
//! for the adapter's lifetime. The adapter never writes; the head is
//! pinned at [`open`](MetathecaAdapter::open), so a query sees one
//! consistent coordinate system.

use metatheca::{parse_entry_id, Fact, Hash, State, Uuid, Vault};
use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

/// An error opening or reading a vault.
#[derive(Debug, thiserror::Error)]
pub enum MetathecaError {
    #[error("metatheca: {0}")]
    Vault(#[from] metatheca::Error),
}

/// What a node is.
#[derive(Clone)]
enum Kind {
    Root,
    /// `/states`.
    StatesDir,
    /// `/head`: an alias for the head state.
    Head,
    State {
        hash: Hash,
    },
    /// The `entries` dir at a state coordinate.
    EntriesDir {
        at: Hash,
    },
    /// The `paths` dir at a state coordinate.
    PathsDir {
        at: Hash,
    },
    /// An intermediate path-tree dir (`path` has no trailing `/`).
    Dir {
        at: Hash,
        path: String,
    },
    /// An entry at a state coordinate. `label` is the incoming edge
    /// name: a path segment under `paths`, the hyphenated UUID under
    /// `entries` (path leaves alias the canonical entry node — same
    /// answers, own tree position).
    Entry {
        at: Hash,
        id: Uuid,
        label: String,
    },
    /// One fact event: `seq` indexes the entry's timeline.
    FactEvent {
        entry: Uuid,
        seq: usize,
    },
}

struct Node {
    kind: Kind,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// The state chain, walked once (state blobs only, no fact reads).
struct Chain {
    /// `(hash, state)` newest-first; index 0 is the head.
    states: Vec<(Hash, State)>,
    pos: HashMap<Hash, usize>,
}

/// One fact event on an entry's timeline.
struct Ev {
    hash: Hash,
    /// Index into `Chain::states` (newest-first) of the introducing
    /// state.
    state_idx: usize,
    fact: Fact,
}

/// The history sweep: every fact blob read once, in chain order.
struct Sweep {
    /// Per-entry fact events, oldest-first.
    timelines: HashMap<Uuid, Vec<Ev>>,
    /// Per-state introduced events, as `(entry, timeline seq)`.
    added: HashMap<Hash, Vec<(Uuid, usize)>>,
    /// Every `core/path` event oldest-first:
    /// `(state_idx, path, entry, linked)`.
    path_log: Vec<(usize, String, Uuid, bool)>,
}

/// The live path→entry projection at one coordinate.
struct Listing {
    paths: BTreeMap<String, Uuid>,
    of_entry: HashMap<Uuid, Vec<String>>,
}

/// A metatheca vault, exposed as an arbor.
pub struct MetathecaAdapter {
    vault: Vault,
    /// The head state, pinned at `open`.
    head: Hash,
    nodes: RefCell<Vec<Node>>,
    /// hash → its `/states/<hash>` node.
    state_nodes: RefCell<HashMap<Hash, NodeId>>,
    /// (coordinate, entry) → the canonical entry node under that
    /// coordinate's `entries` dir.
    entry_nodes: RefCell<HashMap<(Hash, Uuid), NodeId>>,
    /// (entry, seq) → the fact-event node.
    fact_nodes: RefCell<HashMap<(Uuid, usize), NodeId>>,
    chain: RefCell<Option<Rc<Chain>>>,
    sweep: RefCell<Option<Rc<Sweep>>>,
    listings: RefCell<HashMap<Hash, Rc<Listing>>>,
}

const ROOT: NodeId = NodeId(0);
const STATES: NodeId = NodeId(1);
const HEAD: NodeId = NodeId(2);
const ENTRIES: NodeId = NodeId(3);
const PATHS: NodeId = NodeId(4);

const NS: i64 = 1_000_000_000;

fn instant_ns(ns: i64) -> Value {
    Value::Instant {
        secs: ns.div_euclid(NS),
        nanos: ns.rem_euclid(NS) as u32,
        offset_min: None,
    }
}

fn short(hash: &Hash) -> String {
    hash.to_hex()[..12].to_string()
}

/// A fact-body JSON value as a typed quarb value; structured values
/// read as their JSON text.
fn body_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            // Fact bodies admit only 64-bit integers; a u64 above
            // i64::MAX coarsens to a float.
            None => Value::Float(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => Value::Str(s.clone()),
        other => Value::Str(other.to_string()),
    }
}

impl MetathecaAdapter {
    /// Open the vault at `path` (its root — the directory holding
    /// `cella/`).
    pub fn open(path: &std::path::Path) -> Result<Self, MetathecaError> {
        let vault = Vault::open(path)?;
        let head = vault.current_state_hash()?;
        let mk = |kind: Kind, parent: Option<NodeId>| Node {
            kind,
            parent,
            children: RefCell::new(None),
        };
        Ok(MetathecaAdapter {
            vault,
            head,
            nodes: RefCell::new(vec![
                mk(Kind::Root, None),
                mk(Kind::StatesDir, Some(ROOT)),
                mk(Kind::Head, Some(ROOT)),
                mk(Kind::EntriesDir { at: head }, Some(ROOT)),
                mk(Kind::PathsDir { at: head }, Some(ROOT)),
            ]),
            state_nodes: RefCell::new(HashMap::new()),
            entry_nodes: RefCell::new(HashMap::new()),
            fact_nodes: RefCell::new(HashMap::new()),
            chain: RefCell::new(None),
            sweep: RefCell::new(None),
            listings: RefCell::new(HashMap::new()),
        })
    }

    /// A human-readable locator: `/states/<short>/paths/docs/a.md`,
    /// `/entries/<uuid>/<fact-short>`, ...
    pub fn locator(&self, node: NodeId) -> String {
        let nodes = self.nodes.borrow();
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            let nd = &nodes[n.0 as usize];
            match &nd.kind {
                Kind::Root => {}
                Kind::StatesDir => parts.push("states".to_string()),
                Kind::Head => parts.push("head".to_string()),
                Kind::State { hash } => parts.push(short(hash)),
                Kind::EntriesDir { .. } => parts.push("entries".to_string()),
                Kind::PathsDir { .. } => parts.push("paths".to_string()),
                Kind::Dir { path, .. } => {
                    parts.push(path.rsplit('/').next().unwrap_or(path).to_string())
                }
                Kind::Entry { label, .. } => parts.push(label.clone()),
                Kind::FactEvent { entry, seq } => parts.push(
                    self.event(*entry, *seq)
                        .map(|h| short(&h))
                        .unwrap_or_else(|| seq.to_string()),
                ),
            }
            cur = nd.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn push_node(&self, kind: Kind, parent: Option<NodeId>) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind,
            parent,
            children: RefCell::new(None),
        });
        id
    }

    fn cached_children(&self, node: NodeId) -> Option<Vec<NodeId>> {
        self.nodes.borrow()[node.0 as usize]
            .children
            .borrow()
            .clone()
    }

    fn cache_children(&self, node: NodeId, ids: Vec<NodeId>) -> Vec<NodeId> {
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    /// The chain, walked on first touch — state blobs only.
    fn chain(&self) -> Rc<Chain> {
        if let Some(c) = self.chain.borrow().as_ref() {
            return Rc::clone(c);
        }
        let states = self.vault.walk(&self.head).unwrap_or_default();
        let pos = states
            .iter()
            .enumerate()
            .map(|(i, (h, _))| (*h, i))
            .collect();
        let chain = Rc::new(Chain { states, pos });
        *self.chain.borrow_mut() = Some(Rc::clone(&chain));
        chain
    }

    /// The history sweep, built on first touch — every fact blob
    /// read once, oldest state first.
    fn sweep(&self) -> Rc<Sweep> {
        if let Some(s) = self.sweep.borrow().as_ref() {
            return Rc::clone(s);
        }
        let chain = self.chain();
        let mut timelines: HashMap<Uuid, Vec<Ev>> = HashMap::new();
        let mut added: HashMap<Hash, Vec<(Uuid, usize)>> = HashMap::new();
        let mut path_log = Vec::new();
        for state_idx in (0..chain.states.len()).rev() {
            let (state_hash, state) = &chain.states[state_idx];
            for fh in &state.added_facts {
                let Ok(bytes) = self.vault.get_fact_bytes(fh) else {
                    continue;
                };
                let Ok(fact) = Fact::from_bytes(&bytes) else {
                    continue;
                };
                if fact.kind == "core/path" {
                    if let (Some(p), Some(l)) = (
                        fact.body.get("path").and_then(|v| v.as_str()),
                        fact.body.get("linked").and_then(|v| v.as_bool()),
                    ) {
                        path_log.push((state_idx, p.to_string(), fact.entry, l));
                    }
                }
                let timeline = timelines.entry(fact.entry).or_default();
                added
                    .entry(*state_hash)
                    .or_default()
                    .push((fact.entry, timeline.len()));
                timeline.push(Ev {
                    hash: *fh,
                    state_idx,
                    fact,
                });
            }
        }
        let sweep = Rc::new(Sweep {
            timelines,
            added,
            path_log,
        });
        *self.sweep.borrow_mut() = Some(Rc::clone(&sweep));
        sweep
    }

    /// The blob hash of the fact event `(entry, seq)`.
    fn event(&self, entry: Uuid, seq: usize) -> Option<Hash> {
        let sweep = self.sweep();
        sweep.timelines.get(&entry)?.get(seq).map(|ev| ev.hash)
    }

    /// The live path→entry projection at `at`. The head answers from
    /// the persistent index; history replays the sweep's path log.
    fn listing(&self, at: &Hash) -> Rc<Listing> {
        if let Some(l) = self.listings.borrow().get(at) {
            return Rc::clone(l);
        }
        let mut paths = BTreeMap::new();
        if *at == self.head {
            for (path, entry) in self.vault.ls(None).unwrap_or_default() {
                if let Ok(id) = parse_entry_id(&entry) {
                    paths.insert(path, id);
                }
            }
        } else {
            let sweep = self.sweep();
            let cutoff = self.chain().pos.get(at).copied();
            // Latest-wins replay, truncated at the coordinate:
            // larger state_idx = older, so at-or-before means
            // `state_idx >= cutoff`.
            let mut latest: BTreeMap<String, (Uuid, bool)> = BTreeMap::new();
            if let Some(cutoff) = cutoff {
                for (idx, path, entry, linked) in &sweep.path_log {
                    if *idx >= cutoff {
                        latest.insert(path.clone(), (*entry, *linked));
                    }
                }
            }
            paths = latest
                .into_iter()
                .filter(|(_, (_, linked))| *linked)
                .map(|(p, (e, _))| (p, e))
                .collect();
        }
        let mut of_entry: HashMap<Uuid, Vec<String>> = HashMap::new();
        for (p, e) in &paths {
            of_entry.entry(*e).or_default().push(p.clone());
        }
        let listing = Rc::new(Listing { paths, of_entry });
        self.listings.borrow_mut().insert(*at, Rc::clone(&listing));
        listing
    }

    /// The `/states/<hash>` node, interned.
    fn state_node(&self, hash: &Hash) -> NodeId {
        if let Some(&id) = self.state_nodes.borrow().get(hash) {
            return id;
        }
        let id = self.push_node(Kind::State { hash: *hash }, Some(STATES));
        self.state_nodes.borrow_mut().insert(*hash, id);
        id
    }

    /// The canonical entry node under `at`'s `entries` dir,
    /// interned.
    fn entry_node(&self, at: &Hash, id: Uuid) -> NodeId {
        if let Some(&n) = self.entry_nodes.borrow().get(&(*at, id)) {
            return n;
        }
        let dir = self.entries_dir(at);
        let n = self.push_node(
            Kind::Entry {
                at: *at,
                id,
                label: id.hyphenated().to_string(),
            },
            Some(dir),
        );
        self.entry_nodes.borrow_mut().insert((*at, id), n);
        n
    }

    /// The `entries` dir node at a coordinate (the root's for the
    /// head, the state node's otherwise).
    fn entries_dir(&self, at: &Hash) -> NodeId {
        if *at == self.head {
            return ENTRIES;
        }
        let state = self.state_node(at);
        self.state_dirs(state).0
    }

    /// A state node's `[entries, paths]` children, minted once.
    fn state_dirs(&self, state: NodeId) -> (NodeId, NodeId) {
        let at = match &self.nodes.borrow()[state.0 as usize].kind {
            Kind::State { hash } => *hash,
            _ => unreachable!("state_dirs on a non-state node"),
        };
        if let Some(c) = self.cached_children(state) {
            return (c[0], c[1]);
        }
        let e = self.push_node(Kind::EntriesDir { at }, Some(state));
        let p = self.push_node(Kind::PathsDir { at }, Some(state));
        self.cache_children(state, vec![e, p]);
        (e, p)
    }

    /// The fact-event node for `(entry, seq)`, interned. Its tree
    /// parent is the head-coordinate entry node; as-of entry nodes
    /// list the same events, truncated.
    fn fact_node(&self, entry: Uuid, seq: usize) -> NodeId {
        if let Some(&n) = self.fact_nodes.borrow().get(&(entry, seq)) {
            return n;
        }
        let parent = self.entry_node(&self.head.clone(), entry);
        let n = self.push_node(Kind::FactEvent { entry, seq }, Some(parent));
        self.fact_nodes.borrow_mut().insert((entry, seq), n);
        n
    }

    /// The state a node stands for (states and the head alias).
    fn state_of(&self, node: NodeId) -> Option<Hash> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::State { hash } => Some(*hash),
            Kind::Head => Some(self.head),
            _ => None,
        }
    }

    /// The latest fact of `kind` on `entry` as of `at` — the
    /// projection the entry's properties answer from. The head
    /// answers from the persistent index.
    fn latest_fact(&self, at: &Hash, entry: Uuid, kind: &str) -> Option<Fact> {
        if *at == self.head {
            return self
                .vault
                .current_fact(&entry.hyphenated().to_string(), kind)
                .ok()
                .flatten();
        }
        let sweep = self.sweep();
        let cutoff = self.chain().pos.get(at).copied()?;
        sweep
            .timelines
            .get(&entry)?
            .iter()
            .rev()
            .find(|ev| ev.state_idx >= cutoff && ev.fact.kind == kind)
            .map(|ev| ev.fact.clone())
    }

    /// The entry's live paths at `at`, in path order.
    fn paths_of(&self, at: &Hash, entry: Uuid) -> Vec<String> {
        self.listing(at)
            .of_entry
            .get(&entry)
            .cloned()
            .unwrap_or_default()
    }

    /// The direct children of a path-tree dir at `at`: entries for
    /// exact paths, dirs for deeper ones (a name can be both).
    fn dir_children(&self, node: NodeId, at: &Hash, prefix: &str) -> Vec<NodeId> {
        if let Some(c) = self.cached_children(node) {
            return c;
        }
        let listing = self.listing(at);
        let full = |seg: &str| {
            if prefix.is_empty() {
                seg.to_string()
            } else {
                format!("{prefix}/{seg}")
            }
        };
        let mut ids = Vec::new();
        // Deeper paths sharing a segment are contiguous in the
        // sorted listing; one dir node covers them all. An exact
        // path and deeper ones may share a segment (a name can be
        // both a file and a dir) — then both nodes appear.
        let mut last_dir: Option<String> = None;
        let want = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        for (path, entry) in listing.paths.range(want.clone()..) {
            let Some(rest) = path.strip_prefix(&want) else {
                break;
            };
            match rest.split_once('/') {
                None => ids.push(self.push_node(
                    Kind::Entry {
                        at: *at,
                        id: *entry,
                        label: rest.to_string(),
                    },
                    Some(node),
                )),
                Some((seg, _)) => {
                    if last_dir.as_deref() == Some(seg) {
                        continue;
                    }
                    ids.push(self.push_node(
                        Kind::Dir {
                            at: *at,
                            path: full(seg),
                        },
                        Some(node),
                    ));
                    last_dir = Some(seg.to_string());
                }
            }
        }
        self.cache_children(node, ids)
    }

    /// The entry's timeline truncated at `at`, as fact-event nodes.
    fn timeline_children(&self, node: NodeId, at: &Hash, entry: Uuid) -> Vec<NodeId> {
        if let Some(c) = self.cached_children(node) {
            return c;
        }
        let sweep = self.sweep();
        let Some(cutoff) = self.chain().pos.get(at).copied() else {
            return self.cache_children(node, Vec::new());
        };
        let ids = sweep
            .timelines
            .get(&entry)
            .map(|tl| {
                tl.iter()
                    .enumerate()
                    .filter(|(_, ev)| ev.state_idx >= cutoff)
                    .map(|(seq, _)| self.fact_node(entry, seq))
                    .collect()
            })
            .unwrap_or_default();
        self.cache_children(node, ids)
    }

    /// Every entry with at least one fact at-or-before `at`,
    /// ascending by entry ID (UUID v7: chronological).
    fn entries_at(&self, at: &Hash) -> Vec<Uuid> {
        let sweep = self.sweep();
        let Some(cutoff) = self.chain().pos.get(at).copied() else {
            return Vec::new();
        };
        let mut ids: Vec<Uuid> = sweep
            .timelines
            .iter()
            .filter(|(_, tl)| tl.iter().any(|ev| ev.state_idx >= cutoff))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }
}

impl AstAdapter for MetathecaAdapter {
    fn root(&self) -> NodeId {
        ROOT
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        match kind {
            Kind::Root => vec![STATES, HEAD, ENTRIES, PATHS],
            Kind::StatesDir => {
                if let Some(c) = self.cached_children(STATES) {
                    return c;
                }
                let chain = self.chain();
                let ids = chain
                    .states
                    .iter()
                    .map(|(h, _)| self.state_node(h))
                    .collect();
                self.cache_children(STATES, ids)
            }
            Kind::Head => {
                if let Some(c) = self.cached_children(HEAD) {
                    return c;
                }
                let head = self.head;
                let e = self.push_node(Kind::EntriesDir { at: head }, Some(HEAD));
                let p = self.push_node(Kind::PathsDir { at: head }, Some(HEAD));
                self.cache_children(HEAD, vec![e, p])
            }
            Kind::State { .. } => {
                let (e, p) = self.state_dirs(node);
                vec![e, p]
            }
            Kind::EntriesDir { at } => {
                if let Some(c) = self.cached_children(node) {
                    return c;
                }
                let ids = self
                    .entries_at(&at)
                    .into_iter()
                    .map(|id| self.entry_node(&at, id))
                    .collect();
                self.cache_children(node, ids)
            }
            Kind::PathsDir { at } => self.dir_children(node, &at, ""),
            Kind::Dir { at, path } => self.dir_children(node, &at, &path),
            Kind::Entry { at, id, .. } => self.timeline_children(node, &at, id),
            Kind::FactEvent { .. } => Vec::new(),
        }
    }

    /// Literal names under `/states` go straight through metatheca's
    /// stateref resolution — full hash, `>=8`-hex prefix, ISO-8601
    /// instant, `~N`, `current` — with no enumeration.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        match kind {
            Kind::StatesDir => {
                let Ok(hash) = self.vault.resolve(name) else {
                    return Vec::new();
                };
                vec![self.state_node(&hash)]
            }
            Kind::EntriesDir { at } => {
                let Ok(id) = parse_entry_id(name) else {
                    return Vec::new();
                };
                // The head answers from the index (no sweep);
                // history checks the timeline at the coordinate.
                let known = if at == self.head {
                    self.vault.resolve_entry(name).is_ok()
                } else {
                    self.sweep()
                        .timelines
                        .get(&id)
                        .is_some_and(|tl| match self.chain().pos.get(&at) {
                            Some(&cutoff) => tl.iter().any(|ev| ev.state_idx >= cutoff),
                            None => false,
                        })
                };
                if known {
                    vec![self.entry_node(&at, id)]
                } else {
                    Vec::new()
                }
            }
            _ => self
                .children(node)
                .into_iter()
                .filter(|&c| self.name(c).as_deref() == Some(name))
                .collect(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => None,
            Kind::StatesDir => Some("states".to_string()),
            Kind::Head => Some("head".to_string()),
            Kind::State { hash } => Some(hash.to_hex()),
            Kind::EntriesDir { .. } => Some("entries".to_string()),
            Kind::PathsDir { .. } => Some("paths".to_string()),
            Kind::Dir { path, .. } => {
                Some(path.rsplit('/').next().unwrap_or(path).to_string())
            }
            Kind::Entry { label, .. } => Some(label.clone()),
            Kind::FactEvent { entry, seq } => {
                let sweep = self.sweep();
                Some(
                    sweep
                        .timelines
                        .get(entry)
                        .and_then(|tl| tl.get(*seq))
                        .map(|ev| ev.hash.to_hex())
                        .unwrap_or_else(|| seq.to_string()),
                )
            }
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// `<state>`, `<genesis>`, `<head>`; `<dir>`; `<entry>`,
    /// `<orphan>`, `<changed>`; `<fact>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        match kind {
            Kind::Root | Kind::StatesDir | Kind::EntriesDir { .. } | Kind::PathsDir { .. } => {
                Vec::new()
            }
            Kind::Head => vec!["state".to_string(), "head".to_string()],
            Kind::State { hash } => {
                let mut out = vec!["state".to_string()];
                if hash == self.head {
                    out.push("head".to_string());
                }
                if self
                    .vault
                    .get_state(&hash)
                    .ok()
                    .is_some_and(|s| s.previous.is_none())
                {
                    out.push("genesis".to_string());
                }
                out
            }
            Kind::Dir { .. } => vec!["dir".to_string()],
            Kind::Entry { at, id, .. } => {
                let mut out = vec!["entry".to_string()];
                if self.paths_of(&at, id).is_empty() {
                    out.push("orphan".to_string());
                }
                // <changed>: a fact of this entry was added in
                // exactly this coordinate's state.
                if self
                    .sweep()
                    .added
                    .get(&at)
                    .is_some_and(|evs| evs.iter().any(|(e, _)| *e == id))
                {
                    out.push("changed".to_string());
                }
                out
            }
            Kind::FactEvent { .. } => vec!["fact".to_string()],
        }
    }

    /// States: `::at`, `::hash`, `::previous`, `::n-facts`. Entries:
    /// typed core projections and the generic `::'ns/name'`. Fact
    /// events: `::kind`, `::at`, `::entry`, `::body`, and the body's
    /// own keys, typed.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if let Some(hash) = self.state_of(node) {
            let state = self.vault.get_state(&hash).ok()?;
            return Some(match name {
                "at" => instant_ns(state.created_at_ns),
                "hash" => Value::Str(hash.to_hex()),
                "previous" => match state.previous {
                    Some(p) => Value::Str(p.to_hex()),
                    None => Value::Null,
                },
                "n-facts" => Value::Int(state.added_facts.len() as i64),
                _ => return None,
            });
        }
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        match kind {
            Kind::Entry { at, id, .. } => {
                // Any name with a `/` is a fact kind: the latest
                // fact of that kind at this coordinate. A
                // single-field body unwraps to a typed scalar;
                // a multi-field body reads as canonical JSON.
                if name.contains('/') {
                    let fact = self.latest_fact(&at, id, name)?;
                    if fact.body.len() == 1 {
                        return Some(body_value(fact.body.values().next()?));
                    }
                    return Some(Value::Str(
                        serde_json::Value::Object(fact.body).to_string(),
                    ));
                }
                match name {
                    "id" => Some(Value::Str(id.hyphenated().to_string())),
                    "mime" => self
                        .latest_fact(&at, id, "core/mime")?
                        .body
                        .get("mime_type")
                        .map(body_value),
                    "blob" => self
                        .latest_fact(&at, id, "core/blob-ref")?
                        .body
                        .get("blob")
                        .map(body_value),
                    "size" => self
                        .latest_fact(&at, id, "core/blob-ref")?
                        .body
                        .get("size")
                        .and_then(|v| v.as_i64())
                        .map(Value::bytes),
                    "path" => self.paths_of(&at, id).into_iter().next().map(Value::Str),
                    "paths" => Some(Value::List(
                        self.paths_of(&at, id).into_iter().map(Value::Str).collect(),
                    )),
                    "mtime" | "ctime" | "birthtime" => {
                        let key = format!("{name}_ns");
                        self.latest_fact(&at, id, "core/fs-metadata")?
                            .body
                            .get(&key)
                            .and_then(|v| v.as_i64())
                            .map(instant_ns)
                    }
                    "mode" => self
                        .latest_fact(&at, id, "core/fs-metadata")?
                        .body
                        .get("mode")
                        .and_then(|v| v.as_i64())
                        .map(Value::Int),
                    "source-path" => self
                        .latest_fact(&at, id, "core/fs-metadata")?
                        .body
                        .get("source_path")
                        .and_then(|v| v.as_str())
                        .map(|s| Value::Str(s.to_string())),
                    _ => None,
                }
            }
            Kind::FactEvent { entry, seq } => {
                let sweep = self.sweep();
                let ev = sweep.timelines.get(&entry)?.get(seq)?;
                match name {
                    "kind" => Some(Value::Str(ev.fact.kind.clone())),
                    "at" => {
                        let chain = self.chain();
                        Some(instant_ns(chain.states[ev.state_idx].1.created_at_ns))
                    }
                    "entry" => Some(Value::Str(entry.hyphenated().to_string())),
                    "body" => Some(Value::Str(
                        serde_json::Value::Object(ev.fact.body.clone()).to_string(),
                    )),
                    // `size` on a blob-ref stays a byte quantity,
                    // matching the entry-level projection.
                    "size" if ev.fact.kind == "core/blob-ref" => {
                        ev.fact.body.get("size").and_then(|v| v.as_i64()).map(Value::bytes)
                    }
                    _ => ev.fact.body.get(name).map(body_value),
                }
            }
            _ => None,
        }
    }

    /// An entry's content at its coordinate (text, lossily decoded).
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        match kind {
            Kind::Entry { at, id, .. } => {
                let fact = self.latest_fact(&at, id, "core/blob-ref")?;
                let hex = fact.body.get("blob")?.as_str()?;
                let hash = Hash::from_hex(hex).ok()?;
                let bytes = self.vault.get_blob(&hash).ok()?;
                Some(Value::Str(String::from_utf8_lossy(&bytes).into_owned()))
            }
            Kind::FactEvent { entry, seq } => {
                let sweep = self.sweep();
                let ev = sweep.timelines.get(&entry)?.get(seq)?;
                let full = serde_json::json!({
                    "kind": ev.fact.kind,
                    "entry": entry.hyphenated().to_string(),
                    "body": serde_json::Value::Object(ev.fact.body.clone()),
                });
                Some(Value::Str(full.to_string()))
            }
            _ => None,
        }
    }

    /// States: `;;;short`, `;;;n-facts`, `;;;seq` (0 = genesis).
    /// Entries: `;;;blob`, `;;;size`, `;;;n-facts`, `;;;n-paths`,
    /// `;;;first-at`, `;;;last-at`. Fact events: `;;;hash`,
    /// `;;;short`, `;;;state`, `;;;state-short`, `;;;seq`.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if let Some(hash) = self.state_of(node) {
            return match key {
                "short" => Some(Value::Str(short(&hash))),
                "n-facts" => Some(Value::Int(
                    self.vault.get_state(&hash).ok()?.added_facts.len() as i64,
                )),
                "seq" => {
                    let chain = self.chain();
                    let pos = *chain.pos.get(&hash)?;
                    Some(Value::Int((chain.states.len() - 1 - pos) as i64))
                }
                _ => None,
            };
        }
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        match kind {
            Kind::Entry { at, id, .. } => match key {
                "blob" => self.property(node, "blob"),
                "size" => self.property(node, "size"),
                "n-paths" => Some(Value::Int(self.paths_of(&at, id).len() as i64)),
                "n-facts" | "first-at" | "last-at" => {
                    let sweep = self.sweep();
                    let chain = self.chain();
                    let cutoff = *chain.pos.get(&at)?;
                    let tl = sweep.timelines.get(&id)?;
                    let mut evs = tl.iter().filter(|ev| ev.state_idx >= cutoff);
                    match key {
                        "n-facts" => Some(Value::Int(evs.count() as i64)),
                        "first-at" => evs
                            .next()
                            .map(|ev| instant_ns(chain.states[ev.state_idx].1.created_at_ns)),
                        _ => evs
                            .last()
                            .map(|ev| instant_ns(chain.states[ev.state_idx].1.created_at_ns)),
                    }
                }
                _ => None,
            },
            Kind::FactEvent { entry, seq } => {
                let sweep = self.sweep();
                let ev = sweep.timelines.get(&entry)?.get(seq)?;
                match key {
                    "hash" => Some(Value::Str(ev.hash.to_hex())),
                    "short" => Some(Value::Str(short(&ev.hash))),
                    "state" => {
                        let chain = self.chain();
                        Some(Value::Str(chain.states[ev.state_idx].0.to_hex()))
                    }
                    "state-short" => {
                        let chain = self.chain();
                        Some(Value::Str(short(&chain.states[ev.state_idx].0)))
                    }
                    "seq" => Some(Value::Int(seq as i64)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `::previous~>` on a state; `::state~>` / `::entry~>` on a
    /// fact event.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        if let Some(hash) = self.state_of(node) {
            if property != "previous" {
                return None;
            }
            let prev = self.vault.get_state(&hash).ok()?.previous?;
            return Some(self.state_node(&prev));
        }
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        let Kind::FactEvent { entry, seq } = kind else {
            return None;
        };
        match property {
            "state" => {
                let sweep = self.sweep();
                let ev = sweep.timelines.get(&entry)?.get(seq)?;
                let chain = self.chain();
                Some(self.state_node(&chain.states[ev.state_idx].0.clone()))
            }
            "entry" => Some(self.entry_node(&self.head.clone(), entry)),
            _ => None,
        }
    }

    /// A state's `previous` edge and its `added` fact events; a fact
    /// event's `state` and `entry` edges.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        if let Some(hash) = self.state_of(node) {
            let mut out = Vec::new();
            if let Ok(state) = self.vault.get_state(&hash) {
                if let Some(prev) = state.previous {
                    out.push(("previous".to_string(), self.state_node(&prev)));
                }
                if !state.added_facts.is_empty() {
                    let sweep = self.sweep();
                    if let Some(evs) = sweep.added.get(&hash) {
                        for (entry, seq) in evs {
                            out.push(("added".to_string(), self.fact_node(*entry, *seq)));
                        }
                    }
                }
            }
            return out;
        }
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        let Kind::FactEvent { entry, .. } = kind else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(n) = self.resolve(node, "state", None) {
            out.push(("state".to_string(), n));
        }
        out.push(("entry".to_string(), self.entry_node(&self.head.clone(), entry)));
        out
    }

    /// `<-previous`: the successor state (the chain is linear).
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some(hash) = self.state_of(node) else {
            return Vec::new();
        };
        let chain = self.chain();
        let Some(&pos) = chain.pos.get(&hash) else {
            return Vec::new();
        };
        if pos == 0 {
            return Vec::new();
        }
        vec![(
            "previous".to_string(),
            self.state_node(&chain.states[pos - 1].0.clone()),
        )]
    }

    /// Open quantifiers must be able to walk the whole chain:
    /// `(->previous)*` from the head reaches genesis.
    fn quantifier_bound(&self) -> usize {
        32.max(self.chain().states.len() + 1)
    }
}
