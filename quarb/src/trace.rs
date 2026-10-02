//! Tracing: the `quarb/trace` payload for a query — every evaluation
//! snapshot, the threads, the edges they walked (ribbons), and the
//! next hop's candidates with verdicts (lookahead).
//!
//! The trace is built by *replaying* the query one hop per thread
//! with the evaluator's own navigation ([`exec::navigate_paths`] for
//! a hop, [`exec::apply_stage`] for a pipeline stage), so nothing in
//! the evaluator changes and the payload cannot disagree with it in
//! kind — and it is checked against a plain evaluation at the end,
//! so it cannot disagree in fact either. The corpus in the
//! quarbopsis repo is the acceptance surface.
//!
//! Scope (what the corpus needed): one branch, no correlations, no
//! marks or pushes; hops on every axis, one-alternative groups
//! (quantified walks), projections, per-capsa pipeline stages, whole-
//! context aggregates. Anything else is [`QuarbError::Unsupported`].

use std::collections::{HashMap, HashSet};

use quarb_trace as qt;

use crate::adapter::{AstAdapter, NodeId};
use crate::ast::{
    Anchor, Axis, Group, Matcher, PathElem, Predicate, Projection, Query, Stage, Step,
};
use crate::error::{QuarbError, Result};
use crate::exec::{self, Correlation};
use crate::value::Value;
use crate::{Defs, lexer, parser, unparse};

/// What the caller decides: the meta identity, the window over the
/// working context, and which stub measures to compute.
#[derive(Debug, Clone)]
pub struct Options {
    pub adapter: String,
    pub source: String,
    /// An explicit window (1-based, inclusive) over the working
    /// context; otherwise the first page.
    pub window: Option<(usize, usize)>,
    pub page: usize,
    /// Stub measures to compute: nodes, children, size.
    pub measures: Vec<String>,
    /// Embed the whole arbor when it has at most this many nodes
    /// and no window was requested.
    pub full_arbor_limit: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            adapter: "unknown".into(),
            source: String::new(),
            window: None,
            page: 10,
            measures: Vec::new(),
            full_arbor_limit: 5000,
        }
    }
}

fn unsupported(what: impl Into<String>) -> QuarbError {
    QuarbError::Unsupported(format!("trace: {}", what.into()))
}

// ---------------------------------------------------------------
// The plan: the query as a flat step list.

enum PKind {
    Hop(Step),
    Group(Group),
    Projection(Projection),
    Stage(Stage),
}

struct PStep {
    kind: PKind,
    expr: String,
}

fn parse<A: AstAdapter>(text: &str, adapter: &A) -> Result<Query> {
    let tokens = lexer::lex(text)?;
    parser::parse_with_data(&tokens, Defs::default(), Some(adapter))
}

fn plan(q: &Query) -> Result<Vec<PStep>> {
    if !q.correlations.is_empty() {
        return Err(unsupported("correlated queries (<=>)"));
    }
    let [branch] = q.branches.as_slice() else {
        return Err(unsupported("multi-branch queries"));
    };
    if !matches!(branch.anchor, Anchor::Root | Anchor::Current) {
        return Err(unsupported("anchored branches"));
    }
    let mut out = Vec::new();
    for e in &branch.steps {
        match e {
            PathElem::Step(s) => out.push(PStep {
                kind: PKind::Hop(s.clone()),
                expr: unparse::step(s),
            }),
            PathElem::Group(g) => out.push(PStep {
                kind: PKind::Group(g.clone()),
                expr: unparse::group(g),
            }),
            PathElem::Mark(_) => return Err(unsupported("marks")),
            PathElem::Push { .. } => return Err(unsupported("pushes")),
        }
    }
    if let Some(p) = &branch.projection {
        out.push(PStep {
            kind: PKind::Projection(p.clone()),
            expr: unparse::projection(p),
        });
    }
    for st in &q.pipeline {
        out.push(PStep {
            kind: PKind::Stage(st.clone()),
            expr: unparse::stage(st),
        });
    }
    Ok(out)
}

fn axis_text(a: &Axis) -> &'static str {
    match a {
        Axis::Child => "/",
        Axis::Descendant(_) => "//",
        Axis::Parent => "\\",
        Axis::Ancestor(_) => "\\\\",
        Axis::NextSibling => ">",
        Axis::PrevSibling => "<",
        Axis::FollowingSiblings(_) => ">>",
        Axis::PrecedingSiblings(_) => "<<",
        Axis::OutLink => "->",
        Axis::InLink => "<-",
        Axis::BothLink => "--",
        Axis::Resolve { .. } => "-->",
        Axis::ReverseResolve { .. } => "<--",
    }
}

fn matcher_kind(m: &Matcher) -> &'static str {
    match m {
        Matcher::Name(_) => "name",
        Matcher::Glob(_) => "glob",
        Matcher::Regex(_) => "regex",
        Matcher::Any | Matcher::Dot => "any",
    }
}

