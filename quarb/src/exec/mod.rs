//! Executor: evaluate a [`Query`] against an [`AstAdapter`].
//!
//! Navigation maps a node context step by step. Once a projection
//! runs, the context becomes a list of *capsae* — each a node with a
//! register (breadcrumbs) and a current topic — that the pipeline
//! stages transform, aggregate, push, and recall.

use crate::adapter::{AstAdapter, InstantFrom, NodeId, Provenance, ProvenanceList};
use crate::ast::visit::{self, Visitor as _};
use crate::ast::{
    Anchor, Arg, ArithOp, Axis, Branch, CmpOp, FnCall, Group, InterpSeg, Matcher, Operand, PatSeg,
    PathElem, PredExpr, Predicate, Projection, PushBody, Query, Reach, RefProp, RegRef, Stage,
    Step,
};
use crate::stdlib;
use crate::value::{SortKey, Value};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

mod arith;
mod compare;
mod context;
mod correlate;
mod identity;
mod memo;
mod operand;
mod prov;
mod register;
mod run;
mod serialize;
mod stages;
mod steps;
#[cfg(test)]
mod tests;
mod text;

use arith::*;
use compare::*;
use context::*;
pub(crate) use correlate::*;
pub use identity::*;
use memo::*;
pub(crate) use operand::*;
pub use prov::*;
use register::*;
pub use run::*;
use serialize::*;
pub(crate) use stages::*;
pub use steps::*;
pub(crate) use text::*;

/// One entry in a capsa's register: the value, where it was read
/// (`prov`), and where it was pushed (`site`).
#[derive(Clone)]
pub(crate) struct Reg {
    name: Option<String>,
    value: Value,
    prov: Prov,
    /// Where the push happened — the coordinate a cross-capsa read
    /// will address; unread until that lands.
    #[allow(dead_code)]
    site: Origin,
}

/// One entry in a thread's mark array: a node pocketed during
/// navigation (`.name` — or bare `.` for an anonymous slot — in
/// node context), anchored on later by name (`(name)`), position
/// (`(N)`, 1-based), recency (`(.)`), or in the plural (`(@)`,
/// `(@name)`). Marks live beside the register — the register is
/// an array of scalars, marks an array of node handles; names in
/// both are references layered over the indices — and node
/// identity never enters the value space.
#[derive(Clone, PartialEq)]
pub(crate) struct Mark {
    name: Option<String>,
    node: NodeId,
}

/// The nodes an anchor starts navigation from, resolved against a
/// thread's mark array. Singular anchors yield at most one node;
/// the plural forms yield every match in push order (the caller
/// forks one thread per node); a miss yields nothing, never an
/// error.

