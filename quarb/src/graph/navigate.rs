//! The nodes a thread stands on and walks between: the graph's
//! per-thread state (its position, marks, correlation witness and
//! arrived edges), the anchors, and the navigation helpers over an
//! adapter.
use super::*;

/// The graph layer's per-thread state: the position, the marks
/// pocketed on the walk, the correlation witness and the edges the
/// last hop arrived by. Copied on a fork, never shared.
#[derive(Clone)]
pub(crate) struct GraphExt {
    /// The position: the node the thread stands on.
    pub(crate) node: NodeId,
    /// The correlation witness that admitted this thread (one node
    /// per trace context; null for an outer context no tuple
    /// admitted), for `$*k` in pipeline commands.
    pub(crate) bindings: Vec<Option<NodeId>>,
    /// The edges that reached this node on the walk's final hop —
    /// the `@-` operand, aggregated across merged crossings.
    pub(crate) arrived: Vec<EdgeCtx>,
    /// No position (spec: Aggregating Pipe): the one thread a
    /// reduction leaves stands nowhere — `$$_` and `$-` are null,
    /// and navigation continues only where the query says from
    /// where (`$$__/…`, a mark). `node` then holds the root as a
    /// placeholder the host never answers for.
    pub(crate) unplaced: bool,
}

/// What a scope says of the graph's state beyond the attachment it
/// borrows: whether it belongs to a thread standing on its node —
/// what a `$$`-wrapped node operand (`_::prop`, `_/child::x`) reads
/// one scope out. `None` for navigation scopes.
#[derive(Clone, Copy, Default)]
pub(crate) struct GraphView {
    pub(crate) node: Option<NodeId>,
}

impl crate::thread::Attachment for GraphExt {
    type View = GraphView;
    type Crossing = EdgeCtx;
    type Witness = [Option<NodeId>];
    type Key = NodeId;
    fn view(&self) -> GraphView {
        GraphView {
            node: (!self.unplaced).then_some(self.node),
        }
    }
    fn key(&self) -> NodeId {
        self.node
    }
    fn bare(&self) -> GraphExt {
        GraphExt {
            unplaced: self.unplaced,
            ..GraphExt::at(self.node)
        }
    }
    fn standing(view: &GraphView) -> bool {
        view.node.is_some()
    }
}

impl GraphExt {
    /// Standing at `node` with nothing pocketed, witnessed or
    /// arrived by.
    pub(crate) fn at(node: NodeId) -> GraphExt {
        GraphExt {
            node,
            bindings: Vec::new(),
            arrived: Vec::new(),
            unplaced: false,
        }
    }

    /// Standing nowhere: the thread a reduction leaves.
    pub(crate) fn unplaced(root: NodeId) -> GraphExt {
        GraphExt {
            unplaced: true,
            ..GraphExt::at(root)
        }
    }
}

/// The node a value names, when the value is a reference.
pub(crate) fn ref_node(v: &Value) -> Option<NodeId> {
    match v {
        Value::Node(n) => Some(NodeId(n.id)),
        _ => None,
    }
}

/// The references a register holds, in filing order: the node side
/// of the capsa (`$$.N`, `((.N))`, `(@)`).
pub(crate) fn references(register: &[Reg]) -> impl Iterator<Item = NodeId> + '_ {
    register.iter().filter_map(|r| ref_node(&r.value))
}

/// A reference to `node` (spec: The Capsa, "Node references"): its
/// coordinate and its locator, the provenance path where the
/// adapter gives one, else the name path.
pub(crate) fn node_ref(adapter: &impl AstAdapter, node: NodeId) -> Value {
    let locator = resolved_provenance(adapter, node)
        .first()
        .path
        .unwrap_or_else(|| ancestor_path(adapter, node, |a, n| a.name(n)));
    Value::Node(crate::value::NodeRef::new(node.0, locator))
}

/// The regula a mark files: a reference to `node` under `name`
/// (`.name` is `.name($$_)`), its provenance the node itself.
pub(crate) fn mark_reg(adapter: &impl AstAdapter, node: NodeId, name: Option<String>) -> Reg {
    Reg {
        name,
        value: node_ref(adapter, node),
        prov: Prov::at(node.into(), command_now()),
        site: Origin {
            position: node.into(),
            command: command_now(),
        },
    }
}

impl From<NodeId> for Position {
    fn from(n: NodeId) -> Position {
        Position(n.0)
    }
}

impl From<Position> for NodeId {
    fn from(p: Position) -> NodeId {
        NodeId(p.0)
    }
}

/// A thread of the graph: the engine's capsa, a thread with the
/// graph's state attached.
pub(crate) type Capsa = Thread<GraphExt>;