/// The predicates of a step as one text: the bracket bodies,
/// space-joined (`[a][b]` → `a b`).
fn predicates_text(ps: &[Predicate]) -> Option<String> {
    if ps.is_empty() {
        return None;
    }
    let parts: Vec<String> = ps
        .iter()
        .map(|p| {
            let t = unparse::predicate(p);
            t.strip_prefix('[')
                .and_then(|t| t.strip_suffix(']'))
                .unwrap_or(&t)
                .to_string()
        })
        .collect();
    Some(parts.join(" "))
}

fn step_record(i: usize, p: &PStep, staged: bool) -> qt::Step {
    let mut r = qt::Step {
        step: i as i64 + 1,
        expr: p.expr.clone(),
        staged,
        ..Default::default()
    };
    match &p.kind {
        PKind::Hop(s) => {
            r.kind = "hop".into();
            r.axis = Some(axis_text(&s.axis).into());
            r.matcher = Some(unparse::matcher(&s.matcher));
            r.matcherkind = Some(matcher_kind(&s.matcher).into());
            r.predicate = predicates_text(&s.predicates);
        }
        PKind::Group(g) => {
            r.kind = "group".into();
            r.body = Some(
                g.alts
                    .iter()
                    .map(|alt| alt.iter().map(unparse::elem).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("|"),
            );
            r.min = Some(g.quant.min as i64);
            r.max = g.quant.max.map(|m| m as i64);
        }
        PKind::Projection(p) => {
            r.kind = "projection".into();
            let (name, kind) = match p {
                Projection::Property(Some(n)) => (n.clone(), "property"),
                Projection::Property(None) => (String::new(), "default"),
                Projection::CoreMeta(n) => (n.clone(), "coremeta"),
                Projection::AdapterMeta(n) => (n.clone(), "adaptermeta"),
            };
            r.projection = Some(name);
            r.projectionkind = Some(kind.into());
        }
        PKind::Stage(st) => match st {
            Stage::Filter(e) => {
                r.kind = "filter".into();
                r.predicate = Some(unparse::pred_expr(e));
            }
            Stage::Agg(call) => {
                r.kind = "agg".into();
                r.name = Some(call.name.clone());
            }
            Stage::Func(call) => {
                r.kind = "stage".into();
                r.name = Some(call.name.clone());
            }
            _ => r.kind = "stage".into(),
        },
    }
    r
}

// ---------------------------------------------------------------
// Threads and their walked edges.

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum EdgeKey {
    Tree(NodeId, NodeId),
    Link(NodeId, String, NodeId),
}

/// A walked edge and whether it was walked from its `to` end.
type Walked = (EdgeKey, bool);

#[derive(Clone)]
struct Th {
    id: String,
    /// None for a positionless aggregate.
    node: Option<NodeId>,
    topic: Option<Value>,
    edges: Vec<Walked>,
}

struct ThreadRec {
    id: String,
    parent: Option<String>,
    born: i64,
    ended: Option<i64>,
    fate: &'static str,
}

struct Snap {
    at: i64,
    iter: Option<i64>,
    live: Vec<Th>,
}

struct Look {
    at: i64,
    iter: Option<i64>,
    thread: String,
    node: NodeId,
    matched: bool,
    ordinal: i64,
}

struct Agg {
    id: String,
    at: i64,
    name: String,
    members: Vec<String>,
}

struct Tracer<'a, A: AstAdapter> {
    adapter: &'a A,
    threads: Vec<ThreadRec>,
    snaps: Vec<Snap>,
    look: Vec<Look>,
    groups: Vec<Agg>,
}

impl<'a, A: AstAdapter> Tracer<'a, A> {
    fn new_thread(&mut self, id: &str, parent: Option<&str>, born: i64) {
        self.threads.push(ThreadRec {
            id: id.to_string(),
            parent: parent.map(str::to_string),
            born,
            ended: None,
            fate: "live",
        });
    }

    fn end(&mut self, id: &str, at: i64, fate: &'static str) {
        if let Some(t) = self.threads.iter_mut().find(|t| t.id == id) {
            t.ended = Some(at);
            t.fate = fate;
        }
    }

    /// One element navigated from `from`: the landings with the
    /// edges that reached each.
    fn nav(&self, from: NodeId, elem: &PathElem) -> Vec<(NodeId, Vec<exec::EdgeCtx>)> {
        exec::navigate_paths(
            std::slice::from_ref(elem),
            self.adapter,
            from,
            &Correlation::default(),
            None,
            &[],
            &[],
            &[],
        )
        .into_iter()
        .map(|(n, _reg, _marks, arrived)| (n, arrived))
        .collect()
    }

    /// The tree path from `top` down to `bottom` as walked edges.
    fn tree_path(&self, top: NodeId, bottom: NodeId, against: bool) -> Vec<Walked> {
        let mut path = Vec::new();
        let mut cur = bottom;
        while cur != top {
            let Some(p) = self.adapter.parent(cur) else {
                break;
            };
            path.push((EdgeKey::Tree(p, cur), against));
            cur = p;
        }
        path.reverse();
        path
    }

