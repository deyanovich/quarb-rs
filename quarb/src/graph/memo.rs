//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// What a run has learned about one scope of a reverse arrow or a
/// value match: the nodes the scope selects, what each resolves to
/// under a property and a reference label, and the nodes holding
/// each value of a property. A scope is a path from the root, so
/// its nodes are the same for every node the step is asked about;
/// without the memo a step over N nodes read an M-node scope N
/// times.
pub(crate) struct ScopeMemo {
    pub(crate) nodes: std::rc::Rc<Vec<NodeId>>,
    pub(crate) referrers:
        std::cell::RefCell<HashMap<(String, Option<String>), std::rc::Rc<Referrers>>>,
    pub(crate) values: std::cell::RefCell<HashMap<String, std::rc::Rc<ValueKeys>>>,
    /// Whether the scoped nodes declare a reference that can land
    /// in a container (by the kind of question and the container's
    /// node): asked once, since it is a matter of declarations.
    pub(crate) declared: std::cell::RefCell<HashMap<(bool, u64), bool>>,
}

/// Target node → the positions (in scope order) of the nodes whose
/// property resolves to it.
pub(crate) type Referrers = HashMap<NodeId, Vec<usize>>;

/// The scope's nodes by the equality key of a property's value;
/// `unkeyed` are the nodes whose value is compared under another
/// reading and must be tested every time.
pub(crate) struct ValueKeys {
    pub(crate) keyed: HashMap<String, Vec<usize>>,
    pub(crate) unkeyed: Vec<usize>,
}

thread_local! {
    /// The scope memos of the eval in progress, by adapter and
    /// scope text; cleared with the refusal cell. The adapter's
    /// address is in the key because an adapter may run queries of
    /// its own (a model reading its base) inside the outer eval.
    static SCOPES: std::cell::RefCell<HashMap<(usize, String), std::rc::Rc<ScopeMemo>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// A node's link list as last asked: `(adapter, node)` and the
/// list. A step asks for a node's links to walk them, and then
/// once more per successor to recover the label of the edge it
/// arrived by; on a store adapter each ask is a remote query, so
/// the second and later asks answer from here.
pub(crate) type LastLinks = Option<((usize, NodeId), std::rc::Rc<Vec<(String, NodeId)>>)>;

pub(crate) fn links_of(
    adapter: &impl AstAdapter,
    node: NodeId,
) -> std::rc::Rc<Vec<(String, NodeId)>> {
    let key = (std::ptr::from_ref(adapter).cast::<u8>() as usize, node);
    if let Some(hit) = with_graph_run(|s| match &s.last_out {
        Some((k, v)) if *k == key => Some(v.clone()),
        _ => None,
    }) {
        return hit;
    }
    let v = std::rc::Rc::new(adapter.links(node));
    with_graph_run(|s| s.last_out = Some((key, v.clone())));
    v
}

pub(crate) fn backlinks_of(
    adapter: &impl AstAdapter,
    node: NodeId,
) -> std::rc::Rc<Vec<(String, NodeId)>> {
    let key = (std::ptr::from_ref(adapter).cast::<u8>() as usize, node);
    if let Some(hit) = with_graph_run(|s| match &s.last_in {
        Some((k, v)) if *k == key => Some(v.clone()),
        _ => None,
    }) {
        return hit;
    }
    let v = std::rc::Rc::new(adapter.backlinks(node));
    with_graph_run(|s| s.last_in = Some((key, v.clone())));
    v
}

/// Whether a scope's nodes are the same wherever the step stands:
/// its text mentions no register, mark, topic, driver, capture or
/// fragment — nothing that looks outward. A conservative test on
/// the canonical text: a scope it rejects is evaluated afresh for
/// every node, as before.
pub(crate) fn scope_is_static(text: &str) -> bool {
    if text.contains(['$', '@', '%', '&']) || text.contains("((") {
        return false;
    }
    let b: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    for (i, &c) in b.iter().enumerate() {
        // the driver `_`, standing alone
        if c == '_' && !(i > 0 && word(b[i - 1])) && !(i + 1 < b.len() && word(b[i + 1])) {
            return false;
        }
        // a capture `(N)`
        if c == '(' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
            let mut j = i + 1;
            while b.get(j).is_some_and(|d| d.is_ascii_digit()) {
                j += 1;
            }
            if b.get(j) == Some(&')') {
                return false;
            }
        }
    }
    true
}