/// The nodes a relative operand starts from: a
/// group's members when the capsa is a group and the anchor is
/// the current node, else the anchor's own resolution.
pub(crate) fn operand_from(
    adapter: &impl AstAdapter,
    anchor: &Anchor,
    current: NodeId,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_, GraphExt>,
) -> Vec<NodeId> {
    if matches!(anchor, Anchor::Current) && !scope.members.is_empty() {
        return scope.members.iter().map(|m| m.ext.node).collect();
    }
    anchor_nodes(adapter, anchor, current, trace, bound, scope)
}

pub(crate) fn anchor_nodes(
    adapter: &impl AstAdapter,
    anchor: &Anchor,
    current: NodeId,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_, GraphExt>,
) -> Vec<NodeId> {
    let register = scope.register;
    match anchor {
        Anchor::Current => vec![current],
        Anchor::Root => vec![adapter.root()],
        // `((!expr))` — coordinates stood on again: every id the
        // expression yields (an integer, a coordinate record's
        // `id`, a list of either), deduplicated; an id the adapter
        // never minted is no node. The reads the anchor makes are
        // not origins.
        Anchor::Id(op) => {
            let vs = discard(|| eval_operand_at(adapter, current, op, trace, bound, scope));
            let mut ids: Vec<NodeId> = Vec::new();
            fn collect(v: &Value, out: &mut Vec<NodeId>) {
                match v {
                    Value::Int(n) if *n >= 0 => out.push(NodeId(*n as u64)),
                    Value::Node(n) => out.push(NodeId(n.id)),
                    Value::Record(fields) => {
                        if let Some((_, id)) = fields.iter().find(|(k, _)| k == "id") {
                            collect(id, out);
                        }
                    }
                    Value::List(items) => items.iter().for_each(|i| collect(i, out)),
                    _ => {}
                }
            }
            for v in &vs {
                collect(v, &mut ids);
            }
            dedup(ids)
                .into_iter()
                .filter(|&n| adapter.has_node(n))
                .collect()
        }
        // The node side of the capsa (spec: The Capsa): `$$.name`
        // is the latest regula under the name, dereferenced —
        // nothing when a value was pushed under it last (one name
        // pool); `$$.N` the N-th reference in filing order, `$$.`
        // the latest reference.
        Anchor::Mark(m) => register
            .iter()
            .rev()
            .find(|r| r.name.as_deref() == Some(m))
            .and_then(|r| ref_node(&r.value))
            .into_iter()
            .collect(),
        Anchor::MarkIndex(n) => references(register).nth(n - 1).into_iter().collect(),
        Anchor::MarkTop => references(register).last().into_iter().collect(),
        Anchor::MarkBack(n) => references(register)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .nth(n - 1)
            .into_iter()
            .collect(),
        Anchor::MarksAll => references(register).collect(),
        Anchor::MarksNamed(m) => register
            .iter()
            .filter(|r| r.name.as_deref() == Some(m))
            .filter_map(|r| ref_node(&r.value))
            .collect(),
    }
}

/// The edge the current thread most recently walked — the `$-`
/// operand's referent, defined after every hop (a hop walks a
/// labeled edge; the tree is the distinguished subset). Crosslink
/// and resolution hops carry their stored label and direction;
/// structural relations carry the engine-reserved bracketed labels
/// (`[child]`, `[parent]`, `[next]`, `[prev]`) in walked direction —
/// brackets cannot appear in a bare name, so the namespace cannot
/// collide with adapter labels.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct EdgeCtx {
    pub(crate) source: NodeId,
    pub(crate) label: String,
    pub(crate) target: NodeId,
}

/// The engine-reserved structural edge labels.
pub(crate) const CHILD: &str = "[child]";

pub(crate) const PARENT: &str = "[parent]";

pub(crate) const NEXT: &str = "[next]";

pub(crate) const PREV: &str = "[prev]";

/// The core fields every node answers (`:fields`, as projection
/// values), in the order
/// `core_meta` reads them; an unknown `:key` is refused naming them.
pub(crate) const CORE_FIELDS: &[&str] = &[
    "name",
    "id",
    "index",
    "depth",
    "parent-name",
    "parent-id",
    "parent-index",
    "n-children",
    "n-descendants",
    "n-siblings",
    "traits",
    "props",
    "meta",
    "fields",
    "is-leaf",
    "is-root",
    "is-first-child",
    "is-last-child",
    "is-top-level",
    "is-non-tree",
    "name-path",
    "index-path",
    "id-path",
    "source",
    "path",
    "instant",
    "dpid",
    "provenance",
    "%provenance",
    "@provenance",
    "origin",
    "%origin",
    "@origin",
    "elided",
];