    /// The edges a hop from `from` to `to` contributes to a thread's
    /// history (NOTES 002: walked traversal history, not root paths).
    fn hop_edges(
        &self,
        from: NodeId,
        to: NodeId,
        step: &Step,
        arrived: &[exec::EdgeCtx],
    ) -> Vec<Walked> {
        match &step.axis {
            Axis::Child | Axis::Descendant(_) => self.tree_path(from, to, false),
            Axis::Parent | Axis::Ancestor(_) => self.tree_path(to, from, true),
            Axis::NextSibling
            | Axis::PrevSibling
            | Axis::FollowingSiblings(_)
            | Axis::PrecedingSiblings(_) => match self.adapter.parent(to) {
                Some(p) => vec![(EdgeKey::Tree(p, to), false)],
                None => Vec::new(),
            },
            // The evaluator records a crosslink in its STORED
            // direction whichever way it was walked; `against` is
            // walking it from its `to` end.
            Axis::OutLink | Axis::Resolve { .. } => arrived
                .iter()
                .map(|e| (EdgeKey::Link(e.source, e.label.clone(), e.target), false))
                .collect(),
            Axis::InLink | Axis::ReverseResolve { .. } => arrived
                .iter()
                .map(|e| (EdgeKey::Link(e.source, e.label.clone(), e.target), true))
                .collect(),
            Axis::BothLink => arrived
                .iter()
                .map(|e| {
                    (
                        EdgeKey::Link(e.source, e.label.clone(), e.target),
                        e.target == from,
                    )
                })
                .collect(),
        }
    }

    /// Place a thread in the next context, merging into an earlier
    /// thread standing on the same node (the evaluator's dedup).
    fn place(&mut self, at: i64, next: &mut Vec<Th>, seen: &mut HashMap<NodeId, usize>, th: Th) {
        let Some(node) = th.node else {
            next.push(th);
            return;
        };
        match seen.get(&node) {
            Some(&i) => {
                for e in th.edges {
                    if !next[i].edges.contains(&e) {
                        next[i].edges.push(e);
                    }
                }
                self.end(&th.id, at, "merged");
            }
            None => {
                seen.insert(node, next.len());
                next.push(th);
            }
        }
    }

    fn union_edges(mut base: Vec<Walked>, more: Vec<Walked>) -> Vec<Walked> {
        for e in more {
            if !base.contains(&e) {
                base.push(e);
            }
        }
        base
    }

    /// A hop: every live thread forks over its candidates.
    fn hop(&mut self, at: i64, prev: &[Th], step: &Step) -> Result<Vec<Th>> {
        let bare = PathElem::Step(Step {
            predicates: Vec::new(),
            ..step.clone()
        });
        let full = PathElem::Step(step.clone());
        let mut next = Vec::new();
        let mut seen = HashMap::new();
        let mut ordinal = 0i64;
        for th in prev {
            let Some(u) = th.node else {
                return Err(unsupported("a hop after an aggregate"));
            };
            let cands = self.nav(u, &bare);
            let surv: Vec<NodeId> = self.nav(u, &full).into_iter().map(|(n, _)| n).collect();
            for (n, _) in &cands {
                ordinal += 1;
                self.look.push(Look {
                    at: at - 1,
                    iter: None,
                    thread: th.id.clone(),
                    node: *n,
                    matched: surv.contains(n),
                    ordinal,
                });
            }
            let m = cands.len();
            if m == 0 {
                self.end(&th.id, at, "exhausted");
                continue;
            }
            if m == 1 {
                let (n, arrived) = &cands[0];
                if !surv.contains(n) {
                    self.end(&th.id, at, "filtered");
                    continue;
                }
                let edges =
                    Self::union_edges(th.edges.clone(), self.hop_edges(u, *n, step, arrived));
                let child = Th {
                    id: th.id.clone(),
                    node: Some(*n),
                    topic: None,
                    edges,
                };
                self.place(at, &mut next, &mut seen, child);
                continue;
            }
            self.end(&th.id, at, "forked");
            for (i, (n, arrived)) in cands.iter().enumerate() {
                let id = format!("{}.{}", th.id, i + 1);
                self.new_thread(&id, Some(&th.id), at);
                if !surv.contains(n) {
                    self.end(&id, at, "filtered");
                    continue;
                }
                let edges =
                    Self::union_edges(th.edges.clone(), self.hop_edges(u, *n, step, arrived));
                let child = Th {
                    id,
                    node: Some(*n),
                    topic: None,
                    edges,
                };
                self.place(at, &mut next, &mut seen, child);
            }
        }
        Ok(next)
    }