/// The memo of a scope, its nodes found by `nodes` on first use.
pub(crate) fn scope_memo(
    adapter: &impl AstAdapter,
    key: &str,
    nodes: impl FnOnce() -> Vec<NodeId>,
) -> std::rc::Rc<ScopeMemo> {
    let k = (
        std::ptr::from_ref(adapter).cast::<u8>() as usize,
        key.to_string(),
    );
    if let Some(m) = with_graph_run(|s| s.scopes.get(&k).cloned()) {
        return m;
    }
    // Found outside any borrow: the scope's own predicates may run
    // a scoped step of their own.
    let memo = std::rc::Rc::new(ScopeMemo {
        nodes: std::rc::Rc::new(nodes()),
        referrers: std::cell::RefCell::new(HashMap::new()),
        values: std::cell::RefCell::new(HashMap::new()),
        declared: std::cell::RefCell::new(HashMap::new()),
    });
    with_graph_run(|s| s.scopes.insert(k, memo.clone()));
    memo
}

/// Every node of the arbor: the tree and the nodes outside it.
pub(crate) fn whole_arbor(adapter: &impl AstAdapter) -> Vec<NodeId> {
    let mut all = Vec::new();
    collect_subtree(adapter, adapter.root(), &mut all);
    all.extend(adapter.non_tree_nodes());
    all
}

/// The scope's nodes one of whose properties resolves to `target`
/// under `label` — and, with no label written, under `implied`
/// (the target's own container) too — in scope order.
/// `properties_of` says which properties a scoped node is asked
/// about, and `property` names that choice in the memo. Each
/// scoped node is resolved once a run per label; the caller still
/// tests what this names.
pub(crate) fn memo_referrers(
    adapter: &impl AstAdapter,
    memo: &ScopeMemo,
    property: &str,
    properties_of: impl Fn(NodeId) -> Vec<String>,
    label: Option<&str>,
    implied: Option<&str>,
    target: NodeId,
) -> Vec<NodeId> {
    let mut labels = vec![label.map(str::to_string)];
    if label.is_none()
        && let Some(i) = implied
    {
        labels.push(Some(i.to_string()));
    }
    let mut at: Vec<usize> = Vec::new();
    for l in labels {
        let key = (property.to_string(), l);
        let known = memo.referrers.borrow().get(&key).cloned();
        let map = match known {
            Some(m) => m,
            None => {
                let nodes = memo.nodes.clone();
                let mut map: Referrers = HashMap::new();
                for (pos, &u) in nodes.iter().enumerate() {
                    for p in properties_of(u) {
                        if let Some(t) = adapter.resolve(u, &p, key.1.as_deref()) {
                            let at = map.entry(t).or_default();
                            if at.last() != Some(&pos) {
                                at.push(pos);
                            }
                        }
                    }
                }
                let map = std::rc::Rc::new(map);
                memo.referrers.borrow_mut().insert(key, map.clone());
                map
            }
        };
        if let Some(hits) = map.get(&target) {
            at.extend(hits);
        }
    }
    at.sort_unstable();
    at.dedup();
    at.into_iter().map(|pos| memo.nodes[pos]).collect()
}

/// The scope's nodes that may hold `value` in `property`: those
/// under its equality key and those that must be tested every
/// time, in scope order. A value with no key is tested against all.
pub(crate) fn memo_holders(
    adapter: &impl AstAdapter,
    memo: &ScopeMemo,
    property: &str,
    value: &Value,
) -> Vec<NodeId> {
    let Some(key) = value.equality_key() else {
        return memo.nodes.to_vec();
    };
    let known = memo.values.borrow().get(property).cloned();
    let keys = match known {
        Some(k) => k,
        None => {
            let nodes = memo.nodes.clone();
            let mut keyed: HashMap<String, Vec<usize>> = HashMap::new();
            let mut unkeyed = Vec::new();
            for (pos, &u) in nodes.iter().enumerate() {
                match adapter.property(u, property) {
                    Some(v) => match v.equality_key() {
                        Some(k) => keyed.entry(k).or_default().push(pos),
                        None => unkeyed.push(pos),
                    },
                    None => {}
                }
            }
            let keys = std::rc::Rc::new(ValueKeys { keyed, unkeyed });
            memo.values
                .borrow_mut()
                .insert(property.to_string(), keys.clone());
            keys
        }
    };
    let mut at: Vec<usize> = keys.keyed.get(&key).cloned().unwrap_or_default();
    at.extend(&keys.unkeyed);
    at.sort_unstable();
    at.dedup();
    at.into_iter().map(|pos| memo.nodes[pos]).collect()
}