/// The nodes a relative operand starts from: a
/// group's members when the capsa is a group and the anchor is
/// the current node, else the anchor's own resolution.
fn operand_from(
    adapter: &impl AstAdapter,
    anchor: &Anchor,
    current: NodeId,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Vec<NodeId> {
    if matches!(anchor, Anchor::Current) && !scope.members.is_empty() {
        return scope.members.iter().map(|m| m.node).collect();
    }
    anchor_nodes(adapter, anchor, current, trace, bound, scope)
}

fn anchor_nodes(
    adapter: &impl AstAdapter,
    anchor: &Anchor,
    current: NodeId,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Vec<NodeId> {
    let marks = scope.marks;
    match anchor {
        Anchor::Current => vec![current],
        Anchor::Root => vec![adapter.root()],
        // `((!expr))` — coordinates stood on again: every id the
        // expression yields (an integer, a coordinate record's
        // `id`, a list of either), deduplicated; an id the adapter
        // never minted is no node. The reads the anchor makes are
        // not origins.
        Anchor::Id(op) => {
            let vs = discard(|| eval_operand(adapter, current, op, trace, bound, scope));
            let mut ids: Vec<NodeId> = Vec::new();
            fn collect(v: &Value, out: &mut Vec<NodeId>) {
                match v {
                    Value::Int(n) if *n >= 0 => out.push(NodeId(*n as u64)),
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
        Anchor::Mark(m) => marks
            .iter()
            .rev()
            .find(|k| k.name.as_deref() == Some(m))
            .map(|k| k.node)
            .into_iter()
            .collect(),
        Anchor::MarkIndex(n) => marks.get(n - 1).map(|k| k.node).into_iter().collect(),
        Anchor::MarkTop => marks.last().map(|k| k.node).into_iter().collect(),
        Anchor::MarksAll => marks.iter().map(|k| k.node).collect(),
        Anchor::MarksNamed(m) => marks
            .iter()
            .filter(|k| k.name.as_deref() == Some(m))
            .map(|k| k.node)
            .collect(),
    }
}

/// The execution unit: a node, its register (breadcrumbs), and the
/// current topic (the scalar being worked on, once a projection has
/// run).
#[derive(Clone)]
pub(crate) struct Capsa {
    pub(crate) node: NodeId,
    pub(crate) register: Vec<Reg>,
    pub(crate) topic: Option<Value>,
    /// Where the topic's value was read (per field for a record).
    prov: Prov,
    /// A group's member capsae (empty everywhere else). Carried by
    /// `@| group` alongside the list topic so keyed aggregates on
    /// `|` can work per group and `@| ungroup` can flatten back.
    members: Vec<Capsa>,
    /// The groups of the last successful `=~` match in a filter
    /// stage (`$1` … `$9`); empty until one matches.
    captures: Vec<String>,
    /// The named groups of that match (`%+`), as `(name, text)`.
    named: Vec<(String, String)>,
    /// The thread's marks — nodes labeled on the walk, for the
    /// `(name)` anchor.
    marks: Vec<Mark>,
    /// The correlation witness that admitted this capsa (one node
    /// per trace context; null for an outer context no tuple
    /// admitted), for `$*k` in pipeline stages.
    bindings: Vec<Option<NodeId>>,
    /// The edges that reached this node on the walk's final hop —
    /// the `@-` operand, aggregated across merged crossings.
    arrived: Vec<EdgeCtx>,
}

impl Capsa {
    /// The scope a pipeline stage evaluates this capsa's expressions
    /// in: its topic, register, members, captures, marks, witness
    /// and arrivals, at `ordinal` (1-based) in the stage's input,
    /// with the stage's snapshot and the invoking scope.
    fn scope<'a>(
        &'a self,
        ordinal: Option<usize>,
        peers: Option<&'a Peers>,
        outer: Option<&'a Scope<'a>>,
    ) -> Scope<'a> {
        Scope {
            prov: Some(&self.prov),
            node: Some(self.node),
            register: &self.register,
            members: &self.members,
            topic: self.topic.as_ref(),
            ordinal,
            captures: &self.captures,
            named: &self.named,
            marks: &self.marks,
            bindings: &self.bindings,
            edge: None,
            arrived: &self.arrived,
            peers,
            outer,
        }
    }

    /// A bare capsa standing at `node` with `topic` — what the
    /// tracer hands a pipeline stage when it replays one thread.
    pub(crate) fn bare(node: NodeId, topic: Option<Value>) -> Capsa {
        Capsa {
            node,
            register: Vec::new(),
            topic,
            prov: Prov::default(),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            marks: Vec::new(),
            bindings: Vec::new(),
            arrived: Vec::new(),
        }
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
const CHILD: &str = "[child]";
const PARENT: &str = "[parent]";
const NEXT: &str = "[next]";
const PREV: &str = "[prev]";

/// A stage's input context as its expressions see it (`@*`, the
/// cross-capsa read): the snapshot, and an index of who stands
/// where, built on first use — `X:.r` over a big context must not
/// scan every peer per read.
struct Peers {
    caps: Vec<Capsa>,
    by_node: std::cell::OnceCell<HashMap<NodeId, Vec<usize>>>,
}

impl Peers {
    fn new(caps: Vec<Capsa>) -> Peers {
        Peers {
            caps,
            by_node: std::cell::OnceCell::new(),
        }
    }

    /// The peers standing at `node`, in context order.
    fn at(&self, node: NodeId) -> &[usize] {
        let index = self.by_node.get_or_init(|| {
            let mut m: HashMap<NodeId, Vec<usize>> = HashMap::new();
            for (i, c) in self.caps.iter().enumerate() {
                m.entry(c.node).or_default().push(i);
            }
            m
        });
        index.get(&node).map_or(&[], |v| v.as_slice())
    }
}

/// The capsa scope a value expression may reach: the register (for
/// `$.name` recalls) and the topic (`$_`). Empty where no capsa
/// exists yet (navigation predicates).
#[derive(Clone, Copy)]
pub(crate) struct Scope<'a> {
    register: &'a [Reg],
    topic: Option<&'a Value>,
    /// The topic's provenance, where the scope belongs to a capsa.
    prov: Option<&'a Prov>,
    /// 1-based position in the current context, when known.
    ordinal: Option<usize>,
    /// Regex captures from the last matching filter (`$1` …).
    captures: &'a [String],
    /// The named groups of the last matching filter (`%+`).
    named: &'a [(String, String)],
    /// The marks in scope, for `(name)`-anchored paths.
    marks: &'a [Mark],
    /// The correlation witness that admitted this capsa (one node
    /// per trace context; null for an unmatched outer context),
    /// read back by `$*k` in pipeline stages.
    bindings: &'a [Option<NodeId>],
    /// A group's members: the rows the capsa stands
    /// for. A relative operand from a group reads every member's
    /// node, as `:field` reads every member's record; empty on an
    /// ordinary capsa.
    members: &'a [Capsa],
    /// The arrived-by edge (`$-`), where one is defined.
    edge: Option<&'a EdgeCtx>,
    /// All final-hop crossings (`@-`), for pipeline scopes.
    arrived: &'a [EdgeCtx],
    /// The stage's input context (`@*`), where one exists — the
    /// snapshot rule: a stage is the transition, so "the context"
    /// during its evaluation is what it received.
    peers: Option<&'a Peers>,
    /// The invoking capsa's scope, one subcontext out (`$.name`,
    /// `$$_`, `$$ord`); `None` at the top level.
    outer: Option<&'a Scope<'a>>,
    /// The capsa's own node, where the scope belongs to a capsa —
    /// what a `$$`-wrapped node operand (`_::prop`, `_/child::x`)
    /// reads one scope out. `None` for navigation scopes.
    node: Option<NodeId>,
}

const NO_SCOPE: Scope<'static> = Scope {
    prov: None,
    register: &[],
    members: &[],
    topic: None,
    ordinal: None,
    captures: &[],
    named: &[],
    marks: &[],
    bindings: &[],
    edge: None,
    arrived: &[],
    peers: None,
    outer: None,
    node: None,
};

/// The result of evaluating a query: a node set, or\,---\,after a
/// projection\,---\,a list of scalar values (one per node).
#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    /// Nodes selected by navigation (no projection).
    Nodes(Vec<NodeId>),
    /// Scalars produced by a projection.
    Values(Vec<Value>),
}

/// A correlation trace: the node contexts of prior `<=>` expressions,
/// available to predicates as `$*1`, `$*2`, … — plus the witness
/// map: for each node a correlation predicate admitted, the tuple
/// of bound nodes that satisfied it (the *witness*), which the
/// pipeline's `$*k` operands read back.
#[derive(Default)]
pub(crate) struct Correlation {
    /// Per-context outer flags (`<=>?`): an outer context may bind
    /// null when no real tuple admits a row.
    outer: Vec<bool>,
    contexts: Vec<Vec<NodeId>>,
    witnesses: std::cell::RefCell<std::collections::HashMap<u64, Vec<Option<NodeId>>>>,
    /// Per joined context and property: the context's nodes by the
    /// equality key of that property — the hash side of a join
    /// whose ON holds `::p = _::q`, built on first use.
    join_keys:
        std::cell::RefCell<std::collections::HashMap<(usize, String), std::rc::Rc<ValueKeys>>>,
}

pub(crate) type Trace = Correlation;

pub fn eval(query: &Query, adapter: &impl AstAdapter) -> QueryResult {
    eval_with(query, adapter, None)
}

/// [`eval`] under `options` (`None`: the enclosing run's, or the
/// defaults at the top).
pub(crate) fn eval_with(
    query: &Query,
    adapter: &impl AstAdapter,
    options: Option<RunOptions>,
) -> QueryResult {
    let _run = begin_run(options);
    eval_query(query, adapter, adapter.root(), &Correlation::default())
}

/// [`eval_traced`] with each value's provenance: where its topic —
/// per field, for a record — was read.
#[cfg(test)]
pub fn eval_traced_prov(query: &Query, adapter: &impl AstAdapter) -> Vec<Traced> {
    eval_traced_prov_with(query, adapter, None)
}

/// [`eval_traced_prov`] under `options`.
pub(crate) fn eval_traced_prov_with(
    query: &Query,
    adapter: &impl AstAdapter,
    options: Option<RunOptions>,
) -> Vec<Traced> {
    let _run = begin_run(options);
    let (caps, projected) =
        eval_query_caps(query, adapter, adapter.root(), &Correlation::default());
    caps.into_iter()
        .map(|c| {
            let topic = if projected {
                Some(c.topic.unwrap_or(Value::Null))
            } else {
                c.topic
            };
            Traced {
                node: c.node,
                topic,
                prov: c.prov,
            }
        })
        .collect()
}

/// Engine-computed core metadata (`:::key`), derived from the
/// navigation surface and independent of the adapter's domain.
fn core_meta(adapter: &impl AstAdapter, node: NodeId, key: &str) -> Option<Value> {
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
        // coordinates. A node's origin is itself, at stage 0.
        "%provenance" => Some(prov_record(&resolved_provenance(adapter, node).first())),
        "@provenance" => Some(Value::list(
            resolved_provenance(adapter, node)
                .entries
                .iter()
                .map(prov_record)
                .collect(),
        )),
        "origin" => Some(Value::Int(node.0 as i64)),
        "%origin" => Some(origin_record(Origin { node, stage: 0 })),
        "@origin" => Some(Value::list(vec![origin_record(Origin { node, stage: 0 })])),
        "elided" => Some(Value::Int(resolved_provenance(adapter, node).elided as i64)),
        _ => None,
    }
}

/// A root-relative path to `node`: one token per level from `token`
/// (a name, a 1-based index, or an id), `/`-joined with a leading
/// slash. The unnamed root contributes no segment, so the path starts
/// at its first named descendant, matching the adapter locators.
fn ancestor_path<A: AstAdapter>(
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
fn index_of(adapter: &impl AstAdapter, node: NodeId) -> Option<usize> {
    let parent = adapter.parent(node)?;
    siblings_of(adapter, parent)
        .iter()
        .position(|&c| c == node)
        .map(|i| i + 1)
}

/// Distance of `node` from the root (root is depth 0).
fn depth_of(adapter: &impl AstAdapter, node: NodeId) -> usize {
    let mut depth = 0;
    let mut cur = adapter.parent(node);
    while let Some(p) = cur {
        depth += 1;
        cur = adapter.parent(p);
    }
    depth
}

/// Total number of proper descendants of `node`.
fn n_descendants(adapter: &impl AstAdapter, node: NodeId) -> usize {
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
fn matches_name(adapter: &impl AstAdapter, node: NodeId, matcher: &Matcher) -> bool {
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
fn sibling(adapter: &impl AstAdapter, node: NodeId, offset: isize) -> Option<NodeId> {
    let parent = adapter.parent(node)?;
    let sibs = siblings_of(adapter, parent);
    let i = sibs.iter().position(|&s| s == node)? as isize + offset;
    if i < 0 {
        return None;
    }
    sibs.get(i as usize).copied()
}

/// The children of `parent`, memoized for the query.
fn siblings_of(adapter: &impl AstAdapter, parent: NodeId) -> std::rc::Rc<Vec<NodeId>> {
    if let Some(v) = with_run(|s| s.children.get(&parent).cloned()) {
        return v;
    }
    let v = std::rc::Rc::new(adapter.children(parent));
    with_run(|s| {
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
fn by_reach(matches: Vec<(NodeId, usize)>, reach: Reach) -> Vec<NodeId> {
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
fn dedup(nodes: Vec<NodeId>) -> Vec<NodeId> {
    let mut seen = HashSet::new();
    nodes.into_iter().filter(|n| seen.insert(*n)).collect()
}