    /// A quantified walk: the film inside one step — one frame per
    /// iteration, each iteration emitting a thread per node reached
    /// at that depth (NOTES 004).
    fn group(&mut self, at: i64, prev: &[Th], g: &Group) -> Result<Vec<Th>> {
        let [body] = g.alts.as_slice() else {
            return Err(unsupported("group alternatives"));
        };
        if !g.predicates.is_empty() {
            return Err(unsupported("group predicates"));
        }
        if g.quant.min > 1 {
            return Err(unsupported("group quantifier minimum above 1"));
        }
        let steps: Vec<&Step> = body
            .iter()
            .map(|e| match e {
                PathElem::Step(s) => Ok(s),
                _ => Err(unsupported("non-hop group bodies")),
            })
            .collect::<Result<_>>()?;
        let bound = self.adapter.quantifier_bound();
        // Per driver: its frontier, its visited set, its emissions.
        struct Walk {
            driver: Th,
            frontier: Vec<(NodeId, Vec<Walked>)>,
            visited: HashSet<NodeId>,
            emitted: usize,
        }
        let mut walks: Vec<Walk> = Vec::new();
        for th in prev {
            let Some(u) = th.node else {
                return Err(unsupported("a walk after an aggregate"));
            };
            walks.push(Walk {
                driver: th.clone(),
                frontier: vec![(u, th.edges.clone())],
                visited: HashSet::from([u]),
                emitted: 0,
            });
        }
        let mut emitted: Vec<Th> = Vec::new();
        let mut iter = 0i64;
        // `*` emits the origin itself first, as depth 0.
        if g.quant.min == 0 {
            for w in walks.iter_mut() {
                w.emitted += 1;
                let id = format!("{}.{}", w.driver.id, w.emitted);
                self.new_thread(&id, Some(&w.driver.id), at);
                emitted.push(Th {
                    id,
                    node: w.driver.node,
                    topic: None,
                    edges: w.driver.edges.clone(),
                });
            }
            self.snaps.push(Snap {
                at,
                iter: Some(0),
                live: emitted.clone(),
            });
        }
        loop {
            if let Some(max) = g.quant.max
                && iter as usize >= max
            {
                break;
            }
            if iter as usize >= bound {
                break;
            }
            // The next depth, per driver, in context order.
            let mut any = false;
            let mut per_walk: Vec<Vec<(NodeId, Vec<Walked>)>> = Vec::new();
            for w in walks.iter_mut() {
                let mut next = Vec::new();
                for (n, edges) in &w.frontier {
                    // The body, hop by hop.
                    let mut cur: Vec<(NodeId, Vec<Walked>)> = vec![(*n, edges.clone())];
                    for s in &steps {
                        let elem = PathElem::Step((*s).clone());
                        let mut out = Vec::new();
                        for (c, e) in &cur {
                            for (landing, arrived) in self.nav(*c, &elem) {
                                let more = self.hop_edges(*c, landing, s, &arrived);
                                out.push((landing, Self::union_edges(e.clone(), more)));
                            }
                        }
                        cur = out;
                    }
                    for (landing, e) in cur {
                        if w.visited.insert(landing) {
                            next.push((landing, e));
                        }
                    }
                }
                any |= !next.is_empty();
                per_walk.push(next);
            }
            if !any {
                break;
            }
            // Lookahead: the coming depth, credited to the driver,
            // from the previous frame.
            let (la, li) = if iter == 0 {
                (at - 1, None)
            } else {
                (at, Some(iter))
            };
            let mut ordinal = 0i64;
            for (w, next) in walks.iter().zip(&per_walk) {
                for (n, _) in next {
                    ordinal += 1;
                    self.look.push(Look {
                        at: la,
                        iter: li,
                        thread: w.driver.id.clone(),
                        node: *n,
                        matched: true,
                        ordinal,
                    });
                }
            }
            iter += 1;
            for (w, next) in walks.iter_mut().zip(per_walk) {
                for (n, e) in &next {
                    w.emitted += 1;
                    let id = format!("{}.{}", w.driver.id, w.emitted);
                    self.new_thread(&id, Some(&w.driver.id), at);
                    emitted.push(Th {
                        id,
                        node: Some(*n),
                        topic: None,
                        edges: e.clone(),
                    });
                }
                w.frontier = next;
            }
            self.snaps.push(Snap {
                at,
                iter: Some(iter),
                live: emitted.clone(),
            });
        }
        for w in &walks {
            let fate = if w.emitted > 0 { "forked" } else { "exhausted" };
            self.end(&w.driver.id, at, fate);
        }
        Ok(emitted)
    }

    fn projection(&mut self, prev: &[Th], p: &Projection) -> Result<Vec<Th>> {
        let mut next = Vec::new();
        for th in prev {
            let Some(u) = th.node else {
                return Err(unsupported("a projection after an aggregate"));
            };
            next.push(Th {
                topic: Some(exec::project(self.adapter, u, p)),
                ..th.clone()
            });
        }
        Ok(next)
    }