/// The core fields (`:key`), the fields the engine gives every
/// node, derived from the navigation surface and independent of the
/// adapter's domain.
pub(crate) fn core_meta(adapter: &impl AstAdapter, node: NodeId, key: &str) -> Option<Value> {
    let index = || index_of(adapter, node);
    let n_siblings = || {
        adapter
            .parent(node)
            .map(|p| adapter.children(p).len().saturating_sub(1))
    };
    match key {
        "name" => Some(adapter.name(node).map_or(Value::Null, Value::Str)),
        "id" => Some(Value::Int(node.0 as i64)),
        "index" => Some(index().map_or(Value::Null, |i| Value::Int(i as i64))),
        "depth" => Some(Value::Int(depth_of(adapter, node) as i64)),
        "parent-name" => Some(
            adapter
                .parent(node)
                .and_then(|p| adapter.name(p))
                .map_or(Value::Null, Value::Str),
        ),
        "parent-id" => Some(
            adapter
                .parent(node)
                .map_or(Value::Null, |p| Value::Int(p.0 as i64)),
        ),
        "parent-index" => Some(
            adapter
                .parent(node)
                .and_then(|p| index_of(adapter, p))
                .map_or(Value::Null, |i| Value::Int(i as i64)),
        ),
        "n-children" => Some(Value::Int(adapter.children(node).len() as i64)),
        "n-descendants" => Some(Value::Int(n_descendants(adapter, node) as i64)),
        "n-siblings" => Some(n_siblings().map_or(Value::Null, |n| Value::Int(n as i64))),
        // The inspection fields (draft 164): the names of the three
        // ladders, each listable from the node.
        "props" | "properties" => Some(Value::list(
            adapter
                .property_names(node)
                .into_iter()
                .map(|k| Value::projection(2, k))
                .collect(),
        )),
        "meta" | "metadata" => Some(Value::list(
            adapter
                .metadata_keys(node)
                .into_iter()
                .map(|k| Value::projection(3, k))
                .collect(),
        )),
        "fields" => Some(Value::list(
            CORE_FIELDS.iter().map(|k| Value::projection(1, *k)).collect(),
        )),
        "traits" => Some(Value::list(
            adapter.traits(node).into_iter().map(Value::Str).collect(),
        )),
        "is-leaf" => Some(Value::Bool(adapter.children(node).is_empty())),
        "is-root" => Some(Value::Bool(adapter.parent(node).is_none())),
        "is-first-child" => Some(Value::Bool(index() == Some(1))),
        "is-last-child" => Some(Value::Bool(
            matches!((index(), n_siblings()), (Some(i), Some(n)) if i == n + 1),
        )),
        "is-top-level" => Some(Value::Bool(depth_of(adapter, node) == 1)),
        // A node the adapter declares outside the tree (joined to
        // it by crosslinks alone); the shipped adapters declare none.
        "is-non-tree" => Some(Value::Bool(adapter.non_tree_nodes().contains(&node))),
        // Root-relative location paths, one token per level.
        "name-path" => Some(Value::Str(ancestor_path(adapter, node, |a, n| a.name(n)))),
        "index-path" => Some(Value::Str(ancestor_path(adapter, node, |a, n| {
            index_of(a, n).map(|i| i.to_string())
        }))),
        "id-path" => Some(Value::Str(ancestor_path(adapter, node, |_, n| {
            Some(n.0.to_string())
        }))),
        // Data provenance: the layered, adapter-supplied triple and
        // its composite. Absent components answer null — a stated
        // absence, never the invocation clock.
        "source" => Some(
            resolved_provenance(adapter, node)
                .first()
                .source
                .map_or(Value::Null, Value::Str),
        ),
        "path" => Some(
            resolved_provenance(adapter, node)
                .first()
                .path
                .map_or(Value::Null, Value::Str),
        ),
        // The instant of a datum with several sources is its newest.
        "instant" => Some(resolved_provenance(adapter, node).instant_max().map_or(
            Value::Null,
            |(secs, nanos, offset_min)| Value::Instant {
                secs,
                nanos,
                offset_min,
                calendar: Default::default(),
            },
        )),
        "dpid" => Some(
            resolved_provenance(adapter, node)
                .first()
                .dpid
                .map_or(Value::Null, Value::Str),
        ),
        // The composite is the list: `?a@ts#x;b#y;+N` where the
        // substrate records several sources, one entry otherwise.
        "provenance" => Some(
            resolved_provenance(adapter, node)
                .canonical()
                .map_or(Value::Null, Value::Str),
        ),
        // The shaped keys: `%` the record, `@` the
        // list — of the provenance entries, and of the origin
        // coordinates. A node's origin is itself, at command 0.
        "%provenance" => Some(prov_record(&resolved_provenance(adapter, node).first())),
        "@provenance" => Some(Value::list(
            resolved_provenance(adapter, node)
                .entries
                .iter()
                .map(prov_record)
                .collect(),
        )),
        "origin" => Some(Value::Int(node.0 as i64)),
        "%origin" => Some(origin_record(Origin {
            position: node.into(),
            command: 0,
        })),
        "@origin" => Some(Value::list(vec![origin_record(Origin {
            position: node.into(),
            command: 0,
        })])),
        "elided" => Some(Value::Int(resolved_provenance(adapter, node).elided as i64)),
        _ => None,
    }
}