    fn stage(&mut self, at: i64, prev: &[Th], st: &Stage) -> Result<Vec<Th>> {
        let trace = Correlation::default();
        match st {
            Stage::Filter(_) => {
                let mut next = Vec::new();
                for (i, th) in prev.iter().enumerate() {
                    let Some(u) = th.node else {
                        return Err(unsupported("a filter after an aggregate"));
                    };
                    let caps = vec![exec::Capsa::bare(u, th.topic.clone())];
                    let out = exec::apply_stage(st, caps, self.adapter, &trace, None);
                    let kept = !out.is_empty();
                    self.look.push(Look {
                        at: at - 1,
                        iter: None,
                        thread: th.id.clone(),
                        node: u,
                        matched: kept,
                        ordinal: i as i64 + 1,
                    });
                    if kept {
                        next.push(Th {
                            topic: out.into_iter().next().and_then(|c| c.topic),
                            ..th.clone()
                        });
                    } else {
                        self.end(&th.id, at, "filtered");
                    }
                }
                Ok(next)
            }
            Stage::Agg(call) => {
                let caps: Vec<exec::Capsa> = prev
                    .iter()
                    .map(|th| match th.node {
                        Some(u) => Ok(exec::Capsa::bare(u, th.topic.clone())),
                        None => Err(unsupported("an aggregate of an aggregate")),
                    })
                    .collect::<Result<_>>()?;
                let out = exec::apply_stage(st, caps, self.adapter, &trace, None);
                if out.len() != 1 {
                    return Err(unsupported(format!(
                        "aggregate `{}` yielding {} capsae",
                        call.name,
                        out.len()
                    )));
                }
                let id = format!("a{}", self.groups.len() + 1);
                let members: Vec<String> = prev.iter().map(|t| t.id.clone()).collect();
                for m in &members {
                    self.end(m, at, "aggregated");
                }
                self.groups.push(Agg {
                    id: id.clone(),
                    at,
                    name: call.name.clone(),
                    members,
                });
                let topic = out.into_iter().next().and_then(|c| c.topic);
                Ok(vec![Th {
                    id,
                    node: None,
                    topic,
                    edges: Vec::new(),
                }])
            }
            other => {
                // A per-capsa transform: one in, one out.
                let mut next = Vec::new();
                for th in prev {
                    let Some(u) = th.node else {
                        return Err(unsupported("a stage after an aggregate"));
                    };
                    let caps = vec![exec::Capsa::bare(u, th.topic.clone())];
                    let out = exec::apply_stage(other, caps, self.adapter, &trace, None);
                    match out.len() {
                        0 => self.end(&th.id, at, "filtered"),
                        1 => next.push(Th {
                            topic: out.into_iter().next().and_then(|c| c.topic),
                            ..th.clone()
                        }),
                        n => {
                            return Err(unsupported(format!(
                                "a stage forking one thread into {n}"
                            )));
                        }
                    }
                }
                Ok(next)
            }
        }
    }
}

// ---------------------------------------------------------------
// Scalars, node properties, node kinds.

fn scalar(v: &Value) -> qt::Scalar {
    match v {
        Value::Null => qt::Scalar::Null,
        Value::Bool(b) => qt::Scalar::Bool(*b),
        Value::Int(i) => qt::Scalar::Int(*i),
        Value::Float(f) => qt::Scalar::Float(*f),
        Value::Str(s) => qt::Scalar::Str(s.clone()),
        other => qt::Scalar::Str(other.to_string()),
    }
}

/// The node's kind. The adapter trait has no kind surface yet, so
/// this reads the adapter family off its name; the contract leaves
/// `kind` adapter-defined.
fn node_kind<A: AstAdapter>(adapter: &A, family: &str, node: NodeId, depth: usize) -> String {
    let kids = adapter.children(node);
    match family {
        "html" | "xml" => {
            // The unnamed node above the root element is the document.
            if adapter.name(node).is_none() {
                "document".into()
            } else {
                "element".into()
            }
        }
        "sqlite" | "csv" | "xlsx" | "duckdb" | "relational" => match depth {
            0 => "database".into(),
            1 => "table".into(),
            2 => "row".into(),
            _ => "cell".into(),
        },
        "json" | "yaml" | "toml" | "kaiv" => {
            if kids.is_empty() {
                "value".into()
            } else if kids
                .iter()
                .enumerate()
                .all(|(i, &c)| adapter.name(c).as_deref() == Some(i.to_string().as_str()))
            {
                "array".into()
            } else {
                "object".into()
            }
        }
        _ => "node".into(),
    }
}

/// Adapter-open properties for the arbor snapshot: an html element's
/// tag and attributes (its text when it has no element children); a
/// record-shaped node's leaf fields; a leaf's own value.
fn node_props<A: AstAdapter>(adapter: &A, family: &str, node: NodeId) -> Vec<(String, qt::Scalar)> {
    let mut props = Vec::new();
    let kids = adapter.children(node);
    // A property named like a declared field cannot ride the record.
    let clash = |k: &str| matches!(k, "node" | "kind");
    if family == "html" {
        if let Some(Value::Str(tag)) = adapter.metadata(node, "tag") {
            props.push(("tag".to_string(), qt::Scalar::Str(tag)));
        }
        if let Some(Value::List(names)) = adapter.metadata(node, "attrs") {
            for n in names.iter() {
                let Value::Str(name) = n else { continue };
                if clash(name) {
                    continue;
                }
                if let Some(v) = adapter.property(node, name) {
                    props.push((name.clone(), scalar(&v)));
                }
            }
        }
        if kids.is_empty()
            && let Some(Value::Str(text)) = adapter.default_value(node)
            && !text.trim().is_empty()
        {
            props.push(("text".to_string(), qt::Scalar::Str(text)));
        }
        return props;
    }
    for c in &kids {
        if adapter.children(*c).is_empty()
            && let Some(name) = adapter.name(*c)
            && !clash(&name)
        {
            let v = adapter.default_value(*c).unwrap_or(Value::Null);
            props.push((name, scalar(&v)));
        }
    }
    if kids.is_empty()
        && let Some(v) = adapter.default_value(node)
    {
        props.push(("value".to_string(), scalar(&v)));
    }
    props
}