/// A root-relative path to `node`: one token per level from `token`
/// (a name, a 1-based index, or an id), `/`-joined with a leading
/// slash. The unnamed root contributes no segment, so the path starts
/// at its first named descendant, matching the adapter locators.
pub(crate) fn ancestor_path<A: AstAdapter>(
    adapter: &A,
    node: NodeId,
    token: impl Fn(&A, NodeId) -> Option<String>,
) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if adapter.parent(n).is_none() {
            break; // the root itself has no segment
        }
        if let Some(t) = token(adapter, n) {
            parts.push(t);
        }
        cur = adapter.parent(n);
    }
    parts.reverse();
    format!("/{}", parts.join("/"))
}

/// 1-based position of `node` among its parent's children.
pub(crate) fn index_of(adapter: &impl AstAdapter, node: NodeId) -> Option<usize> {
    let parent = adapter.parent(node)?;
    siblings_of(adapter, parent)
        .iter()
        .position(|&c| c == node)
        .map(|i| i + 1)
}

/// Distance of `node` from the root (root is depth 0).
pub(crate) fn depth_of(adapter: &impl AstAdapter, node: NodeId) -> usize {
    let mut depth = 0;
    let mut cur = adapter.parent(node);
    while let Some(p) = cur {
        depth += 1;
        cur = adapter.parent(p);
    }
    depth
}

/// Total number of proper descendants of `node`.
pub(crate) fn n_descendants(adapter: &impl AstAdapter, node: NodeId) -> usize {
    let mut n = 0;
    let mut stack = adapter.children(node);
    while let Some(c) = stack.pop() {
        n += 1;
        stack.extend(adapter.children(c));
    }
    n
}

/// Whether `node`'s name satisfies `matcher`. `Any` and the pattern
/// dot match any node, even an unnamed one (e.g. the root); the other
/// matchers require a name.
pub(crate) fn matches_name(adapter: &impl AstAdapter, node: NodeId, matcher: &Matcher) -> bool {
    match matcher {
        Matcher::Any | Matcher::Dot => true,
        // A literal name asks the adapter (per-node
        // aliases answer on every axis); patterns test the
        // canonical name only.
        Matcher::Name(n) => adapter.answers_to(node, n),
        _ => adapter
            .name(node)
            .is_some_and(|name| matcher.matches(&name)),
    }
}

/// The sibling of `node` at the given offset (`+1` next, `-1` prev).
pub(crate) fn sibling(adapter: &impl AstAdapter, node: NodeId, offset: isize) -> Option<NodeId> {
    let parent = adapter.parent(node)?;
    let sibs = siblings_of(adapter, parent);
    let i = sibs.iter().position(|&s| s == node)? as isize + offset;
    if i < 0 {
        return None;
    }
    sibs.get(i as usize).copied()
}

/// The children of `parent`, memoized for the query.
pub(crate) fn siblings_of(adapter: &impl AstAdapter, parent: NodeId) -> std::rc::Rc<Vec<NodeId>> {
    if let Some(v) = with_graph_run(|s| s.children.get(&parent).cloned()) {
        return v;
    }
    let v = std::rc::Rc::new(adapter.children(parent));
    with_graph_run(|s| {
        // A bound on the memo, as on the regex memo: a query that
        // touches every parent of a huge tree must not keep every
        // child list alive.
        if s.children.len() >= 65_536 {
            s.children.clear();
        }
        s.children.insert(parent, v.clone());
    });
    v
}

/// Reduce depth-tagged matches to the requested reach.
pub(crate) fn by_reach(matches: Vec<(NodeId, usize)>, reach: Reach) -> Vec<NodeId> {
    let extreme = match reach {
        Reach::All => None,
        Reach::Proximal => matches.iter().map(|&(_, d)| d).min(),
        Reach::Distal => matches.iter().map(|&(_, d)| d).max(),
    };
    matches
        .into_iter()
        .filter(|&(_, d)| extreme.is_none_or(|e| d == e))
        .map(|(n, _)| n)
        .collect()
}

/// Deduplicate while preserving first-seen order.
pub(crate) fn dedup(nodes: Vec<NodeId>) -> Vec<NodeId> {
    let mut seen = HashSet::new();
    nodes.into_iter().filter(|n| seen.insert(*n)).collect()
}

/// The witness a scope evaluates under: the one a correlation search
/// tries, else the thread's own.
pub(crate) fn witness_of<'a>(scope: &Scope<'a, GraphExt>) -> &'a [Option<NodeId>] {
    match scope.witness {
        Some(w) => w,
        None => &scope.ext.bindings,
    }
}