fn subtree_count<A: AstAdapter>(adapter: &A, node: NodeId) -> i64 {
    1 + adapter
        .children(node)
        .into_iter()
        .map(|c| subtree_count(adapter, c))
        .sum::<i64>()
}

/// `size`: Unicode characters of a text-level node's text; absent
/// where the adapter defines no size.
fn size_of<A: AstAdapter>(adapter: &A, node: NodeId) -> Option<i64> {
    match adapter.default_value(node) {
        Some(Value::Str(s)) => Some(s.chars().count() as i64),
        _ => None,
    }
}

// ---------------------------------------------------------------
// The trace.

/// Trace `committed` (plus `staged`, an uncommitted suffix such as
/// `| [pred]`) over `adapter`; `locator` renders a node's canonical
/// path (the session's `render`).
pub fn trace<A: AstAdapter>(
    committed: &str,
    staged: Option<&str>,
    adapter: &A,
    locator: &dyn Fn(NodeId) -> String,
    opts: &Options,
) -> Result<qt::Payload> {
    let qc = parse(committed, adapter)?;
    let full_text = match staged {
        Some(s) if !s.trim().is_empty() => format!("{committed} {}", s.trim()),
        _ => committed.to_string(),
    };
    let qf = parse(&full_text, adapter)?;
    let plan_c = plan(&qc)?;
    let plan_f = plan(&qf)?;
    if plan_f.len() < plan_c.len() || plan_c.iter().zip(&plan_f).any(|(a, b)| a.expr != b.expr) {
        return Err(unsupported(
            "the staged text must extend the committed query",
        ));
    }
    let n_committed = plan_c.len();
    let steps: Vec<qt::Step> = plan_f
        .iter()
        .enumerate()
        .map(|(i, p)| step_record(i, p, i >= n_committed))
        .collect();

    let root = adapter.root();
    let mut tr = Tracer {
        adapter,
        threads: Vec::new(),
        snaps: Vec::new(),
        look: Vec::new(),
        groups: Vec::new(),
    };
    tr.new_thread("t1", None, 0);
    let mut live = vec![Th {
        id: "t1".into(),
        node: Some(root),
        topic: None,
        edges: Vec::new(),
    }];
    tr.snaps.push(Snap {
        at: 0,
        iter: None,
        live: live.clone(),
    });
    for (i, p) in plan_f.iter().enumerate() {
        let at = i as i64 + 1;
        live = match &p.kind {
            PKind::Hop(s) => tr.hop(at, &live, s)?,
            PKind::Group(g) => tr.group(at, &live, g)?,
            PKind::Projection(pr) => tr.projection(&live, pr)?,
            PKind::Stage(st) => tr.stage(at, &live, st)?,
        };
        tr.snaps.push(Snap {
            at,
            iter: None,
            live: live.clone(),
        });
    }

    // The safety net: the plain evaluation must agree.
    let (caps, projected) = exec::eval_query_caps(&qf, adapter, root, &Correlation::default());
    let traced_nodes: Vec<Option<NodeId>> = live.iter().map(|t| t.node).collect();
    let eval_nodes: Vec<Option<NodeId>> = caps.iter().map(|c| Some(c.node)).collect();
    if tr.groups.is_empty() && traced_nodes != eval_nodes {
        return Err(QuarbError::Unsupported(format!(
            "trace diverges from evaluation: {} traced vs {} evaluated positions",
            traced_nodes.len(),
            eval_nodes.len()
        )));
    }
    if projected {
        let traced: Vec<Option<Value>> = live.iter().map(|t| t.topic.clone()).collect();
        let evaled: Vec<Option<Value>> = caps
            .iter()
            .map(|c| Some(c.topic.clone().unwrap_or(Value::Null)))
            .collect();
        if traced != evaled {
            return Err(QuarbError::Unsupported(
                "trace diverges from evaluation: topics differ".into(),
            ));
        }
    }

    // The working frame and its window.
    let working_at = n_committed as i64;
    let working = tr
        .snaps
        .iter()
        .find(|s| s.at == working_at && s.iter.is_none())
        .expect("working snapshot exists");
    let n = working.live.len();
    let (first, last) = match opts.window {
        Some((a, b)) => (a.max(1), b.min(n)),
        None if n > opts.page => (1, opts.page),
        None => (1, n),
    };
    let whole = opts.window.is_none() && n <= opts.page;
    let mut embedded: HashSet<String> = HashSet::new();
    if whole {
        embedded.extend(tr.threads.iter().map(|t| t.id.clone()));
        embedded.extend(tr.groups.iter().map(|g| g.id.clone()));
    } else {
        // The windowed threads, their ancestors, their descendants.
        let seed: Vec<String> = working.live[first.saturating_sub(1)..last]
            .iter()
            .map(|t| t.id.clone())
            .collect();
        let parent_of: HashMap<&str, Option<&str>> = tr
            .threads
            .iter()
            .map(|t| (t.id.as_str(), t.parent.as_deref()))
            .collect();
        for id in &seed {
            let mut cur: Option<&str> = Some(id.as_str());
            while let Some(c) = cur {
                embedded.insert(c.to_string());
                cur = parent_of.get(c).copied().flatten();
            }
        }
        loop {
            let before = embedded.len();
            for t in &tr.threads {
                if let Some(p) = &t.parent
                    && embedded.contains(p)
                    && seed
                        .iter()
                        .any(|s| t.id.starts_with(&format!("{s}.")) || *s == *p)
                {
                    embedded.insert(t.id.clone());
                }
            }
            if embedded.len() == before {
                break;
            }
        }
        // Aggregates over windowed threads stay embedded.
        for g in &tr.groups {
            if g.members.iter().any(|m| embedded.contains(m)) {
                embedded.insert(g.id.clone());
            }
        }
    }

    // The arbor to embed.
    let family = opts.adapter.as_str();
    let total = subtree_count(adapter, root);
    let full_arbor = whole && total as usize <= opts.full_arbor_limit;
    let mut keep: HashSet<NodeId> = HashSet::new();
    if !full_arbor {
        let mut want: Vec<NodeId> = Vec::new();
        for s in &tr.snaps {
            for t in &s.live {
                if embedded.contains(&t.id) {
                    if let Some(n) = t.node {
                        want.push(n);
                    }
                    for (e, _) in &t.edges {
                        match e {
                            EdgeKey::Tree(a, b) => {
                                want.push(*a);
                                want.push(*b);
                            }
                            EdgeKey::Link(a, _, b) => {
                                want.push(*a);
                                want.push(*b);
                            }
                        }
                    }
                }
            }
        }
        // Lookahead candidates ride along only where they host an
        // embedded thread (the lookahead filter below), which the
        // positions already cover.
        for n in want {
            let mut cur = Some(n);
            while let Some(c) = cur {
                if !keep.insert(c) {
                    break;
                }
                cur = adapter.parent(c);
            }
        }
        keep.insert(root);
    }
    // Preorder over the (restricted) tree.
    let mut order: Vec<(NodeId, usize)> = Vec::new();
    let mut stack: Vec<(NodeId, usize)> = vec![(root, 0)];
    while let Some((n, depth)) = stack.pop() {
        order.push((n, depth));
        let kids = adapter.children(n);
        for c in kids.into_iter().rev() {
            if full_arbor || keep.contains(&c) {
                stack.push((c, depth + 1));
            }
        }
    }
    let node_id: HashMap<NodeId, String> = order
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (*n, format!("n{}", i + 1)))
        .collect();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut edge_id: HashMap<EdgeKey, String> = HashMap::new();
    for (n, depth) in &order {
        nodes.push(qt::Node {
            node: node_id[n].clone(),
            kind: node_kind(adapter, family, *n, *depth),
            props: node_props(adapter, family, *n),
        });
    }
    for (n, _) in &order {
        for (i, c) in adapter.children(*n).into_iter().enumerate() {
            if !node_id.contains_key(&c) {
                continue;
            }
            let id = format!("g{}", edges.len() + 1);
            edge_id.insert(EdgeKey::Tree(*n, c), id.clone());
            edges.push(qt::Edge {
                edge: id,
                from: node_id[n].clone(),
                to: node_id[&c].clone(),
                kind: qt::EdgeKind::Parent,
                name: adapter.name(c),
                index: Some(i as i64 + 1),
                label: None,
            });
        }
    }
    let mut xcount = 0;
    for (n, _) in &order {
        for (label, target) in adapter.links(*n) {
            if !node_id.contains_key(&target) {
                continue;
            }
            xcount += 1;
            let id = format!("x{xcount}");
            edge_id.insert(EdgeKey::Link(*n, label.clone(), target), id.clone());
            edges.push(qt::Edge {
                edge: id,
                from: node_id[n].clone(),
                to: node_id[&target].clone(),
                kind: qt::EdgeKind::Crosslink,
                name: None,
                index: None,
                label: Some(label),
            });
        }
    }

    // Tables.
    let mode_of = |s: &Snap| {
        if s.live.iter().any(|t| t.topic.is_some()) {
            "scalar"
        } else {
            "navigation"
        }
    };
    let mut snapshots = Vec::new();
    let mut positions = Vec::new();
    let mut ribbons = Vec::new();
    let mut stubs = Vec::new();
    for s in &tr.snaps {
        snapshots.push(qt::Snapshot {
            at: s.at,
            iter: s.iter,
            mode: mode_of(s).into(),
            size: s.live.len() as i64,
        });
        let mut idx_embedded: Vec<i64> = Vec::new();
        for (i, t) in s.live.iter().enumerate() {
            if !embedded.contains(&t.id) {
                continue;
            }
            idx_embedded.push(i as i64 + 1);
            positions.push(qt::Position {
                at: s.at,
                iter: s.iter,
                thread: t.id.clone(),
                node: t.node.map(|n| node_id[&n].clone()),
                topic: t.topic.as_ref().map(scalar),
                index: Some(i as i64 + 1),
                locator: t.node.map(locator),
            });
        }
        // Ribbons: the truth over the full context, for embedded edges.
        let mut count: HashMap<(String, bool), i64> = HashMap::new();
        let mut first_seen: Vec<(String, bool)> = Vec::new();
        for t in &s.live {
            for (e, against) in &t.edges {
                if let Some(id) = edge_id.get(e) {
                    let key = (id.clone(), *against);
                    if !count.contains_key(&key) {
                        first_seen.push(key.clone());
                    }
                    *count.entry(key).or_insert(0) += 1;
                }
            }
        }
        first_seen.sort_by_key(|(id, _)| {
            let (letter, num) = id.split_at(1);
            (letter.to_string(), num.parse::<i64>().unwrap_or(0))
        });
        for key in first_seen {
            ribbons.push(qt::Ribbon {
                at: s.at,
                iter: s.iter,
                edge: key.0.clone(),
                threads: count[&key],
                dir: key.1.then(|| "against".to_string()),
            });
        }
        // Stubs: the elided results on either side.
        if s.iter.is_none() && !whole && !idx_embedded.is_empty() {
            let total = s.live.len() as i64;
            let lo = *idx_embedded.first().unwrap();
            let hi = *idx_embedded.last().unwrap();
            let measure = s.at == working_at;
            let mut stub = |side: &str, a: i64, b: i64| {
                if b < a {
                    return;
                }
                let elided: Vec<&Th> = s.live[(a - 1) as usize..b as usize].iter().collect();
                let m = |name: &str, f: &dyn Fn(NodeId) -> Option<i64>| -> Option<i64> {
                    if !measure || !opts.measures.iter().any(|x| x == name) {
                        return None;
                    }
                    let mut sum = 0;
                    for t in &elided {
                        sum += f(t.node?)?;
                    }
                    Some(sum)
                };
                stubs.push(qt::Stub {
                    at: s.at,
                    side: side.into(),
                    first: a,
                    last: b,
                    count: b - a + 1,
                    nodes: m("nodes", &|n| Some(subtree_count(adapter, n))),
                    children: m("children", &|n| Some(adapter.children(n).len() as i64)),
                    size: m("size", &|n| size_of(adapter, n)),
                    kinds: None,
                });
            };
            stub("before", 1, lo - 1);
            stub("after", hi + 1, total);
        }
    }
    let threads: Vec<qt::Thread> = tr
        .threads
        .iter()
        .filter(|t| embedded.contains(&t.id))
        .map(|t| qt::Thread {
            thread: t.id.clone(),
            parent: t.parent.clone(),
            born: t.born,
            ended: t.ended,
            fate: t.fate.into(),
        })
        .collect();
    let groups: Vec<qt::Group> = tr
        .groups
        .iter()
        .filter(|g| embedded.contains(&g.id))
        .map(|g| qt::Group {
            group: g.id.clone(),
            at: g.at,
            kind: "agg".into(),
            name: g.name.clone(),
            members: g
                .members
                .iter()
                .filter(|m| embedded.contains(*m))
                .cloned()
                .collect(),
        })
        .collect();
    let lookahead: Vec<qt::Lookahead> = tr
        .look
        .iter()
        .filter(|l| embedded.contains(&l.thread) && node_id.contains_key(&l.node))
        .filter(|l| {
            // A candidate is shown when the thread it becomes (or
            // stays) is embedded — or when nothing is elided.
            whole
                || tr.snaps.iter().any(|s| {
                    s.live
                        .iter()
                        .any(|t| embedded.contains(&t.id) && t.node == Some(l.node))
                })
        })
        .map(|l| qt::Lookahead {
            at: l.at,
            iter: l.iter,
            thread: l.thread.clone(),
            node: node_id[&l.node].clone(),
            verdict: if l.matched { "match" } else { "fail" }.into(),
            index: Some(l.ordinal),
            locator: Some(locator(l.node)),
        })
        .collect();
    let results = if projected && whole {
        Some(
            live.iter()
                .map(|t| scalar(t.topic.as_ref().unwrap_or(&Value::Null)))
                .collect(),
        )
    } else {
        None
    };

    Ok(qt::Payload {
        meta: qt::Meta {
            contract: qt::CONTRACT.into(),
            version: qt::VERSION.into(),
            query: committed.to_string(),
            adapter: opts.adapter.clone(),
            source: opts.source.clone(),
        },
        arbor: qt::Arbor {
            root: node_id[&root].clone(),
            nodes,
            edges,
        },
        steps,
        threads,
        groups,
        snapshots,
        window: qt::Window {
            at: working_at,
            first: first as i64,
            last: last as i64,
            of: n as i64,
        },
        stubs,
        positions,
        ribbons,
        lookahead,
        results,
    })
}
